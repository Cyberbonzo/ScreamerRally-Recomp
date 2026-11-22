//! Sampling profiler (diagnostics, `SR_SAMPLE=1`): a thread stops the guest thread about
//! every half millisecond, reads its EIP and lets it go on; the report lists the hottest
//! places (guest code by link address, host code by module), separately for the
//! interpolated frames.

use std::collections::HashMap;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, GetThreadContext};
use windows_sys::Win32::System::Threading::{ResumeThread, SuspendThread};

/// CONTEXT_i386 | CONTROL
const CONTEXT_CONTROL: u32 = 0x0001_0001;
/// Samples are grouped by this many bytes of code.
const BUCKET: u32 = 32;

#[derive(Default)]
struct Samples {
    all: HashMap<u32, u32>,
    extra: HashMap<u32, u32>,
    total: u64,
    total_extra: u64,
}

static SAMPLES: Mutex<Option<Samples>> = Mutex::new(None);

/// Start sampling `thread` (the guest thread) if `SR_SAMPLE` is set.
pub fn start(thread: HANDLE) {
    if std::env::var_os("SR_SAMPLE").is_none() {
        return;
    }
    *SAMPLES.lock().unwrap() = Some(Samples::default());
    let h = thread as usize;
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_micros(500));
            let thread = h as HANDLE;
            // nothing may allocate or lock while the guest is stopped (it may hold the
            // heap or log lock): read the context, resume, then record
            let (eip, extra) = unsafe {
                if SuspendThread(thread) == u32::MAX {
                    break;
                }
                let mut ctx: CONTEXT = std::mem::zeroed();
                ctx.ContextFlags = CONTEXT_CONTROL;
                let ok = GetThreadContext(thread, &mut ctx) != 0;
                let extra = crate::interp::in_extra_draw();
                ResumeThread(thread);
                if !ok {
                    continue;
                }
                (ctx.Eip, extra)
            };
            let mut s = SAMPLES.lock().unwrap();
            let Some(s) = s.as_mut() else { break };
            let key = eip / BUCKET * BUCKET;
            *s.all.entry(key).or_default() += 1;
            s.total += 1;
            if extra {
                *s.extra.entry(key).or_default() += 1;
                s.total_extra += 1;
            }
        }
    });
}

/// Hottest places, for the end-of-run report.
pub fn report(m: &crate::Machine) {
    let guard = SAMPLES.lock().unwrap();
    let Some(s) = guard.as_ref() else { return };
    let name = |a: u32| match m.img.to_link(a) {
        Some(l) => format!("{l:#07x}"),
        None if m.arena.contains(a) => format!("arena {a:#x}"),
        None => crate::trap::module_of(a),
    };
    for (title, map, total) in [("all", &s.all, s.total), ("extra frames", &s.extra, s.total_extra)] {
        if total == 0 {
            continue;
        }
        let mut v: Vec<_> = map.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let n = std::env::var("SR_SAMPLE_TOP").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
        crate::info!("  samples ({title}): {total}");
        // by module first (host code is spread over many buckets)
        let mut modules: HashMap<String, u32> = HashMap::new();
        for (a, c) in map {
            let k = if m.img.contains(*a) {
                "guest image".to_string()
            } else if m.arena.contains(*a) {
                "arena (stubs)".to_string()
            } else {
                crate::trap::module_of(*a).split('+').next().unwrap_or("?").to_string()
            };
            *modules.entry(k).or_default() += *c;
        }
        let mut mv: Vec<_> = modules.into_iter().collect();
        mv.sort_by(|a, b| b.1.cmp(&a.1));
        for (k, c) in mv.iter().take(12) {
            crate::info!("    {:5.1}%  {k}", *c as f64 * 100.0 / total as f64);
        }
        // by function (host code through its symbols, guest code by bucket)
        let mut funcs: HashMap<String, u32> = HashMap::new();
        for (a, c) in &v {
            let k = if m.img.contains(**a) || m.arena.contains(**a) { name(**a) } else { symbol(**a).unwrap_or_else(|| name(**a)) };
            *funcs.entry(k).or_default() += **c;
        }
        let mut fv: Vec<_> = funcs.into_iter().collect();
        fv.sort_by(|a, b| b.1.cmp(&a.1));
        for (k, c) in fv.iter().take(n) {
            crate::info!("    {:5.1}%  {k}", *c as f64 * 100.0 / total as f64);
        }
    }
}

/// Host function containing `addr` (dbghelp, from the executable's PDB and DLL exports).
fn symbol(addr: u32) -> Option<String> {
    use windows_sys::Win32::System::Diagnostics::Debug::{SYMBOL_INFO, SymFromAddr, SymInitialize, SymSetOptions};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| unsafe {
        // SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_FAIL_CRITICAL_ERRORS
        SymSetOptions(0x2 | 0x4 | 0x200);
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned())).unwrap_or_default();
        let path = std::ffi::CString::new(dir).unwrap_or_default();
        SymInitialize(GetCurrentProcess(), path.as_ptr() as *const u8, 1);
    });
    #[repr(C)]
    struct Buf {
        info: SYMBOL_INFO,
        name: [u8; 512],
    }
    unsafe {
        let mut b: Buf = std::mem::zeroed();
        b.info.SizeOfStruct = std::mem::size_of::<SYMBOL_INFO>() as u32;
        b.info.MaxNameLen = 500;
        let mut disp = 0u64;
        if SymFromAddr(GetCurrentProcess(), addr as u64, &mut disp, &mut b.info) == 0 {
            return None;
        }
        let p = b.info.Name.as_ptr() as *const u8;
        let bytes = std::slice::from_raw_parts(p, b.info.NameLen.min(500) as usize);
        let module = crate::trap::module_of(addr);
        let module = module.split('+').next().unwrap_or("");
        Some(format!("{module}!{}", String::from_utf8_lossy(bytes)))
    }
}
