//! Native execution environment for the original Screamer Rally code.
//!
//! The original 32-bit x86 machine code runs directly on the CPU. This crate provides
//! everything around it that a 1997 DOS/4GW program expects: a loader, DOS, DPMI, the
//! BIOS, I/O ports, interrupts, the file system (backed by the user's CD image), and the
//! replacement of hardware-specific libraries at function level.
//!
//! Threading model: the guest runs on one dedicated thread. All guest-facing state lives in
//! the single [`Machine`], which is only touched on that thread (from the trap handler and
//! from hooks). Other host threads talk to it through explicit shared state.

pub mod autopilot;
pub mod camera;
pub mod cdaudio;
/// The race's condition (day, night, fog) on demand.
pub mod conditions;
pub mod cpu;
pub mod depth;
pub mod events;
pub mod guest;
pub mod hmi;
pub mod hooks;
pub mod interp;
pub mod joystick;
pub mod lod;
pub mod log;
pub mod pacing;
/// The road's line (the computer's cars' line round the track).
pub mod road;
pub mod rumble;
pub mod mem;
pub mod native;
pub mod glide;
pub mod ports;
pub mod profile;
pub mod sampler;
pub mod sel;
pub mod segjit;
pub mod subpixel;
pub mod sos_digi;
pub mod services;
pub mod time;
pub mod toolmode;
pub mod trap;
pub mod ui2d;
pub mod unlock;
pub mod vfs;
pub mod view;
pub mod visedit;
pub mod visibility;
pub mod widescreen;

use std::path::PathBuf;

use sr_disc::Disc;
use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;

use crate::cpu::Cpu;
use crate::hooks::{Arena, Hooks};
use crate::mem::{FLAT_CS, FLAT_DS, LoadedImage, lowmem::LowMem};
use crate::ports::Ports;
use crate::sel::{Descriptor, Selectors};
use crate::services::dos::Dos;
use crate::services::dpmi::Dpmi;
use crate::vfs::Vfs;

pub struct Stats {
    pub traps: u64,
    pub low_accesses: u64,
    pub ints: [u64; 256],
    /// int 21h calls per AH, int 31h calls per AH.
    pub dos_fn: [u64; 256],
    pub dpmi_fn: [u64; 256],
    /// Trap count per (link) address, to find hot sites worth patching.
    pub sites: std::collections::HashMap<u32, u64>,
    pub port_reads: u64,
    pub port_writes: u64,
    pub seg_site_hits: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            traps: 0,
            low_accesses: 0,
            ints: [0; 256],
            dos_fn: [0; 256],
            dpmi_fn: [0; 256],
            sites: Default::default(),
            port_reads: 0,
            port_writes: 0,
            seg_site_hits: 0,
        }
    }
}

pub struct Machine {
    pub img: LoadedImage,
    pub profile: &'static profile::Profile,
    pub arena: Arena,
    /// Re-execution buffer used by the trap layer.
    pub scratch: u32,
    pub hooks: Hooks,
    pub sel: Selectors,
    pub lowmem: LowMem,
    pub vfs: Vfs,
    pub dos: Dos,
    pub dpmi: Dpmi,
    pub ports: Ports,
    pub cd: cdaudio::CdAudio,
    pub digi: sos_digi::Digi,
    pub widescreen: widescreen::Widescreen,
    pub options: GameOptions,
    pub unlock: unlock::Unlock,
    /// Highest detail level for every object (`lod`).
    pub lod: lod::Lod,
    /// The extended visibility lists (`visibility`).
    pub visibility: visibility::Visibility,
    /// The game's 2D pictures before the 480-line stretch (`ui2d`).
    pub ui2d: ui2d::Ui2d,
    /// The AI drives the player's car (`autopilot`).
    pub autopilot: autopilot::Autopilot,
    /// Controller vibration from the player's car (`rumble`).
    pub rumble: rumble::Rumble,
    /// Kinds of the 3D objects by instance (`depth`).
    pub kinds: depth::Kinds,
    pub interp: interp::Interp,
    /// The world export (`SR_DUMP_WORLD`, `native::world`).
    pub world: native::world::WorldDump,
    /// The native renderer's integration (`native::frame`).
    pub native: native::frame::NativeState,
    /// Audio output (pausing).
    pub audio: Option<sr_audio::Audio>,
    pub segjit: segjit::SegJit,
    pub glide: Option<glide::Glide>,
    /// Rendering backend handed to the Glide layer when its imports are bound.
    pub pending_backend: Option<Box<dyn glide::Backend>>,
    /// Virtual interrupt flag (guest `cli` / `sti`).
    pub vif: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub sched: Option<events::Scheduler>,
    /// Protected-mode interrupt vectors (selector, offset).
    pub pm_vectors: [(u16, u32); 256],
    /// Real-mode interrupt vectors (segment:offset).
    pub rm_vectors: [u32; 256],
    pub exc_handlers: [(u16, u32); 32],
    pub video_mode: u8,
    /// Flat address of the environment block handed to the guest.
    pub env_linear: u32,
    /// Program path as DOS sees it (`C:\SR\STRT3FX.EXE`).
    pub program: String,
    pub stack_base: u32,
    pub stack_top: u32,
    pub exit_code: Option<u32>,
    pub stats: Stats,
    /// Patched segment-override instructions: address -> original bytes.
    pub seg_sites: std::collections::HashMap<u32, [u8; 16]>,
    pub step_trace: Option<trap::StepTrace>,
}

static mut MACHINE: *mut Machine = std::ptr::null_mut();

/// The machine. Only valid on the guest thread after `boot` created it.
#[allow(static_mut_refs)]
pub fn machine() -> &'static mut Machine {
    unsafe { MACHINE.as_mut().expect("machine not initialised") }
}

pub(crate) fn try_machine() -> Option<&'static mut Machine> {
    unsafe { MACHINE.as_mut() }
}

impl Machine {
    /// Describe a guest address for logs: loaded address plus link-time address.
    pub fn where_(&self, addr: u32) -> String {
        match self.img.to_link(addr) {
            Some(l) => format!("{addr:#010x} (link {l:#07x})"),
            None if self.arena.contains(addr) => format!("{addr:#010x} (runtime arena)"),
            None => format!("{addr:#010x}"),
        }
    }

    pub fn idle(&mut self) {
        std::thread::yield_now();
    }

    pub fn vif(&self) -> bool {
        self.vif.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_vif(&self, on: bool) {
        self.vif.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// The guest installed a protected-mode interrupt handler.
    pub fn vector_set(&mut self, vector: u8, offset: u32) {
        self.pm_vectors[vector as usize] = (FLAT_CS, offset);
        crate::debug!("interrupt vector {vector:02x}h -> {}", self.where_(offset));
        if vector == 0x09 {
            if let Some(s) = &self.sched {
                s.irq1_handler.store(offset, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    pub fn io_read(&mut self, port: u16, width: u32) -> u32 {
        self.stats.port_reads += 1;
        self.ports.read(port, width)
    }

    pub fn io_write(&mut self, port: u16, width: u32, v: u32) {
        self.stats.port_writes += 1;
        self.ports.write(port, width, v)
    }

    /// Selector for a real-mode segment (DPMI 0002h semantics: one per segment).
    pub fn selector_for_segment(&mut self, seg: u16) -> u16 {
        let base = seg as u32 * 16;
        self.sel.create(Descriptor { base, limit: 0xffff, access: 0x00f3 }).unwrap_or(FLAT_DS)
    }

    /// DPMI physical address mapping. Only device memory the runtime emulates can be
    /// mapped; nothing is exposed yet.
    pub fn map_physical(&mut self, phys: u32, size: u32) -> Option<u32> {
        crate::debug!("physical mapping request {phys:#x}+{size:#x}");
        None
    }
}

/// Set when the run time limit (`Config::run_for`) is reached.
pub static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Diagnostics (`SR_OBSERVE`): log registers and the top of the stack (image addresses as
/// link addresses) for the first calls of an observed address; `SR_OBSERVE_N` = how many.
pub fn observe_generic(m: &mut Machine, f: &mut crate::cpu::HookFrame, tramp: u32) -> hooks::HookResult {
    static N: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let n = *N.get_or_init(|| std::env::var("SR_OBSERVE_N").ok().and_then(|v| v.parse().ok()).unwrap_or(4));
    let id = m.hooks.list.iter().position(|h| h.arg == tramp).unwrap_or(0);
    let h = &m.hooks.list[id];
    if h.calls <= n {
        let link = m.img.to_link(h.addr).unwrap_or(0);
        let stack: Vec<String> = (0..40)
            .map(|k| {
                let v = f.stack(k);
                m.img.to_link(v).filter(|_| m.img.is_code(v)).map(|l| format!("<{l:x}>")).unwrap_or_else(|| format!("{v:x}"))
            })
            .collect();
        crate::info!(
            "observe {link:#x} #{}: eax={:x} ebx={:x} ecx={:x} edx={:x} esi={:x} edi={:x} ebp={:x} esp={:x} stack {}",
            h.calls,
            f.eax,
            f.ebx,
            f.ecx,
            f.edx,
            f.esi,
            f.edi,
            f.ebp,
            f.guest_esp(),
            stack.join(" ")
        );
    }
    hooks::HookResult::Resume(tramp)
}

/// Diagnostics: `SR_PEEK="frame:link_addr:len[,...]"` logs guest memory (link-time
/// addresses of the executable) after the given frame, as dwords, words and floats.
pub fn debug_peek(m: &Machine, frame: u64) {
    static SPEC: std::sync::OnceLock<Vec<(u64, u32, u32)>> = std::sync::OnceLock::new();
    let spec = SPEC.get_or_init(|| {
        std::env::var("SR_PEEK")
            .ok()
            .map(|v| {
                v.split(',')
                    .filter_map(|e| {
                        let mut p = e.split(':');
                        let f = p.next()?.trim().parse().ok()?;
                        let at = p.next()?.trim();
                        // "*addr": dump through the pointer stored at addr (bit 31 marks it)
                        let (deref, at) = at.strip_prefix('*').map_or((0, at), |a| (0x8000_0000u32, a));
                        let a = u32::from_str_radix(at.trim_start_matches("0x"), 16).ok()? | deref;
                        let n = p.next().and_then(|n| u32::from_str_radix(n.trim().trim_start_matches("0x"), 16).ok()).unwrap_or(0x40);
                        Some((f, a, n))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    for &(f, link, len) in spec {
        if f != frame {
            continue;
        }
        let (base, link) = if link & 0x8000_0000 != 0 {
            let p = unsafe { mem::rd32(m.img.link(link & 0x7fff_ffff)) };
            (p, p)
        } else {
            (m.img.link(link), link)
        };
        for off in (0..len).step_by(16) {
            let words: Vec<u32> = (0..4).map(|k| unsafe { mem::rd32(base + off + k * 4) }).collect();
            crate::info!(
                "peek {:06x}: {}   {}   | {}",
                link + off,
                words.iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" "),
                words.iter().map(|&w| format!("{}", w as i32)).collect::<Vec<_>>().join(" "),
                words.iter().map(|&w| format!("{:.6}", f32::from_bits(w))).collect::<Vec<_>>().join(" ")
            );
        }
    }
}

/// Called on the guest thread at a point where it holds no runtime locks (after each
/// frame): ends the run if the time limit was reached, with the statistics report.
static VIRTUAL_LIMIT: std::sync::OnceLock<Option<std::time::Duration>> = std::sync::OnceLock::new();

pub fn stop_point(m: &Machine) {
    // deterministic mode: the run time limit counts virtual time
    if crate::time::deterministic() {
        if let Some(limit) = *VIRTUAL_LIMIT.get().unwrap_or(&None) {
            if crate::time::uptime() >= limit {
                STOP_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    if !STOP_REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(s) = &m.sched {
        s.stop();
    }
    // let the timer thread finish a delivery in progress and leave its loop
    std::thread::sleep(std::time::Duration::from_millis(50));
    crate::info!("run time limit reached");
    report_stats(m);
    log::flush();
    terminate(0);
}

/// End the process immediately. `ExitProcess` would run DLL shutdown, which can deadlock on
/// a lock held by the (possibly frozen) guest thread; logs are flushed by the caller.
pub fn terminate(code: u32) -> ! {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
    // the controller's motors would keep running after the process
    rumble::stop();
    unsafe {
        TerminateProcess(GetCurrentProcess(), code);
    }
    unreachable!()
}

/// Stop the process after an unrecoverable guest error, with a full diagnostic dump.
pub fn fatal(m: &mut Machine, ctx: &CONTEXT, msg: &str) -> ! {
    let ins = trap::decode(ctx.Eip);
    crate::error!("FATAL: {msg}");
    crate::error!("  at {}: {}", m.where_(ctx.Eip), trap::format(&ins));
    crate::error!("  {}", ctx.dump());
    crate::error!(
        "  virtual segments: {}",
        (0..6)
            .map(|i| format!("{}={:#06x}/{:#x}", ["es", "cs", "ss", "ds", "fs", "gs"][i], m.sel.virt()[i].0, m.sel.virt()[i].1))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let esp = ctx.Esp;
    if esp >= m.stack_base && esp + 64 <= m.stack_top {
        let words: Vec<String> = (0..16).map(|i| format!("{:08x}", unsafe { mem::rd32(esp + i * 4) })).collect();
        crate::error!("  stack: {}", words.join(" "));
    }
    report_stats(m);
    log::flush();
    terminate(3)
}

pub fn report_stats(m: &Machine) {
    if let Some(t) = &m.step_trace {
        let text: String = t.seen.iter().map(|a| format!("{a:x}
")).collect();
        let path = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("logs").join("executed.txt")));
        if let Some(p) = path {
            let _ = std::fs::write(&p, text);
            crate::info!("executed-address trace: {} addresses -> {}", t.seen.len(), p.display());
        }
    }
    sampler::report(m);
    crate::info!("joystick: {} axis reads", joystick::GAMEPORT.reads.load(std::sync::atomic::Ordering::Relaxed));
    let ints: Vec<String> =
        m.stats.ints.iter().enumerate().filter(|(_, n)| **n > 0).map(|(v, n)| format!("{v:02x}h:{n}")).collect();
    crate::info!(
        "stats: {} traps, {} low-memory accesses, {} segment-site executions, ports {}r/{}w, interrupts [{}]",
        m.stats.traps,
        m.stats.low_accesses,
        m.stats.seg_site_hits,
        m.stats.port_reads,
        m.stats.port_writes,
        ints.join(" ")
    );
    let per = |t: &[u64; 256]| t.iter().enumerate().filter(|(_, n)| **n > 0).map(|(v, n)| format!("{v:02x}:{n}")).collect::<Vec<_>>().join(" ");
    crate::info!("  int 21h by AH: {}", per(&m.stats.dos_fn));
    crate::info!("  int 31h by AH: {}", per(&m.stats.dpmi_fn));
    let mut hot: Vec<(&u32, &u64)> = m.stats.sites.iter().collect();
    hot.sort_by(|a, b| b.1.cmp(a.1));
    crate::info!(
        "  hottest trap sites: {}",
        hot.iter().take(std::env::var("SR_HOT_SITES").ok().and_then(|v| v.parse().ok()).unwrap_or(12)).map(|(a, n)| format!("{:#x}:{n}", m.img.to_link(**a).unwrap_or(**a))).collect::<Vec<_>>().join(" ")
    );
    if let Some(s) = &m.sched {
        use std::sync::atomic::Ordering::Relaxed;
        let sh = s.shared.lock().unwrap();
        crate::info!(
            "  virtual interrupts: {} delivered, {} deferred attempts, {} ticks owed",
            s.delivered.load(Relaxed),
            s.busy_retries.load(Relaxed),
            sh.pending_ticks
        );
        for e in &sh.events {
            crate::info!("  timer event {} {} Hz -> {:#x}: {} calls{}", e.handle, e.rate_hz, e.callback, e.calls, if e.internal { " (held)" } else { "" });
        }
    }
    if let Some(g) = &m.glide {
        crate::info!("  glide: {} frames, {} triangles, {} off screen (broken geometry), {} back faces culled", g.frames, g.tris, g.bad_tris, g.culled_back);
        if m.interp.frames > 0 {
            crate::info!("  interpolation: {} extra frames, {} pages restored", m.interp.frames, m.interp.restored);
            let mut g = m.interp.game_ms.clone();
            if !g.is_empty() {
                g.sort_by(|a, b| a.total_cmp(b));
                let q = |p: f64| g[((g.len() - 1) as f64 * p) as usize];
                crate::info!("  game frame per step: median {:.1} ms, 95% {:.1} ms, 99% {:.1} ms, max {:.1} ms", q(0.5), q(0.95), q(0.99), q(1.0));
            }
            let mut w = m.interp.wait_ms.clone();
            if !w.is_empty() {
                w.sort_by(|a, b| a.total_cmp(b));
                let q = |p: f64| w[((w.len() - 1) as f64 * p) as usize];
                crate::info!("  display waits: median {:.1} ms, 90% {:.1} ms, max {:.1} ms", q(0.5), q(0.9), q(1.0));
            }
            let mut e = m.interp.extra_ms.clone();
            if !e.is_empty() {
                e.sort_by(|a, b| a.0.total_cmp(&b.0));
                let q = |p: f64| e[((e.len() - 1) as f64 * p) as usize].0;
                crate::info!("  extra frame: median {:.1} ms, 95% {:.1} ms, 99% {:.1} ms, max {:.1} ms", q(0.5), q(0.95), q(0.99), q(1.0));
                let worst: Vec<String> = e.iter().rev().take(8).map(|(t, f)| format!("{t:.1} ms at frame {f}")).collect();
                crate::info!("  slowest extra frames: {}", worst.join(", "));
            }
            if m.interp.frames > 0 {
                let ms = m.interp.times.map(|t| t * 1000.0 / m.interp.frames as f64);
                crate::info!(
                    "  per extra frame: sync (per step) {:.2} ms, draw {:.2} ms, HUD {:.2} ms, restore {:.2} ms, present {:.2} ms, {:.1} traps",
                    ms[0], ms[1], ms[2], ms[3], ms[4], m.interp.traps as f64 / m.interp.frames as f64
                );
                let w = m.interp.waits;
                crate::info!("  display waits: {} ready, {} timed out, {:.1} s waiting", w.0, w.1, w.2);
            }
        }
        let mut t: Vec<(usize, u64)> = g.func_time.iter().copied().enumerate().filter(|(_, d)| *d != 0).collect();
        t.sort_by(|a, b| b.1.cmp(&a.1));
        for (i, d) in t.iter().take(10) {
            let ms = crate::time::cycles_to_duration(*d).as_secs_f64() * 1000.0;
            crate::info!("    {:40} {:8} calls {:9.1} ms", g.names[*i], g.calls[*i], ms);
        }
    }
    for h in &m.hooks.list {
        if h.calls > 0 && h.name != "glide import" {
            crate::info!("  hook {:40} {} calls", h.name, h.calls);
        }
    }
}

pub struct Config {
    /// The user's game: the CD image (`.mds`, `.cue`, `.iso`), or a folder with the game
    /// installed (the DOS release that runs without its CD: no CD music).
    pub disc: PathBuf,
    /// Host folder used as the game's drive C:.
    pub c_root: PathBuf,
    /// Executable on the CD to run (in a game folder: its file name, in the folder).
    pub exe: String,
    /// Extra environment variables for the guest (`NAME=value`).
    pub env: Vec<String>,
    /// Stop after this long, printing statistics (diagnostic runs).
    pub run_for: Option<std::time::Duration>,
    /// Renderer for Glide (None = draw nothing).
    pub backend: Option<Box<dyn glide::Backend>>,
    /// Audio output (None = silent).
    pub audio: Option<sr_audio::Audio>,
    /// Game options applied by the executable profile.
    pub options: GameOptions,
}

/// Enhancements applied to the original program.
#[derive(Clone, Debug)]
pub struct GameOptions {
    /// Far-plane distance in percent of the game's own (100 = original, up to 300).
    pub draw_distance: u32,
    /// All tracks and cars selectable (saved progress unchanged).
    pub unlock_all: bool,
    /// Draw interpolated frames between the game's own (more than its simulation steps).
    pub interpolate: bool,
    /// Presented frames per second limit with VSync off (0 = none).
    pub fps_cap: u32,
}

impl Default for GameOptions {
    fn default() -> Self {
        GameOptions { draw_distance: 100, unlock_all: false, interpolate: false, fps_cap: 0 }
    }
}

/// Prepare everything and run the original program to completion. Returns its exit code.
pub fn boot(mut cfg: Config) -> Result<u32, String> {
    let (disc, vfs, exe) = if cfg.disc.is_dir() {
        // a game folder: the executable in it, no CD
        let name = cfg.exe.rsplit(['\\', '/']).next().unwrap_or(&cfg.exe).to_string();
        let exe = std::fs::read(cfg.disc.join(&name)).map_err(|e| format!("{}: {e}", cfg.disc.join(&name).display()))?;
        cfg.exe = name;
        (None, Vfs::from_folder(cfg.disc.clone(), cfg.c_root.clone())?, exe)
    } else {
        let disc: &'static Disc = Box::leak(Box::new(Disc::open(&cfg.disc).map_err(|e| format!("{}: {e}", cfg.disc.display()))?));
        let vfs = Vfs::new(disc, cfg.c_root.clone())?;
        let iso = vfs.iso.as_ref().ok_or("no data track")?;
        let entry = iso.lookup(&cfg.exe).map_err(|e| e.to_string())?.ok_or_else(|| format!("{} not found on the disc", cfg.exe))?;
        let exe = iso.read_file(&entry).map_err(|e| e.to_string())?;
        (Some(disc), vfs, exe)
    };
    let hash = sr_disc::sha256::hex(&sr_disc::sha256::sha256(&exe));
    let profile = profile::find(&hash).ok_or_else(|| format!("{} (sha256 {hash}) is not a supported executable", cfg.exe))?;
    info!("executable {} = {}", cfg.exe, profile.name);

    let le = sr_le::LeImage::parse(&exe).map_err(|e| e.to_string())?;
    let img = LoadedImage::load(&le, 0x3000_0000)?;
    info!("image loaded at {:#010x}-{:#010x}, entry {:#010x}", img.base, img.base + img.size, img.entry);
    for o in &img.objects {
        debug!("  obj{} link {:#08x} -> {:#010x} size {:#x}{}", o.number, o.link_base, o.addr, o.size, if o.code { " code" } else { "" });
    }

    // Guest stack above the image, so Watcom's stack checks (which compare against
    // addresses in the image) always see enough room.
    const STACK: u32 = 1 << 20;
    // deterministic mode: the guest arena (pointers to the stack end up in the game's data)
    let det = if crate::time::deterministic() { mem::alloc_guest(STACK) } else { None };
    let stack_base = det
        .or_else(|| mem::alloc(0x3800_0000, STACK, false))
        .or_else(|| mem::alloc(0, STACK, false))
        .ok_or("cannot allocate guest stack")?;

    let mut arena = Arena::new(1 << 20);
    let scratch = arena.reserve(256);
    let program = format!("{}\\{}", vfs::INSTALL_DIR, cfg.exe.rsplit(['\\', '/']).next().unwrap_or(&cfg.exe));
    let mut m = Box::new(Machine {
        img,
        profile,
        arena,
        scratch,
        hooks: Hooks::default(),
        sel: Selectors::default(),
        lowmem: LowMem::default(),
        vfs,
        dos: Dos::default(),
        dpmi: Dpmi::default(),
        ports: Ports::default(),
        cd: cdaudio::CdAudio::new(disc, cfg.audio.clone()),
        digi: sos_digi::Digi::new(cfg.audio.clone()),
        options: cfg.options.clone(),
        unlock: unlock::Unlock::default(),
        lod: lod::Lod::default(),
        visibility: visibility::Visibility::default(),
        ui2d: ui2d::Ui2d::default(),
        autopilot: autopilot::Autopilot::default(),
        rumble: rumble::Rumble::default(),
        kinds: depth::Kinds::default(),
        interp: interp::Interp::default(),
        world: native::world::WorldDump::default(),
        native: native::frame::NativeState::default(),
        audio: cfg.audio.clone(),
        widescreen: widescreen::Widescreen::default(),
        segjit: segjit::SegJit::default(),
        glide: None,
        pending_backend: cfg.backend.take(),
        vif: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        sched: None,
        pm_vectors: [(FLAT_CS, 0); 256],
        rm_vectors: [0; 256],
        exc_handlers: [(FLAT_CS, 0); 32],
        video_mode: 3,
        env_linear: 0,
        program,
        stack_base,
        stack_top: stack_base + STACK,
        exit_code: None,
        stats: Stats::default(),
        seg_sites: Default::default(),
        step_trace: std::env::var("SR_STEP_TRACE")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|budget| trap::StepTrace { budget, active: false, seen: Default::default() }),
    });

    if mem::lowmem::real_region_reserved() {
        m.lowmem.enable_real();
        info!("conventional memory: real, linear {:#x}-{:#x}", mem::lowmem::REAL_BASE, mem::lowmem::REAL_END);
    } else {
        warn!("conventional memory: virtual (start through the launcher for native-speed DOS memory)");
    }
    setup_dos_memory(&mut m, &cfg.env)?;
    m.sched = Some(events::Scheduler::new(m.img.base, m.img.base + m.img.size, m.vif.clone()));
    unsafe { MACHINE = &mut *m as *mut Machine };
    trap::install();
    (profile.install)(machine())?;

    let regs = guest::EntryRegs { esp: (machine().stack_top - 64) & !15, eip: machine().img.entry, ..Default::default() };
    let (sb, st) = (machine().stack_base, machine().stack_top);
    let handle = std::thread::Builder::new()
        .name("guest".into())
        .stack_size(4 << 20)
        .spawn(move || unsafe {
            machine().sched.as_ref().unwrap().start();
            let old = guest::set_stack_bounds(st, sb);
            let code = guest::enter(regs);
            guest::set_stack_bounds(old.0, old.1);
            code
        })
        .map_err(|e| e.to_string())?;
    {
        use std::os::windows::io::AsRawHandle;
        sampler::start(handle.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE);
    }
    let _ = VIRTUAL_LIMIT.set(cfg.run_for.filter(|_| crate::time::deterministic()));
    if let Some(limit) = cfg.run_for.filter(|_| !crate::time::deterministic()) {
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            // Stop at the guest's next frame (see `stop_point`): suspending it here could
            // freeze it inside a lock (the log, a mixer) that the report then waits for.
            STOP_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_secs(3));
            // no frame for 3 s (hung or not rendering): end without the report
            terminate(0);
        });
    }
    let code = handle.join().map_err(|_| "guest thread panicked".to_string())?;
    report_stats(machine());
    log::flush();
    Ok(code)
}

/// Build the BIOS data area, the PSP and the environment the program starts with.
fn setup_dos_memory(m: &mut Machine, extra_env: &[String]) -> Result<(), String> {
    // Interrupt vector table + BIOS data area: segments 0000-005F.
    let bda = m.lowmem.alloc_at(0, 0x60).ok_or("cannot place BIOS data area")?;
    unsafe {
        mem::wr16(bda + 0x410, 0x0026); // equipment: FPU present, 80x25 colour
        mem::wr16(bda + 0x413, 640);
        mem::wr8(bda + 0x449, 3);
        mem::wr16(bda + 0x44a, 80);
        mem::wr8(bda + 0x484, 24);
    }
    // PSP (256 bytes) in conventional memory, with an empty command tail.
    let (psp_seg, psp) = m.lowmem.alloc(0x10).ok_or("cannot allocate PSP")?;
    unsafe {
        mem::wr16(psp, 0x20cd);
        mem::wr16(psp + 2, 0x9fff);
        mem::wr8(psp + 0x80, 0);
        mem::wr8(psp + 0x81, 0x0d);
    }
    m.dos.psp_seg = psp_seg;
    m.dos.psp_sel = m.selector_for_segment(psp_seg);

    // Environment in flat memory, DOS layout: strings, empty string, count word, path.
    let mut env: Vec<u8> = Vec::new();
    let mut vars = vec!["COMSPEC=C:\\COMMAND.COM".to_string(), "PATH=C:\\".into(), "DOS4G=QUIET".into(), "BLASTER=A220 I5 D1 H5 T6".into()];
    vars.extend(extra_env.iter().cloned());
    for v in &vars {
        env.extend_from_slice(v.as_bytes());
        env.push(0);
    }
    env.push(0);
    env.extend_from_slice(&1u16.to_le_bytes());
    env.extend_from_slice(m.program.as_bytes());
    env.push(0);
    let addr = mem::alloc_guest(4096).ok_or("cannot allocate environment")?;
    unsafe { mem::slice_mut(addr, env.len() as u32).copy_from_slice(&env) };
    m.env_linear = addr;
    unsafe { mem::wr16(psp + 0x2c, FLAT_DS) };
    Ok(())
}

/// `SR_STATE_TRACE=FILE` (diagnostics, with `SR_DETERMINISTIC`): one line per game frame
/// with the virtual tick and a hash of all guest data (image without code pages, DPMI
/// memory, the enlarged object lists; not the stacks), plus a hash per region, to find
/// the first frame where two runs differ.
pub fn trace_state(m: &Machine) {
    use std::io::Write;
    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Some(p) = std::env::var_os("SR_STATE_TRACE") {
            *FILE.lock().unwrap() = std::fs::File::create(p).ok();
        }
    });
    let mut g = FILE.lock().unwrap();
    let Some(f) = g.as_mut() else { return };
    let regions = regions_for_trace(m);
    let mut total: u64 = 0;
    let mut parts = Vec::new();
    for (base, size) in regions {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ base as u64;
        let mut page = base;
        while page < base + size {
            let n = 4096.min(base + size - page);
            if !m.img.is_code(page) && mem::readable(page, n) {
                let words = unsafe { std::slice::from_raw_parts(mem::ptr(page) as *const u32, n as usize / 4) };
                for &w in words {
                    h = (h ^ w as u64).wrapping_mul(0x0100_0000_01b3);
                }
            }
            page += n;
        }
        total = (total ^ h).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        parts.push(format!("{base:08x}:{h:016x}"));
    }
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    let _ = writeln!(f, "{frame} {} {total:016x} {}", crate::time::virtual_ticks(), parts.join(" "));
    // SR_STATE_DUMP=DIR,FRAME: the regions of that frame as files (to diff two runs)
    if let Some(spec) = std::env::var("SR_STATE_DUMP").ok() {
        if let Some((dir, n)) = spec.split_once(',') {
            if n.trim().parse::<u64>().ok() == Some(frame) {
                let _ = std::fs::create_dir_all(dir);
                for (base, size) in regions_for_trace(m) {
                    if mem::readable(base, size.min(4096)) {
                        let bytes = unsafe { mem::slice(base, size) };
                        let _ = std::fs::write(std::path::Path::new(dir).join(format!("{base:08x}.bin")), bytes);
                    }
                }
            }
        }
    }
}

fn regions_for_trace(m: &Machine) -> Vec<(u32, u32)> {
    let mut regions = vec![(m.img.base, m.img.size)];
    regions.extend(m.dpmi.regions());
    regions.extend(m.widescreen.object_lists.iter().copied());
    regions.sort();
    regions
}
