//! The trap layer: a vectored exception handler that completes every instruction the
//! original code cannot execute in a Windows user-mode process.
//!
//! | Instruction | Exception | Handling |
//! |---|---|---|
//! | `int n` | access violation (#GP) | DOS/DPMI/BIOS service, then continue after it |
//! | `in`, `out`, `ins*`, `outs*` | privileged instruction | virtual I/O ports |
//! | `cli`, `sti`, `hlt` | privileged instruction | virtual interrupt flag / idle |
//! | segment loads of DOS/4GW selectors | access violation (#GP) | emulated descriptors (`sel`) |
//! | access below 0x10000 | access violation (#PF) | re-executed against virtual conventional memory |
//!
//! The last case does not emulate the instruction: it is re-encoded with the translated
//! host address into a scratch buffer and executed natively, so its semantics and flags
//! are exactly the CPU's.

use iced_x86::{Decoder, DecoderOptions, Encoder, Formatter, Instruction, IntelFormatter, Mnemonic, OpKind, Register};
use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_BREAKPOINT, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_INT_DIVIDE_BY_ZERO,
    EXCEPTION_PRIV_INSTRUCTION, EXCEPTION_SINGLE_STEP,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS, EXCEPTION_RECORD,
};

use crate::cpu::{Cpu, R32};
use crate::mem::{self, FLAT_DS};
use crate::sel::Seg;
use crate::{Machine, services};

const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

pub fn install() {
    unsafe {
        AddVectoredExceptionHandler(1, Some(veh));
        windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(last_chance));
    }
}

/// Anything that reaches here is a runtime bug: log it before Windows ends the process.
unsafe extern "system" fn last_chance(ep: *const EXCEPTION_POINTERS) -> i32 {
    let (rec, ctx) = unsafe { (&*(*ep).ExceptionRecord, &*(*ep).ContextRecord) };
    crate::error!(
        "UNHANDLED host exception {:#010x} at eip {:#010x} (address {:#x}); {}",
        rec.ExceptionCode,
        ctx.Eip,
        if rec.NumberParameters >= 2 { rec.ExceptionInformation[1] } else { 0 },
        ctx.dump()
    );
    crate::error!("  at {}", module_of(ctx.Eip));
    // on disk before anything that could fault again (a second fault ends the process)
    crate::log::flush();
    let thread = std::thread::current();
    crate::error!("  thread {:?} (id {})", thread.name().unwrap_or("?"), unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() });
    // code addresses on the stack (frame pointers are not reliable inside system DLLs): the
    // likely callers, as module+offset, nearest first
    let mut code = Vec::new();
    for i in 0..256u32 {
        let a = ctx.Esp.wrapping_add(i * 4);
        if !mem::readable(a, 4) {
            break;
        }
        let w = unsafe { mem::rd32(a) };
        let m = module_of(w);
        if m.contains('+') && !code.contains(&m) {
            code.push(m);
            if code.len() >= 24 {
                break;
            }
        }
    }
    crate::error!("  code on the stack: {}", code.join(" "));
    crate::log::flush();
    // host frame chain (release builds keep frame pointers for the runtime crates)
    let mut ebp = ctx.Ebp;
    for _ in 0..12 {
        if ebp < 0x10000 || ebp & 3 != 0 || !mem::readable(ebp, 8) {
            break;
        }
        let ret = unsafe { mem::rd32(ebp + 4) };
        crate::error!("  called from {}", module_of(ret));
        ebp = unsafe { mem::rd32(ebp) };
    }
    if let Some(m) = crate::try_machine() {
        crate::error!("  guest eip region: {}", m.where_(ctx.Eip));
        let words: Vec<String> = (0..12).map(|i| format!("{:08x}", unsafe { mem::rd32(ctx.Esp + i * 4) })).collect();
        crate::error!("  stack: {}", words.join(" "));
        crate::report_stats(m);
    }
    crate::log::flush();
    0
}

/// "module.dll+offset" for a host code address.
pub fn module_of(addr: u32) -> String {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW,
    };
    unsafe {
        let mut h = std::ptr::null_mut();
        if GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, addr as usize as *const u16, &mut h) == 0 {
            return format!("{addr:#010x}");
        }
        let mut buf = [0u16; 260];
        let n = GetModuleFileNameW(h, buf.as_mut_ptr(), buf.len() as u32) as usize;
        let path = String::from_utf16_lossy(&buf[..n]);
        let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
        format!("{name}+{:#x}", addr - h as usize as u32)
    }
}

unsafe extern "system" fn veh(ep: *mut EXCEPTION_POINTERS) -> i32 {
    let Some(m) = crate::try_machine() else { return EXCEPTION_CONTINUE_SEARCH };
    let (rec, ctx) = unsafe { (&*(*ep).ExceptionRecord, &mut *(*ep).ContextRecord) };
    let eip = ctx.Eip;
    if m.step_trace.as_ref().is_some_and(|t| t.active) {
        if rec.ExceptionCode == EXCEPTION_SINGLE_STEP || rec.ExceptionCode == 0x4000_001e {
            return step_trace(m, ctx);
        }
        static SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
            crate::info!("while tracing: exception {:#x} at {}", rec.ExceptionCode, m.where_(eip));
        }
    }
    if !(m.img.contains(eip) || m.arena.contains(eip)) {
        // Not guest code. Frame-based handlers are skipped while ESP is on the guest stack,
        // so report host faults here before passing them on.
        if rec.ExceptionCode == EXCEPTION_ACCESS_VIOLATION || rec.ExceptionCode == EXCEPTION_ILLEGAL_INSTRUCTION {
            static REPORTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if REPORTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
                unsafe { last_chance(ep) };
            }
        }
        return EXCEPTION_CONTINUE_SEARCH;
    }
    m.stats.traps += 1;
    *m.stats.sites.entry(eip).or_default() += 1;
    if rec.ExceptionCode == EXCEPTION_SINGLE_STEP {
        return step_trace(m, ctx);
    }
    let r = match rec.ExceptionCode {
        EXCEPTION_ACCESS_VIOLATION | EXCEPTION_PRIV_INSTRUCTION | EXCEPTION_ILLEGAL_INSTRUCTION => handle(m, ctx, rec),
        EXCEPTION_BREAKPOINT if m.segjit.redirect.contains_key(&eip) => {
            // a jump into an instruction that was moved into a translation stub
            ctx.Eip = m.segjit.redirect[&eip];
            Ok(())
        }
        EXCEPTION_BREAKPOINT if m.seg_sites.contains_key(&eip) => {
            step_trace_maybe_start(m, ctx);
            segment_site(m, ctx)
        }
        EXCEPTION_BREAKPOINT => Err("guest reached a poisoned (hooked) byte or int3".into()),
        EXCEPTION_INT_DIVIDE_BY_ZERO => Err("guest divide error".into()),
        c => Err(format!("unexpected exception {c:#010x}")),
    };
    match r {
        Ok(()) => EXCEPTION_CONTINUE_EXECUTION,
        Err(e) => crate::fatal(m, ctx, &e),
    }
}

/// `SR_SEG_TRACE=n`: log the first n segment events after frame 200.
fn seg_trace_on() -> bool {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LEFT: AtomicI64 = AtomicI64::new(-1);
    let mut left = LEFT.load(Ordering::Relaxed);
    if left == -1 {
        left = std::env::var("SR_SEG_TRACE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        LEFT.store(left, Ordering::Relaxed);
    }
    let frames = crate::try_machine().and_then(|m| m.glide.as_ref().map(|g| g.frames)).unwrap_or(0);
    left > 0 && frames >= 200 && LEFT.fetch_sub(1, Ordering::Relaxed) > 0
}

/// Diagnostic execution tracer (`SR_STEP_TRACE=<steps>`): starting at the first patched
/// segment site after frame 100, single-step the guest and record every executed image
/// address; the set is written to `logs/executed.txt` at exit. Used to build exact patch
/// lists for the hand-written asm modules (code vs. data cannot be told apart statically).
pub struct StepTrace {
    pub budget: u64,
    pub active: bool,
    pub seen: std::collections::BTreeSet<u32>,
}

const TF: u32 = 0x100;

fn step_trace_maybe_start(m: &mut Machine, ctx: &mut CONTEXT) {
    let frames = m.glide.as_ref().map_or(0, |g| g.frames);
    if let Some(t) = &mut m.step_trace {
        if !t.active && t.budget > 0 && frames >= 100 {
            t.active = true;
            crate::info!("step trace started at frame {frames}");
            ctx.EFlags |= TF;
        }
    }
}

fn step_trace(m: &mut Machine, ctx: &mut CONTEXT) -> i32 {
    let Some(t) = &mut m.step_trace else { return EXCEPTION_CONTINUE_SEARCH };
    if !t.active {
        ctx.EFlags &= !TF;
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    if let Some(l) = m.img.to_link(ctx.Eip) {
        t.seen.insert(l);
    }
    t.budget = t.budget.saturating_sub(1);
    if t.budget == 0 {
        t.active = false;
        ctx.EFlags &= !TF;
        crate::info!("step trace finished: {} distinct addresses", t.seen.len());
    } else {
        ctx.EFlags |= TF;
    }
    EXCEPTION_CONTINUE_EXECUTION
}

/// A patched segment-override instruction (`fs:`/`gs:`/`es:` with an emulated selector):
/// re-execute the original instruction at its real linear address
/// (virtual segment base + offset).
fn segment_site(m: &mut Machine, ctx: &mut CONTEXT) -> Result<(), String> {
    let eip = ctx.Eip;
    if let Some(stub) = crate::segjit::translate(m, eip) {
        ctx.Eip = stub;
        return Ok(());
    }
    let orig = m.seg_sites[&eip];
    let ins = Decoder::with_ip(32, &orig, eip as u64, DecoderOptions::NONE).decode();
    if ins.is_invalid() {
        return Err("undecodable segment site".into());
    }
    m.stats.seg_site_hits += 1;
    let next = eip + ins.len() as u32;
    if sreg_instruction(m, ctx, &ins, next)? {
        return Ok(());
    }
    if seg_trace_on() {
        crate::info!("SEG site {} fs={:#x} gs={:#x} es={:#x}", m.where_(eip), m.sel.base(Seg::Fs), m.sel.base(Seg::Gs), m.sel.base(Seg::Es));
    }
    reexecute_low(m, ctx, &ins, eip, next)
}

/// Emulate a segment-register save, restore or read with the *virtual* selector values,
/// so code that saves an emulated selector and restores it later (push fs ... pop fs)
/// leaves the virtual state exactly as the CPU's registers would be. Returns false if `ins`
/// is not such an instruction.
fn sreg_instruction(m: &mut Machine, ctx: &mut CONTEXT, ins: &Instruction, next: u32) -> Result<bool, String> {
    let op_seg = |i: u32| if ins.op_kind(i) == OpKind::Register { seg_of(ins.op_register(i)) } else { None };
    match ins.mnemonic() {
        Mnemonic::Push => {
            let Some(seg) = op_seg(0) else { return Ok(false) };
            let sel = m.sel.virt_sel(seg) as u32;
            let size = ins.stack_pointer_increment().unsigned_abs();
            ctx.Esp = ctx.Esp.wrapping_sub(size);
            unsafe {
                if size == 2 {
                    mem::wr16(ctx.Esp, sel as u16)
                } else {
                    mem::wr32(ctx.Esp, sel)
                }
            };
        }
        Mnemonic::Pop => {
            let Some(seg) = op_seg(0) else { return Ok(false) };
            let sel = unsafe { mem::rd16(ctx.Esp) };
            ctx.Esp = ctx.Esp.wrapping_add(ins.stack_pointer_increment() as u32);
            m.sel.load(seg, sel).map_err(|e| format!("{e} (pop at {})", m.where_(ctx.Eip)))?;
            set_real_seg(ctx, seg);
        }
        Mnemonic::Mov if op_seg(0).is_some() => {
            let seg = op_seg(0).unwrap();
            let sel = match ins.op1_kind() {
                OpKind::Register => reg_value(ctx, ins.op1_register()).ok_or("bad selector source")? as u16,
                _ => {
                    let a = operand_address(m, ctx, ins, 1).ok_or("bad selector address")?;
                    unsafe { mem::rd16(a) }
                }
            };
            m.sel.load(seg, sel).map_err(|e| format!("{e} (mov at {})", m.where_(ctx.Eip)))?;
            set_real_seg(ctx, seg);
            if seg_trace_on() {
                crate::info!("SEG load {} <- {sel:#06x} base {:#x} at {}", seg.name(), m.sel.base(seg), m.where_(ctx.Eip));
            }
        }
        Mnemonic::Mov if op_seg(1).is_some() => {
            let sel = m.sel.virt_sel(op_seg(1).unwrap());
            match ins.op0_kind() {
                OpKind::Register => {
                    let r = ins.op0_register();
                    // a 32-bit destination receives the zero-extended selector
                    let v = if r32_of(r).is_some_and(|x| x.1 == 4) { sel as u32 } else { sel as u32 };
                    write_reg(ctx, r, v)?;
                }
                _ => {
                    let a = operand_address(m, ctx, ins, 0).ok_or("bad selector store address")?;
                    unsafe { mem::wr16(a, sel) };
                }
            }
        }
        _ => return Ok(false),
    }
    ctx.Eip = next;
    Ok(true)
}

impl Machine {
    /// Make the instruction at `link_addr` (which must start with a segment-override prefix)
    /// trap on every execution, so it is completed with the emulated segment's base.
    pub fn patch_segment_site(&mut self, link_addr: u32, expect: &[u8]) -> Result<(), String> {
        let addr = self.img.link(link_addr);
        let actual = unsafe { mem::slice(addr, expect.len() as u32) };
        if actual != expect && actual != self.img.relocate_expected(link_addr, expect).as_slice() {
            return Err(format!("segment site {link_addr:#x}: original bytes differ ({})", crate::hooks::hex(actual)));
        }
        let mut orig = [0u8; 16];
        orig.copy_from_slice(unsafe { mem::slice(addr, 16) });
        self.seg_sites.insert(addr, orig);
        unsafe { mem::wr8(addr, 0xcc) };
        Ok(())
    }
}

/// Would the instruction at `ctx.Eip` raise one of the traps this layer handles?
///
/// Used by the interrupt injector on a suspended thread: if the guest stopped on such an
/// instruction, the exception may already be in flight inside Windows, and changing the
/// thread context then would corrupt it. Must not allocate.
pub fn would_trap(ctx: &CONTEXT) -> bool {
    let ins = decode(ctx.Eip);
    if ins.is_invalid() {
        return true;
    }
    match ins.mnemonic() {
        Mnemonic::Int | Mnemonic::Int1 | Mnemonic::Int3 | Mnemonic::Into | Mnemonic::In | Mnemonic::Out | Mnemonic::Insb
        | Mnemonic::Insw | Mnemonic::Insd | Mnemonic::Outsb | Mnemonic::Outsw | Mnemonic::Outsd | Mnemonic::Cli
        | Mnemonic::Sti | Mnemonic::Hlt | Mnemonic::Lds | Mnemonic::Les | Mnemonic::Lfs | Mnemonic::Lgs | Mnemonic::Lss
        | Mnemonic::Iretd | Mnemonic::Iret => return true,
        _ => {}
    }
    if segment_load_target(&ins).is_some() {
        return true;
    }
    for op in 0..ins.op_count() {
        let kind = ins.op_kind(op);
        let is_mem = matches!(
            kind,
            OpKind::Memory | OpKind::MemorySegSI | OpKind::MemorySegESI | OpKind::MemorySegDI | OpKind::MemorySegEDI | OpKind::MemoryESDI | OpKind::MemoryESEDI
        );
        if is_mem {
            let addr = ins.virtual_address(op, 0, |reg, _, _| if seg_of(reg).is_some() { Some(0) } else { reg_value(ctx, reg).map(|v| v as u64) });
            if addr.is_none_or(|a| a < 0x10000) {
                return true;
            }
        }
    }
    false
}

pub fn decode(addr: u32) -> Instruction {
    let bytes = unsafe { mem::slice(addr, 16) };
    Decoder::with_ip(32, bytes, addr as u64, DecoderOptions::NONE).decode()
}

pub fn format(ins: &Instruction) -> String {
    let mut s = String::new();
    IntelFormatter::new().format(ins, &mut s);
    s
}

fn reg_value(ctx: &CONTEXT, r: Register) -> Option<u32> {
    let full = |r32: R32| ctx.get(r32);
    Some(match r {
        Register::EAX => full(R32::Eax),
        Register::ECX => full(R32::Ecx),
        Register::EDX => full(R32::Edx),
        Register::EBX => full(R32::Ebx),
        Register::ESP => full(R32::Esp),
        Register::EBP => full(R32::Ebp),
        Register::ESI => full(R32::Esi),
        Register::EDI => full(R32::Edi),
        Register::AX => full(R32::Eax) & 0xffff,
        Register::CX => full(R32::Ecx) & 0xffff,
        Register::DX => full(R32::Edx) & 0xffff,
        Register::BX => full(R32::Ebx) & 0xffff,
        Register::SP => full(R32::Esp) & 0xffff,
        Register::BP => full(R32::Ebp) & 0xffff,
        Register::SI => full(R32::Esi) & 0xffff,
        Register::DI => full(R32::Edi) & 0xffff,
        Register::AL => full(R32::Eax) & 0xff,
        Register::CL => full(R32::Ecx) & 0xff,
        Register::DL => full(R32::Edx) & 0xff,
        Register::BL => full(R32::Ebx) & 0xff,
        Register::AH => (full(R32::Eax) >> 8) & 0xff,
        Register::CH => (full(R32::Ecx) >> 8) & 0xff,
        Register::DH => (full(R32::Edx) >> 8) & 0xff,
        Register::BH => (full(R32::Ebx) >> 8) & 0xff,
        _ => return None,
    })
}

fn r32_of(r: Register) -> Option<(R32, u32)> {
    // (full register, width in bytes) — 8-bit high registers are handled by callers
    Some(match r {
        Register::EAX => (R32::Eax, 4),
        Register::ECX => (R32::Ecx, 4),
        Register::EDX => (R32::Edx, 4),
        Register::EBX => (R32::Ebx, 4),
        Register::ESP => (R32::Esp, 4),
        Register::EBP => (R32::Ebp, 4),
        Register::ESI => (R32::Esi, 4),
        Register::EDI => (R32::Edi, 4),
        Register::AX => (R32::Eax, 2),
        Register::CX => (R32::Ecx, 2),
        Register::DX => (R32::Edx, 2),
        Register::BX => (R32::Ebx, 2),
        Register::SP => (R32::Esp, 2),
        Register::BP => (R32::Ebp, 2),
        Register::SI => (R32::Esi, 2),
        Register::DI => (R32::Edi, 2),
        Register::AL => (R32::Eax, 1),
        Register::CL => (R32::Ecx, 1),
        Register::DL => (R32::Edx, 1),
        Register::BL => (R32::Ebx, 1),
        _ => return None,
    })
}

fn write_reg(ctx: &mut CONTEXT, r: Register, v: u32) -> Result<(), String> {
    let (full, w) = r32_of(r).ok_or_else(|| format!("unsupported destination register {r:?}"))?;
    let old = ctx.get(full);
    let new = match w {
        4 => v,
        2 => (old & 0xffff_0000) | (v & 0xffff),
        _ => (old & 0xffff_ff00) | (v & 0xff),
    };
    ctx.set(full, new);
    Ok(())
}

fn seg_of(r: Register) -> Option<Seg> {
    Some(match r {
        Register::ES => Seg::Es,
        Register::CS => Seg::Cs,
        Register::SS => Seg::Ss,
        Register::DS => Seg::Ds,
        Register::FS => Seg::Fs,
        Register::GS => Seg::Gs,
        _ => return None,
    })
}

fn set_real_seg(ctx: &mut CONTEXT, seg: Seg) {
    // WOW64 forces these back to the flat selectors anyway; FS must stay on the TEB.
    match seg {
        Seg::Es => ctx.SegEs = FLAT_DS as u32,
        Seg::Ds => ctx.SegDs = FLAT_DS as u32,
        Seg::Gs => ctx.SegGs = FLAT_DS as u32,
        Seg::Ss => ctx.SegSs = FLAT_DS as u32,
        Seg::Fs | Seg::Cs => {}
    }
}

/// Effective (virtual linear) address of memory operand `op`, using virtual segment bases.
fn operand_address(m: &Machine, ctx: &CONTEXT, ins: &Instruction, op: u32) -> Option<u32> {
    ins.virtual_address(op, 0, |reg, _, _| {
        if let Some(s) = seg_of(reg) {
            Some(m.sel.base(s) as u64)
        } else {
            reg_value(ctx, reg).map(|v| v as u64)
        }
    })
    .map(|a| a as u32)
}

fn handle(m: &mut Machine, ctx: &mut CONTEXT, rec: &EXCEPTION_RECORD) -> Result<(), String> {
    let eip = ctx.Eip;
    let ins = decode(eip);
    if ins.is_invalid() {
        return Err("undecodable instruction".into());
    }
    let next = eip + ins.len() as u32;
    let fault_addr = if rec.ExceptionCode == EXCEPTION_ACCESS_VIOLATION && rec.NumberParameters >= 2 {
        Some(rec.ExceptionInformation[1] as u32)
    } else {
        None
    };

    match ins.mnemonic() {
        Mnemonic::Int => {
            ctx.Eip = next;
            return services::interrupt(m, ctx, ins.immediate8());
        }
        Mnemonic::In => {
            let port = if ins.op1_kind() == OpKind::Immediate8 { ins.immediate8() as u16 } else { ctx.Edx as u16 };
            let width = r32_of(ins.op0_register()).map(|x| x.1).unwrap_or(1);
            let v = m.io_read(port, width);
            write_reg(ctx, ins.op0_register(), v)?;
            ctx.Eip = next;
            return Ok(());
        }
        Mnemonic::Out => {
            let port = if ins.op0_kind() == OpKind::Immediate8 { ins.immediate8() as u16 } else { ctx.Edx as u16 };
            let (r, width) = r32_of(ins.op1_register()).ok_or("bad out operand")?;
            let v = ctx.get(r) & if width == 4 { u32::MAX } else { (1u32 << (width * 8)) - 1 };
            m.io_write(port, width, v);
            ctx.Eip = next;
            return Ok(());
        }
        Mnemonic::Insb | Mnemonic::Insw | Mnemonic::Insd | Mnemonic::Outsb | Mnemonic::Outsw | Mnemonic::Outsd => {
            return string_io(m, ctx, &ins, next);
        }
        Mnemonic::Cli => {
            m.set_vif(false);
            ctx.Eip = next;
            return Ok(());
        }
        Mnemonic::Sti => {
            m.set_vif(true);
            ctx.Eip = next;
            return Ok(());
        }
        Mnemonic::Hlt => {
            m.idle();
            ctx.Eip = next;
            return Ok(());
        }
        _ => {}
    }

    // Segment register loads.
    if let Some(seg) = segment_load_target(&ins) {
        // WoW64 can report a data address for a selector exception. When
        // the FS/GS source is a word wholly inside the loaded program, it
        // was the selector load, not the memory read, that needs emulation.
        let image_selector = matches!(seg, Seg::Fs | Seg::Gs)
            && ins.mnemonic() == Mnemonic::Mov && ins.op1_kind() == OpKind::Memory
            && operand_address(m, ctx, &ins, 1).is_some_and(|src|
                m.img.contains(src) && src.checked_add(1).is_some_and(|last| m.img.contains(last)));
        if fault_addr.is_none_or(|a| a == u32::MAX) || image_selector {
            if let Some(stub) = crate::segjit::translate(m, eip) {
                ctx.Eip = stub;
                return Ok(());
            }
            return load_segment(m, ctx, &ins, seg, next);
        }
    }

    // Faulting access into the never-mapped first 64 KB: virtual conventional memory.
    if let Some(a) = fault_addr {
        // Wine WoW64 can report a segment fault with no linear address. The
        // DOS keyboard initialization writes the BIOS data area through GS.
        // Resolve it only if the decoded operand actually falls in low RAM.
        if a == u32::MAX && ins.segment_prefix() == Register::GS {
            let low = (0..ins.op_count()).filter(|&i| ins.op_kind(i) == OpKind::Memory)
                .filter_map(|i| operand_address(m, ctx, &ins, i))
                .any(|va| va < 0x10000 && m.lowmem.translate(va).is_some());
            if low {
                return reexecute_low(m, ctx, &ins, eip, next);
            }
        }
        if a < 0x10000 {
            // an access through a segment with a virtual base (not conventional memory)
            let based = Seg::from_register(ins.memory_segment()).is_some_and(|s| m.sel.base(s) != 0);
            if based {
                if let Some(stub) = crate::segjit::translate(m, eip) {
                    ctx.Eip = stub;
                    return Ok(());
                }
            }
            return reexecute_low(m, ctx, &ins, eip, next);
        }
        return Err(format!("access violation at {a:#010x} ({})", if rec.ExceptionInformation[0] == 1 { "write" } else { "read" }));
    }
    Err(format!("unhandled privileged instruction ({:?})", ins.mnemonic()))
}

fn segment_load_target(ins: &Instruction) -> Option<Seg> {
    match ins.mnemonic() {
        Mnemonic::Mov if ins.op0_kind() == OpKind::Register => seg_of(ins.op0_register()),
        Mnemonic::Pop if ins.op0_kind() == OpKind::Register => seg_of(ins.op0_register()),
        Mnemonic::Lds => Some(Seg::Ds),
        Mnemonic::Les => Some(Seg::Es),
        Mnemonic::Lfs => Some(Seg::Fs),
        Mnemonic::Lgs => Some(Seg::Gs),
        Mnemonic::Lss => Some(Seg::Ss),
        _ => None,
    }
}

fn load_segment(m: &mut Machine, ctx: &mut CONTEXT, ins: &Instruction, seg: Seg, next: u32) -> Result<(), String> {
    let sel: u16 = match ins.mnemonic() {
        Mnemonic::Mov => match ins.op1_kind() {
            OpKind::Register => reg_value(ctx, ins.op1_register()).ok_or("bad selector source")? as u16,
            OpKind::Memory => {
                let a = operand_address(m, ctx, ins, 1).ok_or("bad selector address")?;
                unsafe { mem::rd16(a) }
            }
            k => return Err(format!("selector load from {k:?}")),
        },
        Mnemonic::Pop => {
            let v = unsafe { mem::rd16(ctx.Esp) };
            ctx.Esp = ctx.Esp.wrapping_add(ins.stack_pointer_increment() as u32);
            v
        }
        _ => {
            // lds/les/lfs/lgs/lss r32, m16:32
            let a = operand_address(m, ctx, ins, 1).ok_or("bad far pointer address")?;
            let (off, sel) = unsafe { (mem::rd32(a), mem::rd16(a + 4)) };
            write_reg(ctx, ins.op0_register(), off)?;
            sel
        }
    };
    let d = m.sel.load(seg, sel).map_err(|e| format!("{e} ({})", format(ins)))?;
    if seg_trace_on() {
        crate::info!("SEG load {} <- {sel:#06x} base {:#x} at {}", seg.name(), d.base, m.where_(ctx.Eip));
    }
    if d.base != 0 {
        crate::debug!(
            "{} <- {sel:#06x} (virtual base {:#x}) at {}",
            seg.name(),
            d.base,
            m.where_(ctx.Eip)
        );
    }
    if seg == Seg::Fs {
        // FS itself must keep the TEB; the selector's base is tracked virtually for the
        // patched segment sites. Expected at 82f32 (engine entry), so only noted once.
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| crate::info!("guest loaded FS with {sel:#06x} at {}; FS stays on the Win32 TEB (base tracked virtually)", m.where_(ctx.Eip)));
    }
    set_real_seg(ctx, seg);
    ctx.Eip = next;
    Ok(())
}

/// Translate a virtual linear address below 0x10000 to host memory.
fn translate_low(m: &mut Machine, linear: u32) -> Result<u32, String> {
    m.lowmem.translate(linear).ok_or_else(|| format!("access to unmapped conventional memory {linear:#x}"))
}

fn reexecute_low(m: &mut Machine, ctx: &mut CONTEXT, ins: &Instruction, eip: u32, next: u32) -> Result<(), String> {
    let scratch = m.scratch;
    if eip >= scratch && eip < scratch + 256 {
        return Err(format!("fault inside conventional memory translation: {}", format(ins)));
    }
    let mut code: Vec<u8> = Vec::with_capacity(48);
    let is_string = matches!(
        ins.mnemonic(),
        Mnemonic::Movsb
            | Mnemonic::Movsw
            | Mnemonic::Movsd
            | Mnemonic::Stosb
            | Mnemonic::Stosw
            | Mnemonic::Stosd
            | Mnemonic::Lodsb
            | Mnemonic::Lodsw
            | Mnemonic::Lodsd
            | Mnemonic::Cmpsb
            | Mnemonic::Cmpsw
            | Mnemonic::Cmpsd
            | Mnemonic::Scasb
            | Mnemonic::Scasw
            | Mnemonic::Scasd
    ) && !(0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory);

    if is_string {
        // lea esi,[esi+d1] / lea edi,[edi+d2] ; original (without segment prefix) ; undo ; jmp next
        let src_seg = match ins.segment_prefix() {
            Register::None => Seg::Ds,
            r => seg_of(r).unwrap_or(Seg::Ds),
        };
        let uses_esi = !matches!(ins.mnemonic(), Mnemonic::Stosb | Mnemonic::Stosw | Mnemonic::Stosd | Mnemonic::Scasb | Mnemonic::Scasw | Mnemonic::Scasd);
        let uses_edi = !matches!(ins.mnemonic(), Mnemonic::Lodsb | Mnemonic::Lodsw | Mnemonic::Lodsd);
        let mut d_esi = 0u32;
        let mut d_edi = 0u32;
        if uses_esi {
            let va = m.sel.base(src_seg).wrapping_add(ctx.Esi);
            if va < 0x10000 {
                d_esi = translate_low(m, va)?.wrapping_sub(ctx.Esi);
            } else {
                d_esi = m.sel.base(src_seg);
            }
        }
        if uses_edi {
            let va = m.sel.base(Seg::Es).wrapping_add(ctx.Edi);
            if va < 0x10000 {
                d_edi = translate_low(m, va)?.wrapping_sub(ctx.Edi);
            } else {
                d_edi = m.sel.base(Seg::Es);
            }
        }
        let lea = |code: &mut Vec<u8>, modrm: u8, d: u32| {
            if d != 0 {
                code.extend_from_slice(&[0x8d, modrm]);
                code.extend_from_slice(&d.to_le_bytes());
            }
        };
        lea(&mut code, 0xb6, d_esi); // lea esi,[esi+disp32]
        lea(&mut code, 0xbf, d_edi); // lea edi,[edi+disp32]
        let raw = unsafe { mem::slice(eip, ins.len() as u32) };
        code.extend(raw.iter().copied().filter(|b| !matches!(b, 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65)));
        lea(&mut code, 0xb6, d_esi.wrapping_neg());
        lea(&mut code, 0xbf, d_edi.wrapping_neg());
    } else {
        let op = (0..ins.op_count())
            .find(|&i| ins.op_kind(i) == OpKind::Memory)
            .ok_or_else(|| format!("low-memory fault in instruction without memory operand: {}", format(ins)))?;
        if matches!(ins.mnemonic(), Mnemonic::Call | Mnemonic::Jmp) {
            return Err(format!("indirect branch through conventional memory: {}", format(ins)));
        }
        let va = operand_address(m, ctx, ins, op).ok_or("cannot compute address")?;
        let host = if va < 0x10000 { translate_low(m, va)? } else { va };
        let mut ni = *ins;
        ni.set_memory_base(Register::None);
        ni.set_memory_index(Register::None);
        ni.set_memory_index_scale(1);
        ni.set_memory_displacement32(host);
        ni.set_memory_displ_size(4);
        ni.set_segment_prefix(Register::None);
        let mut enc = Encoder::new(32);
        enc.encode(&ni, scratch as u64).map_err(|e| format!("re-encode failed: {e}"))?;
        code.extend_from_slice(&enc.take_buffer());
        crate::trace!("low access {va:#x} -> {host:#x} by {} at {}", format(ins), m.where_(eip));
    }
    let jmp_at = scratch + code.len() as u32;
    code.extend_from_slice(&crate::hooks::jmp_rel32(jmp_at, next));
    unsafe { mem::slice_mut(scratch, code.len() as u32).copy_from_slice(&code) };
    m.stats.low_accesses += 1;
    ctx.Eip = scratch;
    Ok(())
}

fn string_io(m: &mut Machine, ctx: &mut CONTEXT, ins: &Instruction, next: u32) -> Result<(), String> {
    let width = match ins.mnemonic() {
        Mnemonic::Insb | Mnemonic::Outsb => 1,
        Mnemonic::Insw | Mnemonic::Outsw => 2,
        _ => 4,
    };
    let rep = ins.has_rep_prefix();
    let mut count = if rep { ctx.Ecx } else { 1 };
    let port = ctx.Edx as u16;
    let step = if ctx.EFlags & crate::cpu::DF != 0 { (width as u32).wrapping_neg() } else { width as u32 };
    let is_in = matches!(ins.mnemonic(), Mnemonic::Insb | Mnemonic::Insw | Mnemonic::Insd);
    while count > 0 {
        unsafe {
            if is_in {
                let v = m.io_read(port, width);
                match width {
                    1 => mem::wr8(ctx.Edi, v as u8),
                    2 => mem::wr16(ctx.Edi, v as u16),
                    _ => mem::wr32(ctx.Edi, v),
                }
                ctx.Edi = ctx.Edi.wrapping_add(step);
            } else {
                let v = match width {
                    1 => mem::rd8(ctx.Esi) as u32,
                    2 => mem::rd16(ctx.Esi) as u32,
                    _ => mem::rd32(ctx.Esi),
                };
                m.io_write(port, width, v);
                ctx.Esi = ctx.Esi.wrapping_add(step);
            }
        }
        count -= 1;
    }
    if rep {
        ctx.Ecx = 0;
    }
    ctx.Eip = next;
    Ok(())
}
