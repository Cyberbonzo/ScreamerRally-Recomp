//! Native renderer, integration (wave 2 topic W2-3, `docs/re/native/integration.md`): the
//! switch, the scene of each race, and per drawn view the [`FrameData`] built from the
//! game's own camera structures, drawn where the game draws its world.
//!
//! Frame flow with the native renderer on (a race whose scene was built and uploaded):
//!
//! * first race frame of a race: `extract::build_scene`, uploaded with
//!   `Backend::native_scene`; `None` or an upload error keeps the Glide path for that race;
//! * each view the game draws (main view, rear-view mirror, split-screen views) enters the
//!   world drawing `78d84` / `78dbc`: the sky routine `7b218` is skipped
//!   ([`skip_sky`]); after the cell set-up `7a2e4` (observer at `7a531`, shared with
//!   `crate::visibility`) the native view is drawn ([`draw_world_view`]) and the game's
//!   collection of the cells' objects is skipped, so the engine goes on with the scene
//!   table's instances only (cars, shadows, smoke, tyre marks), drawn through the Glide
//!   path into the same depth buffer;
//! * interpolated extra frames (`crate::interp`) call the same drawing with the camera
//!   interpolated in floating point ([`set_camera_override`]).
//!
//! Spaces (`sr-scene`, `transforms.md`): the game's view is `q = Rc^T (p - pc)` with x left,
//! y up, z back, the eye 128 units behind the view origin. The contract's view space is x
//! right, y down, z forward, so `view = -q + (0, 0, 128)`: the camera's `pos` is the eye
//! `pc + 128 Rc[:, 2]`, its rows are `-Rc[:, k]` and view z equals the game's w = 128 - q.z
//! (the depth the Glide path writes, so both share the depth buffer).

use super::extract::StaticMeta;
use super::scene::{Camera, FrameData, Projection, Sky, View};
use super::scene::EYE_OFFSET;
use super::transforms::{ANGLE_UNIT, CENTRE_X, FOCAL, rotation};
use crate::Machine;
use crate::mem;
use std::sync::atomic::{AtomicU8, Ordering};

// ---------------------------------------------------------------------------------------
// The switch

/// `graphics.renderer`: 0 = glide (default), 1 = native (experimental).
static REQUESTED: AtomicU8 = AtomicU8::new(0);

/// Ask for the native renderer (settings; `SR_NATIVE` overrides). Takes effect at the next
/// race.
pub fn set_requested(native: bool) {
    REQUESTED.store(native as u8, Ordering::Relaxed);
}

/// The native renderer is asked for (`SR_NATIVE=1` / `0` override the setting).
pub fn requested() -> bool {
    static ENV: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    let env = *ENV.get_or_init(|| match std::env::var("SR_NATIVE").ok().as_deref().map(str::trim) {
        Some("1") | Some("on") | Some("native") => Some(true),
        Some("0") | Some("off") | Some("glide") => Some(false),
        _ => None,
    });
    env.unwrap_or(REQUESTED.load(Ordering::Relaxed) != 0)
}

// ---------------------------------------------------------------------------------------
// Game camera and view mapping (pure functions, unit-tested against transforms.rs)

/// A game camera: the camera structure's angles (a0 pitch, a1 heading, a2 roll; 2048 per
/// turn, fractional for interpolated frames) and position (world units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GameCamera {
    pub angles: [f64; 3],
    pub pos: [f64; 3],
}

impl GameCamera {
    /// From a camera structure (`[81e0c]`: angles at +0, position at +0Ch).
    pub fn read(addr: u32) -> Option<GameCamera> {
        if addr < 0x10000 || !mem::readable(addr, 0x18) {
            return None;
        }
        let d = |k: u32| unsafe { mem::rd32(addr + 4 * k) } as i32 as f64;
        Some(GameCamera { angles: [d(0), d(1), d(2)], pos: [d(3), d(4), d(5)] })
    }

    /// From the dwords of a camera structure (as `crate::interp` keeps them).
    pub fn from_dwords(c: &[i32]) -> GameCamera {
        GameCamera { angles: [c[0] as f64, c[1] as f64, c[2] as f64], pos: [c[3] as f64, c[4] as f64, c[5] as f64] }
    }

    /// Between `a` (t = 0) and `b` (t = 1), in floating point: angles the short way round
    /// (as `crate::interp` does with whole units), positions linearly.
    pub fn lerp(a: &GameCamera, b: &GameCamera, t: f64) -> GameCamera {
        let mut out = *b;
        for k in 0..3 {
            let mut d = (b.angles[k] - a.angles[k]).rem_euclid(2048.0);
            if d > 1024.0 {
                d -= 2048.0;
            }
            out.angles[k] = (a.angles[k] + d * t).rem_euclid(2048.0);
            out.pos[k] = a.pos[k] + (b.pos[k] - a.pos[k]) * t;
        }
        out
    }

    /// Whole angle units (the game's own camera, not an interpolated one).
    fn integral(&self) -> bool {
        self.angles.iter().all(|a| a.fract() == 0.0)
    }
}

/// How a view's game coordinates reach the screen: `set_view` and the 3D module's view
/// constants (`transforms.md` steps 6-7). Grid = the game's 640 x 400 space; Glide = the
/// 640 x 480 game pixels of the contract (widescreen: x from -e to 640 + e).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewMap {
    /// Projection centre column in grid pixels (`C`: 319 + the widescreen extension; the
    /// mirror 319).
    pub centre: f64,
    /// Centre row in grid pixels (`[81e3c]`: race 260, menus 200, mirror 45).
    pub cy: f64,
    /// View size in grid pixels (`[81e50]`, `[81e24]`).
    pub width: f64,
    pub height: f64,
    /// The mirror variant (`[aa5b0]` bit 0): view x negated, half the focal length in y.
    pub mirror: bool,
    /// Glide x = `x_map.0 + x_map.1 * (x_off + grid x)` (x table `[aa6c4]`, offset `[aa6b8]`).
    pub x_map: (f64, f64),
    pub x_off: f64,
    /// Glide y = `y_map.0 + y_map.1 * (y_off + grid y)` (y table `ac6cc`, offset `[aa6bc]`).
    pub y_map: (f64, f64),
    pub y_off: f64,
}

impl ViewMap {
    /// The race's main view with widescreen extension `e` (`transforms.md`, table in step 7).
    pub fn race_main(e: f64) -> ViewMap {
        ViewMap { centre: 319.0 + e, cy: 260.0, width: 640.0 + 2.0 * e, height: 400.0, mirror: false, x_map: (-e, 1.0), x_off: 0.0, y_map: (0.0, 1.2), y_off: 0.0 }
    }

    /// The menus' 3D scenes (centre row 200).
    pub fn menus(e: f64) -> ViewMap {
        ViewMap { cy: 200.0, ..ViewMap::race_main(e) }
    }

    /// The rear-view mirror (`78eb0`: x table b06cc = 0.5 i, x offset 322, y offset 28).
    pub fn mirror() -> ViewMap {
        ViewMap { centre: 319.0, cy: 45.0, width: 640.0, height: 70.0, mirror: true, x_map: (0.0, 0.5), x_off: 322.0, y_map: (0.0, 1.2), y_off: 28.0 }
    }

    /// The view the 3D module is set up for now (inside the world drawing).
    pub fn read(m: &Machine) -> ViewMap {
        let l = |a: u32| m.img.link(a);
        let rd = |a: u32| unsafe { mem::rd32(a) };
        let rdi = |a: u32| rd(a) as i32;
        let mirror = rd(l(VARIANT)) & 1 != 0;
        let centre = (rdi(l(CENTRE_IMM[mirror as usize])) >> 16) as f64;
        // the tables' lines from entries 10 and 12 (the last column of the x table is
        // 639.8 + e, off the line)
        let line = |base: u32| -> (f64, f64) {
            let t = |i: u32| f32::from_bits(rd(base.wrapping_add(4 * i))) as f64;
            if !mem::readable(base, 64) {
                return (0.0, 1.0);
            }
            let slope = (t(12) - t(10)) / 2.0;
            (t(10) - 10.0 * slope, slope)
        };
        ViewMap {
            centre,
            cy: rdi(l(CENTRE_Y)) as f64,
            width: rdi(l(VIEW_W)) as f64,
            height: rdi(l(VIEW_H)) as f64,
            mirror,
            x_map: line(rd(l(X_TABLE_PTR))),
            x_off: rdi(l(X_OFFSET)) as f64,
            y_map: line(l(Y_TABLE)),
            y_off: rdi(l(Y_OFFSET)) as f64,
        }
    }

    fn glide_x(&self, grid_x: f64) -> f64 {
        self.x_map.0 + self.x_map.1 * (self.x_off + grid_x)
    }

    fn glide_y(&self, grid_y: f64) -> f64 {
        self.y_map.0 + self.y_map.1 * (self.y_off + grid_y)
    }

    /// Focal length in grid rows (the mirror's projection shifts y by one bit more).
    fn focal_y(&self) -> f64 {
        if self.mirror { FOCAL / 2.0 } else { FOCAL }
    }

    /// The view's rectangle in game pixels (`View::viewport`): the game's own 640 grid
    /// columns around the projection centre (a widened view's extension is left out: the
    /// renderer widens a full-width rectangle by its own extension, as the Glide path's clip
    /// windows, and the sky's `s` is given at these edges).
    pub fn viewport(&self) -> [f32; 4] {
        let x0 = self.centre - CENTRE_X;
        [self.glide_x(x0) as f32, self.glide_y(0.0) as f32, self.glide_x(x0 + 640.0) as f32, self.glide_y(self.height) as f32]
    }
}

/// The near plane: view z = 128 (the game's view z' = 0).
pub const NEAR: f64 = EYE_OFFSET;
/// Beyond the game's far distance D the native renderer keeps what the game's culling
/// keeps: view z' down to -(D + 8192) (`transforms.md`, step 8), i.e. view z D + 8320.
pub const FAR_MARGIN: f64 = 8192.0 + EYE_OFFSET;

/// The contract's camera for a game camera seen through `map`; `far_distance` is the game's
/// far distance D (`[cf474]`).
pub fn camera(cam: &GameCamera, map: &ViewMap, far_distance: f64) -> Camera {
    let r = rotation(cam.angles);
    // eye = pc + 128 Rc[:, 2]; view axis k = -Rc[:, k]
    let pos = [0, 1, 2].map(|i| cam.pos[i] + EYE_OFFSET * r[i][2]);
    let mut rot = [0f32; 9];
    for k in 0..3 {
        for i in 0..3 {
            rot[k * 3 + i] = -r[i][k] as f32;
        }
    }
    // grid X = C + 256 x / z (mirror: C - 256 x / z, the view x negated), Glide x through the
    // x table; the mirror's picture is given unflipped and flipped by the renderer inside
    // the viewport (View::mirror): x_unflipped = x0 + x1 - x_glide
    let sx = map.x_map.1 * FOCAL;
    let gcx = map.glide_x(map.centre);
    let cx = if map.mirror {
        let vp = map.viewport();
        vp[0] as f64 + vp[2] as f64 - gcx
    } else {
        gcx
    };
    let proj = Projection {
        center: [cx as f32, map.glide_y(map.cy) as f32],
        scale: [sx as f32, (map.y_map.1 * map.focal_y()) as f32],
        near: NEAR as f32,
        far: (far_distance + FAR_MARGIN) as f32,
    };
    Camera { pos, rot, proj }
}

/// The game's sky and ground for a view (`7b218`, `polygons.md` "Sky and background"),
/// continuous in the camera: the game's mapping (s0 = ((7FFh - heading) & 3FFh) / 4: 256
/// texels per 180 degrees of heading; the horizon row h = [798ac] / 128 + cy + 40 - 512 + 2
/// with [798ac] the pitch's sine in Q14) computed in floating point from the camera's
/// angles, without the game's quantisation (the sine table's entry >> 1, the integer
/// divisions truncated towards zero, whole rows). The game's own integer result
/// ([`game_horizon_row`]) is within one row of it for every whole-unit camera; following
/// the quantised values made the sky jump by a row whenever a shaking camera crossed one, and
/// against the interpolated frames' (already continuous) values at every simulation step.
/// `ground` is the ground colour (`0xRRGGBB`).
pub fn sky(cam: &GameCamera, map: &ViewMap, texture: u32, ground: u32) -> Sky {
    let heading = cam.angles[1];
    let s0 = (2047.0 - heading).rem_euclid(1024.0) * 0.25;
    let h = horizon_row(16384.0 * (cam.angles[0] * ANGLE_UNIT).sin(), map);
    let n1 = map.height - 1.0;
    let mut yb = 511.0 - h.abs();
    let te = if yb > n1 { (yb - n1) * 0.5 } else { 0.0 };
    yb = yb.min(n1);
    let table = |i: f64| map.y_map.0 + map.y_map.1 * i;
    let top = table(map.y_off);
    let horizon = top + table(yb);
    let s = if map.mirror { [s0 + 159.0, s0] } else { [s0, s0 + 159.0] };
    Sky {
        texture,
        s: [s[0] as f32, s[1] as f32],
        t: [(h.abs() / 2.0) as f32, (255.0 - te) as f32],
        horizon: horizon as f32,
        ground: (table(yb) < table(n1)).then_some(ground),
    }
}

/// `7b218`'s h for a pitch sine `q14` (`[798ac]`), in floating point.
fn horizon_row(q14: f64, map: &ViewMap) -> f64 {
    q14 / 128.0 + map.cy + 40.0 - 512.0 + 2.0
}

/// The game's own h (`7b218`, integer arithmetic) for a whole-unit pitch: `[798ac]` = the
/// sine table's entry (`table_sin`, Q15) >> 1, `((-54152 [798ac]) >> 7) / -54152`
/// truncated towards zero (diagnostics and tests: [`sky`] is continuous).
pub fn game_horizon_row(table_sin: i32, map: &ViewMap) -> f64 {
    let s = (table_sin as i16 >> 1) as i64;
    let a = ((-54152i64 * s) as i32) >> 7;
    (a / -54152) as f64 + map.cy + 40.0 - 512.0 + 2.0
}

/// A 5:6:5 colour as the game expands it for its constant colours (`0x4288` -> `0x405040`).
pub fn rgb565_to_888(c: u32) -> u32 {
    ((c >> 11) & 31) << 19 | ((c >> 5) & 63) << 10 | (c & 31) << 3
}

// ---------------------------------------------------------------------------------------
// Link addresses

/// The 3D module's camera pointer (the camera structure of the view being drawn).
const CAMERA_PTR: u32 = 0x81e0c;
/// The views' camera objects (`[1296bc + 4 v]`, structure at +A64h).
const VIEW_CAMERAS: u32 = 0x1296bc;
const CAMERA_STRUCT: u32 = 0xa64;
/// Bit 0: the mirror variant of the drawing.
const VARIANT: u32 = 0xaa5b0;
/// The projection centre (`add edx, imm << 16`) of the transform and its mirror variant.
const CENTRE_IMM: [u32; 2] = [0x7ab1a, 0x7ac1e];
const CENTRE_Y: u32 = 0x81e3c;
const VIEW_W: u32 = 0x81e50;
const VIEW_H: u32 = 0x81e24;
const X_TABLE_PTR: u32 = 0xaa6c4;
const X_OFFSET: u32 = 0xaa6b8;
const Y_OFFSET: u32 = 0xaa6bc;
const Y_TABLE: u32 = 0xac6cc;
/// The far distance D (set_far 82fa4 writes -D at cf470 and D at cf474).
const FAR: u32 = 0xcf474;
/// The animation clock.
const CLOCK: u32 = 0x1473a0;
/// The sine table (8 bytes per angle: sin at +0, cos at +4, Q15 in the low words).
const SINE: u32 = 0xc48e0;
/// Sky on (else the view is filled with [123744]), ground colour (5:6:5, low half).
const SKY_ON: u32 = 0x123740;
const GROUND: u32 = 0x81e1c;
const FLAT_FILL: u32 = 0x123744;
/// The game's fog on (night and fog races, `crate::conditions`; set by the track loader).
const FOG_ON: u32 = 0x12373c;
/// The world's row table and the model table (identify the loaded world).
const ROWS: u32 = 0x123720;
const MODELS: u32 = 0x147390;
/// Scene table (28-byte entries: name, instance A at +0Ch, index at +18h, -1 ends it).
const SCENE: u32 = 0x12f860;
/// Return address of set_far in the race set-up (37cdf: call 82fa4): a race was loaded.
pub const RACE_SET_FAR_RETURN: u32 = 0x37ce4;

// ---------------------------------------------------------------------------------------
// State

/// The integration's state (`Machine::native`).
#[derive(Default)]
pub struct NativeState {
    /// The scene of the current race is on the GPU: views are drawn natively.
    pub active: bool,
    /// Race frames are running (cleared by the menus' frame timer).
    pub in_race: bool,
    /// The scene on the GPU is a menu's (W4-2, `native::menus`), not a race's.
    pub menu: bool,
    /// A menu's 3D scene is being drawn natively (from its draw call to the next frame).
    pub menu_drawing: bool,
    /// An extra frame of a menu is being drawn (no guest code runs: `native::menus`).
    pub menu_extra: bool,
    /// The menus' scenes and their extra frames.
    pub menus: super::menus::MenuState,
    /// The sky hook could not be installed (another observer holds 7b218).
    pub(super) unavailable: bool,
    /// The race set-up has run since the last scene was built (set_far from 37cdf).
    race_loaded: bool,
    /// The world the scene was built for (row table, model table): a change is a new race.
    pub(super) key: (u32, u32),
    /// A scene was asked for in this race (built or not).
    pub(super) tried: bool,
    meta: Vec<StaticMeta>,
    sky_texture: Option<u32>,
    vis: Option<super::visibility::StaticCells>,
    /// Fades per view slot (0 main, 1 second player, 2 rear-view mirror): the menus' scenes
    /// and the comparison rule (`SR_NATIVE_VIS=lists`).
    fades: [super::visibility::Fader; 3],
    /// The races' rule, backed by the world, per view slot (`docs/re/native/visibility.md`).
    backed: [super::visibility::Backed; 3],
    /// Backing results tags sent (`FrameData::backing_tag` = count << 2 | slot).
    backing_tags: u64,
    /// Per view slot: simulation steps seen (the slot's own frames), and the step of the
    /// last extra frame (interpolation on: the fades' clock follows the extra frames).
    vis_steps: [u64; 3],
    vis_extra_step: [Option<u64>; 3],
    /// The comparison rule's newest results per slot (the floating detector).
    lists_results: [Option<super::scene::BackingResults>; 3],
    /// Per view slot: the last camera cell's own list (the base while the camera is in a cell
    /// without one).
    last_own: [Option<Vec<u32>>; 3],
    /// The visibility editor's edited lists (`crate::visedit::edits_active`), per view slot:
    /// the rule's fades, whether the slot's last view used it, and the effective list of its
    /// camera cell (`visedit::version` and camera cell it was read for).
    edited: [super::visibility::Edited; 3],
    edit_mode: [bool; 3],
    edit_lists: [Option<EditList>; 3],
    /// The camera of the view drawn next, in floating point (interpolated frames), and the
    /// position between the last two simulation steps.
    camera_override: Option<GameCamera>,
    alpha: Option<f32>,
    /// The scene table's first car instance (env phase).
    car: u32,
    /// The moving objects of the race (wave 3, `native::dynamics`); None: the game draws
    /// them through the Glide path (the hybrid frames of wave 2, `SR_NATIVE_DYNAMICS=0`).
    pub dynamics: Option<super::dynamics::Dynamics>,
    /// Camera collision of the race (`native::camera`).
    pub camera: super::camera::CameraState,
    stats: Stats,
    first_logged: bool,
}

/// `SR_NATIVE_DYNAMICS=0` (diagnostics): cars, shadows, smoke and marks through the Glide
/// path (wave 2's hybrid frames).
fn dynamics_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_NATIVE_DYNAMICS").map_or(true, |v| v.trim() != "0"))
}

/// `SR_NATIVE_SKY_LOG=1` (diagnostics): the sky of every main view in the log.
fn sky_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_NATIVE_SKY_LOG").is_ok_and(|v| v.trim() == "1"))
}

/// `SR_NATIVE_GROUNDED=0` (diagnostics): only the statics of the game's cells, no grounded
/// statics beyond them.
fn grounded_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_NATIVE_GROUNDED").map_or(true, |v| v.trim() != "0"))
}

/// `SR_NATIVE_VIS=lists` (diagnostics, comparisons): the wave-2 rule in races (the game's
/// cells as the engine walks them plus grounded statics, fading with membership), with the
/// floating detector; default: backed by the world.
fn lists_rule() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_NATIVE_VIS").is_ok_and(|v| v.trim() == "lists"))
}

/// `graphics.visibility = original` (outside the visibility editor's tool): the 1997 rule as
/// the engine walks it, the camera cell's own list inside the frustum's box (every cell of the
/// box when the camera cell has none), nothing added: no candidates, no grounded statics, no
/// saved edits (the visibility editor's files stay on disk, unused). Changes of the drawn set
/// fade as the comparison rule's (`SR_NATIVE_VIS=lists` with `SR_NATIVE_GROUNDED=0`).
fn original_lists() -> bool {
    crate::visibility::MODE.load(std::sync::atomic::Ordering::Relaxed) == crate::visibility::ORIGINAL && !crate::visedit::tool_mode()
}

/// `SR_NATIVE_VIS_LOG=1` (diagnostics): a line per view of the game's own frames with the
/// visibility's counts, the floating detector and the fade-outs (`native vis:`).
fn vis_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_NATIVE_VIS_LOG").is_ok_and(|v| v.trim() != "0"))
}

/// A view slot's effective list as last read (`NativeState::edit_lists`).
struct EditList {
    version: u64,
    cell: (u16, u16),
    /// `crate::visedit::effective_list` of `cell` (sorted; `None`: no list).
    list: Option<Vec<u32>>,
}

/// `SR_VISEDIT_LOG=1` (diagnostics): per main view of the game's own frames in the tool, the
/// live data published for the visibility editor (`visedit:`); `=2`: of every drawn frame,
/// interpolated ones included. 0: off.
fn visedit_log() -> u8 {
    static ON: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_VISEDIT_LOG").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

/// `SR_VISEDIT_TEST_HIGHLIGHT=row,col` (tests): the editor's highlight on that cell, blinking,
/// as if the editor's mouse were over it.
fn test_highlight() -> Option<(u16, u16)> {
    static AT: std::sync::OnceLock<Option<(u16, u16)>> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        let v = std::env::var("SR_VISEDIT_TEST_HIGHLIGHT").ok()?;
        let (r, c) = v.trim().split_once(',')?;
        Some((r.trim().parse().ok()?, c.trim().parse().ok()?))
    })
}

/// `SR_VISEDIT_TEST_EDIT=frame,cam_row,cam_col,row,col,0|1` (tests): at that game frame, cell
/// (row, col) hidden (0) or drawn (1) from camera cell (cam_row, cam_col) through
/// `visedit::apply`, as the editor window does it (an edit made during the race).
fn test_edit() -> Option<(u64, (u16, u16), (u16, u16), bool)> {
    static AT: std::sync::OnceLock<Option<(u64, (u16, u16), (u16, u16), bool)>> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        let v: Vec<u64> = std::env::var("SR_VISEDIT_TEST_EDIT").ok()?.split(',').map(|s| s.trim().parse().ok()).collect::<Option<_>>()?;
        (v.len() == 6).then(|| (v[0], (v[1] as u16, v[2] as u16), (v[3] as u16, v[4] as u16), v[5] != 0))
    })
}

/// The camera's cell (row, column) as the game finds it (`7a2e4`: the camera structure's x and
/// z).
fn game_cell(cam_pos: [f64; 3]) -> (usize, usize) {
    let cell_of = |c: f64| ((c as i64 + 0x10_0000) >> 13).clamp(0, 255) as usize;
    (cell_of(cam_pos[2]), cell_of(cam_pos[0]))
}

/// A direction on the ground (x, z) of unit length (`fallback` when it has none).
fn ground_dir(x: f64, z: f64, fallback: [f32; 2]) -> [f32; 2] {
    let l = (x * x + z * z).sqrt();
    if l > 1e-9 { [(x / l) as f32, (z / l) as f32] } else { fallback }
}

#[derive(Default)]
struct Stats {
    since: Option<std::time::Instant>,
    views: u64,
    build_s: f64,
    submit_s: f64,
    draws: u64,
    triangles: u64,
    errors: u64,
    drawn_statics: u64,
    fading: u64,
    /// The visibility (races): time, statics tested, drawn beyond the base, floating.
    vis_s: f64,
    tested: u64,
    extra: u64,
    floating: u64,
}

/// The camera of the next native view, in floating point (`crate::interp`'s extra frames):
/// `None` = the game's camera structure. `alpha`: between the previous simulation step
/// (0) and the last one (1), for the fades.
pub fn set_camera_override(m: &mut Machine, cam: Option<GameCamera>, alpha: Option<f32>) {
    m.native.camera_override = cam;
    m.native.alpha = alpha;
}

/// The native renderer draws the world of this view (a race with its scene uploaded, or a
/// menu's 3D scene with its own, `native::menus`).
pub fn drawing(m: &Machine) -> bool {
    let this_view = if m.native.menu { m.native.menu_drawing } else { m.native.in_race };
    m.native.active && this_view && m.glide.is_some()
}

/// set_far was called (observer in `crate::widescreen`): from the race set-up it marks a
/// newly loaded race.
pub fn set_far_called(m: &mut Machine, return_link: Option<u32>) {
    if return_link == Some(RACE_SET_FAR_RETURN) {
        m.native.race_loaded = true;
    }
}

/// The race's scene built again at this frame's start (`crate::conditions`: the night
/// assets switched live: the cars' models, the backdrop, the reflection map).
pub fn rebuild_scene(m: &mut Machine) {
    m.native.race_loaded = true;
}

/// The menus' frame timer ran: no race frames until the next race frame timer.
pub fn menu_frame(m: &mut Machine) {
    m.native.in_race = false;
    m.native.menu_drawing = false;
}

/// Start of every race frame (`crate::native::race_frame`): at the first one of a race,
/// build and upload its scene when the native renderer is asked for.
pub fn race_frame(m: &mut Machine) {
    m.native.in_race = true;
    m.native.menu_drawing = false;
    if m.native.menu {
        // a menu's scene is on the GPU: the race's is built again
        m.native.menu = false;
        m.native.active = false;
        m.native.dynamics = None;
        m.native.key = (0, 0);
    }
    let l = |a: u32| m.img.link(a);
    let key = unsafe { (mem::rd32(l(ROWS)), mem::rd32(l(MODELS))) };
    if m.native.race_loaded || key != m.native.key {
        m.native.race_loaded = false;
        m.native.key = key;
        m.native.tried = false;
        if m.native.active {
            crate::info!("native renderer: new race, scene dropped");
        }
        m.native.active = false;
        m.native.dynamics = None;
    }
    // decided once, at the race's first frame: a change of the setting applies from the
    // next race
    if m.native.tried {
        return;
    }
    m.native.tried = true;
    if m.native.unavailable || !requested() || m.glide.is_none() {
        return;
    }
    start_race(m);
}

fn start_race(m: &mut Machine) {
    let t0 = std::time::Instant::now();
    let Some(build) = super::extract::build_scene(m) else {
        crate::info!("native renderer: no scene for this race (the world could not be read); the Glide path draws it");
        return;
    };
    let build_ms = t0.elapsed().as_secs_f64() * 1000.0;
    activate(m, build, build_ms, false);
}

/// Upload a scene and draw with it from now on (a race's, or with `menu` a menu's:
/// `native::menus`); false: the Glide path keeps drawing.
pub(super) fn activate(m: &mut Machine, build: super::extract::SceneBuild, build_ms: f64, menu: bool) -> bool {
    if build.statics.len() != build.scene.statics.len() {
        crate::warn!("native renderer: {} static metas for {} statics; the Glide path draws this race", build.statics.len(), build.scene.statics.len());
        return false;
    }
    if !menu {
        // the grid's lists of this race's world, for the visibility editor's map below (they
        // are otherwise copied at the first view's cell set-up, after this: the map had the
        // previous world's lists, or none)
        crate::visibility::refresh_lists(m);
    }
    let t1 = std::time::Instant::now();
    let r = m.glide.as_mut().unwrap().backend.native_scene(&build.scene);
    match r {
        Ok(()) => {
            crate::info!(
                "native renderer: {}scene built in {build_ms:.1} ms, uploaded in {:.1} ms: {} textures, {} materials, {} meshes, {} statics ({} grounded); sky texture {:?}",
                if menu { "menu " } else { "" },
                t1.elapsed().as_secs_f64() * 1000.0,
                build.scene.textures.len(),
                build.scene.materials.len(),
                build.scene.meshes.len(),
                build.scene.statics.len(),
                build.statics.iter().filter(|s| s.grounded).count(),
                build.sky_texture
            );
            m.native.camera = if menu { Default::default() } else { super::camera::CameraState::new(m, &build) };
            // the visibility editor's map of the race (`crate::visedit`): in the tool, and for
            // a track whose saved edits the game draws
            if !menu {
                let t = std::time::Instant::now();
                let mut map = crate::visedit::TrackMap::from_scene(&build.scene, &build.statics, crate::visibility::lists(m), &crate::toolmode::track_name(m).unwrap_or_else(|| "track".into()), crate::visedit::tool_mode());
                // the road's line, for "Extend along the road" (`crate::road`)
                map.road = crate::road::read(m).filter(|line| crate::road::fits(line, map.bounds)).and_then(|line| crate::road::Road::new(&line));
                let (road_points, road_length) = map.road.as_ref().map_or((0, 0.0), |r| (r.points().len(), r.length()));
                crate::info!(
                    "visibility editor: map of the race built in {:.1} ms ({} triangles, {} textures; road line {} points, {:.0} units)",
                    t.elapsed().as_secs_f64() * 1000.0,
                    map.tris.len(),
                    map.textures.len(),
                    road_points,
                    road_length
                );
                crate::visedit::publish_map(map);
            }
            m.native.vis = Some(super::visibility::StaticCells::new(&build.statics));
            m.native.dynamics = dynamics_on().then(|| super::dynamics::Dynamics::new(m, &build, !menu));
            m.native.meta = build.statics;
            m.native.sky_texture = build.sky_texture;
            m.native.fades = Default::default();
            m.native.backed = Default::default();
            m.native.lists_results = Default::default();
            m.native.last_own = Default::default();
            m.native.edited = Default::default();
            m.native.edit_mode = Default::default();
            m.native.edit_lists = Default::default();
            m.native.car = find_car(m);
            m.native.active = true;
            m.native.menu = menu;
            m.native.stats = Stats::default();
            m.native.first_logged = false;
            true
        }
        Err(e) => {
            crate::warn!("native renderer: scene upload failed ({e}); the Glide path draws this {}", if menu { "menu" } else { "race" });
            false
        }
    }
}

/// The scene table's first car (`auto*`) instance, or 0.
fn find_car(m: &Machine) -> u32 {
    let base = m.img.link(SCENE);
    for i in 0..1024u32 {
        let e = base + i * 28;
        let (inst, index) = unsafe { (mem::rd32(e + 0xc), mem::rd32(e + 0x18)) };
        if index == u32::MAX {
            break;
        }
        if unsafe { mem::slice(e, 4) } == b"auto" && mem::readable(inst, 0x40) {
            return inst;
        }
    }
    0
}

/// The sky routine (`7b218`, called first by the world drawing of every view).
const SKY: u32 = 0x7b218;

/// Install the sky observer. Always installed (the switch may change during the run); it
/// does nothing while the native renderer is off.
pub fn install(m: &mut Machine) {
    if let Err(e) = m.install_observer("sky (native renderer)", SKY, |m, _f, tramp| {
        if skip_sky(m) { crate::hooks::HookResult::Return(0) } else { crate::hooks::HookResult::Resume(tramp) }
    }) {
        crate::warn!("native renderer unavailable: {e}");
        m.native.unavailable = true;
    }

}

/// Observer on the sky routine `7b218`: true = skip it (the native view draws the sky).
/// The Glide layer is told that a race view is drawn, as the sky's quads would have.
pub fn skip_sky(m: &mut Machine) -> bool {
    if !drawing(m) {
        return false;
    }
    if let Some(g) = m.glide.as_mut() {
        g.race_frame = true;
    }
    true
}

/// Which of the game's views (0 main, 1 second player, 2 mirror) the camera pointer is
/// drawing.
fn view_slot(m: &Machine, cam_ptr: u32) -> usize {
    for v in 0..3u32 {
        let obj = unsafe { mem::rd32(m.img.link(VIEW_CAMERAS + 4 * v)) };
        if obj >= 0x10000 && obj.wrapping_add(CAMERA_STRUCT) == cam_ptr {
            return v as usize;
        }
    }
    if ViewMap::read(m).mirror { 2 } else { 0 }
}

/// After the cell set-up of a view's world drawing (`7a531`): draw the native view now and
/// return true (the caller then skips the game's collection of the cells' objects).
/// `cells`: the cells the game would draw (its visibility list as `crate::visibility`
/// left it, or the frustum box).
pub fn draw_world_view(m: &mut Machine, cells: &super::visibility::GameCells) -> bool {
    draw_view(m, Some(cells))
}

/// A menu's objects drawn without sky and cells (`78d10`, `native::menus`): the native view
/// of the instance list only, over what the frame holds.
pub fn draw_objects_view(m: &mut Machine) -> bool {
    draw_view(m, None)
}

/// `cells`: None = the objects only (no sky or background, no statics).
fn draw_view(m: &mut Machine, cells: Option<&super::visibility::GameCells>) -> bool {
    if !drawing(m) {
        return false;
    }
    let t0 = std::time::Instant::now();
    let l = |a: u32| m.img.link(a);
    let cam_ptr = unsafe { mem::rd32(l(CAMERA_PTR)) };
    let map = ViewMap::read(m);
    let slot = view_slot(m, cam_ptr);
    let Some(cam) = m.native.camera_override.take().or_else(|| GameCamera::read(cam_ptr)) else { return false };
    let alpha = m.native.alpha.take();
    let far = unsafe { mem::rd32(l(FAR)) } as i32 as f64;
    let mut view = View { camera: camera(&cam, &map, far.abs()), viewport: map.viewport(), mirror: map.mirror, clear: None, fog: None, sky: None };
    // the game's distance fog (night: black, fog: grey; `crate::conditions`) while its fog
    // flag is on: its table and colour as the track loader gave them to Glide (the game's w is
    // the view depth: w_scale 1)
    if !m.native.menu && unsafe { mem::rd32(m.img.link(FOG_ON)) } != 0 {
        if let Some(g) = m.glide.as_ref() {
            view.fog = Some(sr_scene::Fog { color: g.st.fog_color & 0xff_ffff, table: g.st.fog_table, w_scale: 1.0 });
        }
    }
    if m.native.menu {
        // a menu's 3D scene the game does not widen (display.wide_menus off) stays in the
        // 4:3 area in a wide window, as the Glide path draws it (the renderer widens a
        // full-width rectangle)
        view.viewport = super::menus::viewport(&map, view.viewport, crate::view::ext());
    }
    // the chase cameras kept out of walls (only the drawn camera; native::camera)
    super::camera::adjust(m, &mut view, &cam, slot, alpha);
    let l = |a: u32| m.img.link(a);
    // sky and ground, or the flat background
    let rd = |a: u32| unsafe { mem::rd32(a) };
    if cells.is_none() {
        // objects only: drawn over the frame (the menu's picture)
    } else if rd(l(SKY_ON)) != 0 {
        let ground = rgb565_to_888(rd(l(GROUND)) & 0xffff);
        match m.native.sky_texture {
            Some(t) => {
                let k = sky(&cam, &map, t, ground);
                if sky_log() && slot == 0 {
                    // SR_NATIVE_SKY_LOG (diagnostics): the sky per main view, and for a
                    // whole-unit camera the horizon the game's integer arithmetic gives
                    let game = cam.integral().then(|| {
                        let a = (cam.angles[0] as i32 & 0x7ff) as u32;
                        let table = |i: f64| map.y_map.0 + map.y_map.1 * i;
                        let h = game_horizon_row(rd(l(SINE) + 8 * a) as u16 as i16 as i32, &map);
                        table(map.y_off) + table((511.0 - h.abs()).min(map.height - 1.0).trunc())
                    });
                    let frame = crate::glide::FRAME.load(Ordering::Relaxed);
                    let extra = crate::interp::in_extra_draw();
                    crate::info!(
                        "sky: frame {frame} {} alpha {:.3} camera {:.3?} s0 {:.4} t0 {:.4} horizon {:.4} game horizon {game:?}",
                        if extra { "extra" } else { "step" },
                        alpha.unwrap_or(1.0),
                        cam.angles,
                        k.s[0],
                        k.t[0],
                        k.horizon
                    );
                }
                view.sky = Some(k);
            }
            None => view.clear = Some(rgba(ground)),
        }
    } else {
        view.clear = Some(rgba(rgb565_to_888(rd(l(FLAT_FILL)) & 0xffff)));
    }
    // visibility (docs/re/native/integration.md, "Visibility")
    let extra = crate::interp::in_extra_draw() || m.native.menu_extra;
    let ext = crate::view::ext();
    // fades advance once per simulation step (the game's own frame); extra frames show them
    // between the last two steps
    let mut static_backing = Vec::new();
    let mut backing_tag = 0;
    // races: the fades' clock (simulation steps), for the moving objects too
    let mut race_t = None;
    let static_fade = match (m.native.vis.as_ref(), cells) {
        // objects only: no statics
        (_, None) => vec![0; m.native.meta.len()],
        // the menus' scenes: the wave-2 rule
        (Some(vis), Some(cells)) if m.native.menu => {
            if !extra {
                let target = vis.targets(&m.native.meta, cells, &view, ext, grounded_on());
                let onscreen = super::visibility::on_screen(&m.native.meta, &view, ext);
                m.native.fades[slot.min(2)].step(&target, &onscreen);
            }
            m.native.fades[slot.min(2)].values(if extra { alpha } else { None })
        }
        // races: backed by the world (docs/re/native/visibility.md)
        (Some(_), Some(cells)) => {
            let (fade, backing, tag, t) = race_visibility(m, cells, &view, cam.pos, ext, slot.min(2), extra, alpha);
            static_backing = backing;
            backing_tag = tag;
            race_t = Some(t);
            fade
        }
        (None, _) => Vec::new(),
    };
    let l = |a: u32| m.img.link(a);
    if !m.native.first_logged {
        m.native.first_logged = true;
        let c = &view.camera;
        crate::info!(
            "native renderer: first view (slot {slot}{}): camera {cam:?} -> eye {:?}, projection {:?}, viewport {:?}, far distance {far}, cells {}; statics drawn {} of {}",
            if map.mirror { ", mirror" } else { "" },
            c.pos,
            c.proj,
            view.viewport,
            cells.map_or("none (objects only)".to_string(), |cells| format!(
                "{} in the box rows {}+{} cols {}+{}",
                cells.list.as_ref().map_or("no list".to_string(), |l| format!("list of {}", l.len())),
                cells.frustum_box.row,
                cells.frustum_box.rows,
                cells.frustum_box.col,
                cells.frustum_box.cols
            )),
            static_fade.iter().filter(|&&f| f > 0).count(),
            static_fade.len()
        );
    }
    probe(m, &view, &static_fade, slot);
    m.native.stats.drawn_statics += static_fade.iter().filter(|&&f| f > 0).count() as u64;
    m.native.stats.fading += static_fade.iter().filter(|&&f| f > 0 && f < 255).count() as u64;
    let car = m.native.car;
    // the animation clock as the engine uses it (8246c: [1473a0] & 3FFFh)
    let clock = rd(l(CLOCK)) & 0x3fff;
    // per object in the game ([7bb98] = instance +3Ch); one value: the player's car's
    let env_phase = if car != 0 && mem::readable(car, 0x40) { rd(car + 0x3c) } else { 0 };
    // the moving objects (wave 3)
    let race = race_t.map(|t| super::dynamics::RaceCtx { static_fade: &static_fade, t, tag: backing_tag });
    let dynamics = super::dynamics::view_dynamics(m, &super::dynamics::ViewCtx { view: &view, cam, mirror: map.mirror, slot, extra, alpha, race });
    // the visibility editor (docs/05-visibility-editor.md): the sector under its mouse blinks
    // in the main view (the moving objects above follow the rule's fades, not the blinking),
    // and the main view's camera, the player's car and the statics drawn are published
    let edited_main = race_t.is_some() && slot == 0 && m.native.edit_mode[0];
    let mut blinked = None;
    if edited_main {
        if let (Some(cell), true) = crate::visedit::highlight() {
            if super::visibility::blink_inverted(crate::time::uptime().as_secs_f64()) {
                let mut f = static_fade.clone();
                if let Some(vis) = m.native.vis.as_ref() {
                    super::visibility::invert(&mut f, vis.in_cell(cell));
                }
                blinked = Some(f);
            }
        }
        if crate::visedit::tool_mode() {
            publish_live(m, &view, &cam, &static_fade, extra, alpha, blinked.is_some());
        }
    }
    let frame = FrameData {
        views: vec![view],
        static_fade: blinked.unwrap_or(static_fade),
        clock,
        env_phase,
        dynamics: dynamics.instances,
        meshes: Vec::new(),
        static_backing,
        backing_tag,
        dynamic_fade: dynamics.fade,
        dynamic_backing: dynamics.backing,
        dynamic_order: dynamics.order,
        dynamic_dissolve: dynamics.dissolve,
    };
    let t1 = std::time::Instant::now();
    let Some(g) = m.glide.as_mut() else { return false };
    g.flush();
    // (the renderer hands its commands to the GPU after view 0 of a frame: here after every
    // view, the main view's world being drawn while the game's code runs on)
    let r = g.backend.native_frame(&frame).and_then(|_| g.backend.native_view(&frame, 0));
    let t2 = std::time::Instant::now();
    let s = &mut m.native.stats;
    match r {
        Ok(v) => {
            s.views += 1;
            s.draws += v.draws as u64;
            s.triangles += v.triangles as u64;
        }
        Err(e) => {
            s.errors += 1;
            if s.errors <= 5 {
                crate::warn!("native renderer: drawing view {slot} failed: {e}");
            }
        }
    }
    s.build_s += (t1 - t0).as_secs_f64();
    s.submit_s += (t2 - t1).as_secs_f64();
    let since = *s.since.get_or_insert(t0);
    if t2 - since >= std::time::Duration::from_secs(5) && s.views > 0 {
        let n = s.views as f64;
        crate::info!(
            "native renderer: {} views in {:.1} s; per view: build {:.3} ms (visibility {:.3}), submit {:.3} ms, {:.0} draws, {:.0} triangles, {:.0} statics drawn ({:.1} fading, {:.1} beyond the base, {:.1} tested, {:.2} floating); {} errors",
            s.views,
            (t2 - since).as_secs_f64(),
            s.build_s * 1000.0 / n,
            s.vis_s * 1000.0 / n,
            s.submit_s * 1000.0 / n,
            s.draws as f64 / n,
            s.triangles as f64 / n,
            s.drawn_statics as f64 / n,
            s.fading as f64 / n,
            s.extra as f64 / n,
            s.tested as f64 / n,
            s.floating as f64 / n,
            s.errors
        );
        *s = Stats { since: Some(t2), ..Default::default() };
        if let Some(line) = m.native.dynamics.as_mut().and_then(|d| d.take_stats()) {
            crate::info!("native dynamics: {line}");
        }
    }
    if m.native.menu && !extra {
        // the menus' extra frames draw this view again (native::menus)
        super::menus::view_drawn(m, cells.is_some(), cam);
    }
    true
}

/// The races' visibility for one view (`docs/re/native/visibility.md`): the base (the
/// designers' list of the camera cell), the candidates (the lists of the track in view), the
/// backing results that arrived, the fades. Returns `static_fade`, `static_backing`, the
/// tag of this view's backing results and the fades' clock (simulation steps).
#[allow(clippy::too_many_arguments)]
fn race_visibility(m: &mut Machine, cells: &super::visibility::GameCells, view: &View, cam_pos: [f64; 3], ext: f32, slot: usize, extra: bool, alpha: Option<f32>) -> (Vec<u8>, Vec<u8>, u64, f64) {
    use super::visibility as v;
    let t0 = std::time::Instant::now();
    // results that arrived (deterministic runs: wait for them, so that a run repeats)
    let results = m.glide.as_mut().map_or_else(Vec::new, |g| g.backend.native_backing(crate::time::deterministic()));
    let original = original_lists();
    let lists = lists_rule() || original;
    for mut r in results {
        let s = (r.tag & 3) as usize;
        if s < 3 {
            // the moving objects' (the floating detector, `native::dynvis`)
            if !r.dynamics.is_empty() {
                super::dynamics::receive_backing(m, s, r.tag, std::mem::take(&mut r.dynamics));
            }
            if lists {
                if m.native.lists_results[s].as_ref().is_none_or(|p| p.tag < r.tag) {
                    m.native.lists_results[s] = Some(r);
                }
            } else {
                m.native.backed[s].receive(r);
            }
        }
    }
    if m.native.vis.is_none() {
        return (Vec::new(), Vec::new(), 0, 0.0);
    }
    // the visibility editor's edited lists (docs/05-visibility-editor.md): in the tool, or for
    // a track with saved edits; a change of rule starts the new one afresh
    let edit = !lists && crate::visedit::edits_active();
    if edit != m.native.edit_mode[slot] {
        m.native.edit_mode[slot] = edit;
        if edit {
            m.native.edited[slot] = Default::default();
        } else {
            m.native.backed[slot] = Default::default();
        }
    }
    if edit {
        return edited_visibility(m, cells, view, cam_pos, ext, slot, extra, alpha, t0);
    }
    let meta = &m.native.meta;
    let n = meta.len();
    // the camera's cell as the game finds it (7a2e4: the camera structure's x and z)
    let cell_of = |c: f64| ((c as i64 + 0x10_0000) >> 13).clamp(0, 255) as usize;
    // a camera cell without a list (the camera outside the track's cells: the far chase
    // camera, a car off the road, stretches of track through cells without lists): the game
    // draws the whole frustum box (with the extended draw distance nearly the whole world,
    // floating objects included); the base is the list of the nearest cell with one merged
    // with the last list the camera had (`visibility::nearby_list`)
    let own_cells;
    let cell_list = cells.own.is_some();
    if v::cell_check_on() && !extra {
        let frame = m.glide.as_ref().map_or(0, |g| g.frames);
        v::cell_check(crate::visibility::lists(m), cells.own.as_deref(), (cell_of(cam_pos[2]), cell_of(cam_pos[0])), frame, slot);
    }
    let cells = if cells.own.is_none() && !lists {
        // the camera's position in fractional cells (as cell_of)
        let frac = |c: f64| (c + 1048576.0) / 8192.0;
        let last = m.native.last_own[slot].as_deref();
        let near = if v::nolist_last() { last.map(|l| l.to_vec()) } else { v::nearby_list(crate::visibility::lists(m), (cell_of(cam_pos[2]), cell_of(cam_pos[0])), (frac(cam_pos[2]), frac(cam_pos[0])), last) };
        match near {
            Some(own) => {
                own_cells = super::visibility::GameCells { own: Some(own), ..cells.clone() };
                &own_cells
            }
            None => cells,
        }
    } else {
        if cells.own.is_some() {
            m.native.last_own[slot] = cells.own.clone();
        }
        cells
    };
    let (vis, meta) = (m.native.vis.as_ref().unwrap(), &m.native.meta);
    let base = vis.base(n, cells);
    let mut candidate = vis.candidates(n, cells, crate::visibility::lists(m), view, ext);
    for (c, &b) in candidate.iter_mut().zip(&base) {
        *c &= !b;
    }
    let at_risk = if lists { vec![false; n] } else { vis.at_risk(&base, (cell_of(cam_pos[2]), cell_of(cam_pos[0])), crate::visibility::lists(m)) };
    let onscreen = v::on_screen(meta, view, ext);
    let dist = v::distances(meta, view);
    let sets = v::Sets { base: &base, at_risk: &at_risk, candidate: &candidate, onscreen: &onscreen, dist: &dist, min_sky: v::min_sky_pixels(view) };
    let (t, tag) = vis_clock(&mut m.native.vis_steps, &mut m.native.vis_extra_step, &mut m.native.backing_tags, slot, extra, alpha);
    let (fade, backing, report) = if lists {
        let before = m.native.fades[slot].values(None);
        if !extra {
            let target = vis.targets(meta, cells, view, ext, grounded_on() && !original);
            m.native.fades[slot].step(&target, &onscreen);
        }
        let fade = m.native.fades[slot].values(if extra { alpha } else { None });
        let backing = v::lists_backing(&fade, &sets);
        let mut report = v::lists_report(&fade, &sets, m.native.lists_results[slot].as_ref());
        if !extra && before.len() == fade.len() {
            // fade-outs: fully drawn before this step, fading now, on screen
            let cur = m.native.fades[slot].values(None);
            report.fade_outs = (0..cur.len()).filter(|&i| before[i] == 255 && cur[i] < 255 && onscreen[i]).map(|i| (i as u32, dist[i])).collect();
        }
        (fade, backing, report)
    } else {
        let report = m.native.backed[slot].update(&sets, t);
        let (fade, backing, _) = m.native.backed[slot].output(&sets);
        (fade, backing, report)
    };
    let s = &mut m.native.stats;
    s.vis_s += t0.elapsed().as_secs_f64();
    s.tested += backing.iter().filter(|&&b| b & (super::scene::BACKING_TEST1 | super::scene::BACKING_TEST2) != 0).count() as u64;
    s.extra += report.extra as u64;
    s.floating += report.floating.len() as u64;
    if vis_log() && !extra {
        let frame = m.glide.as_ref().map_or(0, |g| g.frames);
        let mut fl: Vec<_> = report.floating.clone();
        fl.sort_by(|a, b| a.1.total_cmp(&b.1));
        let fl: Vec<String> = fl.iter().take(6).map(|(i, d, s, v)| format!("{i}@{d:.0}:{s}/{v}")).collect();
        let fo: Vec<String> = report.fade_outs.iter().take(6).map(|(i, d)| format!("{i}@{d:.0}")).collect();
        crate::info!(
            "native vis: frame {frame} slot {slot} base {} cand {} tested {} extra {} accepted {} floating {} [{}] fadeouts {} [{}] far {:.0} {:.0} own {} floating_accepted {} verdicts {:?} leaving {} nearest_fadeout {:.0} cell_list {}",
            report.base,
            report.candidates,
            backing.iter().filter(|&&b| b & (super::scene::BACKING_TEST1 | super::scene::BACKING_TEST2) != 0).count(),
            report.extra,
            report.accepted,
            report.floating.len(),
            fl.join(" "),
            report.fade_outs.len(),
            fo.join(" "),
            report.far_base,
            report.far_all,
            cells.own.is_some() as u8,
            report.floating_accepted,
            report.verdicts,
            report.floating_leaving,
            report.fade_outs.iter().map(|f| f.1).fold(f32::INFINITY, f32::min),
            cell_list as u8
        );
    }
    (fade, backing, tag, t)
}

/// The visibility editor's live data (`crate::visedit::publish_live`) from a drawn main view:
/// the game camera's position, its viewing direction on the ground (the view's forward axis),
/// its cell (the one whose effective list is drawn), the player's car, and the statics the rule
/// draws (fade above 0, before the blinking). The car: the position of player 1's car record
/// (every camera; the attract race has none: the camera's), its heading from its instance's
/// angles (the moving objects find it in the chase cameras, between the last two steps on an
/// interpolated frame; the bumper camera lists no instance for it, and looks where the car
/// heads: the camera's direction). `blink`: the frame shows the blinking's inverted half (the
/// log only).
fn publish_live(m: &Machine, view: &View, cam: &GameCamera, fade: &[u8], extra: bool, alpha: Option<f32>, blink: bool) {
    let rot = &view.camera.rot;
    let cam_forward = ground_dir(rot[6] as f64, rot[8] as f64, [0.0, -1.0]);
    let (r, c) = game_cell(cam.pos);
    let car = m.native.dynamics.as_ref().and_then(|d| d.player_car(extra, alpha));
    let record = super::dynamics::player_position(m);
    let car_pos = record.or(car.map(|c| c.0)).unwrap_or(cam.pos);
    let car_forward = match car {
        Some((_, angles)) => {
            // the car's forward axis: model -z (transforms.md)
            let r = rotation(angles);
            ground_dir(-r[0][2], -r[2][2], cam_forward)
        }
        None => cam_forward,
    };
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    let live = crate::visedit::Live {
        frame,
        cam_pos: cam.pos.map(|v| v as f32),
        cam_forward,
        cam_cell: (r as u16, c as u16),
        car_pos: car_pos.map(|v| v as f32),
        car_forward,
        drawn: fade.iter().enumerate().filter(|(_, f)| **f > 0).map(|(i, _)| i as u32).collect(),
    };
    if visedit_log() >= 2 || visedit_log() == 1 && !extra {
        crate::info!(
            "visedit: frame {frame}{} uptime {:.3} camera {:.0?} forward {:.3?} cell {:?} car {:.0?} forward {:.3?} (position {}, heading {}) drawn {} blink {} highlight {:?}",
            if extra { " extra" } else { "" },
            crate::time::uptime().as_secs_f64(),
            live.cam_pos,
            live.cam_forward,
            live.cam_cell,
            live.car_pos,
            live.car_forward,
            if record.is_some() { "record" } else if car.is_some() { "instance" } else { "camera" },
            if car.is_some() { "instance" } else { "camera" },
            live.drawn.len(),
            blink as u8,
            crate::visedit::highlight()
        );
    }
    crate::visedit::publish_live(live);
}

/// The races' visibility clock for one view: the time of its fades (simulation steps) and the
/// tag of its backing results (`FrameData::backing_tag` = count << 2 | slot). The slot's own
/// frames count steps; with interpolation the extra frames, shown one step late, run from the
/// previous step to this one.
fn vis_clock(steps: &mut [u64; 3], extra_step: &mut [Option<u64>; 3], tags: &mut u64, slot: usize, extra: bool, alpha: Option<f32>) -> (f64, u64) {
    if !extra {
        steps[slot] += 1;
    } else {
        extra_step[slot] = Some(steps[slot]);
    }
    let s = steps[slot] as f64;
    let interpolating = extra_step[slot].is_some_and(|e| e + 2 >= steps[slot]);
    let t = if extra { s - 1.0 + alpha.unwrap_or(1.0) as f64 } else if interpolating { s - 1.0 } else { s };
    *tags += 1;
    (t, *tags << 2 | slot as u64)
}

/// The races' visibility for one view while the visibility editor's edits are active
/// (`docs/re/native/visibility.md`, "Edited lists"): exactly the statics of the cells in the
/// effective list of the camera's cell (`crate::visedit::effective_list`, read again whenever
/// the edits or the cell change: an edit shows in the next frame) inside the frustum box, with
/// fades ([`super::visibility::Edited`]); no candidates, no GPU test (`static_backing` empty:
/// every static in the first pass). A camera cell without any effective list (no list within
/// `visedit::NOLIST_REACH` cells and nothing added): the base as the automatic rule takes it
/// (the nearest list merged with the last one, or the box). Returns as [`race_visibility`].
#[allow(clippy::too_many_arguments)]
fn edited_visibility(m: &mut Machine, cells: &super::visibility::GameCells, view: &View, cam_pos: [f64; 3], ext: f32, slot: usize, extra: bool, alpha: Option<f32>, t0: std::time::Instant) -> (Vec<u8>, Vec<u8>, u64, f64) {
    use super::visibility as v;
    let (r, c) = game_cell(cam_pos);
    let cell = (r as u16, c as u16);
    if let Some(h) = test_highlight() {
        if crate::visedit::highlight() != (Some(h), true) {
            crate::visedit::set_highlight(Some(h), true);
        }
    }
    if let Some((at, cam, hit, visible)) = test_edit() {
        let frame = m.glide.as_ref().map_or(0, |g| g.frames);
        if frame == at && slot == 0 && !extra && crate::visedit::apply(&[cam], &[hit], visible) {
            crate::info!("visibility edits: test edit at frame {frame}: {hit:?} {} from camera cell {cam:?}", if visible { "drawn" } else { "hidden" });
        }
    }
    // the edits as they are now (an edit by the editor's thread shows in this frame)
    let version = crate::visedit::version();
    if m.native.edit_lists[slot].as_ref().is_none_or(|e| e.version != version || e.cell != cell) {
        let moved = m.native.edit_lists[slot].as_ref().is_none_or(|e| e.cell != cell);
        m.native.edit_lists[slot] = Some(EditList { version, cell, list: crate::visedit::effective_list(cell) });
        if vis_log() && moved && slot == 0 {
            // a new camera cell (diagnostics): its effective list and what the road extension
            // adds to it, cells with statics only
            let with_statics = |e: &u32| m.native.vis.as_ref().is_some_and(|v| !v.in_cell(crate::visedit::cell_of_entry(*e)).is_empty());
            let fmt = |l: &mut dyn Iterator<Item = u32>| l.map(|e| format!("{},{}", e >> 16, e & 0xffff)).collect::<Vec<_>>().join(" ");
            let list = m.native.edit_lists[slot].as_ref().and_then(|e| e.list.clone()).unwrap_or_default();
            let extend = crate::visedit::map().and_then(|map| crate::visedit::road_extension(&map, cell, crate::visedit::edits().road)).map(|r| r.cells).unwrap_or_default();
            let frame = m.glide.as_ref().map_or(0, |g| g.frames);
            crate::info!(
                "native vis edit cell: frame {frame} cell ({r}, {c}) list [{}] road extension [{}]",
                fmt(&mut list.iter().copied().filter(with_statics)),
                fmt(&mut extend.iter().map(|&c| crate::visedit::entry(c)).filter(with_statics))
            );
        }
    }
    // no effective list: the automatic rule's base (as race_visibility finds it for a cell
    // without a list)
    let fallback;
    let base_cells = if m.native.edit_lists[slot].as_ref().is_some_and(|e| e.list.is_some()) {
        None
    } else if cells.own.is_none() {
        let frac = |p: f64| (p + 1048576.0) / 8192.0;
        let near = v::nearby_list(crate::visibility::lists(m), (r, c), (frac(cam_pos[2]), frac(cam_pos[0])), m.native.last_own[slot].as_deref());
        fallback = v::GameCells { own: near, ..cells.clone() };
        Some(&fallback)
    } else {
        Some(cells)
    };
    if cells.own.is_some() {
        m.native.last_own[slot] = cells.own.clone();
    }
    let (vis, meta) = (m.native.vis.as_ref().unwrap(), &m.native.meta);
    let n = meta.len();
    let list = m.native.edit_lists[slot].as_ref().and_then(|e| e.list.as_deref());
    let target = match (list, base_cells) {
        (Some(l), _) => vis.in_list(n, l, &cells.frustum_box),
        (None, Some(b)) => vis.base(n, b),
        (None, None) => vec![false; n],
    };
    let onscreen = v::on_screen(meta, view, ext);
    let (t, tag) = vis_clock(&mut m.native.vis_steps, &mut m.native.vis_extra_step, &mut m.native.backing_tags, slot, extra, alpha);
    let fade = m.native.edited[slot].update(&target, &onscreen, t);
    m.native.stats.vis_s += t0.elapsed().as_secs_f64();
    if vis_log() && !extra {
        // the designers' list of the camera cell as the automatic rule's base takes it (inside
        // the box; none: the box): what the tool draws without edits
        let vis = m.native.vis.as_ref().unwrap();
        let designers = vis.base(n, cells);
        let count = |s: &[bool]| s.iter().filter(|&&b| b).count();
        let both = |a: &[bool], b: &[bool]| a.iter().zip(b).filter(|(x, y)| **x && **y).count();
        let drawn: Vec<bool> = fade.iter().map(|&f| f > 0).collect();
        let frame = m.glide.as_ref().map_or(0, |g| g.frames);
        crate::info!(
            "native vis edit: frame {frame} slot {slot} cell ({r}, {c}) list {} targets {} onscreen {} drawn {} drawn_onscreen {} fading {} designers {} designers_onscreen {} same_as_designers {} own_list {}",
            list.map_or("none".to_string(), |l| l.len().to_string()),
            count(&target),
            both(&target, &onscreen),
            count(&drawn),
            both(&drawn, &onscreen),
            fade.iter().filter(|&&f| f > 0 && f < 255).count(),
            count(&designers),
            both(&designers, &onscreen),
            (target == designers) as u8,
            cells.own.is_some() as u8
        );
    }
    (fade, Vec::new(), tag, t)
}

fn probe_at() -> Option<u64> {
    static AT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *AT.get_or_init(|| std::env::var("SR_NATIVE_PROBE").ok().and_then(|v| v.trim().parse().ok()))
}

/// `SR_NATIVE_PROBE=<game frame>` (diagnostics): at that frame, every static within 30 000
/// units of the main view's camera, with its fade and where its bounding sphere's centre
/// lands on the screen.
fn probe(m: &Machine, view: &View, fade: &[u8], slot: usize) {
    // `SR_NATIVE_CAMLOG=<n>` (diagnostics): the main view's eye every n game frames
    static CAMLOG: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    if let Some(n) = *CAMLOG.get_or_init(|| std::env::var("SR_NATIVE_CAMLOG").ok().and_then(|v| v.trim().parse().ok()).filter(|&n: &u64| n > 0)) {
        let frame = m.glide.as_ref().map_or(0, |g| g.frames);
        if slot == 0 && frame % n == 0 && !crate::interp::in_extra_draw() {
            let p = view.camera.pos;
            let r = view.camera.rot;
            crate::info!("native cam frame {frame}: eye [{:.0}, {:.0}, {:.0}] cell ({}, {}) forward [{:.2}, {:.2}, {:.2}]", p[0], p[1], p[2], ((p[2] as i64 + 0x10_0000) >> 13), ((p[0] as i64 + 0x10_0000) >> 13), r[6], r[7], r[8]);
        }
    }
    let Some(at) = probe_at() else { return };
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    if frame != at || slot != 0 || crate::interp::in_extra_draw() {
        return;
    }
    let c = &view.camera;
    crate::info!("native probe frame {frame}: camera eye {:?} rows {:?}", c.pos, c.rot);
    for (i, (meta, &f)) in m.native.meta.iter().zip(fade).enumerate() {
        let d =[meta.center[0] - c.pos[0], meta.center[1] - c.pos[1], meta.center[2] - c.pos[2]];
        let v = [0, 1, 2].map(|k| c.rot[k * 3] as f64 * d[0] + c.rot[k * 3 + 1] as f64 * d[1] + c.rot[k * 3 + 2] as f64 * d[2]);
        let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if dist > 30000.0 {
            continue;
        }
        let screen = (v[2] > 1.0).then(|| (c.proj.center[0] as f64 + c.proj.scale[0] as f64 * v[0] / v[2], c.proj.center[1] as f64 + c.proj.scale[1] as f64 * v[1] / v[2]));
        crate::info!(
            "native probe frame {frame}: static {i} cell {:?} grounded {} radius {:.0} dist {dist:.0} view ({:.0}, {:.0}, {:.0}) screen {screen:?} fade {f}",
            meta.cell,
            meta.grounded,
            meta.radius,
            v[0],
            v[1],
            v[2]
        );
    }
}

fn rgba(c: u32) -> [u8; 4] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::transforms::{CENTRE_X, Y_SCALE, project, world_to_view};

    /// The reference: transforms.rs's float model of the engine (world -> game view ->
    /// grid -> Glide through the view's tables), topic D's verified projection.
    fn reference(cam: &GameCamera, map: &ViewMap, p: [f64; 3]) -> Option<(f64, f64, f64)> {
        let v = world_to_view(cam.pos, cam.angles).apply(p);
        let v = if map.mirror { [-v[0], v[1], v[2]] } else { v };
        let (gx, gy) = project(v, map.centre, map.cy, map.mirror)?;
        Some((map.glide_x(gx), map.glide_y(gy), crate::native::transforms::EYE - v[2]))
    }

    /// What the renderer makes of the contract's camera (the contract's projection, the
    /// mirror flipped about the viewport's centre).
    fn native(cam: &Camera, view_mirror: bool, viewport: [f32; 4], p: [f64; 3]) -> Option<(f64, f64, f64)> {
        let d = [p[0] - cam.pos[0], p[1] - cam.pos[1], p[2] - cam.pos[2]];
        let r = &cam.rot;
        let v = [0, 1, 2].map(|k| r[k * 3] as f64 * d[0] + r[k * 3 + 1] as f64 * d[1] + r[k * 3 + 2] as f64 * d[2]);
        if v[2] < cam.proj.near as f64 {
            return None;
        }
        let x = cam.proj.center[0] as f64 + cam.proj.scale[0] as f64 * v[0] / v[2];
        let y = cam.proj.center[1] as f64 + cam.proj.scale[1] as f64 * v[1] / v[2];
        let x = if view_mirror { viewport[0] as f64 + viewport[2] as f64 - x } else { x };
        Some((x, y, v[2]))
    }

    fn cameras() -> Vec<GameCamera> {
        vec![
            GameCamera { angles: [1950.0, 1543.0, 44.0], pos: [-60231.0, 512.0, 10994.0] },
            GameCamera { angles: [0.0, 0.0, 0.0], pos: [0.0, 300.0, 0.0] },
            GameCamera { angles: [1958.0, 520.0, 1958.0], pos: [-104289.0, 2970.0, -58025.0] },
            // interpolated: fractional angles and positions
            GameCamera { angles: [1949.37, 1001.5, 3.25], pos: [-6221.4, 270.6, 40994.9] },
        ]
    }

    /// World points around a camera, given in its game view space (x left, y up, z back):
    /// in front at several distances and directions, one behind the near plane.
    fn points(cam: &GameCamera) -> Vec<[f64; 3]> {
        let r = rotation(cam.angles);
        let mut out = Vec::new();
        for q in [[0.0, 0.0, -10.0], [500.0, -200.0, -3000.0], [-1234.0, 777.0, -20000.0], [40.0, -100.0, -300.0], [-9000.0, 2500.0, -45000.0], [300.0, 120.0, 50.0]] {
            out.push([0, 1, 2].map(|i| cam.pos[i] + r[i][0] * q[0] + r[i][1] * q[1] + r[i][2] * q[2]));
        }
        out
    }

    fn check(map: &ViewMap) {
        let far = 50000.0;
        for cam in cameras() {
            let c = camera(&cam, map, far);
            let vp = map.viewport();
            let mut compared = 0;
            for p in points(&cam) {
                let a = reference(&cam, map, p);
                let b = native(&c, map.mirror, vp, p);
                match (a, b) {
                    (Some(a), Some(b)) => {
                        compared += 1;
                        // f32 rotation rows: relative error ~1e-7 of the distance
                        let tol = 1e-3 + 2e-6 * a.2.abs();
                        assert!((a.0 - b.0).abs() < tol && (a.1 - b.1).abs() < tol, "{map:?} {cam:?} {p:?}: game {a:?} native {b:?}");
                        assert!((a.2 - b.2).abs() < 1e-6 * a.2.abs() + 1e-3, "depth {a:?} {b:?}");
                    }
                    (None, None) => {}
                    _ => panic!("{map:?} {cam:?} {p:?}: visibility differs: game {a:?} native {b:?}"),
                }
            }
            // five points in front of the near plane, one behind it
            assert_eq!(compared, 5);
            // the eye is 128 units behind the view origin, and view z is the game's w
            let r = rotation(cam.angles);
            // the contract's eye offset is topic D's eye distance
            assert_eq!(EYE_OFFSET, crate::native::transforms::EYE);
            let eye = [0, 1, 2].map(|i| cam.pos[i] + crate::native::transforms::EYE * r[i][2]);
            for i in 0..3 {
                assert!((eye[i] - c.pos[i]).abs() < 1e-9);
            }
            assert_eq!(c.proj.near, 128.0);
            assert_eq!(c.proj.far, (far + FAR_MARGIN) as f32);
        }
    }

    #[test]
    fn race_main_view_matches_the_game() {
        check(&ViewMap::race_main(0.0));
        check(&ViewMap::race_main(107.0));
        let c = camera(&cameras()[0], &ViewMap::race_main(107.0), 50000.0);
        // Glide principal point (319, 1.2 * 260), focal lengths 256 and 307.2
        assert_eq!(c.proj.center, [CENTRE_X as f32, (260.0 * Y_SCALE) as f32]);
        assert_eq!(c.proj.scale, [256.0, 307.2]);
        // the game's 4:3 rectangle (the renderer widens a full-width one)
        assert_eq!(ViewMap::race_main(107.0).viewport(), [0.0, 0.0, 640.0, 480.0]);
    }

    #[test]
    fn menu_view_matches_the_game() {
        check(&ViewMap::menus(0.0));
        check(&ViewMap::menus(213.0));
    }

    #[test]
    fn mirror_view_matches_the_game() {
        check(&ViewMap::mirror());
        let map = ViewMap::mirror();
        assert_eq!(map.viewport(), [161.0, 33.6, 481.0, 117.6]);
        // transforms.md: the mirror's focal lengths 128 and 153.6 at (320.5, 87.6) in Glide
        // coordinates (here unflipped: 161 + 481 - 320.5)
        let c = camera(&cameras()[0], &map, 50000.0);
        assert_eq!(c.proj.scale, [128.0, 153.6]);
        assert!((c.proj.center[0] - 321.5).abs() < 1e-4 && (c.proj.center[1] - 87.6).abs() < 1e-4);
    }

    #[test]
    fn view_space_is_right_down_forward() {
        // heading 0 looks along world -z; world x is left, y up
        let cam = GameCamera { angles: [0.0; 3], pos: [0.0; 3] };
        let c = camera(&cam, &ViewMap::race_main(0.0), 50000.0);
        let v = |p: [f64; 3]| [0, 1, 2].map(|k| (0..3).map(|i| c.rot[k * 3 + i] as f64 * (p[i] - c.pos[i])).sum::<f64>());
        let ahead = v([0.0, 0.0, -1000.0]);
        assert!(ahead[2] > 1000.0 && ahead[0].abs() < 1e-9 && ahead[1].abs() < 1e-9);
        assert!(v([-100.0, 0.0, -1000.0])[0] > 0.0, "world -x (right) is view +x");
        assert!(v([0.0, -100.0, -1000.0])[1] > 0.0, "world -y (down) is view +y");
    }

    #[test]
    fn interpolated_cameras_take_the_short_way() {
        let a = GameCamera { angles: [2040.0, 10.0, 0.0], pos: [0.0, 0.0, 0.0] };
        let b = GameCamera { angles: [8.0, 2000.0, 0.0], pos: [100.0, -50.0, 3.0] };
        let c = GameCamera::lerp(&a, &b, 0.25);
        assert!((c.angles[0] - 2044.0).abs() < 1e-9);
        // 10 -> 2000 is 58 units back across the wrap: 10 - 14.5 = -4.5 = 2043.5
        assert!((c.angles[1] - 2043.5).abs() < 1e-9);
        assert_eq!(c.pos, [25.0, -12.5, 0.75]);
        assert_eq!(GameCamera::lerp(&a, &b, 1.0).pos, b.pos);
    }

    #[test]
    fn sky_as_the_game_computes_it() {
        // chase camera on China: pitch 1950 (98 units down); the game's [798ac] = table
        // sine >> 1 = -9704 >> 1 = -4852, here 16384 sin = -4852.1
        let table = crate::native::transforms::table_entry(1950).0;
        assert_eq!(table >> 1, -4852);
        let cam = GameCamera { angles: [1950.0, 1543.0, 0.0], pos: [0.0; 3] };
        let map = ViewMap::race_main(107.0);
        let k = sky(&cam, &map, 7, 0x405040);
        // the game, by hand: (-54152 * -4852) >> 7 = 2052699, / -54152 = -37 (truncated
        // towards zero), h = -37 + 260 + 40 - 512 + 2 = -247; yb = 511 - 247 = 264
        assert_eq!(game_horizon_row(table, &map), -247.0);
        // continuous: h = -4852.1 / 128 - 210 = -247.907, yb = 263.093 (< 399: te = 0)
        let h = 16384.0 * (1950.0 * ANGLE_UNIT).sin() / 128.0 - 210.0;
        assert!((h + 247.907).abs() < 1e-3, "{h}");
        assert!((k.t[0] as f64 - h.abs() / 2.0).abs() < 1e-4 && k.t[1] == 255.0);
        assert!((k.horizon as f64 - 1.2 * (511.0 + h)).abs() < 1e-3);
        // s0 = ((7FFh - 1543) & 3FFh) / 4 = 504 / 4
        assert_eq!(k.s, [126.0, 285.0]);
        assert_eq!(k.ground, Some(0x405040));
        // the mirror (cy 45, height 70, y offset 28): h = -37.907 + 45 - 470 = -462.907, yb
        // = 48.093, horizon 1.2 (28 + 48.093); s swapped
        let m = sky(&cam, &ViewMap::mirror(), 7, 0x405040);
        assert_eq!(m.s, [285.0, 126.0]);
        assert!((m.t[0] - 231.454).abs() < 1e-3 && m.t[1] == 255.0, "{:?}", m.t);
        assert!((m.horizon - 1.2 * 76.093).abs() < 1e-3);
        assert_eq!(m.ground, Some(0x405040));
        // looking further down in the mirror: the horizon clamps to the view (te > 0, no
        // ground)
        let low = sky(&GameCamera { angles: [0.0, 0.0, 0.0], pos: [0.0; 3] }, &ViewMap::mirror(), 7, 1);
        // h = 0 + 45 - 470 = -425, yb = 86 > 69: te = 8.5, yb = 69
        assert_eq!(low.t, [212.5, 246.5]);
        assert_eq!(low.ground, None);
        // the same mapping as the game's: for every whole pitch and each view, the horizon
        // row within one row of the game's integer one (its truncations), on the other side
        // of zero as the game truncates towards zero
        for map in [ViewMap::race_main(107.0), ViewMap::menus(0.0), ViewMap::mirror()] {
            let mut worst = 0.0f64;
            for a in 0..2048 {
                let game = game_horizon_row(crate::native::transforms::table_entry(a).0, &map);
                let ours = horizon_row(16384.0 * (a as f64 * ANGLE_UNIT).sin(), &map);
                worst = worst.max((ours - game).abs());
            }
            assert!(worst < 1.01, "{map:?}: {worst}");
        }
        // continuous: a hundredth of an angle unit moves the horizon by a hundredth of the
        // game's 0.39 rows per unit, never a whole row
        let step = |p: f64| sky(&GameCamera { angles: [p, 1543.0, 0.0], pos: [0.0; 3] }, &map, 7, 0).horizon as f64;
        for p in [1950.0, 1950.5, 2047.99, 0.0, 30.0] {
            let d = (step(p + 0.01) - step(p)).abs();
            assert!(d > 0.0 && d < 0.01, "{p}: {d}");
        }
    }

    #[test]
    fn ground_colour_expands_like_the_game() {
        assert_eq!(rgb565_to_888(0x4288), 0x405040);
    }
}
