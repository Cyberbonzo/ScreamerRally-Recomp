// No console window for players (the log is logs/runtime.log).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Screamer Rally for modern Windows.
//!
//! ```text
//! ScreamerRally.exe --disc "Screamer Rally.mds" [--data DIR] [--exe START0\STRT3FX.EXE] [--log debug|trace]
//! ```
//!
//! `--data` is the folder that holds the game's writable drive C: (configuration, replays);
//! it defaults to `data` next to the executable.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, OnceLock};

use sr_runtime::log::{self, Level};

mod crt_menu;
mod display;
mod firstrun;
mod keyscript;
mod language;
mod launcher;
mod menu;
mod settings;
mod welcome;

use settings::{InputMode, Settings};

pub fn log_info(s: &str) {
    log::write(Level::Info, format_args!("{s}"));
}

/// The window mode used for `mode` this session: in the visibility editor exclusive
/// fullscreen is borderless instead (an exclusive window is minimised when the editor's
/// window takes the focus), and with a single monitor the game is windowed, so that the
/// editor's window fits beside it (the editor moves and sizes the game's window to the
/// left part of the screen); the setting itself is kept.
pub fn session_window_mode(mode: sr_render::window::WindowMode) -> sr_render::window::WindowMode {
    use sr_render::window::WindowMode;
    if !sr_runtime::visedit::tool_mode() || mode == WindowMode::Windowed {
        return mode;
    }
    let monitors = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows_sys::Win32::UI::WindowsAndMessaging::SM_CMONITORS) };
    if monitors <= 1 { WindowMode::Windowed } else { WindowMode::Borderless }
}

/// The renderer used this session for the setting `native`: the visibility editor's edits are
/// drawn by the native renderer only, so the tool always uses it (the setting is kept).
pub fn session_native_renderer(native: bool) -> bool {
    native || sr_runtime::visedit::tool_mode()
}

struct Args {
    disc: Option<PathBuf>,
    data: PathBuf,
    exe: String,
    level: Level,
    seconds: Option<f64>,
    dump: Option<(PathBuf, u64)>,
    filter: Option<sr_render::Filter>,
    keys: Option<String>,
    pad: Option<String>,
    input: Option<InputMode>,
    fullscreen: bool,
    aspect: Option<sr_render::Aspect>,
    /// `--vis-editor`: the visibility editor (`docs/05-visibility-editor.md`).
    vis_editor: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let mut a = Args { disc: None, data: exe_dir.join("data"), exe: "START0\\STRT3FX.EXE".into(), level: Level::Info, seconds: None, dump: None, filter: None, keys: None, pad: None, input: None, fullscreen: false, aspect: None, vis_editor: false };
    while let Some(k) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{k} needs a value"));
        match k.as_str() {
            "--disc" => a.disc = Some(val()?.into()),
            "--data" => a.data = val()?.into(),
            "--exe" => a.exe = val()?,
            "--dump-frames" => {
                let v = val()?;
                let (dir, every) = v.split_once(',').map(|(d, n)| (d.to_string(), n.parse().unwrap_or(60))).unwrap_or((v, 60));
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                a.dump = Some((dir.into(), every));
            }
            "--filter" => {
                a.filter = Some(match val()?.as_str() {
                    "nearest" => sr_render::Filter::Nearest,
                    "bilinear" => sr_render::Filter::Bilinear,
                    "game" => sr_render::Filter::Game,
                    f => return Err(format!("unknown filter {f}")),
                })
            }
            "--keys" => a.keys = Some(val()?),
            "--pad" => a.pad = Some(val()?),
            "--fullscreen" => a.fullscreen = true,
            "--vis-editor" => a.vis_editor = true,
            "--aspect" => {
                let v = val()?;
                a.aspect = Some(sr_render::Aspect::parse(&v).ok_or_else(|| format!("unknown aspect {v} (auto, 16:9, 21:9, 32:9 or 4:3)"))?)
            }
            "--input" => {
                a.input = Some(match val()?.as_str() {
                    "auto" => InputMode::Auto,
                    "keyboard" => InputMode::Keyboard,
                    "controller" => InputMode::Controller,
                    m => return Err(format!("unknown input mode {m}")),
                })
            }
            "--seconds" => a.seconds = Some(val()?.parse().map_err(|_| "--seconds needs a number")?),
            "--log" => {
                a.level = match val()?.as_str() {
                    "error" => Level::Error,
                    "warn" => Level::Warn,
                    "info" => Level::Info,
                    "debug" => Level::Debug,
                    "trace" => Level::Trace,
                    l => return Err(format!("unknown log level {l}")),
                }
            }
            _ => return Err(format!("unknown argument {k}")),
        }
    }
    Ok(a)
}

/// Create the files the original installer and SETUP program would have written.
/// Returns the keyboard driving keys (merged into the joystick in controller mode).
fn prepare_install(c_root: &Path, game: &Path, controller: bool) -> Result<sr_input::KeyboardKeys, String> {
    let sr = c_root.join("SR");
    let cfg_dir = sr.join("CONFIG");
    std::fs::create_dir_all(&cfg_dir).map_err(|e| e.to_string())?;
    let screamer_cfg = sr.join("SCREAMER.CFG");
    if !screamer_cfg.exists() {
        // install path, CD path, data path, install type — as written by INSTL.EXE for a
        // full installation (the data path equals the install path).
        std::fs::write(&screamer_cfg, b"C:\\SR\nD:\\\nC:\\SR\nn\n").map_err(|e| e.to_string())?;
        log::write(Level::Info, format_args!("created {}", screamer_cfg.display()));
    }
    let controls = cfg_dir.join("CONTROLS.CFG");
    // the default controls: on the CD, or in the installed game's folder
    let defaults = if game.is_dir() {
        std::fs::read(game.join("CONFIG").join("DEFCTRL.CFG")).map_err(|e| format!("CONFIG\\DEFCTRL.CFG: {e}"))?
    } else {
        let d = sr_disc::Disc::open(game).map_err(|e| e.to_string())?;
        let fs = d.filesystem().map_err(|e| e.to_string())?;
        match fs.lookup("CONFIG\\DEFCTRL.CFG") {
            Ok(Some(e)) => fs.read_file(&e).map_err(|e| e.to_string())?,
            _ => return Err("CONFIG\\DEFCTRL.CFG not found on the disc".into()),
        }
    };
    let keys = configure_controls(&controls, &defaults, controller)?;
    // 160-byte record written by DRV\ST.EXE (fields partly known, docs/re/strt3fx.md).
    // +08: digital sound device (HMI ID; E016h = Sound Blaster 16), +10/+14/+18: its port,
    // IRQ and DMA. The device only selects the code path: sound goes to the native mixer.
    let setup = cfg_dir.join("SETUP.CFG");
    let mut s = match std::fs::read(&setup) {
        Ok(b) if b.len() == 160 => b,
        _ => vec![0u8; 160],
    };
    for (off, v) in [(0x08usize, 0xe016u32), (0x10, 0x220), (0x14, 5), (0x18, 1)] {
        s[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&setup, &s).map_err(|e| e.to_string())?;
    Ok(keys)
}

/// Player 1's controls in `CONFIG\CONTROLS.CFG`, as `DRV\ST.EXE` would write them: the
/// keyboard defaults, or the analog joystick (the virtual gameport joystick) with its
/// calibration. Player 2 (split screen) is left as it is.
fn configure_controls(path: &Path, defaults: &[u8], controller: bool) -> Result<sr_input::KeyboardKeys, String> {
    use sr_runtime::joystick::controls_cfg::*;
    if defaults.len() != SIZE {
        return Err(format!("DEFCTRL.CFG has an unexpected size ({} bytes)", defaults.len()));
    }
    let mut cfg = match std::fs::read(path) {
        Ok(c) if c.len() == SIZE => c,
        _ => defaults.to_vec(),
    };
    // keyboard keys of the default player-1 bindings
    let key = |action: usize| {
        let e = &defaults[PLAYER1 + action * 0x1c..];
        let field = |i: usize| i32::from_le_bytes(e[i * 4..i * 4 + 4].try_into().unwrap());
        if field(0) == 1 && field(2) == 1 { field(6) as u8 } else { 0 }
    };
    let keys = sr_input::KeyboardKeys {
        accelerate: key(ACCELERATE),
        brake: key(BRAKE),
        left: key(STEER_LEFT),
        right: key(STEER_RIGHT),
        gear_up: key(GEAR_UP),
        gear_down: key(GEAR_DOWN),
    };
    if controller {
        // virtual joystick: axis 0 steering, 1 accelerate, 3 brake; buttons 1 / 2 gears
        set_binding(&mut cfg, PLAYER1, ACCELERATE, Binding::Axis { axis: 1, dir: 1 });
        set_binding(&mut cfg, PLAYER1, BRAKE, Binding::Axis { axis: 3, dir: 1 });
        set_binding(&mut cfg, PLAYER1, STEER_LEFT, Binding::Axis { axis: 0, dir: -1 });
        set_binding(&mut cfg, PLAYER1, STEER_RIGHT, Binding::Axis { axis: 0, dir: 1 });
        set_binding(&mut cfg, PLAYER1, GEAR_UP, Binding::Button(1));
        set_binding(&mut cfg, PLAYER1, GEAR_DOWN, Binding::Button(2));
        calibrate_device0(&mut cfg);
    } else {
        cfg[PLAYER1..PLAYER2].copy_from_slice(&defaults[PLAYER1..PLAYER2]);
    }
    std::fs::write(path, &cfg).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    log::write(
        Level::Info,
        format_args!("{}: player 1 uses the {}", path.display(), if controller { "analog joystick (controller)" } else { "keyboard" }),
    );
    Ok(keys)
}

fn main() -> ExitCode {
    if let Some(code) = launcher::run_as_launcher() {
        return code;
    }
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            firstrun::message(&e, true);
            return ExitCode::from(2);
        }
    };
    let logs = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("logs"))).unwrap_or_else(|| "logs".into());
    log::init(&logs.join("runtime.log"), args.level);
    let settings_path = args.data.join("settings.ini");
    let (mut settings, warnings) = Settings::load(&settings_path);
    for w in warnings {
        log::write(Level::Warn, format_args!("{}: {w}", settings_path.display()));
    }
    // the game: command line, remembered, a CD image next to the program, or the first-run
    // window (what is learnt is remembered in the settings)
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let search = vec![exe_dir.clone(), std::env::current_dir().unwrap_or_default(), args.data.clone()];
    let read = settings.to_ini();
    let Some(found) = firstrun::choose(&mut settings, args.disc.as_deref(), &mut firstrun::Windows::new(search)) else {
        log::write(Level::Info, format_args!("no game: nothing to run"));
        log::flush();
        return ExitCode::FAILURE;
    };
    let disc = found.path;
    log::write(Level::Info, format_args!("game: {} ({})", found.description, disc.display()));
    // a settings file from an older version lacks the newer options: write it again (with
    // the values just read) so that they appear in it
    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let migrated = settings.migrate();
    let missing = settings.to_ini().lines().filter(|l| !l.starts_with(';') && l.contains(" = ")).any(|l| {
        let key = l.split(" = ").next().unwrap_or("").trim();
        !existing.lines().any(|e| e.split('=').next().is_some_and(|k| k.trim() == key) && e.contains('='))
    });
    if settings.to_ini() != read || missing || migrated {
        if let Err(e) = std::fs::write(&settings_path, settings.to_ini()) {
            log::write(Level::Warn, format_args!("cannot save {}: {e}", settings_path.display()));
        }
    }
    let script = match args.pad.as_deref().map(sr_input::script::Script::parse).transpose() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("--pad: {e}");
            return ExitCode::from(2);
        }
    };
    let controller = match args.input.unwrap_or(settings.input) {
        InputMode::Keyboard => false,
        InputMode::Controller => true,
        InputMode::Auto => {
            let found = script.is_some() || sr_input::connected_controller().is_some();
            log::write(Level::Info, format_args!("input: {}", if found { "controller connected" } else { "no controller connected, using the keyboard" }));
            found
        }
    };
    let c_root = args.data.join("C");
    let keys = match prepare_install(&c_root, &disc, controller) {
        Ok(k) => k,
        Err(e) => {
            log::write(Level::Error, format_args!("cannot prepare the game folder: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let audio = sr_audio::Audio::start();
    audio.set_volumes(sr_audio::Volumes { master: settings.master_volume, music: settings.music_volume, sfx: settings.sfx_volume });
    audio.set_original_mixing(settings.original_mixing);
    let pad = Arc::new(Mutex::new(settings.pad.clone()));
    // CRT emulation: the shader library next to the program (crt-shaders.pak, generated from
    // ShaderGlass; without it the CRT tab says so) and the settings the renderer follows
    let crt_pack = match sr_crt::Pack::open(&sr_crt::pack::default_path()) {
        Ok(p) => {
            log::write(Level::Info, format_args!("crt: {} ({}): {} presets", p.path.display(), p.index.source, p.index.presets.len()));
            Some(Arc::new(p))
        }
        Err(e) => {
            log::write(Level::Info, format_args!("crt: no shader library ({}: {e})", sr_crt::pack::default_path().display()));
            None
        }
    };
    let crt = sr_crt::CrtLive::new(settings.crt_live(), crt_pack);
    crt_menu::set_data(&args.data);
    // the visibility editor (`docs/05-visibility-editor.md`): the game runs as its game window;
    // edits are saved in (and, in normal play too, read from) <data>\visibility
    sr_runtime::visedit::set_dir(args.data.clone());
    #[cfg(feature = "vis-editor")]
    if args.vis_editor {
        sr_runtime::visedit::set_tool_mode(true);
        log::write(Level::Info, format_args!("visibility editor: tool mode"));
        // the editor window (crates/sr-viseditor): its own thread, window and Direct3D
        // device; it opens next to the game's window and closing it leaves the game running
        if let Err(e) = sr_viseditor::spawn() {
            log::write(Level::Error, format_args!("visibility editor: cannot start the window: {e}"));
        }
    }
    #[cfg(not(feature = "vis-editor"))]
    if args.vis_editor {
        log::write(Level::Warn, format_args!("--vis-editor: this build has no visibility editor (feature vis-editor)"));
    }
    // the settings overlay (F12 / controller Back) and what its changes reach
    let overlay: sr_render::overlay::SharedMenu = Arc::new(Mutex::new(menu::build(&settings, &crt)));
    let menu_ctx: Arc<OnceLock<menu::Context>> = Arc::new(OnceLock::new());
    let keyboard_cell: Arc<OnceLock<Arc<sr_input::KeyboardState>>> = Arc::new(OnceLock::new());
    // Keys from the window, scripted keys and the controller's menu buttons: to the overlay
    // while it is open, otherwise to the guest (and the keyboard merge).
    let key_event = {
        let (overlay, menu_ctx, keyboard_cell, crt) = (overlay.clone(), menu_ctx.clone(), keyboard_cell.clone(), crt.clone());
        let after_e0 = Arc::new(std::sync::atomic::AtomicBool::new(false));
        move |sc: u8| {
            // CRT emulation's hold key (ShaderGlass's hold-Tab): while it is down the game is
            // shown without the shader; never passed to the game (pressed while the settings
            // are open it is theirs; released, always the end of the hold)
            let extended = after_e0.swap(sc == 0xe0, std::sync::atomic::Ordering::Relaxed);
            let (hold, active) = {
                let s = crt.settings.lock().unwrap();
                (s.hold_scancode(), s.active)
            };
            if hold != 0 && active && sc != 0xe0 && !extended && sc & 0x7f == hold {
                let down = sc & 0x80 == 0;
                if !down {
                    crt.hold.store(false, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
                if !overlay.lock().unwrap().open {
                    crt.hold.store(true, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
            }
            let (taken, opened) = {
                let mut m = overlay.lock().unwrap();
                let was_open = m.open;
                let taken = m.key(sc);
                if !m.changed.is_empty() {
                    if let Some(ctx) = menu_ctx.get() {
                        menu::apply(ctx, &mut m);
                    }
                }
                // opened: the display items follow the window (resized, moved to a monitor)
                if !was_open && m.open {
                    if let Some(ctx) = menu_ctx.get() {
                        display::refresh_items(ctx, &mut m, true);
                    }
                }
                // closed: the CRT shader in use goes to the top of the recent ones
                if was_open && !m.open {
                    if let Some(ctx) = menu_ctx.get() {
                        crt_menu::closed(ctx, &mut m);
                    }
                }
                (taken, !was_open && m.open)
            };
            let keyboard = keyboard_cell.get();
            if opened {
                // the game is paused with the overlay: let go of any key it sees held
                if let Some(kb) = keyboard {
                    for (code, down) in kb.snapshot().iter().enumerate() {
                        if *down {
                            kb.scancode(code as u8 | 0x80);
                            sr_runtime::events::push_scancode(code as u8 | 0x80);
                        }
                    }
                }
            }
            if taken {
                return;
            }
            // Next race camera: the controller's camera input, or C on the keyboard (which
            // still reaches the game: it types names). The game's own view keys F1-F4 are
            // sent in turn.
            if sc == sr_input::CAMERA_KEY || sc == 0x2e {
                if let Some(f) = sr_runtime::camera::next_view_key() {
                    // held for a few frames: the game copies its key table once per frame
                    sr_runtime::events::push_scancode(f);
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        sr_runtime::events::push_scancode(f | 0x80);
                    });
                }
                if sc == sr_input::CAMERA_KEY {
                    return;
                }
            }
            // Tab or the controller's autopilot input: autopilot on / off (the game does not
            // use Tab; not passed on). The overlay's item follows.
            if sc == 0x0f || sc == sr_input::AUTOPILOT_KEY {
                let on = sr_runtime::autopilot::toggle();
                if let Some(item) = overlay.lock().unwrap().item_mut("autopilot") {
                    if let sr_render::overlay::Value::Choice { index, .. } = &mut item.value {
                        *index = on as usize;
                    }
                }
                log::write(Level::Info, format_args!("autopilot {}", if on { "on" } else { "off" }));
                return;
            }
            if sc == 0x8f || sc == sr_input::AUTOPILOT_KEY | 0x80 {
                return;
            }
            // the controller's mirror input: the game's M key, held for a few frames
            if sc == sr_input::MIRROR_KEY {
                sr_runtime::events::push_scancode(0x32);
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    sr_runtime::events::push_scancode(0x32 | 0x80);
                });
                return;
            }
            if sc == sr_input::CAMERA_KEY | 0x80 || sc == sr_input::MIRROR_KEY | 0x80 {
                return;
            }
            if let Some(kb) = keyboard {
                kb.scancode(sc);
            }
            sr_runtime::events::push_scancode(sc);
        }
    };
    let input = sr_input::start(
        sr_input::InputConfig { pad: pad.clone(), keyboard: controller.then_some(keys), menu_keys: settings.menu_buttons, script, rumble: controller },
        Box::new(|j, connected| sr_runtime::joystick::GAMEPORT.set(j.axes, j.buttons, connected)),
        Box::new(key_event.clone()),
        Box::new(|msg| log::write(Level::Info, format_args!("input: {msg}"))),
    );
    let _ = keyboard_cell.set(input.keyboard.clone());
    // SR_DETERMINISTIC=1: virtual time (diagnostics; see sr_runtime::time)
    if std::env::var_os("SR_DETERMINISTIC").is_some() {
        sr_runtime::time::set_deterministic(true);
    }
    sr_render::window::init_dpi();
    sr_runtime::widescreen::MENUS_WIDE.store(settings.wide_menus, std::sync::atomic::Ordering::Relaxed);
    sr_runtime::pacing::set_rate(settings.simulation_rate);
    sr_runtime::visibility::set_mode(std::env::var("SR_VISIBILITY").ok().and_then(|v| v.parse().ok()).unwrap_or(settings.visibility));
    sr_runtime::conditions::set(std::env::var("SR_CONDITION").ok().and_then(|v| v.parse().ok()).unwrap_or(settings.conditions));
    sr_runtime::lod::set_forced(std::env::var("SR_DETAIL").map(|v| v != "0").unwrap_or(settings.max_detail));
    sr_runtime::subpixel::set_enabled(std::env::var("SR_PRECISE").map(|v| v != "0").unwrap_or(settings.precise_geometry));
    // graphics.renderer (SR_NATIVE=1 / 0 overrides it; read at each race start)
    sr_runtime::native::frame::set_requested(session_native_renderer(settings.native_renderer));
    // graphics.camera_collision (SR_CAMERA_COLLISION=1 / 0 overrides it)
    sr_runtime::native::camera::set_enabled(settings.camera_collision);
    let key_sink = Box::new(key_event.clone());
    let mode = if args.fullscreen { sr_render::window::WindowMode::Borderless } else { session_window_mode(settings.window) };
    let window = match sr_render::window::spawn("Screamer Rally", settings.width, settings.height, mode, key_sink) {
        Ok(w) => w,
        Err(e) => {
            log::write(Level::Error, format_args!("cannot create the window: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let mut render = sr_render::RenderSettings {
        filter: args.filter.unwrap_or(settings.filter),
        vsync: settings.vsync,
        dump: args.dump.clone(),
        aspect: args.aspect.unwrap_or(settings.aspect),
        render_scale: settings.render_scale,
        exclusive_mode: settings.exclusive_resolution,
        depth: std::env::var("SR_DEPTH").map(|v| v != "0").unwrap_or(settings.depth_buffer),
        show_fps: settings.show_fps,
        grow_cracks: std::env::var("SR_PRECISE").map(|v| v != "0").unwrap_or(settings.precise_geometry),
        live: None,
        menu: Some(overlay.clone()),
        crt: Some(crt.clone()),
    };
    let live = sr_render::LiveRender::new(&render);
    render.live = Some(live.clone());
    let _ = menu_ctx.set(menu::Context {
        settings: Mutex::new(settings.clone()),
        path: settings_path.clone(),
        live,
        audio: audio.clone(),
        hwnd: window.hwnd,
        pad,
        crt: crt.clone(),
        data: args.data.clone(),
    });
    // the resolutions the window's monitor offers, the render scale's pixels
    if let Some(ctx) = menu_ctx.get() {
        display::refresh_items(ctx, &mut overlay.lock().unwrap(), true);
    }
    let backend = match sr_render::D3dBackend::new(&window, render) {
        Ok(b) => b,
        Err(e) => {
            log::write(Level::Error, format_args!("cannot initialise Direct3D 11: {e}"));
            return ExitCode::FAILURE;
        }
    };
    // the visibility editor: the player moves between the game's window (driving with the
    // controller, which keeps reaching the game unfocused) and the editor's; a key held in the
    // game's window when another window takes the focus never sees its release there: let go
    // of the keys the game sees held
    if sr_runtime::visedit::tool_mode() {
        let (kb, hwnd) = (input.keyboard.clone(), window.hwnd);
        std::thread::spawn(move || {
            let mut front = true;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let now = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } as isize == hwnd;
                if front && !now {
                    let mut held = 0;
                    for (code, down) in kb.snapshot().iter().enumerate() {
                        if *down {
                            kb.scancode(code as u8 | 0x80);
                            sr_runtime::events::push_scancode(code as u8 | 0x80);
                            held += 1;
                        }
                    }
                    if held > 0 {
                        log::write(Level::Info, format_args!("visibility editor: the game's window lost the focus: {held} held key(s) let go"));
                    }
                }
                front = now;
            }
        });
    }
    // closing the window ends the game
    let closed = window.closed.clone();
    std::thread::spawn(move || {
        while !closed.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        log::write(Level::Info, format_args!("window closed"));
        log::flush();
        sr_runtime::terminate(0);
    });
    if let Some(script) = &args.keys {
        if let Err(e) = keyscript::spawn(script, key_event) {
            log::write(Level::Error, format_args!("--keys: {e}"));
            return ExitCode::from(2);
        }
    }
    let cfg = sr_runtime::Config {
        disc,
        c_root,
        exe: args.exe,
        env: Vec::new(),
        run_for: args.seconds.map(std::time::Duration::from_secs_f64),
        backend: Some(Box::new(backend)),
        audio: Some(audio),
        options: sr_runtime::GameOptions {
            // always the longest: the Claude Renderer and the extended lists draw what is in view
            draw_distance: sr_runtime::widescreen::MAX_DRAW_DISTANCE,
            unlock_all: settings.unlock_all,
            // SR_INTERP=0 / 1 overrides the setting (diagnostics)
            interpolate: std::env::var("SR_INTERP").map(|v| v != "0").unwrap_or(settings.interpolate),
            fps_cap: settings.fps_cap,
        },
    };
    let result = sr_runtime::boot(cfg);
    // the game ended by itself: the controller's motors stop with it
    sr_runtime::rumble::stop();
    match result {
        Ok(code) => {
            log::write(Level::Info, format_args!("game exited with status {code}"));
            log::flush();
            ExitCode::from(code as u8)
        }
        Err(e) => {
            log::write(Level::Error, format_args!("{e}"));
            log::flush();
            ExitCode::FAILURE
        }
    }
}
