//! Per-vertex depth for the game's flat-colour polygons (STRT3FX), so the renderer's depth
//! buffer can hide them correctly.
//!
//! The game draws without a depth buffer (polygons sorted back to front) and computes a
//! per-vertex 1/w only where it needs one, for texturing. Every polygon set-up routine
//! (one per rendering mode, ca670 .. d60fe) ends by calling the Glide vertex builder
//! `cc5e8` through [aa6ac], which reads the vertices' view z from the list at [aa5dc]
//! (`oow = K / (z - 128)`, K = [122314] = -1). The textured set-ups fill that list from the
//! transformed vertices' z (68fd0); the flat-colour set-up (mode 2, entries d01c4 and
//! d02cc, builder call at d0430) does not, so its polygons get whatever the list held: a
//! constant, wrong depth (docs/re/strt3fx.md, "Depth").
//!
//! At the indexed entry (d02cc, eax = polygon record: +6 vertex count, +8 vertex indices)
//! this module maps the polygon's vertices to Glide screen space exactly as the builder
//! does and fits 1/w as a plane over the screen (exact for a planar polygon under
//! perspective; it also covers vertices made by the set-up's 2D clipper). The Glide layer
//! then evaluates that plane for the polygon's triangles (builder mode [aa5c8] = 2).
//! Polygons given as screen coordinates only (entry d01c4) have no depth: they are drawn
//! in the game's order, as before.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

/// Flat-colour set-up, polygon record with vertex indices (eax = record).
const FLAT_INDEXED: u32 = 0xd02cc;
/// Flat-colour set-up, polygon record with screen coordinates (no depth).
const FLAT_SCREEN: u32 = 0xd01c4;
/// Packed screen positions (x << 16 | y) of the transformed vertices, by vertex offset.
const PACKED_XY: u32 = 0x6ed94;
/// View z of the transformed vertices, at twice the vertex offset.
const VIEW_Z: u32 = 0x68fd0;
/// The builder's 1/w numerator (double).
const OOW_K: u32 = 0x122314;
/// The builder's screen tables: x table pointer, x and y offsets, y table.
const X_TABLE_PTR: u32 = 0xaa6c4;
const X_OFFSET: u32 = 0xaa6b8;
const Y_OFFSET: u32 = 0xaa6bc;
const Y_TABLE: u32 = 0xac6cc;
/// Rendering mode of the polygon being built (set before each builder call).
pub const BUILDER_MODE: u32 = 0xaa5c8;
/// Back-face culling of the object being drawn: on when [aa6b4] != 0 and bit 0 of
/// [aa5b0] is clear. The set-up routines test it (2D cross product of the first three
/// vertices < -20 -> skipped) only for polygons entirely on screen; polygons that reach
/// the 2D clipper are drawn whatever their facing, and only the game's painter order
/// hides them (a scenery polygon seen from behind, right in front of the camera, covered
/// by the road drawn after it).
pub const CULL_ON: u32 = 0xaa6b4;
pub const CULL_OFF_FLAGS: u32 = 0xaa5b0;

/// The renderer resolves hidden surfaces with a depth buffer: polygons the game leaves
/// to its drawing order (see `CULL_ON`) are culled as the game intends.
pub static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Mode of the flat-colour set-up.
pub const MODE_FLAT: u32 = 2;

/// Depth of the flat-colour polygon being built.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum FlatDepth {
    /// No depth known: drawn in the game's order without depth testing.
    #[default]
    Unknown,
    /// 1/w = a * x + b * y + c in Glide screen coordinates, valid inside the polygon's
    /// screen bounding box (x0, y0, x1, y1).
    Plane { a: f32, b: f32, c: f32, bbox: [f32; 4] },
}

impl FlatDepth {
    /// 1/w for a vertex of the current flat polygon (0 = no depth).
    pub fn oow(&self, x: f32, y: f32) -> f32 {
        match *self {
            FlatDepth::Plane { a, b, c, bbox } if x >= bbox[0] - 1.0 && x <= bbox[2] + 1.0 && y >= bbox[1] - 1.0 && y <= bbox[3] + 1.0 => {
                (a * x + b * y + c).max(1e-9)
            }
            _ => 0.0,
        }
    }
}

/// The engine draws an object part's polygons (`lea esi, [67088]` before `call 7ba0c` in
/// the frame loop at 78e71): each part gets an id for the renderer (see `next_part`).
const PART_DRAW: u32 = 0x78e71;

/// A new model part starts: its polygons get the next id (1..255; the renderer keeps it
/// per pixel in the stencil buffer so that glass can overlay its own part only).
fn next_part(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    if let Some(g) = m.glide.as_mut() {
        g.next_part();
    }
    HookResult::Resume(tramp)
}

/// Per-object set-up of the engine's draw loop (8246c; esi = the object's instance) at
/// 8253f: what kind of object the following parts belong to.
const OBJECT_SETUP: u32 = 0x8253f;
const SCENE: u32 = 0x12f860;

/// Kinds of 3D object, from the scene table's names (`crate::interp` has the table layout).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    /// Track sections and scenery (not in the scene table).
    #[default]
    Scenery,
    /// A car body (`auto1` .. `auto4`).
    Car,
    /// A car's wheels (`fauto`, `rauto`, `MV_mauto`).
    Wheel,
    /// A car's shadow (`ashadow`).
    Shadow,
    /// Smoke and dust puffs (`asmoke`, `a2smoke`).
    Smoke,
    /// Tyre marks (`aztri`, `az1tri`).
    Marks,
}

fn kind_of(name: &[u8]) -> Kind {
    let n = |p: &[u8]| name.starts_with(p);
    if n(b"auto") {
        Kind::Car
    } else if n(b"fauto") || n(b"rauto") || n(b"MV_mauto") {
        Kind::Wheel
    } else if n(b"ashadow") {
        Kind::Shadow
    } else if n(b"asmoke") || n(b"a2smoke") {
        Kind::Smoke
    } else if n(b"aztri") || n(b"az1tri") {
        Kind::Marks
    } else {
        Kind::Scenery
    }
}

/// Instance -> kind, from the scene table (read again once per game frame: objects come
/// and go between races).
#[derive(Default)]
pub struct Kinds {
    map: std::collections::HashMap<u32, Kind>,
    /// The scene table and how many of its entries lie inside the game image.
    scene: u32,
    entries: u32,
    frame: u64,
}

impl Kinds {
    fn lookup(&mut self, inst: u32) -> Kind {
        let frame = crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed);
        if frame != self.frame || self.map.is_empty() {
            self.frame = frame;
            self.map.clear();
            for i in 0..self.entries {
                let e = self.scene + i * 28;
                let (ptr, index) = unsafe { (mem::rd32(e + 0xc), mem::rd32(e + 0x18)) };
                if index == u32::MAX {
                    break;
                }
                let k = kind_of(unsafe { mem::slice(e, 8) });
                if k != Kind::Scenery {
                    self.map.insert(ptr, k);
                }
            }
        }
        self.map.get(&inst).copied().unwrap_or_default()
    }
}

fn object_setup(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    // memory may have been freed since the last object (a new race)
    HEAP_REGION.set((0, 0));
    if m.kinds.scene == 0 {
        let scene = m.img.link(SCENE);
        m.kinds.scene = scene;
        m.kinds.entries = (0..1024u32).take_while(|i| in_image(m, scene + i * 28, 28)).count() as u32;
    }
    let k = m.kinds.lookup(f.esi);
    if let Some(g) = m.glide.as_mut() {
        g.next_object(k);
    }
    HookResult::Resume(tramp)
}

pub fn install(m: &mut Machine) -> Result<(), String> {
    m.install_observer("object kinds", OBJECT_SETUP, object_setup)?;
    m.install_observer("model part ids", PART_DRAW, next_part)?;
    m.install_observer("flat polygon depth (indexed)", FLAT_INDEXED, flat_indexed)?;
    m.install_observer("flat polygon depth (screen)", FLAT_SCREEN, flat_screen)?;
    Ok(())
}

fn traced(g: &crate::glide::Glide) -> bool {
    g.trace_frame.is_some_and(|n| n > 0 && g.frames % n == 0 && g.frames > 0)
}

fn flat_screen(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    if let Some(g) = m.glide.as_mut() {
        g.flat_depth = FlatDepth::Unknown;
        if traced(g) {
            crate::info!("    flat polygon (screen) eax {:#x} part {:#x}", f.eax, g.part);
        }
    }
    HookResult::Resume(tramp)
}

fn flat_indexed(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    let d = unsafe { fit(m, f.eax) };
    if let Some(g) = m.glide.as_mut() {
        g.flat_depth = d;
        if traced(g) {
            crate::info!("    flat polygon (indexed) eax {:#x} part {:#x}: {:?}", f.eax, g.part, d);
        }
    }
    HookResult::Resume(tramp)
}

/// `len` bytes at `a` lie inside the game image (where all of the builder's tables and
/// the scenery's polygon records are; a plain range check, cheap enough for every polygon).
fn in_image(m: &Machine, a: u32, len: u32) -> bool {
    m.img.contains(a) && m.img.contains(a.wrapping_add(len - 1))
}

thread_local! {
    /// The readable heap region last found (cars' models are loaded into the heap, one
    /// region per object; reset for every object, see `object_setup`).
    static HEAP_REGION: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
}

/// `len` bytes at `a` can be read: in the image, or in a committed heap region.
fn record_readable(m: &Machine, a: u32, len: u32) -> bool {
    if in_image(m, a, len) {
        return true;
    }
    let Some(end) = a.checked_add(len) else { return false };
    let (s, e) = HEAP_REGION.get();
    if a >= s && end <= e && e > 0 {
        return true;
    }
    match mem::readable_region(a) {
        Some((s, e)) if end <= e => {
            HEAP_REGION.set((s, e));
            true
        }
        _ => false,
    }
}

unsafe fn fit(m: &Machine, record: u32) -> FlatDepth {
    // scenery records are in the image, the cars' in the heap
    if !record_readable(m, record, 8) {
        return FlatDepth::Unknown;
    }
    let n = unsafe { mem::rd16(record + 6) } as u32;
    if !(3..=32).contains(&n) || !record_readable(m, record + 8, 2 * n) {
        return FlatDepth::Unknown;
    }
    let link = |a: u32| m.img.link(a);
    let k = unsafe { f64::from_bits(mem::rd32(link(OOW_K)) as u64 | (mem::rd32(link(OOW_K) + 4) as u64) << 32) };
    let x_table = unsafe { mem::rd32(link(X_TABLE_PTR)) };
    let (xo, yo) = unsafe { (mem::rd32(link(X_OFFSET)) as i32, mem::rd32(link(Y_OFFSET)) as i32) };
    let mut pts: Vec<(f64, f64, f64)> = Vec::with_capacity(n as usize);
    for j in 0..n {
        let idx = unsafe { mem::rd16(record + 8 + 2 * j) } as u32;
        let (pa, za) = (link(PACKED_XY) + idx, link(VIEW_Z) + idx * 2);
        if !in_image(m, pa, 4) || !in_image(m, za, 4) {
            return FlatDepth::Unknown;
        }
        let packed = unsafe { mem::rd32(pa) } as i32;
        let z = unsafe { mem::rd32(za) } as i32;
        let (px, py) = (packed >> 16, packed as i16 as i32);
        let (xa, ya) = (x_table.wrapping_add((4 * (xo + px)) as u32), link(Y_TABLE).wrapping_add((4 * (yo + py)) as u32));
        if !in_image(m, xa, 4) || !in_image(m, ya, 4) || z - 128 == 0 {
            return FlatDepth::Unknown;
        }
        let (gx, gy) = unsafe { (f32::from_bits(mem::rd32(xa)), f32::from_bits(mem::rd32(ya))) };
        // the exact position when the vertex has one (crate::subpixel), as the triangles get
        let (gx, gy, r) = m.glide.as_ref().and_then(|g| g.subpixel.get(gx, gy)).unwrap_or((gx, gy, 1.0));
        let (gx, gy) = (gx as f64, gy as f64);
        let w = k / (z - 128) as f64 * r as f64;
        if !(gx.is_finite() && gy.is_finite() && w.is_finite() && w > 0.0) {
            return FlatDepth::Unknown;
        }
        pts.push((gx, gy, w));
    }
    let bbox = pts.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| {
        [b[0].min(p.0 as f32), b[1].min(p.1 as f32), b[2].max(p.0 as f32), b[3].max(p.1 as f32)]
    });
    // the best-conditioned triangle of vertices for the plane
    let mut best = (0.0f64, 0usize, 0usize);
    for j in 1..pts.len() {
        for l in j + 1..pts.len() {
            let det = (pts[j].0 - pts[0].0) * (pts[l].1 - pts[0].1) - (pts[l].0 - pts[0].0) * (pts[j].1 - pts[0].1);
            if det.abs() > best.0.abs() {
                best = (det, j, l);
            }
        }
    }
    let (det, j, l) = best;
    if det.abs() < 0.5 {
        // degenerate on screen (edge-on): one depth for all of it
        let w = pts.iter().map(|p| p.2).sum::<f64>() / pts.len() as f64;
        return FlatDepth::Plane { a: 0.0, b: 0.0, c: w as f32, bbox };
    }
    let (p0, p1, p2) = (pts[0], pts[j], pts[l]);
    let (dx1, dy1, dw1) = (p1.0 - p0.0, p1.1 - p0.1, p1.2 - p0.2);
    let (dx2, dy2, dw2) = (p2.0 - p0.0, p2.1 - p0.1, p2.2 - p0.2);
    let a = (dw1 * dy2 - dw2 * dy1) / det;
    let b = (dx1 * dw2 - dx2 * dw1) / det;
    let c = p0.2 - a * p0.0 - b * p0.1;
    FlatDepth::Plane { a: a as f32, b: b as f32, c: c as f32, bbox }
}
