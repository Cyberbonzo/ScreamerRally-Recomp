//! Native renderer, moving objects (wave 3 topic W3-1, `docs/re/native/dynamics.md`): the
//! scene table's instances the game draws in a race (cars with their wheels, shadows,
//! smoke and dust puffs, tyre marks) as `FrameData::dynamics`, placed as the engine places
//! them, interpolated in floating point between simulation steps; and the game's own
//! drawing of them skipped while the native renderer draws.
//!
//! * **Which instances**: the frame's instance list (`[7bb7c]`, written by the draw entry
//!   `78d84`; `81fbc` walks it): instance A pointers ending with 0, an entry with bit 30 set
//!   is not drawn this frame (unused puffs and marks, the player's car hidden by the bumper
//!   camera). Read at every view: the game hides and shows entries around its drawing.
//! * **Placement** (`transforms.md`, "Recipe"): part p of an instance:
//!   `world = Ro (Rp v + tp) + po`, Ro = R(instance A angles +14h..+1Ch) (billboards, flag
//!   8000h: the angles relative to the view's camera, `52dc0`), po = instance A +8..+10h,
//!   Rp / tp = instance B + 20h p (angles +8..+10h, offset +14h..+1Ch: wheels' steering and
//!   spin, the body on its suspension), `R = Ry(-a1) Rx(a0) Rz(a2)` in true sine and cosine.
//! * **One version per car** (detail levels, `828c4`): the game picks each part's level by
//!   the distance of its level-0 centre from the camera (level 0 nearer than `[81e60]`,
//!   level 1 nearer than `[81e64]`, else 2; the mirror never uses level 0; the name rule
//!   of three-letter names ending in `i` / `x` only moves the part's sort key); a part without data at
//!   that level is not drawn. With the detail setting at maximum (`crate::lod::FORCE`) every
//!   part takes the model's finest level present instead, so a car is its detailed parts
//!   at any distance and never also its stand-in (slot 0's level 2; `lod.rs` forces each
//!   slot's own highest level, which can draw both versions of a model).
//! * **Animation and reflections**: `phase` = the engine's `[7bb94]` from +20h (car states,
//!   smoke frames); `scroll` = the reflection offsets: the game's `bb380` adds, the first
//!   time per frame a reflective part of the object is drawn, 0.008 x the object's
//!   displacement since then rotated by the view's float camera matrix (`82420`) to the
//!   accumulators at +30h / +38h and saves the position at +24h..+2Ch; with the game's
//!   drawing skipped this module does that update (same arithmetic, same fields).
//! * **Interpolated frames**: every simulation step's instances are kept (positions,
//!   angles, part placements, smoke animation, reflection accumulators); an extra frame
//!   between the last two steps gets them interpolated in floating point, not rounded:
//!   orientations as rotations (objects slerped; parts as a wheel's steering, camber and
//!   spin, [`lerp_part`]), not angle by angle (the game's angle triples switch between two
//!   equivalent forms as a wheel spins: angle by angle the wheels tumbled). The smoke and
//!   dust puffs (fixed in place, only their animation grows) dissolve between the two steps'
//!   pictures, fading in when new and out when gone (`FrameData::dynamic_dissolve`).
//! * **Smoke over cars**: the game draws its scene instances far to near by distance (no
//!   depth buffer), so a puff nearer than a car's position covers the car:
//!   `FrameData::dynamic_order` gives the cars their places and each puff the first place
//!   behind it; the puffs are sent far to near (`docs/re/native/dynamics.md`, "Smoke and
//!   dust puffs").
//!
//! `SR_DYN_CHECK=FIRST,LAST` (diagnostics, off by default; not with `SR_EXTRACT_CHECK`): the
//! game draws its scene instances as well (both versions on screen), and every part the
//! engine transforms is compared with this module's placement of it (level, vertices
//! against the engine's view positions), and the reflection update with the game's own.
//! `SR_DYN_INFO=1`: the scene-table models' parts and levels in the log at each race.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::extract::{PartMesh, phase_of};
use super::frame::GameCamera;
use super::scene::{Instance, Transform, View};
use super::transforms::{ANGLE_UNIT, Affine, Mat3, rotation, world_to_view};
use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

// ---------------------------------------------------------------------------------------
// Link addresses

/// The frame's instance list (`[7bb7c]` before the object loop; 78d84 stores eax there).
const INSTANCE_LIST: u32 = 0x7bb7c;
/// Appends the listed scene instances to the frame's objects (skipped when native).
const APPEND_INSTANCES: u32 = 0x81fbc;
/// An instance list entry with this bit is not drawn this frame (81fbc).
const SKIP_BIT: u32 = 0x4000_0000;
/// Scene table: 28-byte entries {name[8], .., instance A at +0Ch, index at +18h (-1 ends)}.
const SCENE: u32 = 0x12f860;
const SCENE_ENTRY: u32 = 28;
/// Model pointer table.
const MODELS: u32 = 0x147390;
/// Detail level thresholds of instanced parts (`828c4`: `-distance > [81e60]` level 0,
/// `> [81e64]` level 1, else 2).
const LOD_NEAR: u32 = 0x81e60;
const LOD_FAR: u32 = 0x81e64;
/// The view's camera rotation as floats (`82338`; `bb380` reads it for the scroll).
const CAMERA_FLOATS: u32 = 0x82420;
/// The scroll rate (`bb51c`: 0.008).
const SCROLL_RATE: u32 = 0xbb7e8;
/// Part data +18h: the part has reflection coordinates (`7aca2` calls `bb380`).
const REFLECT_FLAG: u32 = 0x2000;
/// Instance A flag: faces the camera (billboard).
const BILLBOARD: u32 = 0x8000;
/// Larger moves between two steps are not interpolated (as `crate::interp`).
const MAX_MOVE: f64 = 4000.0;
const SLOTS: usize = 13;

// ---------------------------------------------------------------------------------------
// The scene-table models

/// What a scene-table model is (from the scene table's names, as `depth.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Car,
    Shadow,
    Marks,
    Smoke,
    Other,
}

/// Order of the kinds in `FrameData::dynamics` (the renderer draws decals and the cars'
/// glass in submission order): cars, the tyre marks on the road, the shadows over them,
/// the smoke above everything.
fn draw_rank(k: Kind) -> u8 {
    match k {
        Kind::Car => 0,
        Kind::Marks => 1,
        Kind::Shadow => 2,
        Kind::Smoke => 3,
        Kind::Other => 4,
    }
}

impl Kind {
    fn of_name(name: &[u8]) -> Kind {
        let n = |p: &[u8]| name.starts_with(p);
        if n(b"auto") {
            Kind::Car
        } else if n(b"ashadow") {
            Kind::Shadow
        } else if n(b"asmoke") || n(b"a2smoke") {
            Kind::Smoke
        } else if n(b"aztri") || n(b"az1tri") {
            Kind::Marks
        } else {
            Kind::Other
        }
    }
}

/// A part slot of a scene-table model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slot {
    /// The mesh of each detail level present.
    pub mesh: [Option<u32>; 3],
    /// The part data of each level (guest addresses, for the check; 0 = none).
    pub data: [u32; 3],
    /// The level choice's centre: the level-0 data's int16 centre (+4; an entry of 0 reads
    /// the model itself, as `827f8`).
    pub centre: [f64; 3],
    /// Name rule of `828c4`: units the part counts as farther (4000 / 1000).
    pub farther: f64,
    /// Per level: the part has reflection coordinates (part data +18h bit 2000h).
    pub reflect: [bool; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// The model's data (guest address; diagnostics).
    pub addr: u32,
    pub kind: Kind,
    /// Bounding radius (model +0).
    pub radius: f64,
    pub slots: Vec<Slot>,
}

impl Model {
    /// The finest level any slot has (the detailed version of the model).
    fn finest(&self) -> Option<usize> {
        (0..3).find(|&l| self.slots.iter().any(|s| s.mesh[l].is_some()))
    }
}

/// The game's level choice for an instanced part (`828c4`): `distance` from the camera to
/// the part's centre (world units), thresholds
/// `[81e60]` / `[81e64]` (negative), mirror view: never level 0.
pub fn game_level(distance: f64, near: i32, far: i32, mirror: bool) -> usize {
    // fistp (round to nearest even) of the distance, negated, plus the name's offset
    // (the name rule's adjusted value only goes to the part's sort key: 829b0 swaps the
    // plain -distance back for the comparisons)
    let d = -(round_even(distance) as i64);
    if d > near as i64 {
        mirror as usize
    } else if d > far as i64 {
        1
    } else {
        2
    }
}

fn round_even(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 { 2.0 * (x / 2.0).round() } else { r }
}

/// `828c4`'s name rule: names of three characters (the fourth byte 0, not starting with
/// `A` / `a`) whose third is `i` count as 4000 units farther, `x` 1000.
pub fn name_farther(name: [u8; 4]) -> f64 {
    if name[0] == b'A' || name[0] == b'a' || name[3] != 0 {
        return 0.0;
    }
    match name[2] {
        b'i' => 4000.0,
        b'x' => 1000.0,
        _ => 0.0,
    }
}

fn rd32(a: u32) -> u32 {
    unsafe { mem::rd32(a) }
}

/// The scene-table models of the race: kinds from the scene table, parts and levels from
/// the models, meshes from the scene build.
fn read_models(m: &Machine, part_meshes: &[PartMesh]) -> HashMap<u32, Model> {
    let mut kinds: HashMap<u32, Kind> = HashMap::new();
    let base = m.img.link(SCENE);
    for i in 0..1024u32 {
        let e = base + i * SCENE_ENTRY;
        if !mem::readable(e, SCENE_ENTRY) {
            break;
        }
        let (inst, index) = (rd32(e + 0xc), rd32(e + 0x18));
        if index == u32::MAX {
            break;
        }
        if !mem::readable(inst, 4) {
            continue;
        }
        let k = Kind::of_name(unsafe { mem::slice(e, 8) });
        kinds.entry(rd32(inst)).or_insert(k);
    }
    // the menus' objects (no scene table, `menus.md`): every model with part meshes
    for p in part_meshes {
        kinds.entry(p.model).or_insert(Kind::Other);
    }
    let table = rd32(m.img.link(MODELS));
    let mut out = HashMap::new();
    for (&mi, &kind) in &kinds {
        let at = table.wrapping_add(4 * mi);
        if !mem::readable(at, 4) {
            continue;
        }
        let addr = rd32(at);
        if !mem::readable(addr, 0xc8) {
            continue;
        }
        let parts = (rd32(addr + 0x14) as usize).min(SLOTS);
        let mut slots = vec![Slot::default(); parts];
        for (s, slot) in slots.iter_mut().enumerate() {
            let off = |l: u32| rd32(addr + 0x18 + 0x34 * l + 4 * s as u32);
            let data0 = addr.wrapping_add(off(0));
            if mem::readable(data0, 0x6c) {
                let c = |k: u32| unsafe { mem::rd16(data0 + 4 + 2 * k) } as i16 as f64;
                slot.centre = [c(0), c(1), c(2)];
                let n = unsafe { mem::slice(data0 + 0x68, 4) };
                slot.farther = name_farther([n[0], n[1], n[2], n[3]]);
            }
            for l in 0..3u32 {
                if off(l) != 0 {
                    let d = addr.wrapping_add(off(l));
                    slot.data[l as usize] = d;
                    slot.reflect[l as usize] = mem::readable(d, 0x1c) && rd32(d + 0x18) & REFLECT_FLAG != 0;
                }
            }
        }
        for p in part_meshes.iter().filter(|p| p.model == mi) {
            if let Some(s) = slots.get_mut(p.slot as usize) {
                s.mesh[p.level as usize & 3] = Some(p.mesh);
            }
        }
        out.insert(mi, Model { addr, kind, radius: rd32(addr) as i32 as f64, slots });
    }
    out
}

// ---------------------------------------------------------------------------------------
// Instances per simulation step

/// One scene instance as the game left it at a simulation step (floats for interpolation).
#[derive(Clone, Debug, PartialEq)]
pub struct InstState {
    pub addr: u32,
    pub model: u32,
    pub flags: u32,
    pub pos: [f64; 3],
    pub angles: [f64; 3],
    /// +20h (animation, car state).
    pub anim: i32,
    /// Instance B per part: angles, offset.
    pub parts: Vec<([f64; 3], [f64; 3])>,
    /// Per part, its angles as a wheel's ([`wheel_angles`]; computed once per step for the
    /// interpolated frames; empty: computed when needed).
    pub wheel: Vec<[f64; 3]>,
    /// Reflection accumulators +30h, +34h, +38h (floats).
    pub acc: [f32; 3],
}

fn read_inst(r: &mut super::world::Reader, addr: u32, parts: usize) -> Option<InstState> {
    if !r.ok(addr, 0x40 + 0x20 * parts as u32) {
        return None;
    }
    let i = |o: u32| rd32(addr + o) as i32 as f64;
    let f = |o: u32| f32::from_bits(rd32(addr + o));
    let mut p = Vec::with_capacity(parts);
    for s in 0..parts as u32 {
        let b = addr + 0x40 + 0x20 * s;
        p.push(([i(b - addr + 8), i(b - addr + 0xc), i(b - addr + 0x10)], [i(b - addr + 0x14), i(b - addr + 0x18), i(b - addr + 0x1c)]));
    }
    Some(InstState {
        addr,
        model: rd32(addr),
        flags: rd32(addr + 4),
        pos: [i(8), i(0xc), i(0x10)],
        angles: [i(0x14), i(0x18), i(0x1c)],
        anim: rd32(addr + 0x20) as i32,
        wheel: p.iter().map(|(a, _)| if *a == [0.0; 3] { [0.0; 3] } else { wheel_angles(&rotation(*a)) }).collect(),
        parts: p,
        acc: [f(0x30), f(0x34), f(0x38)],
    })
}

impl InstState {
    /// Part `slot`'s angles as a wheel's.
    fn wheel_of(&self, slot: usize) -> [f64; 3] {
        self.wheel.get(slot).copied().unwrap_or_else(|| wheel_angles(&rotation(self.parts[slot].0)))
    }
}

/// Angle between two angles (2048 per turn), the short way round, in floating point.
pub fn lerp_angle(a: f64, b: f64, t: f64) -> f64 {
    let mut d = (b - a).rem_euclid(2048.0);
    if d > 1024.0 {
        d -= 2048.0;
    }
    (a + d * t).rem_euclid(2048.0)
}

fn mul3(a: &Mat3, b: &Mat3) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn transpose(a: &Mat3) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[j][i]))
}

/// A rotation matrix as a unit quaternion (w, x, y, z), w >= 0 (the shorter of the two).
fn quat_of(r: &Mat3) -> [f64; 4] {
    let tr = r[0][0] + r[1][1] + r[2][2];
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [s / 4.0, (r[2][1] - r[1][2]) / s, (r[0][2] - r[2][0]) / s, (r[1][0] - r[0][1]) / s]
    } else if r[0][0] > r[1][1] && r[0][0] > r[2][2] {
        let s = (1.0 + r[0][0] - r[1][1] - r[2][2]).sqrt() * 2.0;
        [(r[2][1] - r[1][2]) / s, s / 4.0, (r[0][1] + r[1][0]) / s, (r[0][2] + r[2][0]) / s]
    } else if r[1][1] > r[2][2] {
        let s = (1.0 + r[1][1] - r[0][0] - r[2][2]).sqrt() * 2.0;
        [(r[0][2] - r[2][0]) / s, (r[0][1] + r[1][0]) / s, s / 4.0, (r[1][2] + r[2][1]) / s]
    } else {
        let s = (1.0 + r[2][2] - r[0][0] - r[1][1]).sqrt() * 2.0;
        [(r[1][0] - r[0][1]) / s, (r[0][2] + r[2][0]) / s, (r[1][2] + r[2][1]) / s, s / 4.0]
    };
    let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    let q = q.map(|v| v / n);
    if q[0] < 0.0 { q.map(|v| -v) } else { q }
}

/// The rotation from `a` to `b` in `a`'s own frame (`a^T b`) as an axis and an angle in
/// [0, pi] (the short way); None when there is none.
fn turn(a: &Mat3, b: &Mat3) -> Option<([f64; 3], f64)> {
    let q = quat_of(&mul3(&transpose(a), b));
    let s = (q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    (s > 1e-12).then(|| ([q[1] / s, q[2] / s, q[3] / s], 2.0 * s.atan2(q[0])))
}

/// Rotation by `angle` (radians) about the unit `axis` (Rodrigues).
fn axis_angle(k: [f64; 3], angle: f64) -> Mat3 {
    let (s, c) = angle.sin_cos();
    let v = 1.0 - c;
    [
        [c + k[0] * k[0] * v, k[0] * k[1] * v - k[2] * s, k[0] * k[2] * v + k[1] * s],
        [k[1] * k[0] * v + k[2] * s, c + k[1] * k[1] * v, k[1] * k[2] * v - k[0] * s],
        [k[2] * k[0] * v - k[1] * s, k[2] * k[1] * v + k[0] * s, c + k[2] * k[2] * v],
    ]
}

/// The game's angles (2048 per turn, `transforms::rotation`'s convention `Ry(-a1) Rx(a0)
/// Rz(a2)`) of a rotation matrix, with |a0| <= 512.
pub fn angles_of(r: &Mat3) -> [f64; 3] {
    let h = r[1][0].hypot(r[1][1]);
    let a0 = (-r[1][2]).atan2(h);
    let (a1, a2) = if h > 1e-9 { ((-r[0][2]).atan2(r[2][2]), r[1][0].atan2(r[1][1])) } else { (r[2][0].atan2(r[0][0]), 0.0) };
    [a0, a1, a2].map(|a| (a / ANGLE_UNIT).rem_euclid(2048.0))
}

/// The rotation between two steps' angles `a` (t = 0) and `b` (t = 1), as a rotation: `a`
/// turned about the one axis that takes it to `b`, the short way (quaternion slerp),
/// whatever angle triples the game gave for the two orientations.
pub fn lerp_rotation(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    if a == b {
        return a;
    }
    let (ra, rb) = (rotation(a), rotation(b));
    let Some((k, angle)) = turn(&ra, &rb) else { return a };
    angles_of(&mul3(&ra, &axis_angle(k, angle * t)))
}

/// A part's orientation as a wheel's: steering `s` (about y), camber `c` (about z) and spin
/// `p` about the part's own x axis (the axle), innermost: `R = Ry(-s) Rz(c) Rx(p)` in
/// `transforms::rotation`'s terms (`rotation([0, s, 0]) rotation([0, 0, c]) rotation([p, 0,
/// 0])`), radians. Unlike the game's own order (`Ry(-a1) Rx(a0) Rz(a2)`, the spin in the
/// middle), this one is well defined at every spin: it only degenerates with the axle
/// vertical (camber 90 degrees).
pub fn wheel_angles(r: &Mat3) -> [f64; 3] {
    let axle = [r[0][0], r[1][0], r[2][0]];
    let c = axle[1].atan2(axle[0].hypot(axle[2]));
    let s = axle[2].atan2(axle[0]);
    // the spin: R's y axis in the frame B = Ry(-s) Rz(c) ((B^T R)[1][1], (B^T R)[2][1])
    let b = steer_camber(s, c);
    let y = |k: usize| b[0][k] * r[0][1] + b[1][k] * r[1][1] + b[2][k] * r[2][1];
    [s, c, y(2).atan2(y(1))]
}

/// `rotation([0, s, 0]) rotation([0, 0, c])` (radians).
fn steer_camber(s: f64, c: f64) -> Mat3 {
    let ((ss, cs), (sc, cc)) = (s.sin_cos(), c.sin_cos());
    [[cs * cc, -cs * sc, -ss], [sc, cc, 0.0], [ss * cc, -ss * sc, cs]]
}

/// The inverse of [`wheel_angles`].
pub fn wheel_rotation(w: [f64; 3]) -> Mat3 {
    let b = steer_camber(w[0], w[1]);
    let (sp, cp) = w[2].sin_cos();
    // B Rx(p): x as B's, y = cp By + sp Bz, z = -sp By + cp Bz
    std::array::from_fn(|i| [b[i][0], cp * b[i][1] + sp * b[i][2], -sp * b[i][1] + cp * b[i][2]])
}

/// An angle difference the short way round, in (-pi, pi].
fn wrap_pi(d: f64) -> f64 {
    let d = d.rem_euclid(std::f64::consts::TAU);
    if d > std::f64::consts::PI { d - std::f64::consts::TAU } else { d }
}

/// A part between two steps' angles `a` (t = 0) and `b` (t = 1), as a wheel moves: its
/// steering, camber and spin ([`wheel_angles`]) each interpolated, whatever angle triples
/// the game gave for the two orientations. Steering and camber the short way; the spin the
/// short way (at most half a turn), unless `spin` predicts it (radians, from the car's
/// travel, [`SpinRates`]): then the way (forward or back) closer to the prediction, so a
/// wheel spinning more than half a turn per step goes on forward instead of turning back.
/// A part whose x axis stands near vertical (none on China's cars) is slerped instead.
pub fn lerp_part(a: [f64; 3], b: [f64; 3], t: f64, spin: Option<f64>) -> [f64; 3] {
    if a == b {
        return a;
    }
    lerp_wheel(a, b, wheel_angles(&rotation(a)), wheel_angles(&rotation(b)), t, spin)
}

/// [`lerp_part`] with the two steps' [`wheel_angles`] `wa`, `wb` given.
fn lerp_wheel(a: [f64; 3], b: [f64; 3], wa: [f64; 3], wb: [f64; 3], t: f64, spin: Option<f64>) -> [f64; 3] {
    if wa[1].abs().max(wb[1].abs()) > 60f64.to_radians() {
        return lerp_rotation(a, b, t);
    }
    let mut d = [0, 1, 2].map(|k| wrap_pi(wb[k] - wa[k]));
    // (only a spin of more than a quarter turn may be one the other way round: a wheel
    // locked under braking stays locked whatever the travel predicts)
    if let Some(p) = spin.filter(|_| d[2].abs() > std::f64::consts::FRAC_PI_2) {
        let other = if d[2] > 0.0 { d[2] - std::f64::consts::TAU } else { d[2] + std::f64::consts::TAU };
        if (other - p).abs() < (d[2] - p).abs() {
            d[2] = other;
        }
    }
    angles_of(&wheel_rotation([0, 1, 2].map(|k| wa[k] + d[k] * t)))
}

/// The instance's travel from `a` to `b` along its forward axis (world units; the model's
/// -z, at `b`).
fn travel(a: &InstState, b: &InstState) -> f64 {
    let r = rotation(b.angles);
    -(0..3).map(|i| (b.pos[i] - a.pos[i]) * r[i][2]).sum::<f64>()
}

/// Per car part, its spin ([`wheel_angles`]) per unit of the car's travel (radians per
/// world unit; wheels: about 1 / the wheel's radius, 0.33 degrees per unit measured on
/// China), learned from the simulation steps whose spin is clearly less than half a turn
/// (15 to 120 degrees). It predicts a wheel's spin in the steps where it turns more than
/// half a turn (at full speed: 155 degrees per step measured at 33 steps per second, so
/// about 205 at 25), where the two steps alone cannot tell the direction.
#[derive(Default)]
pub struct SpinRates(HashMap<(u32, usize), f64>);

impl SpinRates {
    /// Learn from two consecutive steps of an instance.
    fn learn(&mut self, a: &InstState, b: &InstState) {
        if a.model != b.model || a.parts.len() != b.parts.len() || b.flags & BILLBOARD != 0 {
            return;
        }
        let d = travel(a, b);
        if d.abs() < 20.0 || d.abs() > MAX_MOVE {
            return;
        }
        for (i, (pa, pb)) in a.parts.iter().zip(&b.parts).enumerate() {
            if pa.0 == pb.0 {
                continue;
            }
            let w = wrap_pi(b.wheel_of(i)[2] - a.wheel_of(i)[2]);
            if !(15f64.to_radians()..120f64.to_radians()).contains(&w.abs()) {
                continue;
            }
            let rate = w / d;
            let e = self.0.entry((b.addr, i)).or_insert(rate);
            *e = 0.7 * *e + 0.3 * rate;
        }
    }

    /// Part `slot`'s rate (radians per unit of travel), if learned.
    fn rate(&self, inst: u32, slot: usize) -> Option<f64> {
        self.0.get(&(inst, slot)).copied()
    }
}

/// An instance between two steps (t = 0: `a`, 1: `b`); `b` alone where it jumped (a
/// reused puff, a reset) or its parts differ. Positions and the parts' offsets linearly;
/// orientations not angle by angle: the game writes them as angle triples taken from
/// matrices (spin in the middle of its order), and one orientation has two triples (a
/// wheel's switches from `(a0, a1, a2)` to `(1024 - a0, a1 + 1024, a2 + 1024)` whenever its
/// spin passes a quarter turn, with the other two angles arbitrary near it,
/// `docs/re/native/dynamics.md`): interpolated angle by angle between them, the wheel turned
/// about its other axes. The object's orientation is slerped ([`lerp_rotation`]), the parts'
/// steering, camber and spin interpolated ([`lerp_part`], `spins`: the wheels' rates).
/// Billboards keep their angles interpolated one by one (they are taken relative to the
/// camera). Smoke's animation is interpolated while it grows within one puff's life (as
/// `crate::interp`); car states are not.
pub fn lerp_inst(a: &InstState, b: &InstState, t: f64, smoke: bool, spins: &SpinRates) -> InstState {
    if a.model != b.model || a.parts.len() != b.parts.len() || (0..3).any(|k| (b.pos[k] - a.pos[k]).abs() > MAX_MOVE) {
        return b.clone();
    }
    let mut out = b.clone();
    for k in 0..3 {
        out.pos[k] = a.pos[k] + (b.pos[k] - a.pos[k]) * t;
        out.acc[k] = a.acc[k] + (b.acc[k] - a.acc[k]) * t as f32;
    }
    if b.flags & BILLBOARD != 0 {
        for k in 0..3 {
            out.angles[k] = lerp_angle(a.angles[k], b.angles[k], t);
        }
    } else {
        out.angles = lerp_rotation(a.angles, b.angles, t);
    }
    let mut d = None;
    for (i, (o, (pa, pb))) in out.parts.iter_mut().zip(a.parts.iter().zip(&b.parts)).enumerate() {
        if pa.0 != pb.0 {
            // the predicted spin: the learned rate times the travel
            let spin = spins.rate(b.addr, i).map(|r| r * *d.get_or_insert_with(|| travel(a, b)));
            o.0 = lerp_wheel(pa.0, pb.0, a.wheel_of(i), b.wheel_of(i), t, spin);
        }
        for k in 0..3 {
            o.1[k] = pa.1[k] + (pb.1[k] - pa.1[k]) * t;
        }
    }
    // (b's; the angles between are not kept)
    out.wheel.clear();
    if smoke && a.anim >= 0 && b.anim >= a.anim && b.anim < 0x80000 && b.anim - a.anim < 0x10000 {
        out.anim = (a.anim as f64 + (b.anim - a.anim) as f64 * t).round() as i32;
    }
    out
}

/// The billboard angles of `52dc0` (`transforms.md`) for a camera with float angles.
pub fn billboard_angles(inst: [f64; 3], cam: [f64; 3]) -> [f64; 3] {
    [(inst[0] - cam[0]).rem_euclid(2048.0), (-(inst[1] - cam[1] + 1024.0)).rem_euclid(2048.0), 2047.0 - cam[2]]
}

/// Model -> world of part `slot` of an instance seen from a camera with angles `cam`.
pub fn part_transform(s: &InstState, slot: usize, cam: [f64; 3]) -> Affine {
    let angles = if s.flags & BILLBOARD != 0 { billboard_angles(s.angles, cam) } else { s.angles };
    let (pa, pp) = s.parts.get(slot).copied().unwrap_or(([0.0; 3], [0.0; 3]));
    Affine::new(rotation(angles), s.pos).after(&Affine::new(rotation(pa), pp))
}

/// The engine's object cull (`81fbc`'s distance test, then `82130`) in floating point: the
/// instance's bounding sphere (position, model +0 radius) in the view's space, its extent at
/// the sphere's nearest depth projected at half resolution against the view's half-size
/// rectangle (`[81e40]`, `[81e28]`, `[81e44]`, `[81e2c]`), behind (`z - r > 0`) and beyond
/// the far distance (`z + r <= [cf470]`) culled. An object the engine culls is not drawn,
/// so its reflection accumulators are not updated in that view.
pub fn object_visible(view: [f64; 3], radius: f64, mirror: bool, consts: [f64; 5]) -> bool {
    let [cx, w, cy, h, far_neg] = consts;
    let (x, y, z) = (view[0], view[1], view[2]);
    let r = radius;
    if z - r > 0.0 || z + r <= far_neg {
        return false;
    }
    // 0x800000 / (z - r - 128), doubled (the mirror: not), then >> 17: 128 (64) / d
    let k = if mirror { 64.0 } else { 128.0 } / (z - r - 128.0);
    let ok = |a: f64, c: f64, lim: f64| (a - r) * k + c >= 0.0 && (a + r) * k + c <= lim;
    ok(x, cx, w) && ok(y, cy, h)
}

fn game_object_visible(m: &Machine, cam: &GameCamera, mirror: bool, pos: [f64; 3], radius: f64) -> bool {
    let l = |a: u32| rd32(m.img.link(a)) as i32 as f64;
    let consts = [l(0x81e40), l(0x81e28), l(0x81e44), l(0x81e2c), l(0xcf470)];
    let v = world_to_view(cam.pos, cam.angles).apply(pos);
    object_visible(v, radius, mirror, consts)
}

fn to_transform(a: &Affine) -> Transform {
    let mut basis = [0f32; 9];
    for i in 0..3 {
        for j in 0..3 {
            basis[i * 3 + j] = a.r[i][j] as f32;
        }
    }
    Transform { basis, origin: a.t }
}

/// The reflection offsets the engine uses (`bb4c5`..: `round(acc) & FFh`, x and z); an
/// interpolated frame keeps the fraction (`acc mod 256`).
pub fn scroll_of(acc: [f32; 3], exact: bool) -> [f32; 2] {
    if exact {
        let r = |v: f32| (round_even(v as f64) as i64 & 0xff) as f32;
        [r(acc[0]), r(acc[2])]
    } else {
        [(acc[0] as f64).rem_euclid(256.0) as f32, (acc[2] as f64).rem_euclid(256.0) as f32]
    }
}

/// `bb380`'s update of the reflection accumulators (`bb3a5`..`bb555`): the displacement
/// since the saved position as floats, rotated by the view's float camera matrix `cam`
/// (`x' = m0 x + m3 y + m6 z`, ...), times `rate`, added to the accumulators (x87 extended
/// precision, stored as floats).
pub fn scroll_update(acc: [f32; 3], d: [i32; 3], cam: &[f32; 9], rate: f32) -> [f32; 3] {
    let df = d.map(|v| v as f32 as f64);
    let m = cam.map(|v| v as f64);
    let v = [
        (m[0] * df[0] + m[3] * df[1] + m[6] * df[2]) as f32,
        (m[1] * df[0] + m[4] * df[1] + m[7] * df[2]) as f32,
        (m[2] * df[0] + m[5] * df[1] + m[8] * df[2]) as f32,
    ];
    [0, 1, 2].map(|k| (v[k] as f64 * rate as f64 + acc[k] as f64) as f32)
}

/// A step's instances (every entry of the list, drawn or not), keyed by address, and the
/// list as its main view read it (instance, drawn).
#[derive(Clone, Debug, Default)]
struct Step {
    frame: u64,
    insts: HashMap<u32, InstState>,
    list: Vec<(u32, bool)>,
}

impl Step {
    /// The step's state of a drawn instance.
    fn drawn(&self, a: u32) -> Option<&InstState> {
        self.list.iter().any(|&(e, d)| e == a && d).then(|| self.insts.get(&a)).flatten()
    }
}

/// The same puff at two consecutive steps (`a`, then `b`): the game never moves a puff and
/// only grows its animation (+CABh per step) until it reuses the slot for a new one
/// (`docs/re/native/dynamics.md`, "Smoke").
pub fn same_puff(a: &InstState, b: &InstState) -> bool {
    a.model == b.model && b.anim >= a.anim && (0..3).all(|k| (b.pos[k] - a.pos[k]).abs() <= MAX_MOVE)
}

/// A puff's picture between two steps ([`super::scene::Dissolve`]): `a` / `b` its state at
/// the earlier / later step when it is drawn there.
pub fn puff_dissolve(a: Option<&InstState>, b: Option<&InstState>, t: f64) -> super::scene::Dissolve {
    super::scene::Dissolve { from: a.map(|s| phase_of(s.anim)), to: b.map(|s| phase_of(s.anim)), t: t.clamp(0.0, 1.0) as f32 }
}

/// The game's drawing order of a view's moving objects (`FrameData::dynamic_order`): for
/// the cars (`dist` of each car object from the camera's position, as `81fbc` measures it)
/// their place nearest first, 1..=63; for a puff at distance `d` the first place behind it.
pub fn car_places(cars: &[f64]) -> Vec<u8> {
    let mut idx: Vec<usize> = (0..cars.len()).collect();
    idx.sort_by(|&i, &j| cars[i].total_cmp(&cars[j]).then(i.cmp(&j)));
    let mut out = vec![0u8; cars.len()];
    for (place, &i) in idx.iter().enumerate() {
        out[i] = (place + 1).min(63) as u8;
    }
    out
}

/// [`car_places`]' counterpart for a decal at distance `d`: 1 + the cars nearer than it
/// (the decal is drawn over the cars of that place or more), at most 64 (none behind it).
pub fn first_place_behind(cars: &[f64], d: f64) -> u8 {
    (1 + cars.iter().filter(|&&c| c < d).count()).min(64) as u8
}

/// A placed part (the check compares it with the engine's).
#[derive(Clone, Copy, Debug)]
struct Placed {
    inst: u32,
    slot: u8,
    level: u8,
    xf: Affine,
}

/// The module's state per race (`NativeState::dynamics`).
#[derive(Default)]
pub struct Dynamics {
    models: HashMap<u32, Model>,
    prev: Option<Step>,
    cur: Option<Step>,
    /// The wheels' spin per unit of travel (fast spins in interpolated frames).
    spins: SpinRates,
    /// The last view's placed parts, per view slot (main, second player, mirror): the check.
    placed: [Vec<Placed>; 3],
    /// Reflection emulation without writing guest memory (the check: the game updates it).
    shadow: HashMap<u32, ([f32; 3], [i32; 3])>,
    stats: Stats,
    analysed: Vec<u32>,
    /// The check: per instance, the frame and the views that tested it (bits 0-2) and found it in view (bits 4-6).
    seen: HashMap<u32, (u64, u8)>,
    /// Which moving objects are drawn (races: backed by the world under them; `dynvis`).
    vis: super::dynvis::DynVis,
    /// Per part mesh: its box in the part's space (lowest, highest corner), for the
    /// detector's bases.
    boxes: HashMap<u32, ([f32; 3], [f32; 3])>,
    /// The player's car (instance A) at the last game frame, and that frame.
    player: (u64, Option<u32>),
}

#[derive(Default)]
struct Stats {
    views: u64,
    instances: u64,
    parts: u64,
    interpolated: u64,
    by_kind: [u64; 5],
    stand_ins: u64,
    /// The visibility (races): objects on screen not fully drawn (cars), with no surface
    /// under them, and the position's height above the surface under it per kind (car,
    /// shadow, marks, smoke).
    hidden: u64,
    hidden_cars: u64,
    unsupported: u64,
    gaps: [Vec<f64>; 4],
    /// CPU time of the visibility (all views, extra frames included).
    vis_s: f64,
}

/// The moving objects of a view (`FrameData::dynamics`, `dynamic_fade`, `dynamic_backing`).
#[derive(Default)]
pub struct DynOut {
    pub instances: Vec<Instance>,
    /// Empty: all drawn (menus, or the rule off).
    pub fade: Vec<u8>,
    pub backing: Vec<super::scene::DynamicBacking>,
    /// `FrameData::dynamic_order` (races; empty: none).
    pub order: Vec<u8>,
    /// `FrameData::dynamic_dissolve` (interpolated frames with puffs; empty: none).
    pub dissolve: Vec<super::scene::Dissolve>,
}

/// `SR_SMOKE=...` (diagnostics, comma-separated): `noorder` (puffs over cars by the decal
/// rule, as before 2026-11-21), `nodissolve` (puffs switch pictures at the steps), `hide`
/// (no puffs drawn: with a run that draws them, the puffs' own pixels).
fn smoke_opts() -> (bool, bool, bool) {
    static O: std::sync::OnceLock<(bool, bool, bool)> = std::sync::OnceLock::new();
    *O.get_or_init(|| {
        let v = std::env::var("SR_SMOKE").unwrap_or_default();
        let has = |k: &str| v.split(',').any(|s| s.trim() == k);
        (!has("noorder"), !has("nodissolve"), has("hide"))
    })
}

impl Dynamics {
    /// At a scene's build: a race's (`race`: the moving objects' visibility with the ground
    /// map of its statics) or a menu's.
    pub fn new(m: &Machine, build: &super::extract::SceneBuild, race: bool) -> Dynamics {
        let part_meshes = &build.part_meshes;
        let models = read_models(m, part_meshes);
        let t0 = std::time::Instant::now();
        let ground = race.then(|| super::dynvis::GroundMap::new(&build.scene));
        if let Some(g) = &ground {
            crate::info!("native dynamics: ground map of {} triangles in {:.1} ms", g.triangles(), t0.elapsed().as_secs_f64() * 1000.0);
        }
        let mut boxes = HashMap::new();
        for p in part_meshes {
            let Some(mesh) = build.scene.meshes.get(p.mesh as usize) else { continue };
            let mut b = ([f32::MAX; 3], [f32::MIN; 3]);
            for v in &mesh.vertices {
                for k in 0..3 {
                    b.0[k] = b.0[k].min(v.pos[k]);
                    b.1[k] = b.1[k].max(v.pos[k]);
                }
            }
            if !mesh.vertices.is_empty() {
                boxes.insert(p.mesh, b);
            }
        }
        let d = Dynamics { models, vis: super::dynvis::DynVis::new(ground), boxes, ..Default::default() };
        crate::info!("native dynamics: {} scene-table models ({}), {} part meshes", d.models.len(), d.summary(), part_meshes.len());
        if info_on() {
            for l in d.describe().lines() {
                crate::info!("native dynamics: {l}");
            }
        }
        d
    }

    fn summary(&self) -> String {
        let mut by: HashMap<Kind, usize> = HashMap::new();
        for md in self.models.values() {
            *by.entry(md.kind).or_default() += 1;
        }
        let mut v: Vec<_> = by.into_iter().collect();
        v.sort();
        v.iter().map(|(k, n)| format!("{n} {k:?}")).collect::<Vec<_>>().join(", ")
    }

    /// Every model's slots and levels (`SR_DYN_INFO`).
    fn describe(&self) -> String {
        let mut s = String::new();
        let mut keys: Vec<_> = self.models.keys().copied().collect();
        keys.sort();
        for k in keys {
            let md = &self.models[&k];
            let _ = writeln!(s, "model {k} {:?} radius {}: {} slots", md.kind, md.radius, md.slots.len());
            for (i, sl) in md.slots.iter().enumerate() {
                let names: Vec<String> = (0..3)
                    .map(|l| {
                        if sl.data[l] == 0 {
                            "-".into()
                        } else {
                            let n = unsafe { mem::slice(sl.data[l] + 0x68, 12) };
                            let end = n.iter().position(|&c| c == 0).unwrap_or(12);
                            format!("{}{}{}", String::from_utf8_lossy(&n[..end]), if sl.reflect[l] { " (reflect)" } else { "" }, sl.mesh[l].map_or(String::new(), |m| format!(" mesh {m}")))
                        }
                    })
                    .collect();
                let _ = writeln!(s, "  slot {i}: centre {:?} farther {}: levels [{}]", sl.centre, sl.farther, names.join(" | "));
                for m in sl.mesh.iter().flatten() {
                    if let Some(b) = self.boxes.get(m) {
                        let _ = writeln!(s, "    mesh {m} box {:?} .. {:?}", b.0, b.1);
                    }
                }
            }
        }
        s
    }
}

fn info_level() -> u32 {
    static ON: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SR_DYN_INFO").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

fn info_on() -> bool {
    info_level() >= 1
}

/// `SR_SMOKE_LOG=FIRST,LAST` (diagnostics): every smoke instance of the steps of those game
/// frames (drawn or not, position, angles, animation).
fn smoke_log() -> Option<(u64, u64)> {
    static L: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
    *L.get_or_init(|| {
        let v: Vec<u64> = std::env::var("SR_SMOKE_LOG").ok()?.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        Some((v.first().copied().unwrap_or(0), v.get(1).copied().unwrap_or(u64::MAX)))
    })
}

/// A polygon of a car placed in the car's own space (the glass analysis).
struct CarPoly {
    slot: usize,
    mode: u8,
    rec: u32,
    pts: Vec<[f64; 3]>,
    /// Towards the visible side (the record normal +14h points away from it).
    out: [f64; 3],
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// `SR_DYN_INFO=2`: where a car's blended polygons (mode 6: glass, stickers, plates) lie
/// against its opaque ones. For sample points of each blended polygon, the line along its
/// visible-side normal is intersected with every opaque polygon of the car (all slots at
/// level 0, placed by the instance's part placements now): an opaque surface t units in
/// front of the glass (t > 0) hides it from a viewer along the normal under a depth test,
/// where the game, drawing the glass after it, shows the glass.
fn glass_analysis(m: &Machine, md: &Model, s: &InstState) -> String {
    let mut reversed: Vec<String> = Vec::new();
    let polys = car_polys(m, md, s, &mut reversed);
    let mut out = glass_report(&polys, &reversed);
    // the darkest flat colours (diagnostics of dark surfaces)
    for p in polys.iter().filter(|p| p.mode == 2) {
        let c = unsafe { mem::rd16(p.rec + 2) } as u32;
        let rgb = [((c & 0xf800) >> 8), ((c & 0x7e0) >> 3), ((c & 0x1f) << 3)];
        if rgb[0] < 72 && rgb[1] < 16 && rgb[2] < 16 {
            let cen = [0, 1, 2].map(|k| p.pts.iter().map(|q| q[k]).sum::<f64>() / p.pts.len() as f64);
            let _ = writeln!(out, "dark flat polygon slot {} record {:#x} colour {rgb:?} centre {cen:.0?} facing {:.2?} ({} vertices)", p.slot, p.rec, p.out, p.pts.len());
        }
    }
    out
}

/// `SR_DYN_PROBE=<game frame>`: at that frame's main view, the fan triangles of the cars
/// within 3000 units of the camera that face away on screen (culled natively), with their
/// polygon's first triangle (the one the game's set-ups test).
fn probe_cars(m: &Machine, models: &HashMap<u32, Model>, step: &Step, list: &[(u32, bool)], view: &View) {
    let c = &view.camera;
    let screen = |p: [f64; 3]| {
        let d = [p[0] - c.pos[0], p[1] - c.pos[1], p[2] - c.pos[2]];
        let v = [0, 1, 2].map(|k| c.rot[k * 3] as f64 * d[0] + c.rot[k * 3 + 1] as f64 * d[1] + c.rot[k * 3 + 2] as f64 * d[2]);
        (v[2] > c.proj.near as f64).then(|| (c.proj.center[0] as f64 + c.proj.scale[0] as f64 * v[0] / v[2], c.proj.center[1] as f64 + c.proj.scale[1] as f64 * v[1] / v[2]))
    };
    for &(a, drawn) in list {
        let Some(s) = step.insts.get(&a) else { continue };
        let Some(md) = models.get(&s.model) else { continue };
        let dist = ((0..3).map(|k| (s.pos[k] - c.pos[k]).powi(2)).sum::<f64>()).sqrt();
        if md.kind != Kind::Car || dist > 3000.0 {
            continue;
        }
        crate::info!("native probe frame {}: car instance {a:#x} model {} drawn {drawn} at {dist:.0} units", step.frame, s.model);
        let mut unused = Vec::new();
        let obj = Affine::new(rotation(s.angles), s.pos);
        for p in car_polys(m, md, s, &mut unused) {
            let pts: Vec<Option<(f64, f64)>> = p.pts.iter().map(|&q| screen(obj.apply(q))).collect();
            if pts.iter().any(|q| q.is_none()) {
                continue;
            }
            let q: Vec<(f64, f64)> = pts.into_iter().flatten().collect();
            let cross = |i: usize, j: usize, k: usize| (q[j].0 - q[i].0) * (q[k].1 - q[i].1) - (q[j].1 - q[i].1) * (q[k].0 - q[i].0);
            let first = cross(0, 1, 2);
            for t in 1..q.len() - 1 {
                let ct = cross(0, t, t + 1);
                // the box's centre inside the triangle
                let inside = |b: &[f64; 4]| {
                    let (px, py) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
                    let s = |a: (f64, f64), b: (f64, f64)| (b.0 - a.0) * (py - a.1) - (b.1 - a.1) * (px - a.0);
                    let (d1, d2, d3) = (s(q[0], q[t]), s(q[t], q[t + 1]), s(q[t + 1], q[0]));
                    (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
                };
                if let Some(b) = probe_box().filter(|b| inside(b)) {
                    let _ = b;
                    let w = [p.pts[0], p.pts[t], p.pts[t + 1]].map(|v| {
                        let w = obj.apply(v);
                        let d = [w[0] - c.pos[0], w[1] - c.pos[1], w[2] - c.pos[2]];
                        c.rot[6] as f64 * d[0] + c.rot[7] as f64 * d[1] + c.rot[8] as f64 * d[2]
                    });
                    crate::info!(
                        "native probe frame {}:   in box: slot {} mode {} record {:#x} triangle {t}: cross {ct:.0} depth {:.0?} corners {:?}",
                        step.frame,
                        p.slot,
                        p.mode,
                        p.rec,
                        w,
                        [q[0], q[t], q[t + 1]].map(|(x, y)| (x.round(), y.round()))
                    );
                }
                if ct < -20.0 && probe_box().is_none() {
                    crate::info!(
                        "native probe frame {}:   slot {} mode {} record {:#x} triangle {t}: screen cross {ct:.0} (first triangle {first:.0}), corners {:?}",
                        step.frame,
                        p.slot,
                        p.mode,
                        p.rec,
                        [q[0], q[t], q[t + 1]].map(|(x, y)| (x.round(), y.round()))
                    );
                }
            }
        }
    }
}

/// `SR_DYN_PROBE_BOX=x0,y0,x1,y1` (game pixels): the probe lists every triangle with a
/// corner in the box instead.
fn probe_box() -> Option<[f64; 4]> {
    static B: std::sync::OnceLock<Option<[f64; 4]>> = std::sync::OnceLock::new();
    *B.get_or_init(|| {
        let v: Vec<f64> = std::env::var("SR_DYN_PROBE_BOX").ok()?.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
    })
}

/// `SR_DYN_HIDE=s,s,...` (diagnostics): the cars' part slots not drawn.
fn hidden_slots() -> u32 {
    static H: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *H.get_or_init(|| std::env::var("SR_DYN_HIDE").map_or(0, |v| v.split(',').filter_map(|s| s.trim().parse::<u32>().ok()).fold(0, |a, s| a | 1 << s.min(31))))
}

fn probe_at() -> Option<u64> {
    static AT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *AT.get_or_init(|| std::env::var("SR_DYN_PROBE").ok().and_then(|v| v.trim().parse().ok()))
}

/// A car's polygons at level 0 in its own space (placed by the instance's part
/// placements); `reversed` collects those whose winding does not face away from their
/// record normal.
fn car_polys(m: &Machine, md: &Model, s: &InstState, reversed: &mut Vec<String>) -> Vec<CarPoly> {
    use super::world;
    let mut r = world::Reader::default();
    let poly_base = rd32(md.addr + 0xb4);
    let mut polys: Vec<CarPoly> = Vec::new();
    for (slot, sl) in md.slots.iter().enumerate() {
        if sl.data[0] == 0 {
            continue;
        }
        let Some(part) = world::read_part(&mut r, sl.data[0]) else { continue };
        let Some(verts) = world::vertices(&mut r, md.addr.wrapping_add(part.verts)) else { continue };
        let Some((records, _)) = world::polygons(&mut r, &part) else { continue };
        let (pa, pp) = s.parts.get(slot).copied().unwrap_or(([0.0; 3], [0.0; 3]));
        let xf = Affine::new(rotation(pa), pp);
        for rec in records {
            let Some((mat, offs)) = world::polygon(&mut r, rec) else { continue };
            let ty = rd32(poly_base.wrapping_add(mat as i16 as i32 as u32));
            let Some(mode) = super::polygons::mode_of_type(m, ty) else { continue };
            let pts: Vec<[f64; 3]> = offs.iter().filter_map(|&o| verts.get(o as usize / 6)).map(|v| xf.apply(v.map(|c| c as f64))).collect();
            if pts.len() < 3 {
                continue;
            }
            let nr = [0, 4, 8].map(|k| rd32(rec + 0x14 + k) as i32 as f64 / 16384.0);
            let nw = [0, 1, 2].map(|i| xf.r[i][0] * nr[0] + xf.r[i][1] * nr[1] + xf.r[i][2] * nr[2]);
            // the winding against the record normal (front faces: (v1 - v0) x (v2 - v0)
            // opposite to it, polygons.md)
            let w = cross(sub(pts[1], pts[0]), sub(pts[2], pts[0]));
            let lw = dot(w, w).sqrt();
            if lw > 0.0 && dot(w, nw) / lw > -0.5 {
                let c = [0, 1, 2].map(|k| pts.iter().map(|p| p[k]).sum::<f64>() / pts.len() as f64);
                reversed.push(format!("slot {slot} mode {mode} record {rec:#x} ({} vertices, centre {c:.0?}): winding . normal {:.2}", pts.len(), dot(w, nw) / lw));
            }
            polys.push(CarPoly { slot, mode, rec, pts, out: nw.map(|c| -c) });
        }
    }
    polys
}

fn glass_report(polys: &[CarPoly], reversed: &[String]) -> String {
    let inside = |q: &CarPoly, x: [f64; 3]| -> bool {
        // fan triangles, barycentric in the triangle's plane
        (1..q.pts.len() - 1).any(|t| {
            let (a, b, c) = (q.pts[0], q.pts[t], q.pts[t + 1]);
            let (v0, v1, v2) = (sub(c, a), sub(b, a), sub(x, a));
            let (d00, d01, d02, d11, d12) = (dot(v0, v0), dot(v0, v1), dot(v0, v2), dot(v1, v1), dot(v1, v2));
            let den = d00 * d11 - d01 * d01;
            if den.abs() < 1e-9 {
                return false;
            }
            let (u, v) = ((d11 * d02 - d01 * d12) / den, (d00 * d12 - d01 * d02) / den);
            u >= -1e-3 && v >= -1e-3 && u + v <= 1.001
        })
    };
    let mut out = String::new();
    let mut worst: Vec<(f64, String)> = Vec::new();
    let (mut glass, mut covered, mut coplanar) = (0, 0, 0);
    for g in polys.iter().filter(|p| p.mode == 6) {
        glass += 1;
        let c = [0, 1, 2].map(|k| g.pts.iter().map(|p| p[k]).sum::<f64>() / g.pts.len() as f64);
        let mut samples = vec![c];
        samples.extend(g.pts.iter().map(|p| [0, 1, 2].map(|k| c[k] + 0.8 * (p[k] - c[k]))));
        let mut need = 0.0f64;
        let mut what = String::new();
        let mut copl = false;
        for p in samples {
            let mut front = f64::MAX;
            let mut who = (0usize, 0u8);
            for q in polys.iter().filter(|q| q.mode != 6 && q.mode != 7) {
                let nq = cross(sub(q.pts[1], q.pts[0]), sub(q.pts[2], q.pts[0]));
                let den = dot(nq, g.out);
                if den.abs() < 1e-9 {
                    continue;
                }
                let t = dot(nq, sub(q.pts[0], p)) / den;
                if !(-40.0..=40.0).contains(&t) {
                    continue;
                }
                let x = [p[0] + t * g.out[0], p[1] + t * g.out[1], p[2] + t * g.out[2]];
                if !inside(q, x) {
                    continue;
                }
                if t.abs() < 0.05 {
                    copl = true;
                }
                if t > -0.05 && t < front {
                    front = t;
                    who = (q.slot, q.mode);
                }
            }
            if front < f64::MAX && front > need {
                need = front;
                what = format!("slot {} mode {}", who.0, who.1);
            }
        }
        if copl {
            coplanar += 1;
        }
        if need > 0.0 {
            covered += 1;
            worst.push((need, format!("glass in slot {} ({} vertices, centre {:.0?}, facing {:.2?}): an opaque surface ({what}) up to {need:.2} units in front", g.slot, g.pts.len(), c, g.out)));
        }
    }
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    let _ = writeln!(out, "glass analysis: {glass} blended polygons, {covered} with an opaque surface of the car within 40 units in front, {coplanar} lying on one; largest {:.2?}", worst.iter().take(12).map(|w| w.0).collect::<Vec<_>>());
    for (_, l) in worst.iter().take(12) {
        let _ = writeln!(out, "  {l}");
    }
    let _ = writeln!(out, "polygons whose winding does not face away from their record normal: {}", reversed.len());
    for l in reversed.iter().take(40) {
        let _ = writeln!(out, "  {l}");
    }
    out
}

/// The frame's instance list: (instance A, drawn this frame).
fn instance_list(m: &Machine, r: &mut super::world::Reader) -> Vec<(u32, bool)> {
    let list = rd32(m.img.link(INSTANCE_LIST));
    let mut out = Vec::new();
    if list < 0x10000 || list == u32::MAX {
        return out;
    }
    for i in 0..4096u32 {
        let a = list + 4 * i;
        if !r.ok(a, 4) {
            break;
        }
        let e = rd32(a);
        if e == 0 {
            break;
        }
        out.push((e & !SKIP_BIT, e & SKIP_BIT == 0));
    }
    out
}

/// What a view needs to place the instances.
pub struct ViewCtx<'a> {
    pub view: &'a View,
    /// The game camera of the view (float angles and position).
    pub cam: GameCamera,
    pub mirror: bool,
    /// View slot (0 main, 1 second player, 2 mirror).
    pub slot: usize,
    /// An interpolated extra frame, at `alpha` between the last two steps.
    pub extra: bool,
    pub alpha: Option<f32>,
    /// A race's view: its statics' fades (what is drawn under the moving objects), the
    /// fades' clock (simulation steps) and the frame's backing tag. `None`: the menus' scenes
    /// (every object drawn).
    pub race: Option<RaceCtx<'a>>,
}

/// [`ViewCtx::race`].
pub struct RaceCtx<'a> {
    pub static_fade: &'a [u8],
    pub t: f64,
    pub tag: u64,
}

/// The view's moving objects (`FrameData::dynamics` and their fades and tests). Call where
/// the native view is drawn (the game's world drawing of that view, after its camera is set
/// up).
pub fn view_dynamics(m: &mut Machine, ctx: &ViewCtx) -> DynOut {
    let Some(mut d) = m.native.dynamics.take() else { return DynOut::default() };
    let out = d.view(m, ctx);
    m.native.dynamics = Some(d);
    out
}

/// The moving objects' backing results of a view slot (`BackingResults::dynamics`).
pub fn receive_backing(m: &mut Machine, slot: usize, tag: u64, results: Vec<(u32, super::scene::BackingCounts)>) {
    if let Some(d) = m.native.dynamics.as_mut() {
        d.vis.receive(slot, tag, results);
    }
}

/// Game mode bits (bit 7: the attract mode's demo race, no player), player 1's car index,
/// the car records and a record's position (floats; `rumble.rs`).
const MODE: u32 = 0x1436dc;
const MODE_ATTRACT: u32 = 0x80;
const PLAYER1: u32 = 0x1436d4;
const CARS: u32 = 0x1296c8;
const CAR_SIZE: u32 = 0xcc8;
const CAR_POS: u32 = 0x52c;

/// The player's car record position, `None` in the attract mode or without a player.
pub(super) fn player_position(m: &Machine) -> Option<[f64; 3]> {
    let l = |a: u32| m.img.link(a);
    if rd32(l(MODE)) & MODE_ATTRACT != 0 {
        return None;
    }
    let idx = rd32(l(PLAYER1));
    if idx >= 8 {
        return None;
    }
    let a = l(CARS) + idx * CAR_SIZE + CAR_POS;
    if !mem::readable(a, 12) {
        return None;
    }
    let p = [0u32, 4, 8].map(|k| f32::from_bits(rd32(a + k)) as f64);
    p.iter().all(|v| v.is_finite()).then_some(p)
}

/// `SR_NATIVE_VIS_LOG` (the detector and its log) or `SR_NATIVE_DYN_TEST=1` (the test only).
fn dyn_test_on() -> (bool, bool) {
    static ON: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let log = std::env::var("SR_NATIVE_VIS_LOG").is_ok_and(|v| v.trim() != "0");
        (log || std::env::var("SR_NATIVE_DYN_TEST").is_ok_and(|v| v.trim() == "1"), log)
    })
}

impl Dynamics {
    fn view(&mut self, m: &mut Machine, ctx: &ViewCtx) -> DynOut {
        // guest reads checked through one region cache per view (VirtualQuery is slow)
        let mut r = super::world::Reader::default();
        let list = instance_list(m, &mut r);
        let frame = crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed);
        let checking = check_cfg().is_some();
        if !ctx.extra {
            // a simulation step's own drawing: keep its instances (once per game frame)
            if self.cur.as_ref().is_none_or(|c| c.frame != frame) {
                let mut step = Step { frame, insts: HashMap::new(), list: list.clone() };
                for &(a, _) in &list {
                    let parts = r.ok(a, 4).then(|| self.models.get(&rd32(a)).map_or(1, |md| md.slots.len().max(1))).unwrap_or(1);
                    if let Some(s) = read_inst(&mut r, a, parts) {
                        step.insts.insert(a, s);
                    }
                }
                if checking {
                    check_scroll(self, &step);
                }
                if smoke_log().is_some_and(|(a, b)| (a..=b).contains(&frame)) {
                    for &(a, drawn) in &list {
                        let Some(s) = step.insts.get(&a) else { continue };
                        if self.models.get(&s.model).is_some_and(|md| md.kind == Kind::Car) {
                            crate::info!("smoke log {frame}: car {a:x} pos {:.0?} angles {:.0?}", s.pos, s.angles);
                        }
                        if self.models.get(&s.model).is_some_and(|md| md.kind == Kind::Smoke) {
                            crate::info!("smoke log {frame}: {a:x} model {} drawn {} flags {:x} pos {:.0?} angles {:.0?} anim {:x}", s.model, drawn as u8, s.flags, s.pos, s.angles, s.anim);
                        }
                    }
                }
                if info_level() >= 2 {
                    for &(a, _) in &list {
                        let Some(s) = step.insts.get(&a) else { continue };
                        match self.models.get(&s.model) {
                            Some(md) if md.kind == Kind::Car && !self.analysed.contains(&s.model) => {
                                self.analysed.push(s.model);
                                crate::info!("native dynamics: model {} first seen at game frame {frame}, instance {a:#x}", s.model);
                                for l in glass_analysis(m, md, s).lines() {
                                    crate::info!("native dynamics: model {} {l}", s.model);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(c) = self.cur.as_ref().filter(|c| c.frame + 1 == step.frame) {
                    for (a, s) in &step.insts {
                        if let Some(p) = c.insts.get(a) {
                            self.spins.learn(p, s);
                        }
                    }
                }
                self.prev = self.cur.take();
                self.cur = Some(step);
            } else if let Some(cur) = self.cur.as_mut() {
                // a later view of the same game frame (the rear-view mirror): the game
                // updates some instances in between (the shadows' tilt): as the engine draws
                // them now (the reflection accumulators stay this module's)
                for &(a, _) in &list {
                    let parts = r.ok(a, 4).then(|| self.models.get(&rd32(a)).map_or(1, |md| md.slots.len().max(1))).unwrap_or(1);
                    if let Some(mut s) = read_inst(&mut r, a, parts) {
                        if let Some(old) = cur.insts.get(&a) {
                            s.acc = old.acc;
                        }
                        cur.insts.insert(a, s);
                    }
                }
            }
            self.reflections(m, ctx, &list, checking);
            if probe_at() == Some(frame) && ctx.slot == 0 {
                if let Some(c) = self.cur.as_ref() {
                    probe_cars(m, &self.models, c, &list, ctx.view);
                }
            }
        }
        // the states to draw: this step's, or between the last two steps
        let (prev, cur) = (self.prev.as_ref(), self.cur.as_ref());
        let lerp = match (ctx.extra, ctx.alpha, prev, cur) {
            (true, Some(t), Some(p), Some(c)) if c.frame == p.frame + 1 => Some((p, t as f64)),
            _ => None,
        };
        let maximum = crate::lod::FORCE.load(std::sync::atomic::Ordering::Relaxed);
        let thresholds = (rd32(m.img.link(LOD_NEAR)) as i32, rd32(m.img.link(LOD_FAR)) as i32);
        let mut out: Vec<(u8, Instance, u32)> = Vec::new();
        let mut placed = Vec::new();
        let Some(cur) = cur else { return DynOut::default() };
        // the moving objects' visibility (races): the objects, their bases when tested
        let (test, log) = dyn_test_on();
        let test = test && ctx.race.is_some();
        let mut objs: Vec<super::dynvis::Obj> = Vec::new();
        // the player's car: the car instance at the player's car record's position (once per
        // game frame)
        if ctx.race.is_some() && self.player.0 != cur.frame {
            let mut best: Option<(f64, u32)> = None;
            let p = player_position(m);
            let probe = super::dynvis::probe_at().is_some_and(|f| m.glide.as_ref().is_some_and(|g| g.frames == f));
            if probe {
                crate::info!("native dyn probe: player record {p:?}");
            }
            if let Some(p) = p {
                for (&a, s) in &cur.insts {
                    if self.models.get(&s.model).is_some_and(|md| md.kind == Kind::Car) {
                        let d = ((s.pos[0] - p[0]).powi(2) + (s.pos[1] - p[1]).powi(2) + (s.pos[2] - p[2]).powi(2)).sqrt();
                        if probe {
                            crate::info!("native dyn probe: car {a:x} at {:?}: {d:.0} from the player's record", s.pos);
                        }
                        if d < 1500.0 && best.is_none_or(|b| d < b.0) {
                            best = Some((d, a));
                        }
                    }
                }
            }
            self.player = (cur.frame, best.map(|b| b.1));
        }
        // what to draw: (instance as dynvis knows it, state, model, dissolve)
        let (order_on, dissolve_on, hide_smoke) = smoke_opts();
        let dissolve_on = dissolve_on && ctx.race.is_some();
        let mut entries: Vec<(u32, InstState, &Model, super::scene::Dissolve)> = Vec::with_capacity(list.len());
        for &(a, drawn) in &list {
            if !drawn {
                continue;
            }
            let Some(sb) = cur.insts.get(&a) else { continue };
            let Some(md) = self.models.get(&sb.model) else { continue };
            let (s, dissolve) = match lerp {
                // puffs (`docs/re/native/dynamics.md`, "Smoke"): the same puff as at the last
                // step (drawn there): its picture dissolves from that step's frame to this
                // one's; a new one fades in (the game shows it from this step)
                Some((p, t)) if md.kind == Kind::Smoke && dissolve_on => match p.drawn(a).filter(|sa| same_puff(sa, sb)) {
                    Some(sa) => {
                        self.stats.interpolated += 1;
                        (lerp_inst(sa, sb, t, true, &self.spins), puff_dissolve(Some(sa), Some(sb), t))
                    }
                    None => (sb.clone(), puff_dissolve(None, Some(sb), t)),
                },
                Some((p, t)) => match p.insts.get(&a) {
                    Some(sa) => {
                        self.stats.interpolated += 1;
                        let s = lerp_inst(sa, sb, t, md.kind == Kind::Smoke, &self.spins);
                        if md.kind == Kind::Car && ctx.slot == 0 {
                            interp_check(md, sa, sb, t, &s, cur.frame);
                        }
                        (s, super::scene::Dissolve::NONE)
                    }
                    None => (sb.clone(), super::scene::Dissolve::NONE),
                },
                None => (sb.clone(), super::scene::Dissolve::NONE),
            };
            entries.push((a, s, md, dissolve));
        }
        // puffs drawn at the last step and gone at this one (ended, or their slot reused):
        // fading out where they were (a key of their own for dynvis: the slot may hold a new
        // puff)
        if let Some((p, t)) = lerp.filter(|_| dissolve_on) {
            for &(a, drawn) in &p.list {
                let Some(sa) = p.insts.get(&a).filter(|_| drawn) else { continue };
                let Some(md) = self.models.get(&sa.model).filter(|md| md.kind == Kind::Smoke) else { continue };
                let still = list.iter().any(|&(e, d)| e == a && d) && cur.insts.get(&a).is_some_and(|sb| same_puff(sa, sb));
                if !still {
                    entries.push((a ^ 1, sa.clone(), md, puff_dissolve(Some(sa), None, t)));
                }
            }
        }
        if hide_smoke {
            entries.retain(|e| e.2.kind != Kind::Smoke);
        }
        if ctx.slot == 0 && smoke_log().is_some_and(|(a, b)| (a..=b).contains(&cur.frame)) {
            // what each puff shows in this drawn frame (the game's frames: t -1)
            let t = lerp.map_or(-1.0, |l| l.1);
            for (a, s, _, d) in entries.iter().filter(|e| e.2.kind == Kind::Smoke) {
                let p = |v: Option<i32>| v.map_or("-".to_string(), |v| v.to_string());
                crate::info!("smoke draw {} t {t:.3} {a:x} model {} pos {:.0?} phase {} dissolve {} {} {:.3}", cur.frame, s.model, s.pos, phase_of(s.anim), p(d.from), p(d.to), d.t);
            }
        }
        let mut dissolves: Vec<super::scene::Dissolve> = Vec::new();
        for (a, s, md, dissolve) in entries {
            let finest = md.finest();
            let scroll = scroll_of(s.acc, !ctx.extra);
            let phase = phase_of(s.anim);
            self.stats.instances += 1;
            self.stats.by_kind[md.kind as usize] += 1;
            let oi = objs.len() as u32;
            objs.push(super::dynvis::Obj { addr: a, kind: md.kind, pos: s.pos, radius: md.radius, heights: None });
            dissolves.push(dissolve);
            for (slot, sl) in md.slots.iter().enumerate() {
                let xf = part_transform(&s, slot, ctx.cam.angles);
                let level = if maximum {
                    match finest {
                        Some(l) => l,
                        None => continue,
                    }
                } else {
                    let c = xf.apply(sl.centre);
                    let dist = ((c[0] - ctx.cam.pos[0]).powi(2) + (c[1] - ctx.cam.pos[1]).powi(2) + (c[2] - ctx.cam.pos[2]).powi(2)).sqrt();
                    game_level(dist, thresholds.0, thresholds.1, ctx.mirror)
                };
                let Some(mesh) = sl.mesh[level] else { continue };
                if md.kind == Kind::Car && hidden_slots() & (1 << slot) != 0 {
                    continue;
                }
                if md.kind == Kind::Car && level == 2 {
                    self.stats.stand_ins += 1;
                }
                self.stats.parts += 1;
                out.push((draw_rank(md.kind), Instance { mesh, transform: to_transform(&xf), phase, scroll }, oi));
                if let Some(b) = self.boxes.get(&mesh).filter(|_| test) {
                    // the part's box in world height: its lowest and highest corner
                    let o = &mut objs[oi as usize];
                    for k in 0..8 {
                        let c = [0, 1, 2].map(|j| if k >> j & 1 == 0 { b.0[j] } else { b.1[j] } as f64);
                        let y = xf.apply(c)[1];
                        o.heights = Some(o.heights.map_or((y, y), |(lo, hi)| (lo.min(y), hi.max(y))));
                    }
                }
                if checking {
                    placed.push(Placed { inst: a, slot: slot as u8, level: level as u8, xf });
                }
            }
        }
        if checking && !ctx.extra {
            self.placed[ctx.slot.min(2)] = placed;
        }
        self.stats.views += 1;
        // which objects are drawn (races)
        let (fades, tests) = match &ctx.race {
            Some(r) => {
                let frame = m.glide.as_ref().map_or(0, |g| g.frames);
                let vctx = super::dynvis::Ctx {
                    view: ctx.view,
                    slot: ctx.slot,
                    static_fade: r.static_fade,
                    t: r.t,
                    extra: ctx.extra,
                    ext: crate::view::ext(),
                    player: self.player.1,
                    test,
                    tag: r.tag,
                    frame,
                };
                let t0 = std::time::Instant::now();
                let (fades, tests, rep) = self.vis.update(&vctx, &objs);
                self.stats.vis_s += t0.elapsed().as_secs_f64();
                if !ctx.extra {
                    let s = &mut self.stats;
                    s.hidden += rep.hidden as u64;
                    s.hidden_cars += rep.hidden_cars as u64;
                    s.unsupported += rep.unsupported as u64;
                    for &(k, g) in &rep.gaps {
                        let i = match k {
                            Kind::Car => 0,
                            Kind::Shadow => 1,
                            Kind::Marks => 2,
                            _ => 3,
                        };
                        if s.gaps[i].len() < 100_000 {
                            s.gaps[i].push(g);
                        }
                    }
                    if log {
                        let mut fl = rep.floating.clone();
                        fl.sort_by(|a, b| a.2.total_cmp(&b.2));
                        let fl: Vec<String> = fl.iter().take(6).map(|(k, a, d, s, v, g)| format!("{k:?}:{a:x}@{d:.0}:{s}/{v}{}", if *g { "g" } else { "u" })).collect();
                        crate::info!(
                            "native dyn: frame {frame} slot {} objects {} onscreen {} hidden {} hidden_cars {} floating {} [{}] floating_cars {} ungrounded {} leaving {} near_hidden {} player_hidden {} unsupported {} hidden_backed {} player {} by_kind hidden {:?} unsupported {:?} floating {:?}",
                            ctx.slot,
                            rep.objects,
                            rep.onscreen,
                            rep.hidden,
                            rep.hidden_cars,
                            rep.floating.len(),
                            fl.join(" "),
                            rep.floating_cars,
                            rep.floating_cars_ungrounded,
                            rep.leaving,
                            rep.near_hidden,
                            rep.player_hidden,
                            rep.unsupported,
                            rep.hidden_backed,
                            self.player.1.is_some() as u8,
                            rep.hidden_by,
                            rep.unsupported_by,
                            rep.floating_by
                        );
                    }
                }
                (fades, tests)
            }
            None => (Vec::new(), Vec::new()),
        };
        // each object's distance from the camera's position (the game's sort key, `81fbc`:
        // the instance position against the camera structure's)
        let dist: Vec<f64> = objs.iter().map(|o| (0..3).map(|k| (o.pos[k] - ctx.cam.pos[k]).powi(2)).sum::<f64>().sqrt()).collect();
        // the renderer draws decals and the cars' glass in this order: cars, then the tyre
        // marks on the road, the shadows over them, the smoke above, far to near among
        // itself as the game draws it (the puffs blend over each other in that order)
        out.sort_by(|a, b| {
            a.0.cmp(&b.0).then_with(|| if a.0 == draw_rank(Kind::Smoke) { dist[b.2 as usize].total_cmp(&dist[a.2 as usize]) } else { std::cmp::Ordering::Equal })
        });
        // the game's order between puffs and cars (`FrameData::dynamic_order`)
        let order = if order_on && ctx.race.is_some() {
            let cars: Vec<u32> = {
                let mut v: Vec<u32> = out.iter().filter(|i| objs[i.2 as usize].kind == Kind::Car).map(|i| i.2).collect();
                v.dedup();
                v
            };
            let car_dist: Vec<f64> = cars.iter().map(|&o| dist[o as usize]).collect();
            let places = car_places(&car_dist);
            out.iter()
                .map(|i| match objs[i.2 as usize].kind {
                    Kind::Car => cars.iter().position(|&o| o == i.2).map_or(0, |k| places[k]),
                    Kind::Smoke => first_place_behind(&car_dist, dist[i.2 as usize]),
                    _ => 0,
                })
                .collect()
        } else {
            Vec::new()
        };
        let rule = ctx.race.is_some() && super::dynvis::rule_on();
        DynOut {
            fade: if rule { out.iter().map(|i| fades.get(i.2 as usize).copied().unwrap_or(255)).collect() } else { Vec::new() },
            backing: if tests.is_empty() { Vec::new() } else { out.iter().map(|i| tests.get(i.2 as usize).copied().unwrap_or_default()).collect() },
            dissolve: if dissolves.iter().any(|d| d.active()) { out.iter().map(|i| dissolves[i.2 as usize]).collect() } else { Vec::new() },
            order,
            instances: out.into_iter().map(|i| i.1).collect(),
        }
    }

    /// The game's reflection update (`bb380`) for this view: for every drawn instance with
    /// a reflective part at the level drawn whose bounding sphere is in the view, the first
    /// time its position differs from the saved one (+24h..+2Ch).
    fn reflections(&mut self, m: &mut Machine, ctx: &ViewCtx, list: &[(u32, bool)], checking: bool) {
        let l = |a: u32| m.img.link(a);
        let cam: [f32; 9] = std::array::from_fn(|k| f32::from_bits(rd32(l(CAMERA_FLOATS) + 4 * k as u32)));
        let rate = f32::from_bits(rd32(l(SCROLL_RATE)));
        let maximum = crate::lod::FORCE.load(std::sync::atomic::Ordering::Relaxed);
        for &(a, drawn) in list {
            if !drawn {
                continue;
            }
            let Some(s) = self.cur.as_ref().and_then(|c| c.insts.get(&a)) else { continue };
            let Some(md) = self.models.get(&s.model) else { continue };
            // a reflective part at the level the view draws
            let thresholds = (rd32(m.img.link(LOD_NEAR)) as i32, rd32(m.img.link(LOD_FAR)) as i32);
            let reflective = md.slots.iter().enumerate().any(|(slot, sl)| {
                let level = if maximum {
                    md.finest()
                } else {
                    let c = part_transform(s, slot, ctx.cam.angles).apply(sl.centre);
                    let dist = ((c[0] - ctx.cam.pos[0]).powi(2) + (c[1] - ctx.cam.pos[1]).powi(2) + (c[2] - ctx.cam.pos[2]).powi(2)).sqrt();
                    Some(game_level(dist, thresholds.0, thresholds.1, ctx.mirror))
                };
                level.is_some_and(|l| sl.reflect[l] && sl.data[l] != 0)
            });
            let visible = game_object_visible(m, &ctx.cam, ctx.mirror, s.pos, md.radius);
            if checking {
                let frame = self.cur.as_ref().map_or(0, |c| c.frame);
                let e = self.seen.entry(a).or_insert((frame, 0));
                if e.0 != frame {
                    *e = (frame, 0);
                }
                e.1 |= (1 << ctx.slot.min(2)) << if visible { 4 } else { 0 };
            }
            if !reflective || !visible {
                continue;
            }
            let pos = s.pos.map(|v| v as i32);
            let (acc, saved) = if checking {
                *self.shadow.entry(a).or_insert_with(|| (s.acc, [rd32(a + 0x24) as i32, rd32(a + 0x28) as i32, rd32(a + 0x2c) as i32]))
            } else {
                (s.acc, [rd32(a + 0x24) as i32, rd32(a + 0x28) as i32, rd32(a + 0x2c) as i32])
            };
            let d = [pos[0].wrapping_sub(saved[0]), pos[1].wrapping_sub(saved[1]), pos[2].wrapping_sub(saved[2])];
            if d == [0; 3] {
                continue;
            }
            let acc = scroll_update(acc, d, &cam, rate);
            if checking {
                self.shadow.insert(a, (acc, pos));
                continue;
            }
            unsafe {
                for k in 0..3u32 {
                    mem::wr32(a + 0x30 + 4 * k, acc[k as usize].to_bits());
                    mem::wr32(a + 0x24 + 4 * k, pos[k as usize] as u32);
                }
            }
            if let Some(c) = self.cur.as_mut().and_then(|c| c.insts.get_mut(&a)) {
                c.acc = acc;
            }
        }
    }

    /// The player's car as the last race view found it (the visibility editor's live data):
    /// position and angles of its instance at the last step, or on an interpolated frame
    /// (`extra`) between the last two steps at `alpha` (angles the short way round). `None`:
    /// no player's car (the attract mode, or not found).
    pub fn player_car(&self, extra: bool, alpha: Option<f32>) -> Option<([f64; 3], [f64; 3])> {
        let a = self.player.1?;
        let cur = self.cur.as_ref()?;
        let s = cur.insts.get(&a)?;
        let p = self.prev.as_ref().filter(|p| extra && p.frame + 1 == cur.frame).and_then(|p| p.insts.get(&a));
        Some(match (p, alpha) {
            (Some(p), Some(t)) => {
                let t = t.clamp(0.0, 1.0) as f64;
                let pos = [0, 1, 2].map(|k| p.pos[k] + (s.pos[k] - p.pos[k]) * t);
                let angles = [0, 1, 2].map(|k| {
                    let d = (s.angles[k] - p.angles[k] + 1024.0).rem_euclid(2048.0) - 1024.0;
                    (p.angles[k] + d * t).rem_euclid(2048.0)
                });
                (pos, angles)
            }
            _ => (s.pos, s.angles),
        })
    }

    /// Statistics line (with the integration's every 5 s).
    pub fn take_stats(&mut self) -> Option<String> {
        let s = std::mem::take(&mut self.stats);
        (s.views > 0).then(|| {
            let n = s.views as f64;
            let gaps = |v: &Vec<f64>| {
                let mut v = v.clone();
                v.sort_by(f64::total_cmp);
                if v.is_empty() { "-".to_string() } else { format!("{:.0}/{:.0}/{:.0}", v[0], v[v.len() / 2], v[v.len() - 1]) }
            };
            format!(
                "per view {:.1} instances ({:.1} cars, {:.1} shadows, {:.1} marks, {:.1} smoke), {:.1} parts, {:.2} car stand-in parts; {:.1} % interpolated; visibility {:.4} ms; on screen not fully drawn {:.2} ({:.2} cars), no ground under {:.2}; height over the ground min/median/max: cars {}, shadows {}, marks {}, smoke {}",
                s.instances as f64 / n,
                s.by_kind[Kind::Car as usize] as f64 / n,
                s.by_kind[Kind::Shadow as usize] as f64 / n,
                s.by_kind[Kind::Marks as usize] as f64 / n,
                s.by_kind[Kind::Smoke as usize] as f64 / n,
                s.parts as f64 / n,
                s.stand_ins as f64 / n,
                100.0 * s.interpolated as f64 / s.instances.max(1) as f64,
                s.vis_s * 1000.0 / n,
                s.hidden as f64 / n,
                s.hidden_cars as f64 / n,
                s.unsupported as f64 / n,
                gaps(&s.gaps[0]),
                gaps(&s.gaps[1]),
                gaps(&s.gaps[2]),
                gaps(&s.gaps[3])
            )
        })
    }
}

// ---------------------------------------------------------------------------------------
// Skipping the game's drawing of the scene instances

/// Install the observer on `81fbc` (the scene instances' append): while the native renderer
/// draws a view, the game appends nothing (its objects are then none at all: the cells'
/// objects are skipped by the visibility observer). `SR_DYN_CHECK` keeps the game's.
pub fn install(m: &mut Machine) {
    if let Err(e) = m.install_observer("scene instances (native renderer)", APPEND_INSTANCES, |m, _f, tramp| {
        if super::frame::drawing(m) && m.native.dynamics.is_some() && check_cfg().is_none() { HookResult::Return(0) } else { HookResult::Resume(tramp) }
    }) {
        crate::warn!("native dynamics: the game's scene instances cannot be skipped ({e})");
    }
    install_check(m);
}

// ---------------------------------------------------------------------------------------
// SR_DYN_CHECK

struct CheckCfg {
    first: u64,
    last: u64,
}

fn check_cfg() -> Option<&'static CheckCfg> {
    static C: std::sync::OnceLock<Option<CheckCfg>> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        let v = std::env::var("SR_DYN_CHECK").ok()?;
        let mut it = v.split(',');
        let first = it.next().and_then(|f| f.trim().parse().ok()).unwrap_or(1500);
        let last = it.next().and_then(|f| f.trim().parse().ok()).unwrap_or(u64::MAX);
        Some(CheckCfg { first, last })
    })
    .as_ref()
}

/// After the part transform (as `extract.rs`'s check; the two are not used together).
const AFTER_TRANSFORM: u32 = 0x78e62;
const CUR_INSTANCE: u32 = 0x7bb8c;
const CUR_SLOT: u32 = 0x7babc;
const CUR_PART: u32 = 0x7bac4;
const PART_ORDER: u32 = 0x81d44;
const CAMERA_PTR: u32 = 0x81e0c;
const VERTS: u32 = 0x7bacc;
const VIEW_POS: u32 = 0x68fc8;
const VARIANT: u32 = 0xaa5b0;

#[derive(Default)]
struct CheckStats {
    parts: u64,
    vertices: u64,
    max: f64,
    sum2: f64,
    worst: String,
    /// Engine parts this module placed at the same level / at another level / not at all.
    same_level: u64,
    other_level: u64,
    missing: u64,
    /// By kind: parts, max difference.
    by_kind: [(u64, f64); 5],
    /// Reflection: instances compared, max |accumulator difference|, offsets that differ.
    scroll_compared: u64,
    scroll_max: f64,
    scroll_offsets_differ: u64,
    /// Divergences (resynchronised) by size: < 0.01, < 1, < 10, more.
    scroll_events: [u64; 4],
    /// Vertices in front of the near plane: count, max difference, the worst.
    front: u64,
    front_max: f64,
    front_worst: String,
    first_bad: Vec<String>,
    reported: bool,
}

static CHECK: std::sync::Mutex<Option<CheckStats>> = std::sync::Mutex::new(None);

fn install_check(m: &mut Machine) {
    let Some(cfg) = check_cfg() else { return };
    *CHECK.lock().unwrap() = Some(CheckStats::default());
    match m.install_observer("dynamics check: after transform", AFTER_TRANSFORM, check_part) {
        Ok(()) => crate::info!("dynamics check (SR_DYN_CHECK): game frames {}..{}", cfg.first, cfg.last),
        Err(e) => crate::warn!("dynamics check: {e}"),
    }
}

fn check_scroll(d: &mut Dynamics, step: &Step) {
    let Some(cfg) = check_cfg() else { return };
    if step.frame < cfg.first || step.frame > cfg.last {
        return;
    }
    let mut g = CHECK.lock().unwrap();
    let Some(c) = g.as_mut() else { return };
    // the game's accumulators now (updated by its own drawing of the last frame) against
    // this module's emulation of the same frame
    for (a, s) in &step.insts {
        let Some(&(acc, saved)) = d.shadow.get(a) else { continue };
        c.scroll_compared += 1;
        let diff = (0..3).map(|k| (acc[k] as f64 - s.acc[k] as f64).abs()).fold(0.0, f64::max);
        c.scroll_max = c.scroll_max.max(diff);
        if scroll_of(acc, true) != scroll_of(s.acc, true) {
            c.scroll_offsets_differ += 1;
        }
        // a divergence: counted, then the emulation continues from the game's values (each
        // event is one frame's update the emulation did differently)
        if diff > 1e-3 {
            c.scroll_events[[0.01, 1.0, 10.0].iter().position(|&b| diff < b).unwrap_or(3)] += 1;
            if c.first_bad.len() < 10 {
                c.first_bad.push(format!("frame {}: instance {a:#x} reflection accumulators {:?} (emulated) vs {:?} (game); views tested / in view last frame {:?}", step.frame, acc, s.acc, d.seen.get(a)));
            }
            let game_saved = [rd32(a + 0x24) as i32, rd32(a + 0x28) as i32, rd32(a + 0x2c) as i32];
            let _ = saved;
            d.shadow.insert(*a, (s.acc, game_saved));
        }
    }
}

fn check_part(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let Some(cfg) = check_cfg() else { return HookResult::Resume(tramp) };
    let frame = crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed);
    if crate::interp::in_extra_draw() || frame < cfg.first {
        return HookResult::Resume(tramp);
    }
    let l = |a: u32| m.img.link(a);
    let raw = rd32(l(CUR_INSTANCE));
    if raw & SKIP_BIT == 0 {
        return HookResult::Resume(tramp);
    }
    let mut g = CHECK.lock().unwrap();
    let Some(c) = g.as_mut() else { return HookResult::Resume(tramp) };
    if frame > cfg.last {
        if !c.reported {
            c.reported = true;
            report(c, frame);
        }
        return HookResult::Resume(tramp);
    }
    let Some(d) = m.native.dynamics.as_ref() else { return HookResult::Resume(tramp) };
    let inst = raw & !SKIP_BIT;
    let mirror = rd32(l(VARIANT)) & 1 != 0;
    let slot = unsafe { mem::rd16(l(PART_ORDER) + 4 * rd32(l(CUR_SLOT))) } as u8;
    let data = rd32(l(CUR_PART));
    let view_slot = if mirror { 2 } else { 0 };
    let kind = d.models.get(&rd32(inst)).map_or(Kind::Other, |md| md.kind);
    let Some(p) = d.placed[view_slot].iter().find(|p| p.inst == inst && p.slot == slot) else {
        c.missing += 1;
        if c.first_bad.len() < 10 {
            c.first_bad.push(format!("frame {frame}: the engine draws {kind:?} instance {inst:#x} slot {slot} (part data {data:#x}), not placed natively"));
        }
        return HookResult::Resume(tramp);
    };
    let level_data = d.models.get(&rd32(inst)).and_then(|md| md.slots.get(slot as usize)).map_or(0, |s| s.data[p.level as usize]);
    if level_data == data {
        c.same_level += 1;
    } else {
        c.other_level += 1;
        if c.first_bad.len() < 10 {
            let sl = d.models.get(&rd32(inst)).and_then(|md| md.slots.get(slot as usize));
            let engine = sl.and_then(|s| s.data.iter().position(|&x| x == data));
            let cam = rd32(l(CAMERA_PTR));
            let cp = [rd32(cam + 0xc) as i32 as f64, rd32(cam + 0x10) as i32 as f64, rd32(cam + 0x14) as i32 as f64];
            let dist = sl.map_or(0.0, |s| {
                let w = p.xf.apply(s.centre);
                ((w[0] - cp[0]).powi(2) + (w[1] - cp[1]).powi(2) + (w[2] - cp[2]).powi(2)).sqrt()
            });
            c.first_bad.push(format!(
                "frame {frame}: {kind:?} instance {inst:#x} slot {slot}{}: level {} here, {engine:?} in the engine; distance {dist:.0}, farther {}, thresholds [81e60] {} [81e64] {}",
                if mirror { " (mirror)" } else { "" },
                p.level,
                sl.map_or(0.0, |s| s.farther),
                rd32(l(LOD_NEAR)) as i32,
                rd32(l(LOD_FAR)) as i32
            ));
        }
    }
    // vertices: the part's model vertices through this module's placement and topic D's
    // camera against the engine's view positions (only where the levels agree)
    let cam = rd32(l(CAMERA_PTR));
    if level_data != data || !mem::readable(cam, 0x18) {
        return HookResult::Resume(tramp);
    }
    let cam_angles = [rd32(cam) as i32 as f64, rd32(cam + 4) as i32 as f64, rd32(cam + 8) as i32 as f64];
    let cam_pos = [rd32(cam + 0xc) as i32 as f64, rd32(cam + 0x10) as i32 as f64, rd32(cam + 0x14) as i32 as f64];
    // a billboard's placement depends on the view's camera: recompute it for this view
    let xf = match d.cur.as_ref().and_then(|s| s.insts.get(&inst)) {
        Some(s) if s.flags & BILLBOARD != 0 => part_transform(s, slot as usize, cam_angles),
        _ => p.xf,
    };
    let to_view = world_to_view(cam_pos, cam_angles);
    let verts = rd32(l(VERTS));
    c.parts += 1;
    let kc = &mut c.by_kind[kind as usize];
    kc.0 += 1;
    for i in 0..8192u32 {
        let a = verts + 6 * i;
        if !mem::readable(a, 6) {
            break;
        }
        let x = unsafe { mem::rd16(a) } as i16;
        if x == 0x7fff {
            break;
        }
        let v = [x as f64, unsafe { mem::rd16(a + 2) } as i16 as f64, unsafe { mem::rd16(a + 4) } as i16 as f64];
        let mut q = to_view.apply(xf.apply(v));
        if mirror {
            q[0] = -q[0];
        }
        let e = l(VIEW_POS) + 12 * i;
        let ev = [rd32(e) as i32 as f64, rd32(e + 4) as i32 as f64, rd32(e + 8) as i32 as f64];
        let diff = (0..3).map(|k| (q[k] - ev[k]).abs()).fold(0.0, f64::max);
        c.vertices += 1;
        c.sum2 += (0..3).map(|k| (q[k] - ev[k]).powi(2)).sum::<f64>();
        kc.1 = kc.1.max(diff);
        if ev[2] <= 0.0 {
            c.front += 1;
            if diff > c.front_max {
                c.front_max = diff;
                c.front_worst = format!("frame {frame} {kind:?} instance {inst:#x} slot {slot} vertex {i}{}: native ({:.1}, {:.1}, {:.1}), engine {:?}", if mirror { " (mirror)" } else { "" }, q[0], q[1], q[2], ev);
            }
        }
        if diff > c.max {
            c.max = diff;
            c.worst = format!("frame {frame} {kind:?} instance {inst:#x} slot {slot} vertex {i}{}: native ({:.1}, {:.1}, {:.1}), engine {:?}; instance angles {:?}, part {:?}, eye distance {:.0}", if mirror { " (mirror)" } else { "" }, q[0], q[1], q[2], ev, [0x14u32, 0x18, 0x1c].map(|o| rd32(inst + o) as i32), [8u32, 0xc, 0x10].map(|o| rd32(inst + 0x40 + 0x20 * slot as u32 + o) as i32), 128.0 - ev[2]);
        }
    }
    if c.parts % 200_000 == 0 {
        report(c, frame);
    }
    HookResult::Resume(tramp)
}

fn report(c: &CheckStats, frame: u64) {
    let rms = if c.vertices > 0 { (c.sum2 / (3 * c.vertices) as f64).sqrt() } else { 0.0 };
    crate::info!(
        "dynamics check at game frame {frame}: {} engine parts of scene instances: {} at this module's level, {} at another level, {} not placed natively; {} vertices compared: max {:.2} units, rms {:.2}; by kind (car, shadow, marks, smoke, other) parts / max {:?}; worst: {}",
        c.parts + c.missing,
        c.same_level,
        c.other_level,
        c.missing,
        c.vertices,
        c.max,
        rms,
        c.by_kind.iter().map(|(n, mx)| format!("{n} / {mx:.2}")).collect::<Vec<_>>(),
        c.worst
    );
    crate::info!(
        "dynamics check at game frame {frame}: reflection accumulators emulated against the game's: {} compared, max difference {:.6}, offsets differing {}; divergences (then resynchronised) by size <0.01/<1/<10/more {:?}",
        c.scroll_compared,
        c.scroll_max,
        c.scroll_offsets_differ,
        c.scroll_events
    );
    crate::info!(
        "dynamics check at game frame {frame}: vertices in front of the near plane (view z <= 0): {}, max {:.2} units; worst: {}",
        c.front,
        c.front_max,
        c.front_worst
    );
    for b in &c.first_bad {
        crate::info!("dynamics check: {b}");
    }
}

// ---------------------------------------------------------------------------------------
// SR_DYN_INTERP_CHECK

/// `SR_DYN_INTERP_CHECK=FIRST,LAST` (diagnostics): in the main view's interpolated frames,
/// every car part as drawn (the angles [`lerp_inst`] gave) against the expected motion
/// between the two steps: steering, camber and spin each changing linearly, the short way
/// (each step's from its rotation matrix, [`wheel_angles`]; at 33 steps per second every
/// spin measured stays under half a turn per step). Compared: the part's model vertices
/// through both rotations (the part's offset is the same in both, so it is left out), and
/// the axle's direction (the part's x axis) against the two steps' axles interpolated
/// (normalised linear interpolation: no angle decomposition); the same for the earlier
/// angle-by-angle interpolation of the game's triples, for scale. The steps whose two
/// triples lie on different branches (one written as `(1024 - a0, a1 + 1024, a2 + 1024)`)
/// are counted separately.
struct InterpCheck {
    first: u64,
    last: u64,
    verts: HashMap<u32, Vec<[f64; 3]>>,
    /// [one branch, across branches][drawn, earlier]: parts, vertices, max, sum of squares,
    /// max axle error (degrees).
    stats: [[(u64, u64, f64, f64, f64); 2]; 2],
    /// Parts at t = 1 (the step itself): max vertex difference from the step's own rotation.
    at_step: (u64, f64),
    worst: String,
    reported: bool,
}

static ICHECK: std::sync::Mutex<Option<InterpCheck>> = std::sync::Mutex::new(None);

fn interp_check(md: &Model, a: &InstState, b: &InstState, t: f64, s: &InstState, frame: u64) {
    static CFG: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
    let Some((first, last)) = *CFG.get_or_init(|| {
        let v = std::env::var("SR_DYN_INTERP_CHECK").ok()?;
        let mut it = v.split(',').map(|x| x.trim().parse::<u64>().ok());
        Some((it.next().flatten().unwrap_or(1500), it.next().flatten().unwrap_or(u64::MAX)))
    }) else {
        return;
    };
    let mut g = ICHECK.lock().unwrap();
    let c = g.get_or_insert_with(|| InterpCheck { first, last, verts: HashMap::new(), stats: Default::default(), at_step: (0, 0.0), worst: String::new(), reported: false });
    if frame < c.first {
        return;
    }
    if frame > c.last {
        if !c.reported {
            c.reported = true;
            let line = |k: usize, w: usize| {
                let (p, n, mx, s2, ax) = c.stats[k][w];
                format!("{p} parts, {n} vertices: max {mx:.3}, rms {:.3}; axle max {ax:.3} degrees", if n > 0 { (s2 / (3 * n) as f64).sqrt() } else { 0.0 })
            };
            crate::info!("interpolation check (SR_DYN_INTERP_CHECK) to game frame {}: car parts in interpolated frames against steering, camber and spin interpolated (vertex differences in units)", c.last);
            crate::info!("  steps on one branch: drawn {}; earlier interpolation {}", line(0, 0), line(0, 1));
            crate::info!("  steps across the two branches: drawn {}; earlier interpolation {}", line(1, 0), line(1, 1));
            crate::info!("  at the steps themselves (t = 1): {} parts, max {:.2e}; worst drawn: {}", c.at_step.0, c.at_step.1, c.worst);
        }
        return;
    }
    let mut r = super::world::Reader::default();
    for (slot, sl) in md.slots.iter().enumerate() {
        let Some(level) = md.finest() else { continue };
        let data = sl.data[level];
        if data == 0 || slot >= a.parts.len() {
            continue;
        }
        if !c.verts.contains_key(&data) {
            let v = super::world::read_part(&mut r, data)
                .and_then(|p| super::world::vertices(&mut r, md.addr.wrapping_add(p.verts)))
                .map(|v| v.iter().map(|q| q.map(|x| x as f64)).collect())
                .unwrap_or_default();
            c.verts.insert(data, v);
        }
        let (pa, pb) = (a.parts[slot].0, b.parts[slot].0);
        let (ra, rb) = (rotation(pa), rotation(pb));
        let across = ((pa[2] * ANGLE_UNIT).cos() < 0.0) != ((pb[2] * ANGLE_UNIT).cos() < 0.0);
        let (wa, wb) = (wheel_angles(&ra), wheel_angles(&rb));
        let want = wheel_rotation([0, 1, 2].map(|k| wa[k] + wrap_pi(wb[k] - wa[k]) * t));
        let got = [rotation(s.parts[slot].0), rotation([0, 1, 2].map(|k| lerp_angle(pa[k], pb[k], t)))];
        // the axles: the two steps' interpolated (normalised), and each drawn one
        let axle = [0, 1, 2].map(|i| ra[i][0] + (rb[i][0] - ra[i][0]) * t);
        let n = axle.iter().map(|v| v * v).sum::<f64>().sqrt();
        let at_step = (t - 1.0).abs() < 1e-9;
        for (w, g) in got.iter().enumerate() {
            let e = &mut c.stats[across as usize][w];
            e.0 += 1;
            if n > 1e-6 {
                let dot = (0..3).map(|i| g[i][0] * axle[i] / n).sum::<f64>().clamp(-1.0, 1.0);
                e.4 = e.4.max(dot.acos().to_degrees());
            }
            for v in &c.verts[&data] {
                let p = |m: &Mat3| [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]);
                let (x, y) = (p(g), p(&want));
                let d = (0..3).map(|k| (x[k] - y[k]).abs()).fold(0.0, f64::max);
                e.1 += 1;
                e.3 += (0..3).map(|k| (x[k] - y[k]).powi(2)).sum::<f64>();
                if w == 0 && d > e.2 {
                    c.worst = format!("frame {frame} t {t:.3} instance {:#x} slot {slot}: {d:.3} units; triples {pa:?} -> {pb:?}, placed {:?}", b.addr, s.parts[slot].0);
                }
                e.2 = e.2.max(d);
                if w == 0 && at_step {
                    let z = p(&rb);
                    c.at_step.1 = c.at_step.1.max((0..3).map(|k| (x[k] - z[k]).abs()).fold(0.0, f64::max));
                }
            }
        }
        if at_step {
            c.at_step.0 += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::transforms::part_to_world;

    #[test]
    fn levels_follow_the_engine() {
        // China's thresholds are read at run time; with -8000 / -16000: level 0 nearer than
        // 8000 units, level 1 nearer than 16000, else the stand-in level 2
        assert_eq!(game_level(100.0, -8000, -16000, false), 0);
        assert_eq!(game_level(7999.0, -8000, -16000, false), 0);
        assert_eq!(game_level(8000.0, -8000, -16000, false), 1);
        assert_eq!(game_level(15999.4, -8000, -16000, false), 1);
        assert_eq!(game_level(16000.0, -8000, -16000, false), 2);
        // the mirror never uses level 0
        assert_eq!(game_level(100.0, -8000, -16000, true), 1);
        assert_eq!(game_level(20000.0, -8000, -16000, true), 2);
        // fistp: round half to even
        assert_eq!(round_even(2.5), 2.0);
        assert_eq!(round_even(3.5), 4.0);
        assert_eq!(round_even(-2.5), -2.0);
    }

    #[test]
    fn the_engines_object_cull() {
        // race main view at extension 107: half-size rectangle (854 x 400) / 2, centre
        // (426, 130) at half resolution; far distance 150 000
        let k = [213.0, 426.0, 65.0, 200.0, -150_000.0];
        // view space: x left, y up, z back (in front: z < 0)
        assert!(object_visible([0.0, 0.0, -2000.0], 800.0, false, k));
        // behind the view origin by more than the radius
        assert!(!object_visible([0.0, 0.0, 900.0], 800.0, false, k));
        // reaching the view origin from behind: kept (the engine tests z - r > 0)
        assert!(object_visible([0.0, 0.0, 700.0], 800.0, false, k));
        // beyond the far distance
        assert!(!object_visible([0.0, 0.0, -151_000.0], 800.0, false, k));
        // far to the side
        assert!(!object_visible([20_000.0, 0.0, -2000.0], 800.0, false, k));
        assert!(!object_visible([-20_000.0, 0.0, -2000.0], 800.0, false, k));
        // the sphere's extent is projected at its nearest depth: a sphere just off the left
        // edge at its centre's depth is still kept
        let edge = 213.0 * (2000.0 + 128.0) / 128.0;
        assert!(object_visible([edge + 500.0, 0.0, -2000.0], 800.0, false, k));
    }

    #[test]
    fn name_rule_of_the_selector() {
        assert_eq!(name_farther(*b"fli\0"), 4000.0);
        assert_eq!(name_farther(*b"brx\0"), 1000.0);
        assert_eq!(name_farther(*b"fai\0"), 4000.0);
        // four characters or more, or starting with A / a: no change
        assert_eq!(name_farther(*b"faut"), 0.0);
        assert_eq!(name_farther(*b"axi\0"), 0.0);
        assert_eq!(name_farther(*b"MV_m"), 0.0);
    }

    fn inst(pos: [f64; 3], angles: [f64; 3], parts: Vec<([f64; 3], [f64; 3])>) -> InstState {
        InstState { addr: 0x1000, model: 7, flags: 0, pos, angles, anim: -1, parts, wheel: Vec::new(), acc: [0.0; 3] }
    }

    #[test]
    fn parts_are_placed_as_topic_d() {
        // world = Ro (Rp v + tp) + po: transforms.rs's recipe with the part from instance B
        let s = inst([-60231.0, -156.0, 10994.0], [4.0, 1549.0, 2044.0], vec![([0.0; 3], [0.0; 3]), ([2043.0, 9.0, 0.0], [210.0, 112.0, -408.0])]);
        let xf = part_transform(&s, 1, [0.0; 3]);
        let d = part_to_world(s.pos, s.angles, &Affine::new(rotation([2043.0, 9.0, 0.0]), [210.0, 112.0, -408.0]));
        for v in [[0.0, 102.0, -42.0], [-280.0, 279.0, -433.0]] {
            let (a, b) = (xf.apply(v), d.apply(v));
            for k in 0..3 {
                assert!((a[k] - b[k]).abs() < 1e-9);
            }
        }
        // the contract's transform carries it (f32 basis, f64 origin)
        let t = to_transform(&xf);
        let p = t.apply([0.0, 102.0, -42.0]);
        let q = xf.apply([0.0, 102.0, -42.0]);
        for k in 0..3 {
            assert!((p[k] - q[k]).abs() < 1e-3);
        }
    }

    #[test]
    fn billboards_face_the_camera() {
        // the integer formula of transforms.rs for whole angles
        let cam = [1950, 1543, 44];
        let b = billboard_angles([10.0, 300.0, 0.0], cam.map(|a| a as f64));
        let i = crate::native::transforms::billboard_angles(10, 300, cam);
        assert_eq!(b, i.map(|a| a as f64));
        // a billboard's placement follows the camera: rotated with the view, its local -z
        // (the side the camera sees) points back at the camera
        let mut s = inst([1000.0, 0.0, -5000.0], [0.0; 3], vec![([0.0; 3], [0.0; 3])]);
        s.flags = BILLBOARD;
        for cam in [[0.0, 0.0, 0.0], [0.0, 512.0, 0.0], [1948.0, 1000.0, 0.0]] {
            let xf = part_transform(&s, 0, cam);
            let r = rotation(cam);
            // camera forward (world) = -Rc[:, 2]; the billboard's z axis = xf.r[:, 2]
            let dot: f64 = (0..3).map(|k| -r[k][2] * xf.r[k][2]).sum();
            assert!(dot.abs() > 0.999, "{cam:?}: {dot}");
        }
    }

    /// Largest element difference of the rotations of two angle triples.
    fn rot_diff(a: [f64; 3], b: [f64; 3]) -> f64 {
        let (ra, rb) = (rotation(a), rotation(b));
        (0..9).map(|i| (ra[i / 3][i % 3] - rb[i / 3][i % 3]).abs()).fold(0.0, f64::max)
    }

    #[test]
    fn interpolation_is_float_and_short_way() {
        let a = inst([0.0, 0.0, 0.0], [2040.0, 10.0, 0.0], vec![([2000.0, 0.0, 0.0], [210.0, 112.0, -408.0])]);
        let mut b = inst([101.0, -3.0, 7.0], [8.0, 2000.0, 0.0], vec![([40.0, 9.0, 0.0], [210.0, 120.0, -408.0])]);
        b.acc = [10.0, 0.0, -4.0];
        let none = SpinRates::default();
        let c = lerp_inst(&a, &b, 0.5, false, &none);
        assert_eq!(c.pos, [50.5, -1.5, 3.5]);
        // pitch -8 -> 8 and heading 10 -> -48 at once: half way, within the cross term of the
        // two turns
        assert!(rot_diff(c.angles, [0.0, 2029.0, 0.0]) < 2e-3, "{:?}", c.angles);
        // the wheel spin across the wrap (2000 -> 40: 88 units forward) with the steering,
        // and the suspension
        assert!(rot_diff(c.parts[0].0, [2044.0, 4.5, 0.0]) < 1e-9, "{:?}", c.parts[0].0);
        assert_eq!(c.parts[0].1, [210.0, 116.0, -408.0]);
        assert_eq!(c.acc, [5.0, 0.0, -2.0]);
        // the ends are the steps themselves
        assert!(rot_diff(lerp_inst(&a, &b, 0.0, false, &none).parts[0].0, a.parts[0].0) < 1e-12);
        assert!(rot_diff(lerp_inst(&a, &b, 1.0, false, &none).angles, b.angles) < 1e-12);
        // one axis: exactly linear, the short way across the wrap
        let h = lerp_rotation([0.0, 2040.0, 0.0], [0.0, 8.0, 0.0], 0.5);
        assert!(h[0].abs() < 1e-9 && (h[1] - 0.0).abs().min((h[1] - 2048.0).abs()) < 1e-9 && h[2].abs() < 1e-9, "{h:?}");
        // a jump (a reused puff) is not swept
        let far = inst([5000.0, 0.0, 0.0], [0.0; 3], vec![([0.0; 3], [0.0; 3])]);
        assert_eq!(lerp_inst(&a, &far, 0.5, true, &none).pos, far.pos);
        // smoke grows within a puff; car states are not interpolated
        let (mut s0, mut s1) = (a.clone(), a.clone());
        s0.anim = 1000;
        s1.anim = 3000;
        assert_eq!(lerp_inst(&s0, &s1, 0.5, true, &none).anim, 2000);
        assert_eq!(lerp_inst(&s0, &s1, 0.5, false, &none).anim, 3000);
    }

    #[test]
    fn puffs_follow_the_games_order_and_dissolve() {
        // the cars' places nearest first; a puff covers the places from 1 + the cars nearer
        // than it (the game sorts far to near: a puff nearer than a car's position is drawn
        // after it, over it)
        let cars = [3000.0, 1200.0, 1200.0, 8000.0];
        assert_eq!(car_places(&cars), vec![3, 1, 2, 4]);
        assert_eq!(first_place_behind(&cars, 1000.0), 1);
        assert_eq!(first_place_behind(&cars, 2000.0), 3);
        assert_eq!(first_place_behind(&cars, 9000.0), 5);
        assert_eq!(first_place_behind(&[], 10.0), 1);
        // a puff's life (measured: fixed position, +CABh per step, a new puff after a gap)
        let mut a = inst([100.0, 20.0, -300.0], [0.0; 3], vec![([0.0; 3], [0.0; 3])]);
        a.anim = 0xcab;
        let mut b = a.clone();
        b.anim = 0x1956;
        assert!(same_puff(&a, &b));
        assert!(!same_puff(&b, &a), "the animation went back: a new puff");
        let mut moved = b.clone();
        moved.pos[0] += 5000.0;
        assert!(!same_puff(&a, &moved));
        let d = puff_dissolve(Some(&a), Some(&b), 0.25);
        assert_eq!((d.from, d.to, d.t), (Some(phase_of(0xcab)), Some(phase_of(0x1956)), 0.25));
        assert!(puff_dissolve(None, Some(&b), 0.5).active() && puff_dissolve(Some(&a), None, 0.5).active());
        assert!(!crate::native::scene::Dissolve::NONE.active());
    }

    /// The game's other triple for the same orientation (as its wheels flip to it).
    fn flipped(a: [f64; 3]) -> [f64; 3] {
        [(1024.0 - a[0]).rem_euclid(2048.0), (a[1] + 1024.0).rem_euclid(2048.0), (a[2] + 1024.0).rem_euclid(2048.0)]
    }

    #[test]
    fn wheel_angles_take_any_orientation_apart() {
        // steering, camber, spin (innermost): round trips, also at the spins where the
        // game's own order is degenerate (a0 = +-512) and on the other branch
        for a in [[0.0, 0.0, 0.0], [511.0, 1713.0, 429.0], [512.0, 30.0, 0.0], [1536.0, 2000.0, 7.0], [1599.0, 13.0, 3.0], [138.0, 1024.0, 1023.0], [1000.0, 300.0, 100.0]] {
            let r = rotation(a);
            let back = wheel_rotation(wheel_angles(&r));
            let d = (0..9).map(|i| (r[i / 3][i % 3] - back[i / 3][i % 3]).abs()).fold(0.0, f64::max);
            assert!(d < 1e-12, "{a:?}: {d}");
            assert!(rot_diff(a, angles_of(&r)) < 1e-12, "{a:?}");
        }
        // a wheel steered s and spun p: (s, 0, p), whatever the triple
        for (spin, steer) in [(400.0, 11.0), (520.0, 11.0), (1600.0, 2040.0), (512.0, 94.0)] {
            let w = wheel_angles(&rotation([spin, steer, 0.0]));
            let u = |x: f64| (x / ANGLE_UNIT).rem_euclid(2048.0);
            assert!((u(w[0]) - steer).abs() < 1e-9 && w[1].abs() < 1e-12 && (u(w[2]) - spin).abs() < 1e-9, "{w:?}");
        }
        // the triple the game wrote at spin 511, steering 94 (Shield race, frame 2079): the
        // steering and roll angles split arbitrarily (1713 + 429 = 2142 = 94 + 2048)
        let w = wheel_angles(&rotation([511.0, 1713.0, 429.0])).map(|x| x / ANGLE_UNIT);
        assert!((w[0] - 94.0).abs() < 0.1 && w[1].abs() < 1.0 && (w[2] - 511.0).abs() < 1.0, "{w:?}");
    }

    #[test]
    fn wheels_spin_about_their_axle() {
        // the two triples are one orientation
        for a in [[138.0, 0.0, 0.0], [396.0, 2.0, 0.0], [1599.0, 13.0, 3.0]] {
            assert!(rot_diff(a, flipped(a)) < 1e-12);
            assert!(rot_diff(a, angles_of(&rotation(flipped(a)))) < 1e-12);
        }
        // the game's frames: spin 400 then 520 (a quarter turn passed: written as the other
        // triple), steering 11
        let (a, b) = ([400.0, 11.0, 0.0], flipped([520.0, 11.0, 0.0]));
        assert_eq!(b, [504.0, 1035.0, 1024.0]);
        for t in [0.25, 0.5, 0.75] {
            let c = lerp_part(a, b, t, None);
            // the steering's and the spin's own interpolation
            assert!(rot_diff(c, [400.0 + 120.0 * t, 11.0, 0.0]) < 1e-9, "{t}: {c:?}");
            // the axle (the part's x axis, turned only by the steering) stays where it is;
            // angle by angle (the earlier interpolation) it tumbled
            let axle = |x: [f64; 3]| {
                let r = rotation(x);
                [r[0][0], r[1][0], r[2][0]]
            };
            let want = axle([0.0, 11.0, 0.0]);
            let got = axle(c);
            assert!((0..3).all(|k| (got[k] - want[k]).abs() < 1e-9), "{got:?}");
            let old = [0, 1, 2].map(|k| lerp_angle(a[k], b[k], t));
            let tumble = axle(old);
            assert!((0..3).any(|k| (tumble[k] - want[k]).abs() > 0.3), "{tumble:?}");
        }
    }

    #[test]
    fn fast_spins_keep_their_direction() {
        // the game's triple of a wheel spun by `a` units, steering 5
        let s = |a: f64| angles_of(&rotation([a.rem_euclid(2048.0), 5.0, 0.0]));
        // a car heading along world -z (heading 0), its wheel 2.17 angle units per unit of
        // travel (radius 150)
        let k = 2048.0 / (std::f64::consts::TAU * 150.0);
        let car = |z: f64, spin: f64| inst([0.0, 0.0, z], [0.0; 3], vec![([0.0; 3], [0.0; 3]), (s(spin), [200.0, 100.0, -400.0])]);
        let mut rates = SpinRates::default();
        // nothing learned: the short way (158 degrees per step forward)
        let c = lerp_inst(&car(0.0, 0.0), &car(-414.0, 414.0 * k), 0.5, false, &rates);
        assert!(rot_diff(c.parts[1].0, [207.0 * k, 5.0, 0.0]) < 1e-9);
        // 500 units of travel: 1086 units (191 degrees) of spin; the short way turns back
        let (a, b) = (car(-1000.0, 3000.0), car(-1500.0, 3000.0 + 500.0 * k));
        let c = lerp_inst(&a, &b, 0.5, false, &rates);
        assert!(rot_diff(c.parts[1].0, [3000.0 + 250.0 * k - 1024.0, 5.0, 0.0]) < 1e-9);
        // a slow step (200 units, 76 degrees) teaches the rate (body slot 0 learns nothing)
        rates.learn(&car(0.0, 0.0), &car(-200.0, 200.0 * k));
        assert!(rates.0.get(&(0x1000, 0)).is_none());
        let r = rates.0[&(0x1000, 1)];
        assert!((r - k * ANGLE_UNIT).abs() < 1e-9, "{r}");
        // now the fast step goes on forward
        for t in [0.25, 0.5, 0.75] {
            let c = lerp_inst(&a, &b, t, false, &rates);
            assert!(rot_diff(c.parts[1].0, [3000.0 + 500.0 * t * k, 5.0, 0.0]) < 1e-9, "{t}");
        }
        // reversing: backwards
        let c = lerp_inst(&b, &a, 0.5, false, &rates);
        assert!(rot_diff(c.parts[1].0, [3000.0 + 250.0 * k, 5.0, 0.0]) < 1e-9);
        // a locked wheel (no spin) or a slow one stays the short way whatever the travel
        let c = lerp_inst(&car(0.0, 100.0), &car(-500.0, 110.0), 0.5, false, &rates);
        assert!(rot_diff(c.parts[1].0, [105.0, 5.0, 0.0]) < 1e-9);
        let c = lerp_inst(&car(0.0, 100.0), &car(-500.0, 100.0), 0.5, false, &rates);
        assert!(rot_diff(c.parts[1].0, [100.0, 5.0, 0.0]) < 1e-9);
    }

    /// `cargo test --release -p sr-runtime bench_interpolation -- --ignored --nocapture`: the
    /// cost of placing a car in an interpolated frame (12 parts, the 8 wheel parts turning),
    /// against the first version's angle-by-angle interpolation.
    #[test]
    #[ignore]
    fn bench_interpolation() {
        let k = 2048.0 / (std::f64::consts::TAU * 150.0);
        let car = |z: f64, spin: f64, steer: f64| {
            let mut parts = vec![([1.0, 0.0, 2045.0], [-2.0, 19.0, 0.0]); 4];
            for w in 0..8 {
                let a = if w < 4 { [spin.rem_euclid(2048.0), steer, 0.0] } else { [0.0, steer, 0.0] };
                parts.push((angles_of(&rotation(a)), [200.0, 100.0, -400.0]));
            }
            let mut s = inst([0.0, 0.0, z], [8.0, 304.0, 1985.0], parts);
            s.wheel = s.parts.iter().map(|p| wheel_angles(&rotation(p.0))).collect();
            s
        };
        let (a, b) = (car(0.0, 300.0, 11.0), car(-400.0, 300.0 + 400.0 * k, 14.0));
        let rates = SpinRates::default();
        let n = 200_000;
        let place = |s: &InstState| (0..s.parts.len()).map(|i| part_transform(s, i, [0.0; 3]).t[0]).sum::<f64>();
        let t0 = std::time::Instant::now();
        let mut acc = 0.0;
        for i in 0..n {
            let s = lerp_inst(&a, &b, (i % 3 + 1) as f64 / 3.0, false, &rates);
            acc += place(&s);
        }
        let now = t0.elapsed().as_secs_f64() / n as f64;
        let t1 = std::time::Instant::now();
        for i in 0..n {
            let t = (i % 3 + 1) as f64 / 3.0;
            let mut s = b.clone();
            for (o, (pa, pb)) in s.parts.iter_mut().zip(a.parts.iter().zip(&b.parts)) {
                o.0 = [0, 1, 2].map(|k| lerp_angle(pa.0[k], pb.0[k], t));
            }
            s.angles = [0, 1, 2].map(|k| lerp_angle(a.angles[k], b.angles[k], t));
            acc += place(&s);
        }
        let before = t1.elapsed().as_secs_f64() / n as f64;
        println!("a car in an interpolated frame: {:.2} us now, {:.2} us angle by angle ({acc:.0})", now * 1e6, before * 1e6);
    }

    #[test]
    fn reflection_scroll_as_the_game() {
        // identity camera matrix, rate 0.008: 125 units along x add 1.0 to the x accumulator
        let id = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let acc = scroll_update([0.5, 0.0, 255.4], [125, 0, -50], &id, 0.008);
        assert!((acc[0] - 1.5).abs() < 1e-6 && (acc[2] - 255.0).abs() < 1e-5);
        // the offsets: round half to even, & FFh (x and z)
        assert_eq!(scroll_of([1.5, 0.0, 255.0], true), [2.0, 255.0]);
        assert_eq!(scroll_of([2.5, 0.0, 256.2], true), [2.0, 0.0]);
        assert_eq!(scroll_of([-1.0, 0.0, 0.0], true), [255.0, 0.0]);
        // interpolated frames keep the fraction
        assert_eq!(scroll_of([-1.25, 0.0, 300.5], false), [254.75, 44.5]);
        // the matrix layout: x' = m0 x + m3 y + m6 z
        let m = [0.0f32, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let r = scroll_update([0.0; 3], [100, 0, 0], &m, 1.0);
        assert_eq!(r, [0.0, 100.0, 0.0]);
    }
}
