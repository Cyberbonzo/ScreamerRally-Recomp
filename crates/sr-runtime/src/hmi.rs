//! HMI Sound Operating System (SOS) 4.x, replaced at API level.
//!
//! The original library programs the PIT, hooks IRQ0, loads sound-card drivers from
//! `HMIDRV.386` and pokes descriptor tables directly. None of that can (or should) run on
//! Windows, so its public functions are replaced. This module starts with the timer
//! service, which drives the game clock; the digital and MIDI services follow with the
//! audio mixer.
//!
//! Calling convention: Watcom register calls, arguments in EAX, EDX, EBX, ECX; result in EAX.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

const SOS_NO_ERROR: u32 = 0;
const SOS_INVALID_HANDLE: u32 = 10;

/// Callers at or above this link address are HMI library code, not the game.
pub const LIBRARY_START: u32 = 0xb0000;

fn caller_link(m: &Machine, f: &HookFrame) -> u32 {
    m.img.to_link(f.stack(0)).unwrap_or(0)
}

/// `sosTIMERInitSystem(rate, flags)`
pub fn timer_init(_m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    crate::debug!("sosTIMERInitSystem(rate {}, flags {:#x})", f.eax, f.edx);
    f.eax = SOS_NO_ERROR;
    HookResult::Return(0)
}

/// `sosTIMERUnInitSystem(flags)`
pub fn timer_uninit(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    crate::debug!("sosTIMERUnInitSystem");
    if let Some(s) = &m.sched {
        s.shared.lock().unwrap().events.clear();
    }
    f.eax = SOS_NO_ERROR;
    HookResult::Return(0)
}

/// `sosTIMERRegisterEvent(rate, callback, &handle)`
pub fn timer_register(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    let (rate, cb, out) = (f.eax, f.edx, f.ebx);
    let caller = caller_link(m, f);
    let internal = caller >= LIBRARY_START;
    let sched = m.sched.as_ref().expect("scheduler");
    let handle = sched.register(rate, cb, internal);
    unsafe { mem::wr32(out, handle) };
    crate::info!(
        "sosTIMERRegisterEvent({rate} Hz, {}) from link {caller:#x} -> handle {handle}{}",
        m.where_(cb),
        if internal { " [HMI driver service: held until audio HLE]" } else { "" }
    );
    f.eax = SOS_NO_ERROR;
    HookResult::Return(0)
}

/// `sosTIMERAlterEventRate(handle, rate)`
pub fn timer_alter(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    let ok = m.sched.as_ref().expect("scheduler").alter_rate(f.eax, f.edx);
    crate::debug!("sosTIMERAlterEventRate(handle {}, {} Hz)", f.eax, f.edx);
    f.eax = if ok { SOS_NO_ERROR } else { SOS_INVALID_HANDLE };
    HookResult::Return(0)
}

/// `sosTIMERRemoveEvent(handle)`
pub fn timer_remove(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    let ok = m.sched.as_ref().expect("scheduler").remove(f.eax);
    crate::debug!("sosTIMERRemoveEvent(handle {})", f.eax);
    f.eax = if ok { SOS_NO_ERROR } else { SOS_INVALID_HANDLE };
    HookResult::Return(0)
}

/// `sosTIMERGetEventRate(handle)`
pub fn timer_get_rate(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    f.eax = m.sched.as_ref().expect("scheduler").rate_of(f.eax).unwrap_or(0);
    HookResult::Return(0)
}

/// Diagnostic observer for HMI entry points (`SR_OBSERVE_HMI=1`): log the call with its
/// register arguments and caller, then run the original function.
pub fn observe(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    let id = m.hooks.list.iter().position(|h| h.arg == tramp).unwrap_or(0);
    let link = m.img.to_link(m.hooks.list[id].addr).unwrap_or(0);
    if m.hooks.list[id].calls <= 6 {
        crate::info!(
            "HMI {link:#x}(eax={:#x} edx={:#x} ebx={:#x} ecx={:#x}) from {:#x}",
            f.eax,
            f.edx,
            f.ebx,
            f.ecx,
            caller_link(m, f)
        );
    }
    HookResult::Resume(tramp)
}

/// Every HMI SOS entry point the game's sound module (b9xxx-bbxxx) calls.
pub const ENTRY_POINTS: &[u32] = &[
    0xc17a7, 0xc287b, 0xd69a5, 0xd69d5, 0xd6c89, 0xd6ce5, 0xd6d25, 0xd6d4d, 0xd6f8c, 0xd7005, 0xd74b3, 0xd79e9,
    0xd7fc5, 0xd8267, 0xd82a0, 0xd848a, 0xd8586, 0xd86f8, 0xd8826, 0xd88cd, 0xd8a4b, 0xd8b55, 0xd8cd6, 0xd8e64,
    0xd8f81, 0xd90d1, 0xd93cb, 0xd99ad, 0xd9a64, 0xd9bdd, 0xd9c54, 0xd9cf9, 0xd9e46, 0xd9ece, 0xda145, 0xda2bf,
    0xda4fc, 0xda70d, 0xda88d, 0xda960,
];
