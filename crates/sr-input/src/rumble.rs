//! Controller vibration. The 1997 game had no force feedback: the runtime reads the
//! player's car once per simulation step (`sr_runtime::rumble`), describes it as a
//! [`Step`], and [`Synth`] turns the steps into the two motors' levels:
//!
//! * the low-frequency (left, heavy) motor: impacts, landings and hard bumps, the road
//!   surface;
//! * the high-frequency (right, light) motor: the sharp part of impacts, the surface's
//!   texture, sliding tyres and the engine at the rev limit, all lightly.
//!
//! The levels are posted with [`post`]; the input thread ([`Output`]) scales them by the
//! setting ([`Level`]), sends them to the controller and drops them to 0 when no step came
//! for a while (menus, pause, the settings overlay, loading), when the game's window is
//! not in the foreground, and at exit ([`stop_now`]).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The vibration setting (`controls.vibration`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Off,
    Low,
    Medium,
    High,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Off, Level::Low, Level::Medium, Level::High];

    /// Multiplier on the motor levels (the effects are tuned at Medium).
    pub fn gain(self) -> f32 {
        match self {
            Level::Off => 0.0,
            Level::Low => 0.5,
            Level::Medium => 1.0,
            Level::High => 1.6,
        }
    }

    /// The settings file's word.
    pub fn key(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Low => "low",
            Level::Medium => "medium",
            Level::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|l| l.key().eq_ignore_ascii_case(s.trim()))
    }

    pub fn index(self) -> usize {
        Level::ALL.iter().position(|&l| l == self).unwrap_or(2)
    }
}

/// The player's car in one simulation step, as the game computed it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Step {
    /// Length of the step, seconds.
    pub dt: f32,
    /// Speed as a fraction of the car's top speed.
    pub speed: f32,
    /// Change of the horizontal velocity in this step, in g (normal driving stays under
    /// about 1.5 g; a wall hit is 10-25 g).
    pub accel_h: f32,
    /// Vertical acceleration felt, in g (1 when resting on the ground, 0 in free fall).
    pub accel_v: f32,
    /// Wheels touching the ground (0-4).
    pub wheels_down: u8,
    /// No wheel touched the ground in the previous step.
    pub was_airborne: bool,
    /// Downward speed before this step, in g x seconds (seconds of free fall).
    pub fall: f32,
    /// The game reports a contact with a wall or another car.
    pub contact: bool,
    /// The car was moved (race restart, reset to the track): no impact from this step.
    pub teleport: bool,
    /// Unevenness of the road under the wheels on the ground (the track's road kinds:
    /// 0 asphalt, 1-5 cobbles, gravel, dirt, grass, snow); mean over those wheels.
    pub roughness: f32,
    /// The game's suspension travel change this step, mean over the wheels (its units:
    /// travel is -30..30).
    pub suspension: f32,
    /// Wheels sliding beyond their grip (0-4).
    pub sliding: u8,
    /// Engine speed as a fraction of the maximum.
    pub rpm: f32,
}

/// Motor levels, 0..1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motors {
    /// Low-frequency (left, heavy) motor.
    pub low: f32,
    /// High-frequency (right, light) motor.
    pub high: f32,
}

/// Something the synthesizer reacted to in a step (for logs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// Collision: strength 0..1, the horizontal velocity change in g, game contact flag.
    Impact { strength: f32, g: f32, contact: bool },
    /// Landing after all four wheels left the ground: strength, fall (g x s).
    Landing { strength: f32, fall: f32 },
    /// Hard vertical bump on the ground: strength, vertical g.
    Bump { strength: f32, g: f32 },
}

// Tuning (at Medium). Thresholds come from a lap of Rally China (docs/re/rumble.md): in
// normal driving the horizontal velocity change stays below 1.5 g in 99 % of the steps
// (max 3.7 g) and the vertical below 2.7 g.
/// Horizontal g above which a step with a game contact flag is an impact.
const IMPACT_G_CONTACT: f32 = 2.0;
/// Without a contact flag (objects, unflagged hits).
const IMPACT_G_FREE: f32 = 4.0;
/// g over the threshold for full strength.
const IMPACT_G_RANGE: f32 = 14.0;
/// A new wall or car contact without a hard jolt (scraping along a wall, rubbing another
/// car): a light pulse.
const CONTACT_PULSE: f32 = 0.2;
/// Vertical g above which a step on the ground is a bump.
const BUMP_G: f32 = 3.0;
const BUMP_G_RANGE: f32 = 12.0;
/// Fall (g x s) below which a landing is not felt, and the range to full strength.
const LANDING_FALL: f32 = 0.12;
const LANDING_FALL_RANGE: f32 = 0.6;
/// Decay times, seconds.
const IMPACT_TAU: f32 = 0.10;
const IMPACT_HIGH_TAU: f32 = 0.05;
const THUMP_TAU: f32 = 0.14;
/// Surface rumble at full speed per unevenness unit (low motor), and its floor for any
/// uneven road.
const SURFACE_BASE: f32 = 0.06;
const SURFACE_PER_UNIT: f32 = 0.04;
const SURFACE_MAX: f32 = 0.32;
/// Light effects on the high-frequency motor.
const SLIDE_HIGH: f32 = 0.10;
const REV_LIMIT: f32 = 0.93;
const REV_HIGH: f32 = 0.06;

/// Effects with memory (pulses decaying over a few steps).
#[derive(Clone, Debug, Default)]
pub struct Synth {
    impact: f32,
    impact_high: f32,
    thump: f32,
    /// The game's contact flags in the previous step.
    contact: bool,
}

fn decay(v: f32, dt: f32, tau: f32) -> f32 {
    v * (-dt / tau).exp()
}

impl Synth {
    /// Forget running pulses (a new race, a jump in time).
    pub fn reset(&mut self) {
        *self = Synth::default();
    }

    /// One simulation step: the motor levels until the next step, and what it reacted to.
    pub fn step(&mut self, s: &Step) -> (Motors, Option<Event>) {
        let dt = s.dt.clamp(0.0, 0.25);
        self.impact = decay(self.impact, dt, IMPACT_TAU);
        self.impact_high = decay(self.impact_high, dt, IMPACT_HIGH_TAU);
        self.thump = decay(self.thump, dt, THUMP_TAU);
        let mut event = None;
        // collisions: a sudden change of the horizontal velocity, or a new contact with a
        // wall or a car (the game's flags) that hardly changes it
        let threshold = if s.contact { IMPACT_G_CONTACT } else { IMPACT_G_FREE };
        let new_contact = s.contact && !self.contact;
        self.contact = s.contact;
        if !s.teleport && s.accel_h > threshold {
            let strength = ((s.accel_h - threshold) / IMPACT_G_RANGE).min(1.0);
            self.impact = self.impact.max(0.25 + 0.75 * strength);
            self.impact_high = self.impact_high.max(0.2 + 0.6 * strength);
            event = Some(Event::Impact { strength, g: s.accel_h, contact: s.contact });
        } else if !s.teleport && new_contact {
            self.impact = self.impact.max(CONTACT_PULSE);
            self.impact_high = self.impact_high.max(CONTACT_PULSE);
            event = Some(Event::Impact { strength: 0.0, g: s.accel_h, contact: true });
        }
        // landings: the first wheel down after all four were in the air (the game's own
        // rule for its landing sound), by the speed of the fall
        if !s.teleport && s.was_airborne && s.wheels_down > 0 && s.fall > LANDING_FALL {
            let strength = ((s.fall - LANDING_FALL) / LANDING_FALL_RANGE).min(1.0);
            self.thump = self.thump.max(0.25 + 0.75 * strength);
            if event.is_none() {
                event = Some(Event::Landing { strength, fall: s.fall });
            }
        } else if !s.teleport && s.wheels_down > 0 && s.accel_v > BUMP_G {
            // a hard bump on the ground (also the second contact of a landing)
            let strength = ((s.accel_v - BUMP_G) / BUMP_G_RANGE).min(1.0);
            self.thump = self.thump.max(0.12 + 0.6 * strength);
            if event.is_none() {
                event = Some(Event::Bump { strength, g: s.accel_v });
            }
        }
        // the road: uneven surfaces rumble with speed (asphalt does not), textured by the
        // game's own suspension movement
        let surface = if s.wheels_down > 0 && s.roughness > 0.0 {
            let speed = ((s.speed - 0.05) / 0.6).clamp(0.0, 1.0);
            let texture = 0.75 + 0.25 * (s.suspension / 10.0).min(1.0);
            ((SURFACE_BASE + SURFACE_PER_UNIT * s.roughness).min(SURFACE_MAX)) * speed.sqrt() * texture * (s.wheels_down as f32 / 4.0)
        } else {
            0.0
        };
        // light: tyres sliding, the engine at the limiter
        let slide = if s.wheels_down > 0 { SLIDE_HIGH * s.sliding.min(4) as f32 / 4.0 } else { 0.0 };
        let revs = REV_HIGH * ((s.rpm - REV_LIMIT) / (1.0 - REV_LIMIT)).clamp(0.0, 1.0);
        let low = self.impact.max(self.thump) + surface;
        let high = self.impact_high.max(self.thump * 0.35) + surface * 0.6 + slide + revs;
        (Motors { low: low.clamp(0.0, 1.0), high: high.clamp(0.0, 1.0) }, event)
    }
}

// ---------------------------------------------------------------------------------------
// Between the game's thread and the input thread.

/// Levels posted longer ago than this are dropped (no simulation step is running: menus,
/// pause, the settings overlay, loading). Race steps come every 20-40 ms.
pub const TIMEOUT: Duration = Duration::from_millis(200);

static TARGET: Mutex<Option<(Motors, Instant)>> = Mutex::new(None);
static STOPPED: AtomicBool = AtomicBool::new(false);
/// Something other than 0 was sent to a controller.
static SENT: AtomicBool = AtomicBool::new(false);

/// `SR_RUMBLE_OUT=FILE` (diagnostics): every change sent to the controller, with the reason,
/// in milliseconds since the first line.
fn log_out(line: std::fmt::Arguments) {
    use std::io::Write;
    static LOG: Mutex<Option<(std::fs::File, Instant)>> = Mutex::new(None);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Some(f) = std::env::var_os("SR_RUMBLE_OUT").and_then(|p| std::fs::File::create(p).ok()) {
            *LOG.lock().unwrap() = Some((f, Instant::now()));
        }
    });
    if let Some((f, start)) = LOG.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{} {line}", start.elapsed().as_millis());
    }
}

/// The motor levels for now (unscaled, as tuned at Medium): called every simulation step.
pub fn post(m: Motors) {
    *TARGET.lock().unwrap() = Some((m, Instant::now()));
}

/// No more vibration from the game until the next [`post`] (leaving the race).
pub fn release() {
    *TARGET.lock().unwrap() = None;
}

/// The levels posted, if still current.
pub fn current(now: Instant) -> Option<Motors> {
    let t = *TARGET.lock().unwrap();
    t.filter(|(_, at)| now.saturating_duration_since(*at) <= TIMEOUT).map(|(m, _)| m)
}

/// Stop every controller's motors now and for good (the program is ending; callable from
/// any thread).
pub fn stop_now() {
    STOPPED.store(true, Ordering::Relaxed);
    release();
    // (a controller that never vibrated is left alone)
    if SENT.load(Ordering::Relaxed) {
        let results: Vec<u32> = (0..4).map(|slot| crate::xinput::set_vibration(slot, 0, 0)).collect();
        log_out(format_args!("exit: every controller low=0 high=0, results {results:?}"));
    } else {
        log_out(format_args!("exit: no controller vibrated"));
    }
}

/// Motor levels scaled by the setting, as the controller's 16-bit speeds.
pub fn scale(m: Motors, level: Level) -> (u16, u16) {
    let g = level.gain();
    let q = |x: f32| ((x * g).clamp(0.0, 1.0) * 65535.0).round() as u16;
    (q(m.low), q(m.high))
}

/// Is a window of this process in the foreground?
pub fn focused() -> bool {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let h = unsafe { GetForegroundWindow() };
    if h.is_null() {
        return false;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    pid == unsafe { GetCurrentProcessId() }
}

/// The input thread's side: what is sent to the controller.
pub struct Output {
    /// Vibration allowed at all (controller mode).
    pub enabled: bool,
    /// Last sent: slot, levels, reason.
    sent: Option<(u32, u16, u16, &'static str)>,
    focus: bool,
    focus_checked: Option<Instant>,
}

impl Output {
    pub fn new(enabled: bool) -> Output {
        Output { enabled, sent: None, focus: true, focus_checked: None }
    }

    /// The levels to send now and why (for the log).
    pub fn decide(&mut self, now: Instant, level: Level) -> ((u16, u16), &'static str) {
        if STOPPED.load(Ordering::Relaxed) {
            return ((0, 0), "exit");
        }
        if !self.enabled {
            return ((0, 0), "keyboard mode");
        }
        if level == Level::Off {
            return ((0, 0), "off");
        }
        let Some(m) = current(now) else { return ((0, 0), "no race step") };
        // the foreground window, ten times a second
        if self.focus_checked.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(100)) {
            self.focus_checked = Some(now);
            self.focus = focused();
        }
        if !self.focus {
            return ((0, 0), "focus lost");
        }
        (scale(m, level), "race")
    }

    /// Send the levels to controller `slot` when they changed (`None`: no controller, or
    /// the scripted one: only logged).
    pub fn update(&mut self, now: Instant, slot: Option<u32>, level: Level) {
        let ((low, high), why) = self.decide(now, level);
        let key = (slot.unwrap_or(u32::MAX), low, high, why);
        if self.sent == Some(key) {
            return;
        }
        // only the reason changed: logged, nothing sent
        let same = self.sent.is_some_and(|(s, l, h, _)| (s, l, h) == (key.0, low, high));
        // a controller connected again: send what is current
        let result = match slot {
            _ if same => 0,
            Some(s) => {
                if low != 0 || high != 0 {
                    SENT.store(true, Ordering::Relaxed);
                }
                crate::xinput::set_vibration(s, low, high)
            }
            None => 0,
        };
        self.sent = Some(key);
        log_out(format_args!("slot={} low={low} high={high} level={} why={why} result={result}", slot.map_or("-".to_string(), |s| s.to_string()), level.key()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cruise() -> Step {
        Step { dt: 0.033, speed: 0.7, accel_h: 0.6, accel_v: 1.0, wheels_down: 4, rpm: 0.75, ..Default::default() }
    }

    fn run(s: &mut Synth, step: Step, n: usize) -> Vec<Motors> {
        (0..n).map(|_| s.step(&step).0).collect()
    }

    #[test]
    fn steady_driving_on_asphalt_is_still() {
        let mut s = Synth::default();
        for m in run(&mut s, cruise(), 100) {
            assert_eq!(m, Motors::default());
        }
        // normal cornering and braking (up to 1.5 g), small bumps (2.5 g), mid revs
        let mut st = cruise();
        st.accel_h = 1.5;
        st.accel_v = 2.5;
        st.rpm = 0.9;
        assert_eq!(s.step(&st), (Motors::default(), None));
    }

    #[test]
    fn rough_roads_rumble_with_speed_and_unevenness() {
        let mut s = Synth::default();
        let level = |rough: f32, speed: f32| {
            let mut st = cruise();
            st.roughness = rough;
            st.speed = speed;
            s.clone().step(&st).0.low
        };
        assert_eq!(level(0.0, 0.8), 0.0, "asphalt");
        assert_eq!(level(2.0, 0.0), 0.0, "standing still");
        assert!(level(1.0, 0.8) > 0.05 && level(1.0, 0.8) < 0.15, "{}", level(1.0, 0.8));
        assert!(level(3.0, 0.8) > level(1.0, 0.8));
        assert!(level(2.0, 0.8) > level(2.0, 0.3));
        assert!(level(5.0, 1.0) <= SURFACE_MAX + 1e-6, "subtle at most");
        // in the air: nothing
        let mut st = cruise();
        st.roughness = 2.0;
        st.wheels_down = 0;
        assert_eq!(s.step(&st).0, Motors::default());
    }

    #[test]
    fn impacts_are_short_pulses_by_strength() {
        let mut s = Synth::default();
        let mut hit = cruise();
        hit.accel_h = 23.0;
        hit.contact = true;
        let (m, e) = s.step(&hit);
        assert!(m.low > 0.9 && m.high > 0.7, "{m:?}");
        assert!(matches!(e, Some(Event::Impact { contact: true, .. })));
        // gone within about a quarter of a second
        let after = run(&mut s, cruise(), 8);
        assert!(after[0].low < m.low && after[7].low < 0.1, "{after:?}");
        // a light contact: felt, but much less
        let mut s = Synth::default();
        let mut touch = cruise();
        touch.accel_h = 3.0;
        touch.contact = true;
        let (m2, _) = s.step(&touch);
        assert!(m2.low > 0.2 && m2.low < 0.4, "{m2:?}");
        // the same jolt without a contact flag is driving (a bump, a landing slide)
        touch.contact = false;
        assert_eq!(Synth::default().step(&touch).1, None);
        // a new contact that hardly changes the velocity (rubbing, scraping): a light pulse,
        // once per contact
        let mut s = Synth::default();
        let mut rub = cruise();
        rub.contact = true;
        let (m3, e3) = s.step(&rub);
        assert!(matches!(e3, Some(Event::Impact { contact: true, .. })) && (m3.low - CONTACT_PULSE).abs() < 1e-6, "{m3:?}");
        assert_eq!(s.step(&rub).1, None, "still the same contact");
        // a car moved by the game (restart) is no impact
        hit.teleport = true;
        assert_eq!(Synth::default().step(&hit), (Motors::default(), None));
    }

    #[test]
    fn landings_by_fall_speed() {
        let land = |fall: f32| {
            let mut st = cruise();
            st.was_airborne = true;
            st.wheels_down = 1;
            st.fall = fall;
            Synth::default().step(&st)
        };
        assert_eq!(land(0.05).0, Motors::default(), "a hop");
        let (small, e) = land(0.25);
        let (big, _) = land(0.85);
        assert!(matches!(e, Some(Event::Landing { .. })));
        assert!(big.low > small.low && big.low > 0.9 && small.low > 0.25, "{small:?} {big:?}");
        // the thump decays
        let mut s = Synth::default();
        let mut st = cruise();
        st.was_airborne = true;
        st.wheels_down = 2;
        st.fall = 0.8;
        let first = s.step(&st).0.low;
        let later = run(&mut s, cruise(), 10);
        assert!(later[9].low < 0.15 * first, "{later:?}");
    }

    #[test]
    fn light_effects_stay_light() {
        let mut st = cruise();
        st.sliding = 4;
        st.rpm = 1.0;
        let (m, e) = Synth::default().step(&st);
        assert_eq!(m.low, 0.0);
        assert!(m.high > 0.1 && m.high <= SLIDE_HIGH + REV_HIGH + 1e-6, "{m:?}");
        assert_eq!(e, None);
    }

    #[test]
    fn levels_scale_and_parse() {
        let m = Motors { low: 0.5, high: 0.25 };
        assert_eq!(scale(m, Level::Off), (0, 0));
        let (lo, _) = scale(m, Level::Low);
        let (med, _) = scale(m, Level::Medium);
        let (hi, _) = scale(m, Level::High);
        assert!(lo < med && med < hi, "{lo} {med} {hi}");
        assert_eq!(med, 32768);
        assert_eq!(scale(Motors { low: 1.0, high: 1.0 }, Level::High), (65535, 65535));
        for l in Level::ALL {
            assert_eq!(Level::parse(l.key()), Some(l));
            assert_eq!(Level::ALL[l.index()], l);
        }
        assert_eq!(Level::parse("Medium"), Some(Level::Medium));
        assert_eq!(Level::parse("loud"), None);
    }

    #[test]
    fn output_needs_a_recent_step() {
        let mut out = Output { enabled: true, sent: None, focus: true, focus_checked: Some(Instant::now()) };
        post(Motors { low: 0.5, high: 0.5 });
        let now = Instant::now();
        // (the test process may have no foreground window: only the timing is checked here)
        assert_eq!(out.decide(now, Level::Off), ((0, 0), "off"));
        assert_eq!(out.decide(now, Level::Medium).1, "race");
        assert_eq!(out.decide(now + TIMEOUT + Duration::from_millis(1), Level::Medium), ((0, 0), "no race step"));
        out.enabled = false;
        assert_eq!(out.decide(now, Level::High), ((0, 0), "keyboard mode"));
        release();
        out.enabled = true;
        assert_eq!(out.decide(now, Level::High), ((0, 0), "no race step"));
    }
}
