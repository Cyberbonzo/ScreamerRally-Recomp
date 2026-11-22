//! The settings overlay's display items whose options depend on the monitor and the
//! window: Resolution (window sizes, exclusive fullscreen's display modes, the monitor's
//! resolution in borderless) and Render scale (with the 3D scene's pixels). Their options
//! are filled at start, when the overlay opens and after a display change
//! (`refresh_items`); what each option stands for is kept here for `apply_*`.

use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;

use sr_render::Aspect;
use sr_render::display::{RENDER_SCALE_MAX, RENDER_SCALE_MIN, pack_mode, render_size, view_rect};
use sr_render::overlay::{Menu, Value};
use sr_render::window::{Monitor, WindowMode};

use crate::menu::Context;
use crate::settings::Settings;

pub const ASPECT_NAMES: &[&str] = &["Auto (the window's shape)", "16:9", "21:9 (ultrawide)", "32:9 (super ultrawide)", "4:3 (original)"];
pub const ASPECT_HINT: &str = "Auto fills the window. A preset keeps its shape with black bars; the 3D view is as wide as the preset.";
pub const RENDER_SCALE_HINT: &str = "The 3D scene's resolution: lower is faster and softer, higher smooths edges. HUD and menus stay sharp.";

/// Window sizes offered (those that fit the monitor): 16:9, 16:10, 21:9 and the game's 4:3.
const WINDOW_SIZES: &[(u32, u32)] = &[
    (1280, 720),
    (1366, 768),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (3200, 1800),
    (3840, 2160),
    (5120, 2880),
    (1280, 800),
    (1440, 900),
    (1680, 1050),
    (1920, 1200),
    (2560, 1600),
    (2880, 1800),
    (3840, 2400),
    (2560, 1080),
    (3440, 1440),
    (3840, 1600),
    (5120, 2160),
    (1024, 768),
    (1280, 960),
    (1600, 1200),
    (2048, 1536),
];

/// Render scales offered (percent); a value from the settings file is added.
const RENDER_SCALES: &[u32] = &[50, 60, 70, 80, 90, 100, 125, 150, 175, 200];

/// What an option of the Resolution item stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Res {
    /// Windowed: this client size.
    Window(u32, u32),
    /// Exclusive fullscreen: this display mode (`None`: the desktop's).
    Mode(Option<(u32, u32, u32)>),
    /// Borderless: the monitor's resolution (nothing to choose).
    Monitor,
}

static RESOLUTIONS: Mutex<Vec<Res>> = Mutex::new(Vec::new());
static SCALES: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// "16:9" etc. for a size of a common shape (within 1 %).
fn shape_name(w: u32, h: u32) -> Option<&'static str> {
    let r = w as f64 / h.max(1) as f64;
    [(16.0 / 9.0, "16:9"), (16.0 / 10.0, "16:10"), (4.0 / 3.0, "4:3"), (5.0 / 4.0, "5:4"), (64.0 / 27.0, "21:9"), (43.0 / 18.0, "21:9"), (12.0 / 5.0, "21:9"), (32.0 / 9.0, "32:9")]
        .iter()
        .find(|(k, _)| (r / k - 1.0).abs() < 0.01)
        .map(|(_, n)| *n)
}

fn size_label(w: u32, h: u32) -> String {
    match shape_name(w, h) {
        Some(n) => format!("{w} x {h} ({n})"),
        None => format!("{w} x {h}"),
    }
}

/// The render scales offered with `current` among them.
fn scales(current: u32) -> Vec<u32> {
    let mut v = RENDER_SCALES.to_vec();
    if !v.contains(&current) {
        v.push(current.clamp(RENDER_SCALE_MIN, RENDER_SCALE_MAX));
        v.sort_unstable();
        v.dedup();
    }
    v
}

/// Render scale options before the window exists (no pixel counts yet) and the current one.
pub fn render_scale_options(s: &Settings) -> (Vec<String>, usize) {
    let v = scales(s.render_scale);
    let i = v.iter().position(|&p| p == s.render_scale).unwrap_or(0);
    *SCALES.lock().unwrap() = v.clone();
    (v.iter().map(|p| format!("{p}%")).collect(), i)
}

/// Resolution options before the window exists (the settings' own) and the current one.
pub fn resolution_options(s: &Settings) -> (Vec<String>, usize) {
    let (res, label) = match s.window {
        WindowMode::Windowed => (Res::Window(s.width, s.height), size_label(s.width, s.height)),
        WindowMode::Exclusive => (Res::Mode(s.exclusive_resolution), s.exclusive_resolution.map_or("Desktop".into(), |(w, h, hz)| format!("{w} x {h}, {hz} Hz"))),
        WindowMode::Borderless => (Res::Monitor, "The monitor's".into()),
    };
    *RESOLUTIONS.lock().unwrap() = vec![res];
    (vec![label], 0)
}

/// The Resolution item for window mode `mode`: options, what they stand for, the current
/// one and the item's description. `client` is the window's current client size.
fn resolution_item(s: &Settings, mon: &Monitor, client: (u32, u32)) -> (Vec<String>, Vec<Res>, usize, &'static str) {
    match s.window {
        WindowMode::Windowed => {
            let mut sizes: Vec<(u32, u32)> = WINDOW_SIZES.iter().copied().filter(|&(w, h)| w <= mon.max_client.0 && h <= mon.max_client.1).collect();
            if client.0 > 0 && client.1 > 0 && !sizes.contains(&client) {
                sizes.push(client);
            }
            sizes.sort_unstable();
            let labels = sizes.iter().map(|&(w, h)| if (w, h) == client && !WINDOW_SIZES.contains(&(w, h)) { format!("{w} x {h} (current)") } else { size_label(w, h) }).collect();
            let index = sizes.iter().position(|&z| z == client).unwrap_or(0);
            (labels, sizes.into_iter().map(|(w, h)| Res::Window(w, h)).collect(), index, "The window's size: those that fit this monitor, and the current one. Applies at once.")
        }
        WindowMode::Exclusive => {
            let (dw, dh, dhz) = mon.desktop;
            let mut modes: Vec<Option<(u32, u32, u32)>> = vec![None];
            modes.extend(mon.modes.iter().map(|&m| Some(m)));
            if let Some(m) = s.exclusive_resolution.filter(|m| !mon.modes.contains(m)) {
                modes.push(Some(m));
            }
            let labels = modes
                .iter()
                .map(|m| match m {
                    None => format!("Desktop ({dw} x {dh}, {dhz} Hz)"),
                    Some((w, h, hz)) if mon.modes.contains(&(*w, *h, *hz)) => format!("{w} x {h}, {hz} Hz"),
                    Some((w, h, hz)) => format!("{w} x {h}, {hz} Hz (not offered)"),
                })
                .collect();
            let index = modes.iter().position(|m| *m == s.exclusive_resolution).unwrap_or(0);
            (
                labels,
                modes.into_iter().map(Res::Mode).collect(),
                index,
                "The display mode in exclusive fullscreen. It switches a second after the last change.",
            )
        }
        WindowMode::Borderless => (
            vec![format!("{} x {} (the monitor's)", mon.size.0, mon.size.1)],
            vec![Res::Monitor],
            0,
            "Borderless uses the monitor's resolution. Graphics > Render scale sets the 3D scene's pixels.",
        ),
    }
}

/// Fill the Resolution and Render scale items for the window's monitor and the current
/// settings. `live`: the window's size is its real one now (not being changed), so a size
/// the player gave it by hand shows as the current one.
pub fn refresh_items(ctx: &Context, menu: &mut Menu, live: bool) {
    let s = ctx.settings.lock().unwrap().clone();
    let Some(mon) = sr_render::window::monitor(ctx.hwnd) else { return };
    // the monitor, once per monitor seen (diagnostics)
    static LOGGED: Mutex<Option<Monitor>> = Mutex::new(None);
    if LOGGED.lock().unwrap().replace(mon.clone()).as_ref() != Some(&mon) {
        let modes: Vec<String> = mon.modes.iter().map(|(w, h, hz)| format!("{w}x{h}@{hz}")).collect();
        crate::log_info(&format!(
            "display: monitor {}x{} at {} Hz, windows up to {}x{}; {} display modes: {}",
            mon.size.0,
            mon.size.1,
            mon.desktop.2,
            mon.max_client.0,
            mon.max_client.1,
            modes.len(),
            modes.join(" ")
        ));
    }
    let real = sr_render::window::client_size(ctx.hwnd);
    let windowed = if live && real.0 > 0 && real.1 > 0 { real } else { (s.width, s.height) };
    fill_items(menu, &s, &mon, windowed);
}

/// `refresh_items` for monitor `mon` and a windowed client size of `windowed`.
fn fill_items(menu: &mut Menu, s: &Settings, mon: &Monitor, windowed: (u32, u32)) {
    let (labels, res, index, hint) = resolution_item(s, mon, windowed);
    if let Some(item) = menu.item_mut("resolution") {
        item.value = Value::Choice { options: labels, index };
        item.hint = hint.into();
        // borderless: shown, nothing to choose
        item.inactive = s.window == WindowMode::Borderless;
    }
    *RESOLUTIONS.lock().unwrap() = res;
    // the picture's size in this mode, and the 3D scene's at each scale
    let client = match s.window {
        WindowMode::Windowed => windowed,
        WindowMode::Borderless => mon.size,
        WindowMode::Exclusive => s.exclusive_resolution.map_or(mon.size, |(w, h, _)| (w, h)),
    };
    let (_, _, pw, ph) = view_rect(client, (640, 480), s.aspect);
    let v = scales(s.render_scale);
    let labels = v
        .iter()
        .map(|&p| {
            let (w, h) = render_size((pw, ph), p);
            format!("{p}% ({w} x {h})")
        })
        .collect();
    let index = v.iter().position(|&p| p == s.render_scale).unwrap_or(0);
    if let Some(item) = menu.item_mut("render_scale") {
        item.value = Value::Choice { options: labels, index };
    }
    *SCALES.lock().unwrap() = v;
}

/// The Resolution item changed to option `index`: the window's size (at once) or exclusive
/// fullscreen's display mode (a second later, see `D3dBackend::follow_exclusive_mode`).
/// Returns a note for the status line.
pub fn apply_resolution(ctx: &Context, s: &mut Settings, index: usize) -> Option<&'static str> {
    let res = RESOLUTIONS.lock().unwrap().get(index).copied();
    match res {
        Some(Res::Window(w, h)) => {
            s.width = w;
            s.height = h;
            sr_render::window::set_client_size(ctx.hwnd, w, h);
            None
        }
        Some(Res::Mode(m)) => {
            s.exclusive_resolution = m;
            ctx.live.exclusive_mode.store(pack_mode(m), Relaxed);
            Some("Saved (mode: in a second)")
        }
        _ => None,
    }
}

/// The Render scale item changed to option `index`.
pub fn apply_render_scale(ctx: &Context, s: &mut Settings, index: usize) {
    if let Some(&p) = SCALES.lock().unwrap().get(index) {
        s.render_scale = p;
        ctx.live.render_scale.store(p, Relaxed);
    }
}

/// The Aspect item changed to option `index`.
pub fn apply_aspect(ctx: &Context, s: &mut Settings, index: usize) {
    s.aspect = Aspect::from_index(index);
    ctx.live.aspect.store(s.aspect.index() as u8, Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> Monitor {
        Monitor {
            size: (3840, 2160),
            max_client: (3818, 2021),
            desktop: (3840, 2160, 144),
            modes: vec![(3840, 2160, 144), (3840, 2160, 60), (2560, 1440, 144), (1920, 1080, 60)],
        }
    }

    #[test]
    fn window_sizes_fit_and_include_the_current_one() {
        let s = Settings::default();
        let (labels, res, index, _) = resolution_item(&s, &monitor(), (1280, 960));
        assert_eq!(res[index], Res::Window(1280, 960));
        assert_eq!(labels[index], "1280 x 960 (4:3)");
        assert!(res.contains(&Res::Window(3200, 1800)) && res.contains(&Res::Window(3440, 1440)));
        // larger than the work area: not offered
        assert!(!res.contains(&Res::Window(3840, 2160)) && !res.contains(&Res::Window(3840, 1600)));
        // a size given by hand
        let (labels, res, index, _) = resolution_item(&s, &monitor(), (1500, 777));
        assert_eq!((res[index], labels[index].as_str()), (Res::Window(1500, 777), "1500 x 777 (current)"));
    }

    #[test]
    fn exclusive_modes_and_borderless() {
        let mut s = Settings { window: WindowMode::Exclusive, ..Settings::default() };
        let (labels, res, index, _) = resolution_item(&s, &monitor(), (1280, 960));
        assert_eq!((index, labels[0].as_str()), (0, "Desktop (3840 x 2160, 144 Hz)"));
        assert_eq!(res.len(), 5);
        s.exclusive_resolution = Some((2560, 1440, 144));
        let (labels, res, index, _) = resolution_item(&s, &monitor(), (1280, 960));
        assert_eq!((res[index], labels[index].as_str()), (Res::Mode(Some((2560, 1440, 144))), "2560 x 1440, 144 Hz"));
        s.window = WindowMode::Borderless;
        let (labels, res, _, _) = resolution_item(&s, &monitor(), (1280, 960));
        assert_eq!((labels, res), (vec!["3840 x 2160 (the monitor's)".to_string()], vec![Res::Monitor]));
    }

    #[test]
    fn filled_items_fit_the_overlay() {
        // every window mode, with a monitor's lists and a long mode not offered
        for window in [WindowMode::Windowed, WindowMode::Borderless, WindowMode::Exclusive] {
            for aspect in Aspect::ALL {
                let s = Settings { window, aspect, render_scale: 75, exclusive_resolution: Some((16384, 16384, 1000)), ..Settings::default() };
                let mut m = crate::menu::build(&s, &sr_crt::CrtLive::new(Default::default(), None));
                fill_items(&mut m, &s, &monitor(), (1500, 777));
                assert!(m.layout_problems().is_empty(), "{:#?}", m.layout_problems());
                let Some(Value::Choice { options, index }) = m.item("render_scale").map(|i| &i.value) else { panic!() };
                assert!(options[*index].starts_with("75% ("), "{options:?}");
            }
        }
        // the pixels of the 3D scene: 16:9 picture on a 3840 x 2160 monitor, borderless
        let s = Settings { window: WindowMode::Borderless, ..Settings::default() };
        let mut m = crate::menu::build(&s, &sr_crt::CrtLive::new(Default::default(), None));
        fill_items(&mut m, &s, &monitor(), (1280, 720));
        let Some(Value::Choice { options, .. }) = m.item("render_scale").map(|i| &i.value) else { panic!() };
        assert!(options.contains(&"100% (3840 x 2160)".to_string()) && options.contains(&"50% (1920 x 1080)".to_string()), "{options:?}");
    }

    #[test]
    fn render_scale_list() {
        assert_eq!(scales(100), RENDER_SCALES);
        assert!(scales(75).contains(&75));
        assert_eq!(shape_name(1366, 768), Some("16:9"));
        assert_eq!(shape_name(3440, 1440), Some("21:9"));
        assert_eq!(shape_name(1500, 777), None);
    }
}
