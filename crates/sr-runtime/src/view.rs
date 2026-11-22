//! View parameters shared between the renderer and the game patches.
//!
//! The renderer publishes how far the visible area extends beyond the game's 640-pixel
//! width on each side (widescreen, in game pixels); the camera patches widen the game's
//! culling to match so nothing pops in at the edges.

use std::sync::atomic::{AtomicU32, Ordering};

static EXT: AtomicU32 = AtomicU32::new(0);

/// Horizontal extension on each side of the 640x480 screen, in game pixels.
pub fn ext() -> f32 {
    f32::from_bits(EXT.load(Ordering::Relaxed))
}

pub fn set_ext(e: f32) {
    EXT.store(e.max(0.0).to_bits(), Ordering::Relaxed);
}

static SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// Screen pixels per game pixel (the renderer's scene height / 480).
pub fn scale() -> f32 {
    f32::from_bits(SCALE.load(Ordering::Relaxed))
}

pub fn set_scale(s: f32) {
    SCALE.store(s.max(0.1).to_bits(), Ordering::Relaxed);
}
