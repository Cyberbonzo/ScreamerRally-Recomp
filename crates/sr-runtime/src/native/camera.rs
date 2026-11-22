//! Native renderer: camera collision (`docs/re/native/camera.md`). The chase cameras of the
//! game pass behind walls and cut through them with their near plane (the original does the
//! same); the native renderer draws the main view from a corrected camera instead:
//!
//! * **Pull-in**: the segment from the followed car's focus point (its origin raised
//!   [`FOCUS_HEIGHT`]) to the game's eye is tested against the world's solid triangles
//!   (statics; decals, blended, colour-keyed and tiny objects left out; only surfaces
//!   facing the car). The drawn eye moves along that segment to stay [`MARGIN`] units in
//!   front of the first hit, never nearer the focus than the car's box allows; the amount
//!   is smoothed per simulation step (fast in, slow out; never leaving the eye behind the
//!   wall) and interpolated for the extra frames.
//! * **Near plane**: when solid geometry facing the eye lies between the eye and the
//!   game's near plane (128 units, `near.md`) inside the view, the drawn near plane moves
//!   in front of it (down to [`NEAR_MIN`]), so a wall beside the camera is drawn instead
//!   of cut open; the car's box keeps it from cutting the car when the camera is pulled in.
//!
//! Only the drawn camera changes (`View::camera` of the native view): the game's camera
//! structure, its smoothing state and the simulation are not touched. Applied in the chase
//! modes (F2..F4: modes 4, 6, 3) of the main views; the rear-view mirror gets the near plane
//! only, and only while the player's car is not between its eye and the game's plane; not
//! the bumper / in-car views, the attract mode's cameras or the menus. Setting
//! `graphics.camera_collision` (default on); `SR_CAMERA_COLLISION=0|1` overrides it,
//! `SR_CAMERA_NEAR=0` keeps the game's near plane, `SR_CAMERA_MIRROR=0` leaves the mirror
//! alone, `SR_CAMERA_SOLID=all|keyed` (diagnostics) counts colour-keyed / tiny objects as
//! obstacles too, `SR_CAMERA_PROBE=N` logs the camera objects and the followed car.
//!
//! The world's triangles are put once per race into a uniform grid over (x, z)
//! ([`World`]); a query looks at the cells under the segment or the near slab only.
//! `SR_CAMERA_LOG=<file>` (diagnostics): one line per race view drawn (mode -1 = the
//! mirror), with the game's and the drawn camera's state (hit, pull, near plane, the eye
//! behind a wall, the near plane cutting solid geometry, clearance, cost).

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};

use super::extract::{SceneBuild, StaticMeta};
use super::frame::GameCamera;
use super::scene::{Camera, Layer, SceneData, Transparency, View};
use super::transforms::rotation;
use crate::Machine;
use crate::mem;

// ---------------------------------------------------------------------------------------
// Parameters

/// The focus point: the followed car's origin (at road level) raised by this much (world
/// up), about the middle of the car's height (the cars are about 300 units tall).
pub const FOCUS_HEIGHT: f64 = 150.0;
/// The drawn eye stays this far in front of the first wall along the focus-eye segment.
pub const MARGIN: f64 = 64.0;
/// The eye never comes nearer the focus than where the segment leaves the car's box plus
/// this much (the car is not entered).
pub const CAR_CLEARANCE: f64 = 60.0;
/// Pull-in smoothing: time constants of moving in (an obstacle appears) and out (it is
/// gone), seconds; applied once per simulation step.
pub const TAU_IN: f64 = 0.05;
pub const TAU_OUT: f64 = 0.4;
/// The game's near plane (view depth from the eye, `frame::NEAR`).
pub const NEAR_GAME: f64 = 128.0;
/// The drawn near plane comes no nearer than this, and stays this far in front of the
/// nearest solid geometry in the view.
pub const NEAR_MIN: f64 = 16.0;
pub const NEAR_MARGIN: f64 = 8.0;
/// Objects whose bounding sphere is smaller are not obstacles (posts, signs).
pub const TINY_RADIUS: f32 = 120.0;
/// Grid cell (world units, x and z).
const CELL: f64 = 1024.0;
/// A larger move of the game's camera between two steps is a cut (as `crate::interp`).
const MAX_CAMERA_MOVE: f64 = 3000.0;

// Link addresses
/// The views' camera objects (`[1296bc + 4 v]`): +AE4h camera mode, +ADCh the followed
/// car's record.
const VIEW_CAMERAS: u32 = 0x1296bc;
const CAMERA_MODE: u32 = 0xae4;
const CAMERA_CAR: u32 = 0xadc;
/// Car records (`1296c8h + n * CC8h`); record n's car is the n-th `auto*` entry of the
/// scene table (verified in the attract race, where the camera changes cars).
const CARS: u32 = 0x1296c8;
const CAR_RECORD: u32 = 0xcc8;
const SCENE: u32 = 0x12f860;
const SCENE_ENTRY: u32 = 28;

// ---------------------------------------------------------------------------------------
// The setting

/// `graphics.camera_collision`: 1 on (default), 0 off.
static ENABLED: AtomicU8 = AtomicU8::new(1);

/// Turn camera collision on or off (settings; takes effect at the next frame).
pub fn set_enabled(on: bool) {
    ENABLED.store(on as u8, Ordering::Relaxed);
}

fn env_flag(name: &str) -> Option<bool> {
    match std::env::var(name).ok().as_deref().map(str::trim) {
        Some("1") | Some("on") => Some(true),
        Some("0") | Some("off") => Some(false),
        _ => None,
    }
}

/// Camera collision is on (`SR_CAMERA_COLLISION=1` / `0` override the setting).
pub fn enabled() -> bool {
    static ENV: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| env_flag("SR_CAMERA_COLLISION")).unwrap_or(ENABLED.load(Ordering::Relaxed) != 0)
}

/// `SR_CAMERA_MIRROR=0` (diagnostics): the rear-view mirror keeps the game's near plane.
fn mirror_on() -> bool {
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENV.get_or_init(|| env_flag("SR_CAMERA_MIRROR").unwrap_or(true))
}

/// `SR_CAMERA_NEAR=0` (diagnostics): the game's near plane, pull-in only.
fn near_on() -> bool {
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENV.get_or_init(|| env_flag("SR_CAMERA_NEAR").unwrap_or(true))
}

// ---------------------------------------------------------------------------------------
// Small vector helpers (f64)

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}

// ---------------------------------------------------------------------------------------
// The world's solid triangles

/// A triangle, vertices relative to [`World::origin`]. Its front side is the side its
/// numerical normal `(b - a) x (c - a)` points to: in the game's left-handed world
/// coordinates the scene's front faces (clockwise on screen) have that normal towards the
/// viewer (`camera.md`, checked by `world_front_faces_point_up` on the road).
#[derive(Clone, Copy, Debug)]
struct Tri {
    v: [[f32; 3]; 3],
    lo: [f32; 3],
    hi: [f32; 3],
    /// The static it belongs to.
    owner: u32,
}

impl Tri {
    fn verts(&self) -> [V3; 3] {
        self.v.map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
    }
}

/// Which materials make a triangle an obstacle.
#[derive(Clone, Copy, Debug)]
pub struct Solid {
    /// Colour-keyed (see-through) surfaces count too (fences, tree panels, backdrops).
    pub keyed: bool,
    pub tiny_radius: f32,
}

impl Default for Solid {
    fn default() -> Self {
        Solid { keyed: false, tiny_radius: TINY_RADIUS }
    }
}

impl Solid {
    /// The default, or with `SR_CAMERA_SOLID=all` (diagnostics) every textured or flat
    /// world surface (colour-keyed and tiny objects too), `keyed` the colour-keyed ones too.
    fn from_env() -> Solid {
        match std::env::var("SR_CAMERA_SOLID").ok().as_deref().map(str::trim) {
            Some("all") => Solid { keyed: true, tiny_radius: 0.0 },
            Some("keyed") => Solid { keyed: true, ..Solid::default() },
            _ => Solid::default(),
        }
    }
}

/// The world's solid triangles in a uniform grid over (x, z), built once per race.
pub struct World {
    origin: V3,
    tris: Vec<Tri>,
    x0: f64,
    z0: f64,
    nx: usize,
    nz: usize,
    start: Vec<u32>,
    items: Vec<u32>,
    stamp: Vec<u32>,
    query: u32,
}

/// A query's statistics (candidates looked at, triangles tested exactly).
#[derive(Clone, Copy, Debug, Default)]
pub struct QueryCost {
    pub candidates: u32,
    pub tested: u32,
}

impl World {
    /// From world-space triangles (`owner` = the static's index).
    pub fn new(input: &[([V3; 3], u32)]) -> World {
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        for (t, _) in input {
            for p in t {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
        if input.is_empty() {
            lo = [0.0; 3];
            hi = [0.0; 3];
        }
        let origin = [((lo[0] + hi[0]) * 0.5).round(), ((lo[1] + hi[1]) * 0.5).round(), ((lo[2] + hi[2]) * 0.5).round()];
        let tris: Vec<Tri> = input
            .iter()
            .map(|(t, owner)| {
                let v = t.map(|p| [(p[0] - origin[0]) as f32, (p[1] - origin[1]) as f32, (p[2] - origin[2]) as f32]);
                let mut l = [f32::MAX; 3];
                let mut h = [f32::MIN; 3];
                for p in &v {
                    for k in 0..3 {
                        l[k] = l[k].min(p[k]);
                        h[k] = h[k].max(p[k]);
                    }
                }
                Tri { v, lo: l, hi: h, owner: *owner }
            })
            .collect();
        let x0 = lo[0] - origin[0] - 1.0;
        let z0 = lo[2] - origin[2] - 1.0;
        let nx = (((hi[0] - lo[0]) + 2.0) / CELL).ceil().max(1.0) as usize;
        let nz = (((hi[2] - lo[2]) + 2.0) / CELL).ceil().max(1.0) as usize;
        let mut w = World { origin, tris, x0, z0, nx, nz, start: vec![0; nx * nz + 1], items: Vec::new(), stamp: Vec::new(), query: 0 };
        // two passes: count per cell, then fill (compressed rows)
        let ranges: Vec<_> = w.tris.iter().map(|t| w.cells(t.lo[0] as f64, t.lo[2] as f64, t.hi[0] as f64, t.hi[2] as f64)).collect();
        for &(cx0, cz0, cx1, cz1) in &ranges {
            for cz in cz0..=cz1 {
                for cx in cx0..=cx1 {
                    w.start[cz * nx + cx + 1] += 1;
                }
            }
        }
        for i in 0..nx * nz {
            w.start[i + 1] += w.start[i];
        }
        let mut fill = w.start.clone();
        w.items = vec![0; w.start[nx * nz] as usize];
        for (i, &(cx0, cz0, cx1, cz1)) in ranges.iter().enumerate() {
            for cz in cz0..=cz1 {
                for cx in cx0..=cx1 {
                    let c = cz * nx + cx;
                    w.items[fill[c] as usize] = i as u32;
                    fill[c] += 1;
                }
            }
        }
        w.stamp = vec![0; w.tris.len()];
        w
    }

    /// The solid triangles of a scene's statics.
    pub fn from_scene(scene: &SceneData, meta: &[StaticMeta], solid: Solid) -> World {
        let mut input = Vec::new();
        for (i, inst) in scene.statics.iter().enumerate() {
            if meta.get(i).is_some_and(|s| s.radius < solid.tiny_radius) {
                continue;
            }
            let Some(mesh) = scene.meshes.get(inst.mesh as usize) else { continue };
            for r in &mesh.ranges {
                let Some(mat) = scene.materials.get(r.material as usize) else { continue };
                let ok = matches!(mat.layer, Layer::World | Layer::Water)
                    && match mat.transparency {
                        Transparency::Opaque => true,
                        Transparency::ChromaKey { .. } => solid.keyed,
                        Transparency::Blended(_) => false,
                    };
                if !ok {
                    continue;
                }
                for k in (r.indices.start as usize..r.indices.end as usize).step_by(3) {
                    let Some(ix) = mesh.indices.get(k..k + 3) else { break };
                    let p = [0, 1, 2].map(|j| inst.transform.apply(mesh.vertices.get(ix[j] as usize).map_or([0.0; 3], |v| v.pos)));
                    let n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
                    if len(n) < 1e-6 {
                        continue;
                    }
                    input.push((p, i as u32));
                }
            }
        }
        World::new(&input)
    }

    pub fn len(&self) -> usize {
        self.tris.len()
    }

    /// Nearly horizontal triangles (normal within 25 degrees of vertical) whose front side
    /// is up, and those whose front side is down: the road and the ground must face up if
    /// the front side is read right (`Tri`).
    pub fn horizontal_facing(&self) -> (usize, usize) {
        let (mut up, mut down) = (0, 0);
        for t in &self.tris {
            let w = t.verts();
            let n = cross(sub(w[1], w[0]), sub(w[2], w[0]));
            let l = len(n);
            if n[1].abs() > 0.9 * l {
                if n[1] > 0.0 { up += 1 } else { down += 1 }
            }
        }
        (up, down)
    }

    /// Entries in the grid (a triangle counts once per cell it covers).
    pub fn entries(&self) -> usize {
        self.items.len()
    }

    fn cells(&self, xa: f64, za: f64, xb: f64, zb: f64) -> (usize, usize, usize, usize) {
        let cx = |x: f64| (((x - self.x0) / CELL).floor().max(0.0) as usize).min(self.nx - 1);
        let cz = |z: f64| (((z - self.z0) / CELL).floor().max(0.0) as usize).min(self.nz - 1);
        (cx(xa.min(xb)), cz(za.min(zb)), cx(xa.max(xb)), cz(za.max(zb)))
    }

    /// Every triangle of the cells under the box (local coordinates) whose box overlaps it,
    /// each once.
    fn candidates(&mut self, lo: V3, hi: V3, mut f: impl FnMut(&Tri)) -> u32 {
        self.query = self.query.wrapping_add(1);
        if self.query == 0 {
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.query = 1;
        }
        let (cx0, cz0, cx1, cz1) = self.cells(lo[0], lo[2], hi[0], hi[2]);
        let mut n = 0;
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let c = cz * self.nx + cx;
                for k in self.start[c]..self.start[c + 1] {
                    let i = self.items[k as usize] as usize;
                    if self.stamp[i] == self.query {
                        continue;
                    }
                    self.stamp[i] = self.query;
                    n += 1;
                    let t = &self.tris[i];
                    if (0..3).all(|k| t.lo[k] as f64 <= hi[k] && t.hi[k] as f64 >= lo[k]) {
                        f(t);
                    }
                }
            }
        }
        n
    }

    /// The first surface facing `from` that the segment `from` -> `to` (world coordinates)
    /// crosses: its distance from `from` and its static.
    pub fn first_hit(&mut self, from: V3, to: V3, cost: &mut QueryCost) -> Option<(f64, u32)> {
        let o = sub(from, self.origin);
        let e = sub(to, self.origin);
        let d = sub(e, o);
        let length = len(d);
        if length < 1e-9 {
            return None;
        }
        let lo = [0, 1, 2].map(|k| o[k].min(e[k]));
        let hi = [0, 1, 2].map(|k| o[k].max(e[k]));
        let mut best: Option<(f64, u32)> = None;
        let mut tested = 0;
        cost.candidates += self.candidates(lo, hi, |t| {
            tested += 1;
            if let Some(s) = segment_hits_front(o, d, &t.verts()) {
                if best.is_none_or(|b| s < b.0) {
                    best = Some((s, t.owner));
                }
            }
        });
        cost.tested += tested;
        best.map(|(s, owner)| (s * length, owner))
    }

    /// Over the solid surfaces facing the eye inside the view pyramid of `cam` (side planes
    /// `k`: view x / z from k[0] to k[1], y / z from k[2] to k[3]) within view depth
    /// `zmax`: the nearest view depth, and whether one of them crosses the plane at depth
    /// `cut` (a near plane there would cut it open).
    pub fn near_slab(&mut self, cam: &Camera, k: [f64; 4], zmax: f64, cut: f64, cost: &mut QueryCost) -> (Option<f64>, bool) {
        let eye = sub(cam.pos, self.origin);
        let rot = cam.rot.map(|v| v as f64);
        let to_world = |v: V3| [0, 1, 2].map(|i| eye[i] + rot[i] * v[0] + rot[3 + i] * v[1] + rot[6 + i] * v[2]);
        let corners = [[0.0; 3], [k[0] * zmax, k[2] * zmax, zmax], [k[1] * zmax, k[2] * zmax, zmax], [k[0] * zmax, k[3] * zmax, zmax], [k[1] * zmax, k[3] * zmax, zmax]].map(to_world);
        let lo = [0, 1, 2].map(|i| corners.iter().map(|c| c[i]).fold(f64::MAX, f64::min));
        let hi = [0, 1, 2].map(|i| corners.iter().map(|c| c[i]).fold(f64::MIN, f64::max));
        let mut nearest: Option<f64> = None;
        let mut crossing = false;
        let mut tested = 0;
        cost.candidates += self.candidates(lo, hi, |t| {
            let w = t.verts();
            // facing the eye (back faces are culled: cutting them shows nothing)
            if dot(cross(sub(w[1], w[0]), sub(w[2], w[0])), sub(eye, w[0])) <= 0.0 {
                return;
            }
            tested += 1;
            let v = w.map(|p| {
                let d = sub(p, eye);
                [dot([rot[0], rot[1], rot[2]], d), dot([rot[3], rot[4], rot[5]], d), dot([rot[6], rot[7], rot[8]], d)]
            });
            if let Some((z0, z1)) = depth_in_pyramid(&v, k) {
                if z0 < zmax {
                    nearest = Some(nearest.map_or(z0, |n| n.min(z0)));
                }
                if z0 < cut && z1 > cut {
                    crossing = true;
                }
            }
        });
        cost.tested += tested;
        (nearest, crossing)
    }
}

impl World {
    /// The distance from `p` (world) to the nearest solid triangle within `radius`, and
    /// whether `p` is on that triangle's front side.
    pub fn clearance(&mut self, p: V3, radius: f64) -> Option<(f64, bool)> {
        let q = sub(p, self.origin);
        let lo = q.map(|v| v - radius);
        let hi = q.map(|v| v + radius);
        let mut best: Option<(f64, bool)> = None;
        self.candidates(lo, hi, |t| {
            let w = t.verts();
            let c = closest_on_triangle(q, &w);
            let d = len(sub(q, c));
            if d <= radius && best.is_none_or(|b| d < b.0) {
                let n = cross(sub(w[1], w[0]), sub(w[2], w[0]));
                best = Some((d, dot(n, sub(q, w[0])) >= 0.0));
            }
        });
        best
    }
}

/// The point of triangle `t` nearest to `p` (Ericson, Real-Time Collision Detection 5.1.5).
fn closest_on_triangle(p: V3, t: &[V3; 3]) -> V3 {
    let (a, b, c) = (t[0], t[1], t[2]);
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return add(a, scale(ab, d1 / (d1 - d3)));
    }
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return add(a, scale(ac, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return add(b, scale(sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denom = 1.0 / (va + vb + vc);
    add(a, add(scale(ab, vb * denom), scale(ac, vc * denom)))
}

/// Moller-Trumbore for the segment `o + s d` (s in 0..1) and a triangle; only a crossing
/// from the triangle's front side (the side its normal points to) counts. Returns s.
fn segment_hits_front(o: V3, d: V3, t: &[V3; 3]) -> Option<f64> {
    let e1 = sub(t[1], t[0]);
    let e2 = sub(t[2], t[0]);
    let n = cross(e1, e2);
    if dot(n, d) >= 0.0 {
        return None;
    }
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s0 = sub(o, t[0]);
    let u = dot(s0, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s0, e1);
    let v = dot(d, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let s = dot(e2, q) * inv;
    (0.0..=1.0).contains(&s).then_some(s)
}

/// The view depths (min, max) of the part of a view-space triangle (x right, y down, z
/// forward) inside the pyramid from the eye: `k[0] z <= x <= k[1] z`, `k[2] z <= y <= k[3]
/// z` (which implies z >= 0). None when no part is inside.
fn depth_in_pyramid(v: &[V3; 3], k: [f64; 4]) -> Option<(f64, f64)> {
    let planes = [[1.0, 0.0, -k[0]], [-1.0, 0.0, k[1]], [0.0, 1.0, -k[2]], [0.0, -1.0, k[3]]];
    let mut poly: Vec<V3> = v.to_vec();
    let mut next = Vec::with_capacity(8);
    for pl in planes {
        next.clear();
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            let fa = dot(pl, a);
            let fb = dot(pl, b);
            if fa >= 0.0 {
                next.push(a);
            }
            if (fa >= 0.0) != (fb >= 0.0) {
                let t = fa / (fa - fb);
                next.push(add(a, scale(sub(b, a), t)));
            }
        }
        std::mem::swap(&mut poly, &mut next);
        if poly.len() < 3 {
            return None;
        }
    }
    let z0 = poly.iter().map(|p| p[2]).fold(f64::MAX, f64::min);
    let z1 = poly.iter().map(|p| p[2]).fold(f64::MIN, f64::max);
    Some((z0, z1))
}

/// The view pyramid of a view (`k` of [`World::near_slab`]): the viewport (a full-width
/// one widened by the widescreen extension `ext`, as the renderer does) through the
/// projection.
pub fn pyramid(view: &View, ext: f64) -> [f64; 4] {
    let vp = view.viewport.map(|v| v as f64);
    let full = vp[0] <= 0.0 && vp[2] >= 640.0;
    let (x0, x1) = if full { (vp[0] - ext, vp[2] + ext) } else { (vp[0], vp[2]) };
    let p = &view.camera.proj;
    let (cx, cy, sx, sy) = (p.center[0] as f64, p.center[1] as f64, p.scale[0] as f64, p.scale[1] as f64);
    let (mut a, mut b) = ((x0 - cx) / sx, (x1 - cx) / sx);
    if view.mirror {
        // flipped inside the viewport: the same rectangle seen the other way
        let (c0, c1) = ((vp[0] + vp[2] - x1 - cx) / sx, (vp[0] + vp[2] - x0 - cx) / sx);
        a = c0;
        b = c1;
    }
    [a, b, (vp[1] - cy) / sy, (vp[3] - cy) / sy]
}

// ---------------------------------------------------------------------------------------
// The followed car

/// A car's placement at a simulation step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarPose {
    pub model: u32,
    pub pos: V3,
    pub angles: V3,
}

impl CarPose {
    fn lerp(a: &CarPose, b: &CarPose, t: f64) -> CarPose {
        if a.model != b.model || len(sub(b.pos, a.pos)) > 4000.0 {
            return *b;
        }
        let mut out = *b;
        for k in 0..3 {
            out.pos[k] = a.pos[k] + (b.pos[k] - a.pos[k]) * t;
            out.angles[k] = super::dynamics::lerp_angle(a.angles[k], b.angles[k], t);
        }
        out
    }

    fn focus(&self) -> V3 {
        add(self.pos, [0.0, FOCUS_HEIGHT, 0.0])
    }
}

/// A car model's box in its own space (the union of its part meshes' vertices).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarBox {
    pub lo: V3,
    pub hi: V3,
}

impl CarBox {
    /// Distance from `from` along the unit direction `dir` (both in the car's space) at
    /// which the ray leaves the box; 0 if it does not pass through it.
    fn exit(&self, from: V3, dir: V3) -> f64 {
        let (mut t0, mut t1) = (0.0f64, f64::MAX);
        for k in 0..3 {
            if dir[k].abs() < 1e-12 {
                if from[k] < self.lo[k] || from[k] > self.hi[k] {
                    return 0.0;
                }
                continue;
            }
            let a = (self.lo[k] - from[k]) / dir[k];
            let b = (self.hi[k] - from[k]) / dir[k];
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
        }
        if t1 < t0 { 0.0 } else { t1 }
    }

    /// The box's twelve triangles in world coordinates.
    fn triangles(&self, pose: &CarPose) -> Vec<[V3; 3]> {
        let r = rotation(pose.angles);
        let c = |i: usize| {
            let p = [if i & 1 != 0 { self.hi[0] } else { self.lo[0] }, if i & 2 != 0 { self.hi[1] } else { self.lo[1] }, if i & 4 != 0 { self.hi[2] } else { self.lo[2] }];
            [0, 1, 2].map(|k| r[k][0] * p[0] + r[k][1] * p[1] + r[k][2] * p[2] + pose.pos[k])
        };
        const FACES: [[usize; 4]; 6] = [[0, 1, 3, 2], [4, 5, 7, 6], [0, 1, 5, 4], [2, 3, 7, 6], [0, 2, 6, 4], [1, 3, 7, 5]];
        let mut out = Vec::with_capacity(12);
        for f in FACES {
            out.push([c(f[0]), c(f[1]), c(f[2])]);
            out.push([c(f[0]), c(f[2]), c(f[3])]);
        }
        out
    }
}

// ---------------------------------------------------------------------------------------
// Smoothing (pure, unit-tested)

/// One view's smoothing state, advanced once per simulation step.
#[derive(Clone, Debug, Default)]
pub struct Track {
    /// Game frame of the current step (0: none yet).
    pub frame: u64,
    pub mode: i32,
    pub cam: V3,
    /// Pull-in (units towards the focus) and near plane at the current step and the one
    /// before (None after a cut).
    pub pull: f64,
    pub prev_pull: Option<f64>,
    pub near: f64,
    pub prev_near: Option<f64>,
    pub car: Option<CarPose>,
    pub prev_car: Option<CarPose>,
}

/// The smoothing's value within half a unit of its target is the target: the eased value
/// reaches the game's camera exactly (an exponential approach alone never would).
fn snap(v: f64, target: f64) -> f64 {
    if (v - target).abs() < 0.5 { target } else { v }
}

/// Per-step smoothing factor for a time constant.
pub fn factor(tau: f64, period: f64) -> f64 {
    if tau <= 0.0 { 1.0 } else { 1.0 - (-period / tau).exp() }
}

impl Track {
    /// A step at `frame` does not follow on from the current one (first step, a gap, the
    /// camera mode changed or the game's camera jumped): it starts at its targets.
    fn is_cut(&self, frame: u64, mode: i32, cam: V3) -> bool {
        self.frame == 0 || frame != self.frame + 1 || mode != self.mode || len(sub(cam, self.cam)) > MAX_CAMERA_MOVE
    }

    /// The smoothed pull-in of a new step whose target is `target`: pulling in follows
    /// fast ([`TAU_IN`]), letting go slowly ([`TAU_OUT`]). The same step drawn again keeps
    /// its value.
    pub fn step_pull(&self, frame: u64, mode: i32, cam: V3, target: f64, period: f64) -> f64 {
        if frame == self.frame {
            return self.pull;
        }
        if self.is_cut(frame, mode, cam) {
            return target;
        }
        let k = if target > self.pull { factor(TAU_IN, period) } else { factor(TAU_OUT, period) };
        snap(self.pull + (target - self.pull) * k, target)
    }

    /// Commit a new step: its smoothed pull-in (`step_pull`) and the near plane's target at
    /// the eye it gives. The near plane comes in at once (that only uncovers what the plane
    /// would cut) and goes back out slowly.
    pub fn step(&mut self, frame: u64, mode: i32, cam: V3, car: Option<CarPose>, pull: f64, near: f64, period: f64) {
        if frame == self.frame {
            return;
        }
        let cut = self.is_cut(frame, mode, cam);
        let n = if cut || near < self.near { near } else { snap(self.near + (near - self.near) * factor(TAU_OUT, period), near) };
        let p = pull;
        self.prev_pull = (!cut).then_some(self.pull);
        self.prev_near = (!cut).then_some(self.near);
        self.prev_car = if cut { None } else { self.car };
        self.pull = p;
        self.near = n;
        self.car = car;
        self.frame = frame;
        self.mode = mode;
        self.cam = cam;
    }

    /// Between the last two steps (an extra frame at `alpha`).
    pub fn between(&self, alpha: f64) -> (f64, f64, Option<CarPose>) {
        let pull = self.prev_pull.map_or(self.pull, |p| p + (self.pull - p) * alpha);
        let near = self.prev_near.map_or(self.near, |n| n + (self.near - n) * alpha);
        let car = match (self.prev_car, self.car) {
            (Some(a), Some(b)) => Some(CarPose::lerp(&a, &b, alpha)),
            (_, b) => b,
        };
        (pull, near, car)
    }
}

// ---------------------------------------------------------------------------------------
// State per race

#[derive(Default)]
struct Stats {
    since: Option<std::time::Instant>,
    views: u64,
    pulled: u64,
    pull_sum: f64,
    near_moved: u64,
    inside_game: u64,
    inside_drawn: u64,
    secs: f64,
    candidates: u64,
}

/// The camera collision's state (`NativeState::camera`).
#[derive(Default)]
pub struct CameraState {
    world: Option<World>,
    boxes: HashMap<u32, CarBox>,
    /// The scene table's car instances in order (car record n = entry n).
    cars: Vec<u32>,
    /// Per view slot (main, second player, rear-view mirror).
    tracks: [Track; 3],
    stats: Stats,
}

impl CameraState {
    /// At a race's scene build (not for the menus' scenes).
    pub fn new(m: &Machine, build: &SceneBuild) -> CameraState {
        let t0 = std::time::Instant::now();
        let world = World::from_scene(&build.scene, &build.statics, Solid::from_env());
        let mut boxes: HashMap<u32, CarBox> = HashMap::new();
        for pm in &build.part_meshes {
            let Some(mesh) = build.scene.meshes.get(pm.mesh as usize) else { continue };
            for v in &mesh.vertices {
                let p = v.pos.map(|x| x as f64);
                let b = boxes.entry(pm.model).or_insert(CarBox { lo: p, hi: p });
                for k in 0..3 {
                    b.lo[k] = b.lo[k].min(p[k]);
                    b.hi[k] = b.hi[k].max(p[k]);
                }
            }
        }
        let cars = car_instances(m);
        let (up, down) = world.horizontal_facing();
        crate::info!(
            "native camera: {} solid triangles of {} statics in a {} x {} grid ({} entries; horizontal ones facing up {up}, down {down}), {} car instances, car boxes {:?}; built in {:.1} ms",
            world.len(),
            build.scene.statics.len(),
            world.nx,
            world.nz,
            world.entries(),
            cars.len(),
            boxes.iter().map(|(k, b)| (*k, b.lo.map(|v| v as i32), b.hi.map(|v| v as i32))).collect::<Vec<_>>(),
            t0.elapsed().as_secs_f64() * 1000.0
        );
        CameraState { world: Some(world), boxes, cars, ..Default::default() }
    }
}

/// The scene table's `auto*` instances in table order.
fn car_instances(m: &Machine) -> Vec<u32> {
    let base = m.img.link(SCENE);
    let mut out = Vec::new();
    for i in 0..1024u32 {
        let e = base + i * SCENE_ENTRY;
        if !mem::readable(e, SCENE_ENTRY) {
            break;
        }
        let (inst, index) = unsafe { (mem::rd32(e + 0xc), mem::rd32(e + 0x18)) };
        if index == u32::MAX {
            break;
        }
        if unsafe { mem::slice(e, 4) } == b"auto" {
            out.push(inst);
        }
    }
    out
}

fn rd(a: u32) -> u32 {
    unsafe { mem::rd32(a) }
}

/// The car followed by view `slot` now (its record at the camera object's +ADCh).
fn followed_car(m: &Machine, st: &CameraState, obj: u32) -> Option<CarPose> {
    let rec = rd(obj + CAMERA_CAR);
    let base = m.img.link(CARS);
    let off = rec.wrapping_sub(base);
    if off % CAR_RECORD != 0 {
        return None;
    }
    let inst = *st.cars.get((off / CAR_RECORD) as usize)?;
    if !mem::readable(inst, 0x20) {
        return None;
    }
    let i = |o: u32| rd(inst + o) as i32 as f64;
    Some(CarPose { model: rd(inst), pos: [i(8), i(0xc), i(0x10)], angles: [i(0x14), i(0x18), i(0x1c)] })
}

// ---------------------------------------------------------------------------------------
// Per view

/// What one adjustment found (the log and the statistics).
#[derive(Clone, Copy, Debug, Default)]
pub struct Outcome {
    pub mode: i32,
    /// Focus to the game's eye.
    pub distance: f64,
    /// The first wall facing the car along the segment (distance from the focus).
    pub hit: Option<f64>,
    pub hit_static: u32,
    pub target_pull: f64,
    /// The pull-in that puts the eye `NEAR_MIN + NEAR_MARGIN` in front of the first wall
    /// (the smoothing never does less).
    pub least_pull: f64,
    pub pull: f64,
    /// Focus to the drawn eye; the nearest it may come.
    pub drawn: f64,
    pub min_distance: f64,
    pub near: f64,
    /// Solid geometry facing the eye nearer than the drawn near plane's target (depth).
    pub slab: Option<f64>,
    pub cost: QueryCost,
}

/// When a view is drawn: a simulation step's own drawing, or an extra frame between the
/// last two steps.
#[derive(Clone, Copy, Debug)]
pub enum When {
    Step { frame: u64, cam: V3, period: f64 },
    Between { alpha: f64 },
}

/// Correct the drawn camera of a native view (`frame::draw_view`, after the view's camera
/// is built from the game's): `cam` is the game camera (interpolated in extra frames, at
/// `alpha`), `slot` the view (0 main, 1 second player, 2 mirror).
pub fn adjust(m: &mut Machine, view: &mut View, cam: &GameCamera, slot: usize, alpha: Option<f32>) {
    probe(m, cam, slot);
    if slot > 2 || m.native.menu || !enabled() || m.native.camera.world.is_none() {
        return;
    }
    let t0 = std::time::Instant::now();
    // the mirror (slot 2) goes with the main view's camera mode
    let obj = rd(m.img.link(VIEW_CAMERAS + 4 * if slot == 2 { 0 } else { slot as u32 }));
    if obj < 0x10000 || !mem::readable(obj, CAMERA_MODE + 4) {
        return;
    }
    let mode = rd(obj + CAMERA_MODE) as i32;
    // the chase views get the pull-in and the near plane, the rear-view mirror the near
    // plane only (it is drawn from beside the car: nothing to pull towards)
    let mirror = slot == 2 || view.mirror;
    let chase = mode == 3 || mode == 4 || mode == 6;
    let pull_on = chase && !mirror;
    let mut st = std::mem::take(&mut m.native.camera);
    if !(chase && !mirror || mirror && mirror_on()) {
        // not corrected: the bumper and in-car views, other cameras (the log still
        // measures them)
        st.tracks[slot] = Track::default();
        if log_path().is_some() {
            let out = Outcome { mode: if view.mirror { -1 } else { mode }, near: NEAR_GAME, hit_static: u32::MAX, ..Default::default() };
            let when = When::Step { frame: crate::glide::FRAME.load(Ordering::Relaxed), cam: cam.pos, period: 0.0 };
            let when = if crate::interp::in_extra_draw() { When::Between { alpha: alpha.unwrap_or(1.0) as f64 } } else { when };
            let game = view.camera;
            log_line(&mut st, &game, view, &out, when, None, 0.0);
        }
        m.native.camera = st;
        return;
    }
    let mode = if mirror { -1 } else { mode };
    let (when, car) = if crate::interp::in_extra_draw() {
        let a = alpha.unwrap_or(1.0) as f64;
        (When::Between { alpha: a }, st.tracks[slot].between(a).2)
    } else {
        let frame = crate::glide::FRAME.load(Ordering::Relaxed);
        (When::Step { frame, cam: cam.pos, period: crate::pacing::period() }, followed_car(m, &st, obj))
    };
    let game_camera = view.camera;
    let ext = crate::view::ext() as f64;
    let CameraState { world, boxes, tracks, .. } = &mut st;
    let out = solve(world.as_mut().unwrap(), boxes, &mut tracks[slot], view, car, mode, pull_on, ext, when);
    let secs = t0.elapsed().as_secs_f64();
    log_line(&mut st, &game_camera, view, &out, when, car, secs);
    let s = &mut st.stats;
    s.views += 1;
    s.secs += secs;
    s.candidates += out.cost.candidates as u64;
    if out.pull > 0.5 {
        s.pulled += 1;
        s.pull_sum += out.pull;
    }
    if out.near < NEAR_GAME - 0.5 {
        s.near_moved += 1;
    }
    if out.hit.is_some_and(|h| h < out.distance) {
        s.inside_game += 1;
    }
    if out.hit.is_some_and(|h| h < out.drawn) {
        s.inside_drawn += 1;
    }
    let since = *s.since.get_or_insert(t0);
    if t0 - since >= std::time::Duration::from_secs(5) && s.views > 0 {
        let n = s.views as f64;
        crate::info!(
            "native camera: {} views in {:.1} s; pulled in {} (mean {:.0} units), near plane moved in {}; eye behind a wall: game {}, drawn {}; {:.1} us and {:.0} candidate triangles per view",
            s.views,
            (t0 - since).as_secs_f64(),
            s.pulled,
            if s.pulled > 0 { s.pull_sum / s.pulled as f64 } else { 0.0 },
            s.near_moved,
            s.inside_game,
            s.inside_drawn,
            s.secs * 1e6 / n,
            s.candidates as f64 / n
        );
        *s = Stats { since: Some(t0), ..Default::default() };
    }
    m.native.camera = st;
}

/// A world point in a camera's view space (x right, y down, z forward).
fn to_view(cam: &Camera, p: V3) -> V3 {
    let r = cam.rot.map(|v| v as f64);
    let d = sub(p, cam.pos);
    [r[0] * d[0] + r[1] * d[1] + r[2] * d[2], r[3] * d[0] + r[4] * d[1] + r[5] * d[2], r[6] * d[0] + r[7] * d[1] + r[8] * d[2]]
}

/// The correction itself (pure apart from the world's query marks and `track`): the
/// pull-in along the focus-eye segment, then the near plane at the eye it gives.
#[allow(clippy::too_many_arguments)]
pub fn solve(world: &mut World, boxes: &HashMap<u32, CarBox>, track: &mut Track, view: &mut View, car: Option<CarPose>, mode: i32, pull_on: bool, ext: f64, when: When) -> Outcome {
    let mut out = Outcome { mode, near: NEAR_GAME, hit_static: u32::MAX, ..Default::default() };
    let mirror = view.mirror;
    let eye = view.camera.pos;
    let car_box = car.and_then(|c| boxes.get(&c.model).copied());
    // the pull-in's target at the game's eye
    let mut dir = [0.0; 3];
    let mut focus = eye;
    if let (true, Some(c)) = (pull_on, car) {
        focus = c.focus();
        let seg = sub(eye, focus);
        let d = len(seg);
        if d > 1.0 {
            dir = scale(seg, 1.0 / d);
            out.distance = d;
            let hit = world.first_hit(focus, add(focus, scale(dir, d + MARGIN)), &mut out.cost);
            out.hit = hit.map(|h| h.0);
            out.hit_static = hit.map_or(u32::MAX, |h| h.1);
            // never into the car: where the segment leaves its box, plus a clearance
            let exit = car_box.map_or(0.0, |b| {
                let r = rotation(c.angles);
                let to_car = |v: V3| [0, 1, 2].map(|k| r[0][k] * v[0] + r[1][k] * v[1] + r[2][k] * v[2]);
                b.exit(to_car(sub(focus, c.pos)), to_car(dir))
            });
            out.min_distance = (exit + CAR_CLEARANCE).min(d);
            if let Some(h) = out.hit {
                out.target_pull = d - (h - MARGIN).clamp(out.min_distance, d);
                // the smoothing may lag the target, but never leave the eye behind the wall
                // or nearer it than the nearest near plane needs (where the car allows)
                out.least_pull = d - (h - NEAR_MIN - NEAR_MARGIN).clamp(out.min_distance, d);
            }
        }
    }
    let place = |view: &mut View, pull: f64, out: &mut Outcome| {
        if out.distance > 1.0 {
            let d = (out.distance - pull).clamp(out.min_distance, out.distance);
            out.pull = out.distance - d;
            out.drawn = d;
            view.camera.pos = add(focus, scale(dir, d));
        }
    };
    // the near plane's target at the drawn eye: in front of the solid geometry facing the
    // eye and of the followed car's box inside the view, within the game's near plane
    let near_target = |view: &View, world: &mut World, out: &mut Outcome| -> f64 {
        if !near_on() {
            return NEAR_GAME;
        }
        let k = pyramid(view, ext);
        let zmax = NEAR_GAME + NEAR_MARGIN;
        let (world_z, _) = world.near_slab(&view.camera, k, zmax, NEAR_GAME, &mut out.cost);
        let mut car_z: Option<f64> = None;
        if let (Some(c), Some(b)) = (car, car_box) {
            for t in b.triangles(&c) {
                if let Some((z0, _)) = depth_in_pyramid(&t.map(|p| to_view(&view.camera, p)), k) {
                    if z0 < zmax {
                        car_z = Some(car_z.map_or(z0, |n| n.min(z0)));
                    }
                }
            }
        }
        let nearest = match (world_z, car_z) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        out.slab = nearest;
        // the mirror: its own car between the eye and the game's near plane is cut by the
        // game; moving the plane in would show more of it, so the mirror keeps the game's
        // plane then
        if mirror && car_z.is_some() {
            return NEAR_GAME;
        }
        nearest.map_or(NEAR_GAME, |z| (z - NEAR_MARGIN).clamp(NEAR_MIN, NEAR_GAME))
    };
    match when {
        When::Between { alpha } => {
            let (pull, near, _) = track.between(alpha);
            // smoothed, but never leaving the eye behind the wall: at worst on it
            place(view, pull.max(out.least_pull), &mut out);
            // between the steps, but never cutting what is in front of this frame's eye
            out.near = near.min(near_target(view, world, &mut out));
        }
        When::Step { frame, cam, period } => {
            // smoothed, but never leaving the eye behind the wall: at worst on it, easing
            // into the margin in front of it
            let pull = track.step_pull(frame, mode, cam, out.target_pull, period).max(out.least_pull);
            place(view, pull, &mut out);
            let near = near_target(view, world, &mut out);
            track.step(frame, mode, cam, car, pull, near, period);
            out.near = track.near.min(near);
        }
    }
    if out.distance <= 1.0 {
        out.drawn = out.distance;
    }
    view.camera.proj.near = out.near as f32;
    out
}

fn log_path() -> Option<&'static str> {
    static P: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    P.get_or_init(|| std::env::var("SR_CAMERA_LOG").ok().filter(|s| !s.trim().is_empty())).as_deref()
}

/// `SR_CAMERA_LOG`: one line per view adjusted, with independent checks of the game's and
/// the drawn camera: the eye behind a wall facing the car (the first hit along the focus-eye
/// segment nearer than the eye) and solid geometry facing the eye crossing the near plane
/// inside the view (the plane cuts it open).
fn log_line(st: &mut CameraState, game: &Camera, view: &View, out: &Outcome, when: When, car: Option<CarPose>, secs: f64) {
    let Some(path) = log_path() else { return };
    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
    let mut f = FILE.lock().unwrap();
    if f.is_none() {
        *f = std::fs::File::create(path).ok();
        if let Some(f) = f.as_mut() {
            let _ = writeln!(f, "frame\textra\talpha\tmode\tdistance\thit\thit_static\ttarget_pull\tpull\tdrawn\tmin_distance\tnear\tslab\tinside_game\tinside_drawn\tnear_cross_game\tnear_cross_drawn\tus\tcandidates\tcar\tclear_game\tclear_drawn");
        }
    }
    let Some(file) = f.as_mut() else { return };
    let (cross_game, cross_drawn, clear_game, clear_drawn) = match st.world.as_mut() {
        Some(world) => {
            let mut cost = QueryCost::default();
            let ext = crate::view::ext() as f64;
            let k = pyramid(view, ext);
            let (_, g) = world.near_slab(game, k, NEAR_GAME + 1.0, NEAR_GAME, &mut cost);
            let (_, d) = world.near_slab(&view.camera, k, out.near + 1.0, out.near, &mut cost);
            let c = |w: &mut World, p: V3| w.clearance(p, 1000.0).map_or("-".to_string(), |(d, front)| format!("{d:.0}{}", if front { "" } else { "b" }));
            (g, d, c(world, game.pos), c(world, view.camera.pos))
        }
        None => (false, false, "-".into(), "-".into()),
    };
    let (frame, extra, alpha) = match when {
        When::Step { frame, .. } => (frame, 0, 1.0),
        When::Between { alpha } => (crate::glide::FRAME.load(Ordering::Relaxed), 1, alpha),
    };
    let _ = writeln!(
        file,
        "{frame}\t{extra}\t{alpha:.3}\t{}\t{:.1}\t{}\t{}\t{:.1}\t{:.3}\t{:.1}\t{:.1}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{}\t{}\t{clear_game}\t{clear_drawn}",
        out.mode,
        out.distance,
        out.hit.map_or("-".to_string(), |h| format!("{h:.1}")),
        if out.hit.is_some() { out.hit_static.to_string() } else { "-".into() },
        out.target_pull,
        out.pull,
        out.drawn,
        out.min_distance,
        out.near,
        out.slab.map_or("-".to_string(), |z| format!("{z:.1}")),
        out.hit.is_some_and(|h| h < out.distance) as u8,
        out.hit.is_some_and(|h| h < out.drawn) as u8,
        cross_game as u8,
        cross_drawn as u8,
        secs * 1e6,
        out.cost.candidates,
        car.map_or("-".to_string(), |c| format!("{:.0},{:.0},{:.0}", c.pos[0], c.pos[1], c.pos[2]))
    );
}

/// `SR_CAMERA_PROBE=<every>` (diagnostics): the camera objects and the cars of the view
/// every N game frames.
fn probe(m: &mut Machine, cam: &GameCamera, slot: usize) {
    static AT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let Some(every) = *AT.get_or_init(|| std::env::var("SR_CAMERA_PROBE").ok().and_then(|v| v.trim().parse().ok())) else { return };
    let frame = crate::glide::FRAME.load(Ordering::Relaxed);
    if every == 0 || frame % every != 0 || slot != 0 || crate::interp::in_extra_draw() {
        return;
    }
    let obj = rd(m.img.link(VIEW_CAMERAS + 4 * slot as u32));
    if obj < 0x10000 || !mem::readable(obj, 0xb00) {
        return;
    }
    let v = super::transforms::world_to_view(cam.pos, cam.angles);
    crate::info!("camprobe {frame}: obj {obj:#x} mode {} car record {:#x} cam {cam:?}", rd(obj + CAMERA_MODE) as i32, rd(obj + CAMERA_CAR));
    for (n, inst) in car_instances(m).into_iter().enumerate() {
        if !mem::readable(inst, 0x20) {
            continue;
        }
        let i = |o: u32| rd(inst + o) as i32 as f64;
        let pos = [i(8), i(0xc), i(0x10)];
        let q = v.apply(pos);
        crate::info!("camprobe {frame}: car {n} inst {inst:#x} model {} pos {pos:?} view {:.0} {:.0} {:.0}", rd(inst), q[0], q[1], q[2]);
    }
    // a sanity check of the queries: straight down from the followed car's focus the road
    // must be hit (it faces up), straight up nothing facing down usually
    let mut st = std::mem::take(&mut m.native.camera);
    if let (Some(car), true) = (followed_car(m, &st, obj), st.world.is_some()) {
        let f = car.focus();
        let w = st.world.as_mut().unwrap();
        let mut cost = QueryCost::default();
        let down = w.first_hit(f, sub(f, [0.0, 2000.0, 0.0]), &mut cost);
        let up = w.first_hit(f, add(f, [0.0, 5000.0, 0.0]), &mut cost);
        crate::info!("camprobe {frame}: followed car at {:?}: down {down:?}, up {up:?} ({} candidates)", car.pos, cost.candidates);
    }
    m.native.camera = st;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::scene::Projection;

    /// A wall in the plane x = `x` from z0 to z1, y 0..h, its front side towards -x when
    /// `towards_minus_x` (the numerical normal of the corner order points to the front).
    fn wall(x: f64, z0: f64, z1: f64, h: f64, towards_minus_x: bool) -> Vec<([V3; 3], u32)> {
        let (a, b, c, d) = ([x, 0.0, z0], [x, 0.0, z1], [x, h, z1], [x, h, z0]);
        let mut t = vec![([a, b, c], 0), ([a, c, d], 0)];
        let n = cross(sub(t[0].0[1], t[0].0[0]), sub(t[0].0[2], t[0].0[0]));
        if (n[0] < 0.0) != towards_minus_x {
            for tri in t.iter_mut() {
                tri.0.swap(1, 2);
            }
        }
        t
    }

    #[test]
    fn segments_hit_walls_facing_their_start_only() {
        let mut w = World::new(&wall(500.0, -5000.0, 5000.0, 1000.0, true));
        let mut cost = QueryCost::default();
        // from the front side through the wall: hit at 500 units
        let h = w.first_hit([0.0, 100.0, 0.0], [2000.0, 100.0, 0.0], &mut cost).unwrap();
        assert!((h.0 - 500.0).abs() < 1e-6, "{h:?}");
        // from behind: the wall faces away, no hit
        assert!(w.first_hit([2000.0, 100.0, 0.0], [0.0, 100.0, 0.0], &mut cost).is_none());
        // short of the wall, past its top, beside it: none
        assert!(w.first_hit([0.0, 100.0, 0.0], [400.0, 100.0, 0.0], &mut cost).is_none());
        assert!(w.first_hit([0.0, 1100.0, 0.0], [2000.0, 1100.0, 0.0], &mut cost).is_none());
        assert!(w.first_hit([0.0, 100.0, 6000.0], [2000.0, 100.0, 6000.0], &mut cost).is_none());
        // across many grid cells, the nearest of two walls
        let mut tris = wall(3000.0, -500.0, 500.0, 1000.0, true);
        tris.extend(wall(1200.0, -500.0, 500.0, 1000.0, true));
        let mut w = World::new(&tris);
        let h = w.first_hit([-4000.0, 10.0, 0.0], [9000.0, 10.0, 0.0], &mut cost).unwrap();
        assert!((h.0 - 5200.0).abs() < 1e-6, "{h:?}");
    }

    fn camera_at(pos: V3) -> Camera {
        // looking along world +x: view x (right) = world -z, y (down) = world -y, z = +x
        Camera { pos, rot: [0.0, 0.0, -1.0, 0.0, -1.0, 0.0, 1.0, 0.0, 0.0], proj: Projection { center: [319.0, 312.0], scale: [256.0, 307.2], near: 128.0, far: 50000.0 } }
    }

    #[test]
    fn the_near_slab_finds_walls_beside_the_eye() {
        let k = [-319.0 / 256.0, 321.0 / 256.0, -312.0 / 307.2, 168.0 / 307.2];
        let mut cost = QueryCost::default();
        // a wall along the view direction 100 units to the right, facing the eye: the game's
        // near plane (128, half width 160) cuts it; it enters the view at depth 100 * 256 /
        // 321 = 79.8
        let mut w = World::new(&wall_z(-100.0, -2000.0, 4000.0, true));
        let (near, cross) = w.near_slab(&camera_at([0.0, 300.0, 0.0]), k, 136.0, 128.0, &mut cost);
        assert!(cross);
        assert!((near.unwrap() - 100.0 * 256.0 / 321.0).abs() < 1e-6, "{near:?}");
        // facing away: ignored (culled anyway)
        let mut w = World::new(&wall_z(-100.0, -2000.0, 4000.0, false));
        assert_eq!(w.near_slab(&camera_at([0.0, 300.0, 0.0]), k, 136.0, 128.0, &mut cost), (None, false));
        // 400 units to the side: outside the slab
        let mut w = World::new(&wall_z(-400.0, -2000.0, 4000.0, true));
        assert_eq!(w.near_slab(&camera_at([0.0, 300.0, 0.0]), k, 136.0, 128.0, &mut cost), (None, false));
    }

    /// A wall in the plane z = `z` (world), x from x0 to x1, y 0..1000, facing +z (towards
    /// the camera's left... ) when `towards_plus_z`.
    fn wall_z(z: f64, x0: f64, x1: f64, towards_plus_z: bool) -> Vec<([V3; 3], u32)> {
        let (a, b, c, d) = ([x0, 0.0, z], [x1, 0.0, z], [x1, 1000.0, z], [x0, 1000.0, z]);
        let mut t = vec![([a, b, c], 0), ([a, c, d], 0)];
        let n = cross(sub(t[0].0[1], t[0].0[0]), sub(t[0].0[2], t[0].0[0]));
        if (n[2] > 0.0) != towards_plus_z {
            for tri in t.iter_mut() {
                tri.0.swap(1, 2);
            }
        }
        t
    }

    #[test]
    fn pull_in_is_fast_release_is_slow_and_cuts_start_at_the_target() {
        let period = 1.0 / 33.0;
        let mut t = Track::default();
        t.step(10, 6, [0.0; 3], None, t.step_pull(10, 6, [0.0; 3], 0.0, period), 128.0, period);
        assert_eq!(t.pull, 0.0);
        // an obstacle: 400 units of pull-in wanted
        let p = t.step_pull(11, 6, [0.0; 3], 400.0, period);
        assert!((p - 400.0 * factor(TAU_IN, period)).abs() < 1e-9 && p > 150.0);
        t.step(11, 6, [0.0; 3], None, p, 60.0, period);
        // the near plane came in at once
        assert_eq!(t.near, 60.0);
        for f in 12..20 {
            let p = t.step_pull(f, 6, [0.0; 3], 400.0, period);
            t.step(f, 6, [0.0; 3], None, p, 60.0, period);
        }
        assert!(t.pull > 395.0);
        // gone: let go slowly, the near plane goes back slowly too
        let p = t.step_pull(20, 6, [0.0; 3], 0.0, period);
        assert!(p > 300.0 && p < 400.0);
        t.step(20, 6, [0.0; 3], None, p, 128.0, period);
        assert!(t.near > 60.0 && t.near < 128.0);
        // an extra frame halfway between the steps
        let (bp, bn, _) = t.between(0.5);
        assert!((bp - (t.prev_pull.unwrap() + t.pull) / 2.0).abs() < 1e-9 && bn < t.near);
        // the release ends exactly at the game's camera (pull 0, the game's near plane)
        let mut r = t.clone();
        for f in 21..200 {
            let p = r.step_pull(f, 6, [0.0; 3], 0.0, period);
            r.step(f, 6, [0.0; 3], None, p, 128.0, period);
        }
        assert_eq!((r.pull, r.near), (0.0, 128.0));
        // a camera change (mode) is a cut: straight to the target
        let p = t.step_pull(21, 3, [0.0; 3], 0.0, period);
        assert_eq!(p, 0.0);
        t.step(21, 3, [0.0; 3], None, p, 128.0, period);
        assert_eq!((t.near, t.prev_pull), (128.0, None));
        // the same step drawn again changes nothing
        assert_eq!(t.step_pull(21, 3, [0.0; 3], 500.0, period), 0.0);
    }

    #[test]
    fn the_eye_stays_out_of_the_car() {
        let b = CarBox { lo: [-300.0, -100.0, -750.0], hi: [300.0, 400.0, 750.0] };
        // from the focus (150 up) straight back: leaves through the rear at 750
        assert!((b.exit([0.0, 150.0, 0.0], [0.0, 0.0, 1.0]) - 750.0).abs() < 1e-9);
        // rising at 45 degrees: through the top after 250 * sqrt(2)
        let s = 0.5f64.sqrt();
        assert!((b.exit([0.0, 150.0, 0.0], [0.0, s, s]) - 250.0 / s).abs() < 1e-6);
        // from outside, missing it: 0
        assert_eq!(b.exit([0.0, 1000.0, 0.0], [0.0, 1.0, 0.0]), 0.0);
    }

    /// The near plane in `solve`: a wall beside the eye inside the view pulls it in (to
    /// stay in front of the wall); the mirror keeps the game's plane while its own car is
    /// between the eye and that plane.
    #[test]
    fn the_near_plane_moves_in_front_of_a_wall_beside_the_eye() {
        let boxes = HashMap::from([(7, CarBox { lo: [-300.0, -100.0, -750.0], hi: [300.0, 400.0, 750.0] })]);
        // the camera at (0, 300, 0) looking along +x; the wall 100 units to its right
        let mut w = World::new(&wall_z(-100.0, -2000.0, 4000.0, true));
        let view = |mirror: bool| View { camera: camera_at([0.0, 300.0, 0.0]), viewport: [0.0, 0.0, 640.0, 480.0], mirror, clear: None, fog: None, sky: None };
        let far_car = CarPose { model: 7, pos: [5000.0, 0.0, 0.0], angles: [0.0; 3] };
        let mut v = view(false);
        let out = solve(&mut w, &boxes, &mut Track::default(), &mut v, Some(far_car), 6, false, 0.0, When::Step { frame: 3, cam: [0.0; 3], period: 0.03 });
        let z = 100.0 * 256.0 / 321.0;
        assert!((out.near - (z - NEAR_MARGIN)).abs() < 1e-9 && (v.camera.proj.near as f64 - out.near).abs() < 1e-4, "{out:?}");
        // the mirror with its car right in front of the eye: the game's plane
        // (the box is 600 wide along world x: from 50 to 650 units in front of the eye)
        let near_car = CarPose { model: 7, pos: [350.0, 0.0, 0.0], angles: [0.0; 3] };
        let mut v = view(true);
        let out = solve(&mut w, &boxes, &mut Track::default(), &mut v, Some(near_car), -1, false, 0.0, When::Step { frame: 3, cam: [0.0; 3], period: 0.03 });
        assert_eq!((out.near, v.camera.proj.near), (NEAR_GAME, NEAR_GAME as f32));
        // nothing within the game's plane: unchanged
        let mut w = World::new(&wall_z(-900.0, -2000.0, 4000.0, true));
        let mut v = view(false);
        let out = solve(&mut w, &boxes, &mut Track::default(), &mut v, Some(far_car), 6, false, 0.0, When::Step { frame: 3, cam: [0.0; 3], period: 0.03 });
        assert_eq!(out.near, NEAR_GAME);
    }

    /// The whole correction on a synthetic scene: a chase camera behind a car whose eye is
    /// behind a wall facing the car comes in front of it; with the wall gone it stays.
    #[test]
    fn the_drawn_eye_comes_in_front_of_the_wall() {
        let mut boxes = HashMap::new();
        boxes.insert(7, CarBox { lo: [-300.0, -100.0, -750.0], hi: [300.0, 400.0, 750.0] });
        // the car at the origin facing -z (heading 0), the camera behind it along +z
        let car = CarPose { model: 7, pos: [0.0; 3], angles: [0.0; 3] };
        let mut w = World::new(&wall_z(1300.0, -3000.0, 3000.0, false));
        let eye = [0.0, 450.0, 1500.0];
        let mut view = View { camera: Camera { pos: eye, rot: [-1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, -1.0], proj: camera_at([0.0; 3]).proj }, viewport: [0.0, 0.0, 640.0, 480.0], mirror: false, clear: None, fog: None, sky: None };
        let mut t = Track::default();
        let out = solve(&mut w, &boxes, &mut t, &mut view, Some(car), 6, true, 0.0, When::Step { frame: 5, cam: eye, period: 0.03 });
        let focus = [0.0, FOCUS_HEIGHT, 0.0];
        let d0 = len(sub(eye, focus));
        assert!((out.distance - d0).abs() < 1e-9);
        let h = out.hit.unwrap();
        assert!(h < out.distance && out.drawn <= h - MARGIN + 1e-6, "{out:?}");
        // still on the segment, and in front of the wall (z < 1300)
        assert!(view.camera.pos[2] < 1300.0 - MARGIN * 0.9 && (len(sub(view.camera.pos, focus)) - out.drawn).abs() < 1e-6);
        // the wall appearing on a step that follows on (smoothed, not a cut), so near the car
        // that the target is limited by the car's box: the eye is still never behind it
        let mut t = Track::default();
        let mut w0 = World::new(&wall_z(-5000.0, -3000.0, 3000.0, true));
        view.camera.pos = eye;
        solve(&mut w0, &boxes, &mut t, &mut view, Some(car), 6, true, 0.0, When::Step { frame: 5, cam: eye, period: 0.03 });
        let mut w = World::new(&wall_z(830.0, -3000.0, 3000.0, false));
        view.camera.pos = eye;
        let out = solve(&mut w, &boxes, &mut t, &mut view, Some(car), 6, true, 0.0, When::Step { frame: 6, cam: eye, period: 0.03 });
        let h = out.hit.unwrap();
        assert!(out.min_distance > h - MARGIN && out.drawn <= h + 1e-6 && view.camera.pos[2] <= 830.0 + 1e-6, "{out:?}");
        // without the wall: the game's eye
        let mut w = World::new(&wall_z(-5000.0, -3000.0, 3000.0, true));
        view.camera.pos = eye;
        let mut t = Track::default();
        let out = solve(&mut w, &boxes, &mut t, &mut view, Some(car), 6, true, 0.0, When::Step { frame: 5, cam: eye, period: 0.03 });
        assert_eq!((out.hit, out.pull), (None, 0.0));
        assert!(len(sub(view.camera.pos, eye)) < 1e-9);
        assert_eq!(view.camera.proj.near, NEAR_GAME as f32);
    }
}
