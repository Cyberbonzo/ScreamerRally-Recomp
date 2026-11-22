//! The game's simulation rate (STRT3FX).
//!
//! The game runs one simulation step per frame with the elapsed time as the step (at least
//! 0.02 s), and waits for the 100 Hz timer between frames (504dc: `while (ticks < n)`, the
//! comparison's immediate at 504e4). The original waits for one tick and, on 1997 hardware,
//! ran at roughly 20-35 frames per second; the runtime makes it wait for `n` ticks.
//!
//! The car physics is not independent of the step size: measured on Rally Wales (AI cars'
//! slip angle in the 4 s after GO, deterministic runs), 50 steps per second gives about twice
//! the slip of 33 or 25 (mean 11-20 degrees against 5-12): cars slide sideways at the start
//! and in corners. 33 per second (3 ticks) is therefore the default; the in-between frames
//! keep the picture smooth whatever the rate.
//!
//! The menus' frame timer (4f6de) shares the tick wait with the race's (4f780). The menus
//! are not interpolated and have no car physics, so they keep 2 ticks (50 frames per
//! second); each timer sets the wait on entry.

use crate::Machine;
use crate::mem;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

/// The tick wait's comparison immediate (`cmp eax, n` at 504e2).
const TICKS_IMM: u32 = 0x504e4;

static TICKS: AtomicU8 = AtomicU8::new(3);
static ADDR: AtomicU32 = AtomicU32::new(0);
/// The race rate last logged.
static LOGGED: AtomicU8 = AtomicU8::new(0);

/// Ticks per menu frame (50 frames per second).
pub const MENU_TICKS: u8 = 2;

/// The menus' and the race's frame timers (each calls the tick wait first).
pub const MENU_TIMER: u32 = 0x4f6de;
pub const RACE_TIMER: u32 = 0x4f780;

/// Steps per second -> ticks of the 100 Hz timer per step (25, 33 or 50 per second).
pub fn set_rate(steps_per_second: u32) {
    let t = match steps_per_second {
        0..=27 => 4,
        28..=40 => 3,
        _ => 2,
    };
    TICKS.store(t, Ordering::Relaxed);
}

pub fn ticks() -> u8 {
    std::env::var("SR_TICKS").ok().and_then(|v| v.parse::<u8>().ok()).map(|t| {
        TICKS.store(t.clamp(1, 10), Ordering::Relaxed);
    });
    TICKS.load(Ordering::Relaxed)
}

/// Seconds per simulation step.
pub fn period() -> f64 {
    TICKS.load(Ordering::Relaxed) as f64 * 0.01
}

pub fn install(m: &Machine) {
    ADDR.store(m.img.link(TICKS_IMM), Ordering::Relaxed);
}

/// On entry to a frame timer (guest thread, before its tick wait): the menus wait
/// `MENU_TICKS`, the race the simulation rate's ticks (a changed setting applies at once).
pub fn enter_timer(race: bool) {
    let a = ADDR.load(Ordering::Relaxed);
    if a == 0 {
        return;
    }
    let want = if race { TICKS.load(Ordering::Relaxed) } else { MENU_TICKS };
    if unsafe { mem::rd8(a) } != want {
        unsafe { mem::wr8(a, want) };
    }
    if race && LOGGED.swap(want, Ordering::Relaxed) != want {
        crate::info!("simulation: {} steps per second", 100 / want as u32);
    }
}
