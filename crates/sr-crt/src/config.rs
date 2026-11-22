//! CRT settings shared by the settings overlay (which changes them) and the renderer (which
//! reads them every frame), and the option tables ported from ShaderGlass's `Options.h`.
//!
//! Option tables: ShaderGlass, Copyright (C) 2021-2025 mausimus (mausimus.net),
//! https://github.com/mausimus/ShaderGlass, GNU General Public License v3.0.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The size of the picture the shader gets (ShaderGlass's "Pixel Size"): the game renders
/// its whole frame (3D, HUD, menus' pictures) at this size while CRT emulation is on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PixelSize {
    /// This many lines (the width follows the picture's shape): 480 is the game's own.
    Lines(u32),
    /// The picture's size on screen divided by this (ShaderGlass's x1, x2, x2.25, ...).
    Divide(f32),
}

/// Pixel sizes offered, in menu order: the game's 480 lines first (the default), other
/// line counts, then ShaderGlass's list (`pixelSizes`).
pub const PIXEL_SIZES: &[(PixelSize, &str, &str)] = &[
    (PixelSize::Lines(480), "480 lines (original)", "480"),
    (PixelSize::Lines(240), "240 lines", "240"),
    (PixelSize::Lines(360), "360 lines", "360"),
    (PixelSize::Lines(600), "600 lines", "600"),
    (PixelSize::Lines(720), "720 lines", "720"),
    (PixelSize::Divide(1.0), "x1 (screen)", "x1"),
    (PixelSize::Divide(2.0), "x2", "x2"),
    (PixelSize::Divide(2.25), "x2.25 (480p -> 1080p)", "x2.25"),
    (PixelSize::Divide(3.0), "x3", "x3"),
    (PixelSize::Divide(4.0), "x4", "x4"),
    (PixelSize::Divide(4.5), "x4.5 (240p -> 1080p)", "x4.5"),
    (PixelSize::Divide(5.0), "x5", "x5"),
    (PixelSize::Divide(5.4), "x5.4 (200p -> 1080p)", "x5.4"),
    (PixelSize::Divide(6.0), "x6 (240p -> 1440p)", "x6"),
    (PixelSize::Divide(7.0), "x7", "x7"),
    (PixelSize::Divide(7.2), "x7.2 (200p -> 1440p)", "x7.2"),
    (PixelSize::Divide(8.0), "x8", "x8"),
    (PixelSize::Divide(9.0), "x9 (240p -> 4K)", "x9"),
    (PixelSize::Divide(10.0), "x10", "x10"),
    (PixelSize::Divide(10.8), "x10.8 (200p -> 4K)", "x10.8"),
];

/// Aspect ratio correction (ShaderGlass `aspectRatios`): the height of an input pixel
/// relative to its width; `CUSTOM` takes `CrtSettings::custom_aspect`, 0 stretches the
/// picture over the whole window.
pub const ASPECT_RATIOS: &[(f32, &str, &str)] = &[
    (1.0, "None", "1"),
    (1.2, "x1.2 (DOS, NTSC)", "1.2"),
    (0.9375, "x0.94 (PAL)", "0.94"),
    (0.8, "x0.8 (NES)", "0.8"),
    (0.857143, "x0.86 (SNES)", "0.86"),
    (0.5, "x0.5 (double wide)", "0.5"),
    (2.0, "x2.0 (double tall)", "2.0"),
    (-1.0, "Custom", "custom"),
    (0.0, "Stretch", "0"),
];
pub const CUSTOM_ASPECT: usize = 7;

/// Output scale (ShaderGlass "Scale": `Free` and `outputScales`), relative to the game's
/// picture on screen; 0 = free (the largest that fits the window).
pub const OUTPUT_SCALES: &[(f32, &str, &str)] = &[
    (0.0, "Free (fit)", "free"),
    (1.0, "100%", "100"),
    (1.5, "150%", "150"),
    (2.0, "200%", "200"),
    (2.5, "250%", "250"),
    (3.0, "300%", "300"),
    (3.5, "350%", "350"),
    (4.0, "400%", "400"),
    (5.0, "500%", "500"),
    (6.0, "600%", "600"),
    (7.0, "700%", "700"),
    (8.0, "800%", "800"),
    (9.0, "900%", "900"),
    (10.0, "1000%", "1000"),
    (0.75, "75%", "75"),
    (0.5, "50%", "50"),
    (0.25, "25%", "25"),
];

/// Keys that, held, show the game without the shader (ShaderGlass: Tab, which the game
/// uses for the autopilot): scancode and name.
pub const HOLD_KEYS: &[(u8, &str)] = &[(0, "Off"), (0x57, "F11"), (0x46, "Scroll Lock"), (0x0e, "Backspace"), (0x29, "` (grave)")];

/// Everything the player sets in the CRT tab.
#[derive(Clone, Debug, PartialEq)]
pub struct CrtSettings {
    /// CRT emulation on (ShaderGlass "Active"). Off: the game renders exactly as without
    /// this feature.
    pub active: bool,
    /// The chosen preset: "category/name" in the pack.
    pub preset: String,
    /// Values of the chosen preset's parameters that differ from its defaults.
    pub params: Vec<(String, f32)>,
    /// Index into `PIXEL_SIZES`.
    pub pixel_size: usize,
    /// Index into `ASPECT_RATIOS`, and the custom ratio.
    pub aspect: usize,
    pub custom_aspect: f32,
    /// Index into `OUTPUT_SCALES`.
    pub scale: usize,
    pub flip_h: bool,
    pub flip_v: bool,
    /// Orientation vertical: the shader runs on the picture turned by a quarter (scanlines
    /// run vertically, as on a monitor stood on its side).
    pub vertical: bool,
    /// Index into `HOLD_KEYS`.
    pub hold_key: usize,
}

impl Default for CrtSettings {
    fn default() -> Self {
        CrtSettings {
            active: false,
            preset: String::new(),
            params: Vec::new(),
            pixel_size: 0,
            aspect: 0,
            custom_aspect: 1.0,
            scale: 0,
            flip_h: false,
            flip_v: false,
            vertical: false,
            hold_key: 1,
        }
    }
}

impl CrtSettings {
    pub fn pixel_size(&self) -> PixelSize {
        PIXEL_SIZES.get(self.pixel_size).map_or(PixelSize::Lines(480), |p| p.0)
    }

    /// The aspect ratio correction factor (0 = stretch).
    pub fn aspect_ratio(&self) -> f32 {
        match ASPECT_RATIOS.get(self.aspect) {
            Some((r, _, _)) if *r < 0.0 => self.custom_aspect.clamp(0.1, 10.0),
            Some((r, _, _)) => *r,
            None => 1.0,
        }
    }

    pub fn output_scale(&self) -> f32 {
        OUTPUT_SCALES.get(self.scale).map_or(0.0, |s| s.0)
    }

    pub fn hold_scancode(&self) -> u8 {
        HOLD_KEYS.get(self.hold_key).map_or(0, |k| k.0)
    }
}

/// The input size for a picture of `picture` pixels on screen.
pub fn input_size(px: PixelSize, picture: (u32, u32)) -> (u32, u32) {
    let (pw, ph) = (picture.0.max(1) as f64, picture.1.max(1) as f64);
    let (w, h) = match px {
        PixelSize::Lines(l) => ((l as f64 * pw / ph).round(), l as f64),
        PixelSize::Divide(d) => ((pw / d as f64).round(), (ph / d as f64).round()),
    };
    ((w as u32).clamp(16, 16384), (h as u32).clamp(16, 16384))
}

/// Where the shader's output goes in a `window`-sized back buffer, with the game's
/// picture at `picture` (x, y, w, h) (ShaderGlass's output box: aspect ratio correction
/// `ratio` (0: stretch over the window), output `scale` (0: free, the largest that fits)).
/// The input has the picture's shape (`input_size`). May reach beyond the window at large
/// scales. `vertical` (the orientation): a fixed scale keeps the width and the correction
/// changes the height (ShaderGlass `UpdateOutputSize`), else the other way round.
pub fn output_rect(window: (u32, u32), picture: (u32, u32, u32, u32), ratio: f32, scale: f32, vertical: bool) -> (i32, i32, u32, u32) {
    let (ww, wh) = (window.0.max(1) as f64, window.1.max(1) as f64);
    if ratio <= 0.0 {
        return (0, 0, window.0.max(1), window.1.max(1));
    }
    if ratio == 1.0 && scale <= 0.0 {
        // no correction, free: the game's picture as it is without the shader
        return (picture.0 as i32, picture.1 as i32, picture.2.max(1), picture.3.max(1));
    }
    // the corrected picture's shape: input pixels `ratio` times as tall as wide
    let aspect = picture.2.max(1) as f64 / (picture.3.max(1) as f64 * ratio as f64);
    let (w, h) = if scale <= 0.0 {
        if ww / wh > aspect { (wh * aspect, wh) } else { (ww, ww / aspect) }
    } else if vertical {
        let w = picture.2.max(1) as f64 * scale as f64;
        (w, w / aspect)
    } else {
        // a scale of the game's picture height (100% = as the game shows it)
        let h = picture.3.max(1) as f64 * scale as f64;
        (h * aspect, h)
    };
    let (w, h) = (w.round().max(1.0), h.round().max(1.0));
    (((ww - w) / 2.0).floor() as i32, ((wh - h) / 2.0).floor() as i32, w as u32, h as u32)
}

/// The CRT state shared by the overlay and the renderer.
pub struct CrtLive {
    pub settings: Mutex<CrtSettings>,
    /// Bumped by every change to `settings`.
    pub generation: AtomicU64,
    /// The hold key is down: the passthrough ("none") is shown instead of the preset.
    pub hold: AtomicBool,
    /// A snapshot is wanted: the folder to save it in.
    pub snapshot: Mutex<Option<PathBuf>>,
    /// Messages from the renderer for the overlay's status line (preset failed, snapshot
    /// saved).
    pub status: Mutex<Option<String>>,
    /// The shader library, if found.
    pub pack: Option<Arc<crate::pack::Pack>>,
}

pub type SharedCrt = Arc<CrtLive>;

impl std::fmt::Debug for CrtLive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrtLive").field("settings", &self.settings).field("pack", &self.pack.as_ref().map(|p| p.path.clone())).finish()
    }
}

impl CrtLive {
    pub fn new(settings: CrtSettings, pack: Option<Arc<crate::pack::Pack>>) -> SharedCrt {
        Arc::new(CrtLive {
            settings: Mutex::new(settings),
            generation: AtomicU64::new(1),
            hold: AtomicBool::new(false),
            snapshot: Mutex::new(None),
            status: Mutex::new(None),
            pack,
        })
    }

    pub fn update(&self, f: impl FnOnce(&mut CrtSettings)) {
        f(&mut self.settings.lock().unwrap());
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> CrtSettings {
        self.settings.lock().unwrap().clone()
    }

    pub fn say(&self, msg: impl Into<String>) {
        *self.status.lock().unwrap() = Some(msg.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_sizes() {
        // the game's 480 lines, the width following the picture
        assert_eq!(input_size(PixelSize::Lines(480), (1440, 1080)), (640, 480));
        assert_eq!(input_size(PixelSize::Lines(480), (1920, 1080)), (853, 480));
        assert_eq!(input_size(PixelSize::Lines(480), (2560, 1080)), (1138, 480));
        assert_eq!(input_size(PixelSize::Lines(240), (1440, 1080)), (320, 240));
        assert_eq!(input_size(PixelSize::Divide(2.25), (1920, 1080)), (853, 480));
        assert_eq!(input_size(PixelSize::Divide(1.0), (1280, 720)), (1280, 720));
    }

    #[test]
    fn output_rects() {
        // free: the largest of the input's shape in the window
        assert_eq!(output_rect((1920, 1080), (240, 0, 1440, 1080), 1.0, 0.0, false), (240, 0, 1440, 1080));
        assert_eq!(output_rect((1920, 1080), (0, 0, 1920, 1080), 1.0, 0.0, false), (0, 0, 1920, 1080));
        // DOS pixels (1.2 tall): narrower; PAL (0.94): wider than the 4:3 picture
        assert_eq!(output_rect((1920, 1080), (240, 0, 1440, 1080), 1.2, 0.0, false), (360, 0, 1200, 1080));
        assert_eq!(output_rect((1920, 1080), (240, 0, 1440, 1080), 0.9375, 0.0, false), (192, 0, 1536, 1080));
        // stretch: the whole window
        assert_eq!(output_rect((1920, 1080), (240, 0, 1440, 1080), 0.0, 0.0, false), (0, 0, 1920, 1080));
        // 50 %: half the picture's height, centred
        assert_eq!(output_rect((1920, 1080), (240, 0, 1440, 1080), 1.0, 0.5, false), (600, 270, 720, 540));
        // 200 %: beyond the window
        let r = output_rect((1920, 1080), (240, 0, 1440, 1080), 1.0, 2.0, false);
        assert_eq!((r.2, r.3), (2880, 2160));
        assert!(r.0 < 0 && r.1 < 0);
        // vertical orientation at a fixed scale: the width kept, the correction on the height
        assert_eq!(output_rect((1920, 1080), (240, 0, 1440, 1080), 1.2, 1.0, true), (240, -108, 1440, 1296));
    }

    #[test]
    fn settings_values() {
        let mut s = CrtSettings::default();
        assert!(!s.active, "off by default");
        assert_eq!(s.pixel_size(), PixelSize::Lines(480));
        assert_eq!((s.aspect_ratio(), s.output_scale()), (1.0, 0.0));
        s.aspect = CUSTOM_ASPECT;
        s.custom_aspect = 1.25;
        assert_eq!(s.aspect_ratio(), 1.25);
        assert_eq!(s.hold_scancode(), 0x57);
    }
}
