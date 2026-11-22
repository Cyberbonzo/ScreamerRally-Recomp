//! Autopilot: the game's own AI drives the player's car (STRT3FX).
//!
//! Each car's record (1296C8h + n * CC8h) points at +B4h to the control bindings it reads
//! (docs/re/strt3fx.md, "Autopilot"): the human players' cars get one at race start
//! (38741: player 1, index [1436D4h], the working copy at 139BD0h), the computer's cars 0.
//! Every physics and steering routine branches on that pointer (25345, 26647, 27223,
//! 29190, 2d728, 2db0d, 336b9, ...): with 0 the AI drives. The autopilot clears the
//! pointer of player 1's car while it is on and puts it back when it is switched off; the
//! game keeps track of positions, laps and times as for any car.

use crate::Machine;
use crate::mem;
use std::sync::atomic::{AtomicBool, Ordering};

const CARS: u32 = 0x1296c8;
const CAR_SIZE: u32 = 0xcc8;
const BINDINGS: u32 = 0xb4;
/// Index of player 1's car.
const PLAYER1: u32 = 0x1436d4;
/// Player 1's working bindings (what the game gives the car at race start).
const PLAYER1_BINDINGS: u32 = 0x139bd0;

static ON: AtomicBool = AtomicBool::new(false);
static ENV: std::sync::Once = std::sync::Once::new();

pub fn toggle() -> bool {
    let on = !ON.load(Ordering::Relaxed);
    ON.store(on, Ordering::Relaxed);
    on
}

pub fn set(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn is_on() -> bool {
    ON.load(Ordering::Relaxed)
}

#[derive(Default)]
pub struct Autopilot {
    /// The bindings pointer taken from the player's car (to give back).
    saved: Option<(u32, u32)>,
    pub active: bool,
}

/// Once per game frame (race frames and menus alike).
pub fn tick(m: &mut Machine) {
    // SR_AUTOPILOT=1 (diagnostics, deterministic runs): on from the start
    ENV.call_once(|| {
        if std::env::var_os("SR_AUTOPILOT").is_some() {
            ON.store(true, Ordering::Relaxed);
        }
    });
    let on = ON.load(Ordering::Relaxed);
    let idx = unsafe { mem::rd32(m.img.link(PLAYER1)) };
    if idx >= 8 {
        return;
    }
    let field = m.img.link(CARS) + idx * CAR_SIZE + BINDINGS;
    let current = unsafe { mem::rd32(field) };
    let player_bindings = m.img.link(PLAYER1_BINDINGS);
    if on {
        // the game set the player's bindings (race start, or the autopilot was just
        // switched on): take them away
        if current == player_bindings {
            m.autopilot.saved = Some((field, current));
            unsafe { mem::wr32(field, 0) };
            if !m.autopilot.active {
                crate::info!("autopilot: on (car {idx})");
            }
            m.autopilot.active = true;
        }
    } else if let Some((at, value)) = m.autopilot.saved.take() {
        if at == field && unsafe { mem::rd32(at) } == 0 {
            unsafe { mem::wr32(at, value) };
        }
        m.autopilot.active = false;
        crate::info!("autopilot: off");
    }
}
