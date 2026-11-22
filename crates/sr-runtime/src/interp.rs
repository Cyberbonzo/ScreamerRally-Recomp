//! Drawing extra frames between the game's own, without letting them change the game.
//!
//! The game simulates and draws once per loop iteration, at most 50 times a second
//! (docs/02-architecture.md, section 6). To show more frames, the runtime calls the game's
//! own world drawing again from the buffer-swap hook, with the camera (and the cars) set to
//! values interpolated between the last two simulation steps. Drawing writes guest memory
//! (vertex tables, polygon lists, render caches, some car fields), so every page an extra
//! draw writes is put back afterwards:
//!
//! * all guest memory is allocated with `MEM_WRITE_WATCH`;
//! * `Guard` keeps a shadow copy of it, brought up to date before extra draws with the
//!   pages the game wrote since the last time (`sync`);
//! * after an extra draw, the pages it wrote are copied back from the shadow (`restore`).
//!
//! The guest stack is not covered: extra draws only use it below the current frames.

use std::collections::HashMap;

use windows_sys::Win32::System::Memory::GetWriteWatch;

const WRITE_WATCH_FLAG_RESET: u32 = 1;

use crate::Machine;
use crate::mem;

const PAGE: u32 = 4096;

struct Region {
    size: u32,
    shadow: Vec<u8>,
}

#[derive(Default)]
pub struct Guard {
    regions: HashMap<u32, Region>,
    scratch: Vec<usize>,
    pub restored_pages: u64,
}

impl Guard {
    /// Make the covered set match `list` (address, size): new regions are copied whole,
    /// regions no longer present are dropped.
    pub fn set_regions(&mut self, list: &[(u32, u32)]) {
        self.regions.retain(|base, r| list.iter().any(|&(b, s)| b == *base && s == r.size));
        for &(base, size) in list {
            if size == 0 || self.regions.contains_key(&base) {
                continue;
            }
            let shadow = unsafe { mem::slice(base, size) }.to_vec();
            dirty(&mut self.scratch, base, size); // start tracking from here
            self.regions.insert(base, Region { size, shadow });
        }
    }

    /// Copy the pages written since the last `sync` / `restore` into the shadow.
    pub fn sync(&mut self) {
        for (&base, r) in self.regions.iter_mut() {
            for p in dirty(&mut self.scratch, base, r.size) {
                let off = p - base;
                let n = PAGE.min(r.size - off);
                r.shadow[off as usize..(off + n) as usize].copy_from_slice(unsafe { mem::slice(p, n) });
            }
        }
    }

    /// Bytes written outside the guest's own execution that must stay (code the runtime
    /// translated during an extra draw): the shadow gets them too.
    pub fn keep(&mut self, at: u32, bytes: &[u8]) {
        for (&base, r) in self.regions.iter_mut() {
            if at >= base && at + bytes.len() as u32 <= base + r.size {
                let off = (at - base) as usize;
                r.shadow[off..off + bytes.len()].copy_from_slice(bytes);
            }
        }
    }

    /// Put back every page written since the last `sync` / `restore`.
    pub fn restore(&mut self) -> usize {
        let mut count = 0;
        for (&base, r) in self.regions.iter_mut() {
            for p in dirty(&mut self.scratch, base, r.size) {
                let off = p - base;
                let n = PAGE.min(r.size - off);
                unsafe { mem::slice_mut(p, n) }.copy_from_slice(&r.shadow[off as usize..(off + n) as usize]);
                count += 1;
            }
        }
        self.restored_pages += count as u64;
        count
    }
}

/// Pages of [base, base + size) written since the last call (and reset the tracking).
fn dirty(buf: &mut Vec<usize>, base: u32, size: u32) -> Vec<u32> {
    let pages = (size as usize).div_ceil(PAGE as usize);
    buf.resize(pages.max(1), 0);
    let mut count = buf.len();
    let mut gran = 0u32;
    let r = unsafe { GetWriteWatch(WRITE_WATCH_FLAG_RESET, base as usize as *const _, size as usize, buf.as_mut_ptr() as *mut *mut _, &mut count, &mut gran) };
    if r != 0 {
        return Vec::new();
    }
    buf[..count].iter().map(|&a| a as u32).collect()
}

// ---------------------------------------------------------------------------------------
// Interpolated frames.

/// The game's world drawing for a view (`349b8`, eax = view index 0).
const DRAW_WORLD: u32 = 0x349b8;
/// `set_view` (see `widescreen`).
const SET_VIEW: u32 = 0x53300;
/// The HUD drawing (`4e224`), called right after the main view's world: the camera
/// structure then holds the main view's camera (the mirror's comes later).
pub const HUD_ENTRY: u32 = 0x4e224;
/// The game's end-of-frame drawing (`4b6c4`), called after the rear-view mirror.
pub const FRAME_END: u32 = 0x4b6c4;
/// The views' camera objects (view 0 main, 2 rear-view mirror); the camera structure the
/// engine uses is at +A64h (`349b8` -> `82338`).
const VIEW_CAMERAS: u32 = 0x1296bc;
const CAMERA_STRUCT: u32 = 0xa64;
const MIRROR_VIEW: u32 = 2;
/// The game's frame pacing (`crate::pacing`: 20, 30 or 40 ms).
/// Camera structure size (angles, position, Q14 matrix at +18h, its transpose at +48h).
const CAMERA_DWORDS: usize = 28;

/// The scene's object table (28-byte entries: name, ..., instance pointer at +0Ch, index
/// at +18h; -1 ends the used part). Instances hold position (+8, +0Ch, +10h) and three
/// angles (+14h..+1Ch, 0-2047) of cars, wheels, shadows, smoke, marks.
const SCENE: u32 = 0x12f860;
const SCENE_ENTRY: u32 = 28;
const SCENE_MAX: u32 = 1024;
/// Larger moves between two steps are not interpolated (a reused slot, a reset).
const MAX_MOVE: i32 = 4000;
/// A larger camera move in one step is a cut.
const MAX_CAMERA_MOVE: f64 = 3000.0;
/// Steps further apart than this are not consecutive (a menu or loading in between).
const MAX_STEP_GAP: f64 = 0.1;

#[derive(Clone)]
struct Step {
    camera: [i32; CAMERA_DWORDS],
    /// The rear-view mirror's camera, when the game drew the mirror in this step.
    mirror: Option<[i32; CAMERA_DWORDS]>,
    /// (instance address, x, y, z, three angles)
    objects: Vec<(u32, [i32; 7])>,
    at: std::time::Instant,
}

fn capture_objects(m: &Machine) -> Vec<(u32, [i32; 7])> {
    let base = m.img.link(SCENE);
    let mut out = Vec::new();
    for i in 0..SCENE_MAX {
        let e = base + i * SCENE_ENTRY;
        let (ptr, index) = unsafe { (mem::rd32(e + 0xc), mem::rd32(e + 0x18)) };
        if index == u32::MAX {
            break;
        }
        if ptr < 0x10000 || !mem::readable(ptr, 0x20) {
            continue;
        }
        let mut v = [0i32; 7];
        for (k, f) in v.iter_mut().enumerate().take(6) {
            *f = unsafe { mem::rd32(ptr + 8 + k as u32 * 4) } as i32;
        }
        // Smoke puffs (`asmoke`, `a2smoke`): +20h is their animation (it grows every step
        // and selects the engine's per-object table entry, 824bf): interpolated too.
        // Other objects: -1 = not touched.
        let name = unsafe { mem::slice(e, 6) };
        v[6] = if name.starts_with(b"asmoke") || name.starts_with(b"a2smok") { (unsafe { mem::rd32(ptr + 0x20) }) as i32 } else { -1 };
        out.push((ptr, v));
    }
    out
}

/// Interpolated object fields between two steps (objects present in both and not jumping
/// far). Every such object is returned, also one that ends up at its value of this step:
/// guest memory is put back only after the step's last extra frame, so it still holds the
/// previous extra frame's values (skipped, the cars of a frame at the step's own time were
/// drawn where the frame before had put them, under the step's camera: they jumped).
fn lerp_objects(a: &[(u32, [i32; 7])], b: &[(u32, [i32; 7])], t: f32) -> Vec<(u32, [i32; 7])> {
    let prev: std::collections::HashMap<u32, &[i32; 7]> = a.iter().map(|(p, v)| (*p, v)).collect();
    let mut out = Vec::with_capacity(b.len());
    for (p, vb) in b {
        let Some(va) = prev.get(p) else { continue };
        if (0..3).any(|k| (vb[k] - va[k]).abs() > MAX_MOVE) {
            continue;
        }
        let mut v = *vb;
        for k in 0..3 {
            v[k] = (va[k] as f32 + (vb[k] - va[k]) as f32 * t).round() as i32;
        }
        for k in 3..6 {
            v[k] = lerp_angle(va[k], vb[k], t);
        }
        // smoke animation: only while it grows within one puff's life (a reused puff
        // starts again: shown as it is)
        if va[6] >= 0 && vb[6] >= 0 && vb[6] < 0x80000 && va[6] < 0x80000 && vb[6] >= va[6] && vb[6] - va[6] < 0x10000 {
            v[6] = (va[6] as f32 + (vb[6] - va[6]) as f32 * t).round() as i32;
        }
        out.push((*p, v));
    }
    out
}

#[derive(Default)]
pub struct Interp {
    /// Drawing extra frames is switched on (settings).
    pub enabled: bool,
    /// Time of the game's own frame per step, ms (diagnostics).
    pub game_ms: Vec<f32>,
    /// Running average of an extra frame's drawing time, s.
    pub draw_avg: f64,
    /// Each wait for the display, ms (diagnostics).
    pub wait_ms: Vec<f32>,
    /// Time of each extra frame (draw to present), ms, with its game frame (diagnostics).
    pub extra_ms: Vec<(f32, u64)>,
    /// Presents per second limit with VSync off (0 = none).
    pub fps_cap: u32,
    pub guard: Guard,
    prev: Option<Step>,
    cur: Option<Step>,
    /// Camera and objects captured at the HUD of the frame being drawn.
    pending: Option<([i32; CAMERA_DWORDS], Vec<(u32, [i32; 7])>)>,
    /// set_view's centre-y argument for the main view.
    pub center_y: u32,
    /// The main view's camera structure (the camera pointer at the HUD).
    main_camera: u32,
    /// The rear-view mirror's camera structure and its `set_view` arguments (eax, edx,
    /// ebx, ecx), as the game last set it up.
    mirror_camera: u32,
    pub mirror_view: (u32, u32, u32, u32),
    /// When the hook last handed control back to the game.
    last_return: Option<std::time::Instant>,
    /// When the game's frame started working (its tick wait ended).
    pub work_start: Option<std::time::Instant>,
    pub frames: u64,
    pub restored: u64,
    /// Seconds spent per phase of the extra frames: memory sync, game drawing, HUD,
    /// restore, present (diagnostics).
    pub times: [f64; 5],
    /// Traps taken while drawing extra frames (diagnostics).
    pub traps: u64,
    /// Waits for the display (VSync): ready, timed out, seconds waited (diagnostics).
    pub waits: (u64, u64, f64),
    /// When the next frame may be shown under the frame rate limit.
    next_frame: Option<std::time::Instant>,
}

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static IN_EXTRA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// An extra frame is being drawn: services with effects outside guest memory (sound) must
/// do nothing (guest memory is put back afterwards anyway).
pub fn in_extra_draw() -> bool {
    IN_EXTRA.load(std::sync::atomic::Ordering::Relaxed)
}
static FPS_CAP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Switch interpolated frames on or off (settings; also while running).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Limit of presented frames per second when VSync is off (0 = unlimited).
pub fn set_fps_cap(cap: u32) {
    FPS_CAP.store(cap, std::sync::atomic::Ordering::Relaxed);
}

/// The limit set (`native::menus` paces the menus' extra frames with it).
pub fn fps_cap() -> u32 {
    FPS_CAP.load(std::sync::atomic::Ordering::Relaxed)
}

/// The camera pointer: the main view's camera structure while it is drawn (and at the
/// HUD), the mirror's after the mirror is drawn.
const CAMERA_PTR: u32 = 0x81e0c;

fn camera_ptr(m: &Machine) -> u32 {
    unsafe { mem::rd32(m.img.link(CAMERA_PTR)) }
}

/// Observer on the HUD drawing: the main camera of this frame.
pub fn capture_camera(m: &mut Machine) {
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        // switched off: forget the steps, so switching on again starts afresh
        m.interp.prev = None;
        m.interp.cur = None;
        m.interp.pending = None;
        return;
    }
    let p = camera_ptr(m);
    if p < 0x10000 {
        return;
    }
    let mut c = [0i32; CAMERA_DWORDS];
    for (i, v) in c.iter_mut().enumerate() {
        *v = unsafe { mem::rd32(p + i as u32 * 4) } as i32;
    }
    let objects = capture_objects(m);
    m.interp.main_camera = p;
    m.interp.pending = Some((c, objects));
}

/// Should the swap of this frame be replaced by interpolated frames?
pub fn active(m: &Machine, race: bool) -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed) && race && m.interp.pending.is_some()
}

/// Angle between two 11-bit angles (0-2047), the short way round.
fn lerp_angle(a: i32, b: i32, t: f32) -> i32 {
    let mut d = (b - a) & 2047;
    if d > 1024 {
        d -= 2048;
    }
    (a + (d as f32 * t).round() as i32) & 2047
}

/// The camera jumped between two steps (a camera change, a replay cut, a reset): such a
/// step is shown as it is, not swept through.
fn camera_cut(a: &[i32; CAMERA_DWORDS], b: &[i32; CAMERA_DWORDS]) -> bool {
    let moved = (3..6).map(|i| (b[i] as f64 - a[i] as f64).powi(2)).sum::<f64>().sqrt();
    // an axis of the rotation (Q14 rows) turned by more than ~25 degrees
    let turned = (0..3).any(|r| {
        let dot: f64 = (0..3).map(|j| a[6 + r * 3 + j] as f64 * b[6 + r * 3 + j] as f64).sum();
        dot / (16384.0 * 16384.0) < 0.9
    });
    moved > MAX_CAMERA_MOVE || turned
}

fn lerp_camera(a: &[i32; CAMERA_DWORDS], b: &[i32; CAMERA_DWORDS], t: f32) -> [i32; CAMERA_DWORDS] {
    let mut out = *b;
    let mix = |x: i32, y: i32| (x as f32 + (y - x) as f32 * t).round() as i32;
    // angles: 11 bits (the sine table is indexed by 7FFh - angle), the short way round
    for i in 0..3 {
        out[i] = lerp_angle(a[i], b[i], t);
    }
    for i in 3..6 {
        out[i] = mix(a[i], b[i]);
    }
    // rotation: blend, then make orthonormal again (rows), Q14
    let row = |m: &[i32; CAMERA_DWORDS], r: usize| [m[6 + r * 3] as f32, m[7 + r * 3] as f32, m[8 + r * 3] as f32];
    let mut r: [[f32; 3]; 3] = [[0.0; 3]; 3];
    for i in 0..3 {
        let (x, y) = (row(a, i), row(b, i));
        r[i] = [x[0] + (y[0] - x[0]) * t, x[1] + (y[1] - x[1]) * t, x[2] + (y[2] - x[2]) * t];
    }
    let dot = |u: [f32; 3], v: [f32; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    let norm = |u: [f32; 3]| {
        let l = dot(u, u).sqrt().max(1e-6);
        [u[0] / l, u[1] / l, u[2] / l]
    };
    let r0 = norm(r[0]);
    let d1 = dot(r[1], r0);
    let r1 = norm([r[1][0] - d1 * r0[0], r[1][1] - d1 * r0[1], r[1][2] - d1 * r0[2]]);
    let (d20, d21) = (dot(r[2], r0), dot(r[2], r1));
    let r2 = norm([r[2][0] - d20 * r0[0] - d21 * r1[0], r[2][1] - d20 * r0[1] - d21 * r1[1], r[2][2] - d20 * r0[2] - d21 * r1[2]]);
    let rows = [r0, r1, r2];
    for (i, rw) in rows.iter().enumerate() {
        for (j, v) in rw.iter().enumerate() {
            let q = (v * 16384.0).round() as i32;
            out[6 + i * 3 + j] = q;
            out[18 + j * 3 + i] = q; // transpose at +48h
        }
    }
    out
}

/// The guest memory an extra draw may write (all of it except the stack).
fn regions(m: &Machine) -> Vec<(u32, u32)> {
    let mut v = vec![(m.img.base, m.img.size)];
    v.extend(m.dpmi.regions());
    v.extend(m.widescreen.object_lists.iter().copied());
    v
}

/// Draw the main view with `camera`, the recorded HUD on top, put guest memory back, show it.
/// Take the shadow of guest memory once per game step, before its extra frames (no guest
/// code but their drawing runs until the game's next frame: interrupts are not delivered
/// while the guest thread is in the runtime, and are held during the drawing).
fn begin_extra_frames(m: &mut Machine) {
    // the main view's widescreen constants are in place before the shadow is taken (the
    // mirror pass may have left its own), so the extra set_view writes nothing
    crate::widescreen::ensure_main(m);
    let t0 = std::time::Instant::now();
    let regs = regions(m);
    m.interp.guard.set_regions(&regs);
    m.interp.guard.sync();
    m.interp.times[0] += t0.elapsed().as_secs_f64();
}

/// After the step's extra frames: guest memory as the game left it.
fn end_extra_frames(m: &mut Machine) {
    let t = std::time::Instant::now();
    m.interp.restored += m.interp.guard.restore() as u64;
    m.interp.times[3] += t.elapsed().as_secs_f64();
}

fn extra_frame(m: &mut Machine, camera: &[i32; CAMERA_DWORDS], mirror: Option<[i32; CAMERA_DWORDS]>, objects: &[(u32, [i32; 7])], native: &NativeCameras) {
    extra_frame_draw(m, camera, mirror, objects, native);
    present_extra(m);
}

/// The rear-view mirror's camera at `alpha` between two steps (the step's own after a cut).
fn mirror_at(a: &Step, b: &Step, alpha: f32) -> Option<[i32; CAMERA_DWORDS]> {
    match (&a.mirror, &b.mirror) {
        (Some(x), Some(y)) if !camera_cut(x, y) => Some(lerp_camera(x, y, alpha)),
        (_, y) => *y,
    }
}

/// The cameras of an extra frame in floating point, for the native renderer's views (the
/// game's own drawing gets them rounded to whole units, `lerp_camera`).
struct NativeCameras {
    main: crate::native::frame::GameCamera,
    mirror: Option<crate::native::frame::GameCamera>,
    alpha: f32,
}

fn native_cameras(a: &Step, b: &Step, alpha: f32) -> NativeCameras {
    use crate::native::frame::GameCamera;
    let lerp = |x: &[i32; CAMERA_DWORDS], y: &[i32; CAMERA_DWORDS]| GameCamera::lerp(&GameCamera::from_dwords(x), &GameCamera::from_dwords(y), alpha as f64);
    let mirror = match (&a.mirror, &b.mirror) {
        (Some(x), Some(y)) if !camera_cut(x, y) => Some(lerp(x, y)),
        (_, y) => y.as_ref().map(|y| GameCamera::from_dwords(y)),
    };
    NativeCameras { main: lerp(&a.camera, &b.camera), mirror, alpha }
}

/// Draw the rear-view mirror again from `camera` (the game: `236dc` with the mirror's
/// camera, which sets up its view, then the world for view 2). Its view start clears the
/// depths (shown as recorded, the mirror's polygons were depth tested against the main
/// view's: everything beyond what the main view had there was missing).
fn draw_mirror(m: &mut Machine, camera: &[i32; CAMERA_DWORDS], native: Option<crate::native::frame::GameCamera>, alpha: f32) {
    let p = m.interp.mirror_camera;
    if p < 0x10000 {
        return;
    }
    let (eax, edx, ebx, ecx) = m.interp.mirror_view;
    m.widescreen.force_mirror = true;
    unsafe { crate::guest::call_guest(m.img.link(SET_VIEW), eax, edx, ebx, ecx) };
    m.widescreen.force_mirror = false;
    for (i, v) in camera.iter().enumerate() {
        unsafe { mem::wr32(p + i as u32 * 4, *v as u32) };
    }
    crate::native::frame::set_camera_override(m, native, Some(alpha));
    unsafe { crate::guest::call_guest(m.img.link(DRAW_WORLD), MIRROR_VIEW, 0, 0, 0) };
    crate::native::frame::set_camera_override(m, None, None);
}

/// Show the extra frame drawn last.
fn present_extra(m: &mut Machine) {
    let t = std::time::Instant::now();
    if let Some(g) = m.glide.as_mut() {
        g.backend.swap(0);
    }
    m.interp.times[4] += t.elapsed().as_secs_f64();
    m.interp.frames += 1;
}

/// Draw an extra frame (not shown yet).
fn extra_frame_draw(m: &mut Machine, camera: &[i32; CAMERA_DWORDS], mirror: Option<[i32; CAMERA_DWORDS]>, objects: &[(u32, [i32; 7])], native: &NativeCameras) {
    let t1 = std::time::Instant::now();
    let traps = m.stats.traps;
    m.segjit.patch_log = Some(Vec::new());
    IN_EXTRA.store(true, std::sync::atomic::Ordering::Relaxed);
    crate::events::DEFER.store(true, std::sync::atomic::Ordering::Relaxed);
    m.widescreen.force_race = true;
    let set_view = m.img.link(SET_VIEW);
    let draw = m.img.link(DRAW_WORLD);
    let cy = m.interp.center_y.max(1);
    unsafe { crate::guest::call_guest(set_view, 320, 400, cy, 0) };
    // the main view's camera, as at the HUD: after the game's frame the camera pointer is
    // left on the rear-view mirror's camera when the mirror is on (written there, the
    // first extra frame drew the world with the step's own camera and the interpolated
    // cars: the player's car jumped and turned)
    let p = m.interp.main_camera;
    if p >= 0x10000 {
        unsafe { mem::wr32(m.img.link(CAMERA_PTR), p) };
        for (i, v) in camera.iter().enumerate() {
            unsafe { mem::wr32(p + i as u32 * 4, *v as u32) };
        }
    }
    for (ptr, v) in objects {
        for (k, f) in v.iter().enumerate().take(6) {
            unsafe { mem::wr32(ptr + 8 + k as u32 * 4, *f as u32) };
        }
        if v[6] >= 0 {
            unsafe { mem::wr32(ptr + 0x20, v[6] as u32) };
        }
    }
    // the native renderer's main view: the same camera in floating point
    crate::native::frame::set_camera_override(m, Some(native.main), Some(native.alpha));
    unsafe { crate::guest::call_guest(draw, 0, 0, 0, 0) };
    crate::native::frame::set_camera_override(m, None, None);
    m.widescreen.force_race = false;
    let t2 = std::time::Instant::now();
    m.interp.traps += m.stats.traps - traps;
    // the recorded HUD and mirror over it; the mirror drawn again at this frame's time
    match (mirror, m.glide.as_ref().and_then(|g| g.last_mirror())) {
        (Some(cam), Some((start, end))) => {
            if let Some(g) = m.glide.as_mut() {
                g.extra_hud(0, start);
            }
            draw_mirror(m, &cam, native.mirror, native.alpha);
            if let Some(g) = m.glide.as_mut() {
                g.extra_hud(end, usize::MAX);
                g.finish_extra_frame();
            }
        }
        _ => {
            if let Some(g) = m.glide.as_mut() {
                g.end_extra_frame();
            }
        }
    }
    let t3 = std::time::Instant::now();
    // Guest memory is put back once after the step's last extra frame (`end_extra_frames`):
    // each extra frame writes the camera and objects it draws anyway, and the world draw
    // only fills scratch it recomputes (verified: identical frames either way).
    // SR_RESTORE_EACH=1: after every frame (diagnostics).
    static EACH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *EACH.get_or_init(|| std::env::var_os("SR_RESTORE_EACH").is_some()) {
        m.interp.restored += m.interp.guard.restore() as u64;
    }
    // code the runtime translated during the draw stays translated
    if let Some(log) = m.segjit.patch_log.take() {
        for (at, bytes) in log {
            unsafe { mem::slice_mut(at, bytes.len() as u32).copy_from_slice(&bytes) };
            m.interp.guard.keep(at, &bytes);
        }
    }
    IN_EXTRA.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::events::DEFER.store(false, std::sync::atomic::Ordering::Relaxed);
    let t4 = std::time::Instant::now();
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    m.interp.extra_ms.push(((t4 - t1).as_secs_f64() as f32 * 1000.0, frame));
    for (i, d) in [t2 - t1, t3 - t2, t4 - t3].iter().enumerate() {
        m.interp.times[i + 1] += d.as_secs_f64();
    }
    // a running average of the drawing time (frames are started only when one fits)
    let took = (t4 - t1).as_secs_f64();
    m.interp.draw_avg = if m.interp.draw_avg == 0.0 { took } else { m.interp.draw_avg * 0.9 + took * 0.1 };
}

/// After the game's frame (its own present suppressed): show frames interpolated between
/// the previous and this simulation step until the game's next frame is due.
pub fn fill(m: &mut Machine) {
    let now = std::time::Instant::now();
    // frames start on the game's tick schedule (every step): anchor everything there
    let started = m.interp.work_start.take().unwrap_or(now);
    // the game's own frame (simulation and its hidden drawing) per step (diagnostics)
    let game_ms = (now - started).as_secs_f64() * 1000.0;
    m.interp.game_ms.push(game_ms as f32);
    if game_ms > 15.0 {
        let (frame, tris, dl) = m.glide.as_ref().map_or((0, 0, 0), |g| (g.frames, g.tris, g.download_count()));
        crate::debug!("slow game frame {frame}: {game_ms:.1} ms ({tris} triangles so far, {dl} texture downloads so far)");
    }
    let Some((cam, objects)) = m.interp.pending.take() else { return };
    // the mirror's camera, if the game drew the mirror in this frame
    let mirror = m.glide.as_ref().and_then(|g| g.last_mirror()).and_then(|_| {
        let obj = unsafe { mem::rd32(m.img.link(VIEW_CAMERAS + 4 * MIRROR_VIEW)) };
        let p = obj.wrapping_add(CAMERA_STRUCT);
        if obj < 0x10000 || !mem::readable(p, 4 * CAMERA_DWORDS as u32) {
            return None;
        }
        m.interp.mirror_camera = p;
        let mut c = [0i32; CAMERA_DWORDS];
        for (i, v) in c.iter_mut().enumerate() {
            *v = unsafe { mem::rd32(p + i as u32 * 4) } as i32;
        }
        Some(c)
    });
    m.interp.prev = m.interp.cur.take();
    m.interp.cur = Some(Step { camera: cam, mirror, objects, at: started });
    let consecutive = match (&m.interp.prev, &m.interp.cur) {
        (Some(a), Some(b)) => (b.at - a.at).as_secs_f64() <= MAX_STEP_GAP && !camera_cut(&a.camera, &b.camera),
        _ => false,
    };
    let (Some(a), Some(b), true) = (m.interp.prev.clone(), m.interp.cur.clone(), consecutive) else {
        // first step, or not following on from the previous one: show the game's frame
        if let Some(g) = m.glide.as_mut() {
            g.backend.swap(0);
        }
        return;
    };
    let period = (b.at - a.at).as_secs_f64().clamp(0.010, 0.050);
    // Return just before the game's next frame is due: frames are paced to the step's 100 Hz
    // ticks (`crate::pacing`) from the start of the previous one. (Measured periods must not be used
    // here: a late return would lengthen the next period, and so on.)
    let deadline = b.at + std::time::Duration::from_secs_f64(crate::pacing::period() - 0.0015);
    // SR_INTERP_STEPS=n (diagnostics): exactly n frames per step at alpha 1/n .. n/n,
    // whatever the time (the game slows down; for comparing consecutive frame dumps)
    static STEPS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let steps = *STEPS.get_or_init(|| std::env::var("SR_INTERP_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
    begin_extra_frames(m);
    if steps > 0 {
        for k in 1..=steps {
            let alpha = k as f32 / steps as f32;
            extra_frame(m, &lerp_camera(&a.camera, &b.camera, alpha), mirror_at(&a, &b, alpha), &lerp_objects(&a.objects, &b.objects, alpha), &native_cameras(&a, &b, alpha));
        }
        end_extra_frames(m);
        return;
    }
    let cap = FPS_CAP.load(std::sync::atomic::Ordering::Relaxed);
    let cap_period = (cap > 0).then(|| std::time::Duration::from_secs_f64(1.0 / cap as f64));
    if cap_period.is_none() {
        m.interp.next_frame = None;
    }
    let mut shown = false;
    loop {
        let t0 = std::time::Instant::now();
        // without a limit at least one frame per step, however late
        if t0 >= deadline && (shown || cap_period.is_some()) {
            break;
        }
        let left = deadline.saturating_duration_since(t0);
        // frame rate limit: evenly spaced frames, across steps
        if cap_period.is_some() {
            let next = *m.interp.next_frame.get_or_insert(t0);
            if next > t0 {
                if next >= deadline {
                    break;
                }
                std::thread::sleep(next - t0);
                continue;
            }
        }
        // Draw first, then wait for the display (VSync), then show: the drawing overlaps the
        // wait, so a frame that takes a little longer than a refresh still makes the next
        // one (drawing only after the wait halves the rate at heavy spots). A frame is
        // started only if it fits before the game's next frame is due.
        let draw_est = std::time::Duration::from_secs_f64(m.interp.draw_avg.max(0.001));
        if shown && draw_est >= left {
            break;
        }
        // shown one simulation step late: between the previous and this step, at the time
        // it is expected on screen
        let shown_at = std::time::Instant::now() + draw_est;
        let alpha = ((shown_at - b.at).as_secs_f64() / period).clamp(0.0, 1.0) as f32;
        extra_frame_draw(m, &lerp_camera(&a.camera, &b.camera, alpha), mirror_at(&a, &b, alpha), &lerp_objects(&a.objects, &b.objects, alpha), &native_cameras(&a, &b, alpha));
        let w0 = std::time::Instant::now();
        let left = deadline.saturating_duration_since(w0);
        let ready = m.glide.as_mut().is_none_or(|g| g.backend.wait_ready(left));
        m.interp.waits.2 += w0.elapsed().as_secs_f64();
        m.interp.wait_ms.push((w0.elapsed().as_secs_f64() * 1000.0) as f32);
        if !ready {
            // the display did not take a frame before the game's next one: dropped
            m.interp.waits.1 += 1;
            break;
        }
        m.interp.waits.0 += 1;
        present_extra(m);
        shown = true;
        if let (Some(p), Some(next)) = (cap_period, m.interp.next_frame) {
            let now = std::time::Instant::now();
            // fell behind (a slow frame, a pause): start again from now instead of catching up
            m.interp.next_frame = Some(if next + p * 2 < now { now + p } else { next + p });
        }
    }
    end_extra_frames(m);
    m.interp.last_return = Some(std::time::Instant::now());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_take_the_short_way_round() {
        assert_eq!(lerp_angle(2040, 8, 0.5), 0); // across the wrap: 2040 -> 2048(=0) -> 8
        assert_eq!(lerp_angle(8, 2040, 0.5), 0);
        assert_eq!(lerp_angle(100, 300, 0.25), 150);
        assert_eq!(lerp_angle(5, 5, 0.7), 5);
    }

    fn camera(x: i32, yaw_deg: f64) -> [i32; CAMERA_DWORDS] {
        let mut c = [0i32; CAMERA_DWORDS];
        c[3] = x;
        let (s, k) = (yaw_deg.to_radians().sin(), yaw_deg.to_radians().cos());
        let rows = [[k, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, k]];
        for (i, r) in rows.iter().enumerate() {
            for (j, v) in r.iter().enumerate() {
                c[6 + i * 3 + j] = (v * 16384.0).round() as i32;
            }
        }
        c
    }

    #[test]
    fn camera_cuts_are_detected() {
        assert!(!camera_cut(&camera(0, 0.0), &camera(200, 5.0)));
        assert!(camera_cut(&camera(0, 0.0), &camera(5000, 0.0)));
        assert!(camera_cut(&camera(0, 0.0), &camera(0, 40.0)));
    }

    #[test]
    fn interpolated_camera_stays_orthonormal() {
        let c = lerp_camera(&camera(0, 0.0), &camera(100, 20.0), 0.5);
        assert_eq!(c[3], 50);
        let row = |r: usize| [c[6 + r * 3] as f64 / 16384.0, c[7 + r * 3] as f64 / 16384.0, c[8 + r * 3] as f64 / 16384.0];
        for r in 0..3 {
            let v = row(r);
            assert!((v[0] * v[0] + v[1] * v[1] + v[2] * v[2] - 1.0).abs() < 1e-3);
        }
        // transpose kept at +48h
        assert_eq!(c[18 + 1], c[6 + 3]);
    }

    #[test]
    fn objects_jumping_far_are_not_swept() {
        let a = vec![(0x1000, [0, 0, 0, 0, 0, 0, -1]), (0x2000, [0, 0, 0, 0, 0, 0, -1])];
        let b = vec![(0x1000, [100, 0, 0, 0, 0, 0, -1]), (0x2000, [MAX_MOVE + 1, 0, 0, 0, 0, 0, -1]), (0x3000, [7, 7, 7, 0, 0, 0, -1])];
        let out = lerp_objects(&a, &b, 0.5);
        assert_eq!(out, vec![(0x1000, [50, 0, 0, 0, 0, 0, -1])]);
    }

    #[test]
    fn smoke_animation_is_interpolated_within_a_puff() {
        let a = vec![(0x1000, [0, 0, 0, 0, 0, 0, 1000]), (0x2000, [0, 0, 0, 0, 0, 0, 9000])];
        let b = vec![(0x1000, [0, 0, 0, 0, 0, 0, 3000]), (0x2000, [0, 0, 0, 0, 0, 0, 100])];
        let out = lerp_objects(&a, &b, 0.5);
        // growing: halfway; a new puff (value dropped): this step's value
        assert_eq!(out, vec![(0x1000, [0, 0, 0, 0, 0, 0, 2000]), (0x2000, [0, 0, 0, 0, 0, 0, 100])]);
    }

    #[test]
    fn objects_at_this_steps_value_are_written_too() {
        // the previous extra frame of the step left its values in guest memory
        let a = vec![(0x1000, [0, 0, 0, 0, 0, 0, -1]), (0x2000, [5, 5, 5, 1, 2, 3, -1])];
        let b = vec![(0x1000, [90, 0, 0, 0, 0, 0, -1]), (0x2000, [5, 5, 5, 1, 2, 3, -1])];
        assert_eq!(lerp_objects(&a, &b, 1.0), b);
        assert_eq!(lerp_objects(&a, &b, 0.5), vec![(0x1000, [45, 0, 0, 0, 0, 0, -1]), (0x2000, [5, 5, 5, 1, 2, 3, -1])]);
    }
}

/// `SR_OBJLOG=FILE` (diagnostics): every game frame, the position and angles of the first
/// scene objects (the player's shadow and car, then the others).
pub fn log_objects(m: &Machine) {
    use std::io::Write;
    {
        static LAST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0xffff_ffff);
        let mut cw: u16 = 0;
        unsafe { core::arch::asm!("fnstcw word ptr [{}]", in(reg) &mut cw as *mut u16) };
        if LAST.swap(cw as u32, std::sync::atomic::Ordering::Relaxed) != cw as u32 {
            crate::info!("guest x87 control word at frame start: {cw:#06x}");
        }
    }
    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Some(p) = std::env::var_os("SR_OBJLOG") {
            *FILE.lock().unwrap() = std::fs::File::create(p).ok();
        }
    });
    let mut f = FILE.lock().unwrap();
    let Some(f) = f.as_mut() else { return };
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    // the cars (entries named auto*)
    let base = m.img.link(SCENE);
    let mut line = Vec::new();
    for i in 0..SCENE_MAX {
        let e = base + i * SCENE_ENTRY;
        let (ptr, index) = unsafe { (mem::rd32(e + 0xc), mem::rd32(e + 0x18)) };
        if index == u32::MAX {
            break;
        }
        let name = unsafe { mem::slice(e, 5) };
        if name != b"auto1" && name != b"auto2" && name != b"auto3" && name != b"auto4" || !mem::readable(ptr, 0x20) {
            continue;
        }
        let v: Vec<i32> = (0..6).map(|k| unsafe { mem::rd32(ptr + 8 + 4 * k) } as i32).collect();
        line.push(format!("{},{},{},{},{},{}", v[0], v[1], v[2], v[3], v[4], v[5]));
    }
    let _ = writeln!(f, "{frame} {}", line.join(" "));
}
