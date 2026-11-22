//! The boundary between host and guest code.
//!
//! * `enter` switches from the host thread to the guest: it saves the host callee-saved
//!   registers and ESP, loads the guest registers and jumps to the guest entry point.
//! * The trap layer leaves the guest by pointing EIP at `exit_trampoline`, which restores
//!   the saved host state and returns from `enter`.
//! * Hooks: a patched guest address jumps to a 10-byte stub (`push id; jmp common`), the
//!   common entry saves the guest registers as a `HookFrame`, calls `dispatch`, and resumes
//!   wherever the handler says.
//!
//! Only one guest thread exists, so the saved state lives in statics.

use core::arch::naked_asm;

use crate::cpu::HookFrame;

/// Initial guest register state for `enter`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct EntryRegs {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    pub esp: u32,
    pub eip: u32,
}

static mut HOST_ESP: u32 = 0;
static mut ENTRY: EntryRegs = EntryRegs { eax: 0, ebx: 0, ecx: 0, edx: 0, esi: 0, edi: 0, ebp: 0, esp: 0, eip: 0 };

#[unsafe(naked)]
unsafe extern "C" fn enter_asm() -> u32 {
    naked_asm!(
        "push ebp",
        "push ebx",
        "push esi",
        "push edi",
        "mov dword ptr [{host_esp}], esp",
        "mov esp, dword ptr [{entry} + 28]",
        "push dword ptr [{entry} + 32]",
        "mov eax, dword ptr [{entry} + 0]",
        "mov ebx, dword ptr [{entry} + 4]",
        "mov ecx, dword ptr [{entry} + 8]",
        "mov edx, dword ptr [{entry} + 12]",
        "mov esi, dword ptr [{entry} + 16]",
        "mov edi, dword ptr [{entry} + 20]",
        "mov ebp, dword ptr [{entry} + 24]",
        "cld",
        "ret",
        host_esp = sym HOST_ESP,
        entry = sym ENTRY,
    )
}

/// Guest code continues here (with EAX = exit status) when the program terminates.
#[unsafe(naked)]
pub unsafe extern "C" fn exit_trampoline() {
    naked_asm!(
        "mov esp, dword ptr [{host_esp}]",
        "pop edi",
        "pop esi",
        "pop ebx",
        "pop ebp",
        "cld",
        "ret",
        host_esp = sym HOST_ESP,
    )
}

/// Run the guest until it exits; returns its exit status.
///
/// # Safety
/// The guest image must be loaded and the trap layer installed; must be called on the
/// guest thread.
pub unsafe fn enter(regs: EntryRegs) -> u32 {
    unsafe {
        ENTRY = regs;
        enter_asm()
    }
}

/// Common hook entry. Stack on arrival: `[esp] = hook id`, guest state otherwise intact.
#[unsafe(naked)]
pub unsafe extern "C" fn hook_common() {
    naked_asm!(
        "pushfd",
        "pushad",
        "cld",
        "push esp",
        "call {dispatch}",
        "add esp, 4",
        "mov dword ptr [esp + 36], eax",
        "popad",
        "popfd",
        "ret",
        dispatch = sym hook_dispatch,
    )
}

/// Call guest code at `func` on another stack (the interrupt stack), with the guest's
/// x87 control word loaded for the duration. `mode` 0 = plain `call` (HMI event
/// callbacks), 1 = interrupt frame (`pushfd; push cs; call`) for handlers ending in `iretd`.
/// All host callee-saved registers and the host control word are restored.
#[unsafe(naked)]
pub unsafe extern "C" fn call_on_stack(func: u32, stack_top: u32, mode: u32, fpu_cw: u32) {
    naked_asm!(
        "push ebp",
        "push ebx",
        "push esi",
        "push edi",
        "mov eax, dword ptr [esp + 20]",
        "mov ecx, dword ptr [esp + 24]",
        "mov edx, dword ptr [esp + 28]",
        "mov ebx, esp",
        "mov esp, ecx",
        "push ebx",
        "sub esp, 4",
        "fnstcw word ptr [esp]",
        "push dword ptr [ebx + 32]",
        "fldcw word ptr [esp]",
        "add esp, 4",
        "cld",
        "test edx, edx",
        "jz 2f",
        "pushfd",
        ".byte 0x0e", // push cs (32-bit operand size: 4 bytes, as the CPU pushes for an interrupt)
        "call eax",
        "jmp 3f",
        "2:",
        "call eax",
        "3:",
        "cld",
        "fldcw word ptr [esp]",
        "add esp, 4",
        "pop esp",
        "pop edi",
        "pop esi",
        "pop ebx",
        "pop ebp",
        "ret",
    )
}

/// Call guest code at `func` from a hook handler (on the guest thread and stack) with
/// Watcom register arguments; returns EAX. The guest preserves the other registers.
#[unsafe(naked)]
pub unsafe extern "C" fn call_guest(func: u32, eax: u32, edx: u32, ebx: u32, ecx: u32) -> u32 {
    naked_asm!(
        "push ebp",
        "push ebx",
        "push esi",
        "push edi",
        "mov esi, dword ptr [esp + 20]",
        "mov eax, dword ptr [esp + 24]",
        "mov edx, dword ptr [esp + 28]",
        "mov ebx, dword ptr [esp + 32]",
        "mov ecx, dword ptr [esp + 36]",
        "cld",
        "call esi",
        "cld",
        "pop edi",
        "pop esi",
        "pop ebx",
        "pop ebp",
        "ret",
    )
}

extern "C" fn hook_dispatch(frame: *mut HookFrame) -> u32 {
    let frame = unsafe { &mut *frame };
    // The guest's x87 control word (precision and rounding of all its physics) must come
    // back unchanged from host code: drivers and runtime libraries called while handling
    // the hook may set their own.
    let mut cw: u16 = 0;
    unsafe { core::arch::asm!("fnstcw word ptr [{}]", in(reg) &mut cw as *mut u16) };
    let r = crate::hooks::dispatch(frame);
    let mut now: u16 = 0;
    unsafe { core::arch::asm!("fnstcw word ptr [{}]", in(reg) &mut now as *mut u16) };
    if now != cw {
        static REPORTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if REPORTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
            crate::warn!("x87 control word changed by host code during a hook: {cw:#06x} -> {now:#06x} (restored)");
        }
        unsafe { core::arch::asm!("fldcw word ptr [{}]", in(reg) &cw as *const u16) };
    }
    r
}

/// Set the Win32 TEB stack bounds of the current thread (NT_TIB StackBase / StackLimit) so
/// exception dispatch accepts the guest stack. Returns the previous bounds.
pub unsafe fn set_stack_bounds(base: u32, limit: u32) -> (u32, u32) {
    let (old_base, old_limit): (u32, u32);
    unsafe {
        core::arch::asm!(
            "mov {ob}, dword ptr fs:[4]",
            "mov {ol}, dword ptr fs:[8]",
            "mov dword ptr fs:[4], {b}",
            "mov dword ptr fs:[8], {l}",
            ob = out(reg) old_base,
            ol = out(reg) old_limit,
            b = in(reg) base,
            l = in(reg) limit,
        );
    }
    (old_base, old_limit)
}
