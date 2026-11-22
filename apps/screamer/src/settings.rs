//! Settings file (`settings.ini` in the data folder).
//!
//! Plain INI so it can be edited by hand; the in-game settings screen will write the same
//! file. Missing keys take their defaults, unknown keys are reported and ignored. The file
//! is created with all defaults and comments on first run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sr_input::{PadSettings, Source, parse_sources};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    /// Controller if one is connected at startup, else keyboard.
    Auto,
    Keyboard,
    Controller,
}

#[derive(Clone)]
pub struct Settings {
    /// The player's game (`[game] path`): the CD image (`.mds`, `.cue`, `.iso`) or the folder
    /// of the DOS game installed without its CD; asked for at the first start.
    pub game: Option<PathBuf>,
    /// Games remembered by earlier versions (`disc`, `disc_italian`, `disc_english`): tried
    /// when `game` is not set (`firstrun::choose`), never written back.
    pub earlier_games: Vec<PathBuf>,
    pub window: sr_render::window::WindowMode,
    /// Windowed client size.
    pub width: u32,
    pub height: u32,
    /// Exclusive fullscreen's display mode (width, height, refresh rate); `None` = the
    /// desktop's.
    pub exclusive_resolution: Option<(u32, u32, u32)>,
    /// Shape of the picture: the window's (`Auto`, Hor+) or a preset with black bars.
    pub aspect: sr_render::Aspect,
    /// The 3D scene drawn at this percentage of the screen's resolution (50-200).
    pub render_scale: u32,
    /// The menus' 3D scenes widen too (2D pictures keep black sides).
    pub wide_menus: bool,
    pub filter: sr_render::Filter,
    pub vsync: bool,
    pub interpolate: bool,
    pub fps_cap: u32,
    /// Hidden surfaces by depth buffer (on) or by the game's polygon sorting (off).
    pub depth_buffer: bool,
    /// Frame rate counter in a corner of the screen.
    pub show_fps: bool,
    /// Every object at its highest detail level (the game lowers it with distance).
    pub max_detail: bool,
    /// 3D vertices at their exact screen positions (the game rounds to its 640 x 400 grid).
    pub precise_geometry: bool,
    /// World visibility: 0 the game's lists, 1 extended.
    pub visibility: u8,
    /// The world drawn by the Claude Renderer (the native renderer) instead of the game's
    /// Glide drawing (`graphics.renderer = claude`, the default; `glide`: as in 1997).
    pub native_renderer: bool,
    /// Native renderer: the chase cameras kept in front of walls (`graphics.camera_collision`,
    /// on by default; only the drawn camera changes).
    pub camera_collision: bool,
    pub master_volume: f32,
    pub music_volume: f32,
    pub sfx_volume: f32,
    /// Sound effects mixed as the original driver (11025 Hz) instead of at the full rate.
    pub original_mixing: bool,
    pub input: InputMode,
    pub menu_buttons: bool,
    pub unlock_all: bool,
    /// The races' condition (`gameplay.conditions`): 0 the game's choice (night and fog in
    /// some championship stages), 1 day, 2 night, 3 fog (`sr_runtime::conditions`).
    pub conditions: u8,
    /// Simulation steps per second (25, 33 or 50).
    pub simulation_rate: u32,
    pub pad: PadSettings,
    /// CRT emulation (`[crt]`): the CRT tab's options; `crt.params` holds the chosen
    /// preset's changed parameters (filled from `crt_params` when used).
    pub crt: sr_crt::CrtSettings,
    /// Personal favourites and recently used presets ("category/name"), newest first.
    pub crt_favorites: Vec<String>,
    pub crt_recent: Vec<String>,
    /// Parameters changed from their defaults, per preset (`[crt.params]`).
    pub crt_params: BTreeMap<String, Vec<(String, f32)>>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            game: None,
            earlier_games: Vec::new(),
            window: sr_render::window::WindowMode::Windowed,
            width: 1280,
            height: 960,
            exclusive_resolution: None,
            aspect: sr_render::Aspect::Auto,
            render_scale: 100,
            wide_menus: true,
            filter: sr_render::Filter::Nearest,
            vsync: true,
            interpolate: true,
            fps_cap: 0,
            depth_buffer: true,
            show_fps: false,
            max_detail: true,
            precise_geometry: true,
            visibility: 1,
            native_renderer: true,
            camera_collision: true,
            master_volume: 1.0,
            music_volume: 1.0,
            sfx_volume: 1.0,
            original_mixing: true,
            input: InputMode::Auto,
            menu_buttons: true,
            unlock_all: false,
            conditions: 0,
            simulation_rate: 33,
            pad: PadSettings::default(),
            crt: sr_crt::CrtSettings::default(),
            crt_favorites: Vec::new(),
            crt_recent: Vec::new(),
            crt_params: BTreeMap::new(),
        }
    }
}

fn sources(v: &[Source]) -> String {
    v.iter().map(Source::name).collect::<Vec<_>>().join(",")
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn path(p: &Option<PathBuf>) -> String {
    p.as_ref().map(|d| d.display().to_string()).unwrap_or_default()
}

/// `desktop` (or empty) or `WIDTHxHEIGHT@HZ` (spaces allowed, `Hz` optional).
fn parse_mode(v: &str) -> Result<Option<(u32, u32, u32)>, ()> {
    let v = v.trim().to_ascii_lowercase().replace(' ', "");
    if v.is_empty() || v == "desktop" {
        return Ok(None);
    }
    let v = v.trim_end_matches("hz");
    let (size, hz) = v.split_once('@').ok_or(())?;
    let (w, h) = size.split_once('x').ok_or(())?;
    let n = |s: &str, lo: u32, hi: u32| s.parse::<u32>().ok().filter(|x| (lo..=hi).contains(x)).ok_or(());
    Ok(Some((n(w, 320, 16384)?, n(h, 200, 16384)?, n(hz, 1, 1000)?)))
}

/// A list of presets in the settings file: separated by `|` (names have spaces, commas).
fn preset_list(v: &[String]) -> String {
    v.join(" | ")
}

fn parse_preset_list(v: &str) -> Vec<String> {
    v.split('|').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// A float written short (1.5, not 1.500000).
pub fn short(v: f32) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" || s == "-0" { "0".into() } else { s.into() }
}

/// The settings file's word for a hold key ("f11", "scroll_lock").
fn hold_key_word(i: usize) -> String {
    match sr_crt::config::HOLD_KEYS.get(i) {
        Some((0x29, _)) => "grave".into(),
        Some((_, n)) => n.to_ascii_lowercase().replace(' ', "_"),
        None => "off".into(),
    }
}

impl Settings {
    /// The `[crt]` and `[crt.params]` sections (also a CRT profile's contents, with the
    /// chosen preset's parameters only).
    pub fn crt_ini(&self, all_params: bool) -> String {
        use sr_crt::config::{ASPECT_RATIOS, OUTPUT_SCALES, PIXEL_SIZES};
        let c = &self.crt;
        let flip = match (c.flip_h, c.flip_v) {
            (false, false) => "none",
            (true, false) => "horizontal",
            (false, true) => "vertical",
            (true, true) => "both",
        };
        let mut params = String::new();
        for (preset, list) in &self.crt_params {
            if !all_params && *preset != c.preset {
                continue;
            }
            for (n, v) in list {
                params.push_str(&format!("{preset}:{n} = {}\n", short(*v)));
            }
        }
        format!(
            "[crt]\n\
             ; CRT emulation (a port of ShaderGlass and its RetroArch shader library): on / off\n\
             active = {}\n\
             ; the shader preset, category/name as the CRT tab lists it (empty: ShaderGlass's default)\n\
             shader = {}\n\
             ; the picture the shader gets: 480 (the game's own 480 lines; default), 240, 360, 600 or 720 lines,\n\
             ; or the screen's size divided: x1, x2, x2.25, x3, x4, x4.5, x5, x5.4, x6, x7, x7.2, x8, x9, x10, x10.8\n\
             pixel_size = {}\n\
             ; aspect ratio correction (the height of an input pixel): 1, 1.2, 0.94, 0.8, 0.86, 0.5, 2.0,\n\
             ; custom (custom_aspect_ratio) or 0 (stretched over the window)\n\
             aspect_ratio = {}\n\
             custom_aspect_ratio = {}\n\
             ; output scale: free (the largest that fits) or a percentage of the game's picture (25 to 1000)\n\
             scale = {}\n\
             ; flip: none, horizontal, vertical or both\n\
             flip = {flip}\n\
             ; orientation: horizontal, or vertical (the shader works on the picture turned a quarter)\n\
             orientation = {}\n\
             ; held down, this key shows the game without the shader: off, f11, scroll_lock, backspace, grave\n\
             hold_key = {}\n\
             ; personal favourites and recently used shaders, separated by |\n\
             favorites = {}\n\
             recent = {}\n\
             \n\
             [crt.params]\n\
             ; shader parameters changed from their defaults: shader:parameter = value\n\
             {params}",
            on_off(c.active),
            c.preset,
            PIXEL_SIZES.get(c.pixel_size).map_or("480", |p| p.2),
            ASPECT_RATIOS.get(c.aspect).map_or("1", |a| a.2),
            short(c.custom_aspect),
            OUTPUT_SCALES.get(c.scale).map_or("free", |o| o.2),
            if c.vertical { "vertical" } else { "horizontal" },
            hold_key_word(c.hold_key),
            preset_list(&self.crt_favorites),
            preset_list(&self.crt_recent),
        )
    }

    /// The CRT settings for the renderer: the chosen preset's parameters filled in.
    pub fn crt_live(&self) -> sr_crt::CrtSettings {
        let mut c = self.crt.clone();
        c.params = self.crt_params.get(&c.preset).cloned().unwrap_or_default();
        c
    }

    /// The file contents for these settings, with comments.
    pub fn to_ini(&self) -> String {
        let p = &self.pad;
        let filter = match self.filter {
            sr_render::Filter::Nearest => "nearest",
            sr_render::Filter::Bilinear => "bilinear",
            sr_render::Filter::Game => "game",
        };
        let input = match self.input {
            InputMode::Auto => "auto",
            InputMode::Keyboard => "keyboard",
            InputMode::Controller => "controller",
        };
        let pct = |x: f32| (x * 100.0).round() as i32;
        format!(
            "; Screamer Rally settings. Edit while the game is not running.\n\
             \n\
             [game]\n\
             ; your Screamer Rally: the CD image (.mds, .cue or .iso: with the CD music) or the folder of the\n\
             ; DOS game installed without its CD (no music); asked for when missing. Its language is the game's\n\
             path = {}\n\
             \n\
             [display]\n\
             ; windowed, borderless (fullscreen on the window's monitor, at its resolution) or exclusive\n\
             ; (fullscreen in the display mode below); Alt+Enter switches between windowed and fullscreen\n\
             mode = {}\n\
             ; window client size (windowed)\n\
             width = {}\n\
             height = {}\n\
             ; exclusive fullscreen: desktop (the desktop's display mode) or a mode the monitor offers,\n\
             ; WIDTHxHEIGHT@HZ (e.g. 2560x1440@144)\n\
             exclusive_resolution = {}\n\
             ; auto: the picture fills the window, wider windows show more of the scene to the sides (no\n\
             ; stretching); 16:9, 21:9 (2.37:1), 32:9 or 4:3: the picture has that shape, the scene as wide\n\
             ; as it, with black bars where the window's shape differs\n\
             aspect = {}\n\
             ; wide windows: the menus' 3D scenes fill the width too (2D pictures keep black sides)\n\
             wide_menus = {}\n\
             \n\
             [graphics]\n\
             ; the 3D scene drawn at this percentage of the screen's resolution, 50 to 200 (100 = as is; below:\n\
             ; faster, softer; above: smoother edges). The HUD, the menus' pictures and text stay sharp\n\
             render_scale = {}\n\
             ; texture filtering: nearest (sharp pixels, default), bilinear, game (what the game asks the 3dfx card for)\n\
             filter = {filter}\n\
             vsync = {}\n\
             ; races at more frames than the game's simulation steps: frames in between are drawn with\n\
             ; the camera and cars interpolated (the game is shown one simulation step later)\n\
             interpolate = {}\n\
             ; with vsync off: at most this many frames per second (0 = no limit)\n\
             fps_cap = {}\n\
             ; on: overlapping surfaces are resolved per pixel (depth buffer); off: the game's own\n\
             ; polygon sorting decides what is in front (as in 1997, with its occasional errors)\n\
             depth_buffer = {}\n\
             ; frame rate counter in the bottom-left corner\n\
             show_fps = {}\n\
             ; maximum: cars, track and scenery always at their highest detail level; original: the\n\
             ; game lowers the detail with distance (its track sections show cracks where levels meet)\n\
             detail = {}\n\
             ; on: 3D vertices at their exact positions; off: rounded to the game's 640 x 400 grid as in\n\
             ; 1997 (at high resolutions polygons then wobble and seams open between them)\n\
             precise_geometry = {}\n\
             ; what of the world is drawn: extended (everything the game shows from any part of the track\n\
             ; in view; default) or original (the game's hand-made lists exactly, as in 1997: objects pop\n\
             ; up where the lists end)\n\
             visibility = {}\n\
             ; claude: the Claude Renderer draws the world from the game's data (default); glide: the game\n\
             ; draws it as in 1997. Applies from the next race\n\
             renderer = {}\n\
             ; Claude Renderer: the chase cameras stay in front of walls instead of cutting through them\n\
             camera_collision = {}\n\
             \n\
             [audio]\n\
             ; percent, on top of the game's own volume settings\n\
             master_volume = {}\n\
             music_volume = {}\n\
             sfx_volume = {}\n\
             ; sound effects: original (mixed at 11025 Hz like the 1997 sound driver) or full (44.1 kHz)\n\
             mixing = {}\n\
             \n\
             [controls]\n\
             ; auto: controller if one is connected at startup, otherwise keyboard; or keyboard / controller.\n\
             ; In controller mode the keyboard driving keys keep working too.\n\
             input = {input}\n\
             ; D-pad = arrow keys, A = Enter, B / Start = Esc (menus and pause)\n\
             menu_buttons = {}\n\
             ; controller vibration in races (impacts, landings, the road surface): off, low, medium or high\n\
             vibration = {}\n\
             \n\
             [gameplay]\n\
             ; all tracks and cars selectable; your saved progress is not changed\n\
             unlock_all = {}\n\
             ; races at night or in fog: game (as the game chooses: some championship stages), day, night\n\
             ; or fog (every race; changes at once, mid-race too; Stadium has no night)\n\
             conditions = {}\n\
             ; simulation steps per second: 33 (as 1997 hardware ran the game; default), 50 or 25. The car\n\
             ; physics depends on it (at 50 cars slide more); the picture stays smooth at any rate\n\
             simulation_rate = {}\n\
             \n\
             [controller]\n\
             ; inputs: LX LY RX RY (stick axes), LX+ LX- LY+ ... (half axes), LT RT (triggers),\n\
             ; A B X Y LB RB BACK START LS RS UP DOWN LEFT RIGHT (buttons); several separated by commas\n\
             steer = {}\n\
             accelerate = {}\n\
             brake = {}\n\
             gear_up = {}\n\
             gear_down = {}\n\
             ; next race camera (the game's own views F1-F4 in turn; on the keyboard: C)\n\
             camera = {}\n\
             ; rear-view mirror on / off (on the keyboard: M)\n\
             mirror = {}\n\
             ; the game's AI drives your car (on the keyboard: Tab)\n\
             autopilot = {}\n\
             ; dead zones in percent of travel\n\
             steer_dead_zone = {}\n\
             trigger_dead_zone = {}\n\
             ; steering sensitivity in percent (input multiplier) and response curve (1.0 = linear)\n\
             steer_sensitivity = {}\n\
             steer_curve = {}\n\
             pedal_curve = {}\n\
             ; the game ignores the first 10% of joystick travel; on = only the dead zones above apply\n\
             compensate_game_dead_zone = {}\n\
             \n\
             {}",
            path(&self.game),
            match self.window {
                sr_render::window::WindowMode::Windowed => "windowed",
                sr_render::window::WindowMode::Borderless => "borderless",
                sr_render::window::WindowMode::Exclusive => "exclusive",
            },
            self.width,
            self.height,
            self.exclusive_resolution.map_or("desktop".to_string(), |(w, h, hz)| format!("{w}x{h}@{hz}")),
            self.aspect.key(),
            on_off(self.wide_menus),
            self.render_scale,
            on_off(self.vsync),
            on_off(self.interpolate),
            self.fps_cap,
            on_off(self.depth_buffer),
            on_off(self.show_fps),
            if self.max_detail { "maximum" } else { "original" },
            on_off(self.precise_geometry),
            ["original", "extended"][self.visibility.min(1) as usize],
            if self.native_renderer { "claude" } else { "glide" },
            on_off(self.camera_collision),
            pct(self.master_volume),
            pct(self.music_volume),
            pct(self.sfx_volume),
            if self.original_mixing { "original" } else { "full" },
            on_off(self.menu_buttons),
            p.vibration.key(),
            on_off(self.unlock_all),
            ["game", "day", "night", "fog"][self.conditions.min(3) as usize],
            self.simulation_rate,
            sources(&p.steer),
            sources(&p.accelerate),
            sources(&p.brake),
            sources(&p.gear_up),
            sources(&p.gear_down),
            sources(&p.camera),
            sources(&p.mirror),
            sources(&p.autopilot),
            pct(p.steer_dead_zone),
            pct(p.trigger_dead_zone),
            pct(p.steer_sensitivity),
            p.steer_curve,
            p.pedal_curve,
            on_off(p.compensate_game_dead_zone),
            self.crt_ini(true),
        )
    }

    /// Settings written by the previous version with its default controller layout (Y / X
    /// as second gear buttons, the camera only on the right stick click) get the current
    /// defaults (Y camera, X mirror). Returns true when something changed.
    pub fn migrate(&mut self) -> bool {
        use sr_input::Source::Button;
        let p = &mut self.pad;
        if p.gear_up == [Button(0x0200), Button(0x8000)] && p.gear_down == [Button(0x0100), Button(0x4000)] && p.camera == [Button(0x0080)] {
            let d = sr_input::PadSettings::default();
            p.gear_up = d.gear_up;
            p.gear_down = d.gear_down;
            p.camera = d.camera;
            p.mirror = d.mirror;
            return true;
        }
        false
    }

    /// Parse a settings file; problems are returned as warnings, never fatal.
    pub fn parse(text: &str) -> (Settings, Vec<String>) {
        let mut s = Settings::default();
        let mut warn = Vec::new();
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        let mut section = String::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.split(';').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].trim().to_ascii_lowercase();
                continue;
            }
            match line.split_once('=') {
                Some((k, v)) => {
                    // shader parameter names keep their case (several differ only by it)
                    let k = if section == "crt.params" { k.trim().to_string() } else { k.trim().to_ascii_lowercase() };
                    values.insert(format!("{section}.{k}"), v.trim().to_string());
                }
                None => warn.push(format!("line {}: expected key = value", n + 1)),
            }
        }
        for (key, v) in &values {
            let num = |lo: f32, hi: f32| -> Option<f32> { v.parse::<f32>().ok().filter(|x| (lo..=hi).contains(x)) };
            let flag = || match v.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => Some(true),
                "off" | "false" | "no" | "0" => Some(false),
                _ => None,
            };
            let p = &mut s.pad;
            let ok = match key.as_str() {
                "game.path" => {
                    s.game = (!v.is_empty()).then(|| PathBuf::from(v));
                    true
                }
                // earlier versions: their images, tried when no game is set; the language
                // is the game's now
                "game.disc" | "game.disc_italian" | "game.disc_english" => {
                    if !v.is_empty() {
                        s.earlier_games.push(PathBuf::from(v));
                    }
                    true
                }
                "game.language" => true,
                "display.mode" => match v.to_ascii_lowercase().as_str() {
                    "windowed" | "window" => Some(sr_render::window::WindowMode::Windowed),
                    "borderless" | "fullscreen" => Some(sr_render::window::WindowMode::Borderless),
                    "exclusive" => Some(sr_render::window::WindowMode::Exclusive),
                    _ => None,
                }
                .map(|w| s.window = w)
                .is_some(),
                "display.width" => num(320.0, 16384.0).map(|x| s.width = x as u32).is_some(),
                "display.height" => num(240.0, 16384.0).map(|x| s.height = x as u32).is_some(),
                // `wide` (earlier versions) is `auto`
                "display.aspect" => sr_render::Aspect::parse(v).map(|a| s.aspect = a).is_some(),
                "display.exclusive_resolution" => match parse_mode(v) {
                    Ok(m) => {
                        s.exclusive_resolution = m;
                        true
                    }
                    Err(()) => false,
                },
                "graphics.render_scale" => {
                    num(sr_render::display::RENDER_SCALE_MIN as f32, sr_render::display::RENDER_SCALE_MAX as f32).map(|x| s.render_scale = x.round() as u32).is_some()
                }
                "display.wide_menus" => flag().map(|b| s.wide_menus = b).is_some(),
                "graphics.filter" => match v.to_ascii_lowercase().as_str() {
                    "nearest" => Some(sr_render::Filter::Nearest),
                    "bilinear" => Some(sr_render::Filter::Bilinear),
                    "game" => Some(sr_render::Filter::Game),
                    _ => None,
                }
                .map(|f| s.filter = f)
                .is_some(),
                "graphics.vsync" => flag().map(|b| s.vsync = b).is_some(),
                // earlier versions: the draw distance is always the longest now
                "graphics.draw_distance" => true,
                "graphics.interpolate" => flag().map(|b| s.interpolate = b).is_some(),
                "graphics.fps_cap" => num(0.0, 1000.0).map(|x| s.fps_cap = x as u32).is_some(),
                "graphics.depth_buffer" => flag().map(|b| s.depth_buffer = b).is_some(),
                "gameplay.simulation_rate" => num(20.0, 60.0).map(|x| s.simulation_rate = x as u32).is_some(),
                "graphics.show_fps" => flag().map(|b| s.show_fps = b).is_some(),
                "graphics.detail" => match v.to_ascii_lowercase().as_str() {
                    "maximum" | "max" | "high" => Some(true),
                    "original" => Some(false),
                    _ => None,
                }
                .map(|b| s.max_detail = b)
                .is_some(),
                "graphics.precise_geometry" => flag().map(|b| s.precise_geometry = b).is_some(),
                "graphics.visibility" => match v.to_ascii_lowercase().as_str() {
                    "original" => Some(0),
                    "extended" => Some(1),
                    // earlier versions' "everything in view": extended
                    "everything" | "all" => Some(1),
                    _ => None,
                }
                .map(|x| s.visibility = x)
                .is_some(),
                "graphics.renderer" => match v.to_ascii_lowercase().as_str() {
                    "glide" | "original" => Some(false),
                    "claude" | "native" => Some(true),
                    _ => None,
                }
                .map(|b| s.native_renderer = b)
                .is_some(),
                "graphics.camera_collision" => flag().map(|b| s.camera_collision = b).is_some(),
                "audio.master_volume" => num(0.0, 100.0).map(|x| s.master_volume = x / 100.0).is_some(),
                "audio.music_volume" => num(0.0, 100.0).map(|x| s.music_volume = x / 100.0).is_some(),
                "audio.sfx_volume" => num(0.0, 100.0).map(|x| s.sfx_volume = x / 100.0).is_some(),
                "audio.mixing" => match v.to_ascii_lowercase().as_str() {
                    "original" => Some(true),
                    "full" => Some(false),
                    _ => None,
                }
                .map(|o| s.original_mixing = o)
                .is_some(),
                "controls.input" => match v.to_ascii_lowercase().as_str() {
                    "auto" => Some(InputMode::Auto),
                    "keyboard" => Some(InputMode::Keyboard),
                    "controller" => Some(InputMode::Controller),
                    _ => None,
                }
                .map(|m| s.input = m)
                .is_some(),
                "controls.menu_buttons" => flag().map(|b| s.menu_buttons = b).is_some(),
                "controls.vibration" => sr_input::rumble::Level::parse(v).map(|l| p.vibration = l).is_some(),
                "gameplay.unlock_all" => flag().map(|b| s.unlock_all = b).is_some(),
                "gameplay.conditions" => match v.to_ascii_lowercase().as_str() {
                    "game" | "auto" => Some(0),
                    "day" => Some(1),
                    "night" => Some(2),
                    "fog" => Some(3),
                    _ => None,
                }
                .map(|x| s.conditions = x)
                .is_some(),
                "controller.steer" | "controller.accelerate" | "controller.brake" | "controller.gear_up" | "controller.gear_down" | "controller.camera" | "controller.mirror" | "controller.autopilot" => {
                    match parse_sources(v) {
                        Ok(list) => {
                            let slot = match key.as_str() {
                                "controller.steer" => &mut p.steer,
                                "controller.accelerate" => &mut p.accelerate,
                                "controller.brake" => &mut p.brake,
                                "controller.gear_up" => &mut p.gear_up,
                                "controller.camera" => &mut p.camera,
                                "controller.mirror" => &mut p.mirror,
                                "controller.autopilot" => &mut p.autopilot,
                                _ => &mut p.gear_down,
                            };
                            *slot = list;
                            true
                        }
                        Err(e) => {
                            warn.push(format!("{key}: {e}"));
                            continue;
                        }
                    }
                }
                "controller.steer_dead_zone" => num(0.0, 90.0).map(|x| p.steer_dead_zone = x / 100.0).is_some(),
                "controller.trigger_dead_zone" => num(0.0, 90.0).map(|x| p.trigger_dead_zone = x / 100.0).is_some(),
                "controller.steer_sensitivity" => num(10.0, 400.0).map(|x| p.steer_sensitivity = x / 100.0).is_some(),
                "controller.steer_curve" => num(0.2, 5.0).map(|x| p.steer_curve = x).is_some(),
                "controller.pedal_curve" => num(0.2, 5.0).map(|x| p.pedal_curve = x).is_some(),
                "controller.compensate_game_dead_zone" => flag().map(|b| p.compensate_game_dead_zone = b).is_some(),
                "crt.active" => flag().map(|b| s.crt.active = b).is_some(),
                "crt.shader" => {
                    s.crt.preset = v.clone();
                    true
                }
                "crt.pixel_size" => {
                    let v = v.to_ascii_lowercase();
                    sr_crt::config::PIXEL_SIZES.iter().position(|p| p.2 == v || p.2 == v.trim_end_matches(" lines")).map(|i| s.crt.pixel_size = i).is_some()
                }
                "crt.aspect_ratio" => {
                    let v = v.to_ascii_lowercase();
                    let table = sr_crt::config::ASPECT_RATIOS;
                    table
                        .iter()
                        .position(|a| a.2 == v)
                        .or_else(|| v.parse::<f32>().ok().and_then(|x| table.iter().position(|a| a.0 >= 0.0 && (a.0 - x).abs() < 0.005)))
                        .map(|i| s.crt.aspect = i)
                        .is_some()
                }
                "crt.custom_aspect_ratio" => num(0.1, 10.0).map(|x| s.crt.custom_aspect = x).is_some(),
                "crt.scale" => {
                    let v = v.to_ascii_lowercase();
                    sr_crt::config::OUTPUT_SCALES.iter().position(|o| o.2 == v.trim_end_matches('%')).map(|i| s.crt.scale = i).is_some()
                }
                "crt.flip" => match v.to_ascii_lowercase().as_str() {
                    "none" | "off" => Some((false, false)),
                    "horizontal" => Some((true, false)),
                    "vertical" => Some((false, true)),
                    "both" => Some((true, true)),
                    _ => None,
                }
                .map(|(h, vv)| (s.crt.flip_h, s.crt.flip_v) = (h, vv))
                .is_some(),
                "crt.orientation" => match v.to_ascii_lowercase().as_str() {
                    "horizontal" => Some(false),
                    "vertical" => Some(true),
                    _ => None,
                }
                .map(|b| s.crt.vertical = b)
                .is_some(),
                "crt.hold_key" => {
                    let v = v.to_ascii_lowercase().replace(' ', "_");
                    (0..sr_crt::config::HOLD_KEYS.len()).find(|&i| hold_key_word(i) == v).map(|i| s.crt.hold_key = i).is_some()
                }
                "crt.favorites" => {
                    s.crt_favorites = parse_preset_list(v);
                    true
                }
                "crt.recent" => {
                    s.crt_recent = parse_preset_list(v);
                    true
                }
                k if k.starts_with("crt.params.") => {
                    // "category/name:Parameter" (the last colon: names may have one)
                    let rest = &k["crt.params.".len()..];
                    match (rest.rsplit_once(':'), v.parse::<f32>()) {
                        (Some((preset, name)), Ok(x)) if !preset.is_empty() && !name.is_empty() => {
                            let list = s.crt_params.entry(preset.to_string()).or_default();
                            list.retain(|(n, _)| n != name);
                            list.push((name.to_string(), x));
                            true
                        }
                        _ => false,
                    }
                }
                _ => {
                    warn.push(format!("unknown setting {key}"));
                    continue;
                }
            };
            if !ok {
                warn.push(format!("{key}: invalid value '{v}' (default kept)"));
            }
        }
        (s, warn)
    }

    /// Load `path`, creating it with defaults if missing.
    pub fn load(path: &Path) -> (Settings, Vec<String>) {
        match std::fs::read_to_string(path) {
            Ok(t) => Settings::parse(&t),
            Err(_) => {
                let s = Settings::default();
                let mut warn = Vec::new();
                if let Some(d) = path.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                if let Err(e) = std::fs::write(path, s.to_ini()) {
                    warn.push(format!("cannot create {}: {e}", path.display()));
                }
                (s, warn)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let d = Settings::default();
        let (s, warn) = Settings::parse(&d.to_ini());
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(s.to_ini(), d.to_ini());
    }

    #[test]
    fn the_game_and_earlier_keys() {
        let (s, warn) = Settings::parse("[game]\npath = C:\\games\\sr\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(s.game.as_deref(), Some(Path::new("C:\\games\\sr")));
        let (back, _) = Settings::parse(&s.to_ini());
        assert_eq!(back.to_ini(), s.to_ini());
        assert!(Settings::default().to_ini().contains("\npath = \n"));
        // earlier versions' keys: read (by key name), never written back, no warnings
        let (s, warn) = Settings::parse("[game]\nlanguage = English\ndisc_italian = C:\\cd\\Screamer Rally.mds\ndisc_english = D:\\en\\srally.cue\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(s.game, None);
        assert_eq!(s.earlier_games, [Path::new("D:\\en\\srally.cue"), Path::new("C:\\cd\\Screamer Rally.mds")]);
        assert!(!s.to_ini().lines().any(|l| l.starts_with("disc") || l.starts_with("language")));
    }

    #[test]
    fn values_and_warnings() {
        let (s, warn) = Settings::parse("[controls]\ninput = controller\n[controller]\nsteer = RX\nsteer_dead_zone = 15\nbrake = LT,B\nfoo = 1\n[audio]\nmusic_volume = 250\n");
        assert_eq!(s.input, InputMode::Controller);
        assert_eq!(s.pad.steer, vec![Source::Axis(sr_input::Stick::RX)]);
        assert!((s.pad.steer_dead_zone - 0.15).abs() < 1e-6);
        assert_eq!(s.pad.brake.len(), 2);
        assert_eq!(s.music_volume, 1.0);
        assert_eq!(warn.len(), 2, "{warn:?}");
    }

    #[test]
    fn vibration_setting() {
        use sr_input::rumble::Level;
        assert_eq!(Settings::default().pad.vibration, Level::Medium);
        assert!(Settings::default().to_ini().contains("\nvibration = medium\n"));
        let (s, warn) = Settings::parse("[controls]\nvibration = High\n");
        assert!(warn.is_empty() && s.pad.vibration == Level::High, "{warn:?}");
        assert!(s.to_ini().contains("\nvibration = high\n"));
        let (s, warn) = Settings::parse("[controls]\nvibration = strong\n");
        assert!(s.pad.vibration == Level::Medium && warn.len() == 1);
    }

    #[test]
    fn display_settings() {
        use sr_render::Aspect;
        // defaults: as before the presets (the window's aspect, full resolution, the desktop)
        let d = Settings::default();
        assert_eq!((d.aspect, d.render_scale, d.exclusive_resolution), (Aspect::Auto, 100, None));
        assert!(d.to_ini().contains("\naspect = auto\n") && d.to_ini().contains("\nrender_scale = 100\n") && d.to_ini().contains("\nexclusive_resolution = desktop\n"));
        // earlier versions' files: `wide` is auto, 4:3 stays 4:3
        let (s, warn) = Settings::parse("[display]\naspect = wide\nwidth = 1280\nheight = 720\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!((s.aspect, s.width, s.height, s.render_scale), (Aspect::Auto, 1280, 720, 100));
        assert!(s.to_ini().contains("\naspect = auto\n"));
        let (s, warn) = Settings::parse("[display]\naspect = 4:3\n");
        assert!(warn.is_empty() && s.aspect == Aspect::Classic4x3);
        // every preset, a mode and a render scale round trip
        for a in Aspect::ALL {
            let mut s = Settings::default();
            s.aspect = a;
            s.render_scale = 75;
            s.exclusive_resolution = Some((2560, 1440, 144));
            let (back, warn) = Settings::parse(&s.to_ini());
            assert!(warn.is_empty(), "{warn:?}");
            assert_eq!((back.aspect, back.render_scale, back.exclusive_resolution), (a, 75, Some((2560, 1440, 144))));
            assert_eq!(back.to_ini(), s.to_ini());
        }
        assert!(Settings::parse("[display]\nexclusive_resolution = 2560x1440@144\n").0.to_ini().contains("\nexclusive_resolution = 2560x1440@144\n"));
        let (s, warn) = Settings::parse("[display]\nexclusive_resolution = 1920 x 1080 @ 60 Hz\naspect = 21:9\n[graphics]\nrender_scale = 150\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!((s.exclusive_resolution, s.aspect, s.render_scale), (Some((1920, 1080, 60)), Aspect::Wide21x9, 150));
        // invalid values: reported, defaults kept
        let (s, warn) = Settings::parse("[display]\nexclusive_resolution = huge\naspect = 5:4\n[graphics]\nrender_scale = 400\n");
        assert_eq!(warn.len(), 3, "{warn:?}");
        assert_eq!((s.exclusive_resolution, s.aspect, s.render_scale), (None, Aspect::Auto, 100));
    }

    #[test]
    fn renderer_switch() {
        assert!(Settings::default().native_renderer, "the Claude Renderer is the default");
        assert!(Settings::default().to_ini().contains("renderer = claude"));
        let (s, warn) = Settings::parse("[graphics]\nrenderer = glide\n");
        assert!(!s.native_renderer && warn.is_empty());
        assert!(s.to_ini().contains("renderer = glide"));
        // earlier versions' name
        let (s, warn) = Settings::parse("[graphics]\nrenderer = native\n");
        assert!(s.native_renderer && warn.is_empty());
        let (s, warn) = Settings::parse("[graphics]\nrenderer = vulkan\n");
        assert!(s.native_renderer && warn.len() == 1);
        // earlier versions' keys and values: no warnings
        let (s, warn) = Settings::parse("[graphics]\ndraw_distance = 150\nvisibility = everything\n");
        assert!(s.visibility == 1 && warn.is_empty(), "{warn:?}");
        // camera collision: on by default, a plain on / off
        assert!(Settings::default().camera_collision && Settings::default().to_ini().contains("camera_collision = on"));
        let (s, warn) = Settings::parse("[graphics]\ncamera_collision = off\n");
        assert!(!s.camera_collision && warn.is_empty() && s.to_ini().contains("camera_collision = off"));
    }

    #[test]
    fn crt_settings() {
        // off by default, the game's 480 lines, F11 to compare
        let d = Settings::default();
        assert!(!d.crt.active && d.crt.pixel_size == 0 && d.crt.hold_key == 1);
        assert!(d.to_ini().contains("\n[crt]\n") && d.to_ini().contains("\nactive = off\n") &&d.to_ini().contains("\nhold_key = f11\n") && d.to_ini().contains("\npixel_size = 480\n"));
        // every option, lists and parameters (case kept, names with spaces and colons)
        let text = "[crt]\nactive = on\nshader = handheld/color-mod/PSP-P3(pure-gamma2.2)\npixel_size = x2.25\naspect_ratio = custom\ncustom_aspect_ratio = 1.25\nscale = 150\nflip = both\norientation = vertical\nhold_key = scroll_lock\nfavorites = crt/crt-geom | crt/crt-royale\nrecent = crt/crt-royale\n[crt.params]\ncrt/crt-geom:CRTgamma = 2.2\ncrt/crt-geom:R = 1.5\ncrt/crt-geom:r = 3\na b/c: d:X = -0.5\n";
        let (s, warn) = Settings::parse(text);
        assert!(warn.is_empty(), "{warn:?}");
        let c = &s.crt;
        assert!(c.active && c.flip_h && c.flip_v && c.vertical);
        assert_eq!(c.preset, "handheld/color-mod/PSP-P3(pure-gamma2.2)");
        assert_eq!(sr_crt::config::PIXEL_SIZES[c.pixel_size].2, "x2.25");
        assert_eq!((c.aspect, c.custom_aspect, c.aspect_ratio()), (sr_crt::config::CUSTOM_ASPECT, 1.25, 1.25));
        assert_eq!(c.output_scale(), 1.5);
        assert_eq!(c.hold_scancode(), 0x46);
        assert_eq!(s.crt_favorites, ["crt/crt-geom", "crt/crt-royale"]);
        assert_eq!(s.crt_params["crt/crt-geom"], [("CRTgamma".to_string(), 2.2), ("R".to_string(), 1.5), ("r".to_string(), 3.0)]);
        assert_eq!(s.crt_params["a b/c: d"], [("X".to_string(), -0.5)]);
        let (back, warn) = Settings::parse(&s.to_ini());
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(back.to_ini(), s.to_ini());
        // the renderer gets the chosen preset's parameters; a profile holds only those
        let mut s = back;
        s.crt.preset = "crt/crt-geom".into();
        assert_eq!(s.crt_live().params.len(), 3);
        assert!(!s.crt_ini(false).contains("a b/c: d:X") && s.crt_ini(true).contains("a b/c: d:X = -0.5"));
        // a ratio given as a number, invalid values reported
        let (s, warn) = Settings::parse("[crt]\naspect_ratio = 0.94\nscale = 33\nhold_key = f13\n");
        assert_eq!((sr_crt::config::ASPECT_RATIOS[s.crt.aspect].2, warn.len()), ("0.94", 2));
    }
}
