//! A "next camera" control on top of the game's own view keys (STRT3FX).
//!
//! The game selects player 1's race camera directly with F1 .. F4 (and player 2's in split
//! screen with F5 .. F8): the routine at 23aa8 scans the function keys and sets the view's
//! camera mode, [player view + AE4h] (docs/re/strt3fx.md, "Cameras"): F1 -> 2 (bumper),
//! F2 -> 4, F3 -> 6, F4 -> 3 (chase views from near to far). F9 sets mode 1 only when a
//! game flag allows it (not in a normal race). This module tells which F key gives the view
//! after the current one, so one key or controller button can step through the four.

use crate::mem;
use std::sync::atomic::{AtomicU32, Ordering};

/// Player view pointers (one per player, 1296BCh in the image).
const PLAYER_VIEWS: u32 = 0x1296bc;
/// Camera mode in a player view.
const MODE: u32 = 0xae4;
/// The four views in order: (camera mode, F key scancode).
const VIEWS: [(u32, u8); 4] = [(2, 0x3b), (4, 0x3c), (6, 0x3d), (3, 0x3e)];

/// Run-time address of the player view table (0 until the game is loaded).
static TABLE: AtomicU32 = AtomicU32::new(0);

pub fn install(m: &crate::Machine) {
    TABLE.store(m.img.link(PLAYER_VIEWS), Ordering::Relaxed);
}

/// The F key (scancode) that selects the view after player 1's current one; None when no
/// race view is known. Reads one dword of guest memory (any thread).
pub fn next_view_key() -> Option<u8> {
    let table = TABLE.load(Ordering::Relaxed);
    if table == 0 || !mem::readable(table, 4) {
        return None;
    }
    let view = unsafe { mem::rd32(table) };
    if view == 0 || !mem::readable(view.wrapping_add(MODE), 4) {
        return None;
    }
    let mode = unsafe { mem::rd32(view + MODE) };
    let i = VIEWS.iter().position(|v| v.0 == mode)?;
    Some(VIEWS[(i + 1) % VIEWS.len()].1)
}
