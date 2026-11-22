//! Sub-pixel vertex positions for the 3D engine (STRT3FX).
//!
//! The engine projects every vertex to whole pixels of its 640 x 400 view: the transform
//! (`7aa00`, docs/re/strt3fx.md "3D view") computes `x * 256 / (z - 128)` with 16
//! fractional bits but packs the result as `x << 16 | y` integers, and the Glide vertex
//! builder maps those integers through its x / y tables. At the original resolution that is
//! invisible; at 4K one game pixel is six screen pixels: polygons wobble as they move and,
//! where a vertex lies on a neighbour's edge (T-junctions in the track), rounding opens
//! thin cracks through which the scenery behind shows.
//!
//! The transform also rounds the view-space position itself: it computes it in floating
//! point from the model vertex (int16) and the part's 3 x 4 matrix (`7a9c4`..`7a9f0`) and
//! stores it with `fistp` (integers, `68fc8 + 12 n`), so near the camera, where one unit is
//! most of a game pixel, every vertex jitters by up to half a unit on its own as the view
//! moves and the textures on near polygons wobble. After the transform of each object part
//! (observer at `78e4e`) this module recomputes the view position without the rounding and
//! the screen position with the same projection (centre and y scale as patched in the
//! code, so widescreen and the mirror view follow), and maps it through the builder's
//! tables. The Glide layer then replaces each
//! builder vertex that still sits exactly on a vertex's rounded position with the exact
//! one, and scales its 1/w (and s/w, t/w with it) from the rounded z to the exact one. Vertices made by the engine's 2D clipper (on the screen edges) keep theirs.
//! Exact positions are shifted by half a pixel so that, on average, the picture stays where
//! the original's rounded-down (floor) positions put it.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;
use std::sync::atomic::{AtomicBool, Ordering};

/// After the part transform (`call 7aa00` at 78e41 returns; 10-byte `mov [7050c], imm`).
const AFTER_TRANSFORM: u32 = 0x78e4e;
/// The part's model vertex list (int16 x, y, z; ends with x = 7FFFh).
const PART_VERTICES: u32 = 0x7bacc;
/// View-space positions written by the transform (3 x i32 per vertex).
const VIEW_POS: u32 = 0x68fc8;
/// The part's model-to-view matrix used by the transform (12 floats, column by column:
/// x' = m0 x + m3 y + m6 z + m9, y' = m1 x + m4 y + m7 z + m10, z' = m2 x + m5 y + m8 z + m11).
const MATRIX: u32 = 0x7a9c4;
/// Packed screen positions (x << 16 | y; 10001000h = behind the camera), 6 bytes apart.
const PACKED_XY: u32 = 0x6ed94;
/// Bit 0: the transform's second variant (rear-view mirror: x negated, y at half scale).
const VARIANT: u32 = 0xaa5b0;
/// `add edx, imm32` of the two variants: the x centre << 16 (patched for widescreen).
const CENTER_IMM: [u32; 2] = [0x7ab1a, 0x7ac1e];
/// y centre of the view.
const CENTER_Y: u32 = 0x81e3c;
/// The builder's tables: x table pointer, x and y index offsets, y table.
const X_TABLE_PTR: u32 = 0xaa6c4;
const X_OFFSET: u32 = 0xaa6b8;
const Y_OFFSET: u32 = 0xaa6bc;
const Y_TABLE: u32 = 0xac6cc;
const TABLE_LEN: i64 = 2048;

/// Precise geometry on (settings: graphics.precise_geometry).
pub static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

static ADDR: std::sync::OnceLock<Addr> = std::sync::OnceLock::new();

/// Run-time addresses of the engine data used here (resolved once).
#[derive(Clone, Copy, Default, Debug)]
struct Addr {
    part_vertices: u32,
    view_pos: u32,
    matrix: u32,
    packed_xy: u32,
    variant: u32,
    center_imm: [u32; 2],
    center_y: u32,
    x_table_ptr: u32,
    x_offset: u32,
    y_offset: u32,
    y_table: u32,
    /// The image's address range (table entries are checked against it).
    image: (u32, u32),
}

/// Rounded Glide position -> exact Glide position and 1/w ratio (exact / rounded z) of the
/// vertices of the part being drawn
/// (open addressing; entries of older parts are told apart by a generation number, so
/// nothing is cleared between parts).
#[derive(Default)]
pub struct SubPixelMap {
    keys: Vec<u64>,
    vals: Vec<(f32, f32, f32)>,
    gens: Vec<u32>,
    generation: u32,
    used: usize,
}

impl SubPixelMap {
    fn key(x: f32, y: f32) -> u64 {
        (x.to_bits() as u64) << 32 | y.to_bits() as u64
    }

    fn slot(&self, k: u64) -> usize {
        (k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40) as usize & (self.keys.len() - 1)
    }

    fn reset(&mut self, n: usize) {
        let cap = (2 * n).next_power_of_two().max(256);
        if self.keys.len() < cap {
            self.keys = vec![0; cap];
            self.vals = vec![(0.0, 0.0, 1.0); cap];
            self.gens = vec![0; cap];
        }
        self.generation = self.generation.wrapping_add(1).max(1);
        self.used = 0;
    }

    fn insert(&mut self, x: f32, y: f32, exact: (f32, f32, f32)) {
        let k = Self::key(x, y);
        let mask = self.keys.len() - 1;
        let mut i = self.slot(k);
        loop {
            if self.gens[i] != self.generation {
                self.gens[i] = self.generation;
                self.keys[i] = k;
                self.vals[i] = exact;
                self.used += 1;
                return;
            }
            if self.keys[i] == k {
                return; // two vertices on one pixel: keep the first
            }
            i = (i + 1) & mask;
        }
    }

    /// The exact position of a vertex at rounded position (x, y), if it is one, and the
    /// factor for its 1/w.
    #[inline]
    pub fn get(&self, x: f32, y: f32) -> Option<(f32, f32, f32)> {
        if self.used == 0 {
            return None;
        }
        let k = Self::key(x, y);
        let mask = self.keys.len() - 1;
        let mut i = self.slot(k);
        loop {
            if self.gens[i] != self.generation {
                return None;
            }
            if self.keys[i] == k {
                return Some(self.vals[i]);
            }
            i = (i + 1) & mask;
        }
    }

    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.used = 0;
    }
}

pub fn install(m: &mut Machine) -> Result<(), String> {
    let l = |a: u32| m.img.link(a);
    let addr = Addr {
        part_vertices: l(PART_VERTICES),
        view_pos: l(VIEW_POS),
        matrix: l(MATRIX),
        packed_xy: l(PACKED_XY),
        variant: l(VARIANT),
        center_imm: [l(CENTER_IMM[0]), l(CENTER_IMM[1])],
        center_y: l(CENTER_Y),
        x_table_ptr: l(X_TABLE_PTR),
        x_offset: l(X_OFFSET),
        y_offset: l(Y_OFFSET),
        y_table: l(Y_TABLE),
        image: (m.img.base, m.img.base + m.img.size),
    };
    let _ = ADDR.set(addr);
    m.install_observer("sub-pixel vertices", AFTER_TRANSFORM, after_transform)
}

fn after_transform(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    if let Some(g) = m.glide.as_mut() {
        if let (true, Some(a)) = (ENABLED.load(Ordering::Relaxed), ADDR.get()) {
            unsafe { fill(&mut g.subpixel, a) };
        } else {
            g.subpixel.clear();
        }
    }
    HookResult::Resume(tramp)
}

/// A builder table around index `i`: (entry i, slope per index). The tables are linear
/// (float(i - e), 1.2 i, the mirror's half scale); the slope is taken from the neighbours
/// two apart, which skips the x table's .8 entry for the last column.
#[inline]
unsafe fn table(a: &Addr, base: u32, i: i64) -> Option<(f32, f64)> {
    if i < 1 || i + 1 >= TABLE_LEN {
        return None;
    }
    let p = base.wrapping_add(4 * (i - 1) as u32);
    if p < a.image.0 || p + 12 > a.image.1 {
        return None;
    }
    let (lo, at, hi) = unsafe { (f32::from_bits(mem::rd32(p)), f32::from_bits(mem::rd32(p + 4)), f32::from_bits(mem::rd32(p + 8))) };
    Some((at, (hi as f64 - lo as f64) / 2.0))
}

unsafe fn fill(map: &mut SubPixelMap, a: &Addr) {
    let list = unsafe { mem::rd32(a.part_vertices) };
    // vertex count: the model list ends with x = 7FFFh
    let mut n = 0usize;
    while n < 8192 && unsafe { mem::rd16(list.wrapping_add(6 * n as u32)) } != 0x7fff {
        n += 1;
    }
    map.reset(n);
    let mirror = unsafe { mem::rd32(a.variant) } & 1 != 0;
    let center = unsafe { mem::rd32(a.center_imm[mirror as usize]) } as i32 as f64;
    let cy = unsafe { mem::rd32(a.center_y) } as i32 as f64;
    let y_div = if mirror { 65536.0 } else { 32768.0 };
    let xt = unsafe { mem::rd32(a.x_table_ptr) };
    let (xo, yo) = unsafe { (mem::rd32(a.x_offset) as i32 as i64, mem::rd32(a.y_offset) as i32 as i64) };
    let mut mat = [0f64; 12];
    for (i, m) in mat.iter_mut().enumerate() {
        *m = unsafe { f32::from_bits(mem::rd32(a.matrix + 4 * i as u32)) } as f64;
    }
    for v in 0..n as u32 {
        let packed = unsafe { mem::rd32(a.packed_xy + 6 * v) };
        if packed == 0x1000_1000 {
            continue;
        }
        let p = a.view_pos + 12 * v;
        let (xi, yi, zi) = unsafe { (mem::rd32(p) as i32 as f64, mem::rd32(p + 4) as i32 as f64, mem::rd32(p + 8) as i32 as f64) };
        // the view position before the transform's rounding (the mirror view negates x);
        // used only where it rounds to what the transform stored
        let mv = list.wrapping_add(6 * v);
        let (mx, my, mz) = unsafe { (mem::rd16(mv) as i16 as f64, mem::rd16(mv + 2) as i16 as f64, mem::rd16(mv + 4) as i16 as f64) };
        let fx = mat[0] * mx + mat[3] * my + mat[6] * mz + mat[9];
        let fy = mat[1] * mx + mat[4] * my + mat[7] * mz + mat[10];
        let fz = mat[2] * mx + mat[5] * my + mat[8] * mz + mat[11];
        let fx = if mirror { -fx } else { fx };
        let exact = (fx - xi).abs() <= 0.501 && (fy - yi).abs() <= 0.501 && (fz - zi).abs() <= 0.501;
        let (x, y, z) = if exact { (fx, fy, fz) } else { (xi, yi, zi) };
        let d = z - 128.0;
        if d > -128.0 {
            continue;
        }
        // the transform: r = 800000h / (z - 128); x = (x r 2 + centre) >> 16; y = (y r >> 15) + cy
        let r = 8_388_608.0 / d;
        let px = (x * r * 2.0 + center) / 65536.0;
        let py = y * r / y_div + cy;
        let (ix, iy) = ((packed as i32 >> 16) as i64, (packed as i16) as i64);
        // the rounded position exactly as the builder computes it, and the table's slope
        let (Some((gx, sx)), Some((gy, sy))) = (unsafe { table(a, xt, xo + ix) }, unsafe { table(a, a.y_table, yo + iy) }) else {
            continue;
        };
        // exact, half a pixel back to match the rounding down on average
        let (dx, dy) = (px - 0.5 - ix as f64, py - 0.5 - iy as f64);
        // only when the exact position agrees with the rounded one (guards against reading
        // a vertex the transform did not write)
        if dx.abs() <= 1.5 && dy.abs() <= 1.5 {
            // 1/w = K / (z - 128): the builder's comes from the rounded z
            let ratio = ((zi - 128.0) / d) as f32;
            map.insert(gx, gy, ((gx as f64 + sx * dx) as f32, (gy as f64 + sy * dy) as f32, ratio));
        }
    }
}
