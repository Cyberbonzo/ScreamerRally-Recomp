//! Which moving objects the native renderer draws in a race (`docs/re/native/visibility.md`,
//! "Moving objects"; the decision: `docs/04-native-renderer.md`, "Visibility, revised":
//! nothing floats).
//!
//! The game draws every scene instance (cars, their shadows, smoke, tyre marks) within its far
//! distance, wherever the visibility lists leave the world undrawn: an AI car on a far stretch
//! of road that is not drawn hangs in the sky (the original does the same, Rally China frame
//! 6750 with `graphics.visibility = original`). The statics follow "backed by the world"
//! (`native::visibility`); a moving object follows the same principle through what it stands
//! on: **it is drawn only while the world under it is drawn** in that view.
//!
//! * **The world under it** ([`GroundMap`]): the statics' surfaces (opaque and colour-keyed
//!   triangles, world space, bucketed by x / z once per race) under the object's position
//!   within [`SUPPORT_DROP`] below and [`SUPPORT_RISE`] above it; the highest one and those
//!   within [`SUPPORT_LAYER`] of it are its support. Drawn when one of their statics has a
//!   fade above 0 in this view's `static_fade` (the same frame's: no latency). A car on the
//!   road under a crest, seen from below with the sky behind it, stands on drawn road and is
//!   drawn; a car on a stretch of road that is not drawn is not, whatever lies behind it.
//! * **Never hidden**: the player's car, and any object on screen within
//!   [`NEAR_KEEP`](super::visibility::NEAR_KEEP) of the eye (to its bounding sphere): drawn at
//!   full whatever the rule says.
//! * **Fades** as the statics': an object that starts or stops being drawn while on screen
//!   fades in over [`FADE_STEPS`] / out over [`FADE_OUT_STEPS`] simulation steps (screen-door,
//!   `FrameData::dynamic_fade`); off screen, a new object (a puff spawned, a car listed again)
//!   or one that jumped (a reused puff) takes its state at once.
//!
//! **The floating detector** (diagnostics, with `SR_NATIVE_VIS_LOG`): every moving object on
//! screen is tested on the GPU like the statics' bases (`FrameData::dynamic_backing`, test 3:
//! its surfaces below [`BASE_FRACTION`] of its height against every static drawn); a drawn
//! object whose base shows the sky in at least [`FLOATING_SKY`] of its visible pixels is
//! counted as floating (`native dyn:` lines).

use std::collections::{HashMap, VecDeque};

use super::dynamics::Kind;
use super::scene::{BackingCounts, DynamicBacking, Layer, SceneData, Transparency, View};
use super::visibility::{FADE_OUT_STEPS, FADE_STEPS, FLOATING_SKY, Frustum, NEAR_KEEP};

/// Bucket size (x, z) of the ground map.
const GRID: f64 = 1024.0;
/// A surface supports an object whose position lies at most this far above it ...
pub const SUPPORT_DROP: f64 = 4000.0;
/// ... or this far below it (an origin under the surface drawn).
pub const SUPPORT_RISE: f64 = 300.0;
/// Surfaces within this height under the highest one count as the support too (overlapping
/// statics, seams).
pub const SUPPORT_LAYER: f64 = 100.0;
/// The detector's base: the object's surfaces up to this share of its height above its
/// lowest point (at least [`BASE_MIN`] units).
pub const BASE_FRACTION: f64 = 0.3;
pub const BASE_MIN: f64 = 30.0;
/// An object not listed for this many views is forgotten.
const FORGET: u64 = 200;
/// Moves longer than this between two views are a new object (a reused puff or mark).
const JUMP: f64 = 4000.0;

/// `SR_DYN_VIS_PROBE=<frame>` (diagnostics): every moving object of that frame's views in the
/// log (position, support, fade, test counts).
pub(crate) fn probe_at() -> Option<u64> {
    static AT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *AT.get_or_init(|| std::env::var("SR_DYN_VIS_PROBE").ok().and_then(|v| v.trim().parse().ok()))
}

/// `SR_NATIVE_DYN_VIS=0` (diagnostics, comparisons): moving objects drawn wherever they are
/// (as before 2026-11-21); the detector still runs with the log.
pub fn rule_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_NATIVE_DYN_VIS").map_or(true, |v| v.trim() != "0"))
}

// ---------------------------------------------------------------------------------------
// The world under an object

/// What lies under a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// A drawn static's surface.
    Drawn,
    /// Surfaces of statics not drawn in this view.
    Undrawn,
    /// No surface within the band.
    None,
}

/// The statics' surfaces by (x, z) bucket (built once per race).
#[derive(Clone, Debug, Default)]
pub struct GroundMap {
    /// Triangles in world space and their static.
    tris: Vec<([[f32; 3]; 3], u32)>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
}

fn bucket(v: f64) -> i32 {
    (v / GRID).floor() as i32
}

impl GroundMap {
    /// The opaque and colour-keyed triangles of every static (what can stand under
    /// something; blended, decal and sky surfaces are not ground).
    pub fn new(scene: &SceneData) -> GroundMap {
        let mut g = GroundMap::default();
        for (si, s) in scene.statics.iter().enumerate() {
            let Some(mesh) = scene.meshes.get(s.mesh as usize) else { continue };
            for r in &mesh.ranges {
                let solid = scene.materials.get(r.material as usize).is_some_and(|m| matches!(m.layer, Layer::World | Layer::Water) && !matches!(m.transparency, Transparency::Blended(_)));
                if !solid {
                    continue;
                }
                for t in mesh.indices.get(r.indices.start as usize..r.indices.end as usize).unwrap_or(&[]).chunks_exact(3) {
                    let p = |i: u32| mesh.vertices.get(i as usize).map(|v| s.transform.apply(v.pos));
                    let (Some(a), Some(b), Some(c)) = (p(t[0]), p(t[1]), p(t[2])) else { continue };
                    // vertical triangles support nothing
                    let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
                    if d.abs() < 1e-3 {
                        continue;
                    }
                    let k = g.tris.len() as u32;
                    g.tris.push(([a, b, c].map(|v| v.map(|x| x as f32)), si as u32));
                    let (x0, x1) = (a[0].min(b[0]).min(c[0]), a[0].max(b[0]).max(c[0]));
                    let (z0, z1) = (a[2].min(b[2]).min(c[2]), a[2].max(b[2]).max(c[2]));
                    for bz in bucket(z0)..=bucket(z1).min(bucket(z0) + 64) {
                        for bx in bucket(x0)..=bucket(x1).min(bucket(x0) + 64) {
                            g.buckets.entry((bx, bz)).or_default().push(k);
                        }
                    }
                }
            }
        }
        g
    }

    pub fn triangles(&self) -> usize {
        self.tris.len()
    }

    /// The support of a point: the surfaces under it within the band (heights from
    /// `p.y - SUPPORT_DROP` to `p.y + SUPPORT_RISE`), the highest and those within
    /// [`SUPPORT_LAYER`] of it; drawn when one of their statics is. Also the highest surface's
    /// height.
    pub fn support(&self, p: [f64; 3], drawn: impl Fn(u32) -> bool) -> (Support, Option<f64>) {
        let (owners, top) = self.surfaces(p);
        (support_of(&owners, drawn), top)
    }

    /// The statics whose surfaces under `p` are its support (the highest within the band and
    /// those within [`SUPPORT_LAYER`] of it), and the highest surface's height.
    pub fn surfaces(&self, p: [f64; 3]) -> (Vec<u32>, Option<f64>) {
        let Some(list) = self.buckets.get(&(bucket(p[0]), bucket(p[2]))) else { return (Vec::new(), None) };
        let (lo, hi) = (p[1] - SUPPORT_DROP, p[1] + SUPPORT_RISE);
        let mut found: Vec<(f64, u32)> = Vec::new();
        for &k in list {
            let (t, owner) = &self.tris[k as usize];
            if let Some(h) = height_at(t, p[0], p[2]).filter(|h| (lo..=hi).contains(h)) {
                found.push((h, *owner));
            }
        }
        let Some(top) = found.iter().map(|f| f.0).reduce(f64::max) else { return (Vec::new(), None) };
        let mut owners: Vec<u32> = found.iter().filter(|f| f.0 >= top - SUPPORT_LAYER).map(|f| f.1).collect();
        owners.sort_unstable();
        owners.dedup();
        (owners, Some(top))
    }
}

/// The support given its statics ([`GroundMap::surfaces`]) and which statics are drawn.
fn support_of(owners: &[u32], drawn: impl Fn(u32) -> bool) -> Support {
    if owners.is_empty() {
        Support::None
    } else if owners.iter().any(|&s| drawn(s)) {
        Support::Drawn
    } else {
        Support::Undrawn
    }
}

/// An object's support is looked up again when it has moved this far (world units) since.
const REQUERY: f64 = 64.0;

/// The height of a triangle at (x, z), when (x, z) lies in its projection.
fn height_at(t: &[[f32; 3]; 3], x: f64, z: f64) -> Option<f64> {
    let [a, b, c] = t.map(|v| v.map(|k| k as f64));
    let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
    if d.abs() < 1e-6 {
        return None;
    }
    let l1 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / d;
    let l2 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / d;
    let l3 = 1.0 - l1 - l2;
    (l1 >= -1e-6 && l2 >= -1e-6 && l3 >= -1e-6).then(|| l1 * a[1] + l2 * b[1] + l3 * c[1])
}

// ---------------------------------------------------------------------------------------
// Per view

/// One moving object (scene instance) of a view.
#[derive(Clone, Copy, Debug)]
pub struct Obj {
    /// Instance A (identity across frames).
    pub addr: u32,
    pub kind: Kind,
    /// Position (interpolated in extra frames) and bounding radius (model +0).
    pub pos: [f64; 3],
    pub radius: f64,
    /// Its lowest and highest world height (the detector's base), when tested.
    pub heights: Option<(f64, f64)>,
}

/// What the view needs.
pub struct Ctx<'a> {
    pub view: &'a View,
    /// View slot (0 main, 1 second player, 2 mirror).
    pub slot: usize,
    /// The view's statics' fades (`FrameData::static_fade`).
    pub static_fade: &'a [u8],
    /// The fades' clock (simulation steps, fractional between them).
    pub t: f64,
    /// An interpolated extra frame (no statistics, no tests).
    pub extra: bool,
    /// The widescreen extension (game pixels).
    pub ext: f32,
    /// The player's car (instance A), if any.
    pub player: Option<u32>,
    /// Ask for the detector's test (`FrameData::dynamic_backing`), and the frame's results tag.
    pub test: bool,
    pub tag: u64,
    /// For the log.
    pub frame: u64,
}

#[derive(Clone, Debug, Default)]
struct ObjState {
    fade: f32,
    /// The rule's target (drawn) at the last update.
    target: bool,
    pos: [f64; 3],
    /// The update it was last listed in.
    seen: u64,
    /// The newest test result and its tag.
    counts: Option<(u64, BackingCounts)>,
    /// The statics under it and the highest surface's height, looked up at this position.
    ground: Option<([f64; 3], Vec<u32>, Option<f64>)>,
}

#[derive(Default)]
struct ViewState {
    objs: HashMap<u32, ObjState>,
    t: Option<f64>,
    updates: u64,
    /// Per results tag sent: the tested objects' instances (index = object).
    sent: VecDeque<(u64, Vec<u32>)>,
    /// The newest results tag applied.
    newest: u64,
}

/// A view's report (diagnostics).
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub objects: u32,
    pub onscreen: u32,
    /// On screen and not fully drawn (fading or not drawn), and of those cars.
    pub hidden: u32,
    pub hidden_cars: u32,
    /// Drawn on screen with the newest result showing the base against the sky: (kind,
    /// instance, distance, sky, visible, the world under it drawn: at a silhouette); of them
    /// cars, and cars with the world under them not drawn (floating in the rule's terms).
    pub floating: Vec<(Kind, u32, f32, u32, u32, bool)>,
    pub floating_cars: u32,
    pub floating_cars_ungrounded: u32,
    /// Floating while fading out (being removed).
    pub leaving: u32,
    /// Must stay 0: near objects on screen or the player's car not fully drawn.
    pub near_hidden: u32,
    pub player_hidden: u32,
    /// Listed objects with no surface under them, and on-screen objects hidden although
    /// their newest result shows their base backed (to check: a hidden object that would not
    /// float).
    pub unsupported: u32,
    pub hidden_backed: u32,
    /// Per kind (car, shadow, marks, smoke): position height above the support's surface
    /// (calibration).
    pub gaps: Vec<(Kind, f64)>,
    /// By kind (car, shadow, marks, smoke and others): on screen not fully drawn, no
    /// surface under them, floating (drawn).
    pub hidden_by: [u32; 4],
    pub unsupported_by: [u32; 4],
    pub floating_by: [u32; 4],
}

/// The report's kind index (car, shadow, marks, smoke and others).
pub fn kind_index(k: Kind) -> usize {
    match k {
        Kind::Car => 0,
        Kind::Shadow => 1,
        Kind::Marks => 2,
        _ => 3,
    }
}

/// The rule's state per view slot, and the ground map of the race.
#[derive(Default)]
pub struct DynVis {
    ground: Option<GroundMap>,
    views: [ViewState; 3],
}

impl DynVis {
    /// For a race (`ground`); without it (the menus' scenes) every object is drawn.
    pub fn new(ground: Option<GroundMap>) -> DynVis {
        DynVis { ground, ..Default::default() }
    }

    /// Test results of a view: per tested object, its counts (`BackingResults::dynamics`).
    pub fn receive(&mut self, slot: usize, tag: u64, results: Vec<(u32, BackingCounts)>) {
        let Some(v) = self.views.get_mut(slot) else { return };
        if tag <= v.newest {
            return;
        }
        let Some(k) = v.sent.iter().position(|s| s.0 == tag) else { return };
        let (_, addrs) = v.sent.remove(k).unwrap();
        v.newest = tag;
        for (o, c) in results {
            if let Some(st) = addrs.get(o as usize).and_then(|a| v.objs.get_mut(a)) {
                st.counts = Some((tag, c));
            }
        }
    }

    /// One drawn view: per object its fade (0..255) and its test entry; the report.
    pub fn update(&mut self, ctx: &Ctx, objs: &[Obj]) -> (Vec<u8>, Vec<DynamicBacking>, Report) {
        let mut rep = Report::default();
        let ground = self.ground.as_ref();
        let Some(v) = self.views.get_mut(ctx.slot.min(2)) else { return (vec![255; objs.len()], Vec::new(), rep) };
        let rule = ground.is_some() && rule_on();
        let dt = v.t.map_or(0.0, |t0| (ctx.t - t0).max(0.0));
        v.t = Some(v.t.map_or(ctx.t, |t0| t0.max(ctx.t)));
        v.updates += 1;
        let now = v.updates;
        let frustum = Frustum::of(ctx.view, ctx.ext);
        let eye = ctx.view.camera.pos;
        let mut fades = Vec::with_capacity(objs.len());
        let mut tests = Vec::with_capacity(objs.len());
        let mut tested: Vec<u32> = Vec::new();
        for o in objs {
            let on_screen = frustum.sphere(o.pos, o.radius as f32);
            let dist = ((0..3).map(|k| (o.pos[k] - eye[k]).powi(2)).sum::<f64>().sqrt() - o.radius).max(0.0);
            let player = ctx.player == Some(o.addr);
            let protected = player || (on_screen && dist <= NEAR_KEEP);
            let st = v.objs.entry(o.addr).or_insert(ObjState { fade: -1.0, pos: o.pos, ..Default::default() });
            // the statics under it (looked up again when it has moved)
            let (support, top) = match ground {
                Some(g) => {
                    let moved = st.ground.as_ref().is_none_or(|(at, _, _)| (0..3).map(|k| (o.pos[k] - at[k]).powi(2)).sum::<f64>() > REQUERY * REQUERY);
                    if moved {
                        let (owners, top) = g.surfaces(o.pos);
                        st.ground = Some((o.pos, owners, top));
                    }
                    let (_, owners, top) = st.ground.as_ref().unwrap();
                    (support_of(owners, |s| ctx.static_fade.get(s as usize).is_some_and(|&f| f > 0)), *top)
                }
                None => (Support::Drawn, None),
            };
            let target = !rule || protected || support == Support::Drawn;
            if st.fade < 0.0 {
                // new: its state at once
                st.fade = target as u8 as f32;
            }
            let fresh = st.seen + 1 != now && st.seen != now || (0..3).map(|k| (o.pos[k] - st.pos[k]).powi(2)).sum::<f64>().sqrt() > JUMP;
            if fresh {
                // listed again, spawned or reused: its state at once
                st.fade = target as u8 as f32;
                st.counts = None;
            }
            st.target = target;
            st.pos = o.pos;
            st.seen = now;
            let goal = target as u8 as f32;
            st.fade = if protected {
                1.0
            } else if !on_screen {
                goal
            } else if st.fade < goal {
                (st.fade + (dt / FADE_STEPS as f64) as f32).min(goal)
            } else {
                (st.fade - (dt / FADE_OUT_STEPS as f64) as f32).max(goal)
            };
            fades.push(if st.fade > 0.0 { ((st.fade * 255.0).round() as u8).max(1) } else { 0 });
            // the detector's test: objects on screen (drawn or not)
            let test = match o.heights {
                Some((lo, hi)) if ctx.test && on_screen => {
                    tested.push(o.addr);
                    DynamicBacking { object: tested.len() as u32 - 1, base_top: lo + (BASE_FRACTION * (hi - lo)).max(BASE_MIN) }
                }
                _ => DynamicBacking::NONE,
            };
            tests.push(test);
            // statistics (the game's own frames)
            if ctx.extra {
                continue;
            }
            if probe_at() == Some(ctx.frame) {
                crate::info!(
                    "native dyn probe frame {} slot {}: {:?} {:x} pos {:.0?} radius {:.0} heights {:.0?} on screen {on_screen} dist {dist:.0} player {player} support {support:?} top {top:.0?} fade {:.2} counts {:?}",
                    ctx.frame,
                    ctx.slot,
                    o.kind,
                    o.addr,
                    o.pos,
                    o.radius,
                    o.heights,
                    st.fade,
                    st.counts
                );
            }
            rep.objects += 1;
            if let Some(h) = top {
                rep.gaps.push((o.kind, o.pos[1] - h));
            } else {
                rep.unsupported += 1;
                rep.unsupported_by[kind_index(o.kind)] += 1;
            }
            if !on_screen {
                continue;
            }
            rep.onscreen += 1;
            if st.fade < 1.0 {
                rep.hidden += 1;
                rep.hidden_cars += (o.kind == Kind::Car) as u32;
                rep.hidden_by[kind_index(o.kind)] += 1;
                rep.near_hidden += (dist <= NEAR_KEEP) as u32;
                rep.player_hidden += player as u32;
            }
            let newest = st.counts.filter(|c| c.0 == v.newest).map(|c| c.1);
            if let Some(c) = newest {
                let floating = c.tested & 1 != 0 && c.visible[0] > 0 && c.sky[0] as f32 >= FLOATING_SKY * c.visible[0] as f32;
                if st.fade > 0.0 && floating {
                    if target {
                        rep.floating.push((o.kind, o.addr, dist as f32, c.sky[0], c.visible[0], support == Support::Drawn));
                        rep.floating_cars += (o.kind == Kind::Car) as u32;
                        rep.floating_cars_ungrounded += (o.kind == Kind::Car && support != Support::Drawn) as u32;
                        rep.floating_by[kind_index(o.kind)] += 1;
                    } else {
                        rep.leaving += 1;
                    }
                }
                let backed = c.visible[0] > 0 && (c.sky[0] as f32) <= super::visibility::ACCEPT_SKY * c.visible[0] as f32;
                if st.fade == 0.0 && backed {
                    rep.hidden_backed += 1;
                }
            }
        }
        if !tested.is_empty() {
            v.sent.push_back((ctx.tag, tested));
            while v.sent.len() > 16 {
                v.sent.pop_front();
            }
        }
        // forget objects no longer listed
        if now % 64 == 0 {
            v.objs.retain(|_, s| now - s.seen < FORGET);
        }
        (fades, if ctx.test { tests } else { Vec::new() }, rep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::frame::{GameCamera, ViewMap, camera};
    use crate::native::scene::{Instance, Material, Mesh, MeshRange, Transform, Vertex};

    fn view() -> View {
        // heading 0: looking along world -z from (0, 300, 0)
        let cam = GameCamera { angles: [0.0; 3], pos: [0.0, 300.0, 0.0] };
        let map = ViewMap::race_main(0.0);
        View { camera: camera(&cam, &map, 100000.0), viewport: map.viewport(), mirror: false, clear: None, fog: None, sky: None }
    }

    /// Two flat road pieces (statics 0 and 1) ahead along -z, at height 0: 0 from z = 0 to
    /// -20000, 1 from -20000 to -40000; a blended one (static 2) at 0 from -40000 to -60000;
    /// a bridge (static 3) at 2000 over the first.
    fn scene() -> SceneData {
        let quad = |z0: f32, z1: f32, y: f32| Mesh {
            vertices: [[-2000.0, y, z0], [2000.0, y, z0], [2000.0, y, z1], [-2000.0, y, z1]].iter().map(|&pos| Vertex { pos, ..Default::default() }).collect(),
            indices: vec![0, 1, 2, 0, 2, 3],
            ranges: vec![MeshRange { material: 0, indices: 0..6 }],
        };
        let mut blended = quad(-40000.0, -60000.0, 0.0);
        blended.ranges[0].material = 1;
        SceneData {
            materials: vec![Material::opaque(0), Material::blended(None, crate::native::scene::Blend::ALPHA)],
            meshes: vec![quad(0.0, -20000.0, 0.0), quad(-20000.0, -40000.0, 0.0), blended, quad(-5000.0, -6000.0, 2000.0)],
            statics: (0..4).map(|m| Instance::new(m, Transform::IDENTITY)).collect(),
            ..Default::default()
        }
    }

    fn obj(addr: u32, z: f64) -> Obj {
        Obj { addr, kind: Kind::Car, pos: [0.0, 100.0, z], radius: 200.0, heights: Some((0.0, 300.0)) }
    }

    fn ctx<'a>(v: &'a View, fade: &'a [u8], t: f64) -> Ctx<'a> {
        Ctx { view: v, slot: 0, static_fade: fade, t, extra: false, ext: 0.0, player: None, test: true, tag: t as u64 + 1, frame: 0 }
    }

    #[test]
    fn the_ground_under_a_point() {
        let g = GroundMap::new(&scene());
        // the blended piece is not ground
        assert_eq!(g.triangles(), 6);
        let all = |_| true;
        assert_eq!(g.support([0.0, 100.0, -10000.0], all), (Support::Drawn, Some(0.0)));
        assert_eq!(g.support([0.0, 100.0, -10000.0], |s| s != 0), (Support::Undrawn, Some(0.0)));
        // under the bridge: the road (the bridge is above the band)
        assert_eq!(g.support([0.0, 100.0, -5500.0], |s| s == 0).0, Support::Drawn);
        // on the bridge: the bridge
        assert_eq!(g.support([0.0, 2100.0, -5500.0], |s| s == 3).0, Support::Drawn);
        assert_eq!(g.support([0.0, 2100.0, -5500.0], |s| s == 0).0, Support::Undrawn);
        // over the blended piece and beside the road: nothing
        assert_eq!(g.support([0.0, 100.0, -50000.0], all).0, Support::None);
        assert_eq!(g.support([5000.0, 100.0, -10000.0], all).0, Support::None);
        // far above the road (beyond SUPPORT_DROP): nothing
        assert_eq!(g.support([0.0, 100.0 + SUPPORT_DROP, -10000.0], all).0, Support::None);
    }

    #[test]
    fn objects_on_undrawn_ground_fade_out_near_ones_and_the_player_never() {
        let v = view();
        let mut d = DynVis::new(Some(GroundMap::new(&scene())));
        // 1 at 30 000 on the second piece, 2 at 3 000 on the first, 3 the player at 35 000
        let objs = [obj(1, -30000.0), obj(2, -3000.0), obj(3, -35000.0)];
        let drawn = [255u8, 255, 0, 0];
        let (f, tests, rep) = d.update(&Ctx { player: Some(3), ..ctx(&v, &drawn, 0.0) }, &objs);
        assert_eq!(f, vec![255, 255, 255]);
        assert_eq!(tests.iter().map(|t| t.object).collect::<Vec<_>>(), vec![0, 1, 2], "every object on screen tested");
        assert_eq!(tests[0].base_top, 90.0, "lowest + 30 % of the height");
        assert_eq!((rep.onscreen, rep.hidden), (3, 0));
        // the second piece is no longer drawn: 1 fades out over FADE_OUT_STEPS, the player stays
        let undrawn = [255u8, 0, 0, 0];
        let (f, _, rep) = d.update(&Ctx { player: Some(3), ..ctx(&v, &undrawn, 1.0) }, &objs);
        assert_eq!(f, vec![191, 255, 255]);
        assert_eq!((rep.hidden, rep.near_hidden, rep.player_hidden), (1, 0, 0));
        let (f, _, _) = d.update(&Ctx { player: Some(3), ..ctx(&v, &undrawn, 1.0 + FADE_OUT_STEPS as f64) }, &objs);
        assert_eq!(f, vec![0, 255, 255]);
        // nothing under the near one either: still drawn (near and on screen)
        let none = [0u8; 4];
        let (f, _, rep) = d.update(&ctx(&v, &none, 6.0), &objs);
        assert_eq!((f[1], rep.near_hidden), (255, 0));
        // the player's car without the player: hidden like any other far object (fades out)
        assert!(f[2] < 255);
        // drawn again: fades in over FADE_STEPS
        let (f, _, _) = d.update(&ctx(&v, &drawn, 7.0), &objs);
        assert_eq!(f[0], 32);
        let (f, _, _) = d.update(&ctx(&v, &drawn, 7.0 + FADE_STEPS as f64), &objs);
        assert_eq!(f[0], 255);
    }

    #[test]
    fn new_and_off_screen_objects_take_their_state_at_once() {
        let v = view();
        let mut d = DynVis::new(Some(GroundMap::new(&scene())));
        let undrawn = [255u8, 0, 0, 0];
        // a new object on undrawn ground: not drawn at once (no fade from full)
        let (f, _, _) = d.update(&ctx(&v, &undrawn, 0.0), &[obj(1, -30000.0)]);
        assert_eq!(f, vec![0]);
        // behind the camera: off screen, takes its target at once (and is not tested)
        let behind = Obj { pos: [0.0, 100.0, 30000.0], ..obj(2, 0.0) };
        let (f, tests, _) = d.update(&ctx(&v, &[0u8; 4], 1.0), &[behind]);
        assert_eq!((f, tests[0].object), (vec![0], DynamicBacking::NONE.object));
        // a jump (a reused puff): its new state at once
        let (f, _, _) = d.update(&ctx(&v, &undrawn, 2.0), &[obj(1, -30000.0)]);
        assert_eq!(f, vec![0]);
        let (f, _, _) = d.update(&ctx(&v, &undrawn, 3.0), &[obj(1, -10000.0)]);
        assert_eq!(f, vec![255]);
    }

    #[test]
    fn the_detector_counts_drawn_objects_whose_base_shows_the_sky() {
        let v = view();
        let mut d = DynVis::new(Some(GroundMap::new(&scene())));
        let drawn = [255u8, 255, 0, 0];
        let objs = [obj(1, -30000.0), obj(2, -10000.0)];
        let (_, _, _) = d.update(&ctx(&v, &drawn, 0.0), &objs);
        // tag 1: object 0 (instance 1) floats, object 1 (instance 2) is backed
        let fl = BackingCounts { area: 10, visible: [10, 0], sky: [8, 0], tested: 1 };
        let ok = BackingCounts { area: 10, visible: [10, 0], sky: [0, 0], tested: 1 };
        d.receive(0, 1, vec![(0, fl), (1, ok)]);
        let (_, _, rep) = d.update(&ctx(&v, &drawn, 1.0), &objs);
        assert_eq!(rep.floating.iter().map(|f| f.1).collect::<Vec<_>>(), vec![1]);
        assert_eq!(rep.floating_cars, 1);
        // a result for a tag never sent, or older than the newest, is ignored
        d.receive(0, 99, vec![(0, ok)]);
        d.receive(0, 1, vec![(0, ok)]);
        // the next result: the floating one's ground is not drawn: it fades out (leaving)
        d.receive(0, 2, vec![(0, fl), (1, ok)]);
        let (_, _, rep) = d.update(&ctx(&v, &[255, 0, 0, 0], 2.0), &objs);
        assert_eq!((rep.floating.len(), rep.leaving), (0, 1));
    }
}
