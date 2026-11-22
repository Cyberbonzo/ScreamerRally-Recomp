//! Native renderer, the menus' 3D scenes (wave 4 topic W4-2, `docs/re/native/menus.md`).
//!
//! The menus draw their 3D scenes through the engine's own entries, with the camera
//! structure at `144fb0` (`82338` sets it up) and the instance list `12f478` (the menus
//! have no scene table: the list's instances are the menu's objects):
//!
//! * `78d10(eax = list)`: the objects only, no sky and no cells, over the menu's picture
//!   (the name entry's letters, the car and transmission selection's turntable);
//! * `78d84(eax = list)`: sky, cells and objects like a race view (the main menu: its
//!   background picture as the sky, the world `map1`, the cabinet and the trophies).
//!
//! With the native renderer on, a menu's 3D scene is drawn natively: at its draw call the
//! menu's scene is built from the loaded models and textures (`extract::build_menu_scene`,
//! again whenever the menu loads other ones) and uploaded; `78d84` then goes through the
//! race's hooks (sky skipped, the native view drawn after the cell set-up, the game's
//! objects skipped), `78d10` draws its native view after its set-up (observer at `78d56`)
//! and the game's objects are skipped (`native::dynamics`). The menus' 2D (pictures, texts,
//! boxes, fades) stays on the Glide path.
//!
//! **Extra frames** (interpolation on): the menu logic keeps the game's 50 steps per
//! second (the menu frame timer, `crate::pacing`); everything a menu frame draws is
//! recorded (the Glide layer's HUD recording, from the frame's start) with the place of
//! each native view in it, and the frame's present is replaced by frames drawn between the
//! last two steps until the next one is due: the recorded 2D replayed, each native view
//! drawn again with the camera and the objects interpolated in floating point. No guest
//! code runs for them (unlike the races' extra frames, `crate::interp`).

use std::time::{Duration, Instant};

use super::frame::{self, GameCamera, ViewMap};
use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

// ---------------------------------------------------------------------------------------
// Link addresses

/// The engine's object drawing without sky and cells (`78d10(eax = instance list)`).
const DRAW_OBJECTS: u32 = 0x78d10;
/// In `78d10`, after its set-up (`7a158`, the view constants): the native view is drawn
/// here (`mov edi, [147394]`).
const OBJECTS_SET_UP: u32 = 0x78d56;
/// The engine's world drawing (`78d84(eax = instance list)`): sky, cells, objects.
const DRAW_WORLD: u32 = 0x78d84;
/// Return address of the race's call of `78d84` (`349b8`, 34a8d): race views.
const RACE_DRAW_RETURN: u32 = 0x34a92;
/// End of the menu frame timer's tick wait (4f6ec: call 504dc): the menu frame starts.
const MENU_FRAME_START: u32 = 0x4f6f1;
const CAMERA_PTR: u32 = 0x81e0c;
const MODELS: u32 = 0x147390;
const ROWS: u32 = 0x123720;
const SCENE: u32 = 0x12f860;

/// Steps further apart than this are not consecutive (a loading pause).
const MAX_STEP_GAP: f64 = 0.1;
/// A larger camera move or turn in one step is a cut (shown, not swept through): units,
/// angle units (2048 per turn; 140 = 25 degrees).
const MAX_CAMERA_MOVE: f64 = 3000.0;
const MAX_CAMERA_TURN: f64 = 140.0;

fn rd32(a: u32) -> u32 {
    unsafe { mem::rd32(a) }
}

// ---------------------------------------------------------------------------------------
// State

/// A native view drawn in a menu frame, for its extra frames.
#[derive(Clone, Copy, Debug)]
struct ViewMark {
    /// Recorded ops before it (the Glide layer's recording).
    ops: usize,
    /// `78d84` (sky, cells, objects); else `78d10` (objects).
    world: bool,
    cam: GameCamera,
}

#[derive(Clone, Debug)]
struct Step {
    at: Instant,
    frame: u64,
    views: Vec<ViewMark>,
}

/// The menus' scenes and extra frames (`NativeState::menus`).
#[derive(Default)]
pub struct MenuState {
    /// The menu scene built (or tried) last: its key, and whether the build failed.
    key: Option<u64>,
    failed: bool,
    /// This frame is recorded (from its start) for extra frames.
    recording: bool,
    /// When this frame started (the menu frame timer's tick wait ended).
    frame_start: Option<Instant>,
    /// This frame's native views.
    views: Vec<ViewMark>,
    prev: Option<Step>,
    cur: Option<Step>,
    /// Running average of an extra frame's drawing time, s.
    draw_avg: f64,
    /// The next frame's time under the frame rate limit.
    next_frame: Option<Instant>,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    since: Option<Instant>,
    game_frames: u64,
    extra: u64,
    shown: u64,
    draw_s: f64,
    dropped: u64,
    longest_gap: f64,
    last_present: Option<Instant>,
}

fn probe_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SR_MENU_PROBE").is_some())
}

// ---------------------------------------------------------------------------------------
// Hooks

pub fn install(m: &mut Machine) {
    let r = (|| -> Result<(), String> {
        m.install_observer("menu objects (native renderer)", DRAW_OBJECTS, |m, f, tramp| {
            if probe_on() {
                calls(m, f, 0);
                probe(m, f);
            }
            begin_draw(m, f, false);
            HookResult::Resume(tramp)
        })?;
        m.install_observer("menu objects view (native renderer)", OBJECTS_SET_UP, |m, _f, tramp| {
            if m.native.menu && frame::drawing(m) {
                frame::draw_objects_view(m);
            }
            HookResult::Resume(tramp)
        })?;
        m.install_observer("menu world (native renderer)", DRAW_WORLD, |m, f, tramp| {
            if m.img.to_link(f.stack(0)) != Some(RACE_DRAW_RETURN) {
                if probe_on() {
                    calls(m, f, 1);
                    probe(m, f);
                }
                begin_draw(m, f, true);
            } else {
                // a race view (349b8), also under the menus' timer (the pause screen): never
                // with a menu's scene
                m.native.menu_drawing = false;
            }
            HookResult::Resume(tramp)
        })?;
        m.install_observer("menu frame start (native renderer)", MENU_FRAME_START, |m, _f, tramp| {
            frame_start(m);
            HookResult::Resume(tramp)
        })?;
        if probe_on() {
            m.install_observer("view camera (probe)", 0x82338, |m, f, tramp| {
                calls(m, f, 2);
                HookResult::Resume(tramp)
            })?;
            m.install_observer("draw world (probe)", 0x349b8, |m, f, tramp| {
                calls(m, f, 3);
                HookResult::Resume(tramp)
            })?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        crate::warn!("native renderer: the menus' 3D scenes stay on the Glide path ({e})");
    }
}

/// A menu draws a 3D scene (`78d10` / `78d84` from menu code; race frames are the race's):
/// make sure its scene is on the GPU, and draw this call natively.
fn begin_draw(m: &mut Machine, _f: &HookFrame, world: bool) {
    if m.native.in_race {
        return;
    }
    m.native.menu_drawing = false;
    if !frame::requested() || m.glide.is_none() || m.native.unavailable {
        return;
    }
    let key = scene_key(m, world);
    let s = &m.native.menus;
    if m.native.menu && m.native.active && s.key == Some(key) {
        m.native.menu_drawing = true;
        return;
    }
    if s.key == Some(key) && s.failed {
        return;
    }
    // a new menu scene (or the first after a race): what was on the GPU is replaced; the
    // next race builds its own again
    m.native.menus.key = Some(key);
    m.native.menus.failed = false;
    m.native.menus.prev = None;
    m.native.menus.cur = None;
    m.native.active = false;
    m.native.menu = false;
    m.native.dynamics = None;
    m.native.key = (0, 0);
    m.native.tried = false;
    let t0 = Instant::now();
    let ok = match super::extract::build_menu_scene(m, world) {
        Some(b) => frame::activate(m, b, t0.elapsed().as_secs_f64() * 1000.0, true),
        None => {
            crate::info!("native renderer: no scene for this menu (its models cannot be read); the Glide path draws it");
            false
        }
    };
    m.native.menus.failed = !ok;
    m.native.menu_drawing = ok;
    crate::debug!(
        "native menus: scene for game frame {} ready after {:.1} ms",
        m.glide.as_ref().map_or(0, |g| g.frames),
        t0.elapsed().as_secs_f64() * 1000.0
    );
}

/// What identifies a menu's scene: the model table and its models, the loaded libraries,
/// with a world its grid and its background picture.
fn scene_key(m: &Machine, world: bool) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut add = |v: u32| h = (h ^ v as u64).wrapping_mul(0x0100_0000_01b3);
    let link = |a: u32| m.img.link(a);
    add(world as u32);
    let table = rd32(link(MODELS));
    add(table);
    for i in 0..4096u32 {
        let a = table.wrapping_add(4 * i);
        if !mem::readable(a, 4) {
            break;
        }
        let p = rd32(a);
        add(p);
        if p == u32::MAX {
            break;
        }
    }
    for lib in super::textures::libraries() {
        add(lib.slot);
        add(lib.data);
        for b in lib.name.bytes() {
            add(b as u32);
        }
    }
    if world {
        add(rd32(link(ROWS)));
        // the background picture (in texture memory): its format, address and some texels
        for k in 0..5 {
            add(rd32(link(super::textures::SKY_INFO) + 4 * k));
        }
        let start = rd32(link(super::textures::SKY_TMU)) as usize;
        if let Some(ram) = m.glide.as_ref().and_then(|g| g.tmu.ram.get(start..start + 0x20000)) {
            for i in (0..ram.len()).step_by(509) {
                add(ram[i] as u32);
            }
        }
    }
    h
}

/// A menu's view rectangle: the game's own 640 columns; a view the game does not widen
/// (`display.wide_menus` off) in a wide window keeps to them (`View::viewport` widens a
/// full-width rectangle), as the Glide path draws it (its x table ends at 639.8).
/// `ext`: the window's widescreen extension (game pixels on each side).
pub(super) fn viewport(map: &ViewMap, vp: [f32; 4], ext: f32) -> [f32; 4] {
    if map.width <= 640.0 && ext > 0.0 && vp[0] <= 0.0 && vp[2] >= 640.0 { [vp[0], vp[1], 639.8, vp[3]] } else { vp }
}

/// The menu frame starts (its tick wait ended): with a menu scene native and interpolation
/// on, everything the frame draws is recorded for its extra frames.
fn frame_start(m: &mut Machine) {
    let s = &mut m.native.menus;
    if s.frame_start.is_none() {
        s.frame_start = Some(Instant::now());
    }
    let on = crate::interp::enabled() && frame::requested() && m.native.menu && m.native.active;
    if on && !m.native.menus.recording {
        if let Some(g) = m.glide.as_mut() {
            // what is batched belongs to the frame before
            g.flush();
            g.hud_recording = true;
            m.native.menus.recording = true;
        }
    }
}

/// A menu's native view was drawn in the game's frame (`frame::draw_view`).
pub(super) fn view_drawn(m: &mut Machine, world: bool, cam: GameCamera) {
    if !m.native.menus.recording {
        return;
    }
    let Some(ops) = m.glide.as_ref().map(|g| g.recorded_ops()) else { return };
    m.native.menus.views.push(ViewMark { ops, world, cam });
}

/// At the game's buffer swap (before it is carried out): show extra frames instead of this
/// frame (`fill` after the swap)? Otherwise this frame's bookkeeping ends here.
pub fn extra_frames(m: &mut Machine) -> bool {
    let s = &m.native.menus;
    let due = s.recording && !s.views.is_empty() && crate::interp::enabled() && m.native.menu && m.native.active;
    if probe_on() {
        crate::info!(
            "menu probe swap at game frame {}: recording {}, views {}, frame start {:?}, due {due}",
            m.glide.as_ref().map_or(0, |g| g.frames),
            s.recording,
            s.views.len(),
            s.frame_start.map(|t| t.elapsed())
        );
    }
    if !due {
        let s = &mut m.native.menus;
        s.recording = false;
        s.views.clear();
        s.frame_start = None;
        if s.cur.is_some() {
            s.prev = None;
            s.cur = None;
        }
        // presented by the game's swap: the statistics' gaps are between this module's
        // presents only
        s.stats.last_present = None;
    }
    due
}

fn camera_cut(a: &GameCamera, b: &GameCamera) -> bool {
    let moved = (0..3).map(|k| (b.pos[k] - a.pos[k]).powi(2)).sum::<f64>().sqrt();
    let turned = (0..3).any(|k| {
        let d = (b.angles[k] - a.angles[k]).rem_euclid(2048.0);
        d.min(2048.0 - d) > MAX_CAMERA_TURN
    });
    moved > MAX_CAMERA_MOVE || turned
}

/// Two steps whose views can be drawn in between (`timed`: the wall time between them
/// counts; not with a fixed number of extra frames, whose shots take their time).
fn consecutive(a: &Step, b: &Step, timed: bool) -> bool {
    b.frame == a.frame + 1
        && (!timed || (b.at - a.at).as_secs_f64() <= MAX_STEP_GAP)
        && a.views.len() == b.views.len()
        && a.views.iter().zip(&b.views).all(|(x, y)| x.world == y.world && !camera_cut(&x.cam, &y.cam))
}

/// After the game's swap (its present suppressed): frames between the previous and this
/// step until the menu's next frame is due (as `crate::interp::fill` does for races).
pub fn fill(m: &mut Machine) {
    let now = Instant::now();
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    let s = &mut m.native.menus;
    let step = Step { at: s.frame_start.take().unwrap_or(now), frame, views: std::mem::take(&mut s.views) };
    s.recording = false;
    s.prev = s.cur.take();
    s.cur = Some(step);
    s.stats.game_frames += 1;
    let (Some(a), Some(b)) = (s.prev.clone(), s.cur.clone()) else { return present_game(m) };
    // SR_INTERP_STEPS=n (diagnostics, as for races): n frames per step at 1/n .. n/n
    static STEPS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let steps = *STEPS.get_or_init(|| std::env::var("SR_INTERP_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
    if !consecutive(&a, &b, steps == 0) {
        if probe_on() {
            crate::info!(
                "menu probe: game frame {} not interpolated: frames {} / {}, {:.1} ms apart, views {:?} / {:?}",
                b.frame,
                a.frame,
                b.frame,
                (b.at - a.at).as_secs_f64() * 1000.0,
                a.views.iter().map(|v| (v.world, v.cam)).collect::<Vec<_>>(),
                b.views.iter().map(|v| (v.world, v.cam)).collect::<Vec<_>>()
            );
        }
        return present_game(m);
    }
    let period = (b.at - a.at).as_secs_f64().clamp(0.010, 0.050);
    // the menus' frames are paced to 2 ticks of the 100 Hz timer (crate::pacing)
    let deadline = b.at + Duration::from_secs_f64(crate::pacing::MENU_TICKS as f64 * 0.01 - 0.0015);
    if steps > 0 {
        for k in 1..=steps {
            extra_frame(m, &a, &b, k as f32 / steps as f32);
            present(m);
        }
        return;
    }
    let cap = crate::interp::fps_cap();
    let cap_period = (cap > 0).then(|| Duration::from_secs_f64(1.0 / cap as f64));
    if cap_period.is_none() {
        m.native.menus.next_frame = None;
    }
    let mut shown = false;
    loop {
        let t0 = Instant::now();
        // without a limit at least one frame per step, however late
        if t0 >= deadline && (shown || cap_period.is_some()) {
            break;
        }
        let left = deadline.saturating_duration_since(t0);
        if cap_period.is_some() {
            let next = *m.native.menus.next_frame.get_or_insert(t0);
            if next > t0 {
                if next >= deadline {
                    break;
                }
                std::thread::sleep(next - t0);
                continue;
            }
        }
        let draw_est = Duration::from_secs_f64(m.native.menus.draw_avg.max(0.0005));
        if shown && draw_est >= left {
            break;
        }
        // shown one step late: between the previous step and this one, at the time it is
        // expected on screen
        let shown_at = Instant::now() + draw_est;
        let alpha = ((shown_at - b.at).as_secs_f64() / period).clamp(0.0, 1.0) as f32;
        extra_frame(m, &a, &b, alpha);
        let w0 = Instant::now();
        let left = deadline.saturating_duration_since(w0);
        let ready = m.glide.as_mut().is_none_or(|g| g.backend.wait_ready(left));
        if !ready {
            // the display did not take a frame before the game's next one: dropped
            m.native.menus.stats.dropped += 1;
            break;
        }
        present(m);
        shown = true;
        if let (Some(p), Some(next)) = (cap_period, m.native.menus.next_frame) {
            let now = Instant::now();
            m.native.menus.next_frame = Some(if next + p * 2 < now { now + p } else { next + p });
        }
    }
    if !shown {
        // nothing could be shown before the next frame: at least the game's own
        present_game(m);
    }
}

/// Show the game's own frame (drawn and not presented).
fn present_game(m: &mut Machine) {
    if let Some(g) = m.glide.as_mut() {
        g.backend.swap(0);
    }
    note_present(m);
}

fn present(m: &mut Machine) {
    if let Some(g) = m.glide.as_mut() {
        g.backend.swap(0);
        g.extra_frames += 1;
    }
    m.native.menus.stats.extra += 1;
    note_present(m);
}

fn note_present(m: &mut Machine) {
    let now = Instant::now();
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    let s = &mut m.native.menus.stats;
    s.shown += 1;
    if let Some(t) = s.last_present {
        let gap = (now - t).as_secs_f64();
        s.longest_gap = s.longest_gap.max(gap);
        if gap > 0.1 {
            crate::debug!("native menus: {:.1} ms between two presents (game frame {frame})", gap * 1000.0);
        }
    }
    s.last_present = Some(now);
    let since = *s.since.get_or_insert(now);
    let dt = (now - since).as_secs_f64();
    if dt >= 2.0 {
        crate::info!(
            "native menus: {:.1} presents per second ({:.1} game frames, {:.1} extra), longest gap {:.1} ms, extra frame drawn in {:.3} ms on average, {} dropped",
            s.shown as f64 / dt,
            s.game_frames as f64 / dt,
            s.extra as f64 / dt,
            s.longest_gap * 1000.0,
            if s.extra > 0 { s.draw_s * 1000.0 / s.extra as f64 } else { 0.0 },
            s.dropped
        );
        *s = Stats { since: Some(now), last_present: Some(now), ..Default::default() };
    }
}

/// Draw an extra frame at `alpha` between steps `a` and `b` (not shown yet): the recorded
/// 2D replayed, the native views drawn again with interpolated cameras and objects.
fn extra_frame(m: &mut Machine, a: &Step, b: &Step, alpha: f32) {
    let t0 = Instant::now();
    if let Some(g) = m.glide.as_mut() {
        g.flush();
        clear_frame(g);
    }
    let mut from = 0;
    for (va, vb) in a.views.iter().zip(&b.views) {
        if let Some(g) = m.glide.as_mut() {
            g.extra_hud(from, vb.ops);
        }
        from = vb.ops;
        let cam = GameCamera::lerp(&va.cam, &vb.cam, alpha as f64);
        frame::set_camera_override(m, Some(cam), Some(alpha));
        m.native.menu_extra = true;
        m.native.menu_drawing = true;
        if vb.world {
            let cells = super::visibility::GameCells { list: None, own: None, frustum_box: super::visibility::CellBox { row: 0, col: 0, rows: 0, cols: 0 } };
            frame::draw_world_view(m, &cells);
        } else {
            frame::draw_objects_view(m);
        }
        m.native.menu_extra = false;
        m.native.menu_drawing = false;
        frame::set_camera_override(m, None, None);
    }
    if let Some(g) = m.glide.as_mut() {
        g.extra_hud(from, usize::MAX);
        g.flush();
    }
    let took = t0.elapsed().as_secs_f64();
    let s = &mut m.native.menus;
    s.draw_avg = if s.draw_avg == 0.0 { took } else { s.draw_avg * 0.9 + took * 0.1 };
    s.stats.draw_s += took;
}

/// The whole frame black and its depth cleared, as a game frame starts (the backend clears
/// the sides and the depth at each swap; the menus cover the 4:3 area with their picture).
fn clear_frame(g: &mut crate::glide::Glide) {
    let mut st = g.st;
    st.clip = [0, 0, g.width, g.height];
    st.color_mask = 1;
    st.depth_mask = 0;
    st.depth_mode = crate::glide::consts::GR_DEPTHBUFFER_DISABLE;
    let ctx = crate::glide::DrawContext { st: &st, tmu: &g.tmu, palette: &g.palette, palette_gen: g.palette_gen, ncc: &g.ncc, screen: (g.width, g.height), part: 0, kind: Default::default() };
    g.backend.clear(&ctx, 0, 255, 0);
    g.begin_view();
}

// ---------------------------------------------------------------------------------------
// SR_MENU_PROBE (diagnostics): the menus' draw calls, cameras, objects and models

fn name_at(a: u32, n: u32) -> String {
    if !mem::readable(a, n) {
        return "?".into();
    }
    let b = unsafe { mem::slice(a, n) };
    let end = b.iter().position(|&c| c == 0).unwrap_or(n as usize);
    b[..end].iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '?' }).collect()
}

/// A line whenever the caller of entry `k` changes.
fn calls(m: &Machine, f: &HookFrame, k: usize) {
    static LAST: std::sync::Mutex<[u32; 4]> = std::sync::Mutex::new([0; 4]);
    let caller = m.img.to_link(f.stack(0)).unwrap_or(0);
    let mut l = LAST.lock().unwrap();
    if l[k] != caller {
        l[k] = caller;
        let frame = crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed);
        let chain: Vec<String> = (0..40).map(|i| f.stack(i)).filter(|&v| m.img.is_code(v)).filter_map(|v| m.img.to_link(v)).take(6).map(|l| format!("{l:x}")).collect();
        crate::info!("menu probe calls frame {frame}: {} from {caller:#x} eax {:#x} (chain {})", ["78d10", "78d84", "82338", "349b8"][k], f.eax, chain.join(" "));
    }
}

/// The draw call's camera, objects and models (when they change, else every 50 frames).
fn probe(m: &Machine, f: &HookFrame) {
    static LAST: std::sync::Mutex<(u64, u32)> = std::sync::Mutex::new((0, 0));
    let frame = crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed);
    let caller = m.img.to_link(f.stack(0)).unwrap_or(0);
    let list = f.eax;
    let mut entries = Vec::new();
    for i in 0..256u32 {
        if !mem::readable(list + 4 * i, 4) {
            break;
        }
        let e = rd32(list + 4 * i);
        if e == 0 {
            break;
        }
        entries.push(e);
    }
    let sig = entries.iter().fold(caller ^ list, |h, &e| h.rotate_left(5) ^ e);
    {
        let mut l = LAST.lock().unwrap();
        if l.1 == sig && frame < l.0 + 50 {
            return;
        }
        *l = (frame, sig);
    }
    let link = |a: u32| m.img.link(a);
    let cam = rd32(link(CAMERA_PTR));
    let c = |k: u32| rd32(cam + 4 * k) as i32;
    let table = rd32(link(MODELS));
    crate::info!(
        "menu probe frame {frame}: draw from {caller:#x} list {list:#x} ({} entries); camera {cam:#x} angles ({}, {}, {}) pos ({}, {}, {}); cy {} w {} h {} ext {} far {}; models {table:#x}",
        entries.len(),
        c(0),
        c(1),
        c(2),
        c(3),
        c(4),
        c(5),
        rd32(link(0x81e3c)) as i32,
        rd32(link(0x81e50)) as i32,
        rd32(link(0x81e24)) as i32,
        m.widescreen.applied,
        rd32(link(0xcf474)) as i32,
    );
    for &e in &entries {
        if e & 0x4000_0000 != 0 {
            continue;
        }
        if !mem::readable(e, 0x60) {
            crate::info!("menu probe   entry {e:#x} unreadable");
            continue;
        }
        let mi = rd32(e);
        let model = rd32(table.wrapping_add(4 * mi));
        let parts = if mem::readable(model, 0xc8) { rd32(model + 0x14) } else { 0 };
        let mut names = Vec::new();
        for s in 0..parts.min(13) {
            let off = (0..3).map(|l| rd32(model + 0x18 + 0x34 * l + 4 * s)).find(|&o| o != 0).unwrap_or(0);
            names.push(if off != 0 { name_at(model.wrapping_add(off) + 0x68, 12) } else { "-".into() });
        }
        let i = |o: u32| rd32(e + o) as i32;
        let b: Vec<String> = (0..parts.min(13))
            .map(|s| {
                let p = e + 0x40 + 0x20 * s;
                format!("[{} {} {} | {} {} {}]", rd32(p + 8) as i32, rd32(p + 0xc) as i32, rd32(p + 0x10) as i32, rd32(p + 0x14) as i32, rd32(p + 0x18) as i32, rd32(p + 0x1c) as i32)
            })
            .collect();
        crate::info!(
            "menu probe   inst {e:#x}: model {mi} ({model:#x}, {parts} parts {names:?}) flags {:#x} pos ({}, {}, {}) angles ({}, {}, {}) +20h {:#x} B {}",
            i(4),
            i(8),
            i(0xc),
            i(0x10),
            i(0x14),
            i(0x18),
            i(0x1c),
            i(0x20),
            b.join(" ")
        );
    }
    let base = link(SCENE);
    let n = (0..1024u32).take_while(|e| rd32(base + e * 28 + 0x18) != u32::MAX).count();
    let mut names = Vec::new();
    for i in 0..4096u32 {
        if !mem::readable(table.wrapping_add(4 * i), 4) {
            names.push("<unreadable>".to_string());
            break;
        }
        let p = rd32(table.wrapping_add(4 * i));
        if p == u32::MAX {
            break;
        }
        let ok = mem::readable(p, 0xc8);
        let off = if ok { rd32(p + 0x18) } else { 0 };
        names.push(if ok && off != 0 { name_at(p.wrapping_add(off) + 0x68, 12) } else { format!("?{p:#x}") });
    }
    let libs: Vec<String> = super::textures::libraries().iter().map(|l| l.name.clone()).collect();
    crate::info!(
        "menu probe   scene table {n} entries; libraries {libs:?}, {} texture records; rows {:#x}, sky on {}; model table {table:#x}: {} models: {}",
        super::textures::records().len(),
        rd32(link(ROWS)),
        rd32(link(0x123740)),
        names.len(),
        names.join(" ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(a: [f64; 3], p: [f64; 3]) -> GameCamera {
        GameCamera { angles: a, pos: p }
    }

    #[test]
    fn camera_cuts() {
        assert!(!camera_cut(&cam([10.0, 1024.0, 0.0], [0.0, 1000.0, -3200.0]), &cam([12.0, 1030.0, 0.0], [0.0, 990.0, -3100.0])));
        // across the wrap: 2040 -> 8 is 16 units
        assert!(!camera_cut(&cam([2040.0, 0.0, 0.0], [0.0; 3]), &cam([8.0, 0.0, 0.0], [0.0; 3])));
        assert!(camera_cut(&cam([0.0, 0.0, 0.0], [0.0; 3]), &cam([0.0, 200.0, 0.0], [0.0; 3])));
        assert!(camera_cut(&cam([0.0; 3], [0.0; 3]), &cam([0.0; 3], [0.0, 0.0, 3500.0])));
    }

    #[test]
    fn steps_follow_on() {
        let t = Instant::now();
        let v = |world: bool, x: f64| ViewMark { ops: 1, world, cam: cam([0.0; 3], [x, 0.0, 0.0]) };
        let a = Step { at: t, frame: 10, views: vec![v(false, 0.0)] };
        let b = Step { at: t + Duration::from_millis(20), frame: 11, views: vec![v(false, 50.0)] };
        assert!(consecutive(&a, &b, true));
        // a skipped frame, a loading pause, another kind of view, a cut
        assert!(!consecutive(&a, &Step { frame: 12, ..b.clone() }, true));
        assert!(!consecutive(&a, &Step { at: t + Duration::from_millis(150), ..b.clone() }, true));
        assert!(!consecutive(&a, &Step { views: vec![v(true, 50.0)], ..b.clone() }, true));
        assert!(!consecutive(&a, &Step { views: vec![v(false, 5000.0)], ..b.clone() }, true));
        assert!(!consecutive(&a, &Step { views: vec![], ..b.clone() }, true));
        // fixed extra frames (SR_INTERP_STEPS): the wall time does not count
        assert!(consecutive(&a, &Step { at: t + Duration::from_millis(150), ..b.clone() }, false));
    }

    #[test]
    fn unwidened_menu_views_keep_to_the_game_columns() {
        let wide = ViewMap::menus(107.0);
        let narrow = ViewMap::menus(0.0);
        // widened by the game: the renderer widens the full-width rectangle
        assert_eq!(viewport(&wide, wide.viewport(), 107.0), [0.0, 0.0, 640.0, 480.0]);
        // not widened (display.wide_menus off) in a wide window: the game's columns only
        assert_eq!(viewport(&narrow, narrow.viewport(), 107.0), [0.0, 0.0, 639.8, 480.0]);
        // a 4:3 window: nothing to keep out
        assert_eq!(viewport(&narrow, narrow.viewport(), 0.0), [0.0, 0.0, 640.0, 480.0]);
    }
}
