//! Controller input.
//!
//! The original game reads an analog joystick on the PC gameport. This crate turns a
//! modern XInput controller (Xbox and compatible pads, most wheels in XInput mode) into
//! that joystick: four axes in the game's native range (-1023..1023) and four buttons.
//!
//! * axis 0: steering, axis 1: accelerate, axis 3: brake (axis 2 unused)
//! * button 1: gear up, button 2: gear down
//!
//! Each of those is mapped to a controller source (stick axis, half axis, trigger or
//! button), with dead zones, sensitivity and a response curve. The game's own dead zone
//! (10% of travel) can be compensated so the configured dead zone is the only one.
//!
//! The controller is polled on its own thread at 500 Hz; connecting and disconnecting is
//! detected at any time (hot-plug). Menu navigation uses the D-pad and face buttons, sent
//! to the game as the keys its menus expect.
//!
//! The controller's motors are driven from the player's car in races ([`rumble`]).

pub mod rumble;
pub mod script;
mod xinput;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Game units: full deflection.
pub const FULL: f32 = 1023.0;
/// The game ignores axis values below 66h.
pub const GAME_DEAD_ZONE: f32 = 102.0;

/// A controller input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Full stick axis (-1..1): LX, LY, RX, RY.
    Axis(Stick),
    /// One half of a stick axis (0..1): `LX+`, `LY-`, ...
    HalfAxis(Stick, bool),
    LeftTrigger,
    RightTrigger,
    Button(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stick {
    LX,
    LY,
    RX,
    RY,
}

pub const BUTTONS: &[(&str, u16)] = &[
    ("A", 0x1000),
    ("B", 0x2000),
    ("X", 0x4000),
    ("Y", 0x8000),
    ("LB", 0x0100),
    ("RB", 0x0200),
    ("BACK", 0x0020),
    ("START", 0x0010),
    ("LS", 0x0040),
    ("RS", 0x0080),
    ("UP", 0x0001),
    ("DOWN", 0x0002),
    ("LEFT", 0x0004),
    ("RIGHT", 0x0008),
];

impl Source {
    pub fn parse(s: &str) -> Option<Source> {
        let u = s.trim().to_ascii_uppercase();
        let stick = |n: &str| match n {
            "LX" => Some(Stick::LX),
            "LY" => Some(Stick::LY),
            "RX" => Some(Stick::RX),
            "RY" => Some(Stick::RY),
            _ => None,
        };
        if u == "LT" {
            return Some(Source::LeftTrigger);
        }
        if u == "RT" {
            return Some(Source::RightTrigger);
        }
        if let Some(st) = stick(&u) {
            return Some(Source::Axis(st));
        }
        if u.len() == 3 {
            if let Some(st) = stick(&u[..2]) {
                match &u[2..] {
                    "+" => return Some(Source::HalfAxis(st, true)),
                    "-" => return Some(Source::HalfAxis(st, false)),
                    _ => {}
                }
            }
        }
        BUTTONS.iter().find(|(n, _)| *n == u).map(|&(_, b)| Source::Button(b))
    }

    pub fn name(&self) -> String {
        let st = |s: &Stick| format!("{s:?}");
        match self {
            Source::Axis(s) => st(s),
            Source::HalfAxis(s, pos) => format!("{}{}", st(s), if *pos { '+' } else { '-' }),
            Source::LeftTrigger => "LT".into(),
            Source::RightTrigger => "RT".into(),
            Source::Button(b) => BUTTONS.iter().find(|(_, x)| x == b).map(|(n, _)| n.to_string()).unwrap_or_default(),
        }
    }
}

/// Parse a comma-separated list of sources ("RB,Y").
pub fn parse_sources(s: &str) -> Result<Vec<Source>, String> {
    s.split(',').filter(|x| !x.trim().is_empty()).map(|x| Source::parse(x).ok_or_else(|| format!("unknown controller input '{}'", x.trim()))).collect()
}

/// Controller settings (the `[controller]` section of the settings file).
#[derive(Clone, Debug)]
pub struct PadSettings {
    /// Fraction of stick travel ignored around the center (0..0.9).
    pub steer_dead_zone: f32,
    /// Steering sensitivity: input multiplier before the curve (0.1..4).
    pub steer_sensitivity: f32,
    /// Steering response exponent: 1 = linear (as a 1997 joystick), >1 = finer near center.
    pub steer_curve: f32,
    /// Fraction of trigger travel ignored (0..0.9).
    pub trigger_dead_zone: f32,
    /// Throttle / brake response exponent.
    pub pedal_curve: f32,
    /// Remove the game's own 10% dead zone so only the dead zones above apply.
    pub compensate_game_dead_zone: bool,
    pub steer: Vec<Source>,
    pub accelerate: Vec<Source>,
    pub brake: Vec<Source>,
    pub gear_up: Vec<Source>,
    pub gear_down: Vec<Source>,
    /// Next race camera (sent as CAMERA_KEY).
    pub camera: Vec<Source>,
    /// Rear-view mirror on / off (sent as MIRROR_KEY).
    pub mirror: Vec<Source>,
    /// Autopilot on / off (sent as AUTOPILOT_KEY).
    pub autopilot: Vec<Source>,
    /// Vibration strength (`controls.vibration`).
    pub vibration: rumble::Level,
}

impl Default for PadSettings {
    fn default() -> Self {
        PadSettings {
            steer_dead_zone: 0.08,
            steer_sensitivity: 1.0,
            steer_curve: 1.0,
            trigger_dead_zone: 0.04,
            pedal_curve: 1.0,
            compensate_game_dead_zone: true,
            steer: vec![Source::Axis(Stick::LX)],
            accelerate: vec![Source::RightTrigger],
            brake: vec![Source::LeftTrigger],
            gear_up: vec![Source::Button(0x0200)],
            gear_down: vec![Source::Button(0x0100)],
            camera: vec![Source::Button(0x8000), Source::Button(0x0080)],
            mirror: vec![Source::Button(0x4000)],
            autopilot: vec![Source::Button(0x0040)],
            vibration: rumble::Level::Medium,
        }
    }
}

/// Raw controller state (XInput layout).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadState {
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

impl PadState {
    fn stick(&self, s: Stick) -> f32 {
        let v = match s {
            Stick::LX => self.lx,
            Stick::LY => self.ly,
            Stick::RX => self.rx,
            Stick::RY => self.ry,
        };
        (v as f32 / 32767.0).clamp(-1.0, 1.0)
    }
}

fn dead_zone(x: f32, dz: f32) -> f32 {
    let a = x.abs();
    if a <= dz { 0.0 } else { ((a - dz) / (1.0 - dz)).min(1.0) * x.signum() }
}

/// Value of one source after dead zones: -1..1 for full axes, 0..1 for everything else.
fn source_value(p: &PadState, s: Source, cfg: &PadSettings) -> f32 {
    match s {
        Source::Axis(st) => dead_zone(p.stick(st), cfg.steer_dead_zone),
        Source::HalfAxis(st, pos) => {
            let v = dead_zone(p.stick(st), cfg.steer_dead_zone);
            if pos { v.max(0.0) } else { (-v).max(0.0) }
        }
        Source::LeftTrigger => dead_zone(p.lt as f32 / 255.0, cfg.trigger_dead_zone),
        Source::RightTrigger => dead_zone(p.rt as f32 / 255.0, cfg.trigger_dead_zone),
        Source::Button(b) => (p.buttons & b != 0) as u8 as f32,
    }
}

/// Strongest of several sources (by magnitude).
fn strongest(p: &PadState, list: &[Source], cfg: &PadSettings) -> f32 {
    list.iter().map(|&s| source_value(p, s, cfg)).fold(0.0f32, |a, v| if v.abs() > a.abs() { v } else { a })
}

/// -1..1 to game units, optionally lifting non-zero input past the game's dead zone.
fn to_game(x: f32, compensate: bool) -> i32 {
    if x == 0.0 {
        return 0;
    }
    let m = x.abs().min(1.0);
    let v = if compensate { GAME_DEAD_ZONE + (FULL - GAME_DEAD_ZONE) * m } else { FULL * m };
    (v.round() as i32) * x.signum() as i32
}

/// Keyboard keys merged into the joystick (controller mode), as base scancodes.
#[derive(Clone, Copy, Debug)]
pub struct KeyboardKeys {
    pub accelerate: u8,
    pub brake: u8,
    pub left: u8,
    pub right: u8,
    pub gear_up: u8,
    pub gear_down: u8,
}

impl Default for KeyboardKeys {
    fn default() -> Self {
        // the game's defaults (CONFIG\DEFCTRL.CFG)
        KeyboardKeys { accelerate: 0x48, brake: 0x50, left: 0x4b, right: 0x4d, gear_up: 0x36, gear_down: 0x1d }
    }
}

/// Joystick output: axes in game units, button bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Joystick {
    pub axes: [i32; 4],
    pub buttons: u32,
}

/// Map a controller state (and the merged keyboard keys) to the virtual joystick.
pub fn map(p: &PadState, cfg: &PadSettings, keys: Option<(&KeyboardKeys, &[bool; 128])>) -> Joystick {
    let mut steer = strongest(p, &cfg.steer, cfg);
    if steer != 0.0 {
        let m = (steer.abs() * cfg.steer_sensitivity).min(1.0).powf(cfg.steer_curve);
        steer = m * steer.signum();
    }
    let pedal = |list: &[Source]| strongest(p, list, cfg).abs().min(1.0).powf(cfg.pedal_curve);
    let mut accel = pedal(&cfg.accelerate);
    let mut brake = pedal(&cfg.brake);
    let mut gear_up = strongest(p, &cfg.gear_up, cfg) >= 0.5;
    let mut gear_down = strongest(p, &cfg.gear_down, cfg) >= 0.5;
    if let Some((k, down)) = keys {
        let d = |sc: u8| down[(sc & 0x7f) as usize];
        let kb_steer = d(k.right) as i32 - d(k.left) as i32;
        if kb_steer != 0 {
            steer = kb_steer as f32;
        }
        if d(k.accelerate) {
            accel = 1.0;
        }
        if d(k.brake) {
            brake = 1.0;
        }
        gear_up |= d(k.gear_up);
        gear_down |= d(k.gear_down);
    }
    let c = cfg.compensate_game_dead_zone;
    Joystick { axes: [to_game(steer, c), to_game(accel, c), 0, to_game(brake, c)], buttons: gear_up as u32 | (gear_down as u32) << 1 }
}

/// Controller buttons that act as keys (menus, pause), sent as PC scancodes.
/// (button, make code, extended)
pub const MENU_KEYS: &[(u16, u8, bool)] = &[
    (0x0001, 0x48, true),  // D-pad up    -> Up
    (0x0002, 0x50, true),  // D-pad down  -> Down
    (0x0004, 0x4b, true),  // D-pad left  -> Left
    (0x0008, 0x4d, true),  // D-pad right -> Right
    (0x1000, 0x1c, false), // A           -> Enter
    (0x2000, 0x01, false), // B           -> Esc
    (0x0010, 0x01, false), // Start       -> Esc (pause)
    (0x0020, 0x58, false), // Back / View -> F12 (settings overlay)
    (0x0100, 0x49, true),  // LB          -> Page Up (overlay sections)
    (0x0200, 0x51, true),  // RB          -> Page Down
];

/// Pseudo scancode sent to the key sink when the controller's camera input is pressed
/// (no PC key uses it; the application turns it into the game's view keys).
pub const CAMERA_KEY: u8 = 0x7e;
/// Pseudo scancode for the controller's mirror input (the application sends M).
pub const MIRROR_KEY: u8 = 0x7d;
/// Pseudo scancode for the controller's autopilot input.
pub const AUTOPILOT_KEY: u8 = 0x7c;

pub type JoystickSink = Box<dyn Fn(Joystick, bool) + Send>;
pub type KeySink = Box<dyn Fn(u8) + Send>;
pub type LogSink = Box<dyn Fn(&str) + Send>;

/// Shared keyboard state (fed by the window) for the keyboard merge.
pub struct KeyboardState {
    down: Mutex<[bool; 128]>,
    e0: AtomicU32,
}

impl Default for KeyboardState {
    fn default() -> Self {
        KeyboardState { down: Mutex::new([false; 128]), e0: AtomicU32::new(0) }
    }
}

impl KeyboardState {
    /// Track a PC scancode byte (set 1, with E0 prefixes).
    pub fn scancode(&self, sc: u8) {
        if sc == 0xe0 {
            self.e0.store(1, Ordering::Relaxed);
            return;
        }
        self.e0.store(0, Ordering::Relaxed);
        self.down.lock().unwrap()[(sc & 0x7f) as usize] = sc & 0x80 == 0;
    }

    /// Which make codes are held.
    pub fn snapshot(&self) -> [bool; 128] {
        *self.down.lock().unwrap()
    }
}

pub struct InputConfig {
    /// Controller mapping and tuning; may be changed while running (settings overlay).
    pub pad: Arc<Mutex<PadSettings>>,
    /// Merge these keyboard keys into the joystick (controller mode).
    pub keyboard: Option<KeyboardKeys>,
    /// Send D-pad / face buttons as menu keys.
    pub menu_keys: bool,
    /// Scripted controller instead of XInput (tests): see [`script`].
    pub script: Option<script::Script>,
    /// Vibration allowed (controller mode; a keyboard player's pad stays still).
    pub rumble: bool,
}

pub struct Input {
    pub keyboard: Arc<KeyboardState>,
}

/// First connected controller slot, if any.
pub fn connected_controller() -> Option<u32> {
    (0..4).find(|&i| xinput::get(i).is_some())
}

/// Start the polling thread.
pub fn start(cfg: InputConfig, joy: JoystickSink, keys: KeySink, log: LogSink) -> Input {
    let keyboard = Arc::new(KeyboardState::default());
    let kb = keyboard.clone();
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || run(cfg, kb, joy, keys, log))
        .expect("input thread");
    Input { keyboard }
}

fn run(cfg: InputConfig, kb: Arc<KeyboardState>, joy: JoystickSink, keys: KeySink, log: LogSink) {
    const PERIOD: Duration = Duration::from_millis(2);
    let timer = xinput::Timer::new();
    let start = Instant::now();
    let mut slot: Option<u32> = None;
    let mut last_scan = None::<Instant>;
    let mut menu_down = [false; 128];
    let mut camera_down = false;
    let mut mirror_down = false;
    let mut autopilot_down = false;
    let mut last = None;
    let mut rumble = rumble::Output::new(cfg.rumble);
    loop {
        let now = Instant::now();
        let state = if let Some(sc) = &cfg.script {
            Some(sc.state_at(now.duration_since(start).as_secs_f64()))
        } else {
            if let Some(s) = slot {
                if xinput::get(s).is_none() {
                    log(&format!("controller {} disconnected", s + 1));
                    slot = None;
                }
            }
            // XInput is slow on empty slots: look for a controller once a second
            if slot.is_none() && last_scan.is_none_or(|t: Instant| now.duration_since(t) >= Duration::from_secs(1)) {
                last_scan = Some(now);
                if let Some(s) = connected_controller() {
                    log(&format!("controller {} connected", s + 1));
                    slot = Some(s);
                }
            }
            slot.and_then(xinput::get)
        };
        let connected = state.is_some();
        let p = state.unwrap_or_default();
        let kbs = kb.snapshot();
        let pad = cfg.pad.lock().unwrap().clone();
        let mut j = map(&p, &pad, cfg.keyboard.as_ref().map(|k| (k, &kbs)));
        if !connected && cfg.keyboard.is_some() {
            // keyboard-only driving through the joystick still works without a controller
            j = map(&PadState::default(), &pad, cfg.keyboard.as_ref().map(|k| (k, &kbs)));
        }
        // the motors (the scripted controller has none: only logged)
        rumble.update(now, if cfg.script.is_some() { None } else { slot }, pad.vibration);
        let out = (j, connected || cfg.keyboard.is_some());
        if last != Some(out) {
            joy(out.0, out.1);
            last = Some(out);
        }
        let camera = strongest(&p, &pad.camera, &pad) >= 0.5;
        if camera && !camera_down {
            keys(CAMERA_KEY);
        }
        camera_down = camera;
        let mirror = strongest(&p, &pad.mirror, &pad) >= 0.5;
        if mirror && !mirror_down {
            keys(MIRROR_KEY);
        }
        mirror_down = mirror;
        let autopilot = strongest(&p, &pad.autopilot, &pad) >= 0.5;
        if autopilot && !autopilot_down {
            keys(AUTOPILOT_KEY);
        }
        autopilot_down = autopilot;
        if cfg.menu_keys {
            for &(_, code, ext) in MENU_KEYS {
                // several buttons may share a key: it is down while any of them is
                let is = MENU_KEYS.iter().any(|&(b, c, _)| c == code && p.buttons & b != 0);
                if is != menu_down[code as usize] {
                    menu_down[code as usize] = is;
                    let sc = if is { code } else { code | 0x80 };
                    if ext {
                        keys(0xe0);
                        kb.scancode(0xe0);
                    }
                    keys(sc);
                    kb.scancode(sc);
                }
            }
        }
        timer.wait(PERIOD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sources() {
        assert_eq!(Source::parse("lx"), Some(Source::Axis(Stick::LX)));
        assert_eq!(Source::parse("RY-"), Some(Source::HalfAxis(Stick::RY, false)));
        assert_eq!(Source::parse("RT"), Some(Source::RightTrigger));
        assert_eq!(Source::parse("rb"), Some(Source::Button(0x0200)));
        assert_eq!(Source::parse("Q"), None);
        for &(n, b) in BUTTONS {
            assert_eq!(Source::Button(b).name(), n);
        }
    }

    #[test]
    fn steering_mapping() {
        let cfg = PadSettings::default();
        let at = |lx: i16| map(&PadState { lx, ..Default::default() }, &cfg, None).axes[0];
        assert_eq!(at(0), 0);
        assert_eq!(at(1000), 0); // inside the 8% dead zone
        assert_eq!(at(32767), 1023);
        assert_eq!(at(-32768), -1023);
        // just past the dead zone: lifted past the game's own dead zone
        let v = at(2700);
        assert!(v >= 102 && v < 110, "{v}");
        // monotonic
        let mut prev = -2000;
        for x in (-32767..=32767).step_by(257) {
            let v = at(x as i16);
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn pedals_and_buttons() {
        let cfg = PadSettings::default();
        let j = map(&PadState { rt: 255, lt: 128, buttons: 0x0200, ..Default::default() }, &cfg, None);
        assert_eq!(j.axes[1], 1023);
        assert!(j.axes[3] > 500 && j.axes[3] < 600, "{}", j.axes[3]);
        assert_eq!(j.buttons, 1);
        // LB shifts down; X (the mirror) and Y (the camera) are not gear buttons
        let j = map(&PadState { buttons: 0x0100, ..Default::default() }, &cfg, None);
        assert_eq!(j.buttons, 2);
        assert_eq!(j.axes, [0; 4]);
        let j = map(&PadState { buttons: 0x4000 | 0x8000, ..Default::default() }, &cfg, None);
        assert_eq!(j.buttons, 0);
    }

    #[test]
    fn no_compensation_is_linear() {
        let cfg = PadSettings { compensate_game_dead_zone: false, steer_dead_zone: 0.0, ..Default::default() };
        let j = map(&PadState { lx: 16384, ..Default::default() }, &cfg, None);
        assert_eq!(j.axes[0], 512);
    }

    #[test]
    fn keyboard_merge() {
        let cfg = PadSettings::default();
        let k = KeyboardKeys::default();
        let mut down = [false; 128];
        down[0x4b] = true; // left
        down[0x48] = true; // up
        let j = map(&PadState { lx: 20000, ..Default::default() }, &cfg, Some((&k, &down)));
        assert_eq!(j.axes[0], -1023);
        assert_eq!(j.axes[1], 1023);
    }
}
