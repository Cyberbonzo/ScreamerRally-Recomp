//! Content of the in-game settings overlay (F12 / controller Back): built from the
//! settings, changes applied while the game runs and saved to `settings.ini` at once.
//!
//! Each tab is organised in sections; an option that only matters with a certain value of
//! the one above it is indented under it (and dimmed while it has no effect), and rarely
//! changed options sit in a group that starts collapsed ("+ Advanced"). The CRT tab is
//! `crt_menu`.

use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};

use sr_render::overlay::{Item, Menu, Row, Tab, Value};

use crate::settings::{InputMode, Settings};

/// Everything a change may need to reach.
pub struct Context {
    pub settings: Mutex<Settings>,
    pub path: PathBuf,
    pub live: Arc<sr_render::LiveRender>,
    pub audio: sr_audio::Audio,
    pub hwnd: isize,
    pub pad: Arc<Mutex<sr_input::PadSettings>>,
    /// CRT emulation's shared state (`crt_menu`), and the data folder (snapshots, profiles).
    pub crt: sr_crt::SharedCrt,
    pub data: PathBuf,
}

fn choice(id: &str, label: &str, options: &[&str], index: usize, hint: &str) -> Item {
    Item {
        id: id.into(),
        label: label.into(),
        value: Value::Choice { options: options.iter().map(|s| s.to_string()).collect(), index },
        hint: hint.into(),
        inactive: false,
    }
}

fn number(id: &str, label: &str, value: i32, (min, max, step): (i32, i32, i32), suffix: &str, hint: &str) -> Item {
    Item { id: id.into(), label: label.into(), value: Value::Number { value, min, max, step, suffix: suffix.into() }, hint: hint.into(), inactive: false }
}

fn section(title: &str) -> Row {
    Row::Section(title.into())
}

/// An option directly under its section.
fn top(item: Item) -> Row {
    Row::Item { item, depth: 0 }
}

/// An option indented: it depends on the option above it, or is in the group above it.
fn sub(item: Item) -> Row {
    Row::Item { item, depth: 1 }
}

/// Rarely changed options (the `sub` rows after it), collapsed at start.
fn advanced(hint: &str) -> Row {
    Row::Group { title: "Advanced".into(), hint: hint.into(), depth: 0, expanded: false }
}

const ON_OFF: &[&str] = &["Off", "On"];
const CURVES: &[f32] = &[1.0, 1.25, 1.5, 2.0, 2.5, 3.0];
const CURVE_NAMES: &[&str] = &["Linear", "1.25", "1.5", "2.0", "2.5", "3.0"];

fn curve_index(c: f32) -> usize {
    CURVES.iter().enumerate().min_by(|a, b| (a.1 - c).abs().total_cmp(&(b.1 - c).abs())).map_or(0, |(i, _)| i)
}

const FPS_CAPS: &[u32] = &[0, 60, 75, 100, 120, 144, 165, 200, 240, 360];
const FPS_CAP_NAMES: &[&str] = &["No limit", "60", "75", "100", "120", "144", "165", "200", "240", "360"];

fn fps_cap_index(cap: u32) -> usize {
    if cap == 0 {
        return 0;
    }
    FPS_CAPS.iter().enumerate().skip(1).min_by_key(|(_, c)| c.abs_diff(cap)).map_or(0, |(i, _)| i)
}

fn pct(x: f32) -> i32 {
    (x * 100.0).round() as i32
}

pub fn build(s: &Settings, crt: &sr_crt::SharedCrt) -> Menu {
    let p = &s.pad;
    let tabs = vec![
        Tab {
            title: "Display".into(),
            page: 0,
            rows: vec![
                section("Screen"),
                top(choice(
                    "mode",
                    "Window",
                    &["Windowed", "Borderless fullscreen", "Exclusive fullscreen"],
                    match s.window {
                        sr_render::window::WindowMode::Windowed => 0,
                        sr_render::window::WindowMode::Borderless => 1,
                        sr_render::window::WindowMode::Exclusive => 2,
                    },
                    "Alt+Enter switches between windowed and fullscreen. Exclusive uses the display mode under Resolution.",
                )),
                sub({
                    let (options, index) = crate::display::resolution_options(s);
                    choice("resolution", "Resolution", &options.iter().map(String::as_str).collect::<Vec<_>>(), index, "")
                }),
                section("Aspect ratio"),
                top(choice("aspect", "Aspect", crate::display::ASPECT_NAMES, s.aspect.index(), crate::display::ASPECT_HINT)),
                sub(choice(
                    "wide_menus",
                    "Widescreen menus",
                    ON_OFF,
                    s.wide_menus as usize,
                    "Wider than 4:3: the 3D scenes of the menus fill the width; 2D pictures keep black sides.",
                )),
            ],
        },
        Tab {
            title: "Graphics".into(),
            page: 0,
            rows: vec![
                section("Picture quality"),
                top({
                    let (options, index) = crate::display::render_scale_options(s);
                    choice("render_scale", "Render scale", &options.iter().map(String::as_str).collect::<Vec<_>>(), index, crate::display::RENDER_SCALE_HINT)
                }),
                top(choice(
                    "filter",
                    "Texture filtering",
                    &["Sharp (nearest)", "Smooth (bilinear)", "As the game asks"],
                    match s.filter {
                        sr_render::Filter::Nearest => 0,
                        sr_render::Filter::Bilinear => 1,
                        sr_render::Filter::Game => 2,
                    },
                    "Nearest keeps the original texels crisp at any resolution.",
                )),
                advanced("Rarely changed: how overlaps and vertex positions are worked out (the 1997 way or exact)."),
                sub(choice(
                    "depth",
                    "Hidden surfaces",
                    &["Depth buffer (correct)", "Polygon sorting (original)"],
                    (!s.depth_buffer) as usize,
                    "The game sorts polygons and sometimes draws a far one over a near one; the depth buffer resolves every pixel.",
                )),
                sub(choice(
                    "precise",
                    "Precise geometry",
                    ON_OFF,
                    s.precise_geometry as usize,
                    "Exact vertex positions: no wobbling polygons or seams at high resolutions.",
                )),
                section("Frame rate & VSync"),
                top(choice(
                    "frame_rate",
                    "Race frame rate",
                    &["Original (the game's steps)", "Interpolated (smooth)"],
                    s.interpolate as usize,
                    "Extra frames between the game's simulation steps (shown one step, 30 ms at 33 per second, later).",
                )),
                sub(choice("fps_cap", "Frame rate limit", FPS_CAP_NAMES, fps_cap_index(s.fps_cap), "With VSync off and interpolated frames. VSync on: one frame per display refresh.")),
                top(choice("vsync", "VSync", ON_OFF, s.vsync as usize, "Off: no waiting for the display (may tear on screens without variable refresh).")),
                top(choice("show_fps", "Show frame rate", ON_OFF, s.show_fps as usize, "Frames per second in the bottom-left corner.")),
                section("World & detail"),
                top(choice(
                    "detail",
                    "Detail",
                    &["Maximum (all distances)", "Original (lower when far)"],
                    (!s.max_detail) as usize,
                    "Cars, track and scenery at their highest detail at any distance.",
                )),
                top(choice(
                    "visibility",
                    "World drawn",
                    &["Original (game's lists)", "Extended"],
                    s.visibility.min(1) as usize,
                    "Original: the game's hand-made lists (objects pop up). Extended: all it shows from the track in view.",
                )),
                section("Renderer"),
                top(choice(
                    "renderer",
                    "Renderer",
                    &["Original (Glide)", "Claude Renderer"],
                    s.native_renderer as usize,
                    "Claude Renderer: the world drawn by a new renderer from the game's own data. Applies from the next race.",
                )),
                sub(choice(
                    "camera_collision",
                    "Camera collision",
                    ON_OFF,
                    s.camera_collision as usize,
                    "Claude Renderer: the chase cameras stay in front of walls instead of cutting through them.",
                )),
            ],
        },
        Tab {
            title: "Audio".into(),
            page: 0,
            rows: vec![
                section("Volume"),
                top(number("master", "Master volume", pct(s.master_volume), (0, 100, 5), "%", "On top of the game's own volume settings.")),
                top(number("music", "Music (CD)", pct(s.music_volume), (0, 100, 5), "%", "The original CD soundtrack, read from your disc image.")),
                top(number("sfx", "Sound effects", pct(s.sfx_volume), (0, 100, 5), "%", "Engines, collisions, menus.")),
                section("Mixing"),
                top(choice(
                    "mixing",
                    "Sound effect mixing",
                    &["Original (11 kHz)", "Full rate (44.1 kHz)"],
                    (!s.original_mixing) as usize,
                    "Original: mixed at 11025 Hz like the 1997 sound driver. Full: smoother pitch changes.",
                )),
            ],
        },
        Tab {
            title: "Controls".into(),
            page: 0,
            rows: vec![
                section("Keyboard & controller"),
                top(choice(
                    "input",
                    "Input",
                    &["Automatic", "Keyboard", "Controller"],
                    match s.input {
                        InputMode::Auto => 0,
                        InputMode::Keyboard => 1,
                        InputMode::Controller => 2,
                    },
                    "Automatic: controller if one is connected at start. Applies after a restart.",
                )),
                top(choice("menu_buttons", "Controller menu buttons", ON_OFF, s.menu_buttons as usize, "D-pad = arrows, A = Enter, B / Start = Esc, Back = this menu.")),
                top(choice(
                    "vibration",
                    "Vibration",
                    &["Off", "Low", "Medium", "High"],
                    p.vibration.index(),
                    "Controller motors in races: impacts, landings, the road surface. Not with the keyboard.",
                )),
                section("Controller dead zones"),
                top(number("steer_dead_zone", "Steering dead zone", pct(p.steer_dead_zone), (0, 50, 1), "%", "Stick travel ignored around the centre.")),
                top(number("trigger_dead_zone", "Trigger dead zone", pct(p.trigger_dead_zone), (0, 50, 1), "%", "Trigger travel ignored at rest.")),
                advanced("Rarely changed: the game's own joystick dead zone."),
                sub(choice(
                    "compensate",
                    "Remove the game's dead zone",
                    ON_OFF,
                    p.compensate_game_dead_zone as usize,
                    "The game ignores the first 10% of joystick travel; On = only the dead zones above apply.",
                )),
                section("Controller response"),
                top(number("steer_sensitivity", "Steering sensitivity", pct(p.steer_sensitivity), (50, 200, 5), "%", "Multiplier on the stick before the game's own steering curve.")),
                top(choice("steer_curve", "Steering response", CURVE_NAMES, curve_index(p.steer_curve), "Higher = finer control near the centre.")),
                top(choice("pedal_curve", "Pedal response", CURVE_NAMES, curve_index(p.pedal_curve), "Higher = finer control at light throttle and brake.")),
            ],
        },
        Tab {
            title: "Gameplay".into(),
            page: 0,
            rows: vec![
                section("Game"),
                top(choice(
                    "unlock_all",
                    "Unlock all content",
                    ON_OFF,
                    s.unlock_all as usize,
                    "All tracks, championships and the bonus car. Your saved progress is not changed.",
                )),
                top(choice(
                    "conditions",
                    "Race conditions",
                    &["Game's choice", "Day", "Night", "Fog"],
                    s.conditions.min(3) as usize,
                    "Night or fog on every race (Stadium has no night). Changes at once, mid-race too.",
                )),
                section("Driving"),
                top(choice(
                    "autopilot",
                    "Autopilot",
                    if sr_runtime::autopilot::locked_off() { &["Off"] } else { ON_OFF },
                    sr_runtime::autopilot::is_on() as usize,
                    if sr_runtime::autopilot::locked_off() {
                        "Manual driving is locked on for this profile."
                    } else {
                        "The game's own AI drives your car. Tab or the left stick click switch it at any time."
                    },
                )),
                advanced("Rarely changed: the simulation rate, which the car physics depends on."),
                sub(choice(
                    "sim_rate",
                    "Simulation rate",
                    &["33 per second (1997)", "50 per second", "25 per second"],
                    match s.simulation_rate {
                        0..=27 => 2,
                        28..=40 => 0,
                        _ => 1,
                    },
                    "The car physics depends on it: at 33, cars handle as on 1997 PCs. The picture stays smooth.",
                )),
            ],
        },
        crate::crt_menu::tab(s, crt),
    ];
    let mut m = Menu::new(tabs);
    m.toast_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(8));
    update_inactive(&mut m);
    m
}

/// Dim the options that have no effect with the current values of the ones they depend on.
fn update_inactive(menu: &mut Menu) {
    if let Some(i) = menu.item_mut("autopilot") {
        i.inactive = sr_runtime::autopilot::locked_off();
    }
    // the limit paces interpolated frames only (`interp` and `native::menus` read it then)
    let interpolated = index(menu, "frame_rate") == 1;
    // in 4:3 the picture is pillarboxed: there is no width for the menus' scenes to fill
    let wide = sr_render::Aspect::from_index(index(menu, "aspect")) != sr_render::Aspect::Classic4x3;
    if let Some(i) = menu.item_mut("fps_cap") {
        i.inactive = !interpolated;
    }
    if let Some(i) = menu.item_mut("wide_menus") {
        i.inactive = !wide;
    }
    // camera collision corrects the native renderer's camera only
    let native = index(menu, "renderer") == 1;
    if let Some(i) = menu.item_mut("camera_collision") {
        i.inactive = !native;
    }
    // keyboard mode: the controller is not the game's input and does not vibrate
    let keyboard = index(menu, "input") == 1;
    if let Some(i) = menu.item_mut("vibration") {
        i.inactive = keyboard;
    }
}

fn index(menu: &Menu, id: &str) -> usize {
    match menu.item(id).map(|i| &i.value) {
        Some(Value::Choice { index, .. }) => *index,
        _ => 0,
    }
}

fn num(menu: &Menu, id: &str) -> i32 {
    match menu.item(id).map(|i| &i.value) {
        Some(Value::Number { value, .. }) => *value,
        _ => 0,
    }
}

/// Apply the items changed in the menu and save the settings file.
pub fn apply(ctx: &Context, menu: &mut Menu) {
    let ids = std::mem::take(&mut menu.changed);
    if ids.is_empty() {
        return;
    }
    let mut s = ctx.settings.lock().unwrap();
    // the Resolution and Render scale items follow display changes
    let mut display = false;
    let mut note = None;
    let mut crt_note: Option<String> = None;
    for id in &ids {
        match id.as_str() {
            id if id.starts_with("crt_") => {
                if let Some(n) = crate::crt_menu::apply(ctx, menu, &mut s, id) {
                    crt_note = Some(n);
                }
            }
            "mode" => {
                s.window = [sr_render::window::WindowMode::Windowed, sr_render::window::WindowMode::Borderless, sr_render::window::WindowMode::Exclusive][index(menu, "mode").min(2)];
                sr_render::window::set_mode(ctx.hwnd, crate::session_window_mode(s.window));
                display = true;
            }
            "resolution" => {
                note = crate::display::apply_resolution(ctx, &mut s, index(menu, id));
                display = true;
            }
            "render_scale" => {
                crate::display::apply_render_scale(ctx, &mut s, index(menu, id));
                display = true;
            }
            "wide_menus" => {
                s.wide_menus = index(menu, "wide_menus") == 1;
                sr_runtime::widescreen::MENUS_WIDE.store(s.wide_menus, Relaxed);
            }
            "aspect" => {
                crate::display::apply_aspect(ctx, &mut s, index(menu, id));
                display = true;
            }
            "filter" => {
                let i = index(menu, "filter");
                s.filter = [sr_render::Filter::Nearest, sr_render::Filter::Bilinear, sr_render::Filter::Game][i.min(2)];
                ctx.live.filter.store(i as u8, Relaxed);
            }
            "vsync" => {
                s.vsync = index(menu, "vsync") == 1;
                ctx.live.vsync.store(s.vsync, Relaxed);
            }
            "frame_rate" => {
                s.interpolate = index(menu, "frame_rate") == 1;
                sr_runtime::interp::set_enabled(s.interpolate);
            }
            "depth" => {
                s.depth_buffer = index(menu, "depth") == 0;
                ctx.live.depth.store(s.depth_buffer, Relaxed);
            }
            "detail" => {
                s.max_detail = index(menu, "detail") == 0;
                sr_runtime::lod::set_forced(s.max_detail);
            }
            "visibility" => {
                s.visibility = index(menu, "visibility").min(1) as u8;
                sr_runtime::visibility::set_mode(s.visibility);
            }
            "precise" => {
                s.precise_geometry = index(menu, "precise") == 1;
                sr_runtime::subpixel::set_enabled(s.precise_geometry);
            }
            "renderer" => {
                s.native_renderer = index(menu, "renderer") == 1;
                sr_runtime::native::frame::set_requested(crate::session_native_renderer(s.native_renderer));
            }
            "camera_collision" => {
                s.camera_collision = index(menu, "camera_collision") == 1;
                sr_runtime::native::camera::set_enabled(s.camera_collision);
            }
            "show_fps" => {
                s.show_fps = index(menu, "show_fps") == 1;
                ctx.live.show_fps.store(s.show_fps, Relaxed);
            }
            "fps_cap" => {
                s.fps_cap = FPS_CAPS[index(menu, "fps_cap").min(FPS_CAPS.len() - 1)];
                sr_runtime::interp::set_fps_cap(s.fps_cap);
            }
            "master" | "music" | "sfx" => {
                s.master_volume = num(menu, "master") as f32 / 100.0;
                s.music_volume = num(menu, "music") as f32 / 100.0;
                s.sfx_volume = num(menu, "sfx") as f32 / 100.0;
                ctx.audio.set_volumes(sr_audio::Volumes { master: s.master_volume, music: s.music_volume, sfx: s.sfx_volume });
            }
            "mixing" => {
                s.original_mixing = index(menu, "mixing") == 0;
                ctx.audio.set_original_mixing(s.original_mixing);
            }
            "input" => s.input = [InputMode::Auto, InputMode::Keyboard, InputMode::Controller][index(menu, "input").min(2)],
            "menu_buttons" => s.menu_buttons = index(menu, "menu_buttons") == 1,
            "steer_dead_zone" => s.pad.steer_dead_zone = num(menu, id) as f32 / 100.0,
            "trigger_dead_zone" => s.pad.trigger_dead_zone = num(menu, id) as f32 / 100.0,
            "steer_sensitivity" => s.pad.steer_sensitivity = num(menu, id) as f32 / 100.0,
            "steer_curve" => s.pad.steer_curve = CURVES[index(menu, id).min(CURVES.len() - 1)],
            "pedal_curve" => s.pad.pedal_curve = CURVES[index(menu, id).min(CURVES.len() - 1)],
            "compensate" => s.pad.compensate_game_dead_zone = index(menu, id) == 1,
            "vibration" => s.pad.vibration = sr_input::rumble::Level::ALL[index(menu, id).min(3)],
            "sim_rate" => {
                s.simulation_rate = [33, 50, 25][index(menu, "sim_rate").min(2)];
                sr_runtime::pacing::set_rate(s.simulation_rate);
            }
            "autopilot" => {
                sr_runtime::autopilot::set(index(menu, "autopilot") == 1);
            }
            "conditions" => {
                s.conditions = index(menu, "conditions").min(3) as u8;
                sr_runtime::conditions::set(s.conditions);
            }
            "unlock_all" => {
                s.unlock_all = index(menu, "unlock_all") == 1;
                sr_runtime::unlock::set_enabled(s.unlock_all);
            }
            _ => {}
        }
    }
    *ctx.pad.lock().unwrap() = s.pad.clone();
    update_inactive(menu);
    menu.status = match std::fs::write(&ctx.path, s.to_ini()) {
        Ok(()) => crt_note.unwrap_or_else(|| note.unwrap_or("Saved").into()),
        Err(e) => format!("Not saved: {e}"),
    };
    drop(s);
    if display {
        crate::display::refresh_items(ctx, menu, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sr_render::overlay::Layout;

    #[test]
    fn every_option_once_and_fitting() {
        let m = build(&Settings::default(), &sr_crt::CrtLive::new(Default::default(), None));
        assert!(m.layout_problems().is_empty(), "{:#?}", m.layout_problems());
        let all: Vec<&str> = m.tabs.iter().flat_map(|t| t.items()).map(|i| i.id.as_str()).collect();
        let mut unique = all.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), all.len(), "an option twice");
        // (the CRT tab: crt_menu's tests)
        let ids: Vec<&str> = all.iter().copied().filter(|i| !i.starts_with("crt_")).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "an option twice");
        // every option `apply` knows is in the menu
        for id in [
            "mode", "resolution", "wide_menus", "aspect", "render_scale", "filter", "vsync", "frame_rate", "depth", "detail", "visibility", "precise", "renderer", "show_fps", "fps_cap", "master",
            "music", "sfx", "mixing", "input", "menu_buttons", "steer_dead_zone", "trigger_dead_zone", "steer_sensitivity", "steer_curve", "pedal_curve", "compensate", "sim_rate",
            "autopilot", "unlock_all", "conditions", "vibration", "camera_collision",
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
        assert_eq!(ids.len(), 33);
        // every tab, every group open, without scrolling at 720p, 1080p and 2160p, 16:9 and 4:3
        for (w, h, scale) in [(1280, 720, 2), (960, 720, 2), (1920, 1080, 2), (1440, 1080, 2), (3840, 2160, 4), (2880, 2160, 4)] {
            let l = Layout::new(&m, w, h);
            assert_eq!((l.scale, l.scrolls), (scale, false), "{w} x {h}");
        }
    }

    #[test]
    fn dependent_options_dimmed() {
        let mut s = Settings::default();
        s.interpolate = false;
        s.aspect = sr_render::Aspect::Classic4x3;
        let crt = sr_crt::CrtLive::new(Default::default(), None);
        let m = build(&s, &crt);
        assert!(m.item("fps_cap").unwrap().inactive);
        assert!(m.item("wide_menus").unwrap().inactive);
        s.interpolate = true;
        for a in [sr_render::Aspect::Auto, sr_render::Aspect::Wide16x9, sr_render::Aspect::Wide21x9, sr_render::Aspect::Wide32x9] {
            s.aspect = a;
            let m = build(&s, &crt);
            assert!(!m.item("fps_cap").unwrap().inactive);
            assert!(!m.item("wide_menus").unwrap().inactive);
        }
    }
}
