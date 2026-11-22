//! Polygon records and the polygon builder (topic B of `docs/04-native-renderer.md`; the
//! layout and the builder are described in `docs/re/native/polygons.md`).
//!
//! Diagnostics only, off by default. `SR_POLY_CHECK=N` reproduces, for every polygon one of
//! the engine's set-up routines hands to the Glide vertex builder (`cb6b0`), what the builder
//! sends to `grDrawTriangle`, from the polygon record and the data it names: texture
//! coordinates (s/w, t/w), 1/w, the flat colour, the reflection (mode 5) coordinates
//! recomputed from vertex normals, and the Glide state of each mode (texture and palette,
//! clamp, chroma key, colour / alpha combine, blending, tint). It compares that with the
//! game's own vertices (read from the builder's vertex buffer just before each
//! `grDrawTriangle`, so the runtime's own adjustments do not enter) and with the Glide state
//! at that moment, and logs a summary every N frames. The sky and ground quads (builder
//! mode 8) are predicted from the camera the same way. `SR_POLY_SAMPLE=FRAME` additionally
//! logs the first polygons of every mode in that frame in full.
//!
//! Addresses are link addresses (`docs/re/strt3fx.md`).

use std::collections::{BTreeMap, HashMap};

use crate::Machine;
use crate::cpu::HookFrame;
use crate::glide::consts::*;
use crate::hooks::HookResult;
use crate::mem;

/// Set-up routines for polygon records whose vertices are all in front of the camera
/// (dispatch table `798e0`, column 0) and the builder mode each one sets ([aa5c8]).
const SETUPS: [(u32, u8); 7] = [(0xca670, 0), (0xd5f30, 1), (0xce9f0, 3), (0xd25c0, 4), (0xd567c, 5), (0xce190, 6), (0xd2e20, 7)];
/// Mode 2 (flat colour, d02cc): observed one instruction pair later, at d02d2 (d02cc is
/// observed by `crate::depth`); esi = the record there.
const FLAT_SETUP: u32 = 0xd02d2;
/// Set-up routines for polygons the near-plane clipper rebuilt in screen space (column 1 of
/// the dispatch table; d01c4 is observed by `crate::depth`, so d01ca).
const CLIPPED_SETUPS: [(u32, u8); 8] =
    [(0xca4a0, 0), (0xd5d60, 1), (0xd01ca, 2), (0xce820, 3), (0xd23f0, 4), (0xd54bc, 5), (0xcdfc0, 6), (0xd2c50, 7)];
/// The builder's triangle loop, before `push edx; push eax; push 1738a8; call grDrawTriangle`
/// (cc6df: `mov eax, [173e48]`, `lea edx, [eax + 3Ch]`).
const TRI_LOOP: u32 = 0xcc6df;
/// Sky: the engine's sky routine (7b218) and the builder's sky path after its two textured
/// triangles (cb9b3) and after the two ground triangles (cba4d).
const SKY_ENTRY: u32 = 0x7b218;
const SKY_DRAWN: u32 = 0xcb9b3;
const GROUND_DRAWN: u32 = 0xcba4d;

// Engine data (link addresses).
const MODE: u32 = 0xaa5c8;
const PACKED_XY: u32 = 0x6ed94;
const VIEW_POS: u32 = 0x68fc8;
const UV_PTR: u32 = 0x7bad4;
/// The part's model vertex list (int16 x, y, z by vertex offset; [level + 1Ch] + model).
const MODEL_VERTS: u32 = 0x7bacc;
const ENV_UV: u32 = 0xbcf94;
const MODEL_BASE: u32 = 0x14739c;
const PHASE: u32 = 0x7bb94;
const PHASE5: u32 = 0x7bb98;
const ENV_MATERIAL: u32 = 0xaa6a8;
const TEX_TABLE: u32 = 0x1479d4;
const TEX_ENTRY: u32 = 0x38;
const TMU_SLOTS: u32 = 0x171814;
const TINTS: u32 = 0x173fa4;
const X_TABLE_PTR: u32 = 0xaa6c4;
const X_OFFSET: u32 = 0xaa6b8;
const Y_OFFSET: u32 = 0xaa6bc;
const Y_TABLE: u32 = 0xac6cc;
const OOW_K: u32 = 0x122314;
const FOG_ON: u32 = 0x12373c;
const VERTS: u32 = 0x1738a8;
const VERT_PTR: u32 = 0x173e48;
const VSTRIDE: u32 = 0x3c;
// mode 5
const NORMALS_PTR: u32 = 0x7bad0;
const PART_RECORD: u32 = 0x7bac0;
const NORMAL_SCALE: u32 = 0xbb7e4;
const INSTANCE: u32 = 0x7bb8c;
const ENV_SCRATCH: u32 = 0xbb6a0;
// sky
const CAMERA_PTR: u32 = 0x81e0c;
const PITCH: u32 = 0x798ac;
const CENTER_Y: u32 = 0x81e3c;
const VIEW_H: u32 = 0x81e24;
const SKY_MIRROR: u32 = 0xaa5b0;
const SKY_TEXTURE: u32 = 0x173fd4;
const SKY_TMU: u32 = 0x173f80;
const SKY_ON: u32 = 0x123740;
const GROUND_COLOR_SKY: u32 = 0x81e1c;
const GROUND_COLOR_FLAT: u32 = 0x123744;
const SKY_VERTS: u32 = 0x173e4c;
/// The set-ups' back-face test switch ([aa6b4] != 0, bit 0 of [aa5b0] clear).
const CULL_ON: u32 = 0xaa6b4;
/// The part's model-to-view matrix (12 floats, `crate::subpixel`).
const PART_MATRIX: u32 = 0x7a9c4;
/// The mode-0 set-up's screen limits: `cmp ebx, 27E0000h` (ca81f, x << 16) and `cmp dx, 18Fh`
/// (ca827), both patched at run time (widescreen, view height).
const X_LIMIT_IMM: u32 = 0xca821;
const Y_LIMIT_IMM: u32 = 0xca82a;
/// Displacement of `fld [eax + 9FCh]` at cb971 (right edge of the sky quad in the x table).
const SKY_RIGHT_DISP: u32 = 0xcb973;

/// The polygon set-up dispatch table: `[798e0 + material type + variant]` (`7ba26`), four
/// variants (0, 4, 8, C) per row of 10h bytes, types 00h..1F0h.
pub const DISPATCH: u32 = 0x798e0;

/// The builder mode of polygons whose material has type `ty` (material +00, a multiple of
/// 10h): the variant-0 set-up routine the dispatch table names (read from memory, so a
/// patched table is followed), mapped to the mode that routine stores in [aa5c8]. `None`
/// for an unknown routine or an implausible type.
pub fn mode_of_type(m: &Machine, ty: u32) -> Option<u8> {
    if ty & 0xf != 0 || ty >= 0x200 {
        return None;
    }
    let at = m.img.link(DISPATCH) + ty;
    if !mem::readable(at, 4) {
        return None;
    }
    let routine = m.img.to_link(unsafe { mem::rd32(at) })?;
    // d02cc: the flat set-up's entry (FLAT_SETUP observes it one instruction pair later)
    SETUPS.iter().chain(&[(0xd02cc, 2)]).find(|&&(a, _)| a == routine).map(|&(_, mode)| mode)
}

fn env_flag(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

pub fn install(m: &mut Machine) -> Result<(), String> {
    let Some(every) = env_flag("SR_POLY_CHECK") else { return Ok(()) };
    let l = |a: u32| m.img.link(a);
    let addrs = Addrs {
        mode: l(MODE),
        packed: l(PACKED_XY),
        view: l(VIEW_POS),
        uv_ptr: l(UV_PTR),
        model_verts: l(MODEL_VERTS),
        env_uv: l(ENV_UV),
        model_base: l(MODEL_BASE),
        phase: l(PHASE),
        phase5: l(PHASE5),
        env_material: l(ENV_MATERIAL),
        tex_table: l(TEX_TABLE),
        tmu_slots: l(TMU_SLOTS),
        tints: l(TINTS),
        x_table_ptr: l(X_TABLE_PTR),
        x_offset: l(X_OFFSET),
        y_offset: l(Y_OFFSET),
        y_table: l(Y_TABLE),
        oow_k: l(OOW_K),
        fog_on: l(FOG_ON),
        verts: l(VERTS),
        vert_ptr: l(VERT_PTR),
        normals_ptr: l(NORMALS_PTR),
        part_record: l(PART_RECORD),
        normal_scale: l(NORMAL_SCALE),
        instance: l(INSTANCE),
        env_scratch: l(ENV_SCRATCH),
        camera_ptr: l(CAMERA_PTR),
        pitch: l(PITCH),
        center_y: l(CENTER_Y),
        view_h: l(VIEW_H),
        sky_mirror: l(SKY_MIRROR),
        sky_texture: l(SKY_TEXTURE),
        sky_tmu: l(SKY_TMU),
        sky_on: l(SKY_ON),
        ground_sky: l(GROUND_COLOR_SKY),
        ground_flat: l(GROUND_COLOR_FLAT),
        sky_verts: l(SKY_VERTS),
        cull_on: l(CULL_ON),
        matrix: l(PART_MATRIX),
        x_limit: l(X_LIMIT_IMM),
        y_limit: l(Y_LIMIT_IMM),
        sky_right: l(SKY_RIGHT_DISP),
    };
    *CHECK.lock().unwrap() = Some(Check { addrs, every: every.max(1), sample_frame: env_flag("SR_POLY_SAMPLE"), ..Default::default() });
    let mut ok = 0;
    let mut add = |m: &mut Machine, name: &'static str, at: u32, h: crate::hooks::Handler| match m.install_observer(name, at, h) {
        Ok(()) => ok += 1,
        Err(e) => crate::warn!("polygon check: {e}"),
    };
    for (at, mode) in SETUPS {
        // eax = the polygon record at the routine's entry
        let h: crate::hooks::Handler = match mode {
            0 => |m, f, t| setup(m, t, 0, f.eax),
            1 => |m, f, t| setup(m, t, 1, f.eax),
            3 => |m, f, t| setup(m, t, 3, f.eax),
            4 => |m, f, t| setup(m, t, 4, f.eax),
            5 => |m, f, t| setup(m, t, 5, f.eax),
            6 => |m, f, t| setup(m, t, 6, f.eax),
            _ => |m, f, t| setup(m, t, 7, f.eax),
        };
        add(m, "polygon check: set-up", at, h);
    }
    add(m, "polygon check: flat set-up", FLAT_SETUP, |m, f, t| setup(m, t, 2, f.esi));
    for (at, _) in CLIPPED_SETUPS {
        add(m, "polygon check: clipped set-up", at, clipped_setup);
    }
    add(m, "polygon check: triangle", TRI_LOOP, triangle);
    add(m, "polygon check: sky", SKY_ENTRY, sky_entry);
    add(m, "polygon check: sky drawn", SKY_DRAWN, sky_drawn);
    add(m, "polygon check: ground drawn", GROUND_DRAWN, ground_drawn);
    crate::info!("polygon check (SR_POLY_CHECK): {ok} observers, summary every {every} frames");
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct Addrs {
    mode: u32,
    packed: u32,
    view: u32,
    uv_ptr: u32,
    model_verts: u32,
    env_uv: u32,
    model_base: u32,
    phase: u32,
    phase5: u32,
    env_material: u32,
    tex_table: u32,
    tmu_slots: u32,
    tints: u32,
    x_table_ptr: u32,
    x_offset: u32,
    y_offset: u32,
    y_table: u32,
    oow_k: u32,
    fog_on: u32,
    verts: u32,
    vert_ptr: u32,
    normals_ptr: u32,
    part_record: u32,
    normal_scale: u32,
    instance: u32,
    env_scratch: u32,
    camera_ptr: u32,
    pitch: u32,
    center_y: u32,
    view_h: u32,
    sky_mirror: u32,
    sky_texture: u32,
    sky_tmu: u32,
    sky_on: u32,
    ground_sky: u32,
    ground_flat: u32,
    sky_verts: u32,
    cull_on: u32,
    matrix: u32,
    x_limit: u32,
    y_limit: u32,
    sky_right: u32,
}

/// A predicted builder vertex: Glide screen position, 1/w, s/w and t/w, and the record's
/// texture coordinates (u, v).
#[derive(Clone, Copy, Default, Debug)]
struct PVert {
    x: f32,
    y: f32,
    oow: f32,
    sow: f32,
    tow: f32,
    u: i16,
    v: i16,
}

/// The Glide state a polygon of this mode is drawn with (None: not determined by the mode).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct PState {
    color_combine: [u32; 5],
    alpha_combine: [u32; 5],
    blend: [u32; 4],
    chromakey: u8,
    chromakey_value: Option<u32>,
    clamp: Option<[u32; 2]>,
    constant: Option<u32>,
    /// Texture entry (address of its 38h-byte record), for the texture binding and palette.
    tex_entry: Option<u32>,
    /// grFogMode: table fog (2) while [12373c] is set, except the multiplied decals.
    fog: u32,
}

#[derive(Default)]
struct Pending {
    mode: u8,
    record: u32,
    material: u32,
    mtype: u32,
    tex: u32,
    verts: Vec<PVert>,
    rgb: Option<[f32; 3]>,
    state: PState,
    /// Goes through the 2D clipper (some vertex outside the screen limits).
    clip2d: bool,
    sample: bool,
    tris: u32,
}

const STATE_FIELDS: [&str; 11] = ["color_combine", "alpha_combine", "blend", "chromakey", "chromakey_value", "clamp", "constant", "texture", "palette", "filter", "fog"];

#[derive(Default)]
struct ModeStats {
    polys: u64,
    tris: u64,
    tris_unpredicted: u64,
    verts: u64,
    verts_2d: u64,
    st_bad: u64,
    st_max_err: f64,
    oow_bad: u64,
    rgb_bad: u64,
    state_bad: [u64; 11],
    env_checked: u64,
    env_bad: u64,
    env_offset_bad: u64,
    /// Vertices the 2D clipper kept (found by position, not by fan order) and mismatches
    /// among them (two vertices on one whole pixel can be confused there).
    verts_kept: u64,
    kept_bad: u64,
    /// Vertices of polygons drawn without the 2D clipper that are not where the fan puts
    /// the record's vertices.
    order_bad: u64,
    /// Polygons that go through the 2D clipper (by the set-up's screen test).
    clip2d: u64,
    counts: BTreeMap<u16, u64>,
    mtypes: BTreeMap<u32, u64>,
    rec0: BTreeMap<u16, u64>,
    rec2: BTreeMap<u16, u64>,
    formats: BTreeMap<u32, u64>,
    blend_flag: BTreeMap<u32, u64>,
    tinted: u64,
    /// Record geometry: the vertex list closed by a copy of the first vertex; zero normals;
    /// normal against the model-space cross product (v1 - v0) x (v2 - v0); +20h against the
    /// largest distance between two vertices; screen winding at the set-up.
    closed: u64,
    not_closed: u64,
    zero_normal: u64,
    normal_same: u64,
    normal_opposite: u64,
    normal_other: u64,
    size_exact: u64,
    size_near: u64,
    size_other: u64,
    size_example: Option<String>,
    size_cands: [u64; 5],
    winding: [u64; 4],
    cull_on: u64,
    /// Sign of (v0 - eye) . n in view space: away from the eye, towards it, zero.
    normal_view: [u64; 3],
    first_mismatch: Option<String>,
}

#[derive(Default)]
struct SkyStats {
    quads: u64,
    bad: u64,
    ground: u64,
    ground_bad: u64,
    first_mismatch: Option<String>,
}

#[derive(Default)]
struct Check {
    addrs: Addrs,
    every: u64,
    sample_frame: Option<u64>,
    frame: u64,
    frames_seen: u64,
    pending: Option<Pending>,
    /// Polygons handed to the set-ups for near-clipped polygons.
    clipped: u64,
    stats: [ModeStats; 8],
    sampled: [u32; 8],
    sky: Option<[PVert; 4]>,
    ground: Option<(f32, u32)>,
    sky_stats: SkyStats,
    /// Largest |v| of a part's vertex list, by list address (record statistics).
    part_max: HashMap<u32, f64>,
}

/// The check's state (the hooks run on the guest thread, `install` on the main thread).
static CHECK: std::sync::Mutex<Option<Check>> = std::sync::Mutex::new(None);

fn with_check(f: impl FnOnce(&mut Check)) {
    if let Ok(mut g) = CHECK.lock() {
        if let Some(c) = g.as_mut() {
            f(c)
        }
    }
}

fn bump<K: Ord>(map: &mut BTreeMap<K, u64>, k: K) {
    if map.len() < 64 || map.contains_key(&k) {
        *map.entry(k).or_insert(0) += 1;
    }
}

unsafe fn f32_at(a: u32) -> f32 {
    f32::from_bits(unsafe { mem::rd32(a) })
}

/// 5:6:5 colour to the builder's iterated 0..255 components (cbcd9: (c & F800h) >> 8,
/// (c & 7E0h) >> 3, (c & 1Fh) << 3).
fn rgb565(c: u32) -> [f32; 3] {
    [((c & 0xf800) >> 8) as f32, ((c & 0x7e0) >> 3) as f32, ((c & 0x1f) << 3) as f32]
}

/// 5:6:5 colour to the 888 constant colour the sky's ground quad uses (cb9fb).
fn rgb565_888(c: u32) -> u32 {
    (c & 0xf800) << 8 | (c & 0x7e0) << 5 | (c & 0x1f) << 3
}

impl Check {
    /// Frame bookkeeping: print the summary every `every` frames.
    fn tick(&mut self, frame: u64) {
        if frame == self.frame {
            return;
        }
        self.frame = frame;
        self.frames_seen += 1;
        // models come and go between races: measure them again every frame
        self.part_max.clear();
        if self.frames_seen % self.every == 0 {
            self.report();
        }
    }

    fn report(&mut self) {
        crate::info!("polygon check: summary up to frame {} ({} frames), {} near-clipped polygons (not predicted)", self.frame, self.frames_seen, self.clipped);
        for (mode, s) in self.stats.iter_mut().enumerate() {
            if s.polys == 0 && s.tris == 0 {
                continue;
            }
            let bad: Vec<String> = STATE_FIELDS.iter().zip(s.state_bad.iter()).filter(|(_, n)| **n > 0).map(|(f, n)| format!("{f} {n}")).collect();
            crate::info!(
                "  mode {mode}: {} polygons ({} through the 2D clipper), {} triangles ({} unpredicted), {} vertices checked in fan order ({} out of order); 2D-clipped polygons: {} vertices found by position ({} differing), {} new; s/t mismatches {} (max |err| {:.2e}), 1/w mismatches {}, colour mismatches {}, state mismatches [{}]; reflection uv {} checked, {} bad (offsets bad {}); tinted {}",
                s.polys,
                s.clip2d,
                s.tris,
                s.tris_unpredicted,
                s.verts,
                s.order_bad,
                s.verts_kept,
                s.kept_bad,
                s.verts_2d,
                s.st_bad,
                s.st_max_err,
                s.oow_bad,
                s.rgb_bad,
                bad.join(", "),
                s.env_checked,
                s.env_bad,
                s.env_offset_bad,
                s.tinted
            );
            let fmt = |m: &BTreeMap<u32, u64>| m.iter().map(|(k, v)| format!("{k:#x}:{v}")).collect::<Vec<_>>().join(" ");
            let fmt16 = |m: &BTreeMap<u16, u64>| m.iter().map(|(k, v)| format!("{k:#x}:{v}")).collect::<Vec<_>>().join(" ");
            crate::info!(
                "    vertex counts [{}] material types [{}] record+0 [{}] record+2 [{}] texture formats [{}] texture+34h [{}]",
                fmt16(&s.counts),
                fmt(&s.mtypes),
                fmt16(&s.rec0),
                fmt16(&s.rec2),
                fmt(&s.formats),
                fmt(&s.blend_flag)
            );
            crate::info!(
                "    vertex list closed {} / not {}; normal zero {}, along (v1-v0)x(v2-v0) {}, opposite {}, other {}; +20h = largest vertex distance {} (within 1: {}, other {}{}); set-up winding [< -20: {}, -20..-1: {}, 0: {}, > 0: {}]",
                s.closed,
                s.not_closed,
                s.zero_normal,
                s.normal_same,
                s.normal_opposite,
                s.normal_other,
                s.size_exact,
                s.size_near,
                s.size_other,
                s.size_example.as_ref().map(|e| format!(", e.g. {e}")).unwrap_or_default(),
                s.winding[0],
                s.winding[1],
                s.winding[2],
                s.winding[3]
            );
            crate::info!(
                "    +20h within 1 of: 2 x largest |v| {}, largest |v| {}, |n.v0| / 16384 {}, largest |v - v0| {}, 2 x largest |v| of the part {}; set-up back-face test on for {}; (v0 - eye).n in view space > 0: {}, < 0: {}, 0: {}",
                s.size_cands[0],
                s.size_cands[1],
                s.size_cands[2],
                s.size_cands[3],
                s.size_cands[4],
                s.cull_on,
                s.normal_view[0],
                s.normal_view[1],
                s.normal_view[2]
            );
            if let Some(msg) = &s.first_mismatch {
                crate::info!("    first mismatch: {msg}");
            }
        }
        let k = &self.sky_stats;
        crate::info!(
            "  sky (mode 8): {} quads, {} mismatches; ground quads {}, {} mismatches{}",
            k.quads,
            k.bad,
            k.ground,
            k.ground_bad,
            k.first_mismatch.as_ref().map(|m| format!("; first: {m}")).unwrap_or_default()
        );
    }
}

/// The texture entry a polygon of `mode` uses: the material's animation list
/// {count, ?, texture[count]} at model base + material[+4]; frame = count * phase >> 14 with
/// phase = [7bb94] (low 16 bits if a tint is selected); mode 5: the reflection material
/// [aa6a8] if set, phase [7bb98] >> 16 (d59c7).
unsafe fn texture_of(a: &Addrs, mode: u8, material: u32) -> (u32, u32, u32) {
    unsafe {
        let base = mem::rd32(a.model_base);
        let (list, phase, shift, tint) = if mode == 5 {
            let env = mem::rd32(a.env_material);
            let list = if env & 0x0fff_ffff != 0 { env.wrapping_add(mem::rd32(env + 4)) } else { base.wrapping_add(mem::rd32(material + 4)) };
            (list, mem::rd32(a.phase5), 16, 0)
        } else {
            let p = mem::rd32(a.phase);
            let tint = p & 0x60000;
            (base.wrapping_add(mem::rd32(material + 4)), if tint != 0 { p & 0xffff } else { p }, 14, tint)
        };
        let count = mem::rd32(list) as i32;
        let frame = count.wrapping_mul(phase as i32) >> shift;
        let tex = mem::rd32(list.wrapping_add(8).wrapping_add((frame as u32).wrapping_mul(4)));
        (tex, a.tex_table.wrapping_add(tex.wrapping_mul(TEX_ENTRY)), tint)
    }
}

/// The Glide state the builder sets for `mode` (cb6b0: cbd40 .. cc5e3).
unsafe fn state_of(a: &Addrs, mode: u8, entry: u32, tint: u32) -> PState {
    const TEXTURE: [u32; 5] = [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_TEXTURE, 0];
    const NO_BLEND: [u32; 4] = [GR_BLEND_ONE, GR_BLEND_ZERO, GR_BLEND_ONE, GR_BLEND_ZERO];
    const ALPHA_BLEND: [u32; 4] = [GR_BLEND_SRC_ALPHA, GR_BLEND_ONE_MINUS_SRC_ALPHA, GR_BLEND_ONE, GR_BLEND_ZERO];
    let clamp = Some([GR_TEXTURECLAMP_CLAMP; 2]);
    let fog = if unsafe { mem::rd32(a.fog_on) } != 0 { GR_FOG_WITH_TABLE } else { GR_FOG_DISABLE };
    if mode == 2 {
        return PState {
            color_combine: [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_ITERATED, 0],
            alpha_combine: TEXTURE,
            blend: NO_BLEND,
            fog,
            ..Default::default()
        };
    }
    let (color_combine, constant) = if tint != 0 {
        (
            [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_LOCAL, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_TEXTURE, 0],
            Some(unsafe { mem::rd32(a.tints + 4 * (tint >> 17)) }),
        )
    } else {
        (TEXTURE, None)
    };
    let key = Some(unsafe { mem::rd32(entry + 0x14) });
    let base = PState { color_combine, alpha_combine: TEXTURE, blend: NO_BLEND, constant, clamp, tex_entry: Some(entry), fog, ..Default::default() };
    match mode {
        1 => PState { chromakey: 1, chromakey_value: key, ..base },
        3 => PState { chromakey: 1, chromakey_value: key, blend: [GR_BLEND_ONE, GR_BLEND_SRC_COLOR, GR_BLEND_ONE, GR_BLEND_ZERO], ..base },
        5 => PState { clamp: Some([GR_TEXTURECLAMP_WRAP; 2]), ..base },
        6 => PState { blend: ALPHA_BLEND, ..base },
        7 => {
            let multiply = unsafe { mem::rd32(entry + 0x34) } != 0;
            // (2, 2): GR_BLEND_DST_COLOR as source factor, GR_BLEND_SRC_COLOR as destination
            if multiply {
                PState { blend: [2, GR_BLEND_SRC_COLOR, GR_BLEND_ONE, GR_BLEND_ZERO], fog: GR_FOG_DISABLE, ..base }
            } else {
                PState { blend: ALPHA_BLEND, ..base }
            }
        }
        _ => base,
    }
}

/// Builder screen position and 1/w of a transformed vertex (vertex offset `off`).
unsafe fn screen(a: &Addrs, off: u32) -> (f32, f32, f32) {
    unsafe {
        let packed = mem::rd32(a.packed + off) as i32;
        let z = mem::rd32(a.view + 2 * off + 8) as i32;
        let xt = mem::rd32(a.x_table_ptr);
        let (xo, yo) = (mem::rd32(a.x_offset) as i32, mem::rd32(a.y_offset) as i32);
        let x = f32_at(xt.wrapping_add((4 * (xo + (packed >> 16))) as u32));
        let y = f32_at(a.y_table.wrapping_add((4 * (yo + packed as i16 as i32)) as u32));
        let k = f64::from_bits(mem::rd32(a.oow_k) as u64 | (mem::rd32(a.oow_k + 4) as u64) << 32);
        (x, y, (k / (z - 128) as f64) as f32)
    }
}

/// Mode 5 texture coordinates of transformed vertex `i`, as bb380 computes them: e = the
/// normalised view-space position, n = the vertex normal rotated into view space, r = 2 (e.n)
/// n - e; u = round(127 r.x) + 127 - ox, v = round(127 r.z) + 127 - oz (ox, oz: the
/// object's scroll offsets).
unsafe fn env_uv(a: &Addrs, i: u32, ox: i32, oz: i32) -> Option<(i16, i16)> {
    unsafe {
        let normals = mem::rd32(a.normals_ptr);
        if normals == 0 || normals & 0xffff == 0xffff || !mem::readable(normals, 4) {
            return None;
        }
        let count = mem::rd32(normals);
        if i >= count || !mem::readable(normals + 4 + 12 * i, 12) {
            return None;
        }
        let rec = mem::rd32(a.part_record) + 0x118;
        let scale = f32_at(a.normal_scale) as f64;
        let mut mm = [0f64; 9];
        for (j, v) in mm.iter_mut().enumerate() {
            *v = (mem::rd32(rec + 4 * j as u32) as i32 as f64 * scale) as f32 as f64;
        }
        let nb = normals + 4 + 12 * i;
        let (nx, ny, nz) = (f32_at(nb) as f64, f32_at(nb + 4) as f64, f32_at(nb + 8) as f64);
        let n = [
            (mm[0] * nx + mm[3] * ny + mm[6] * nz) as f32 as f64,
            (mm[1] * nx + mm[4] * ny + mm[7] * nz) as f32 as f64,
            (mm[2] * nx + mm[5] * ny + mm[8] * nz) as f32 as f64,
        ];
        let p = a.view + 12 * i;
        let (x, y, z) = (mem::rd32(p) as i32 as f64, mem::rd32(p + 4) as i32 as f64, mem::rd32(p + 8) as i32 as f64);
        let len = (x * x + y * y + z * z).sqrt();
        let e = [x / len, y / len, z / len];
        let d2 = 2.0 * (e[0] * n[0] + e[1] * n[1] + e[2] * n[2]);
        let rx = d2 * n[0] - e[0] as f32 as f64;
        let rz = d2 * n[2] - e[2] as f32 as f64;
        let k = f32_at(a.env_scratch + 0x34) as f64;
        let u = (rx * k).round_ties_even() as i32 + 0x7f - ox;
        let v = (rz * k).round_ties_even() as i32 + 0x7f - oz;
        Some((u as i16, v as i16))
    }
}

/// The object's reflection scroll offsets (bb3a5 .. bb4f1): the accumulated view-space
/// motion at instance +30h / +38h (floats), rounded, low 8 bits; 0 for objects that are not
/// scene instances.
unsafe fn env_offsets(a: &Addrs) -> (i32, i32) {
    unsafe {
        let inst = mem::rd32(a.instance);
        if inst & 0x4000_0000 == 0 {
            return (0, 0);
        }
        let inst = inst & !0x4000_0000;
        if !mem::readable(inst + 0x30, 12) {
            return (0, 0);
        }
        let r = |o: u32| (f32_at(inst + o) as f64).round_ties_even() as i32 & 0xff;
        (r(0x30), r(0x38))
    }
}

fn frame_of(m: &Machine) -> u64 {
    m.glide.as_ref().map_or(0, |g| g.frames)
}

fn setup(m: &mut Machine, tramp: u32, mode: u8, record: u32) -> HookResult {
    let frame = frame_of(m);
    with_check(|c| {
        c.tick(frame);
        c.pending = unsafe { predict(c, mode, record, frame) };
    });
    HookResult::Resume(tramp)
}

fn clipped_setup(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let frame = frame_of(m);
    with_check(|c| {
        c.tick(frame);
        // near-clipped polygons are not predicted: their triangles are counted as
        // unpredicted (by the mode the routine sets)
        c.pending = None;
        c.clipped += 1;
    });
    HookResult::Resume(tramp)
}

/// Statistics of the record's geometry fields against the part's model vertices
/// (int16 x, y, z at [7bacc] + vertex offset) and its screen winding at the set-up.
unsafe fn record_geometry(a: &Addrs, s: &mut ModeStats, part_max: &mut HashMap<u32, f64>, record: u32, n: u32) {
    unsafe {
        let off = |j: u32| mem::rd16(record + 8 + 2 * j) as u32;
        if off(n) == off(0) {
            s.closed += 1;
        } else {
            s.not_closed += 1;
        }
        let list = mem::rd32(a.model_verts);
        let mv = |j: u32| {
            let p = list.wrapping_add(off(j));
            [mem::rd16(p) as i16 as f64, mem::rd16(p + 2) as i16 as f64, mem::rd16(p + 4) as i16 as f64]
        };
        if !mem::readable(list, 6) {
            return;
        }
        let nrm = [mem::rd32(record + 0x14) as i32 as f64, mem::rd32(record + 0x18) as i32 as f64, mem::rd32(record + 0x1c) as i32 as f64];
        let (v0, v1, v2) = (mv(0), mv(1), mv(2));
        let (e1, e2) = ([v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]], [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]]);
        let cr = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        let (ln, lc) = ((nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt(), (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt());
        if ln == 0.0 {
            s.zero_normal += 1;
        } else if lc > 0.0 {
            let cos = (nrm[0] * cr[0] + nrm[1] * cr[1] + nrm[2] * cr[2]) / (ln * lc);
            if cos > 0.99 {
                s.normal_same += 1;
            } else if cos < -0.99 {
                s.normal_opposite += 1;
            } else {
                s.normal_other += 1;
            }
        }
        let mut dmax = 0f64;
        for j in 0..n {
            for k in j + 1..n {
                let (p, q) = (mv(j), mv(k));
                dmax = dmax.max(((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt());
            }
        }
        let size = mem::rd32(record + 0x20) as i32 as f64;
        if (size - dmax).abs() < 0.5 || size == dmax.round() {
            s.size_exact += 1;
        } else if (size - dmax).abs() <= 1.0 {
            s.size_near += 1;
        } else {
            s.size_other += 1;
            if s.size_example.is_none() {
                s.size_example = Some(format!("record {record:#x}: +20h {size}, largest distance {dmax:.2}"));
            }
        }
        // other candidates for +20h: twice the largest distance of a vertex from the model
        // origin, that distance, the plane's distance from the origin (n.v0 / 16384), the
        // largest distance from v0
        let len = |p: [f64; 3]| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        let rmax = (0..n).map(|j| len(mv(j))).fold(0.0, f64::max);
        let plane = ((nrm[0] * v0[0] + nrm[1] * v0[1] + nrm[2] * v0[2]) / 16384.0).abs();
        let from0 = (1..n).map(|j| {
            let p = mv(j);
            len([p[0] - v0[0], p[1] - v0[1], p[2] - v0[2]])
        });
        let from0 = from0.fold(0.0, f64::max);
        // twice the largest |v| over the whole part (its vertex list ends with x = 7FFFh;
        // cached by list address)
        let pmax = *part_max.entry(list).or_insert_with(|| {
            let mut pmax = 0f64;
            for i in 0..4096u32 {
                let p = list.wrapping_add(6 * i);
                if !mem::readable(p, 6) || mem::rd16(p) == 0x7fff {
                    break;
                }
                pmax = pmax.max(len([mem::rd16(p) as i16 as f64, mem::rd16(p + 2) as i16 as f64, mem::rd16(p + 4) as i16 as f64]));
            }
            pmax
        });
        for (i, cand) in [2.0 * rmax, rmax, plane, from0, 2.0 * pmax].iter().enumerate() {
            if (size - cand).abs() <= 1.0 {
                s.size_cands[i] += 1;
            }
        }
        // the set-ups' back-face test (ca89c; words: y at +0, x at +2 of a packed position):
        // (x0 - x1)(y0 - y2) - (y0 - y1)(x0 - x2) = cross(v1 - v0, v2 - v0) with y down;
        // culled if < -20 while [aa6b4] != 0 and bit 0 of [aa5b0] is clear
        let xy = |j: u32| {
            let p = mem::rd32(a.packed + off(j)) as i32;
            ((p >> 16) as i64, p as i16 as i64)
        };
        let ((x0, y0), (x1, y1), (x2, y2)) = (xy(0), xy(1), xy(2));
        let w = (x0 - x1) * (y0 - y2) - (y0 - y1) * (x0 - x2);
        s.winding[if w < -20 { 0 } else if w < 0 { 1 } else if w == 0 { 2 } else { 3 }] += 1;
        if mem::rd32(a.cull_on) != 0 && mem::rd32(a.sky_mirror) & 1 == 0 {
            s.cull_on += 1;
        }
        // the record normal against the eye in view space: n_view = R n (R: the part's
        // matrix at 7a9c4, x' = m0 x + m3 y + m6 z + ...), v0 from the transform
        let mut mm = [0f64; 12];
        for (i, v) in mm.iter_mut().enumerate() {
            *v = f32_at(a.matrix + 4 * i as u32) as f64;
        }
        let nv = [
            mm[0] * nrm[0] + mm[3] * nrm[1] + mm[6] * nrm[2],
            mm[1] * nrm[0] + mm[4] * nrm[1] + mm[7] * nrm[2],
            mm[2] * nrm[0] + mm[5] * nrm[1] + mm[8] * nrm[2],
        ];
        let p0 = a.view + 2 * off(0);
        let pv = [mem::rd32(p0) as i32 as f64, mem::rd32(p0 + 4) as i32 as f64, mem::rd32(p0 + 8) as i32 as f64];
        let dot = pv[0] * nv[0] + pv[1] * nv[1] + pv[2] * nv[2];
        s.normal_view[if dot > 0.0 { 0 } else if dot < 0.0 { 1 } else { 2 }] += 1;
    }
}

unsafe fn predict(c: &mut Check, mode: u8, record: u32, frame: u64) -> Option<Pending> {
    let a = c.addrs;
    unsafe {
        if !mem::readable(record, 0x24) {
            return None;
        }
        let n = mem::rd16(record + 6);
        let material = mem::rd32(a.model_base).wrapping_add(mem::rd16(record + 4) as i16 as i32 as u32);
        let s = &mut c.stats[mode as usize];
        s.polys += 1;
        bump(&mut s.counts, n);
        bump(&mut s.rec0, mem::rd16(record));
        if mode != 2 {
            bump(&mut s.rec2, mem::rd16(record + 2));
        }
        if !(3..=16).contains(&n) || !mem::readable(material, 0x14) || !mem::readable(record + 8, 2 * n as u32 + 2) {
            return None;
        }
        record_geometry(&a, s, &mut c.part_max, record, n as u32);
        let mtype = mem::rd32(material);
        bump(&mut s.mtypes, mtype);
        let mut p = Pending { mode, record, material, mtype, ..Default::default() };
        let (tex, entry, tint) = if mode == 2 { (0, 0, 0) } else { texture_of(&a, mode, material) };
        p.tex = tex;
        if mode != 2 && !mem::readable(entry, TEX_ENTRY) {
            return None;
        }
        p.state = state_of(&a, mode, entry, tint);
        if mode != 2 {
            bump(&mut s.formats, mem::rd32(entry + 0x24));
            if mode == 7 {
                bump(&mut s.blend_flag, mem::rd32(entry + 0x34));
            }
            if tint != 0 {
                s.tinted += 1;
            }
        }
        let (ss, ts) = if mode == 2 { (0.0, 0.0) } else { (f32_at(entry + 0x2c) as f64, f32_at(entry + 0x30) as f64) };
        let uv = if mode == 5 { a.env_uv } else { mem::rd32(a.uv_ptr) };
        // modes other than 2 load four vertex slots at most (the set-ups handle triangles
        // and quads)
        let used = if mode == 2 { n } else { n.min(4) };
        let (ox, oz) = if mode == 5 { env_offsets(&a) } else { (0, 0) };
        if mode == 5 {
            let (sx, sz) = (mem::rd32(a.env_scratch + 0xc) as i32, mem::rd32(a.env_scratch + 0x14) as i32);
            if (sx, sz) != (ox, oz) {
                s.env_offset_bad += 1;
            }
        }
        // the set-ups' screen test (ca7dd ff.): drawn directly when every vertex lies in
        // x 0 .. limit, y 0 .. limit (immediates patched for the view, e.g. 852 / 400),
        // otherwise through the 2D clipper
        let (xl, yl) = (mem::rd32(a.x_limit) as i32, mem::rd16(a.y_limit) as i16 as i32);
        let (mut minx, mut maxx, mut miny, mut maxy) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for j in 0..used as u32 {
            let packed = mem::rd32(a.packed + mem::rd16(record + 8 + 2 * j) as u32) as i32;
            let (px, py) = (packed >> 16, packed as i16 as i32);
            (minx, maxx, miny, maxy) = (minx.min(px), maxx.max(px), miny.min(py), maxy.max(py));
        }
        // (max x << 16 | max y) is compared with the x limit << 16 as one dword
        p.clip2d = minx < 0 || miny < 0 || maxy > yl || ((maxx << 16) | (maxy & 0xffff)) > xl;
        s.clip2d += p.clip2d as u64;
        for j in 0..used as u32 {
            let off = mem::rd16(record + 8 + 2 * j) as u32;
            let (x, y, oow) = screen(&a, off);
            let (u, v) = if mode == 2 { (0, 0) } else { (mem::rd16(uv + off) as i16, mem::rd16(uv + off + 2) as i16) };
            if mode == 5 && off % 6 == 0 {
                if let Some((eu, ev)) = env_uv(&a, off / 6, ox, oz) {
                    s.env_checked += 1;
                    if (eu, ev) != (u, v) {
                        s.env_bad += 1;
                        if s.first_mismatch.is_none() {
                            s.first_mismatch = Some(format!("frame {frame} reflection uv of vertex {}: table ({u}, {v}), formula ({eu}, {ev})", off / 6));
                        }
                    }
                }
            }
            let sow = (u as f64 * ss * oow as f64) as f32;
            let tow = (v as f64 * ts * oow as f64) as f32;
            p.verts.push(PVert { x, y, oow, sow, tow, u, v });
        }
        if mode == 2 {
            p.rgb = Some(rgb565(mem::rd16(record + 2) as u32));
        }
        if c.sample_frame == Some(frame) && c.sampled[mode as usize] < 3 {
            c.sampled[mode as usize] += 1;
            p.sample = true;
            let rec: Vec<String> = (0..0x24u32).step_by(2).map(|o| format!("{:04x}", mem::rd16(record + o))).collect();
            let mat: Vec<String> = (0..0x14u32).step_by(4).map(|o| format!("{:x}", mem::rd32(material + o))).collect();
            crate::info!(
                "polygon sample frame {frame} mode {mode}: record {record:#x} [{}] material {material:#x} [{}] texture {tex:#x} entry {entry:#x} scale ({ss}, {ts}) state {:?}",
                rec.join(" "),
                mat.join(" "),
                p.state
            );
            for (j, v) in p.verts.iter().enumerate() {
                crate::info!(
                    "    predicted vertex {j}: x {} y {} 1/w {:e} uv ({}, {}) s {} t {}{}",
                    v.x,
                    v.y,
                    v.oow,
                    v.u,
                    v.v,
                    v.sow / v.oow,
                    v.tow / v.oow,
                    p.rgb.map(|c| format!(" rgb {c:?}")).unwrap_or_default()
                );
            }
        }
        Some(p)
    }
}

/// Relative / absolute closeness for values the x87 rounds once and we round twice.
fn close(a: f32, b: f32) -> bool {
    a == b || (a - b).abs() as f64 <= 1e-6 * (a.abs().max(b.abs()) as f64) + 1e-9
}

fn triangle(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let frame = frame_of(m);
    let Some(g) = m.glide.as_ref() else { return HookResult::Resume(tramp) };
    let st = g.st;
    let palette = g.palette;
    with_check(|c| {
        c.tick(frame);
        let a = c.addrs;
        let mode = unsafe { mem::rd32(a.mode) } as usize;
        if mode > 7 {
            return;
        }
        let Some(p) = c.pending.as_mut().filter(|p| p.mode as usize == mode) else {
            let s = &mut c.stats[mode];
            s.tris += 1;
            s.tris_unpredicted += 1;
            return;
        };
        let s = &mut c.stats[mode];
        p.tris += 1;
        s.tris += 1;
        let vb = unsafe { mem::rd32(a.vert_ptr) };
        let addrs = [a.verts, vb, vb + VSTRIDE];
        let mut problems: Vec<String> = Vec::new();
        // the builder's fan: triangle b (1-based) is record vertices (0, b, b + 1) for the
        // textured modes (vertex buffer cf4b0 in record order); mode 2 starts at a rotated
        // vertex (d02cc: the one after the rightmost), so it is matched by position only
        let b = p.tris as usize;
        let fan = [0, b, b + 1];
        for (k, &va) in addrs.iter().enumerate() {
            let rd = |o: u32| unsafe { f32_at(va + o) };
            let (x, y, r, gg, b, oow, sow, tow) = (rd(0), rd(4), rd(0xc), rd(0x10), rd(0x14), rd(0x20), rd(0x24), rd(0x28));
            let in_order = (p.mode != 2 && !p.clip2d).then(|| p.verts.get(fan[k]).filter(|v| v.x == x && v.y == y)).flatten();
            if p.mode != 2 && !p.clip2d && in_order.is_none() {
                s.order_bad += 1;
                problems.push(format!("vertex {k} ({x}, {y}) not record vertex {} of an unclipped polygon", fan[k]));
            }
            let (pv, kept) = match in_order {
                Some(v) => (v, false),
                None => match p.verts.iter().find(|v| v.x == x && v.y == y) {
                    Some(v) => (v, p.mode != 2),
                    None => {
                        s.verts_2d += 1;
                        if p.sample {
                            crate::info!("    triangle {} vertex {k}: x {x} y {y} (made by the 2D clipper)", p.tris);
                        }
                        continue;
                    }
                },
            };
            if kept {
                s.verts_kept += 1;
            } else {
                s.verts += 1;
            }
            if p.mode == 2 {
                if p.rgb != Some([r, gg, b]) {
                    s.rgb_bad += 1;
                    problems.push(format!("rgb ({r}, {gg}, {b}) predicted {:?}", p.rgb));
                }
            } else if kept {
                // a vertex of a polygon the 2D clipper rebuilt, found by its whole-pixel
                // position (another vertex may share it)
                if !close(oow, pv.oow) || !close(sow, pv.sow) || !close(tow, pv.tow) {
                    s.kept_bad += 1;
                }
            } else {
                if !close(oow, pv.oow) {
                    s.oow_bad += 1;
                    problems.push(format!("1/w {oow:e} predicted {:e}", pv.oow));
                }
                let es = (sow / oow - pv.sow / pv.oow).abs().max((tow / oow - pv.tow / pv.oow).abs()) as f64;
                s.st_max_err = s.st_max_err.max(es);
                if !close(sow, pv.sow) || !close(tow, pv.tow) {
                    s.st_bad += 1;
                    problems.push(format!("s/w t/w ({sow:e}, {tow:e}) predicted ({:e}, {:e})", pv.sow, pv.tow));
                }
            }
            if p.sample {
                crate::info!(
                    "    triangle {} vertex {k}: game x {x} y {y} 1/w {oow:e} s {} t {} rgba ({r}, {gg}, {b}, {}) | predicted s {} t {}",
                    p.tris,
                    sow / oow,
                    tow / oow,
                    rd(0x1c),
                    pv.sow / pv.oow,
                    pv.tow / pv.oow
                );
            }
        }
        // Glide state at this triangle
        let ps = p.state;
        let mut bad = [false; 11];
        bad[0] = st.color_combine != ps.color_combine;
        bad[1] = st.alpha_combine != ps.alpha_combine;
        bad[2] = st.blend != ps.blend;
        bad[3] = st.chromakey != ps.chromakey;
        bad[4] = ps.chromakey_value.is_some_and(|v| v != st.chromakey_value);
        bad[5] = ps.clamp.is_some_and(|v| v != st.clamp);
        bad[6] = ps.constant.is_some_and(|v| v != st.constant_color);
        if let Some(e) = ps.tex_entry {
            let info = unsafe { crate::glide::tex::TexInfo::read(e + 0x18) }.layout();
            let slot = unsafe { mem::rd32(e + 4) };
            let start = unsafe { mem::rd32(a.tmu_slots.wrapping_add(slot.wrapping_mul(8))) };
            bad[7] = st.tex.info != info || st.tex.start != start;
            if info.format == GR_TEXFMT_P_8 {
                let pal = unsafe { mem::rd32(e + 0xc) };
                bad[8] = !mem::readable(pal, 1024) || (0..256u32).any(|i| unsafe { mem::rd32(pal + 4 * i) } != palette[i as usize]);
            }
        }
        // bilinear filtering is set once (sky, cb7be) and never changed by the builder
        bad[9] = p.mode != 2 && st.filter != [GR_TEXTUREFILTER_BILINEAR; 2];
        bad[10] = st.fog_mode != ps.fog;
        for (i, b) in bad.iter().enumerate() {
            if *b {
                s.state_bad[i] += 1;
                problems.push(format!("state {}", STATE_FIELDS[i]));
            }
        }
        if p.sample {
            crate::info!("    triangle {} Glide state: {:?} constant {:#x} chroma {} {:#x} blend {:?} clamp {:?} tex {:?}", p.tris, st.color_combine, st.constant_color, st.chromakey, st.chromakey_value, st.blend, st.clamp, st.tex);
        }
        if !problems.is_empty() && s.first_mismatch.is_none() {
            s.first_mismatch = Some(format!("frame {frame} record {:#x} material {:#x} type {:#x} texture {:#x}: {}", p.record, p.material, p.mtype, p.tex, problems.join("; ")));
        }
    });
    HookResult::Resume(tramp)
}

fn sky_entry(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let frame = frame_of(m);
    with_check(|c| {
        c.tick(frame);
        c.pending = None;
        let (sky, ground) = unsafe { predict_sky(&c.addrs) };
        c.sky = sky;
        c.ground = ground;
    });
    HookResult::Resume(tramp)
}

/// The sky quad and the ground quad as 7b218 and the builder's mode 8 compute them.
unsafe fn predict_sky(a: &Addrs) -> (Option<[PVert; 4]>, Option<(f32, u32)>) {
    unsafe {
        if mem::rd32(a.sky_on) == 0 {
            return (None, None);
        }
        let cam = mem::rd32(a.camera_ptr);
        let heading = mem::rd32(cam + 4);
        let sky_s = (0x7ffu32.wrapping_sub(heading) & 0x3ff) as f32 * 0.25;
        // horizon: ((-54152 * [798ac]) low 32 bits >> 7) / -54152 + centre + 28h - 200h + 2
        let pitch = mem::rd32(a.pitch) as i32;
        let h = ((-54152i32).wrapping_mul(pitch) >> 7) / -54152 + mem::rd32(a.center_y) as i32 + 0x28 - 0x200 + 2;
        let ah = h.unsigned_abs() as f32;
        let n1 = (mem::rd32(a.view_h) as i32 - 1) as f32;
        let mut yb = (0x1ff - h.unsigned_abs() as i32) as f32;
        let mut te = 0.0f32;
        if yb > n1 {
            te = ((yb as f64 - n1 as f64) * 0.5) as f32;
            yb = n1;
        }
        // 553ac rounds toward zero
        let ytab = |i: i32| f32_at(a.y_table.wrapping_add((4 * i) as u32));
        let yo = mem::rd32(a.y_offset) as i32;
        let top = ytab(yo);
        let yh_t = ytab(yb.trunc() as i32);
        let bottom_t = ytab(n1.trunc() as i32);
        let xt = mem::rd32(a.x_table_ptr);
        let xo = mem::rd32(a.x_offset) as i32;
        let left = f32_at(xt.wrapping_add((4 * xo) as u32));
        // `fld [eax + 9FCh]` (cb971): the right edge's table offset, 4 * 639 in the original
        // and patched for widescreen (crate::widescreen)
        let right = f32_at(xt.wrapping_add((4 * xo) as u32).wrapping_add(mem::rd32(a.sky_right)));
        let (sl, sr) = if mem::rd32(a.sky_mirror) != 0 { (sky_s + 160.0 - 1.0, sky_s) } else { (sky_s, sky_s + 160.0 - 1.0) };
        let t0 = ah * 0.5;
        let yh = top + yh_t;
        let tb = 255.0 - te;
        let v = |x: f32, y: f32, s: f32, t: f32| PVert { x, y, oow: 1.0, sow: s, tow: t, u: 0, v: 0 };
        let sky = [v(left, top, sl, t0), v(right, top, sr, t0), v(right, yh, sr, tb), v(left, yh, sl, tb)];
        let ground = if yh_t < bottom_t {
            let on = mem::rd32(a.sky_on) != 0;
            let c = if on { mem::rd32(a.ground_sky) } else { mem::rd32(a.ground_flat) };
            Some((top + bottom_t, rgb565_888(c & 0xffff)))
        } else {
            None
        };
        (Some(sky), ground)
    }
}

fn sky_drawn(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let frame = frame_of(m);
    let Some(g) = m.glide.as_ref() else { return HookResult::Resume(tramp) };
    let st = g.st;
    with_check(|c| {
        c.tick(frame);
        let a = c.addrs;
        let Some(sky) = c.sky else { return };
        let k = &mut c.sky_stats;
        k.quads += 1;
        let mut problems = Vec::new();
        for (i, p) in sky.iter().enumerate() {
            let va = a.sky_verts + VSTRIDE * i as u32;
            let rd = |o: u32| unsafe { f32_at(va + o) };
            let (x, y, oow, s, t) = (rd(0), rd(4), rd(0x20), rd(0x24), rd(0x28));
            if (x, y, s, t) != (p.x, p.y, p.sow, p.tow) {
                problems.push(format!("vertex {i} ({x}, {y}, s {s}, t {t}) predicted ({}, {}, s {}, t {})", p.x, p.y, p.sow, p.tow));
            }
            if c.sample_frame == Some(frame) {
                crate::info!("sky sample frame {frame} vertex {i}: game x {x} y {y} 1/w {oow} s {s} t {t} | predicted x {} y {} s {} t {}", p.x, p.y, p.sow, p.tow);
            }
        }
        let info = unsafe { crate::glide::tex::TexInfo::read(a.sky_texture) }.layout();
        let start = unsafe { mem::rd32(a.sky_tmu) };
        if st.tex.info != info || st.tex.start != start {
            problems.push("texture".into());
        }
        if st.clamp != [GR_TEXTURECLAMP_WRAP; 2] || st.filter != [GR_TEXTUREFILTER_BILINEAR; 2] || st.chromakey != 0 {
            problems.push(format!("clamp {:?} filter {:?} chroma {}", st.clamp, st.filter, st.chromakey));
        }
        if st.color_combine != [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_TEXTURE, 0] {
            problems.push(format!("colour combine {:?}", st.color_combine));
        }
        if !problems.is_empty() {
            k.bad += 1;
            if k.first_mismatch.is_none() {
                k.first_mismatch = Some(format!("frame {frame}: {}", problems.join("; ")));
            }
        }
    });
    HookResult::Resume(tramp)
}

fn ground_drawn(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let frame = frame_of(m);
    let Some(g) = m.glide.as_ref() else { return HookResult::Resume(tramp) };
    let st = g.st;
    with_check(|c| {
        let a = c.addrs;
        let k = &mut c.sky_stats;
        k.ground += 1;
        let Some((bottom, color)) = c.ground else {
            k.ground_bad += 1;
            return;
        };
        // v0 and v1 (173e4c, 173e88) are moved to the bottom; v2 / v3 stay on the horizon
        let y0 = unsafe { f32_at(a.sky_verts + 4) };
        let y1 = unsafe { f32_at(a.sky_verts + VSTRIDE + 4) };
        let cc = [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_CONSTANT, 0];
        if y0 != bottom || y1 != bottom || st.constant_color & 0xff_ffff != color || st.color_combine != cc {
            k.ground_bad += 1;
            if k.first_mismatch.is_none() {
                k.first_mismatch = Some(format!(
                    "frame {frame} ground: y {y0} {y1} predicted {bottom}, colour {:#x} predicted {color:#x}, combine {:?}",
                    st.constant_color, st.color_combine
                ));
            }
        }
        if c.sample_frame == Some(frame) {
            crate::info!("ground sample frame {frame}: bottom y {y0} {y1} colour {:#x} | predicted y {bottom} colour {color:#x}", st.constant_color);
        }
    });
    HookResult::Resume(tramp)
}
