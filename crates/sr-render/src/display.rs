//! Display geometry shared by the renderer and the settings: the aspect presets, the part
//! of the window the picture fills, the widescreen extension that follows from it, and the
//! render scale of the 3D scene.

/// Shape of the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Aspect {
    /// The window's shape: wider windows show more to the sides (Hor+); windows narrower
    /// than the game's 4:3 get bars above and below.
    #[default]
    Auto,
    Wide16x9,
    /// 21:9 as ultrawide monitors sell it (2560 x 1080, 5120 x 2160): 64:27, 2.37:1.
    Wide21x9,
    Wide32x9,
    /// The original 4:3.
    Classic4x3,
}

impl Aspect {
    pub const ALL: [Aspect; 5] = [Aspect::Auto, Aspect::Wide16x9, Aspect::Wide21x9, Aspect::Wide32x9, Aspect::Classic4x3];

    /// Width : height in whole numbers; `None` for the window's own.
    pub fn ratio(self) -> Option<(u32, u32)> {
        match self {
            Aspect::Auto => None,
            Aspect::Wide16x9 => Some((16, 9)),
            Aspect::Wide21x9 => Some((64, 27)),
            Aspect::Wide32x9 => Some((32, 9)),
            Aspect::Classic4x3 => Some((4, 3)),
        }
    }

    /// The settings file's word for it.
    pub fn key(self) -> &'static str {
        match self {
            Aspect::Auto => "auto",
            Aspect::Wide16x9 => "16:9",
            Aspect::Wide21x9 => "21:9",
            Aspect::Wide32x9 => "32:9",
            Aspect::Classic4x3 => "4:3",
        }
    }

    /// From the settings file or the command line (`wide`: the earlier versions' word for
    /// `auto`).
    pub fn parse(s: &str) -> Option<Aspect> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "wide" | "widescreen" => Some(Aspect::Auto),
            "16:9" => Some(Aspect::Wide16x9),
            "21:9" => Some(Aspect::Wide21x9),
            "32:9" => Some(Aspect::Wide32x9),
            "4:3" => Some(Aspect::Classic4x3),
            _ => None,
        }
    }

    /// Position in `ALL` (for atomics and menus).
    pub fn index(self) -> usize {
        Aspect::ALL.iter().position(|&a| a == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Aspect {
        Aspect::ALL.get(i).copied().unwrap_or_default()
    }
}

/// The rectangle `(x, y, w, h)` of a `window`-sized back buffer that the picture fills:
/// for `Auto` the whole window (or the game's 4:3 with bars above and below when the window
/// is narrower than that), for a preset the largest rectangle of its shape that fits,
/// centred: bars at the sides (pillarbox) or above and below (letterbox).
pub fn view_rect(window: (u32, u32), game: (u32, u32), aspect: Aspect) -> (u32, u32, u32, u32) {
    let (ww, wh) = window;
    let (gw, gh) = game;
    let (rw, rh) = match aspect.ratio() {
        None if ww as u64 * gh as u64 >= wh as u64 * gw as u64 => return (0, 0, ww.max(1), wh.max(1)),
        // the game's own 4:3 (as before the presets)
        None | Some((4, 3)) => (gw, gh),
        Some(r) => r,
    };
    let scale = (ww as f64 / rw as f64).min(wh as f64 / rh as f64);
    let (w, h) = (((rw as f64 * scale).round() as u32).max(1), ((rh as f64 * scale).round() as u32).max(1));
    ((ww - w.min(ww)) / 2, (wh - h.min(wh)) / 2, w, h)
}

/// The widescreen extension in game pixels on each side of the game's `game` screen: from
/// the preset's shape exactly, or for `Auto` from the picture's size.
pub fn extension(game: (u32, u32), aspect: Aspect, picture: (u32, u32)) -> f32 {
    let (gw, gh) = (game.0 as f32, game.1 as f32);
    let e = match aspect.ratio() {
        Some((n, d)) => (gh * n as f32 / d as f32 - gw) / 2.0,
        None => (gh * picture.0 as f32 / picture.1.max(1) as f32 - gw) / 2.0,
    };
    e.max(0.0)
}

/// Render scale limits, percent of the picture's size.
pub const RENDER_SCALE_MIN: u32 = 50;
pub const RENDER_SCALE_MAX: u32 = 200;
/// Largest render target side Direct3D 11 allows.
const MAX_SIDE: f64 = 16384.0;

/// The size of the 3D scene drawn at `percent` of the picture's (`w` x `h`); 100 = the
/// picture's own. Kept within Direct3D's largest texture.
pub fn render_size(picture: (u32, u32), percent: u32) -> (u32, u32) {
    if percent == 100 {
        return picture;
    }
    let (w, h) = (picture.0.max(1) as f64, picture.1.max(1) as f64);
    let k = (percent.clamp(RENDER_SCALE_MIN, RENDER_SCALE_MAX) as f64 / 100.0).min(MAX_SIDE / w.max(h));
    (((w * k).round() as u32).max(1), ((h * k).round() as u32).max(1))
}

/// A display mode (width, height, refresh rate in Hz) packed into one integer for an
/// atomic; 0 = none (the desktop's mode).
pub fn pack_mode(m: Option<(u32, u32, u32)>) -> u64 {
    m.map_or(0, |(w, h, hz)| (w as u64 & 0xf_ffff) << 40 | (h as u64 & 0xf_ffff) << 20 | (hz as u64 & 0xf_ffff))
}

pub fn unpack_mode(v: u64) -> Option<(u32, u32, u32)> {
    (v != 0).then(|| ((v >> 40) as u32 & 0xf_ffff, (v >> 20) as u32 & 0xf_ffff, v as u32 & 0xf_ffff))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: (u32, u32) = (640, 480);

    #[test]
    fn auto_is_the_window() {
        assert_eq!(view_rect((1280, 720), GAME, Aspect::Auto), (0, 0, 1280, 720));
        assert_eq!(view_rect((3840, 2160), GAME, Aspect::Auto), (0, 0, 3840, 2160));
        // narrower than 4:3: 4:3 with bars above and below
        assert_eq!(view_rect((1000, 1000), GAME, Aspect::Auto), (0, 125, 1000, 750));
        assert!((extension(GAME, Aspect::Auto, (1280, 720)) - 320.0 / 3.0).abs() < 1e-3);
        assert_eq!(extension(GAME, Aspect::Auto, (1000, 750)), 0.0);
    }

    #[test]
    fn presets_bars_and_extension() {
        // a 16:9 window
        assert_eq!(view_rect((1280, 720), GAME, Aspect::Wide16x9), (0, 0, 1280, 720));
        assert_eq!(view_rect((1280, 720), GAME, Aspect::Classic4x3), (160, 0, 960, 720));
        assert_eq!(view_rect((1280, 720), GAME, Aspect::Wide21x9), (0, 90, 1280, 540));
        assert_eq!(view_rect((1280, 720), GAME, Aspect::Wide32x9), (0, 180, 1280, 360));
        assert_eq!(view_rect((3840, 2160), GAME, Aspect::Wide21x9), (0, 270, 3840, 1620));
        // a 32:9 window with 16:9: bars at the sides
        assert_eq!(view_rect((3840, 1080), GAME, Aspect::Wide16x9), (960, 0, 1920, 1080));
        // the extension follows the preset, not the window
        let e = |a| extension(GAME, a, (1280, 720));
        assert_eq!(e(Aspect::Classic4x3), 0.0);
        assert!((e(Aspect::Wide16x9) - (480.0 * 16.0 / 9.0 - 640.0) / 2.0).abs() < 1e-3);
        assert!((e(Aspect::Wide21x9) - (480.0 * 64.0 / 27.0 - 640.0) / 2.0).abs() < 1e-3);
        assert!((e(Aspect::Wide32x9) - (480.0 * 32.0 / 9.0 - 640.0) / 2.0).abs() < 1e-3);
        // within what the game's x table allows (widescreen::MAX_EXT = 700)
        assert!(e(Aspect::Wide32x9) < 700.0);
    }

    #[test]
    fn keys_round_trip() {
        for a in Aspect::ALL {
            assert_eq!(Aspect::parse(a.key()), Some(a));
            assert_eq!(Aspect::from_index(a.index()), a);
        }
        assert_eq!(Aspect::parse("wide"), Some(Aspect::Auto));
        assert_eq!(Aspect::parse("5:4"), None);
    }

    #[test]
    fn render_sizes() {
        assert_eq!(render_size((3840, 2160), 100), (3840, 2160));
        assert_eq!(render_size((3840, 2160), 50), (1920, 1080));
        assert_eq!(render_size((3840, 2160), 200), (7680, 4320));
        assert_eq!(render_size((1280, 720), 75), (960, 540));
        // limited by the largest texture
        assert_eq!(render_size((10000, 5000), 200), (16384, 8192));
    }

    #[test]
    fn modes_pack() {
        for m in [None, Some((3840, 2160, 144)), Some((640, 480, 60)), Some((16384, 16384, 1000))] {
            assert_eq!(unpack_mode(pack_mode(m)), m);
        }
    }
}
