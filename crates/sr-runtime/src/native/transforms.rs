//! Transforms, camera and projection of the 3D engine (STRT3FX), in floating point.
//!
//! The math is documented step by step in `docs/re/native/transforms.md`. In short, for a
//! model vertex `v` (int16) of part `p` of object `o` seen by camera `c`:
//!
//! ```text
//! view = Rc^T * (Ro * (Rp * v + tp) + (po - pc))        (82d04, 79638, 7a920, 7aa00)
//! R(a0, a1, a2) = Ry(-a1) * Rx(a0) * Rz(a2)              (793b0; angle unit 2*pi/2048)
//! d = 128 - view.z  (eye 128 units behind the view origin; near plane view.z = 0)
//! grid x = C + 256 * view.x / (view.z - 128)              (C = 319 + widescreen extension)
//! grid y = cy + 256 * view.y / (view.z - 128)             (mirror: 128; cy = [81e3c])
//! Glide x = xtable(grid x), Glide y = 1.2 * (grid y + y offset)
//! ```
//!
//! The engine does this in Q14 fixed point and whole units (`rec+118h` integers, `fistp`
//! view positions, `sar` screen positions). This module has both: an exact replica of the
//! engine's integer pipeline (to prove the reading of the code: it must reproduce the
//! engine's numbers bit for bit) and the float model the native renderer uses.
//!
//! `SR_TRANSFORMS_CHECK=FIRST[,LAST[,CSV]]` (diagnostics, off by default): for every object
//! part the engine transforms in game frames FIRST..=LAST (observer after the transform,
//! 78e58), recompute its matrix and every vertex's view and screen position from the
//! instance (position, angles), the camera structure and the model only, and compare with
//! the engine's (`rec+118h`, `68fc8 + 12 n`, `6ed94 + 6 n`). The statistics go to the log
//! (per frame and in all at the end); with CSV, one line per vertex of frames FIRST,
//! FIRST + 100, ... goes to that file (put it under `work/`).

use std::f64::consts::TAU;

/// One game angle unit (angles are 11 bits, 0..2047 = one turn).
pub const ANGLE_UNIT: f64 = TAU / 2048.0;
/// The packed screen position of a vertex behind the near plane (or overflowing).
pub const BEHIND: u32 = 0x1000_1000;
/// The eye lies 128 units behind the view origin: d = 128 - z is the distance along the
/// viewing direction; the near plane is z = 0 (d = 128).
pub const EYE: f64 = 128.0;
/// Focal length of the 640 x 400 grid (both axes; the mirror's y uses half of it).
pub const FOCAL: f64 = 256.0;
/// Glide y = 1.2 * grid y (the 400-line grid stretched to 480).
pub const Y_SCALE: f64 = 1.2;
/// The projection centre in the grid's columns (`add edx, 13F0000h`; widescreen adds e).
pub const CENTRE_X: f64 = 319.0;

// ----------------------------------------------------------------------------------------
// Engine replica (integer pipeline, bit exact)

/// The engine's sine table entry for an angle (c48e0 + 8 a: sin, then cos, as int16 in the
/// low words): trunc(32768 * sin), trunc(32768 * cos), clamped to +-32767. The engine reads
/// it from memory; this generator documents it: the check found 63 of the 2048 entries
/// differing from this by 1 (either way), each where 32768 sin (cos) lies within 0.013 of a
/// whole number (27411.996 -> 27412, -1407.0008 -> -1406): the table was made with slightly
/// different arithmetic. The replica below uses the game's table.
pub fn table_entry(a: i32) -> (i32, i32) {
    let x = (a & 2047) as f64 * ANGLE_UNIT;
    let q = |v: f64| ((v * 32768.0) as i64).clamp(-32767, 32767) as i32;
    (q(x.sin()), q(x.cos()))
}

/// The Q14 rotation built by `793b0` from three angles (s = sin >> 1, c = cos >> 1), as the
/// engine stores it: element `e[r + 3 c]` is row r, column c of the matrix that maps local
/// to parent coordinates (`x' = e0 x + e3 y + e6 z`). Equal to Ry(-a1) Rx(a0) Rz(a2) with
/// the engine's truncations. `sc(a)` gives the table's (sin, cos) for an angle.
pub fn q14_rotation(a: [i32; 3], sc: &dyn Fn(i32) -> (i32, i32)) -> [i32; 9] {
    let (s0, c0) = sc(a[0]);
    let (s1, c1) = sc(a[1]);
    let (s2, c2) = sc(a[2]);
    let (s0, c0, s1, c1, s2, c2) = (s0 >> 1, c0 >> 1, s1 >> 1, c1 >> 1, s2 >> 1, c2 >> 1);
    let m = |x: i32, y: i32| x.wrapping_mul(y);
    let s1s0 = m(s1, s0) >> 14;
    let c1s0 = m(c1, s0) >> 14;
    let mut e = [0i32; 9];
    e[0] = m(c2, c1).wrapping_sub(m(s1s0, s2)) >> 14;
    e[1] = m(s2, c0) >> 14;
    e[2] = m(c1s0, s2).wrapping_add(m(s1, c2)) >> 14;
    e[3] = m(c2, s1s0).wrapping_add(m(c1, s2)).wrapping_neg() >> 14;
    e[4] = m(c2, c0) >> 14;
    e[5] = m(c1s0, c2).wrapping_sub(m(s1, s2)) >> 14;
    e[6] = m(c0, s1).wrapping_neg() >> 14;
    e[7] = -s0;
    e[8] = m(c1, c0) >> 14;
    e
}

/// The transpose (the inverse rotation), as `82d04` stores it at +48h.
pub fn transpose9(e: &[i32; 9]) -> [i32; 9] {
    [e[0], e[3], e[6], e[1], e[4], e[7], e[2], e[5], e[8]]
}

/// A 3 x 4 engine matrix (9 Q14 rotation elements + translation in whole units).
pub fn with_translation(r: &[i32; 9], t: [i32; 3]) -> [i32; 12] {
    let mut m = [0i32; 12];
    m[..9].copy_from_slice(r);
    m[9..].copy_from_slice(&t);
    m
}

/// `79638`: C = A after B (apply B, then A), 32-bit products, each sum `sar 14`.
pub fn mul_q14(a: &[i32; 12], b: &[i32; 12]) -> [i32; 12] {
    let mut c = [0i32; 12];
    let dot = |r: usize, b0: i32, b1: i32, b2: i32| a[r].wrapping_mul(b0).wrapping_add(a[r + 3].wrapping_mul(b1)).wrapping_add(a[r + 6].wrapping_mul(b2)) >> 14;
    for r in 0..3 {
        for col in 0..3 {
            c[r + 3 * col] = dot(r, b[3 * col], b[3 * col + 1], b[3 * col + 2]);
        }
        c[9 + r] = dot(r, b[9], b[10], b[11]).wrapping_add(a[9 + r]);
    }
    c
}

/// Whether one of `mul_q14`'s sums leaves the 32-bit range (the engine's result then wraps:
/// a part whose translation exceeds about 2^31 / 16384 = 131072 units).
pub fn q14_overflows(a: &[i32; 12], b: &[i32; 12]) -> bool {
    let dot = |r: usize, b0: i32, b1: i32, b2: i32| a[r] as i64 * b0 as i64 + a[r + 3] as i64 * b1 as i64 + a[r + 6] as i64 * b2 as i64;
    (0..3).any(|r| (0..4).any(|col| i32::try_from(dot(r, b[3 * col], b[3 * col + 1], b[3 * col + 2])).is_err()))
}

/// The engine's angles for an object drawn facing the camera (flag 8000h at +4 of its
/// instance; `52dc0`, from the instance's +14h and +18h angles and the camera's angles).
pub fn billboard_angles(inst_a0: i32, inst_a1: i32, cam: [i32; 3]) -> [i32; 3] {
    [
        inst_a0.wrapping_sub(cam[0]) & 0x7ff,
        (inst_a1.wrapping_sub(cam[1]).wrapping_add(0x400)).wrapping_neg() & 0x7ff,
        0x7ff_i32.wrapping_sub(cam[2]),
    ]
}

/// The camera-space matrix of a part (`rec+118h`), as the engine builds it in `826ec`
/// (instanced objects) and `827d8` (others):
/// part (angles + offset, or an animation frame's matrix) -> object (angles, position
/// relative to the camera) -> view (the camera's transpose, no translation).
pub fn engine_part_matrix(cam_rt: &[i32; 9], obj: &[i32; 12], part: &[i32; 12]) -> [i32; 12] {
    let op = mul_q14(obj, part);
    mul_q14(&with_translation(cam_rt, [0; 3]), &op)
}

/// `7a920` + `7aa00` / `7ab44`: the matrix as floats (rotation / 16384, translation as
/// is), the vertex transformed in floating point and stored with `fistp` (round to nearest
/// even; the guest's x87 control word is 037Fh). The mirror variant negates x first.
/// Every step is exact in f64 (int16 times a 24-bit float, three sums), as in the x87.
pub fn engine_view(m: &[i32; 12], v: [i16; 3], mirror: bool) -> [i32; 3] {
    engine_view_unrounded(m, v, mirror).map(|c| c.round_ties_even() as i32)
}

/// `engine_view` before the `fistp`.
pub fn engine_view_unrounded(m: &[i32; 12], v: [i16; 3], mirror: bool) -> [f64; 3] {
    let f = |k: usize| if k < 9 { (m[k] as f64 / 16384.0) as f32 as f64 } else { m[k] as f32 as f64 };
    let (x, y, z) = (v[0] as f64, v[1] as f64, v[2] as f64);
    let vx = f(0) * x + f(3) * y + f(6) * z + f(9);
    let vy = f(1) * x + f(4) * y + f(7) * z + f(10);
    let vz = f(2) * x + f(5) * y + f(8) * z + f(11);
    [if mirror { -vx } else { vx }, vy, vz]
}

/// The integer projection of `7aa00` (`7ab44` for the mirror): r = 800000h / (z - 128)
/// (truncated), x = (2 x r + centre << 16) >> 16, y = ((y r) >> 15) + cy (mirror >> 16);
/// packed as x << 16 | y. `BEHIND` for z > 0 or when 2 x r + centre overflows.
pub fn engine_project(v: [i32; 3], centre_hi: i32, cy: i32, mirror: bool) -> u32 {
    let z = v[2].wrapping_sub(128);
    if z > -128 {
        return BEHIND;
    }
    let r = (0x80_0000i64 / z as i64) as i32;
    let y = v[1].wrapping_mul(r) >> if mirror { 16 } else { 15 };
    let x = v[0].wrapping_mul(r);
    let Some(x) = x.checked_add(x).and_then(|x| x.checked_add(centre_hi)) else {
        return BEHIND;
    };
    (x as u32 & 0xffff_0000) | (y.wrapping_add(cy) as u32 & 0xffff)
}

/// Unpack a packed screen position (grid column, grid row).
pub fn unpack(p: u32) -> (i32, i32) {
    ((p as i32) >> 16, p as u16 as i16 as i32)
}

// ----------------------------------------------------------------------------------------
// Float model (what the native renderer uses)

pub type Mat3 = [[f64; 3]; 3];

/// The rotation of three game angles (in angle units, fractional allowed): local ->
/// parent, `R = Ry(-a1) * Rx(a0) * Rz(a2)` with the standard right-handed axis rotations
/// (Rx(t) = [[1,0,0],[0,c,-s],[0,s,c]], Ry(t) = [[c,0,s],[0,1,0],[-s,0,c]],
/// Rz(t) = [[c,-s,0],[s,c,0],[0,0,1]]).
pub fn rotation(a: [f64; 3]) -> Mat3 {
    let (s0, c0) = (a[0] * ANGLE_UNIT).sin_cos();
    let (s1, c1) = (a[1] * ANGLE_UNIT).sin_cos();
    let (s2, c2) = (a[2] * ANGLE_UNIT).sin_cos();
    [
        [c2 * c1 - s2 * s1 * s0, -c2 * s1 * s0 - s2 * c1, -c0 * s1],
        [s2 * c0, c2 * c0, -s0],
        [s2 * c1 * s0 + c2 * s1, c2 * c1 * s0 - s2 * s1, c1 * c0],
    ]
}

/// A rigid transform `p -> r * p + t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub r: Mat3,
    pub t: [f64; 3],
}

impl Affine {
    pub const IDENTITY: Affine = Affine { r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] };

    pub fn new(r: Mat3, t: [f64; 3]) -> Affine {
        Affine { r, t }
    }

    /// From an engine matrix (e[r + 3 c] / 16384, translation as is): animation frames.
    pub fn from_engine(m: &[i32; 12]) -> Affine {
        let mut r = [[0.0; 3]; 3];
        for (row, rr) in r.iter_mut().enumerate() {
            for (col, v) in rr.iter_mut().enumerate() {
                *v = m[row + 3 * col] as f64 / 16384.0;
            }
        }
        Affine { r, t: [m[9] as f64, m[10] as f64, m[11] as f64] }
    }

    pub fn apply(&self, p: [f64; 3]) -> [f64; 3] {
        let r = &self.r;
        [
            r[0][0] * p[0] + r[0][1] * p[1] + r[0][2] * p[2] + self.t[0],
            r[1][0] * p[0] + r[1][1] * p[1] + r[1][2] * p[2] + self.t[1],
            r[2][0] * p[0] + r[2][1] * p[1] + r[2][2] * p[2] + self.t[2],
        ]
    }

    /// `self` after `b` (apply b first).
    pub fn after(&self, b: &Affine) -> Affine {
        let mut r = [[0.0; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| self.r[i][k] * b.r[k][j]).sum();
            }
        }
        let t = self.apply(b.t);
        Affine { r, t }
    }

    /// The inverse of a rigid transform (rotation transposed).
    pub fn inverse(&self) -> Affine {
        let r = &self.r;
        let rt = [[r[0][0], r[1][0], r[2][0]], [r[0][1], r[1][1], r[2][1]], [r[0][2], r[1][2], r[2][2]]];
        let t = [
            -(rt[0][0] * self.t[0] + rt[0][1] * self.t[1] + rt[0][2] * self.t[2]),
            -(rt[1][0] * self.t[0] + rt[1][1] * self.t[1] + rt[1][2] * self.t[2]),
            -(rt[2][0] * self.t[0] + rt[2][1] * self.t[1] + rt[2][2] * self.t[2]),
        ];
        Affine { r: rt, t }
    }

    /// 3 x 4, row-major (`Instance::transform` of the scene description).
    pub fn to_3x4(&self) -> [f32; 12] {
        let mut out = [0f32; 12];
        for i in 0..3 {
            for j in 0..3 {
                out[i * 4 + j] = self.r[i][j] as f32;
            }
            out[i * 4 + 3] = self.t[i] as f32;
        }
        out
    }
}

/// Model -> world of an object part: object at `pos` with angles `obj`, part placed by
/// `part` (angles + offset, or an animation frame's matrix) in the object.
pub fn part_to_world(pos: [f64; 3], obj: [f64; 3], part: &Affine) -> Affine {
    Affine::new(rotation(obj), pos).after(part)
}

/// World -> view of a camera at `pos` with angles `cam` (the camera structure's dwords 3-5
/// and 0-2): view = Rc^T (p - pos).
pub fn world_to_view(pos: [f64; 3], cam: [f64; 3]) -> Affine {
    Affine::new(rotation(cam), pos).inverse()
}

/// Grid position (640 x 400 game pixels, before the engine's rounding down) of a view
/// position; None behind the near plane (z > 0). `centre` = 319 + widescreen extension,
/// `cy` = [81e3c]; the mirror's y uses half the focal length (and its x arrives negated).
pub fn project(v: [f64; 3], centre: f64, cy: f64, mirror: bool) -> Option<(f64, f64)> {
    if v[2] > 0.0 {
        return None;
    }
    let k = 1.0 / (v[2] - EYE);
    let fy = if mirror { FOCAL / 2.0 } else { FOCAL };
    Some((centre + FOCAL * v[0] * k, cy + fy * v[1] * k))
}

/// The main view's projection as a standard off-centre perspective (right-handed, looking
/// down -z, D3D clip depth 0..1), mapping to Glide's 640 x 480 coordinates. Use it with the
/// view matrix `view_matrix(world_to_view(..), mirror)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum {
    /// Extents of the near plane in eye space (x right, y up), at distance `near`.
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
    pub top: f64,
    pub near: f64,
    pub far: f64,
}

impl Frustum {
    /// The view covering Glide x `x0..x1` and y `y0..y1` (the main view: -e .. 640 + e and
    /// 0 .. 480), centre column `CENTRE_X` (319) and row `cy` (grid units, [81e3c]), far
    /// plane at view z = -far_distance (set_far's distance). `mirror`: half focal in y.
    pub fn glide(x0: f64, x1: f64, y0: f64, y1: f64, cy: f64, far_distance: f64, mirror: bool) -> Frustum {
        let fx = FOCAL;
        let fy = if mirror { FOCAL / 2.0 } else { FOCAL } * Y_SCALE;
        let gcy = cy * Y_SCALE;
        let n = EYE;
        Frustum {
            left: (x0 - CENTRE_X) * n / fx,
            right: (x1 - CENTRE_X) * n / fx,
            top: (gcy - y0) * n / fy,
            bottom: (gcy - y1) * n / fy,
            near: n,
            far: far_distance + EYE,
        }
    }

    /// Column-vector 4 x 4 (clip = P * eye), D3D depth range 0..1 (near -> 0, far -> 1),
    /// = D3DXMatrixPerspectiveOffCenterRH transposed.
    pub fn matrix(&self) -> [[f64; 4]; 4] {
        let (l, r, b, t, n, f) = (self.left, self.right, self.bottom, self.top, self.near, self.far);
        [
            [2.0 * n / (r - l), 0.0, (r + l) / (r - l), 0.0],
            [0.0, 2.0 * n / (t - b), (t + b) / (t - b), 0.0],
            [0.0, 0.0, f / (n - f), n * f / (n - f)],
            [0.0, 0.0, -1.0, 0.0],
        ]
    }
}

/// The game's view space -> standard eye space (x right, y up, looking down -z, eye at the
/// origin): x is mirrored (the game's view x points left on screen) except in the rear-view
/// mirror, whose transform already negates it; the origin moves 128 units back to the eye.
pub fn game_view_to_eye(mirror: bool) -> Affine {
    let sx = if mirror { 1.0 } else { -1.0 };
    Affine::new([[sx, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], [0.0, 0.0, -EYE])
}

/// Glide position of an eye-space point through a frustum covering Glide x0..x1, y0..y1.
pub fn eye_to_glide(f: &Frustum, x0: f64, x1: f64, y0: f64, y1: f64, p: [f64; 3]) -> Option<(f64, f64)> {
    let m = f.matrix();
    let c: Vec<f64> = (0..4).map(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3]).collect();
    if c[3] <= 0.0 {
        return None;
    }
    let (nx, ny) = (c[0] / c[3], c[1] / c[3]);
    Some((x0 + (nx + 1.0) * 0.5 * (x1 - x0), y0 + (1.0 - ny) * 0.5 * (y1 - y0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guest_like(a: i32) -> (i32, i32) {
        table_entry(a)
    }

    #[test]
    fn sine_table_matches_the_game() {
        // values read from the game's table at c48e0 (SR_PEEK, see transforms.md)
        assert_eq!(table_entry(0), (0, 32767));
        assert_eq!(table_entry(1), (100, 32767));
        assert_eq!(table_entry(3), (301, 32766));
        assert_eq!(table_entry(5), (502, 32764));
        assert_eq!(table_entry(6), (603, 32762));
        assert_eq!(table_entry(7), (703, 32760));
        assert_eq!(table_entry(128), (12539, 30273));
        assert_eq!(table_entry(129), (12632, 30235));
        assert_eq!(table_entry(256), (23170, 23170));
        assert_eq!(table_entry(257), (23241, 23099));
        assert_eq!(table_entry(512), (32767, 0));
        assert_eq!(table_entry(513), (32767, -100));
        assert_eq!(table_entry(2047), (-100, 32767));
    }

    #[test]
    fn q14_rotation_is_the_float_rotation_with_truncation() {
        for a in [[0, 0, 0], [100, 700, 1500], [2000, 33, 1024], [1536, 512, 7], [300, 1800, 1200]] {
            let e = q14_rotation(a, &guest_like);
            let r = rotation([a[0] as f64, a[1] as f64, a[2] as f64]);
            for row in 0..3 {
                for col in 0..3 {
                    let d = e[row + 3 * col] as f64 / 16384.0 - r[row][col];
                    assert!(d.abs() < 4.0 / 16384.0, "{a:?} [{row}][{col}] {d}");
                }
            }
        }
        // zero angles: cos 0 = 32767 >> 1 = 16383, and 16383 * 16383 >> 14 = 16382: the
        // engine's "identity" scales by 0.99988
        assert_eq!(q14_rotation([0, 0, 0], &guest_like), [16382, 0, 0, 0, 16382, 0, 0, 0, 16382]);
    }

    #[test]
    fn rotation_axes() {
        // heading (a1) 512 = a quarter turn: local -z (forward) -> world +x
        let r = rotation([0.0, 512.0, 0.0]);
        let fwd = [-r[0][2], -r[1][2], -r[2][2]];
        assert!((fwd[0] - 1.0).abs() < 1e-12 && fwd[1].abs() < 1e-12 && fwd[2].abs() < 1e-12);
        // pitch (a0) > 0 raises the forward direction (y is up)
        let r = rotation([100.0, 0.0, 0.0]);
        assert!(-r[1][2] > 0.0);
        // orthonormal
        let r = rotation([123.0, 1700.0, 345.0]);
        for i in 0..3 {
            for j in 0..3 {
                let d: f64 = (0..3).map(|k| r[k][i] * r[k][j]).sum();
                assert!((d - if i == j { 1.0 } else { 0.0 }).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn q14_product_composes_like_affine() {
        let a = with_translation(&q14_rotation([10, 200, 30], &guest_like), [1000, -50, 7]);
        let b = with_translation(&q14_rotation([400, 5, 1900], &guest_like), [-30, 60, 900]);
        let c = mul_q14(&a, &b);
        let fa = Affine::from_engine(&a);
        let fb = Affine::from_engine(&b);
        let fc = fa.after(&fb);
        let gc = Affine::from_engine(&c);
        for i in 0..3 {
            assert!((gc.t[i] - fc.t[i]).abs() <= 1.0);
            for j in 0..3 {
                assert!((gc.r[i][j] - fc.r[i][j]).abs() < 3.0 / 16384.0);
            }
        }
    }

    #[test]
    fn projection_replica() {
        // on the axis: the centre column; 100 units left of the axis at z = -128: 2 * 100 / 2
        assert_eq!(unpack(engine_project([0, 0, -128], 0x13f << 16, 260, false)), (319, 260));
        assert_eq!(unpack(engine_project([100, 0, -128], 0x13f << 16, 260, false)), (219, 260));
        assert_eq!(unpack(engine_project([0, 100, -128], 0x13f << 16, 260, false)), (319, 160));
        assert_eq!(unpack(engine_project([0, 100, -128], 0x13f << 16, 260, true)), (319, 210));
        assert_eq!(engine_project([0, 0, 1], 0x13f << 16, 260, false), BEHIND);
        assert_ne!(engine_project([0, 0, 0], 0x13f << 16, 260, false), BEHIND);
        // the float projection agrees within the engine's rounding (down, plus r's truncation)
        let mut worst = 0f64;
        for i in 0..2000 {
            let v = [(i * 37 % 4001) - 2000, (i * 53 % 3001) - 1500, -(i * 97 % 40000) - 1];
            let p = engine_project(v, 0x13f << 16, 260, false);
            if p == BEHIND {
                continue;
            }
            let (px, py) = unpack(p);
            let (fx, fy) = project([v[0] as f64, v[1] as f64, v[2] as f64], 319.0, 260.0, false).unwrap();
            worst = worst.max((fx - px as f64 - 0.5).abs()).max((fy - py as f64 - 0.5).abs());
        }
        assert!(worst < 0.6, "{worst}");
    }

    #[test]
    fn view_replica_rounds_to_nearest_even() {
        let m = with_translation(&[16384, 0, 0, 0, 16384, 0, 0, 0, 16384], [0, 0, 0]);
        assert_eq!(engine_view(&m, [3, -5, 7], false), [3, -5, 7]);
        assert_eq!(engine_view(&m, [3, -5, 7], true), [-3, -5, 7]);
        // 0.5 steps: 8192 / 16384 = 0.5
        let h = with_translation(&[8192, 0, 0, 0, 8192, 0, 0, 0, 8192], [0, 0, 0]);
        assert_eq!(engine_view(&h, [1, 3, -1], false), [0, 2, 0]);
    }

    #[test]
    fn frustum_reproduces_the_glide_projection() {
        let (cy, far) = (260.0, 50000.0);
        for &(mirror, e) in &[(false, 0.0), (false, 107.0)] {
            let (x0, x1) = (-e, 640.0 + e);
            let f = Frustum::glide(x0, x1, 0.0, 480.0, cy, far, mirror);
            let to_eye = game_view_to_eye(mirror);
            for v in [[0.0, 0.0, -10.0], [500.0, -200.0, -3000.0], [-1234.0, 777.0, -20000.0]] {
                let (gx, gy) = project(v, CENTRE_X, cy, mirror).unwrap();
                let (gx, gy) = (gx, gy * Y_SCALE);
                let (hx, hy) = eye_to_glide(&f, x0, x1, 0.0, 480.0, to_eye.apply(v)).unwrap();
                assert!((gx - hx).abs() < 1e-9 && (gy - hy).abs() < 1e-9, "{v:?}: {gx},{gy} vs {hx},{hy}");
            }
            // depth: near plane (view z = 0) -> 0, far plane (view z = -far) -> 1
            let m = f.matrix();
            let depth = |z: f64| {
                let p = to_eye.apply([0.0, 0.0, z]);
                (m[2][2] * p[2] + m[2][3]) / (m[3][2] * p[2])
            };
            assert!(depth(0.0).abs() < 1e-12 && (depth(-far) - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn billboards_face_the_camera() {
        assert_eq!(billboard_angles(10, 20, [10, 20, 0]), [0, 0x400, 0x7ff]);
        assert_eq!(billboard_angles(0, 0, [100, 1000, 5]), [1948, 0x7ff & -(0x400 - 1000), 0x7fa]);
    }
}

// ----------------------------------------------------------------------------------------
// The check against the engine (diagnostics)

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

/// After the part transform (`call 7aa00` at 78e41; 78e4e is the sub-pixel observer):
/// `mov dword ptr [82ea8], 17700000h` (10 bytes).
const AFTER_TRANSFORM: u32 = 0x78e58;
/// The draw loop's current object (7bab4): +0 model, +8 part counter, +0Ch the part's
/// draw record, +18h vertex list, +30h angles, +3Ch position relative to the camera,
/// +48h object matrix, +D4h animation offset, +D8h instance (bit 30: scene table).
const OBJ: u32 = 0x7bab4;
/// Draw records of the object's parts (500 bytes each); +118h the part's camera matrix.
const RECORDS: u32 = 0x7bb9c;
const RECORD_SIZE: u32 = 500;
const CAMERA_PTR: u32 = 0x81e0c;
const MODELS_PTR: u32 = 0x147390;
const ANIM_CLOCK: u32 = 0x1473a0;
const SINE: u32 = 0xc48e0;
const VIEW_POS: u32 = 0x68fc8;
const PACKED_XY: u32 = 0x6ed94;
const PART_VERTICES: u32 = 0x7bacc;
const VARIANT: u32 = 0xaa5b0;
const CENTER_IMM: [u32; 2] = [0x7ab1a, 0x7ac1e];
const CENTER_Y: u32 = 0x81e3c;
const X_TABLE_PTR: u32 = 0xaa6c4;
const X_OFFSET: u32 = 0xaa6b8;
const Y_OFFSET: u32 = 0xaa6bc;
const Y_TABLE: u32 = 0xac6cc;
/// Far plane: [cf470] = -distance (set_far 82fa4).
const FAR_NEG: u32 = 0xcf470;
/// The view's width and height in grid pixels (set_view: [81e50] = W, [81e24] = H).
const VIEW_W: u32 = 0x81e50;
const VIEW_H: u32 = 0x81e24;
const SCENE: u32 = 0x12f860;

#[derive(Default, Clone)]
struct Stats {
    parts: u64,
    verts: u64,
    /// in front of the near plane (engine)
    visible: u64,
    /// visible and inside the view rectangle (grid x 0..W, y 0..H)
    onscreen: u64,
    /// engine replica differs from the engine (rec+118h elements, view ints, packed)
    matrix_diff: u64,
    view_diff: u64,
    packed_diff: u64,
    /// float model vs the engine's view ints: max and sum of squares (units), max relative
    /// to the eye distance (visible vertices)
    max_view: f64,
    sum_view2: f64,
    max_view_rel: f64,
    /// on screen, in grid pixels: float model minus (engine packed + 0.5) (all errors), its
    /// sum of squares, within the engine's cell, float model minus the exact projection of
    /// the engine's view ints (the transform alone), exact projection of the engine's view
    /// ints minus (packed + 0.5) (the engine's own projection rounding)
    max_total: f64,
    sum_total2: f64,
    in_cell: u64,
    max_transform: f64,
    max_engine_round: f64,
    /// visible but off screen (clipped away by the engine): max of all errors
    max_total_off: f64,
    /// float model behind / in front where the engine says otherwise, and the largest |z|
    /// of such a vertex
    behind_diff: u64,
    behind_diff_z: f64,
    /// in front of the near plane, marked behind by the engine (overflow of 2 x r + centre)
    overflow_verts: u64,
    /// on screen, Glide pixels: float through the builder's tables minus the engine's
    /// (minus half a cell)
    max_glide_x: f64,
    max_glide_y: f64,
    anim_parts: u64,
    billboard_parts: u64,
    overflow_parts: u64,
    /// per eye-distance bin (`BINS`): max view error of the float model, of the engine's
    /// quantised rotations alone (their exact composition), of the products' truncations
    /// alone (the unrounded transform against that composition), and the max on-screen
    /// grid error; vertex counts
    bins: [[f64; 4]; 4],
    bin_n: [u64; 4],
}

/// Eye-distance bins (128 - z) of the error budget.
const BINS: [f64; 3] = [1000.0, 4000.0, 16000.0];

impl Stats {
    fn merge(&mut self, o: &Stats) {
        self.parts += o.parts;
        self.verts += o.verts;
        self.visible += o.visible;
        self.onscreen += o.onscreen;
        self.matrix_diff += o.matrix_diff;
        self.view_diff += o.view_diff;
        self.packed_diff += o.packed_diff;
        self.max_view = self.max_view.max(o.max_view);
        self.sum_view2 += o.sum_view2;
        self.max_view_rel = self.max_view_rel.max(o.max_view_rel);
        self.max_total = self.max_total.max(o.max_total);
        self.sum_total2 += o.sum_total2;
        self.in_cell += o.in_cell;
        self.max_transform = self.max_transform.max(o.max_transform);
        self.max_engine_round = self.max_engine_round.max(o.max_engine_round);
        self.max_total_off = self.max_total_off.max(o.max_total_off);
        self.behind_diff += o.behind_diff;
        self.behind_diff_z = self.behind_diff_z.max(o.behind_diff_z);
        self.overflow_verts += o.overflow_verts;
        self.max_glide_x = self.max_glide_x.max(o.max_glide_x);
        self.max_glide_y = self.max_glide_y.max(o.max_glide_y);
        self.anim_parts += o.anim_parts;
        self.billboard_parts += o.billboard_parts;
        self.overflow_parts += o.overflow_parts;
        for b in 0..4 {
            self.bin_n[b] += o.bin_n[b];
            for k in 0..4 {
                self.bins[b][k] = self.bins[b][k].max(o.bins[b][k]);
            }
        }
    }

    fn line(&self, label: &str) -> String {
        let rms = |s: f64, n: u64| if n > 0 { (s / n as f64).sqrt() } else { 0.0 };
        format!(
            "{label:20} parts {:5} verts {:6} visible {:6} on screen {:6} | replica diffs: matrix {} view {} packed {} | float view: max {:.2} rms {:.2} units, rel {:.2e} | on screen (grid px): all max {:.3} rms {:.3}, in cell {:.2}%, transform max {:.3}, engine rounding max {:.3}; glide max x {:.3} y {:.3} | off screen max {:.2} | behind diffs {} (|z| <= {:.2}), projection overflows {} | anim {} billboard {} overflow {}",
            self.parts,
            self.verts,
            self.visible,
            self.onscreen,
            self.matrix_diff,
            self.view_diff,
            self.packed_diff,
            self.max_view,
            rms(self.sum_view2, self.verts * 3),
            self.max_view_rel,
            self.max_total,
            rms(self.sum_total2, self.onscreen * 2),
            if self.onscreen > 0 { self.in_cell as f64 * 100.0 / self.onscreen as f64 } else { 0.0 },
            self.max_transform,
            self.max_engine_round,
            self.max_glide_x,
            self.max_glide_y,
            self.max_total_off,
            self.behind_diff,
            self.behind_diff_z,
            self.overflow_verts,
            self.anim_parts,
            self.billboard_parts,
            self.overflow_parts,
        ) + &format!(
            " | by eye distance <1000 / <4000 / <16000 / more: vertices {:?}, view max {}, rotations {}, truncations {}, grid {}",
            self.bin_n,
            (0..4).map(|b| format!("{:.2}", self.bins[b][0])).collect::<Vec<_>>().join("/"),
            (0..4).map(|b| format!("{:.2}", self.bins[b][1])).collect::<Vec<_>>().join("/"),
            (0..4).map(|b| format!("{:.2}", self.bins[b][2])).collect::<Vec<_>>().join("/"),
            (0..4).map(|b| format!("{:.2}", self.bins[b][3])).collect::<Vec<_>>().join("/"),
        )
    }
}
#[derive(Default)]
struct Check {
    first: u64,
    last: u64,
    csv: Option<std::io::BufWriter<std::fs::File>>,
    frame: u64,
    /// per kind label
    by_kind: std::collections::BTreeMap<String, Stats>,
    total: std::collections::BTreeMap<String, Stats>,
    done: bool,
    table_checked: bool,
    logged_view: [u64; 2],
    logged_parts: std::collections::HashSet<(u32, u32)>,
}

static CHECK: std::sync::Mutex<Option<Check>> = std::sync::Mutex::new(None);

pub fn install(m: &mut Machine) -> Result<(), String> {
    let Ok(spec) = std::env::var("SR_TRANSFORMS_CHECK") else { return Ok(()) };
    let mut it = spec.split(',');
    let first: u64 = it.next().and_then(|v| v.trim().parse().ok()).ok_or("SR_TRANSFORMS_CHECK=FIRST[,LAST[,CSV]]")?;
    let last: u64 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(first);
    let csv = it.next().map(str::trim).filter(|p| !p.is_empty()).map(|p| std::fs::File::create(p).map(std::io::BufWriter::new)).transpose().map_err(|e| e.to_string())?;
    let mut c = Check { first, last, csv, ..Default::default() };
    if let Some(w) = c.csv.as_mut() {
        use std::io::Write;
        let _ = writeln!(
            w,
            "frame,view,kind,scene,model,part,part_name,vertex,mx,my,mz,ex,ey,ez,rx,ry,rz,fx,fy,fz,epx,epy,rpx,rpy,fgridx,fgridy,glide_x,glide_y,fglide_x,fglide_y"
        );
    }
    *CHECK.lock().unwrap() = Some(c);
    m.install_observer("transforms check", AFTER_TRANSFORM, after_transform)?;
    crate::info!("transforms check: frames {first}..={last}");
    Ok(())
}

fn rd(a: u32) -> u32 {
    unsafe { mem::rd32(a) }
}
fn rdi(a: u32) -> i32 {
    rd(a) as i32
}

/// Name of the scene-table entry of an instance (empty if none).
fn scene_name(m: &Machine, inst: u32) -> String {
    let base = m.img.link(SCENE);
    for i in 0..1024 {
        let e = base + i * 28;
        let (ptr, index) = (rd(e + 0xc), rd(e + 0x18));
        if index == u32::MAX {
            break;
        }
        if ptr == inst {
            let n = unsafe { mem::slice(e, 8) };
            return n.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        }
    }
    String::new()
}

fn part_name(part: u32) -> String {
    if !mem::readable(part + 0x68, 8) {
        return String::new();
    }
    let n = unsafe { mem::slice(part + 0x68, 8) };
    n.iter().take_while(|&&b| b != 0).map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
}

fn kind_label(scene: &str) -> &'static str {
    let n = |p: &str| scene.starts_with(p);
    if scene.is_empty() {
        "world"
    } else if n("auto") {
        "car"
    } else if n("fauto") || n("rauto") || n("MV_mauto") {
        "wheel object"
    } else if n("ashadow") {
        "shadow"
    } else if n("asmoke") || n("a2smoke") {
        "smoke"
    } else if n("aztri") || n("az1tri") {
        "marks"
    } else {
        "other scene"
    }
}

fn after_transform(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let frame = crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed);
    let mut guard = CHECK.lock().unwrap();
    if let Some(c) = guard.as_mut() {
        if !c.done {
            if frame > c.last {
                finish(c);
            } else if frame >= c.first {
                if frame != c.frame {
                    frame_summary(c);
                    c.frame = frame;
                }
                check_part(m, c, frame);
            }
        }
    }
    HookResult::Resume(tramp)
}

fn frame_summary(c: &mut Check) {
    if c.by_kind.is_empty() {
        return;
    }
    crate::info!("transforms check, frame {}:", c.frame);
    let kinds = std::mem::take(&mut c.by_kind);
    for (k, s) in &kinds {
        crate::info!("  {}", s.line(k));
        c.total.entry(k.clone()).or_default().merge(s);
    }
}

fn finish(c: &mut Check) {
    frame_summary(c);
    c.done = true;
    crate::info!("transforms check, frames {}..={} in all:", c.first, c.last);
    let mut all = Stats::default();
    for (k, s) in &c.total {
        crate::info!("  {}", s.line(k));
        all.merge(s);
    }
    crate::info!("  {}", all.line("all"));
    if let Some(w) = c.csv.as_mut() {
        use std::io::Write;
        let _ = w.flush();
    }
}

/// Recompute the part the engine just transformed and compare.
fn check_part(m: &Machine, c: &mut Check, frame: u64) {
    let l = |a: u32| m.img.link(a);
    let obj = l(OBJ);
    let mirror = rd(l(VARIANT)) & 1 != 0;
    // the game's sine table against the generator (once)
    let sine = l(SINE);
    if !c.table_checked {
        c.table_checked = true;
        let bad: Vec<i32> = (0..2048).filter(|&a| {
            let (s, k) = table_entry(a);
            (rd(sine + 8 * a as u32) as i16 as i32, rd(sine + 8 * a as u32 + 4) as i16 as i32) != (s, k)
        }).collect();
        let show: Vec<String> = bad
            .iter()
            .take(12)
            .map(|&a| format!("{a}: game ({}, {}) generated {:?} (32768 sin {:.4}, cos {:.4})", rd(sine + 8 * a as u32) as i16, rd(sine + 8 * a as u32 + 4) as i16, table_entry(a), 32768.0 * (a as f64 * ANGLE_UNIT).sin(), 32768.0 * (a as f64 * ANGLE_UNIT).cos()))
            .collect();
        let worst = (0..2048u32)
            .map(|a| {
                let (s, k) = table_entry(a as i32);
                ((rd(sine + 8 * a) as i16 as i32 - s).abs()).max((rd(sine + 8 * a + 4) as i16 as i32 - k).abs())
            })
            .max()
            .unwrap_or(0);
        // where they differ: the game's value minus the exact 32768 sin (cos), over all entries
        let gaps: Vec<f64> = (0..2048u32)
            .flat_map(|a| {
                let x = a as f64 * ANGLE_UNIT;
                let (s, k) = table_entry(a as i32);
                let g = (rd(sine + 8 * a) as i16 as i32, rd(sine + 8 * a + 4) as i16 as i32);
                let mut v = Vec::new();
                if g.0 != s {
                    v.push(g.0 as f64 - 32768.0 * x.sin());
                }
                if g.1 != k {
                    v.push(g.1 as f64 - 32768.0 * x.cos());
                }
                v
            })
            .collect();
        let (gmin, gmax) = gaps.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &g| (lo.min(g), hi.max(g)));
        crate::info!(
            "transforms check: sine table: {} of 2048 entries differ from the generator (by at most {worst}; {} values, game minus exact 32768 sin/cos from {gmin:.4} to {gmax:.4}): {}",
            bad.len(),
            gaps.len(),
            show.join("; ")
        );
        for a in [1024, 1536] {
            crate::info!("  table[{a}] = ({}, {})", rd(sine + 8 * a) as i16, rd(sine + 8 * a + 4) as i16);
        }
        // candidate generators: other values of pi, single precision
        let game = |a: u32| (rd(sine + 8 * a) as i16 as i32, rd(sine + 8 * a + 4) as i16 as i32);
        let q = |v: f64| ((v * 32768.0) as i64).clamp(-32767, 32767) as i32;
        for pi in [std::f64::consts::PI, 3.14159, 3.141592, 3.141593, 3.1416, std::f32::consts::PI as f64] {
            let n = (0..2048u32).filter(|&a| {
                let x = a as f64 * 2.0 * pi / 2048.0;
                game(a) != (q(x.sin()), q(x.cos()))
            }).count();
            let n32 = (0..2048u32).filter(|&a| {
                let x = a as f32 * (2.0 * pi as f32 / 2048.0);
                game(a) != (q(x.sin() as f64), q(x.cos() as f64))
            }).count();
            crate::info!("  generator trunc(32768 sin(a * 2 pi / 2048)) with pi = {pi}: {n} entries differ (in single precision: {n32})");
        }
    }
    let sc = |a: i32| -> (i32, i32) {
        let p = sine.wrapping_add(8 * a as u32);
        (rd(p) as i16 as i32, rd(p + 4) as i16 as i32)
    };
    // camera
    let cam = rd(l(CAMERA_PTR));
    if cam < 0x10000 || !mem::readable(cam, 0x78) {
        return;
    }
    let cam_angles = [rdi(cam), rdi(cam + 4), rdi(cam + 8)];
    let cam_pos = [rdi(cam + 0xc), rdi(cam + 0x10), rdi(cam + 0x14)];
    let cam_r = q14_rotation(cam_angles, &sc);
    let cam_rt = transpose9(&cam_r);
    let stored_rt: Vec<i32> = (0..9).map(|k| rdi(cam + 0x48 + 4 * k)).collect();
    // object
    let model = rd(obj);
    let inst_raw = rd(obj + 0xd8);
    let scene = inst_raw & 0x4000_0000 != 0;
    let inst = inst_raw & !0x4000_0000;
    if !mem::readable(inst, 0x40) || !mem::readable(model, 0x20) {
        return;
    }
    let models = rd(l(MODELS_PTR));
    let model_calc = rd(models.wrapping_add(4 * rd(inst)));
    let flags = rd(inst + 4);
    let billboard = flags & 0x8000 != 0;
    let obj_angles = if billboard { billboard_angles(rdi(inst + 0x14), rdi(inst + 0x18), cam_angles) } else { [rdi(inst + 0x14), rdi(inst + 0x18), rdi(inst + 0x1c)] };
    let obj_pos = [rdi(inst + 8), rdi(inst + 0xc), rdi(inst + 0x10)];
    let rel = [obj_pos[0].wrapping_sub(cam_pos[0]), obj_pos[1].wrapping_sub(cam_pos[1]), obj_pos[2].wrapping_sub(cam_pos[2])];
    // part
    let rec = rd(obj + 0xc);
    if rec < l(RECORDS) || (rec - l(RECORDS)) % RECORD_SIZE != 0 {
        return;
    }
    let p = (rec - l(RECORDS)) / RECORD_SIZE;
    let part = rd(model + 0x18 + 4 * p).wrapping_add(model);
    let place = rd(part + 0xc).wrapping_add(model);
    let anim_count = rdi(model + 0x10);
    let anim = if anim_count >= 0 { ((anim_count.wrapping_mul(rdi(l(ANIM_CLOCK)))) >> 16) * 0x30 } else { anim_count };
    let anim_stored = rdi(obj + 0xd4);
    let (part_m, part_f, how) = if anim >= 0 {
        let a = place + 0x1c + anim as u32;
        let mut e = [0i32; 12];
        for (k, v) in e.iter_mut().enumerate() {
            *v = unsafe { mem::rd16(a + 2 * k as u32) } as i16 as i32;
        }
        (e, Affine::from_engine(&e), "anim")
    } else {
        let (ang, pos) = if scene {
            let b = inst + 0x40 + 0x20 * p;
            ([rdi(b + 8), rdi(b + 0xc), rdi(b + 0x10)], [rdi(b + 0x14), rdi(b + 0x18), rdi(b + 0x1c)])
        } else {
            ([rdi(place + 4), rdi(place + 8), rdi(place + 0xc)], [rdi(place + 0x10), rdi(place + 0x14), rdi(place + 0x18)])
        };
        let f = Affine::new(rotation(ang.map(|a| a as f64)), pos.map(|v| v as f64));
        (with_translation(&q14_rotation(ang, &sc), pos), f, if scene { "instance" } else { "model" })
    };
    // engine replica of rec+118h
    let obj_m = with_translation(&q14_rotation(obj_angles, &sc), rel);
    let replica = engine_part_matrix(&cam_rt, &obj_m, &part_m);
    let engine_m: Vec<i32> = (0..12).map(|k| rdi(rec + 0x118 + 4 * k)).collect();
    // float model: model -> world -> view
    let to_world = part_to_world(obj_pos.map(|v| v as f64), obj_angles.map(|a| a as f64), &part_f);
    let to_view = world_to_view(cam_pos.map(|v| v as f64), cam_angles.map(|a| a as f64)).after(&to_world);
    // the engine's quantised matrices composed exactly (no truncated products)
    let q_view = Affine::from_engine(&with_translation(&cam_rt, [0; 3])).after(&Affine::from_engine(&obj_m)).after(&Affine::from_engine(&part_m));
    // the engine's 32-bit sums of products wrapped (translations beyond ~131072 units)
    let overflow = q14_overflows(&obj_m, &part_m) || q14_overflows(&with_translation(&cam_rt, [0; 3]), &mul_q14(&obj_m, &part_m));
    // view constants
    let centre_hi = rdi(l(CENTER_IMM[mirror as usize]));
    let centre = centre_hi as f64 / 65536.0;
    let cy = rdi(l(CENTER_Y));
    let xt = rd(l(X_TABLE_PTR));
    let (xo, yo) = (rdi(l(X_OFFSET)), rdi(l(Y_OFFSET)));
    let yt = l(Y_TABLE);
    let (width, height) = (rdi(l(VIEW_W)), rdi(l(VIEW_H)));
    let tab = |base: u32, i: i32| f32::from_bits(rd(base.wrapping_add(4 * i as u32))) as f64;
    let (x_slope, y_slope) = ((tab(xt, 12) - tab(xt, 10)) / 2.0, (tab(yt, 12) - tab(yt, 10)) / 2.0);
    let (x_icpt, y_icpt) = (tab(xt, 10) - 10.0 * x_slope, tab(yt, 10) - 10.0 * y_slope);
    if c.logged_view[mirror as usize] != frame {
        c.logged_view[mirror as usize] = frame;
        // the views' camera objects (1296bc + 4 v, v = 0 main, 2 mirror): mode at +AE4h,
        // camera structure at +A64h
        let views: Vec<String> = [0u32, 2]
            .iter()
            .map(|&v| {
                let o = rd(l(0x1296bc + 4 * v));
                if mem::readable(o, 0xae8) { format!("view {v}: object {o:#x} (structure {:#x}) mode {}", o + 0xa64, rd(o + 0xae4)) } else { format!("view {v}: -") }
            })
            .collect();
        crate::info!(
            "transforms check frame {frame} {} ({}): camera {cam:#x} angles {cam_angles:?} pos {cam_pos:?}, stored transpose {} the angles' ({:?}); view {width} x {height}, centre {centre} cy {cy}, x table {:#x} (glide x = {x_icpt:.3} + {x_slope:.4} i, i = {xo} + grid x), y table glide y = {y_icpt:.3} + {y_slope:.4} i (i = {yo} + grid y); far {}",
            if mirror { "mirror" } else { "main" },
            views.join(", "),
            if stored_rt == cam_rt.to_vec() { "matches" } else { "differs from" },
            stored_rt,
            m.img.to_link(xt).unwrap_or(xt),
            -rdi(l(FAR_NEG)),
        );
    }
    let name = if scene { scene_name(m, inst) } else { String::new() };
    let label = kind_label(&name);
    let pname = part_name(part);
    if c.logged_parts.insert((inst, p)) && c.logged_parts.len() < 400 {
        crate::debug!(
            "transforms check: {label} {name} model {model:#x} part {p} '{pname}' ({how}) obj angles {obj_angles:?} pos {obj_pos:?} flags {flags:#x}, part matrix {part_m:?}"
        );
    }
    let mut s = Stats { parts: 1, anim_parts: (how == "anim") as u64, billboard_parts: billboard as u64, overflow_parts: overflow as u64, ..Default::default() };
    s.matrix_diff = (0..12).filter(|&k| engine_m[k] != replica[k]).count() as u64 + (model_calc != model) as u64 + (anim != anim_stored) as u64;
    if s.matrix_diff > 0 && s.matrix_diff < 1000 {
        crate::debug!("transforms check: replica differs for {label} {name} part {p} '{pname}': engine {engine_m:?} replica {replica:?} (model {model:#x}/{model_calc:#x}, anim {anim_stored}/{anim})");
    }
    // vertices
    let list = rd(l(PART_VERTICES));
    let (vp, pk) = (l(VIEW_POS), l(PACKED_XY));
    let mut n = 0u32;
    while n < 8192 && unsafe { mem::rd16(list + 6 * n) } != 0x7fff {
        let mv = list + 6 * n;
        let v = unsafe { [mem::rd16(mv) as i16, mem::rd16(mv + 2) as i16, mem::rd16(mv + 4) as i16] };
        let e = [rdi(vp + 12 * n), rdi(vp + 12 * n + 4), rdi(vp + 12 * n + 8)];
        let packed = rd(pk + 6 * n);
        let r = engine_view(&replica, v, mirror);
        let rp = engine_project(r, centre_hi, cy, mirror);
        s.verts += 1;
        s.view_diff += (r != e) as u64;
        s.packed_diff += (rp != packed) as u64;
        let fv = to_view.apply(v.map(|x| x as f64));
        let fv = if mirror { [-fv[0], fv[1], fv[2]] } else { fv };
        let d = (0..3).map(|k| (fv[k] - e[k] as f64).abs()).fold(0.0, f64::max);
        s.max_view = s.max_view.max(d);
        // error budget by eye distance
        let qv = q_view.apply(v.map(|x| x as f64));
        let qv = if mirror { [-qv[0], qv[1], qv[2]] } else { qv };
        let uv = engine_view_unrounded(&replica, v, mirror);
        let dist = EYE - e[2] as f64;
        let bin = BINS.iter().position(|&b| dist < b).unwrap_or(3);
        let dmax = |a: [f64; 3], b: [f64; 3]| (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f64::max);
        if dist >= EYE {
            s.bin_n[bin] += 1;
            s.bins[bin][0] = s.bins[bin][0].max(d);
            s.bins[bin][1] = s.bins[bin][1].max(dmax(qv, fv));
            s.bins[bin][2] = s.bins[bin][2].max(dmax(uv, qv));
        }
        s.sum_view2 += (0..3).map(|k| (fv[k] - e[k] as f64).powi(2)).sum::<f64>();
        let fp = project(fv, centre, cy as f64, mirror);
        let mut row = None;
        match (fp, packed == BEHIND) {
            (Some((gx, gy)), false) => {
                s.visible += 1;
                s.max_view_rel = s.max_view_rel.max(d / (EYE - e[2] as f64));
                let (px, py) = unpack(packed);
                let (dx, dy) = (gx - px as f64 - 0.5, gy - py as f64 - 0.5);
                if px >= 0 && px < width && py >= 0 && py <= height {
                    s.onscreen += 1;
                    s.max_total = s.max_total.max(dx.abs()).max(dy.abs());
                    s.sum_total2 += dx * dx + dy * dy;
                    s.in_cell += (dx.abs() <= 0.5 && dy.abs() <= 0.5) as u64;
                    s.bins[bin][3] = s.bins[bin][3].max(dx.abs()).max(dy.abs());
                    // the same projection of the engine's own (rounded) view position
                    if let Some((ex, ey)) = project(e.map(|v| v as f64), centre, cy as f64, mirror) {
                        s.max_transform = s.max_transform.max((gx - ex).abs()).max((gy - ey).abs());
                        s.max_engine_round = s.max_engine_round.max((ex - px as f64 - 0.5).abs()).max((ey - py as f64 - 0.5).abs());
                    }
                    // Glide: the builder's tables at the engine's position vs the float
                    // position through the tables' line (minus half a cell: rounding down)
                    let (ix, iy) = (xo + px, yo + py);
                    if (0..2048).contains(&ix) && (0..2048).contains(&iy) {
                        let (bx, by) = (tab(xt, ix), tab(yt, iy));
                        let (fgx, fgy) = (x_icpt + x_slope * (xo as f64 + gx), y_icpt + y_slope * (yo as f64 + gy));
                        // the table's last column (639.8 + e) is not on the line
                        if px + 1 < width {
                            s.max_glide_x = s.max_glide_x.max((fgx - bx - 0.5 * x_slope).abs());
                        }
                        s.max_glide_y = s.max_glide_y.max((fgy - by - 0.5 * y_slope).abs());
                        row = Some((px, py, gx, gy, bx, by, fgx, fgy));
                    }
                } else {
                    s.max_total_off = s.max_total_off.max(dx.abs()).max(dy.abs());
                }
            }
            (None, true) => {}
            // in front, but 2 x r + centre overflowed: the engine marks it behind (far off
            // screen to the side, next to the near plane)
            (Some(_), true) if e[2] <= 0 => s.overflow_verts += 1,
            _ => {
                s.behind_diff += 1;
                s.behind_diff_z = s.behind_diff_z.max(fv[2].abs()).max(e[2].abs() as f64);
            }
        }
        if let Some(w) = c.csv.as_mut().filter(|_| (frame - c.first) % 100 == 0) {
            use std::io::Write;
            let (rpx, rpy) = unpack(rp);
            let (px, py, gx, gy, bx, by, fgx, fgy) = row.unwrap_or((i32::MIN, i32::MIN, f64::NAN, f64::NAN, f64::NAN, f64::NAN, f64::NAN, f64::NAN));
            let _ = writeln!(
                w,
                "{frame},{},{label},{name},{model:#x},{p},{pname},{n},{},{},{},{},{},{},{},{},{},{:.4},{:.4},{:.4},{px},{py},{rpx},{rpy},{gx:.4},{gy:.4},{bx:.3},{by:.3},{fgx:.4},{fgy:.4}",
                if mirror { "mirror" } else { "main" },
                v[0],
                v[1],
                v[2],
                e[0],
                e[1],
                e[2],
                r[0],
                r[1],
                r[2],
                fv[0],
                fv[1],
                fv[2],
            );
        }
        n += 1;
    }
    let key = format!("{}{}", if mirror { "mirror " } else { "" }, if label == "car" { if p == 0 { "car part 0" } else { "car parts 1+" } } else { label });
    c.by_kind.entry(key).or_default().merge(&s);
}
