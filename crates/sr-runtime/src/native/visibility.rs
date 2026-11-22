//! Which statics the native renderer draws.
//!
//! **Races: backed by the world** (`docs/re/native/visibility.md`; the decision:
//! `docs/04-native-renderer.md`, "Visibility, revised"). The tracks are stage sets (no
//! ground outside the track corridor). Per view ([`Backed`]):
//!
//! * **base**: the designers' list of the camera's cell as the game collects it (the list
//!   inside the frustum box; the box when the cell has none), not merged with other lists:
//!   drawn at once, in the first opaque pass;
//! * **candidates**: statics that the lists of the track cells in view (and the camera
//!   cell's own list beyond the box) name, not in the base: drawn only while their base (their
//!   lowest part) lies in front of world geometry already drawn, never against the sky, the
//!   background or the void, tested on the GPU every frame (`sr-scene`
//!   `FrameData::static_backing`), results used one frame late, with hysteresis;
//! * **fades**: an object that starts being drawn while on screen fades in over
//!   [`FADE_STEPS`]; an object on screen and nearer than [`NEAR_KEEP`] is never faded out
//!   (depth hides it when something is in front); others stop being drawn only when off
//!   screen (at once) or after being unbacked for a while (fading out).
//!
//! **Menus' scenes and the comparison rule** (`SR_NATIVE_VIS=lists`): the wave-2 rule
//! (`docs/re/native/integration.md`, "Visibility"):
//!
//! * **listed**: statics in the cells the game itself would draw now: the camera cell's
//!   visibility list as `crate::visibility` leaves it for the engine (the game's list, or
//!   extended, per the `graphics.visibility` setting), or the frustum box's cells when the
//!   camera cell has no list (the engine's own rule, `7a640`);
//! * **grounded**: plus statics with ground under them (`StaticMeta::grounded`) whose
//!   bounding sphere is within the draw distance (the game's far distance plus the margin
//!   its culling keeps) and the view frustum;
//! * **fades**: a static whose state changes while its bounding sphere is in the view
//!   fades over [`FADE_STEPS`] simulation steps instead of popping; one outside the view
//!   takes its new state at once (nobody sees it, and objects entering the view from its
//!   sides are therefore already there). Interpolated frames show the fades between the
//!   last two steps.

use super::extract::StaticMeta;
use super::scene::{BACKING_TEST1, BACKING_TEST2, BackingCounts, BackingResults, View};
use std::collections::HashMap;

/// Simulation steps a fade takes (33 steps per second: about a quarter of a second).
pub const FADE_STEPS: u32 = 8;

/// A base is backed when at most this share of its visible pixels shows nothing drawn ...
pub const ACCEPT_SKY: f32 = 0.10;
/// ... and unbacked from this share on (between: undecided, nothing changes), counting at
/// least [`MIN_SKY_PIXELS`] (a pixel at a silhouette is not evidence) in the race's main
/// view; in a view with a shorter focal length (the rear-view mirror: half the main view's
/// in x and y) the same object covers fewer pixels and the minimum scales with the pixel
/// area ([`min_sky_pixels`]: the mirror's quarter of 2 is 1).
pub const REJECT_SKY: f32 = 0.25;
pub const MIN_SKY_PIXELS: u32 = 2;
/// The race's main view's focal lengths in game pixels (`transforms.md`: 256 and 256 x 1.2):
/// [`MIN_SKY_PIXELS`] holds for them.
pub const REFERENCE_SCALE: [f32; 2] = [256.0, 307.2];
/// A base counts as backed only when at least this share of it is visible: a base hidden
/// behind what is drawn (beyond a crest) proves nothing and would float once the camera
/// comes over the crest.
pub const VISIBLE_SHARE: f32 = 0.5;
/// The floating detector (diagnostics): a drawn object whose base shows the sky in at least
/// this share of its visible pixels.
pub const FLOATING_SKY: f32 = 0.5;
/// Consecutive backed results before a candidate starts being drawn.
pub const ON_RESULTS: u8 = 2;
/// An accepted candidate stops being drawn after this many consecutive unbacked results
/// spanning at least [`OFF_STEPS`] simulation steps (an object kept after leaving the base,
/// never accepted, after its first unbacked result: it was floating in the base already),
/// fading out over [`FADE_OUT_STEPS`] ...
pub const OFF_RESULTS: u8 = 2;
pub const OFF_STEPS: f64 = 1.0;
pub const FADE_OUT_STEPS: u32 = 4;
/// ... unless it is nearer than this (world units, from the eye to its bounding sphere):
/// near objects on screen are never faded out.
pub const NEAR_KEEP: f64 = 8192.0;
/// The world grid's size (cells per row and column).
const GRID: usize = 256;

/// The cells the game would draw for this view (`7a52c`): those of its visibility list that
/// lie inside the view frustum's box of cells, or every cell of the box when the camera
/// cell has no list.
#[derive(Clone, Debug, PartialEq)]
pub struct GameCells {
    /// Visibility list entries (`row << 16 | col`) as the engine walks them (the designers'
    /// list, or merged per `graphics.visibility`); `None`: no list.
    pub list: Option<Vec<u32>>,
    /// The camera cell's own list (the designers', never merged); `None`: no list.
    pub own: Option<Vec<u32>>,
    /// The frustum's box of cells: first row and column, row and column counts.
    pub frustum_box: CellBox,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellBox {
    pub row: u32,
    pub col: u32,
    pub rows: u32,
    pub cols: u32,
}

impl CellBox {
    fn contains(&self, r: u16, c: u16) -> bool {
        let (r, c) = (r as u32, c as u32);
        r >= self.row && r < self.row + self.rows && c >= self.col && c < self.col + self.cols
    }
}

impl GameCells {
    pub fn contains(&self, r: u16, c: u16) -> bool {
        self.frustum_box.contains(r, c) && self.list.as_ref().is_none_or(|l| l.contains(&((r as u32) << 16 | c as u32)))
    }
}

/// The statics of a scene by cell (built once per scene).
#[derive(Clone, Debug, Default)]
pub struct StaticCells {
    by_cell: HashMap<(u16, u16), Vec<u32>>,
    grounded: Vec<u32>,
    /// Per cell: the bounding sphere of its statics.
    bounds: HashMap<(u16, u16), ([f64; 3], f32)>,
}

impl StaticCells {
    pub fn new(meta: &[StaticMeta]) -> StaticCells {
        let mut s = StaticCells::default();
        let mut boxes: HashMap<(u16, u16), ([f64; 3], [f64; 3])> = HashMap::new();
        for (i, m) in meta.iter().enumerate() {
            s.by_cell.entry(m.cell).or_default().push(i as u32);
            if m.grounded {
                s.grounded.push(i as u32);
            }
            let r = m.radius as f64;
            let b = boxes.entry(m.cell).or_insert(([f64::MAX; 3], [f64::MIN; 3]));
            for k in 0..3 {
                b.0[k] = b.0[k].min(m.center[k] - r);
                b.1[k] = b.1[k].max(m.center[k] + r);
            }
        }
        for (cell, (lo, hi)) in boxes {
            let c = [0, 1, 2].map(|k| (lo[k] + hi[k]) * 0.5);
            let r = (0..3).map(|k| (hi[k] - c[k]).powi(2)).sum::<f64>().sqrt();
            s.bounds.insert(cell, (c, r as f32));
        }
        s
    }

    /// Per static: in the **base**, the designers' list of the camera's cell as the game
    /// collects it (inside the frustum box; the box when the cell has no list).
    pub fn base(&self, n: usize, cells: &GameCells) -> Vec<bool> {
        let mut t = vec![false; n];
        let own: Option<std::collections::HashSet<u32>> = cells.own.as_ref().map(|l| l.iter().copied().collect());
        for (&(r, c), v) in &self.by_cell {
            if cells.frustum_box.contains(r, c) && own.as_ref().is_none_or(|l| l.contains(&((r as u32) << 16 | c as u32))) {
                for &i in v {
                    if let Some(x) = t.get_mut(i as usize) {
                        *x = true;
                    }
                }
            }
        }
        t
    }

    /// Per static: in the base but not in the lists of the track cells around the camera's
    /// cell (`camera_cell` = (row, column); `lists` as in [`StaticCells::candidates`]): it
    /// leaves the base when the camera moves on. No neighbouring track cell: none.
    pub fn at_risk(&self, base: &[bool], camera_cell: (usize, usize), lists: &[Vec<u32>]) -> Vec<bool> {
        let mut out = vec![false; base.len()];
        if lists.len() != GRID * GRID {
            return out;
        }
        let (r0, c0) = camera_cell;
        let mut neighbours: Vec<std::collections::HashSet<u32>> = Vec::new();
        for r in r0.saturating_sub(1)..=(r0 + 1).min(GRID - 1) {
            for c in c0.saturating_sub(1)..=(c0 + 1).min(GRID - 1) {
                if (r, c) != (r0, c0) && !lists[r * GRID + c].is_empty() {
                    neighbours.push(lists[r * GRID + c].iter().copied().collect());
                }
            }
        }
        if neighbours.is_empty() {
            return out;
        }
        for (&(r, c), v) in &self.by_cell {
            let e = (r as u32) << 16 | c as u32;
            if neighbours.iter().all(|l| l.contains(&e)) {
                continue;
            }
            for &i in v {
                if let Some(x) = out.get_mut(i as usize) {
                    *x = base[i as usize];
                }
            }
        }
        out
    }

    /// Per static: a **candidate**: in a cell named by the list of a track cell in view (a
    /// cell with a list whose statics' bounds touch the view frustum: the track ahead within
    /// the draw distance) or by the camera cell's own list. `lists`: every cell's list
    /// (`row * 256 + col`; empty: none). The base is not excluded here.
    pub fn candidates(&self, n: usize, cells: &GameCells, lists: &[Vec<u32>], view: &View, ext: f32) -> Vec<bool> {
        let f = Frustum::of(view, ext);
        let mut marks = vec![0u64; GRID * GRID / 64];
        let mut mark = |e: u32| {
            let (r, c) = ((e >> 16) as usize, (e & 0xffff) as usize);
            if r < GRID && c < GRID {
                let k = r * GRID + c;
                marks[k / 64] |= 1 << (k % 64);
            }
        };
        for &e in cells.own.iter().flatten() {
            mark(e);
        }
        if lists.len() == GRID * GRID {
            for (&cell, &(c, r)) in &self.bounds {
                let k = cell.0 as usize * GRID + cell.1 as usize;
                if k < lists.len() && !lists[k].is_empty() && f.sphere(c, r) {
                    for &e in &lists[k] {
                        mark(e);
                    }
                }
            }
        }
        let mut t = vec![false; n];
        for (&(r, c), v) in &self.by_cell {
            let k = r as usize * GRID + c as usize;
            if k < GRID * GRID && marks[k / 64] & (1 << (k % 64)) != 0 {
                for &i in v {
                    if let Some(x) = t.get_mut(i as usize) {
                        *x = true;
                    }
                }
            }
        }
        t
    }

    /// Per static: in a cell of `list` (visibility list entries `row << 16 | column`,
    /// sorted: the visibility editor's effective list) inside `frustum_box` (the edited lists'
    /// rule, [`Edited`]).
    pub fn in_list(&self, n: usize, list: &[u32], frustum_box: &CellBox) -> Vec<bool> {
        let mut t = vec![false; n];
        for (&(r, c), v) in &self.by_cell {
            if frustum_box.contains(r, c) && list.binary_search(&((r as u32) << 16 | c as u32)).is_ok() {
                for &i in v {
                    if let Some(x) = t.get_mut(i as usize) {
                        *x = true;
                    }
                }
            }
        }
        t
    }

    /// The statics of one cell (none: an empty slice).
    pub fn in_cell(&self, cell: (u16, u16)) -> &[u32] {
        self.by_cell.get(&cell).map_or(&[], |v| v.as_slice())
    }

    /// Per static: drawn by the rule (before fading). `grounded`: add the grounded statics
    /// beyond the game's cells (false: the game's cells only, diagnostics).
    pub fn targets(&self, meta: &[StaticMeta], cells: &GameCells, view: &View, ext: f32, grounded: bool) -> Vec<bool> {
        let mut t = vec![false; meta.len()];
        let list: Option<std::collections::HashSet<u32>> = cells.list.as_ref().map(|l| l.iter().copied().collect());
        for (&(r, c), v) in &self.by_cell {
            if cells.frustum_box.contains(r, c) && list.as_ref().is_none_or(|l| l.contains(&((r as u32) << 16 | c as u32))) {
                for &i in v {
                    t[i as usize] = true;
                }
            }
        }
        if !grounded {
            return t;
        }
        let f = Frustum::of(view, ext);
        for &i in &self.grounded {
            let m = &meta[i as usize];
            if !t[i as usize] && f.sphere(m.center, m.radius) {
                t[i as usize] = true;
            }
        }
        t
    }
}

/// How far (cells) [`nearby_list`] looks for a cell with a list around a camera cell
/// without one: Canada's longest stretch without lists runs up to 6 cells from the nearest
/// list (a first value of 2 left the view empty there for 2 s).
pub const NOLIST_REACH: usize = 8;

/// The base's list for a camera cell without one (the engine then draws its whole frustum
/// box, floating objects included): the designers' list of the nearest cell with one (its
/// centre nearest to the camera, `at` = (row, column) in fractional cells, within
/// [`NOLIST_REACH`] cells), merged with `last` (the last list the view had: what it showed so
/// far, also what the rear-view mirror looks back at), plus the camera's cell itself (what lies
/// under the camera). `None`: neither. `lists`: every cell's list (`row * 256 + col`), `cell`
/// = (row, column). The first version kept the last list alone: on long stretches through
/// cells without lists (Canada, Wales, Arizona, Sweden: 8-16 % of the frames, up to 400 frames
/// in a row) it no longer covered the view ahead and nothing was drawn around the car (Canada
/// 4 900: no road, the cars floating over the ground colour, where the original shows the road
/// and its buildings). Merging every list of the nearest ring instead of the nearest cell's
/// drew more objects floating (Sweden: 25 frames of the main view with a floating static
/// against 12).
pub fn nearby_list(lists: &[Vec<u32>], cell: (usize, usize), at: (f64, f64), last: Option<&[u32]>) -> Option<Vec<u32>> {
    let (r0, c0) = (cell.0.min(GRID - 1), cell.1.min(GRID - 1));
    let mut nearest: Option<(f64, usize)> = None;
    if lists.len() == GRID * GRID {
        for reach in 1..=NOLIST_REACH {
            for r in r0.saturating_sub(reach)..=(r0 + reach).min(GRID - 1) {
                for c in c0.saturating_sub(reach)..=(c0 + reach).min(GRID - 1) {
                    let k = r * GRID + c;
                    let d = (r as f64 + 0.5 - at.0).powi(2) + (c as f64 + 0.5 - at.1).powi(2);
                    if !lists[k].is_empty() && nearest.is_none_or(|n| d < n.0) {
                        nearest = Some((d, k));
                    }
                }
            }
            if nearest.is_some() {
                break;
            }
        }
    }
    if nearest.is_none() && last.is_none() {
        return None;
    }
    let mut set: std::collections::BTreeSet<u32> = nearest.map(|n| lists[n.1].iter().copied().collect()).unwrap_or_default();
    set.extend(last.into_iter().flatten().copied());
    set.insert((r0 as u32) << 16 | c0 as u32);
    Some(set.into_iter().collect())
}

/// `SR_NATIVE_VIS_CELLCHECK=1` (diagnostics): the camera's cell (row, column) as computed from
/// its position against the list the engine took (`own`), and without a list the distance to
/// the nearest cell with one, in the log.
pub fn cell_check(lists: &[Vec<u32>], own: Option<&[u32]>, cell: (usize, usize), frame: u64, slot: usize) {
    let (r, c) = cell;
    if lists.len() != GRID * GRID || r >= GRID || c >= GRID {
        return;
    }
    match own {
        Some(own) if own != lists[r * GRID + c].as_slice() => {
            crate::info!("native vis cell check: frame {frame} slot {slot}: camera cell ({r}, {c}) list differs from the engine's ({} / {} entries)", lists[r * GRID + c].len(), own.len())
        }
        Some(_) => {}
        None => {
            let near = (0..GRID * GRID).filter(|&k| !lists[k].is_empty()).map(|k| ((k / GRID) as i64 - r as i64).abs().max(((k % GRID) as i64 - c as i64).abs())).min();
            crate::info!("native vis cell check: frame {frame} slot {slot}: camera cell ({r}, {c}) has no list (engine and here: {}), nearest cell with one {near:?} cells away", lists[r * GRID + c].is_empty());
        }
    }
}

/// [`cell_check`] is asked for.
pub fn cell_check_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_NATIVE_VIS_CELLCHECK").is_ok_and(|v| v.trim() == "1"))
}

/// `SR_NATIVE_VIS_NOLIST=last` (diagnostics, comparisons): a camera cell without a list keeps
/// the last list the view had (as before 2026-11-21) instead of [`nearby_list`].
pub fn nolist_last() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_NATIVE_VIS_NOLIST").is_ok_and(|v| v.trim() == "last"))
}

/// Per static: its bounding sphere touches the view (frustum with near and far planes).
pub fn on_screen(meta: &[StaticMeta], view: &View, ext: f32) -> Vec<bool> {
    let f = Frustum::of(view, ext);
    meta.iter().map(|m| f.sphere(m.center, m.radius)).collect()
}

/// Per static: the distance from the view's eye to its bounding sphere (0 inside it).
pub fn distances(meta: &[StaticMeta], view: &View) -> Vec<f32> {
    let p = view.camera.pos;
    meta.iter()
        .map(|m| {
            let d = ((m.center[0] - p[0]).powi(2) + (m.center[1] - p[1]).powi(2) + (m.center[2] - p[2]).powi(2)).sqrt();
            (d - m.radius as f64).max(0.0) as f32
        })
        .collect()
}

/// A view's frustum in world space terms: planes in view space, the camera to get there.
pub struct Frustum {
    pos: [f64; 3],
    rot: [f64; 9],
    /// `n . v + d >= 0` inside: left, right, top, bottom, near, far.
    planes: [[f64; 4]; 6],
}

impl Frustum {
    /// The view's frustum (`ext`: the widescreen extension the renderer widens a full-width
    /// viewport by).
    pub fn of(view: &View, ext: f32) -> Frustum {
        let c = &view.camera;
        let p = &c.proj;
        // the viewport as the renderer widens it (a full-width view covers the extension)
        let vp = view.viewport;
        let vp = if vp[0] <= 0.0 && vp[2] >= 640.0 { [-ext, vp[1], 640.0 + ext, vp[3]] } else { vp };
        let (cx, cy, sx, sy) = (p.center[0] as f64, p.center[1] as f64, p.scale[0] as f64, p.scale[1] as f64);
        // unflipped screen x = cx + sx x / z; the mirror's flip about the viewport's centre
        // maps the viewport onto itself, so the frustum is the same
        let (x0, x1) = (vp[0] as f64, vp[2] as f64);
        let (y0, y1) = (vp[1] as f64, vp[3] as f64);
        // x / z >= (x0 - cx) / sx  <=>  sx x - (x0 - cx) z >= 0 (sx > 0), and so on
        let norm = |v: [f64; 4]| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
            [v[0] / l, v[1] / l, v[2] / l, v[3] / l]
        };
        let planes = [
            norm([sx, 0.0, -(x0 - cx), 0.0]),
            norm([-sx, 0.0, x1 - cx, 0.0]),
            norm([0.0, sy, -(y0 - cy), 0.0]),
            norm([0.0, -sy, y1 - cy, 0.0]),
            [0.0, 0.0, 1.0, -(p.near as f64)],
            [0.0, 0.0, -1.0, p.far as f64],
        ];
        Frustum { pos: c.pos, rot: c.rot.map(|v| v as f64), planes }
    }

    /// A sphere touches the frustum.
    pub fn sphere(&self, center: [f64; 3], radius: f32) -> bool {
        let d = [center[0] - self.pos[0], center[1] - self.pos[1], center[2] - self.pos[2]];
        let r = &self.rot;
        let v = [0, 1, 2].map(|k| r[k * 3] * d[0] + r[k * 3 + 1] * d[1] + r[k * 3 + 2] * d[2]);
        let rad = radius as f64;
        self.planes.iter().all(|n| n[0] * v[0] + n[1] * v[1] + n[2] * v[2] + n[3] >= -rad)
    }
}

/// The fade state of one view's statics (0 = not drawn, 255 = drawn), per simulation step.
#[derive(Clone, Debug, Default)]
pub struct Fader {
    prev: Vec<u8>,
    cur: Vec<u8>,
}

impl Fader {
    /// One simulation step: on-screen statics move towards their target by 1 / FADE_STEPS,
    /// the others take it at once. The first step (or a new scene) starts at the targets.
    pub fn step(&mut self, target: &[bool], onscreen: &[bool]) {
        let fresh = self.cur.len() != target.len();
        if fresh {
            self.cur = target.iter().map(|&t| if t { 255 } else { 0 }).collect();
        }
        self.prev.clone_from(&self.cur);
        if fresh {
            return;
        }
        let d = 255u32.div_ceil(FADE_STEPS);
        for (i, v) in self.cur.iter_mut().enumerate() {
            let goal = if target[i] { 255u32 } else { 0 };
            let now = *v as u32;
            *v = if !onscreen[i] || goal == now {
                goal
            } else if goal > now {
                (now + d).min(255)
            } else {
                now.saturating_sub(d)
            } as u8;
        }
    }

    /// The values to draw with: the last step's, or between the last two steps at `alpha`
    /// (interpolated frames; a static that took its state at once keeps it). Empty before
    /// the first step.
    pub fn values(&self, alpha: Option<f32>) -> Vec<u8> {
        let d = 255u32.div_ceil(FADE_STEPS) as i32;
        match alpha {
            Some(a) if self.prev.len() == self.cur.len() => {
                let a = a.clamp(0.0, 1.0);
                self.prev
                    .iter()
                    .zip(&self.cur)
                    .map(|(&p, &c)| if (c as i32 - p as i32).abs() > d { c } else { (p as f32 + (c as f32 - p as f32) * a).round() as u8 })
                    .collect()
            }
            _ => self.cur.clone(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Backed by the world (races)

/// What one backing test result says about a static.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Verdict {
    /// No pixel of its base in the view (off screen, too small): no evidence.
    #[default]
    None,
    /// Its base lies in front of (or behind) what the first `n` passes drew.
    Backed(u8),
    /// Its base shows the sky, the background or the void.
    Unbacked,
    /// Between the thresholds, or not tested against the passes that could back it.
    Unsure,
    /// Drawn in pass 1 and not backed by pass 0: to be drawn in pass 2, where test 2 (which
    /// pass 1 can back) sees it.
    Demote,
}

/// [`MIN_SKY_PIXELS`] for a view: scaled by the area of its pixels in the world relative to
/// the main view's (the product of the focal lengths), at least 1 (`SR_NATIVE_VIS_SCALE=0`,
/// diagnostics: [`MIN_SKY_PIXELS`] in every view, as before 2026-11-21).
pub fn min_sky_pixels(view: &View) -> u32 {
    static SCALED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*SCALED.get_or_init(|| std::env::var("SR_NATIVE_VIS_SCALE").map_or(true, |v| v.trim() != "0")) {
        return MIN_SKY_PIXELS;
    }
    let s = view.camera.proj.scale;
    let k = (s[0].abs() * s[1].abs()) / (REFERENCE_SCALE[0] * REFERENCE_SCALE[1]);
    ((MIN_SKY_PIXELS as f32 * k).round() as u32).clamp(1, MIN_SKY_PIXELS.max(1) * 16)
}

/// One test level: backed, unbacked, undecided, or not tested; `min_sky`: the view's
/// [`min_sky_pixels`].
fn level_verdict(c: &BackingCounts, k: usize, min_sky: u32) -> Option<Verdict> {
    if c.tested & (1 << k) == 0 {
        return None;
    }
    let (v, s) = (c.visible[k], c.sky[k]);
    let f = s as f32 / v.max(1) as f32;
    Some(if f >= REJECT_SKY && s >= min_sky {
        // what shows of the base shows the sky
        Verdict::Unbacked
    } else if f <= ACCEPT_SKY && v > 0 && v as f32 >= VISIBLE_SHARE * c.area as f32 {
        Verdict::Backed(k as u8 + 1)
    } else {
        // mostly hidden behind what is drawn (it may float once in view), or between the
        // thresholds
        Verdict::Unsure
    })
}

/// The verdict of a static's counts (in the main view).
pub fn classify(c: &BackingCounts) -> Verdict {
    classify_in(c, MIN_SKY_PIXELS)
}

/// The verdict of a static's counts in a view whose [`min_sky_pixels`] is `min_sky`.
pub fn classify_in(c: &BackingCounts, min_sky: u32) -> Verdict {
    if c.area == 0 {
        return Verdict::None;
    }
    match (level_verdict(c, 0, min_sky), level_verdict(c, 1, min_sky)) {
        (Some(Verdict::Backed(l)), _) | (_, Some(Verdict::Backed(l))) => Verdict::Backed(l),
        (Some(_), None) => Verdict::Demote,
        (_, Some(Verdict::Unbacked)) => Verdict::Unbacked,
        _ => Verdict::Unsure,
    }
}

/// The floating detector: the base of a static drawn in pass `level` (1 or 2) shows the
/// sky in at least [`FLOATING_SKY`] of its visible pixels in the test against the passes
/// before it.
pub fn floating(c: &BackingCounts, level: u8) -> bool {
    let k = (level.clamp(1, 2) - 1) as usize;
    c.tested & (1 << k) != 0 && c.visible[k] > 0 && c.sky[k] as f32 >= FLOATING_SKY * c.visible[k] as f32
}

/// One view's sets (per static).
pub struct Sets<'a> {
    pub base: &'a [bool],
    /// Of the base: statics that leave it when the camera moves to a neighbouring track cell
    /// (drawn in pass 1 and tested, so that their state is known when they leave).
    pub at_risk: &'a [bool],
    pub candidate: &'a [bool],
    pub onscreen: &'a [bool],
    /// From the eye to the bounding sphere (world units).
    pub dist: &'a [f32],
    /// The view's [`min_sky_pixels`].
    pub min_sky: u32,
}

/// What a view's update did (diagnostics: `SR_NATIVE_VIS_LOG`).
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Statics on screen: in the base, candidates (not in the base), tested.
    pub base: u32,
    pub candidates: u32,
    pub tested: u32,
    /// Statics on screen drawn beyond the base (fade above 0), and those of them whose last
    /// result shows the base against the sky (the floating detector): (static, distance,
    /// sky, visible).
    pub extra: u32,
    pub floating: Vec<(u32, f32, u32, u32)>,
    /// Of the floating: drawn because they were accepted as candidates (the others were in
    /// the base and are kept while being tested).
    pub floating_accepted: u32,
    /// Floating statics already fading out (removed because unbacked: not in `floating`).
    pub floating_leaving: u32,
    /// Statics that started fading out while on screen: (static, distance).
    pub fade_outs: Vec<(u32, f32)>,
    /// Candidates that started being drawn.
    pub accepted: u32,
    /// Candidates on screen by their last result: no evidence, backed, unbacked, mostly
    /// hidden, undecided.
    pub verdicts: [u32; 5],
    /// The farthest drawn static on screen (distance to its sphere): of the base, of all.
    pub far_base: f32,
    pub far_all: f32,
    /// A result was applied.
    pub results: bool,
}

/// The rule's state for one view (main, second player, mirror).
#[derive(Clone, Debug, Default)]
pub struct Backed {
    /// Per static: drawn (the target of its fade), fade 0..1, the pass it is drawn in (0
    /// base, 1 or 2), consecutive backed / unbacked results, since when unbacked (steps),
    /// the last result.
    drawn: Vec<bool>,
    fade: Vec<f32>,
    level: Vec<u8>,
    on: Vec<u8>,
    off: Vec<u8>,
    off_since: Vec<f64>,
    last: Vec<BackingCounts>,
    /// Drawn because accepted as a candidate (not kept from the base).
    accepted: Vec<bool>,
    /// The time of the last update (simulation steps).
    t: Option<f64>,
    /// The newest result not applied yet, and the last applied tag.
    pending: Option<BackingResults>,
    applied: u64,
}

impl Backed {
    /// A result arrived (applied at the next update; an older one than what is kept is
    /// dropped).
    pub fn receive(&mut self, r: BackingResults) {
        if r.tag > self.applied && self.pending.as_ref().is_none_or(|p| p.tag < r.tag) {
            self.pending = Some(r);
        }
    }

    /// One drawn view at time `t` (simulation steps, fractional between them): apply the
    /// newest result, decide what is drawn, advance the fades.
    pub fn update(&mut self, sets: &Sets, t: f64) -> Report {
        let n = sets.base.len();
        let mut rep = Report::default();
        if self.drawn.len() != n {
            // a new scene: the base at once, nothing else
            *self = Backed {
                drawn: sets.base.to_vec(),
                fade: sets.base.iter().map(|&b| b as u8 as f32).collect(),
                level: vec![2; n],
                on: vec![0; n],
                off: vec![0; n],
                off_since: vec![0.0; n],
                last: vec![BackingCounts::default(); n],
                accepted: vec![false; n],
                t: Some(t),
                pending: None,
                applied: self.applied,
            };
        }
        let dt = self.t.map_or(0.0, |t0| (t - t0).max(0.0));
        self.t = Some(self.t.map_or(t, |t0| t0.max(t)));
        // the newest result
        if let Some(r) = self.pending.take() {
            self.applied = r.tag;
            rep.results = true;
            for &(i, c) in &r.statics {
                let i = i as usize;
                if i >= n {
                    continue;
                }
                self.last[i] = c;
                // a base static (at risk of leaving it) is tested once, against the base's
                // core: what it says counts as is
                let verdict = if sets.base[i] { level_verdict(&c, 0, sets.min_sky).filter(|_| c.area > 0).unwrap_or(Verdict::None) } else { classify_in(&c, sets.min_sky) };
                match verdict {
                    Verdict::Backed(l) => {
                        self.on[i] = self.on[i].saturating_add(1);
                        self.off[i] = 0;
                        if !sets.base[i] {
                            self.level[i] = l;
                        }
                    }
                    Verdict::Unbacked => {
                        if self.off[i] == 0 {
                            self.off_since[i] = t;
                        }
                        self.off[i] = self.off[i].saturating_add(1);
                        self.on[i] = 0;
                    }
                    Verdict::Demote if !sets.base[i] => self.level[i] = 2,
                    Verdict::Demote => {}
                    Verdict::Unsure => self.on[i] = 0,
                    Verdict::None => {}
                }
            }
        }
        // what is drawn
        for i in 0..n {
            let on_screen = sets.onscreen[i];
            if sets.base[i] {
                self.drawn[i] = true;
                self.accepted[i] = false;
                if sets.at_risk[i] {
                    // tested against the base's core (pass 1): known when it leaves
                    self.level[i] = 1;
                } else {
                    self.level[i] = 0;
                    self.off[i] = 0;
                }
                continue;
            }
            if self.level[i] == 0 {
                // left the base: drawn last, so that both tests see it
                self.level[i] = 2;
            }
            if self.drawn[i] {
                if !on_screen {
                    // nobody sees it: kept only as an accepted candidate
                    self.drawn[i] = sets.candidate[i] && self.on[i] > 0 && self.off[i] == 0;
                } else if (!self.accepted[i] || self.off[i] >= OFF_RESULTS && t - self.off_since[i] >= OFF_STEPS) && self.off[i] > 0 && (sets.dist[i] as f64) > NEAR_KEEP {
                    // floating for a while, and not near: fades out
                    self.drawn[i] = false;
                    rep.fade_outs.push((i as u32, sets.dist[i]));
                }
            } else if sets.candidate[i] && on_screen && self.on[i] >= ON_RESULTS {
                self.drawn[i] = true;
                self.accepted[i] = true;
                rep.accepted += 1;
            }
        }
        // fades: on screen in over FADE_STEPS and out over FADE_OUT_STEPS, off screen at once
        let up = (dt / FADE_STEPS as f64) as f32;
        let down = (dt / FADE_OUT_STEPS as f64) as f32;
        for i in 0..n {
            let goal = self.drawn[i] as u8 as f32;
            let f = &mut self.fade[i];
            *f = if !sets.onscreen[i] {
                goal
            } else if *f < goal {
                (*f + up).min(goal)
            } else {
                (*f - down).max(goal)
            };
        }
        // statistics
        for i in 0..n {
            if !sets.onscreen[i] {
                continue;
            }
            let drawn = self.fade[i] > 0.0;
            if sets.base[i] {
                rep.base += 1;
                if drawn {
                    rep.far_base = rep.far_base.max(sets.dist[i]);
                }
            } else {
                rep.candidates += sets.candidate[i] as u32;
                if sets.candidate[i] {
                    let c = &self.last[i];
                    let k = match classify_in(c, sets.min_sky) {
                        Verdict::None => 0,
                        Verdict::Backed(_) => 1,
                        Verdict::Unbacked => 2,
                        _ if c.visible.iter().zip(c.sky.iter()).all(|(&v, _)| (v as f32) < VISIBLE_SHARE * c.area as f32) => 3,
                        _ => 4,
                    };
                    rep.verdicts[k] += 1;
                }
                if drawn {
                    rep.extra += 1;
                    let c = &self.last[i];
                    if floating(c, self.level[i]) {
                        if self.drawn[i] {
                            let k = (self.level[i].clamp(1, 2) - 1) as usize;
                            rep.floating.push((i as u32, sets.dist[i], c.sky[k], c.visible[k]));
                            rep.floating_accepted += self.accepted[i] as u32;
                        } else {
                            rep.floating_leaving += 1;
                        }
                    }
                }
            }
            if drawn {
                rep.far_all = rep.far_all.max(sets.dist[i]);
            }
        }
        rep
    }

    /// The frame's `static_fade` (0..255) and `static_backing` (pass and tests): the base in
    /// pass 0; drawn statics beyond it in their pass, tested against the passes before it;
    /// candidates on screen not drawn tested against both.
    pub fn output(&self, sets: &Sets) -> (Vec<u8>, Vec<u8>, u32) {
        let n = self.drawn.len();
        let mut fade = Vec::with_capacity(n);
        let mut backing = Vec::with_capacity(n);
        let mut tested = 0;
        for i in 0..n {
            let f = self.fade[i];
            fade.push(if f > 0.0 { ((f * 255.0).round() as u8).max(1) } else { 0 });
            let b = if sets.base[i] {
                if sets.at_risk[i] { 1 | if sets.onscreen[i] { BACKING_TEST1 } else { 0 } } else { 0 }
            } else if f > 0.0 || self.drawn[i] {
                let l = self.level[i].clamp(1, 2);
                l | if !sets.onscreen[i] {
                    0
                } else if l == 1 {
                    BACKING_TEST1
                } else {
                    BACKING_TEST1 | BACKING_TEST2
                }
            } else if sets.candidate[i] && sets.onscreen[i] {
                2 | BACKING_TEST1 | BACKING_TEST2
            } else {
                0
            };
            tested += (b & (BACKING_TEST1 | BACKING_TEST2) != 0) as u32;
            backing.push(b);
        }
        (fade, backing, tested)
    }

    /// The last result of a static (diagnostics).
    pub fn last(&self, i: usize) -> Option<(BackingCounts, u8, bool)> {
        Some((*self.last.get(i)?, *self.level.get(i)?, *self.drawn.get(i)?))
    }
}

// ---------------------------------------------------------------------------------------
// Edited lists (the visibility editor, `docs/05-visibility-editor.md`)

/// The races' rule while the visibility editor's edits are active (`crate::visedit::edits_active`:
/// in the tool, or for a track with saved edits), for one view: exactly the statics of the cells
/// in the camera cell's effective list inside the frustum box ([`StaticCells::in_list`]), the
/// target of each static's fade; no candidates, no GPU test. A static that starts being drawn
/// while on screen fades in over [`FADE_STEPS`] (as [`Fader`] and [`Backed`]); one that stops
/// while on screen fades out over [`FADE_OUT_STEPS`] (0.12 s, softer than a pop when the camera
/// changes cell; an edit shows at once all the same); off screen both take their state at once.
/// The fades run on the races' clock of simulation steps (interpolated frames between steps).
#[derive(Clone, Debug, Default)]
pub struct Edited {
    fade: Vec<f32>,
    /// The time of the last update (simulation steps).
    t: Option<f64>,
}

impl Edited {
    /// One drawn view at time `t` (simulation steps): the fades towards `target`; returns the
    /// frame's `static_fade`. The first update (or a new scene) starts at the targets.
    pub fn update(&mut self, target: &[bool], onscreen: &[bool], t: f64) -> Vec<u8> {
        if self.fade.len() != target.len() {
            self.fade = target.iter().map(|&b| b as u8 as f32).collect();
            self.t = Some(t);
        }
        let dt = self.t.map_or(0.0, |t0| (t - t0).max(0.0));
        self.t = Some(self.t.map_or(t, |t0| t0.max(t)));
        let up = (dt / FADE_STEPS as f64) as f32;
        let down = (dt / FADE_OUT_STEPS as f64) as f32;
        self.fade
            .iter_mut()
            .zip(target)
            .zip(onscreen)
            .map(|((f, &drawn), &on)| {
                let goal = drawn as u8 as f32;
                *f = if !on {
                    goal
                } else if *f < goal {
                    (*f + up).min(goal)
                } else {
                    (*f - down).max(goal)
                };
                if *f > 0.0 { ((*f * 255.0).round() as u8).max(1) } else { 0 }
            })
            .collect()
    }
}

/// Blinking of the cell under the visibility editor's mouse (`crate::visedit::highlight`):
/// full cycles per second (real time; virtual in deterministic runs).
pub const BLINK_HZ: f64 = 3.5;

/// The half period of the blinking at time `secs` in which the highlighted cell's statics show
/// the opposite of their state (the other half: as drawn).
pub fn blink_inverted(secs: f64) -> bool {
    ((secs * BLINK_HZ * 2.0).floor() as i64).rem_euclid(2) == 0
}

/// The blinking's inverted half period for the statics `cell` (of one cell): drawn ones (any fade)
/// hidden, hidden ones fully drawn (what a hidden sector holds).
pub fn invert(fade: &mut [u8], cell: &[u32]) {
    for &i in cell {
        if let Some(f) = fade.get_mut(i as usize) {
            *f = if *f > 0 { 0 } else { 255 };
        }
    }
}

/// The comparison rule (`SR_NATIVE_VIS=lists`): the wave-2 fades as drawn, with every drawn
/// static beyond the designers' list on screen tested (the floating detector only).
pub fn lists_backing(fade: &[u8], sets: &Sets) -> Vec<u8> {
    (0..fade.len())
        .map(|i| {
            if sets.base[i] || fade[i] == 0 {
                0
            } else {
                2 | if sets.onscreen[i] { BACKING_TEST1 | BACKING_TEST2 } else { 0 }
            }
        })
        .collect()
}

/// The floating detector for the comparison rule: drawn statics beyond the designers' list
/// on screen whose results show the base against the sky (pass 2 tests).
pub fn lists_report(fade: &[u8], sets: &Sets, r: Option<&BackingResults>) -> Report {
    let mut rep = Report { results: r.is_some(), ..Default::default() };
    for i in 0..fade.len() {
        if !sets.onscreen[i] {
            continue;
        }
        if sets.base[i] {
            rep.base += 1;
            if fade[i] > 0 {
                rep.far_base = rep.far_base.max(sets.dist[i]);
            }
        } else if fade[i] > 0 {
            rep.extra += 1;
        }
        if fade[i] > 0 {
            rep.far_all = rep.far_all.max(sets.dist[i]);
        }
        rep.candidates += (!sets.base[i] && sets.candidate[i]) as u32;
    }
    if let Some(r) = r {
        for &(i, c) in &r.statics {
            let iu = i as usize;
            if iu < fade.len() && fade[iu] > 0 && sets.onscreen[iu] && !sets.base[iu] && floating(&c, 2) {
                rep.floating.push((i, sets.dist[iu], c.sky[1], c.visible[1]));
            }
        }
        rep.tested = r.statics.len() as u32;
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::frame::{GameCamera, ViewMap, camera};

    fn meta(cell: (u16, u16), grounded: bool, center: [f64; 3]) -> StaticMeta {
        StaticMeta { cell, grounded, center, radius: 100.0 }
    }

    fn view() -> View {
        // heading 0: looking along world -z from the origin
        let cam = GameCamera { angles: [0.0; 3], pos: [0.0, 0.0, 0.0] };
        let map = ViewMap::race_main(0.0);
        View { camera: camera(&cam, &map, 50000.0), viewport: map.viewport(), mirror: false, clear: None, fog: None, sky: None }
    }

    const WIDE: CellBox = CellBox { row: 0, col: 0, rows: 256, cols: 256 };

    #[test]
    fn listed_cells_and_grounded_statics() {
        let m = vec![
            meta((128, 128), false, [0.0, 0.0, -5000.0]),     // listed, not grounded
            meta((127, 128), false, [0.0, 0.0, -9000.0]),     // not listed, floating: hidden
            meta((127, 128), true, [0.0, 0.0, -9000.0]),      // not listed, grounded, in view
            meta((127, 128), true, [0.0, 0.0, 9000.0]),       // grounded, behind the camera
            meta((100, 128), true, [0.0, 0.0, -70000.0]),     // grounded, beyond the far plane
            meta((128, 128), false, [0.0, 0.0, 9000.0]),      // listed: drawn wherever it is
            meta((129, 128), false, [0.0, 0.0, -3000.0]),     // listed, outside the frustum box
        ];
        let s = StaticCells::new(&m);
        let cells = GameCells { list: Some(vec![128 << 16 | 128, 129 << 16 | 128]), own: None, frustum_box: CellBox { row: 120, col: 120, rows: 9, cols: 16 } };
        let t = s.targets(&m, &cells, &view(), 0.0, true);
        assert_eq!(t, vec![true, false, true, false, false, true, false]);
        // the game's cells only (diagnostics)
        assert_eq!(s.targets(&m, &cells, &view(), 0.0, false), vec![true, false, false, false, false, true, false]);
        // no list: the frustum box
        let t = s.targets(&m, &GameCells { list: None, own: None, frustum_box: CellBox { row: 127, col: 120, rows: 1, cols: 10 } }, &view(), 0.0, true);
        assert_eq!(t, vec![false, true, true, true, false, false, false]);
    }

    #[test]
    fn frustum_follows_the_widescreen_extension() {
        // view (1800, 0, 1128): screen x = 319 + 256 * 1800 / 1128 = 727.5, beyond 640 by
        // more than its radius at 4:3, inside the view widened by e = 107 (747)
        let m = vec![meta((0, 0), true, [-1800.0, 0.0, -1000.0])];
        let s = StaticCells::new(&m);
        let list = GameCells { list: Some(vec![]), own: None, frustum_box: WIDE };
        assert_eq!(s.targets(&m, &list, &view(), 0.0, true), vec![false]);
        assert_eq!(s.targets(&m, &list, &view(), 107.0, true), vec![true]);
    }

    #[test]
    fn fades_on_screen_snaps_off_screen() {
        let mut f = Fader::default();
        f.step(&[true, false], &[true, true]);
        assert_eq!(f.values(None), vec![255, 0]);
        // a steady state stays (a first version stepped away from 255 and back)
        for _ in 0..3 {
            f.step(&[true, false], &[true, true]);
            assert_eq!(f.values(None), vec![255, 0]);
            assert_eq!(f.values(Some(0.5)), vec![255, 0]);
        }
        // both change: the on-screen one fades, the off-screen one snaps
        f.step(&[false, true], &[true, false]);
        assert_eq!(f.values(None), vec![255 - 32, 255]);
        // between the steps: the fade halfway, the snap kept
        assert_eq!(f.values(Some(0.5)), vec![255 - 16, 255]);
        for _ in 0..FADE_STEPS {
            f.step(&[false, true], &[true, true]);
        }
        assert_eq!(f.values(None), vec![0, 255]);
        // fading in takes FADE_STEPS steps
        let mut n = 0;
        while f.values(None)[0] < 255 {
            f.step(&[true, true], &[true, true]);
            n += 1;
        }
        assert_eq!(n, FADE_STEPS);
    }

    // ---- backed by the world ----

    /// Counts of a base `area` pixels large, `visible` of them not behind what is drawn,
    /// `sky` of those over nothing, tested at both levels (the same at both).
    fn counts(area: u32, visible: u32, sky: u32) -> BackingCounts {
        BackingCounts { area, visible: [visible, visible], sky: [sky, sky], tested: 3 }
    }

    fn results(tag: u64, list: &[(u32, BackingCounts)]) -> BackingResults {
        BackingResults { tag, view: 0, statics: list.to_vec(), dynamics: Vec::new() }
    }

    #[test]
    fn a_cell_without_a_list_takes_the_nearest_list_and_the_last() {
        let e = |r: u32, c: u32| r << 16 | c;
        let mut lists = vec![Vec::new(); GRID * GRID];
        // (10, 10) has no list; (10, 11) and (9, 9) next to it do, (10, 13) farther away
        lists[10 * GRID + 11] = vec![e(10, 11), e(10, 12)];
        lists[9 * GRID + 9] = vec![e(9, 9), e(8, 9)];
        lists[10 * GRID + 13] = vec![e(10, 13)];
        // the camera near (10, 11)'s side of its cell: (10, 11)'s list and its own cell
        assert_eq!(nearby_list(&lists, (10, 10), (10.5, 10.9), None), Some(vec![e(10, 10), e(10, 11), e(10, 12)]));
        // near (9, 9)'s corner: (9, 9)'s, merged with the last list
        assert_eq!(nearby_list(&lists, (10, 10), (10.1, 10.1), Some(&[e(7, 7)])), Some(vec![e(7, 7), e(8, 9), e(9, 9), e(10, 10)]));
        // two cells away: found in the second ring
        assert_eq!(nearby_list(&lists, (10, 15), (10.5, 15.5), None), Some(vec![e(10, 13), e(10, 15)]));
        // nothing within NOLIST_REACH: the last list, or none
        assert_eq!(nearby_list(&lists, (100, 100), (100.5, 100.5), Some(&[e(7, 7)])), Some(vec![e(7, 7), e(100, 100)]));
        assert_eq!(nearby_list(&lists, (100, 100), (100.5, 100.5), None), None);
        assert_eq!(nearby_list(&[], (10, 10), (10.5, 10.5), None), None);
    }

    #[test]
    fn the_mirrors_pixels_count_for_more() {
        // the main view (any widescreen extension) keeps MIN_SKY_PIXELS; the mirror's focal
        // lengths are half the main view's: a quarter of the pixel area, 2 -> 1
        let main = view();
        assert_eq!(min_sky_pixels(&main), MIN_SKY_PIXELS);
        let map = ViewMap::mirror();
        let cam = GameCamera { angles: [0.0; 3], pos: [0.0; 3] };
        let mirror = View { camera: camera(&cam, &map, 50000.0), viewport: map.viewport(), mirror: true, clear: None, fog: None, sky: None };
        assert_eq!(min_sky_pixels(&mirror), 1);
        // a base of one pixel showing the sky: undecided in the main view, unbacked in the
        // mirror (so a static kept after leaving the mirror's base goes)
        let one = counts(1, 1, 1);
        assert_eq!(classify_in(&one, min_sky_pixels(&main)), Verdict::Unsure);
        assert_eq!(classify_in(&one, min_sky_pixels(&mirror)), Verdict::Unbacked);
        let mut w = World::new(1);
        w.base[0] = true;
        let mut b = Backed::default();
        b.update(&w.sets(), 0.0);
        w.base[0] = false;
        b.update(&w.sets(), 1.0);
        b.receive(results(1, &[(0, one)]));
        let sets = Sets { min_sky: 1, ..w.sets() };
        assert_eq!(b.update(&sets, 2.0).fade_outs.len(), 1, "the mirror: floating at one pixel, fades out");
        let mut b = Backed::default();
        w.base[0] = true;
        b.update(&w.sets(), 0.0);
        w.base[0] = false;
        b.update(&w.sets(), 1.0);
        b.receive(results(1, &[(0, one)]));
        assert!(b.update(&w.sets(), 2.0).fade_outs.is_empty(), "the main view: one pixel is no evidence");
    }

    #[test]
    fn verdicts_of_the_counts() {
        assert_eq!(classify(&counts(0, 0, 0)), Verdict::None, "off screen");
        assert_eq!(classify(&counts(100, 100, 0)), Verdict::Backed(1));
        assert_eq!(classify(&counts(100, 100, 10)), Verdict::Backed(1), "at the threshold");
        assert_eq!(classify(&counts(100, 100, 100)), Verdict::Unbacked);
        assert_eq!(classify(&counts(100, 100, 20)), Verdict::Unsure, "between the thresholds");
        assert_eq!(classify(&counts(100, 0, 0)), Verdict::Unsure, "hidden: no proof");
        assert_eq!(classify(&counts(100, 40, 0)), Verdict::Unsure, "mostly hidden");
        assert_eq!(classify(&counts(100, 40, 40)), Verdict::Unbacked, "what shows is sky");
        assert_eq!(classify(&counts(100, 1, 1)), Verdict::Unsure, "one pixel is no evidence");
        // backed only by the first pass's world plus pass 1
        let c = BackingCounts { area: 100, visible: [100, 100], sky: [100, 0], tested: 3 };
        assert_eq!(classify(&c), Verdict::Backed(2));
        // drawn in pass 1 (tested once) and not backed by pass 0: to pass 2
        assert_eq!(classify(&BackingCounts { tested: 1, ..counts(100, 100, 100) }), Verdict::Demote);
        assert!(floating(&counts(100, 100, 60), 2) && !floating(&counts(100, 100, 40), 1));
    }

    struct World {
        base: Vec<bool>,
        at_risk: Vec<bool>,
        candidate: Vec<bool>,
        onscreen: Vec<bool>,
        dist: Vec<f32>,
    }

    impl World {
        fn new(n: usize) -> World {
            World { base: vec![false; n], at_risk: vec![false; n], candidate: vec![false; n], onscreen: vec![true; n], dist: vec![50_000.0; n] }
        }
        fn sets(&self) -> Sets<'_> {
            Sets { base: &self.base, at_risk: &self.at_risk, candidate: &self.candidate, onscreen: &self.onscreen, dist: &self.dist, min_sky: MIN_SKY_PIXELS }
        }
    }

    #[test]
    fn candidates_are_drawn_when_backed_and_fade_in() {
        let mut w = World::new(3);
        w.base[0] = true;
        w.candidate[1] = true;
        w.candidate[2] = true;
        let mut b = Backed::default();
        b.update(&w.sets(), 0.0);
        let (fade, backing, tested) = b.output(&w.sets());
        assert_eq!(fade, vec![255, 0, 0], "the base at once, nothing else");
        assert_eq!(backing, vec![0, 2 | BACKING_TEST1 | BACKING_TEST2, 2 | BACKING_TEST1 | BACKING_TEST2]);
        assert_eq!(tested, 2);
        // 1 backed, 2 floating: one result is not enough ...
        let r = |tag| results(tag, &[(1, counts(100, 100, 0)), (2, counts(100, 100, 100))]);
        b.receive(r(1));
        b.update(&w.sets(), 1.0);
        assert_eq!(b.output(&w.sets()).0, vec![255, 0, 0]);
        // ... two are: 1 starts fading in, in pass 1, tested against pass 0
        b.receive(r(2));
        let rep = b.update(&w.sets(), 2.0);
        assert_eq!(rep.accepted, 1);
        let (fade, backing, _) = b.output(&w.sets());
        assert_eq!((fade[1], backing[1]), (32, 1 | BACKING_TEST1), "drawn from now on, one step of its fade in");
        assert_eq!(fade[2], 0);
        // the fade takes FADE_STEPS steps; between steps it is in between
        b.update(&w.sets(), 2.5);
        let half = b.output(&w.sets()).0[1];
        assert!(half > 0 && half < 255, "{half}");
        b.update(&w.sets(), 2.0 + FADE_STEPS as f64);
        assert_eq!(b.output(&w.sets()).0, vec![255, 255, 0]);
        // an older result than the one applied is ignored
        b.receive(r(1));
        assert!(!b.update(&w.sets(), 11.0).results);
    }

    #[test]
    fn near_objects_on_screen_are_never_faded_out() {
        // 0 near, 1 far: both in the base, then not (kept while on screen), then floating
        let mut w = World::new(3);
        w.base = vec![true, true, true];
        w.dist = vec![NEAR_KEEP as f32 - 1.0, 40_000.0, 40_000.0];
        let mut b = Backed::default();
        b.update(&w.sets(), 0.0);
        w.base = vec![false; 3];
        b.update(&w.sets(), 1.0);
        let (fade, backing, _) = b.output(&w.sets());
        assert_eq!(fade, vec![255, 255, 255], "kept while on screen");
        assert_eq!(backing[0], 2 | BACKING_TEST1 | BACKING_TEST2, "tested, drawn after the tests");
        // 0 and 1 float, 2 is backed: the far floating one fades out at once (it was never
        // accepted), the near one stays
        let fl = counts(100, 100, 100);
        b.receive(results(1, &[(0, fl), (1, fl), (2, counts(100, 100, 0))]));
        let rep = b.update(&w.sets(), 2.0);
        assert_eq!(rep.fade_outs.iter().map(|f| f.0).collect::<Vec<_>>(), vec![1]);
        assert_eq!(rep.floating.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0], "near and floating: kept (and counted)");
        for (k, t) in [(2u64, 3.0), (3, 4.0), (4, 5.0), (5, 6.0), (6, 7.0)] {
            b.receive(results(k, &[(0, fl), (1, fl), (2, counts(100, 100, 0))]));
            b.update(&w.sets(), t);
        }
        let fade = b.output(&w.sets()).0;
        assert_eq!((fade[0], fade[1], fade[2]), (255, 0, 255), "near kept, far floating gone after FADE_OUT_STEPS, backed kept");
        // off screen: what is not an accepted candidate snaps off
        w.onscreen = vec![false; 3];
        b.update(&w.sets(), 8.0);
        assert_eq!(b.output(&w.sets()).0, vec![0, 0, 0]);
    }

    #[test]
    fn accepted_candidates_leave_after_several_unbacked_results() {
        let mut w = World::new(1);
        w.candidate[0] = true;
        let mut b = Backed::default();
        b.update(&w.sets(), 0.0);
        for k in 1..=2 {
            b.receive(results(k, &[(0, counts(50, 50, 0))]));
            b.update(&w.sets(), k as f64);
        }
        b.update(&w.sets(), 20.0);
        assert_eq!(b.output(&w.sets()).0, vec![255]);
        // one unbacked result keeps it (hysteresis), the second one later removes it
        b.receive(results(3, &[(0, counts(50, 50, 50))]));
        assert!(b.update(&w.sets(), 21.0).fade_outs.is_empty());
        b.receive(results(4, &[(0, counts(50, 50, 50))]));
        assert_eq!(b.update(&w.sets(), 22.0).fade_outs.len(), 1);
        // a hidden base (no evidence) keeps a drawn object
        let mut b2 = Backed::default();
        b2.update(&w.sets(), 0.0);
        for k in 1..=2 {
            b2.receive(results(k, &[(0, counts(50, 50, 0))]));
            b2.update(&w.sets(), k as f64);
        }
        for k in 3..=10 {
            b2.receive(results(k, &[(0, counts(50, 0, 0))]));
            assert!(b2.update(&w.sets(), k as f64).fade_outs.is_empty());
        }
    }

    #[test]
    fn statics_at_risk_are_tested_in_the_base_and_leave_at_once_when_floating() {
        let mut w = World::new(2);
        w.base = vec![true, true];
        w.at_risk = vec![true, true];
        let mut b = Backed::default();
        b.update(&w.sets(), 0.0);
        assert_eq!(b.output(&w.sets()).1, vec![1 | BACKING_TEST1, 1 | BACKING_TEST1], "pass 1, tested against the base's core");
        // 0 floats against the core, 1 is backed; drawn regardless while in the base
        b.receive(results(1, &[(0, BackingCounts { tested: 1, ..counts(80, 80, 80) }), (1, BackingCounts { tested: 1, ..counts(80, 80, 0) })]));
        b.update(&w.sets(), 1.0);
        assert_eq!(b.output(&w.sets()).0, vec![255, 255]);
        // both leave the base: 0 fades out at once, 1 stays (in pass 1)
        w.base = vec![false, false];
        w.at_risk = vec![false, false];
        let rep = b.update(&w.sets(), 2.0);
        assert_eq!(rep.fade_outs.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0]);
        assert_eq!(b.output(&w.sets()).1[1], 1 | BACKING_TEST1);
    }

    // ---- edited lists (the visibility editor) ----

    #[test]
    fn edited_lists_draw_exactly_the_list_inside_the_box() {
        let m = vec![
            meta((10, 10), false, [0.0, 0.0, -5000.0]),
            meta((10, 11), false, [0.0, 0.0, -9000.0]),
            meta((10, 11), false, [0.0, 0.0, -9500.0]),
            meta((12, 30), false, [0.0, 0.0, -30000.0]), // in the list, outside the box
            meta((11, 11), false, [0.0, 0.0, -7000.0]),  // in the box, not in the list
        ];
        let s = StaticCells::new(&m);
        let e = |r: u32, c: u32| r << 16 | c;
        let boxed = CellBox { row: 8, col: 8, rows: 6, cols: 6 };
        // the effective list is sorted (visedit::Edits::effective)
        let list = vec![e(10, 10), e(10, 11), e(12, 30)];
        assert_eq!(s.in_list(m.len(), &list, &boxed), vec![true, true, true, false, false]);
        assert_eq!(s.in_list(m.len(), &list, &WIDE), vec![true, true, true, true, false]);
        assert_eq!(s.in_list(m.len(), &[], &WIDE), vec![false; 5]);
        assert_eq!(s.in_cell((10, 11)), &[1, 2]);
        assert!(s.in_cell((50, 50)).is_empty());
    }

    #[test]
    fn edited_fades_in_on_screen_out_quickly_and_snap_off_screen() {
        let mut e = Edited::default();
        // the first update: the targets at once
        assert_eq!(e.update(&[true, false, false], &[true, true, false], 10.0), vec![255, 0, 0]);
        // 1 is added on screen: fades in over FADE_STEPS; 2 added off screen: at once; 0
        // removed on screen: fades out over FADE_OUT_STEPS
        let f = e.update(&[false, true, true], &[true, true, false], 11.0);
        assert_eq!(f, vec![(255.0 * (1.0 - 1.0 / FADE_OUT_STEPS as f32)).round() as u8, 32, 255]);
        // between steps (an interpolated frame) in between, and time never runs back
        let f = e.update(&[false, true, true], &[true, true, false], 11.5);
        assert!(f[1] > 32 && f[1] < 64, "{f:?}");
        assert_eq!(e.update(&[false, true, true], &[true, true, false], 11.2), f);
        let f = e.update(&[false, true, true], &[true, true, false], 11.0 + FADE_OUT_STEPS as f64);
        assert_eq!(f[0], 0, "gone after FADE_OUT_STEPS");
        assert!(f[1] > 0 && f[1] < 255);
        let f = e.update(&[false, true, true], &[true, true, false], 11.0 + FADE_STEPS as f64);
        assert_eq!(f, vec![0, 255, 255], "in after FADE_STEPS");
        // leaving the screen while fading: at once
        e.update(&[true, true, true], &[true, true, true], 20.0);
        assert_eq!(e.update(&[true, true, true], &[false, true, true], 20.0)[0], 255);
        // a new scene (another number of statics): the targets at once
        assert_eq!(e.update(&[false, true], &[true, true], 30.0), vec![0, 255]);
    }

    #[test]
    fn blinking_inverts_the_highlighted_cell_at_three_and_a_half_hertz() {
        // half periods of 1 / 7 s: inverted, as drawn, inverted ...
        assert!(blink_inverted(0.0) && blink_inverted(0.14));
        assert!(!blink_inverted(0.15) && !blink_inverted(0.28));
        assert!(blink_inverted(0.29));
        let changes = (0..7000).filter(|&k| blink_inverted(k as f64 / 1000.0) != blink_inverted((k + 1) as f64 / 1000.0)).count();
        assert_eq!(changes, 49, "7 changes per second");
        // drawn (any fade) -> hidden, hidden -> fully drawn; other cells unchanged
        let mut f = vec![255, 0, 40, 255, 0];
        invert(&mut f, &[0, 1, 2, 99]);
        assert_eq!(f, vec![0, 255, 0, 255, 0]);
    }

    #[test]
    fn base_candidates_and_cells_at_risk() {
        // cells (10, 10) camera, (10, 11) and (10, 12) track cells ahead, (20, 20) far away
        let m = vec![
            meta((10, 10), false, [0.0, 0.0, -5000.0]),
            meta((10, 11), false, [0.0, 0.0, -9000.0]),
            meta((10, 12), false, [0.0, 0.0, -20000.0]),
            meta((20, 20), false, [0.0, 0.0, -30000.0]),
            meta((10, 13), false, [0.0, 0.0, 9000.0]),
        ];
        let s = StaticCells::new(&m);
        let e = |r: u32, c: u32| r << 16 | c;
        let mut lists = vec![Vec::new(); GRID * GRID];
        lists[10 * GRID + 10] = vec![e(10, 10), e(10, 11)];
        lists[10 * GRID + 11] = vec![e(10, 10), e(10, 11), e(10, 12)];
        lists[10 * GRID + 12] = vec![e(10, 12), e(20, 20)];
        let cells = GameCells { list: None, own: Some(lists[10 * GRID + 10].clone()), frustum_box: CellBox { row: 0, col: 0, rows: 256, cols: 256 } };
        let base = s.base(m.len(), &cells);
        assert_eq!(base, vec![true, true, false, false, false]);
        // the lists of the track cells in view name (10, 12) and (20, 20); (10, 13) is no
        // track cell's
        let c = s.candidates(m.len(), &cells, &lists, &view(), 0.0);
        assert_eq!(c, vec![true, true, true, true, false]);
        // at risk: (10, 10) is not in the list of the neighbour (10, 11)... it is; nothing of
        // the base leaves when the camera moves to (10, 11)
        assert_eq!(s.at_risk(&base, (10, 10), &lists), vec![false; 5]);
        // from (10, 11): its neighbour (10, 12) does not list (10, 10) or (10, 11), its
        // neighbour (10, 10) not (10, 12)
        let cells11 = GameCells { own: Some(lists[10 * GRID + 11].clone()), ..cells.clone() };
        let base11 = s.base(m.len(), &cells11);
        assert_eq!(base11, vec![true, true, true, false, false]);
        assert_eq!(s.at_risk(&base11, (10, 11), &lists), vec![true, true, true, false, false]);
        // no list: the frustum box (the engine's own rule)
        let boxed = GameCells { own: None, ..cells };
        assert!(s.base(m.len(), &boxed).iter().all(|&b| b));
    }
}
