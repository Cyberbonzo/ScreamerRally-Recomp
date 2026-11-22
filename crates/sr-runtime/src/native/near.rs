//! Near-camera fidelity (wave 3 topic W3-2, `docs/re/native/near.md`): what the engine does
//! with world polygons next to its near plane, and the diagnostic that compares it, polygon
//! by polygon, with what the native renderer draws.
//!
//! The engine clips at view z' = 0 (the contract's `near` = 128 from the eye), as native
//! does; the near-camera differences of wave 2 were the renderer losing polygons that reach
//! behind the eye (fixed in `sr-render`'s `native.hlsl`, the clip-space depth).
//!
//! `SR_NEAR_CHECK=FIRST,LAST,DIR[,DUMP...]` (diagnostics, off by default; run with the
//! native renderer off, `SR_NATIVE=0`, so that the engine draws the world): for every world
//! part of the main view in game frames FIRST..=LAST it reads the polygon list the engine's
//! list builder (`7ac50`) made, the near clipper's output, the engine's view and screen
//! positions, and computes the same polygons as the native renderer sees them (the scene's
//! float transform, clipped at the near plane). Per frame one line goes to
//! `DIR/near_summary.tsv`; for the game frames listed after DIR every polygon of the parts
//! that reach the near plane goes to `DIR/near_<frame>.tsv`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Mutex;

use super::transforms::{self, Affine, BEHIND, EYE, FOCAL};
use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

// ---------------------------------------------------------------------------------------
// Link addresses (docs/re/native/near.md)

/// The polygon list drawer's loop head (`7ba2d`: `cld; mov ebx, [esi]; add esi, 4`); with
/// esi = the list's base the list builder has just finished (`7ba0c` jumps here first).
const LIST_LOOP: u32 = 0x7ba2d;
/// The part's polygon list (pairs: material offset | variant, record or clip buffer).
const POLY_LIST: u32 = 0x67088;
/// The near-plane clippers (variant 8: textured, flat, reflection) and the far ones
/// (variant C); ebp = the record, [7050c] = where their output starts.
const NEAR_CLIPPERS: [u32; 3] = [0xced48, 0xcfaf0, 0xd4de8];
const FAR_CLIPPERS: [u32; 3] = [0xcf0d4, 0xcfcbc, 0xd50f0];
const CLIP_OUT: u32 = 0x7050c;
/// The object being drawn: instance (bit 30 = scene-table instance), model, draw record,
/// level data, vertex list.
const CUR_INSTANCE: u32 = 0x7bb8c;
const CUR_MODEL: u32 = 0x7bab4;
const CUR_SLOT: u32 = 0x7babc;
const CUR_RECORD: u32 = 0x7bac0;
const CUR_PART: u32 = 0x7bac4;
const CUR_VERTS: u32 = 0x7bacc;
const PART_ORDER: u32 = 0x81d44;
const CAMERA_PTR: u32 = 0x81e0c;
const VIEW_CAMERAS: u32 = 0x1296bc;
const VARIANT: u32 = 0xaa5b0;
const VIEW_POS: u32 = 0x68fc8;
const PACKED_XY: u32 = 0x6ed94;
const CENTER_IMM: u32 = 0x7ab1a;
const CENTER_Y: u32 = 0x81e3c;
const VIEW_W: u32 = 0x81e50;
const VIEW_H: u32 = 0x81e24;
/// -D (set_far).
const FAR_NEG: u32 = 0xcf470;
/// The list builder's back-face point (the view origin in model coordinates, measured),
/// patched into the three paths' `sub r, imm32` (7ac50: path 1 at 7ada2, path 2 at 7ae56,
/// path 3 at 7b116).
const CULL_POINT: [[u32; 3]; 3] = [[0x7ada3, 0x7ada9, 0x7adaf], [0x7ae57, 0x7ae5d, 0x7ae63], [0x7b117, 0x7b11d, 0x7b123]];
/// The near path's side test table (indexed by negative values: `[aa3a4 + 4 k]`, k < 0).
const SIDE_TABLE: u32 = 0xaa3a4;

// ---------------------------------------------------------------------------------------
// The engine's decisions, as decoded (docs/re/native/near.md, "The engine")

/// Which of the list builder's three paths a part takes (`7ad1c`..`7ad46`): from the view
/// z of the part's origin (`rec+144h`) and its radius (level data +0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// The part's sphere stays more than 8 units in front of the near plane: back faces only.
    Plain,
    /// The sphere reaches z' >= -8: back faces, the side test, the near clipper.
    Near,
    /// The sphere reaches beyond the far distance: back faces, the far clipper.
    Far,
}

pub fn path(origin_z: i32, radius: i32, far_neg: i32) -> Path {
    if origin_z.wrapping_add(radius) >= -8 {
        Path::Near
    } else if origin_z.wrapping_sub(radius) <= far_neg {
        Path::Far
    } else {
        Path::Plain
    }
}

/// The near path's side test (`7ae87`..`7aff2`) for one polygon: kept when some vertex lies
/// in the "x wedge" or the "y wedge": `z' - d < 0` and `|x'| <= T[z' - d] + d` (x wedge) or
/// the same with `|y'|`, d = record +20h. The game tests the fourth vertex's x twice (its y
/// test reads x: `gs:[ebx]` at 7afdb instead of `gs:[ebx + 4]`). `table(k)` for k < 0.
pub fn side_test(view: &[[i32; 3]], d: i32, table: &dyn Fn(i32) -> i32) -> bool {
    let n = view.len().min(4);
    let inside = |v: &[i32; 3], c: i32| {
        let k = v[2].wrapping_sub(d);
        k < 0 && table(k).wrapping_add(d) >= c.wrapping_abs()
    };
    // x of the (up to) four vertex slots, then y of the first three, then x of the fourth
    (0..4).any(|i| inside(&view[i % n], view[i % n][0])) || (0..3).any(|i| inside(&view[i % n], view[i % n][1])) || inside(&view[3 % n], view[3 % n][0])
}

/// What the near path does with a polygon that passed the back-face and side tests
/// (`7b010`..`7b0a5`): every vertex z' < 0: drawn as it is; every vertex z' >= 0: dropped;
/// mixed: the near clipper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NearClass {
    Front,
    Behind,
    Crossing,
}

pub fn near_class(z: &[i32]) -> NearClass {
    let behind = z.iter().filter(|&&z| z >= 0).count();
    if behind == 0 {
        NearClass::Front
    } else if behind == z.len() {
        NearClass::Behind
    } else {
        NearClass::Crossing
    }
}

// ---------------------------------------------------------------------------------------
// Screen-space helpers (grid pixels)

/// Area of a polygon (shoelace, positive for counter-clockwise in y-up terms).
pub fn signed_area(p: &[(f64, f64)]) -> f64 {
    let n = p.len();
    if n < 3 {
        return 0.0;
    }
    (0..n).map(|i| p[i].0 * p[(i + 1) % n].1 - p[(i + 1) % n].0 * p[i].1).sum::<f64>() / 2.0
}

/// Clip a polygon by the half-plane `a x + b y + c >= 0`.
fn clip_half(p: &[(f64, f64)], a: f64, b: f64, c: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(p.len() + 2);
    let n = p.len();
    for i in 0..n {
        let (s, e) = (p[i], p[(i + 1) % n]);
        let (ds, de) = (a * s.0 + b * s.1 + c, a * e.0 + b * e.1 + c);
        if ds >= 0.0 {
            out.push(s);
        }
        if (ds >= 0.0) != (de >= 0.0) {
            let t = ds / (ds - de);
            out.push((s.0 + t * (e.0 - s.0), s.1 + t * (e.1 - s.1)));
        }
    }
    out
}

/// A polygon clipped to the rectangle `[x0, x1] x [y0, y1]`.
pub fn clip_rect(p: &[(f64, f64)], r: [f64; 4]) -> Vec<(f64, f64)> {
    let mut q = p.to_vec();
    for (a, b, c) in [(1.0, 0.0, -r[0]), (-1.0, 0.0, r[2]), (0.0, 1.0, -r[1]), (0.0, -1.0, r[3])] {
        if q.len() < 3 {
            return Vec::new();
        }
        q = clip_half(&q, a, b, c);
    }
    if q.len() < 3 { Vec::new() } else { q }
}

/// The part of `p` inside the convex polygon `k` (either winding).
pub fn clip_convex(p: &[(f64, f64)], k: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let s = signed_area(k).signum();
    if s == 0.0 {
        return Vec::new();
    }
    let mut q = p.to_vec();
    for i in 0..k.len() {
        if q.len() < 3 {
            return Vec::new();
        }
        let (a, b) = (k[i], k[(i + 1) % k.len()]);
        // inside = left of a -> b for a positive (counter-clockwise) k
        let (nx, ny) = (-(b.1 - a.1) * s, (b.0 - a.0) * s);
        q = clip_half(&q, nx, ny, -(nx * a.0 + ny * a.1));
    }
    q
}

/// A polygon given in the game's view space (x left, y up, z back) clipped at the near plane
/// z' <= 0 and projected to the grid (centre, cy, focal 256): what the native renderer
/// draws of it (before the screen edges).
pub fn native_screen(view: &[[f64; 3]], centre: f64, cy: f64) -> Vec<(f64, f64)> {
    let n = view.len();
    let mut q: Vec<[f64; 3]> = Vec::with_capacity(n + 2);
    for i in 0..n {
        let (s, e) = (view[i], view[(i + 1) % n]);
        if s[2] <= 0.0 {
            q.push(s);
        }
        if (s[2] <= 0.0) != (e[2] <= 0.0) {
            let t = s[2] / (s[2] - e[2]);
            q.push([s[0] + t * (e[0] - s[0]), s[1] + t * (e[1] - s[1]), 0.0]);
        }
    }
    q.iter().map(|v| (centre + FOCAL * v[0] / (v[2] - EYE), cy + FOCAL * v[1] / (v[2] - EYE))).collect()
}

/// The side of the eye (view (0, 0, 128)) the polygon's front is on: true when the eye sees
/// its front, i.e. the native renderer draws it (front faces are clockwise on screen; in the
/// game's view space the record normal points away from the visible side).
pub fn eye_sees_front(view: &[[f64; 3]]) -> bool {
    let (a, b, c) = (view[0], view[1], view[2]);
    let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
    // (v1 - v0) x (v2 - v0) is opposite to the record normal; the eye sees the front when
    // it lies on that cross product's side
    (0.0 - a[0]) * n[0] + (0.0 - a[1]) * n[1] + (EYE - a[2]) * n[2] > 0.0
}

// ---------------------------------------------------------------------------------------
// The check

struct Cfg {
    first: u64,
    last: u64,
    dir: std::path::PathBuf,
    dump: Vec<u64>,
}

fn cfg() -> Option<&'static Cfg> {
    static C: std::sync::OnceLock<Option<Cfg>> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        let v = std::env::var("SR_NEAR_CHECK").ok()?;
        let mut it = v.split(',').map(str::trim);
        let first = it.next()?.parse().ok()?;
        let last = it.next()?.parse().ok()?;
        let dir = std::path::PathBuf::from(it.next()?);
        let dump = it.filter_map(|f| f.parse().ok()).collect();
        Some(Cfg { first, last, dir, dump })
    })
    .as_ref()
}

#[derive(Default)]
struct FrameStats {
    parts: [u64; 3],
    /// Near-path polygons by the engine's decision: kept, clipped, clipper rejected, back
    /// face, side test, all behind.
    decisions: [u64; 6],
    /// Grid px^2 on screen: native's area, the engine's, their overlap; polygons drawn only
    /// by native (with the engine's reason) and only by the engine.
    native_area: f64,
    game_area: f64,
    overlap: f64,
    native_only: [f64; 6],
    game_only: [f64; 6],
    /// Vertices in front of the near plane whose packed position overflowed (BEHIND) or
    /// whose y wrapped.
    overflow_x: u64,
    wrap_y: u64,
    /// Polygons the engine drew with such a vertex, and their area drawn where native
    /// draws nothing (grid px^2).
    overflow_polys: u64,
    overflow_game_only: f64,
    /// The back-face point of near-path parts in view space (game axes): sum and extremes
    /// of its z, count (the view origin is z' = 0, the eye z' = 128).
    cull_z: (f64, f64, f64, u64),
    cull_xy_max: f64,
    /// Polygons native draws (on screen) with a vertex behind the eye (z' >= 128, the
    /// renderer's view z <= 0), and their area on screen.
    behind_eye: u64,
    behind_eye_area: f64,
    /// Predicted decisions that differ from the engine's (the decoding's check).
    mispredicted: u64,
    first_bad: Vec<String>,
}

#[derive(Default)]
struct State {
    /// Clip buffer start -> (record, far clipper).
    clipped: HashMap<u32, (u32, bool)>,
    frame: u64,
    stats: FrameStats,
    summary_header: bool,
    dump: String,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn rd(a: u32) -> u32 {
    unsafe { mem::rd32(a) }
}
fn rdi(a: u32) -> i32 {
    rd(a) as i32
}
fn rd16i(a: u32) -> i32 {
    unsafe { mem::rd16(a) as i16 as i32 }
}

pub fn install(m: &mut Machine) {
    let Some(c) = cfg() else { return };
    *STATE.lock().unwrap() = Some(State::default());
    let _ = std::fs::create_dir_all(&c.dir);
    let mut failed = Vec::new();
    if let Err(e) = m.install_observer("near check: list built", LIST_LOOP, list_loop) {
        failed.push(e);
    }
    for at in NEAR_CLIPPERS {
        if let Err(e) = m.install_observer("near check: near clipper", at, |m, f, t| clipper(m, f, t, false)) {
            failed.push(e);
        }
    }
    for at in FAR_CLIPPERS {
        if let Err(e) = m.install_observer("near check: far clipper", at, |m, f, t| clipper(m, f, t, true)) {
            failed.push(e);
        }
    }
    for e in &failed {
        crate::warn!("near check: {e}");
    }
    crate::info!("near check (SR_NEAR_CHECK): game frames {}..{}, dumps {:?}, {} observers failed", c.first, c.last, c.dump, failed.len());
}

fn frame_now() -> u64 {
    crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed)
}

fn active() -> Option<&'static Cfg> {
    let c = cfg()?;
    let f = frame_now();
    (f >= c.first && f <= c.last).then_some(c)
}

fn clipper(m: &mut Machine, f: &mut HookFrame, tramp: u32, far: bool) -> HookResult {
    if active().is_some() {
        let out = rd(m.img.link(CLIP_OUT));
        if let Some(s) = STATE.lock().unwrap().as_mut() {
            s.clipped.insert(out, (f.ebp, far));
        }
    }
    HookResult::Resume(tramp)
}

fn list_loop(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    if f.esi == m.img.link(POLY_LIST) {
        if let Some(c) = cfg().filter(|c| frame_now() > c.last) {
            // past the range: write the last frame once
            if let Some(s) = STATE.lock().unwrap().as_mut().filter(|s| s.frame != 0) {
                finish_frame(s, c);
                s.frame = 0;
            }
        }
        if let Some(c) = active() {
            let mut g = STATE.lock().unwrap();
            if let Some(s) = g.as_mut() {
                let frame = frame_now();
                if frame != s.frame {
                    finish_frame(s, c);
                    s.frame = frame;
                }
                analyse_part(m, s, c.dump.contains(&frame));
                s.clipped.clear();
            }
        }
    }
    HookResult::Resume(tramp)
}

fn finish_frame(s: &mut State, c: &Cfg) {
    if s.frame == 0 {
        return;
    }
    let st = std::mem::take(&mut s.stats);
    let mut line = String::new();
    if !s.summary_header {
        s.summary_header = true;
        line.push_str(
            "frame\tparts_plain\tparts_near\tparts_far\tkept\tclipped\tclip_rejected\tback\tside\tbehind\tnative_area\tgame_area\toverlap\t\
             native_only_kept\tnative_only_clipped\tnative_only_rejected\tnative_only_back\tnative_only_side\tnative_only_behind\t\
             game_only_kept\tgame_only_clipped\tgame_only_rejected\tgame_only_back\tgame_only_side\tgame_only_behind\toverflow_x\twrap_y\tmispredicted\t\
             overflow_polys\toverflow_game_only\tcull_z_mean\tcull_z_min\tcull_z_max\tcull_xy_max\tbehind_eye\tbehind_eye_area\n",
        );
    }
    let _ = write!(line, "{}\t{}\t{}\t{}", s.frame, st.parts[0], st.parts[1], st.parts[2]);
    for d in st.decisions {
        let _ = write!(line, "\t{d}");
    }
    let _ = write!(line, "\t{:.0}\t{:.0}\t{:.0}", st.native_area, st.game_area, st.overlap);
    for a in st.native_only.iter().chain(&st.game_only) {
        let _ = write!(line, "\t{a:.0}");
    }
    let cz = st.cull_z;
    let _ = writeln!(
        line,
        "\t{}\t{}\t{}\t{}\t{:.0}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{}\t{:.0}",
        st.overflow_x,
        st.wrap_y,
        st.mispredicted,
        st.overflow_polys,
        st.overflow_game_only,
        if cz.3 > 0 { cz.0 / cz.3 as f64 } else { 0.0 },
        cz.1,
        cz.2,
        st.cull_xy_max,
        st.behind_eye,
        st.behind_eye_area
    );
    append(&c.dir.join("near_summary.tsv"), &line);
    for b in &st.first_bad {
        crate::info!("near check frame {}: {b}", s.frame);
    }
    if !s.dump.is_empty() {
        let _ = std::fs::write(c.dir.join(format!("near_{}.tsv", s.frame)), std::mem::take(&mut s.dump));
    }
}

fn append(p: &std::path::Path, s: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = f.write_all(s.as_bytes());
    }
}

/// The engine's decision on one polygon (index into `FrameStats::decisions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Kept = 0,
    Clipped = 1,
    ClipRejected = 2,
    Back = 3,
    Side = 4,
    Behind = 5,
}

fn analyse_part(m: &Machine, s: &mut State, dump: bool) {
    let l = |a: u32| m.img.link(a);
    // main view, world objects only
    if rd(l(VARIANT)) & 1 != 0 {
        return;
    }
    let cam = rd(l(CAMERA_PTR));
    let view0 = rd(l(VIEW_CAMERAS));
    if view0 < 0x10000 || view0.wrapping_add(0xa64) != cam || !mem::readable(cam, 0x18) {
        return;
    }
    let raw = rd(l(CUR_INSTANCE));
    if raw & 0x4000_0000 != 0 || raw == u32::MAX {
        return;
    }
    let inst = raw;
    let model = rd(l(CUR_MODEL));
    let rec = rd(l(CUR_RECORD));
    let part = rd(l(CUR_PART));
    let verts = rd(l(CUR_VERTS));
    if !mem::readable(inst, 0x20) || !mem::readable(model, 0xc8) || !mem::readable(rec, 0x150) || !mem::readable(part, 0x70) {
        return;
    }
    // animated models and billboards: not handled (none on China's world)
    if rdi(model + 0x10) >= 0 || rd(inst + 4) & 0x8000 != 0 {
        return;
    }
    let slot = unsafe { mem::rd16(l(PART_ORDER) + 4 * rd(l(CUR_SLOT))) } as u32;
    let origin_z = rdi(rec + 0x144);
    let radius = rdi(rd(rec));
    let p = path(origin_z, radius, rdi(l(FAR_NEG)));
    s.stats.parts[p as usize] += 1;
    if p != Path::Near && !dump {
        return;
    }
    // the float transform (as the scene's static: true sine and cosine)
    let cam_a = [rdi(cam) as f64, rdi(cam + 4) as f64, rdi(cam + 8) as f64];
    let cam_p = [rdi(cam + 0xc) as f64, rdi(cam + 0x10) as f64, rdi(cam + 0x14) as f64];
    let obj_a = [rdi(inst + 0x14) as f64, rdi(inst + 0x18) as f64, rdi(inst + 0x1c) as f64];
    let obj_p = [rdi(inst + 8) as f64, rdi(inst + 0xc) as f64, rdi(inst + 0x10) as f64];
    let part0 = model.wrapping_add(rd(model + 0x18 + 4 * slot));
    let placement = model.wrapping_add(rd(part0 + 0xc));
    if !mem::readable(placement, 0x1c) {
        return;
    }
    let pa = [rdi(placement + 4) as f64, rdi(placement + 8) as f64, rdi(placement + 0xc) as f64];
    let pp = [rdi(placement + 0x10) as f64, rdi(placement + 0x14) as f64, rdi(placement + 0x18) as f64];
    let to_view = transforms::world_to_view(cam_p, cam_a).after(&transforms::part_to_world(obj_p, obj_a, &Affine::new(transforms::rotation(pa), pp)));
    let centre = (rdi(l(CENTER_IMM)) >> 16) as f64;
    let cy = rdi(l(CENTER_Y)) as f64;
    let rect = [0.0, 0.0, rdi(l(VIEW_W)) as f64, rdi(l(VIEW_H)) as f64];
    let name = unsafe { mem::cstr(part + 0x68, 12) };
    let name = String::from_utf8_lossy(&name).into_owned();
    // the engine's list: kept records and clip buffers
    let mut kept = std::collections::HashSet::new();
    let mut clip_out: HashMap<u32, u32> = HashMap::new();
    let list = l(POLY_LIST);
    for k in 0..16384u32 {
        let e = rd(list + 8 * k);
        if e == u32::MAX {
            break;
        }
        let ptr = rd(list + 8 * k + 4);
        if e & 0xf == 0 {
            kept.insert(ptr);
        } else if let Some(&(r, _)) = s.clipped.get(&ptr) {
            clip_out.insert(r, ptr);
        }
    }
    let clip_called: HashMap<u32, bool> = s.clipped.values().map(|&(r, far)| (r, far)).collect();
    // the back-face point of the path used (model coordinates)
    let cp = CULL_POINT[match p {
        Path::Plain => 0,
        Path::Near => 1,
        Path::Far => 2,
    }]
    .map(|a| rdi(l(a)) as i64);
    if p == Path::Near {
        let c = to_view.apply(cp.map(|v| v as f64));
        let z = &mut s.stats.cull_z;
        if z.3 == 0 {
            (z.1, z.2) = (c[2], c[2]);
        }
        *z = (z.0 + c[2], z.1.min(c[2]), z.2.max(c[2]), z.3 + 1);
        s.stats.cull_xy_max = s.stats.cull_xy_max.max(c[0].abs()).max(c[1].abs());
    }
    let table =|k: i32| rdi(l(SIDE_TABLE).wrapping_add((k as u32).wrapping_mul(4)));
    let nverts = {
        let mut n = 0u32;
        while n < 65536 && mem::readable(verts + 6 * n, 6) && rd16i(verts + 6 * n) != 0x7fff {
            n += 1;
        }
        n
    };
    // every polygon of the level (the union of its 16 orders)
    let mut records: Vec<u32> = Vec::new();
    for o in 0..16 {
        let order = rd(part + 0x28 + 4 * o);
        if order == 0 {
            continue;
        }
        let mut a = part.wrapping_add(order);
        for _ in 0..65536 {
            if !mem::readable(a, 2) {
                break;
            }
            let off = rd16i(a);
            if off <= 0 {
                break;
            }
            records.push(part.wrapping_add(off as u32));
            a += 2;
        }
    }
    records.sort_unstable();
    records.dedup();
    let frame = s.frame;
    for r in records {
        if !mem::readable(r, 0x24) {
            continue;
        }
        let n = unsafe { mem::rd16(r + 6) } as usize;
        if !(3..=4).contains(&n) {
            continue;
        }
        let offs: Vec<u32> = (0..n).map(|k| unsafe { mem::rd16(r + 8 + 2 * k as u32) } as u32).collect();
        if offs.iter().any(|&o| o % 6 != 0 || o / 6 >= nverts) {
            continue;
        }
        let ev: Vec<[i32; 3]> = offs.iter().map(|&o| [rdi(l(VIEW_POS) + 2 * o), rdi(l(VIEW_POS) + 2 * o + 4), rdi(l(VIEW_POS) + 2 * o + 8)]).collect();
        let packed: Vec<u32> = offs.iter().map(|&o| rd(l(PACKED_XY) + o)).collect();
        let mv: Vec<[i32; 3]> = offs.iter().map(|&o| [rd16i(verts + o), rd16i(verts + o + 2), rd16i(verts + o + 4)]).collect();
        let fv: Vec<[f64; 3]> = mv.iter().map(|v| to_view.apply([v[0] as f64, v[1] as f64, v[2] as f64])).collect();
        let d = rdi(r + 0x20);
        let normal = [rdi(r + 0x14) as i64, rdi(r + 0x18) as i64, rdi(r + 0x1c) as i64];
        let back = (0..3).map(|k| (mv[0][k] as i64 - cp[k]) * normal[k]).sum::<i64>() < 0;
        let zs: Vec<i32> = ev.iter().map(|v| v[2]).collect();
        // the engine's decision, from its list and the clipper calls
        let decision = if kept.contains(&r) {
            Decision::Kept
        } else if clip_out.contains_key(&r) {
            Decision::Clipped
        } else if clip_called.contains_key(&r) {
            Decision::ClipRejected
        } else if back {
            Decision::Back
        } else if p == Path::Near && !side_test(&ev, d, &table) {
            Decision::Side
        } else {
            Decision::Behind
        };
        // predicted from the decoding (path 2 only; path 1 keeps every front face)
        let predicted = if back {
            Decision::Back
        } else {
            match p {
                Path::Plain => Decision::Kept,
                Path::Far => decision,
                Path::Near => {
                    if !side_test(&ev, d, &table) {
                        Decision::Side
                    } else {
                        match near_class(&zs) {
                            NearClass::Front => Decision::Kept,
                            NearClass::Behind => Decision::Behind,
                            NearClass::Crossing => {
                                if decision == Decision::ClipRejected {
                                    Decision::ClipRejected
                                } else {
                                    Decision::Clipped
                                }
                            }
                        }
                    }
                }
            }
        };
        if predicted != decision {
            s.stats.mispredicted += 1;
            if s.stats.first_bad.len() < 8 {
                s.stats.first_bad.push(format!("{name} record {r:#x}: engine {decision:?}, predicted {predicted:?} (path {p:?}, z {zs:?})"));
            }
        }
        if p == Path::Near {
            s.stats.decisions[decision as usize] += 1;
        }
        // the engine's screen polygon
        let game_poly: Vec<(f64, f64)> = match decision {
            Decision::Kept => packed.iter().map(|&q| unpack_f(q)).collect(),
            Decision::Clipped => {
                let b = clip_out[&r];
                let cnt = rd(b + 4) & 0xff;
                (0..cnt).map(|k| unpack_f(rd(b + 8 + 16 * k + 8))).collect()
            }
            _ => Vec::new(),
        };
        let mut overflowed = false;
        for (i, v) in ev.iter().enumerate() {
            if v[2] <= 0 && packed[i] == BEHIND {
                s.stats.overflow_x += 1;
                overflowed = true;
            } else if v[2] <= 0 {
                let exact = cy + FOCAL * v[1] as f64 / (v[2] as f64 - EYE);
                if (exact - transforms::unpack(packed[i]).1 as f64).abs() > 1000.0 {
                    s.stats.wrap_y += 1;
                    overflowed = true;
                }
            }
        }
        let overflowed = overflowed && matches!(decision, Decision::Kept | Decision::Clipped);
        let front = eye_sees_front(&fv);
        let native_poly = if front { native_screen(&fv, centre, cy) } else { Vec::new() };
        let nat = clip_rect(&native_poly, rect);
        let gam = clip_rect(&game_poly, rect);
        let na = signed_area(&nat).abs();
        let ga = signed_area(&gam).abs();
        let ov = if na > 0.0 && ga > 0.0 { signed_area(&clip_convex(&nat, &gam)).abs().min(na).min(ga) } else { 0.0 };
        let st = &mut s.stats;
        st.native_area += na;
        st.game_area += ga;
        st.overlap += ov;
        st.native_only[decision as usize] += na - ov;
        st.game_only[decision as usize] += ga - ov;
        if overflowed {
            st.overflow_polys += 1;
            st.overflow_game_only += ga - ov;
        }
        if na > 0.0 && fv.iter().any(|v| v[2] >= EYE) {
            st.behind_eye += 1;
            st.behind_eye_area += na;
        }
        if dump && (p == Path::Near || (na - ov).abs() > 20.0 || (ga - ov).abs() > 20.0) {
            let fmt_poly = |q: &[(f64, f64)]| q.iter().map(|v| format!("{:.1},{:.1}", v.0, v.1)).collect::<Vec<_>>().join(" ");
            if s.dump.is_empty() {
                s.dump.push_str("frame\tobject\tname\tslot\tpath\trecord\tmaterial\tn\td\tdecision\tback\tfront\tnative_area\tgame_area\toverlap\tengine_view\tpacked\tfloat_view\tnative_poly\tgame_poly\n");
            }
            let mat = rd16i(r + 4);
            let mtype = rd(rd(model + 0xb4).wrapping_add(mat as u32));
            let _ = writeln!(
                s.dump,
                "{frame}\t{inst:#x}\t{name}\t{slot}\t{p:?}\t{r:#x}\t{mtype:#x}\t{n}\t{d}\t{decision:?}\t{back}\t{front}\t{na:.0}\t{ga:.0}\t{ov:.0}\t{}\t{}\t{}\t{}\t{}",
                ev.iter().map(|v| format!("{},{},{}", v[0], v[1], v[2])).collect::<Vec<_>>().join(" "),
                packed.iter().map(|&q| if q == BEHIND { "B".to_string() } else { let (x, y) = transforms::unpack(q); format!("{x},{y}") }).collect::<Vec<_>>().join(" "),
                fv.iter().map(|v| format!("{:.1},{:.1},{:.1}", v[0], v[1], v[2])).collect::<Vec<_>>().join(" "),
                fmt_poly(&native_poly),
                fmt_poly(&game_poly),
            );
        }
    }
}

/// A packed screen position as the engine's set-ups read it (BEHIND = column 4096, row 4096).
fn unpack_f(q: u32) -> (f64, f64) {
    let (x, y) = transforms::unpack(q);
    (x as f64, y as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_list_builder() {
        // 7ad33: origin z + radius >= -8 -> near path; 7ad40: origin z - radius <= -D -> far
        assert_eq!(path(-1000, 992, -50000), Path::Near);
        assert_eq!(path(-1000, 991, -50000), Path::Plain);
        assert_eq!(path(-49000, 1000, -50000), Path::Far);
        assert_eq!(path(-48999, 1000, -50000), Path::Plain);
    }

    #[test]
    fn near_classes() {
        assert_eq!(near_class(&[-1, -5, -9]), NearClass::Front);
        assert_eq!(near_class(&[0, 3, 9]), NearClass::Behind);
        assert_eq!(near_class(&[-1, 0, -9, -4]), NearClass::Crossing);
    }

    #[test]
    fn rectangle_and_convex_clips() {
        let sq = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert_eq!(signed_area(&clip_rect(&sq, [5.0, 0.0, 20.0, 20.0])).abs(), 50.0);
        let tri = [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)];
        assert!((signed_area(&clip_convex(&sq, &tri)).abs() - 50.0).abs() < 1e-9);
        // either winding of the clipping polygon
        let rev: Vec<_> = tri.iter().rev().copied().collect();
        assert!((signed_area(&clip_convex(&sq, &rev)).abs() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn near_plane_clip_of_a_crossing_polygon() {
        // one vertex behind the near plane (z' = 100), two in front
        let v = [[100.0, 0.0, -1000.0], [-100.0, 0.0, -1000.0], [0.0, 50.0, 100.0]];
        let p = native_screen(&v, 319.0, 260.0);
        assert_eq!(p.len(), 4);
        // the new vertices lie on z' = 0: x = 319 - 2 x', y = 260 - 2 y'
        assert!(p.iter().filter(|q| (q.1 - 260.0).abs() > 1e-9 && (q.1 - (260.0 - 2.0 * 50.0 * (1000.0 / 1100.0))).abs() < 1e-6).count() == 2);
    }
}
