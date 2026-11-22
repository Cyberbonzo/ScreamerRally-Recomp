//! The loaded world (STRT3FX): grid cells, world objects, models, parts and detail levels
//! (docs/re/native/world.md), and the `SR_DUMP_WORLD` export.
//!
//! `SR_DUMP_WORLD=<dir>[,<game frame>[,<frames>]]` (diagnostics; nothing is installed
//! without it): at the first race frame (race frame timer, 4f793) from the given game
//! frame on (default 1700: a race on Rally China started with the documented key script)
//! every object of the loaded world (all grid cells, not only the visible ones, plus the
//! scene table's instances) is exported to `<dir>/world.obj` (each part at its highest
//! available detail level, vertices in world coordinates, game units, y up) with
//! `<dir>/world.txt` (structures, counts, the completeness analysis) and `<dir>/
//! world_top.bmp` (a top view). During the next `<frames>` race frames (default 1) every
//! part the engine transforms in the main view is compared with the export's own placement
//! (`<dir>/verify.txt`): the engine's view positions (`68fc8 + 12 n`) against the export's
//! world positions brought into the view with the camera of that moment.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::OnceLock;

// Link addresses (docs/re/native/world.md).
/// Pointer to the model pointer table (one pointer per model, the table ends with -1).
const MODELS: u32 = 0x147390;
/// Pointer to the grid's row table (256 rows, -1 = empty; a row: 256 cell pointers).
const ROWS: u32 = 0x123720;
/// Scene table (28-byte entries: name, instance A at +0Ch, B at +10h, index at +18h).
const SCENE: u32 = 0x12f860;
/// The instanced-object list handed to the frame drawing (34a88: eax = 12f478 -> [7bb7c]):
/// instance pointers, bit 30 = skipped this frame, 0 ends it.
const INSTANCED: u32 = 0x12f478;
/// Sine table: 2048 entries of 8 bytes (two 16-bit values used, at +0 and +4).
const SINE: u32 = 0xc48e0;
/// Global animation clock (animated models' frame = frames * clock >> 16).
const ANIM_CLOCK: u32 = 0x1473a0;
/// The 3D module's camera pointer and the camera position copied by `82338`.
const CAMERA_PTR: u32 = 0x81e0c;
const CAMERA_POS: u32 = 0x82454;
/// Transformed vertices: view position (3 x i32 per vertex).
const VIEW_POS: u32 = 0x68fc8;
/// Bit 0: rear-view mirror variant of the drawing.
const VARIANT: u32 = 0xaa5b0;
/// The draw loop's current object block (7bab4: model, part count, ...).
const CUR_MODEL: u32 = 0x7bab4;
const CUR_SLOT: u32 = 0x7babc;
const CUR_PART: u32 = 0x7bac4;
const CUR_VERTS: u32 = 0x7bacc;
const CUR_INSTANCE: u32 = 0x7bb8c;
/// Sorted part indices of the current object (words, 4 bytes apart).
const PART_ORDER: u32 = 0x81d44;
/// After the part transform (78e41 call 7aa00) and the sub-pixel observer (78e4e):
/// `mov dword ptr [82ea8h], 17700000h`.
const AFTER_TRANSFORM: u32 = 0x78e58;

/// Parts per detail level in a model (the level tables are 13 entries, 34h bytes, apart).
const LEVEL_SLOTS: u32 = 13;
const OBJECT_SIZE: u32 = 0x40;

struct Config {
    dir: std::path::PathBuf,
    frame: u64,
    /// Race frames over which the export is compared with the engine (default 1).
    verify_frames: u64,
}

fn config() -> Option<&'static Config> {
    static C: OnceLock<Option<Config>> = OnceLock::new();
    C.get_or_init(|| {
        let v = std::env::var("SR_DUMP_WORLD").ok()?;
        let mut it = v.split(',');
        let dir = it.next()?.trim().to_string();
        let frame = it.next().and_then(|f| f.trim().parse().ok()).unwrap_or(1700);
        let verify_frames = it.next().and_then(|f| f.trim().parse().ok()).unwrap_or(1u64).max(1);
        Some(Config { dir: dir.into(), frame, verify_frames })
    })
    .as_ref()
}

#[derive(Default)]
pub struct WorldDump {
    race_frames: u64,
    done: bool,
    verify_left: u64,
    /// Verification armed for the current game frame.
    verify: Option<Verify>,
}

#[derive(Default)]
struct Verify {
    /// Instance address -> object (as exported).
    objects: BTreeMap<u32, Object>,
    lines: Vec<String>,
    parts: u32,
    vertices: u64,
    unknown: u32,
    /// Largest difference (units, modulo 2^18) and largest difference / view distance;
    /// per kind of object: (parts, vertices, largest, largest relative).
    worst: f64,
    worst_rel: f64,
    by_source: BTreeMap<&'static str, (u32, u64, f64, f64)>,
    /// Distinct (instance, part index) compared, and the world objects in the export.
    seen: BTreeSet<(u32, u32)>,
    world_objects: u32,
    /// Errors by view distance band (10^k units): (vertices, max error).
    by_distance: BTreeMap<u32, (u64, f64)>,
    /// Vertices the engine placed a multiple of 2^18 away (its 32-bit overflow), and the
    /// nearest such vertex's true view distance.
    wrapped: u64,
    wrapped_min_dist: f64,
    /// Objects whose position, angles, model or flags changed since the dump, and per
    /// (field, object) the dump's value and the last one seen.
    changed: BTreeSet<u32>,
    changed_fields: BTreeMap<(&'static str, u32), (String, String)>,
}

/// Installs the verification observer (only with `SR_DUMP_WORLD`).
pub fn install(m: &mut Machine) -> Result<(), String> {
    if config().is_none() {
        return Ok(());
    }
    m.install_observer("world dump (verification)", AFTER_TRANSFORM, after_transform)
}

/// Called at every race frame (end of the race timer's tick wait, 4f793).
pub fn race_frame(m: &mut Machine) {
    let Some(cfg) = config() else { return };
    if m.world.verify.is_some() {
        m.world.verify_left = m.world.verify_left.saturating_sub(1);
        if m.world.verify_left == 0 {
            finish_verify(cfg, m.world.verify.as_ref().unwrap());
            m.world.verify = None;
        } else if m.world.verify_left % 200 == 0 {
            // a long verification may outlast the run: written as it goes
            finish_verify(cfg, m.world.verify.as_ref().unwrap());
        }
    }
    m.world.race_frames += 1;
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    let camera = unsafe { mem::rd32(m.img.link(CAMERA_PTR)) };
    if m.world.done || frame < cfg.frame || camera == 0 {
        return;
    }
    m.world.done = true;
    match dump(m, cfg) {
        Ok(objects) => {
            let world_objects = objects.iter().filter(|o| matches!(o.source, Source::Cell { .. })).count() as u32;
            m.world.verify = Some(Verify { objects: objects.into_iter().map(|o| (o.addr, o)).collect(), world_objects, wrapped_min_dist: f64::MAX, ..Default::default() });
            m.world.verify_left = cfg.verify_frames;
        }
        Err(e) => crate::warn!("world dump: {e}"),
    }
}

// ---------------------------------------------------------------------------------------
// Guest memory, read defensively (the dump walks pointers found in data).

#[derive(Default)]
pub(crate) struct Reader {
    regions: Vec<(u32, u32)>,
}

impl Reader {
    pub(crate) fn ok(&mut self, a: u32, len: u32) -> bool {
        let end = a as u64 + len as u64;
        if a < 0x10000 || end > u32::MAX as u64 {
            return false;
        }
        if self.regions.iter().any(|&(s, e)| a >= s && end <= e as u64) {
            return true;
        }
        let mut p = a as u64;
        while p < end {
            match mem::readable_region(p as u32) {
                Some((s, e)) if e as u64 > p => {
                    self.regions.push((s, e));
                    p = e as u64;
                }
                _ => return false,
            }
        }
        true
    }
    pub(crate) fn u32(&mut self, a: u32) -> Option<u32> {
        self.ok(a, 4).then(|| unsafe { mem::rd32(a) })
    }
    pub(crate) fn i32(&mut self, a: u32) -> Option<i32> {
        self.u32(a).map(|v| v as i32)
    }
    pub(crate) fn u16(&mut self, a: u32) -> Option<u16> {
        self.ok(a, 2).then(|| unsafe { mem::rd16(a) })
    }
    pub(crate) fn i16(&mut self, a: u32) -> Option<i16> {
        self.u16(a).map(|v| v as i16)
    }
    pub(crate) fn bytes(&mut self, a: u32, n: u32) -> Option<Vec<u8>> {
        self.ok(a, n).then(|| unsafe { mem::slice(a, n) }.to_vec())
    }
}

// ---------------------------------------------------------------------------------------
// Structures.

/// A model (the model table's entry).
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub index: u32,
    pub addr: u32,
    /// +0: bounding sphere radius (the object cull at 82240).
    pub radius: i32,
    /// +10h: animation frames (-1: not animated).
    pub anim_frames: i32,
    /// +14h: part count.
    pub parts: u32,
    /// +18h: three level tables of 13 part offsets (relative to the model; 0 = none).
    pub levels: [[u32; LEVEL_SLOTS as usize]; 3],
    /// +B4h: base of the polygon records' material words ([14739c] while drawing).
    pub poly_base: u32,
    /// +C4h: sort bias (subtracted from the object's sort key at 8225b).
    pub sort_bias: i16,
}

/// A part at one detail level (the part data).
#[derive(Clone, Debug, Default)]
pub struct Part {
    pub addr: u32,
    pub radius: i32,
    pub centre: [i16; 3],
    /// +0Ch: placement (relative to the model): angles at +4, +8, +0Ch, position at +10h,
    /// +14h, +18h; animated models: frames of two 3 x 4 matrices (int16) from +1Ch.
    pub placement: u32,
    pub flags: u32,
    /// +1Ch: vertex list (relative to the model; int16 x, y, z, ends with x = 7FFFh).
    pub verts: u32,
    pub f20: u32,
    pub f24: u32,
    /// +28h: 16 polygon orders (relative to the part; int16 polygon offsets relative to the
    /// part, ended by a value <= 0), chosen by the viewing octant (81eac).
    pub orders: [u32; 16],
    /// +68h: name.
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// A grid cell's object list (0: cell +0 / count +14h, 1: cell +4 / count +18h).
    Cell { row: u16, col: u16, list: u8, index: u32 },
    /// A scene table entry's instance A.
    Scene { entry: u32 },
}

/// A world object (the 40h-byte instance).
#[derive(Clone, Debug)]
pub struct Object {
    pub addr: u32,
    pub source: Source,
    pub name: String,
    /// The scene entry's instance B (per-part placements, 20h bytes each), if any.
    pub parts_b: Option<u32>,
    pub model: u32,
    pub flags: u32,
    pub pos: [i32; 3],
    pub angles: [i32; 3],
    pub anim: i32,
}

impl Object {
    fn source_name(&self) -> &'static str {
        match self.source {
            Source::Cell { list: 0, .. } => "cell list 0",
            Source::Cell { .. } => "cell list 1",
            Source::Scene { .. } => kind(self),
        }
    }
}

/// x' = r0 x + r3 y + r6 z + t0, y' = r1 x + r4 y + r7 z + t1, z' = r2 x + r5 y + r8 z + t2
/// (the engine's matrix layout, docs/re/strt3fx.md "Transform").
#[derive(Clone, Copy, Debug)]
struct Xf {
    r: [f64; 9],
    t: [f64; 3],
}

impl Xf {
    fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let r = &self.r;
        [
            r[0] * v[0] + r[3] * v[1] + r[6] * v[2] + self.t[0],
            r[1] * v[0] + r[4] * v[1] + r[7] * v[2] + self.t[1],
            r[2] * v[0] + r[5] * v[1] + r[8] * v[2] + self.t[2],
        ]
    }
    /// self after other (the engine's 79638 with esi = self, ebp = other).
    fn after(&self, o: &Xf) -> Xf {
        let a = &self.r;
        let b = &o.r;
        let mut r = [0.0; 9];
        for c in 0..3 {
            for row in 0..3 {
                r[c * 3 + row] = (0..3).map(|k| a[k * 3 + row] * b[c * 3 + k]).sum();
            }
        }
        let t = self.apply(o.t);
        Xf { r, t }
    }
}

pub(crate) struct World {
    pub(crate) sine: Vec<(i16, i16)>,
    pub(crate) clock: i32,
    pub(crate) camera_angles: [i32; 3],
    pub(crate) models: Vec<Option<Model>>,
    pub(crate) parts: BTreeMap<u32, Option<Part>>,
}

impl World {
    /// The engine's rotation from three 11-bit angles (82d04 -> 793b0), in floating point
    /// with the table's values (Q15).
    fn rot(&self, a: [i32; 3]) -> [f64; 9] {
        let t = |x: i32| {
            let (w0, w1) = self.sine[(x & 0x7ff) as usize];
            (w0 as f64 / 32768.0, w1 as f64 / 32768.0)
        };
        let (a0, b0) = t(a[0]);
        let (c1, d1) = t(a[1]);
        let (e2, f2) = t(a[2]);
        [f2 * d1 - a0 * c1 * e2, b0 * e2, a0 * d1 * e2 + c1 * f2, -(a0 * c1 * f2 + d1 * e2), b0 * f2, a0 * d1 * f2 - c1 * e2, -b0 * c1, -a0, b0 * d1]
    }

    pub(crate) fn model(&self, i: u32) -> Option<&Model> {
        self.models.get(i as usize).and_then(|m| m.as_ref())
    }

    pub(crate) fn part(&self, addr: u32) -> Option<&Part> {
        self.parts.get(&addr).and_then(|p| p.as_ref())
    }

    /// Model -> world transform of part `i` of an object (docs/re/native/world.md,
    /// "Placement"); None if the data cannot be read.
    fn part_xf(&self, r: &mut Reader, o: &Object, i: u32) -> Option<Xf> {
        let model = self.model(o.model)?;
        // the object: billboards (flag 8000h, 82c3c -> 52dc0) face the camera
        let angles = if o.flags & 0x8000 != 0 {
            let cam = self.camera_angles;
            [(o.angles[0] - cam[0]) & 0x7ff, (-(o.angles[1] - cam[1] + 0x400)) & 0x7ff, 0x7ff - cam[2]]
        } else {
            o.angles
        };
        let obj = Xf { r: self.rot(angles), t: [o.pos[0] as f64, o.pos[1] as f64, o.pos[2] as f64] };
        // the part's placement comes from its level-0 data (an entry of 0 reads the model)
        let part0 = model.addr.wrapping_add(model.levels[0][i as usize]);
        let placement = model.addr.wrapping_add(r.u32(part0 + 0xc)?);
        let part = if model.anim_frames >= 0 {
            // animated: the frame's 3 x 4 matrix (int16, rotation Q14), 826b4
            let frame = (model.anim_frames.wrapping_mul(self.clock) >> 16).wrapping_mul(0x30) as u32;
            let at = placement.wrapping_add(0x1c).wrapping_add(frame);
            let mut w = [0f64; 12];
            for (k, x) in w.iter_mut().enumerate() {
                *x = r.i16(at + 2 * k as u32)? as f64;
            }
            let mut rr = [0.0; 9];
            for k in 0..9 {
                rr[k] = w[k] / 16384.0;
            }
            Xf { r: rr, t: [w[9], w[10], w[11]] }
        } else if let Some(b) = o.parts_b {
            // instanced objects (cars): per-part angles at +8 and position at +14h of the
            // instance's part entry (826ec)
            let e = b + 0x20 * i;
            let a = [r.i32(e + 8)?, r.i32(e + 0xc)?, r.i32(e + 0x10)?];
            let p = [r.i32(e + 0x14)?, r.i32(e + 0x18)?, r.i32(e + 0x1c)?];
            Xf { r: self.rot(a), t: [p[0] as f64, p[1] as f64, p[2] as f64] }
        } else {
            let a = [r.i32(placement + 4)?, r.i32(placement + 8)?, r.i32(placement + 0xc)?];
            let p = [r.i32(placement + 0x10)?, r.i32(placement + 0x14)?, r.i32(placement + 0x18)?];
            Xf { r: self.rot(a), t: [p[0] as f64, p[1] as f64, p[2] as f64] }
        };
        Some(obj.after(&part))
    }
}

fn read_model(r: &mut Reader, index: u32, addr: u32) -> Option<Model> {
    let mut m = Model { index, addr, ..Default::default() };
    m.radius = r.i32(addr)?;
    m.anim_frames = r.i32(addr + 0x10)?;
    m.parts = r.u32(addr + 0x14)?;
    for l in 0..3 {
        for s in 0..LEVEL_SLOTS {
            m.levels[l as usize][s as usize] = r.u32(addr + 0x18 + l * 0x34 + 4 * s)?;
        }
    }
    m.poly_base = r.u32(addr + 0xb4)?;
    m.sort_bias = r.i16(addr + 0xc4)?;
    Some(m)
}

pub(crate) fn read_part(r: &mut Reader, addr: u32) -> Option<Part> {
    let mut p = Part { addr, ..Default::default() };
    p.radius = r.i32(addr)?;
    for k in 0..3 {
        p.centre[k] = r.i16(addr + 4 + 2 * k as u32)?;
    }
    p.placement = r.u32(addr + 0xc)?;
    p.flags = r.u32(addr + 0x18)?;
    p.verts = r.u32(addr + 0x1c)?;
    p.f20 = r.u32(addr + 0x20)?;
    p.f24 = r.u32(addr + 0x24)?;
    for k in 0..16 {
        p.orders[k] = r.u32(addr + 0x28 + 4 * k as u32)?;
    }
    let name = r.bytes(addr + 0x68, 16)?;
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    p.name = name[..end].iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '?' }).collect();
    Some(p)
}

/// The vertices of a vertex list (ends with x = 7FFFh).
pub(crate) fn vertices(r: &mut Reader, list: u32) -> Option<Vec<[i16; 3]>> {
    let mut out = Vec::new();
    for n in 0..8192u32 {
        let a = list.wrapping_add(6 * n);
        let x = r.i16(a)?;
        if x == 0x7fff {
            return Some(out);
        }
        out.push([x, r.i16(a + 2)?, r.i16(a + 4)?]);
    }
    None
}

/// The polygon records of a part (the union of its 16 orders) and whether all 16 orders
/// hold the same set.
pub(crate) fn polygons(r: &mut Reader, part: &Part) -> Option<(BTreeSet<u32>, bool)> {
    let mut all = BTreeSet::new();
    let mut first: Option<BTreeSet<u32>> = None;
    let mut same = true;
    for k in 0..16 {
        let mut set = BTreeSet::new();
        let mut p = part.addr.wrapping_add(part.orders[k]);
        for _ in 0..65536 {
            let v = r.i16(p)?;
            if v <= 0 {
                break;
            }
            set.insert(part.addr + v as u32);
            p += 2;
        }
        match &first {
            None => first = Some(set.clone()),
            Some(f) => same &= *f == set,
        }
        all.extend(set);
    }
    Some((all, same))
}

/// A polygon record: material word (+4), vertex offsets (6 n, from +8, count at +6).
pub(crate) fn polygon(r: &mut Reader, rec: u32) -> Option<(u16, Vec<u16>)> {
    let n = r.u16(rec + 6)?;
    if n == 0 || n > 64 {
        return None;
    }
    let mut v = Vec::with_capacity(n as usize);
    for j in 0..n as u32 {
        v.push(r.u16(rec + 8 + 2 * j)?);
    }
    Some((r.u16(rec + 4)?, v))
}

fn scene_name(m: &Machine, entry: u32) -> String {
    let e = m.img.link(SCENE) + entry * 28;
    let b = unsafe { mem::slice(e, 8) };
    let end = b.iter().position(|&c| c == 0).unwrap_or(8);
    b[..end].iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '?' }).collect()
}

fn read_object(r: &mut Reader, addr: u32, source: Source, name: String, parts_b: Option<u32>) -> Option<Object> {
    Some(Object {
        addr,
        source,
        name,
        parts_b,
        model: r.u32(addr)?,
        flags: r.u32(addr + 4)?,
        pos: [r.i32(addr + 8)?, r.i32(addr + 0xc)?, r.i32(addr + 0x10)?],
        angles: [r.i32(addr + 0x14)?, r.i32(addr + 0x18)?, r.i32(addr + 0x1c)?],
        anim: r.i32(addr + 0x20)?,
    })
}

/// A grid cell (only the fields this module reads).
#[derive(Clone, Debug, Default)]
pub(crate) struct Cell {
    pub(crate) row: u16,
    pub(crate) col: u16,
    pub(crate) addr: u32,
    /// +0 .. +10h: the five relocated pointers (lists 0 and 1, two unknown, visible cells).
    pub(crate) ptr: [u32; 5],
    /// +14h .. +24h: their counts.
    pub(crate) count: [u32; 5],
}

pub(crate) fn read_world(m: &Machine, r: &mut Reader) -> Result<(World, Vec<Cell>, Vec<Object>), String> {
    read_world_with(m, r, true)
}

/// As [`read_world`]; without `cells` the grid is not read (a menu's 3D scene drawn without
/// cells, `78d10`: the row table may be left over from another world, `menus.md`).
pub(crate) fn read_world_with(m: &Machine, r: &mut Reader, cells: bool) -> Result<(World, Vec<Cell>, Vec<Object>), String> {
    let link = |a: u32| m.img.link(a);
    let grid_rows = if cells { 256u32 } else { 0 };
    let sine: Vec<(i16, i16)> = (0..2048u32).map(|i| unsafe { (mem::rd16(link(SINE) + 8 * i) as i16, mem::rd16(link(SINE) + 8 * i + 4) as i16) }).collect();
    let clock = unsafe { mem::rd32(link(ANIM_CLOCK)) } as i32;
    let cam = unsafe { mem::rd32(link(CAMERA_PTR)) };
    let camera_angles = if r.ok(cam, 12) { unsafe { [mem::rd32(cam) as i32, mem::rd32(cam + 4) as i32, mem::rd32(cam + 8) as i32] } } else { [0; 3] };
    // models
    let table = unsafe { mem::rd32(link(MODELS)) };
    let mut models = Vec::new();
    for i in 0..4096u32 {
        let Some(p) = r.u32(table.wrapping_add(4 * i)) else { return Err(format!("model table {table:#x} unreadable")) };
        if p == u32::MAX {
            break;
        }
        models.push(read_model(r, i, p));
    }
    // cells and their objects
    let rows = unsafe { mem::rd32(link(ROWS)) };
    let mut cells = Vec::new();
    let mut objects = Vec::new();
    for row in 0..grid_rows {
        let Some(rp) = r.u32(rows.wrapping_add(4 * row)) else { return Err(format!("row table {rows:#x} unreadable")) };
        if rp == u32::MAX {
            continue;
        }
        for col in 0..256u32 {
            let Some(cp) = r.u32(rp.wrapping_add(4 * col)) else { break };
            if cp == u32::MAX {
                continue;
            }
            let mut c = Cell { row: row as u16, col: col as u16, addr: cp, ..Default::default() };
            for k in 0..5 {
                c.ptr[k] = r.u32(cp + 4 * k as u32).unwrap_or(0);
                c.count[k] = r.u32(cp + 0x14 + 4 * k as u32).unwrap_or(0);
            }
            for list in 0..2u8 {
                let (base, n) = (c.ptr[list as usize], c.count[list as usize]);
                if n > 100000 {
                    continue;
                }
                for j in 0..n {
                    let a = base.wrapping_add(OBJECT_SIZE * j);
                    let src = Source::Cell { row: row as u16, col: col as u16, list, index: j };
                    if let Some(o) = read_object(r, a, src, String::new(), None) {
                        objects.push(o);
                    }
                }
            }
            cells.push(c);
        }
    }
    // the scene table's instances (cars, shadows, smoke, marks)
    let scene = link(SCENE);
    for e in 0..1024u32 {
        let a = scene + e * 28;
        let (inst, b, index) = unsafe { (mem::rd32(a + 0xc), mem::rd32(a + 0x10), mem::rd32(a + 0x18)) };
        if index == u32::MAX {
            break;
        }
        let parts_b = r.ok(b, 0x20).then_some(b);
        if let Some(o) = read_object(r, inst, Source::Scene { entry: e }, scene_name(m, e), parts_b) {
            objects.push(o);
        }
    }
    let mut w = World { sine, clock, camera_angles, models, parts: BTreeMap::new() };
    // every part data of every level of every model
    for mo in w.models.iter().flatten() {
        for l in 0..3 {
            for s in 0..LEVEL_SLOTS as usize {
                let off = mo.levels[l][s];
                if off != 0 {
                    let a = mo.addr.wrapping_add(off);
                    w.parts.entry(a).or_insert_with(|| read_part(r, a));
                }
            }
        }
    }
    Ok((w, cells, objects))
}

// ---------------------------------------------------------------------------------------
// The dump.

pub(crate) fn kind(o: &Object) -> &'static str {
    let n = |p: &str| o.name.starts_with(p);
    match o.source {
        Source::Cell { .. } => "world",
        _ if n("auto") => "car",
        _ if n("fauto") || n("rauto") || n("MV_mauto") => "wheel",
        _ if n("ashadow") => "shadow",
        _ if n("asmoke") || n("a2smoke") => "smoke",
        _ if n("aztri") || n("az1tri") => "marks",
        _ => "scene other",
    }
}

/// The level the export takes: the highest with data (as `crate::lod`).
pub(crate) fn best_level(model: &Model, i: usize) -> Option<(usize, u32)> {
    (0..3).find(|&l| model.levels[l][i] != 0).map(|l| (l, model.addr.wrapping_add(model.levels[l][i])))
}

fn dump(m: &mut Machine, cfg: &Config) -> Result<Vec<Object>, String> {
    let t0 = std::time::Instant::now();
    let mut r = Reader::default();
    let (w, cells, objects) = read_world(m, &mut r)?;
    std::fs::create_dir_all(&cfg.dir).map_err(|e| e.to_string())?;
    let mut txt = String::new();
    let mut obj = String::new();
    let link = |a: u32| m.img.link(a);
    let _ = writeln!(txt, "SR_DUMP_WORLD at race frame {} (game frame {})", m.world.race_frames, m.glide.as_ref().map_or(0, |g| g.frames));
    let _ = writeln!(txt, "models: table {:#x}, {} entries; grid rows {:#x}; {} cells; {} objects ({} in cells)", unsafe { mem::rd32(link(MODELS)) }, w.models.len(), unsafe { mem::rd32(link(ROWS)) }, cells.len(), objects.len(), objects.iter().filter(|o| matches!(o.source, Source::Cell { .. })).count());
    let _ = writeln!(txt, "sine table entry 0: {:?}, 256: {:?}, 512: {:?}, 1024: {:?}", w.sine[0], w.sine[256], w.sine[512], w.sine[1024]);
    let cam = unsafe { mem::rd32(link(CAMERA_PTR)) };
    let campos = unsafe { [mem::rd32(link(CAMERA_POS)) as i32, mem::rd32(link(CAMERA_POS) + 4) as i32, mem::rd32(link(CAMERA_POS) + 8) as i32] };
    let _ = writeln!(txt, "camera struct {cam:#x}: angles {:?}, position {campos:?}; animation clock {}", w.camera_angles, w.clock);
    // instanced list
    let mut inst = Vec::new();
    for k in 0..250u32 {
        let v = unsafe { mem::rd32(link(INSTANCED) + 4 * k) };
        if v == 0 {
            break;
        }
        inst.push(v);
    }
    let _ = writeln!(txt, "instanced list 12f478: {} entries, {} skipped (bit 30)", inst.len(), inst.iter().filter(|&&v| v & 0x4000_0000 != 0).count());

    // ---- models
    let _ = writeln!(txt, "\n== models (index addr radius anim parts | per level: parts present / polygons / vertices) ==");
    let mut model_polys: BTreeMap<u32, [(u32, u32, u32); 3]> = BTreeMap::new();
    let mut part_info: BTreeMap<u32, (usize, bool, usize)> = BTreeMap::new(); // part -> (polygons, orders same, vertices)
    for mo in w.models.iter().flatten() {
        let mut lv = [(0u32, 0u32, 0u32); 3];
        for (l, out) in lv.iter_mut().enumerate() {
            for s in 0..LEVEL_SLOTS as usize {
                if mo.levels[l][s] == 0 {
                    continue;
                }
                let a = mo.addr.wrapping_add(mo.levels[l][s]);
                let info = *part_info.entry(a).or_insert_with(|| {
                    let Some(p) = w.part(a) else { return (0, false, 0) };
                    let (polys, same) = polygons(&mut r, p).unwrap_or_default();
                    let nv = vertices(&mut r, mo.addr.wrapping_add(p.verts)).map_or(0, |v| v.len());
                    (polys.len(), same, nv)
                });
                out.0 += 1;
                out.1 += info.0 as u32;
                out.2 += info.2 as u32;
            }
        }
        model_polys.insert(mo.index, lv);
        let names: Vec<String> = (0..mo.parts.min(LEVEL_SLOTS) as usize)
            .map(|s| {
                let lv: String = (0..3).map(|l| if mo.levels[l][s] != 0 { char::from(b'0' + l as u8) } else { '-' }).collect();
                let name = best_level(mo, s).and_then(|(_, a)| w.part(a)).map_or("?".into(), |p| p.name.clone());
                format!("{name}[{lv}]")
            })
            .collect();
        let _ = writeln!(
            txt,
            "model {:3} {:#010x} r {:6} anim {:3} parts {:2} | L0 {:2}/{:5}/{:5} L1 {:2}/{:5}/{:5} L2 {:2}/{:5}/{:5} | bias {} base {:#x} | {}",
            mo.index, mo.addr, mo.radius, mo.anim_frames, mo.parts, lv[0].0, lv[0].1, lv[0].2, lv[1].0, lv[1].1, lv[1].2, lv[2].0, lv[2].1, lv[2].2, mo.sort_bias, mo.poly_base, names.join(" ")
        );
    }
    let unequal = part_info.values().filter(|i| !i.1).count();
    let _ = writeln!(txt, "parts: {} distinct part data; {} with orders that differ in their polygon set", part_info.len(), unequal);
    for mo in w.models.iter().flatten() {
        if mo.parts > LEVEL_SLOTS {
            let _ = writeln!(txt, "  model {} has {} parts (> 13 slots)", mo.index, mo.parts);
        }
        for l in 0..3 {
            for s in mo.parts.min(LEVEL_SLOTS) as usize..LEVEL_SLOTS as usize {
                if mo.levels[l][s] != 0 {
                    let _ = writeln!(txt, "  model {} level {l} slot {s} beyond its part count holds {:#x}", mo.index, mo.levels[l][s]);
                }
            }
        }
    }

    // ---- cells
    let mut counts = [0u64; 5];
    let mut nonzero = [0u32; 5];
    for c in &cells {
        for k in 0..5 {
            counts[k] += c.count[k] as u64;
            nonzero[k] += (c.count[k] != 0) as u32;
        }
    }
    let _ = writeln!(txt, "\n== cells ==\nfield counts (sum / cells with non-zero): +0/+14h {}/{}, +4/+18h {}/{}, +8/+1Ch {}/{}, +0Ch/+20h {}/{}, +10h/+24h {}/{}", counts[0], nonzero[0], counts[1], nonzero[1], counts[2], nonzero[2], counts[3], nonzero[3], counts[4], nonzero[4]);
    for c in cells.iter().filter(|c| c.count[2] != 0 || c.count[3] != 0).take(6) {
        let _ = writeln!(txt, "  cell ({},{}) {:#x}: ptr {:x?} count {:?}", c.row, c.col, c.addr, c.ptr, c.count);
        for k in 2..4 {
            if c.count[k] != 0 {
                if let Some(b) = r.bytes(c.ptr[k], 64) {
                    let _ = writeln!(txt, "    +{:x}: {}", 4 * k, b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
                }
            }
        }
    }

    // ---- objects and the OBJ
    let _ = writeln!(txt, "\n== objects (source addr model flags pos angles anim | parts exported) ==");
    let _ = writeln!(obj, "# Screamer Rally world export (SR_DUMP_WORLD): game world units, vertices in world coordinates");
    let mut vbase = 1u64;
    let mut stats: BTreeMap<&'static str, (u32, u64, u64)> = BTreeMap::new();
    let mut per_object: Vec<Placed> = Vec::new();
    let mut flags_seen: BTreeMap<u32, u32> = BTreeMap::new();
    let mut tris = Vec::new(); // (object, triangle in world)
    for (n, o) in objects.iter().enumerate() {
        *flags_seen.entry(o.flags).or_default() += 1;
        let Some(mo) = w.model(o.model) else {
            let _ = writeln!(txt, "{:?} {:#x}: model {} unknown", o.source, o.addr, o.model);
            continue;
        };
        let _ = writeln!(obj, "o {}_{}_m{}", n, match o.source {
            Source::Cell { row, col, list, index } => format!("c{row}_{col}_{list}_{index}"),
            Source::Scene { entry } => format!("s{entry}_{}", o.name),
        }, o.model);
        let mut np = 0u32;
        let mut bb = ([f64::MAX; 3], [f64::MIN; 3]);
        let mut desc = Vec::new();
        let mut all_vertices: Vec<[f64; 3]> = Vec::new();
        let mut first_name = String::new();
        for i in 0..mo.parts.min(LEVEL_SLOTS) {
            let Some((level, pa)) = best_level(mo, i as usize) else { continue };
            let Some(p) = w.part(pa) else { continue };
            let Some(xf) = w.part_xf(&mut r, o, i) else {
                desc.push(format!("{i}:noplace"));
                continue;
            };
            let Some(vs) = vertices(&mut r, mo.addr.wrapping_add(p.verts)) else { continue };
            let Some((polys, _)) = polygons(&mut r, p) else { continue };
            let _ = writeln!(obj, "g part{i}_{}_L{level}", p.name);
            let world: Vec<[f64; 3]> = vs.iter().map(|v| xf.apply([v[0] as f64, v[1] as f64, v[2] as f64])).collect();
            for v in &world {
                let _ = writeln!(obj, "v {:.3} {:.3} {:.3}", v[0], v[1], v[2]);
                for k in 0..3 {
                    bb.0[k] = bb.0[k].min(v[k]);
                    bb.1[k] = bb.1[k].max(v[k]);
                }
            }
            for &rec in &polys {
                let Some((_mat, idx)) = polygon(&mut r, rec) else { continue };
                let ids: Vec<u64> = idx.iter().map(|&o6| o6 as u64 / 6).collect();
                if ids.iter().any(|&k| k as usize >= world.len()) || idx.iter().any(|&o6| o6 % 6 != 0) {
                    continue;
                }
                let _ = writeln!(obj, "f {}", ids.iter().map(|k| (k + vbase).to_string()).collect::<Vec<_>>().join(" "));
                for t in 1..ids.len() - 1 {
                    tris.push((n as u32, [world[ids[0] as usize], world[ids[t] as usize], world[ids[t + 1] as usize]]));
                }
                np += 1;
            }
            vbase += world.len() as u64;
            desc.push(format!("{i}:{}L{level}", p.name));
            if first_name.is_empty() {
                first_name = p.name.clone();
            }
            all_vertices.extend_from_slice(&world);
        }
        let e = stats.entry(kind(o)).or_default();
        e.0 += 1;
        e.1 += np as u64;
        // the object's lowest vertices (y grows upwards): where it meets the ground
        let low: Vec<[f64; 3]> = all_vertices.iter().copied().filter(|v| v[1] <= bb.0[1] + 50.0).take(32).collect();
        per_object.push(Placed { n, polys: np, min: bb.0, max: bb.1, name: first_name, low });
        let _ = writeln!(txt, "{:?} {} {:#x} m{} {:#x} {:?} {:?} {} | {} polys | {}", o.source, o.name, o.addr, o.model, o.flags, o.pos, o.angles, o.anim, np, desc.join(" "));
    }
    let _ = writeln!(txt, "\n== totals by kind (objects, exported polygons) ==");
    for (k, v) in &stats {
        let _ = writeln!(txt, "{k:12} {:6} objects {:8} polygons", v.0, v.1);
    }
    let _ = writeln!(txt, "object flags (+4) seen: {:x?}", flags_seen);

    // ---- per-level totals over the world's objects (every object counted with its model)
    let mut lv_tot = [(0u64, 0u64, 0u64); 3];
    for o in objects.iter().filter(|o| matches!(o.source, Source::Cell { .. })) {
        if let Some(l) = model_polys.get(&o.model) {
            for k in 0..3 {
                lv_tot[k].0 += l[k].0 as u64;
                lv_tot[k].1 += l[k].1 as u64;
                lv_tot[k].2 += l[k].2 as u64;
            }
        }
    }
    let _ = writeln!(txt, "world objects, per level (parts / polygons / vertices): L0 {:?} L1 {:?} L2 {:?}", lv_tot[0], lv_tot[1], lv_tot[2]);
    // the world objects' shapes: parts per model, angles used, part placements
    let mut parts_hist: BTreeMap<u32, u32> = BTreeMap::new();
    let (mut pitch_roll, mut placed_parts, mut nonzero_place, mut shared_levels, mut models_seen) = (0u32, 0u32, 0u32, 0u32, BTreeSet::new());
    for o in objects.iter().filter(|o| matches!(o.source, Source::Cell { .. })) {
        if o.angles[0] & 0x7ff != 0 || o.angles[2] & 0x7ff != 0 {
            pitch_roll += 1;
        }
        let Some(mo) = w.model(o.model) else { continue };
        *parts_hist.entry(mo.parts).or_default() += 1;
        if !models_seen.insert(mo.index) {
            continue;
        }
        for i in 0..mo.parts.min(LEVEL_SLOTS) as usize {
            let part0 = mo.addr.wrapping_add(mo.levels[0][i]);
            let pl = r.u32(part0 + 0xc).map(|p| mo.addr.wrapping_add(p));
            let vals: Vec<i32> = pl.map(|p| (1..7).filter_map(|k| r.i32(p + 4 * k)).collect()).unwrap_or_default();
            placed_parts += 1;
            if vals.iter().any(|&v| v != 0) {
                nonzero_place += 1;
            }
            if mo.levels[0][i] != 0 && (mo.levels[0][i] == mo.levels[1][i] || mo.levels[1][i] == mo.levels[2][i]) {
                shared_levels += 1;
            }
        }
    }
    let _ = writeln!(
        txt,
        "world objects: parts per model {parts_hist:?}; objects with angle 0 or 2 (not the heading) non-zero: {pitch_roll}; distinct world models {}: {placed_parts} parts, {nonzero_place} with a non-zero part placement (angles / position), {shared_levels} with the same part data at two levels",
        models_seen.len()
    );

    analysis(&mut txt, &cfg.dir, &cells, &objects, &per_object, &tris);
    let _ = writeln!(txt, "\ndump took {:.1} s", t0.elapsed().as_secs_f64());
    std::fs::write(cfg.dir.join("world.txt"), &txt).map_err(|e| e.to_string())?;
    std::fs::write(cfg.dir.join("world.obj"), &obj).map_err(|e| e.to_string())?;
    crate::info!("world dump: {} objects, {} cells -> {}", objects.len(), cells.len(), cfg.dir.display());
    // keep the world for the verification
    VERIFY_WORLD.with(|v| *v.borrow_mut() = Some(w));
    Ok(objects)
}

thread_local! {
    static VERIFY_WORLD: std::cell::RefCell<Option<World>> = const { std::cell::RefCell::new(None) };
}

/// An exported object's footprint for the completeness analysis.
struct Placed {
    /// Index into the object list.
    n: usize,
    polys: u32,
    min: [f64; 3],
    max: [f64; 3],
    /// Its first part's name.
    name: String,
    /// Its lowest vertices (within 50 units of its lowest point).
    low: Vec<[f64; 3]>,
}

/// The name without digits (`a12vetta` -> `a#vetta`): the artists' object classes.
fn name_class(name: &str) -> String {
    let mut s = String::new();
    for ch in name.chars() {
        if ch.is_ascii_digit() {
            if !s.ends_with('#') {
                s.push('#');
            }
        } else {
            s.push(ch);
        }
    }
    s
}

/// Completeness: what lies beyond the track corridor, and whether objects have ground
/// under them in the full world (y grows upwards: a car's wheels are below its body).
fn analysis(txt: &mut String, dir: &std::path::Path, cells: &[Cell], objects: &[Object], placed: &[Placed], tris: &[(u32, [[f64; 3]; 3])]) {
    let _ = writeln!(txt, "\n== completeness ==");
    // track cells: cells with a visibility list
    let track: Vec<(i32, i32)> = cells.iter().filter(|c| c.count[4] > 0).map(|c| (c.row as i32, c.col as i32)).collect();
    let listed: BTreeSet<(i32, i32)> = cells
        .iter()
        .filter(|c| c.count[4] > 0 && c.count[4] < 4096)
        .flat_map(|c| (0..c.count[4]).filter_map(move |i| if mem::readable(c.ptr[4] + 4 * i, 4) { Some(unsafe { mem::rd32(c.ptr[4] + 4 * i) }) } else { None }))
        .map(|e| ((e >> 16) as i32, (e & 0xffff) as i32))
        .collect();
    let existing: BTreeSet<(i32, i32)> = cells.iter().map(|c| (c.row as i32, c.col as i32)).collect();
    let dist = |row: i32, col: i32| track.iter().map(|&(r, c)| (r - row).abs().max((c - col).abs())).min().unwrap_or(i32::MAX);
    let _ = writeln!(
        txt,
        "track cells (with a visibility list): {}; distinct cells named in the lists: {} ({} of them exist); cells in the grid: {} ({} with objects)",
        track.len(),
        listed.len(),
        listed.intersection(&existing).count(),
        cells.len(),
        cells.iter().filter(|c| c.count[0] + c.count[1] > 0).count()
    );
    // where an object sits against the cell that holds it
    let cell_of = |x: f64| ((x + 1048576.0) / 8192.0).floor() as i32;
    let mut origin_off: BTreeMap<i32, u32> = BTreeMap::new();
    for p in placed {
        let o = &objects[p.n];
        if let Source::Cell { row, col, .. } = o.source {
            let d = (cell_of(o.pos[2] as f64) - row as i32).abs().max((cell_of(o.pos[0] as f64) - col as i32).abs());
            *origin_off.entry(d).or_default() += 1;
        }
    }
    let _ = writeln!(txt, "object origin (x, z) against its cell (row = z cell, column = x cell): Chebyshev distance -> objects: {origin_off:?}");
    let mut bands: BTreeMap<i32, (u32, u64, u32, u64)> = BTreeMap::new();
    for p in placed {
        let o = &objects[p.n];
        let Source::Cell { row, col, .. } = o.source else { continue };
        let d = dist(row as i32, col as i32);
        let e = bands.entry(d.min(99)).or_default();
        e.0 += 1;
        e.1 += p.polys as u64;
        if listed.contains(&(row as i32, col as i32)) {
            e.2 += 1;
            e.3 += p.polys as u64;
        }
    }
    let _ = writeln!(txt, "world objects by Chebyshev distance (cells) of their cell from the nearest track cell: objects / polygons (of which in cells named by some list: objects / polygons)");
    for (d, v) in &bands {
        let _ = writeln!(txt, "  {d:2}: {:5} / {:7}   ({:5} / {:7})", v.0, v.1, v.2, v.3);
    }
    // map of the grid
    let (mut r0, mut r1, mut c0, mut c1) = (255u16, 0u16, 255u16, 0u16);
    for c in cells {
        r0 = r0.min(c.row);
        r1 = r1.max(c.row);
        c0 = c0.min(c.col);
        c1 = c1.max(c.col);
    }
    let cellset: BTreeMap<(u16, u16), &Cell> = cells.iter().map(|c| ((c.row, c.col), c)).collect();
    let _ = writeln!(txt, "grid map rows {r0}..{r1} (down), columns {c0}..{c1} (right): T track cell, L cell named by a list, o cell with objects named by no list, . empty cell named by no list, ' ' no cell");
    for row in r0..=r1 {
        let mut line = String::new();
        for col in c0..=c1 {
            let ch = match cellset.get(&(row, col)) {
                None => ' ',
                Some(c) if c.count[4] > 0 => 'T',
                Some(_) if listed.contains(&(row as i32, col as i32)) => 'L',
                Some(c) if c.count[0] + c.count[1] > 0 => 'o',
                Some(_) => '.',
            };
            line.push(ch);
        }
        let _ = writeln!(txt, "  {row:3} {line}");
    }

    // ground under objects: at each of an object's lowest vertices, the surfaces of other
    // objects there (triangles whose xz footprint contains the point)
    let mut buckets: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    for (k, (_, t)) in tris.iter().enumerate() {
        let (mut x0, mut x1, mut z0, mut z1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for v in t {
            x0 = x0.min(v[0]);
            x1 = x1.max(v[0]);
            z0 = z0.min(v[2]);
            z1 = z1.max(v[2]);
        }
        for cz in cell_of(z0)..=cell_of(z1).min(cell_of(z0) + 64) {
            for cx in cell_of(x0)..=cell_of(x1).min(cell_of(x0) + 64) {
                buckets.entry((cz, cx)).or_default().push(k);
            }
        }
    }
    let height_at = |t: &[[f64; 3]; 3], x: f64, z: f64| -> Option<f64> {
        let (a, b, c) = (t[0], t[1], t[2]);
        let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
        if d.abs() < 1e-9 {
            return None;
        }
        let l1 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / d;
        let l2 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / d;
        let l3 = 1.0 - l1 - l2;
        (l1 >= -1e-6 && l2 >= -1e-6 && l3 >= -1e-6).then(|| l1 * a[1] + l2 * b[1] + l3 * c[1])
    };
    // per object: share of its lowest vertices with another object's surface within 1000
    // units above or below (ground), and with any surface below at all
    #[derive(Default, Clone, Copy)]
    struct G {
        objects: u32,
        grounded: u32,
        partly: u32,
        floating: u32,
        nothing_below: u32,
        void: u32,
    }
    let mut by_band: BTreeMap<i32, G> = BTreeMap::new();
    let mut by_class: BTreeMap<String, G> = BTreeMap::new();
    let mut floating: Vec<(i32, usize, f64)> = Vec::new();
    let mut void: Vec<([f64; 3], [f64; 3])> = Vec::new();
    let mut void_polys = 0u64;
    let mut float_boxes: Vec<([f64; 3], [f64; 3])> = Vec::new();
    for (pi, p) in placed.iter().enumerate() {
        let o = &objects[p.n];
        let Source::Cell { row, col, .. } = o.source else { continue };
        if p.polys == 0 || p.low.is_empty() {
            continue;
        }
        let (mut near, mut below_any, mut covered) = (0usize, 0usize, 0usize);
        for v in &p.low {
            let (mut n_ok, mut b_ok, mut c_ok) = (false, false, false);
            if let Some(list) = buckets.get(&(cell_of(v[2]), cell_of(v[0]))) {
                for &k in list {
                    if tris[k].0 == p.n as u32 {
                        continue;
                    }
                    if let Some(h) = height_at(&tris[k].1, v[0], v[2]) {
                        c_ok = true;
                        if (h - v[1]).abs() <= 1000.0 {
                            n_ok = true;
                        }
                        if h <= v[1] + 50.0 {
                            b_ok = true;
                        }
                    }
                }
            }
            near += n_ok as usize;
            below_any += b_ok as usize;
            covered += c_ok as usize;
        }
        let d = dist(row as i32, col as i32).min(99);
        for g in [by_band.entry(d).or_default(), by_class.entry(name_class(&p.name)).or_default()] {
            g.objects += 1;
            if near == p.low.len() {
                g.grounded += 1;
            } else if near > 0 {
                g.partly += 1;
            } else {
                g.floating += 1;
            }
            if below_any == 0 {
                g.nothing_below += 1;
            }
            if covered == 0 {
                g.void += 1;
            }
        }
        if near == 0 {
            floating.push((d, pi, p.max[1] - p.min[1]));
            float_boxes.push((p.min, p.max));
        }
        if covered == 0 {
            void.push((p.min, p.max));
            void_polys += p.polys as u64;
        }
    }
    let _ = writeln!(txt, "objects with no surface of another object above or below any of their lowest vertices at all (standing over the void): {} with {void_polys} polygons", void.len());
    let _ = writeln!(
        txt,
        "ground test (y up): at each object's lowest vertices, is there a surface of another object within 1000 units above or below? objects / all lowest vertices grounded / some / none (floating) / no surface at or below any of them / no surface above or below any of them (void)"
    );
    for (d, g) in &by_band {
        let _ = writeln!(txt, "  distance {d:2}: {:5} / {:5} / {:5} / {:5} / {:5} / {:5}", g.objects, g.grounded, g.partly, g.floating, g.nothing_below, g.void);
    }
    let _ = writeln!(txt, "  by name class (objects / grounded / some / floating / nothing below / void):");
    let mut classes: Vec<(&String, &G)> = by_class.iter().collect();
    classes.sort_by(|a, b| b.1.objects.cmp(&a.1.objects));
    for (c, g) in classes {
        let _ = writeln!(txt, "    {c:12} {:5} / {:5} / {:5} / {:5} / {:5} / {:5}", g.objects, g.grounded, g.partly, g.floating, g.nothing_below, g.void);
    }
    floating.sort_by(|a, b| b.2.total_cmp(&a.2));
    let _ = writeln!(txt, "  floating objects (no surface of another object within 1000 units of any lowest vertex), tallest first:");
    for (d, pi, h) in floating.iter().take(60) {
        let p = &placed[*pi];
        let o = &objects[p.n];
        let _ = writeln!(txt, "    d {d} {:?} {} m{} {} polys, height {:.0}, bbox ({:.0},{:.0},{:.0})..({:.0},{:.0},{:.0})", o.source, p.name, o.model, p.polys, h, p.min[0], p.min[1], p.min[2], p.max[0], p.max[1], p.max[2]);
    }
    height_image(txt, dir, cells, &track, tris, objects, &float_boxes);
}

/// A top view of the world's polygons (`world_top.bmp`): colour = height of the highest
/// surface (blue low .. red high), black = no polygon; track cells outlined in white, other
/// cells of the grid in grey.
fn height_image(txt: &mut String, dir: &std::path::Path, cells: &[Cell], track: &[(i32, i32)], tris: &[(u32, [[f64; 3]; 3])], objects: &[Object], void: &[([f64; 3], [f64; 3])]) {
    let world: Vec<&[[f64; 3]; 3]> = tris.iter().filter(|(n, _)| matches!(objects[*n as usize].source, Source::Cell { .. })).map(|(_, t)| t).collect();
    if world.is_empty() {
        return;
    }
    let (mut x0, mut x1, mut z0, mut z1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for t in &world {
        for v in t.iter() {
            x0 = x0.min(v[0]);
            x1 = x1.max(v[0]);
            z0 = z0.min(v[2]);
            z1 = z1.max(v[2]);
            y0 = y0.min(v[1]);
            y1 = y1.max(v[1]);
        }
    }
    // whole cells around the geometry
    let snap0 = |v: f64| ((v + 1048576.0) / 8192.0).floor() * 8192.0 - 1048576.0;
    let snap1 = |v: f64| ((v + 1048576.0) / 8192.0).ceil() * 8192.0 - 1048576.0;
    let (x0, x1, z0, z1) = (snap0(x0), snap1(x1), snap0(z0), snap1(z1));
    let scale = 128.0; // world units per pixel (64 pixels per cell)
    let (w, h) = (((x1 - x0) / scale).ceil() as usize, ((z1 - z0) / scale).ceil() as usize);
    let mut top = vec![f64::NAN; w * h];
    for t in &world {
        let px = |v: &[f64; 3]| ((v[0] - x0) / scale, (v[2] - z0) / scale);
        let (a, b, c) = (px(&t[0]), px(&t[1]), px(&t[2]));
        let d = (b.1 - c.1) * (a.0 - c.0) + (c.0 - b.0) * (a.1 - c.1);
        if d.abs() < 1e-12 {
            continue;
        }
        let (bx0, bx1) = (a.0.min(b.0).min(c.0).floor().max(0.0) as usize, (a.0.max(b.0).max(c.0).ceil() as usize).min(w - 1));
        let (by0, by1) = (a.1.min(b.1).min(c.1).floor().max(0.0) as usize, (a.1.max(b.1).max(c.1).ceil() as usize).min(h - 1));
        for yy in by0..=by1 {
            for xx in bx0..=bx1 {
                let (x, z) = (xx as f64 + 0.5, yy as f64 + 0.5);
                let l1 = ((b.1 - c.1) * (x - c.0) + (c.0 - b.0) * (z - c.1)) / d;
                let l2 = ((c.1 - a.1) * (x - c.0) + (a.0 - c.0) * (z - c.1)) / d;
                let l3 = 1.0 - l1 - l2;
                if l1 >= 0.0 && l2 >= 0.0 && l3 >= 0.0 {
                    let y = l1 * t[0][1] + l2 * t[1][1] + l3 * t[2][1];
                    let k = yy * w + xx;
                    if top[k].is_nan() || y > top[k] {
                        top[k] = y;
                    }
                }
            }
        }
    }
    let covered = top.iter().filter(|v| !v.is_nan()).count();
    let mut px = vec![[0u8; 3]; w * h];
    for (k, &y) in top.iter().enumerate() {
        if !y.is_nan() {
            let f = ((y - y0) / (y1 - y0).max(1.0)).clamp(0.0, 1.0);
            // blue -> green -> red
            let (r, g, b) = if f < 0.5 { (0.0, f * 2.0, 1.0 - f * 2.0) } else { ((f - 0.5) * 2.0, 1.0 - (f - 0.5) * 2.0, 0.0) };
            px[k] = [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8];
        }
    }
    // cell outlines
    let trackset: BTreeSet<(i32, i32)> = track.iter().copied().collect();
    let cell_px = (8192.0 / scale) as usize;
    for pass in 0..2 {
        for c in cells {
            let is_track = trackset.contains(&(c.row as i32, c.col as i32));
            if (pass == 1) != is_track {
                continue;
            }
            let cx = ((c.col as f64 * 8192.0 - 1048576.0 - x0) / scale) as i64;
            let cz = ((c.row as f64 * 8192.0 - 1048576.0 - z0) / scale) as i64;
            let colour = if is_track { [255, 255, 255] } else { [90, 90, 90] };
            for i in 0..cell_px as i64 {
                for (x, z) in [(cx + i, cz), (cx, cz + i)] {
                    if x >= 0 && z >= 0 && (x as usize) < w && (z as usize) < h {
                        px[z as usize * w + x as usize] = colour;
                    }
                }
            }
        }
    }
    // steep polygons (vertical panels, walls: no area seen from above) in yellow
    let mut steep = 0usize;
    for t in &world {
        let (u, v) = ([t[1][0] - t[0][0], t[1][1] - t[0][1], t[1][2] - t[0][2]], [t[2][0] - t[0][0], t[2][1] - t[0][1], t[2][2] - t[0][2]]);
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len == 0.0 || n[1].abs() / len > 0.2 {
            continue;
        }
        steep += 1;
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let steps = (((b[0] - a[0]).abs().max((b[2] - a[2]).abs())) / scale).ceil().max(1.0) as usize;
            for s in 0..=steps {
                let f = s as f64 / steps as f64;
                let (x, z) = (((a[0] + (b[0] - a[0]) * f - x0) / scale) as i64, ((a[2] + (b[2] - a[2]) * f - z0) / scale) as i64);
                if x >= 0 && z >= 0 && (x as usize) < w && (z as usize) < h {
                    px[z as usize * w + x as usize] = [255, 220, 0];
                }
            }
        }
    }
    let _ = writeln!(txt, "steep world polygons (|normal y| < 0.2, drawn as yellow lines in the top view): {steep} triangles");
    // floating objects (no other surface within 1000 units of their lowest vertices): magenta footprints
    for (mn, mx) in void {
        let (ax, az) = (((mn[0] - x0) / scale) as i64, ((mn[2] - z0) / scale) as i64);
        let (bx, bz) = (((mx[0] - x0) / scale) as i64, ((mx[2] - z0) / scale) as i64);
        let mut put = |x: i64, z: i64| {
            if x >= 0 && z >= 0 && (x as usize) < w && (z as usize) < h {
                px[z as usize * w + x as usize] = [255, 0, 255];
            }
        };
        for x in ax..=bx {
            put(x, az);
            put(x, bz);
        }
        for z in az..=bz {
            put(ax, z);
            put(bx, z);
        }
    }
    let path = dir.join("world_top.bmp");
    let _ = write_bmp(&path, w, h, &px);
    let _ = writeln!(
        txt,
        "top view {}: {w} x {h} pixels of {scale} units (x right, z down), heights {y0:.0} (blue) .. {y1:.0} (red), black = no polygon, white / grey = track / other cells, yellow = steep polygons, magenta = footprints of floating objects; {:.1} % of the cells' rectangle covered by world polygons",
        path.display(),
        100.0 * covered as f64 / (w * h) as f64
    );
}

fn write_bmp(path: &std::path::Path, w: usize, h: usize, px: &[[u8; 3]]) -> std::io::Result<()> {
    let row = (w * 3 + 3) & !3;
    let size = 54 + row * h;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(-(h as i32)).to_le_bytes()); // top-down rows
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    for y in 0..h {
        for x in 0..w {
            let [r, g, bl] = px[y * w + x];
            b.extend_from_slice(&[bl, g, r]);
        }
        b.resize(b.len() + (row - w * 3), 0);
    }
    std::fs::write(path, b)
}

// ---------------------------------------------------------------------------------------

fn after_transform(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    if m.world.verify.is_some() {
        verify_part(m);
    }
    HookResult::Resume(tramp)
}

fn verify_part(m: &mut Machine) {
    let link = |a: u32| m.img.link(a);
    if unsafe { mem::rd32(link(VARIANT)) } & 1 != 0 {
        return; // rear-view mirror
    }
    let (inst, model_addr, part, verts, slot) = unsafe {
        (
            mem::rd32(link(CUR_INSTANCE)) & !0x4000_0000,
            mem::rd32(link(CUR_MODEL)),
            mem::rd32(link(CUR_PART)),
            mem::rd32(link(CUR_VERTS)),
            mem::rd32(link(CUR_SLOT)),
        )
    };
    let index = unsafe { mem::rd16(link(PART_ORDER) + 4 * slot) } as u32;
    let cam = unsafe { mem::rd32(link(CAMERA_PTR)) };
    let campos = unsafe { [mem::rd32(link(CAMERA_POS)) as i32 as f64, mem::rd32(link(CAMERA_POS) + 4) as i32 as f64, mem::rd32(link(CAMERA_POS) + 8) as i32 as f64] };
    let mut ct = [0f64; 9];
    for (k, c) in ct.iter_mut().enumerate() {
        *c = unsafe { mem::rd32(cam + 0x48 + 4 * k as u32) } as i32 as f64 / 16384.0;
    }
    let view = Xf { r: ct, t: [0.0; 3] };
    let mut r = Reader::default();
    let v = m.world.verify.as_mut().unwrap();
    let Some(o) = v.objects.get(&inst).cloned() else {
        v.unknown += 1;
        if v.unknown <= 20 {
            v.lines.push(format!("unknown instance {inst:#x} (model {model_addr:#x}) part {index}"));
        }
        return;
    };
    VERIFY_WORLD.with(|cell| {
        let mut wb = cell.borrow_mut();
        let Some(w) = wb.as_mut() else { return };
        // billboards follow the camera of this frame
        w.camera_angles = unsafe { [mem::rd32(cam) as i32, mem::rd32(cam + 4) as i32, mem::rd32(cam + 8) as i32] };
        w.clock = unsafe { mem::rd32(link(ANIM_CLOCK)) } as i32;
        // the instance as it is now (cars move)
        let Some(now) = read_object(&mut r, o.addr, o.source, o.name.clone(), o.parts_b) else { return };
        for (field, same) in [("position", now.pos == o.pos), ("angles", now.angles == o.angles), ("model", now.model == o.model), ("flags", now.flags == o.flags)] {
            if !same {
                v.changed.insert(o.addr);
                v.changed_fields.insert((field, o.addr), (format!("{:x?}", match field {
                    "position" => o.pos.to_vec(),
                    "angles" => o.angles.to_vec(),
                    "model" => vec![o.model as i32],
                    _ => vec![o.flags as i32],
                }), format!("{:x?}", match field {
                    "position" => now.pos.to_vec(),
                    "angles" => now.angles.to_vec(),
                    "model" => vec![now.model as i32],
                    _ => vec![now.flags as i32],
                })));
            }
        }
        let o = now;
        let Some(xf) = w.part_xf(&mut r, &o, index) else {
            v.lines.push(format!("{:?} part {index}: no placement", o.source));
            return;
        };
        let Some(vs) = vertices(&mut r, verts) else { return };
        let mut worst = (0.0f64, 0.0f64, 0usize);
        let mut sum = 0.0;
        for (n, vert) in vs.iter().enumerate() {
            let wpos = xf.apply([vert[0] as f64, vert[1] as f64, vert[2] as f64]);
            let mine = view.apply([wpos[0] - campos[0], wpos[1] - campos[1], wpos[2] - campos[2]]);
            let p = link(VIEW_POS) + 12 * n as u32;
            let eng = unsafe { [mem::rd32(p) as i32 as f64, mem::rd32(p + 4) as i32 as f64, mem::rd32(p + 8) as i32 as f64] };
            let raw = (0..3).map(|k| (mine[k] - eng[k]).abs()).fold(0.0, f64::max);
            // the engine's integer matrix product (79638) overflows 32 bits when a part's
            // view translation is large (|r . t| >= 2^31 / 2^14): its result is then off by
            // a multiple of 2^32 >> 14 = 2^18; counted apart, compared modulo 2^18
            const WRAP: f64 = 262144.0;
            let err = (0..3).map(|k| {
                let d = mine[k] - eng[k];
                (d - (d / WRAP).round() * WRAP).abs()
            }).fold(0.0, f64::max);
            if raw > 1000.0 && err < 1000.0 {
                v.wrapped += 1;
                v.wrapped_min_dist = v.wrapped_min_dist.min((mine[0] * mine[0] + mine[1] * mine[1] + mine[2] * mine[2]).sqrt());
            }
            let d = (eng[0] * eng[0] + eng[1] * eng[1] + eng[2] * eng[2]).sqrt().max(1.0);
            sum += err;
            if err > worst.0 {
                worst = (err, d, n);
            }
            v.worst = v.worst.max(err);
            v.worst_rel = v.worst_rel.max(err / d);
            let band = (d.log10().floor().max(0.0)) as u32;
            let b = v.by_distance.entry(band).or_default();
            b.0 += 1;
            b.1 = b.1.max(err);
        }
        v.seen.insert((o.addr, index));
        v.parts += 1;
        v.vertices += vs.len() as u64;
        let e = v.by_source.entry(o.source_name()).or_default();
        e.0 += 1;
        e.1 += vs.len() as u64;
        e.2 = e.2.max(worst.0);
        e.3 = e.3.max(worst.0 / worst.1);
        if v.lines.len() < 400 {
            v.lines.push(format!(
                "{:?} {} m{} part {index} ({:#x}): {} vertices, max error {:.2} at view distance {:.0} (vertex {}), mean {:.2}",
                o.source,
                o.name,
                o.model,
                part,
                vs.len(),
                worst.0,
                worst.1,
                worst.2,
                sum / vs.len().max(1) as f64
            ));
        }
    });
}

fn finish_verify(cfg: &Config, v: &Verify) {
    let mut s = String::new();
    let _ = writeln!(s, "verification: {} parts, {} vertices compared, {} parts of instances not in the export", v.parts, v.vertices, v.unknown);
    let _ = writeln!(s, "max |error| {:.3} units (largest per-axis difference of a vertex, modulo 2^18), max relative error {:.6} (error / view distance)", v.worst, v.worst_rel);
    let _ = writeln!(
        s,
        "vertices the engine misplaced by a multiple of 2^18 (its 32-bit overflow in 79638): {}{}",
        v.wrapped,
        if v.wrapped > 0 { format!(", the nearest at a true view distance of {:.0}", v.wrapped_min_dist) } else { String::new() }
    );
    for (k, e) in &v.by_source {
        let _ = writeln!(s, "  {k:12}: {} parts, {} vertices, max error {:.3}, max relative {:.6}", e.0, e.1, e.2, e.3);
    }
    for (b, e) in &v.by_distance {
        let _ = writeln!(s, "  view distance {:>7} .. {:>7}: {} vertices, max error {:.3}", 10u64.pow(*b), 10u64.pow(*b + 1), e.0, e.1);
    }
    let world_objects: BTreeSet<u32> = v.seen.iter().map(|s| s.0).filter(|a| v.objects.get(a).is_some_and(|o| matches!(o.source, Source::Cell { .. }))).collect();
    let _ = writeln!(s, "distinct (object, part) pairs compared: {}; world objects compared: {} of {}", v.seen.len(), world_objects.len(), v.world_objects);
    let mut changed: BTreeMap<String, u32> = BTreeMap::new();
    for a in &v.changed {
        if let Some(o) = v.objects.get(a) {
            *changed.entry(format!("{} {}", o.source_name(), o.name)).or_default() += 1;
        }
    }
    let _ = writeln!(s, "objects drawn with a position, angles, model or flags different from the dump's: {} {:?}", v.changed.len(), changed);
    let mut by_field: BTreeMap<(&str, &str), (u32, Vec<String>)> = BTreeMap::new();
    for ((field, a), (was, now)) in &v.changed_fields {
        let src = v.objects.get(a).map_or("?", |o| o.source_name());
        let e = by_field.entry((src, field)).or_default();
        e.0 += 1;
        if e.1.len() < 4 {
            e.1.push(format!("{a:#x}: {was} -> {now}"));
        }
    }
    for ((src, field), (n, ex)) in &by_field {
        let _ = writeln!(s, "  {src} {field}: {n} objects, e.g. {}", ex.join("; "));
    }
    for l in &v.lines {
        let _ = writeln!(s, "{l}");
    }
    let _ = std::fs::write(cfg.dir.join("verify.txt"), s);
    crate::info!("world dump: verification of {} parts written", v.parts);
}
