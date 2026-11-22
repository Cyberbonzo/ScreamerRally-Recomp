//! Building the scene description from the game's memory (`docs/04-native-renderer.md`,
//! wave 2 topic W2-1; what is built from where: `docs/re/native/extract.md`). The
//! integration (topic W2-3) calls [`build_scene`] once per race, at its first race frame.
//!
//! * **Textures**: every texture record in use (track, misc, cars, the environment map;
//!   `textures.md`), in the game's own format (P8 with the record's palette, the palette the
//!   game downloads before every bind; RGB565; ARGB4444), then the background (sky) picture,
//!   read back from the emulated texture memory (its RAM copy is freed after its download).
//! * **Materials**: one per (game material, culling, decal pull), from the material's type
//!   through the game's own dispatch table (`polygons.md`), mapped as the mode table of
//!   `docs/04-native-renderer.md`; animated materials and the cars' 128-entry state lists
//!   get a [`TexAnim`] over `texture_frames`; reflection polygons (mode 5) take the
//!   environment library's material 0, as the game does.
//! * **Meshes**: one per world model, every part at its highest detail level present, the
//!   part placement baked in (model space = the object's space), triangles as fans of the
//!   polygon records in record order (front faces clockwise on screen, as the game's;
//!   every material culls the counter-clockwise winding, see `WORLD_CULL`);
//!   then, for wave 3, one mesh per part of every scene-table model (cars, shadows, smoke,
//!   tyre marks) in the part's own space. Vertex `i` of a part is its model vertex `i`
//!   (flat polygons get extra vertices for their colour).
//! * **Statics**: every object of every grid cell, `world = Ro v + to` (the part placement
//!   being in the mesh), with its cell, bounding sphere and the ground test.
//!
//! `SR_EXTRACT_CHECK=FIRST[,LAST[,DIR]]` (diagnostics, off by default; not together with
//! `SR_POLY_CHECK`, whose observers sit at the same addresses): builds the scene at every
//! new race from game frame FIRST on and, until frame LAST, compares every part the engine
//! transforms with the scene (its vertices through the instance's transform and topic D's
//! camera against the engine's view positions), every polygon the engine draws with the
//! scene's polygon (texture and texture coordinates, mode and Glide state), and what the
//! engine's back-face selection keeps against the scene's winding. Summaries go to the log
//! (`extract: check ...`) and, with DIR, to `DIR/extract_check.txt` and, per build,
//! `DIR/grounded<n>.tsv` (the ground test of every static).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use super::scene::{
    AnimSource, Blend, BlendFactor, Combine, Cull, Instance, Layer, Material, Mesh, MeshRange, Mipmaps, SceneData, TexAnim, TexFormat, TexGen, TextureData, Transform,
    Transparency, Vertex, Wrap,
};
use super::world::{self, Model, Reader, Source, World};
use super::{polygons, textures, transforms};
use crate::Machine;
use crate::cpu::HookFrame;
use crate::glide::consts::*;
use crate::hooks::HookResult;
use crate::mem;

/// What the runtime needs per static instance to decide its visibility (the index matches
/// `SceneData::statics`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StaticMeta {
    /// The world cell holding the object (row, column of the game's 256 x 256 grid).
    pub cell: (u16, u16),
    /// Ground (another surface) lies under the object's base: it may be drawn beyond the
    /// game's visibility lists without floating (`docs/04-native-renderer.md`, "Decisions").
    pub grounded: bool,
    /// Bounding sphere in world coordinates.
    pub center: [f64; 3],
    pub radius: f32,
}

/// The scene of a race and what goes with it.
#[derive(Clone, Debug, Default)]
pub struct SceneBuild {
    pub scene: SceneData,
    /// One entry per `scene.statics`.
    pub statics: Vec<StaticMeta>,
    /// Index into `scene.textures` of the background (sky) picture, if there is one.
    pub sky_texture: Option<u32>,
    /// The part meshes of the scene-table models (cars, shadows, smoke, tyre marks; wave 3,
    /// `native::dynamics`): one per (model, part slot, detail level) present, in the part's
    /// own space.
    pub part_meshes: Vec<PartMesh>,
}

/// A part mesh of a scene-table model: part slot `slot` of model `model` at detail level
/// `level` (`world.md`: model +18h + 34h level + 4 slot), placed per frame from instance B.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartMesh {
    pub model: u32,
    pub slot: u8,
    pub level: u8,
    pub mesh: u32,
}

/// Build the scene of the race just loaded (call at its first race frame); `None` if the
/// world cannot be read.
pub fn build_scene(m: &crate::Machine) -> Option<SceneBuild> {
    build(m).map(|(b, _)| b)
}

/// A menu's 3D scene (W4-2, `docs/re/native/menus.md`; the menus have no scene table): the
/// loaded textures; with `world` (a menu drawn with `78d84`) its cells' statics, none of
/// them grounded (the menus show the game's cells only), and its background picture; and
/// one part mesh per part slot and level of every other model of the model table (the
/// menu's objects, placed per frame from the instance list like a race's scene-table
/// instances).
pub fn build_menu_scene(m: &crate::Machine, world: bool) -> Option<SceneBuild> {
    build_with(m, Some(world)).map(|(mut b, _)| {
        for s in b.statics.iter_mut() {
            s.grounded = false;
        }
        b
    })
}

// ---------------------------------------------------------------------------------------
// Decisions (docs/re/native/extract.md).

/// Back-face culling of world objects' materials. The set-ups' screen test is off for them
/// ([aa6b4] = 0 in `82a04`, 1 in `828c4` for scene-table instances), but the engine's
/// polygon list builder (`7ac50`, all three paths) drops every polygon whose record normal
/// faces away from the camera, for every object: measured by the check within the original
/// draw distance, the counter-clockwise triangles the engine draws cover 0.003 % of the
/// area it draws, the back faces it drops 48 % more (`extract.md`, "Culling"). The world
/// is therefore culled like the game draws it (front faces clockwise on screen).
const WORLD_CULL: Cull = Cull::CounterClockwise;
/// Scene-table instances (cars, shadows, smoke, marks): culled by the game; front faces are
/// clockwise on screen, so the counter-clockwise winding is removed.
const INSTANCE_CULL: Cull = Cull::CounterClockwise;

/// The ground test ([`ground_test`]): the track sections are the ground; another object is
/// grounded when at least `GROUND_SHARE` of its base points (its vertices within
/// `BASE_BAND` units of its lowest point, at most `BASE_POINTS` of them spread over the
/// list) have a grounded static's surface under them within `GROUND_BELOW` units, or over
/// them within `GROUND_ABOVE` units (sunk into it). The bounds sit in the gaps of the
/// distribution measured on Rally China (`extract.md`, "Grounded").
const BASE_BAND: f64 = 50.0;
const BASE_POINTS: usize = 16;
const GROUND_BELOW: f64 = 200.0;
const GROUND_ABOVE: f64 = 1000.0;
const GROUND_SHARE: f64 = 0.5;
/// Bucket size (x, z) of the ground test's triangle grid.
const GROUND_GRID: f64 = 1024.0;

/// `SR_DYN_CULL` (diagnostics, wave 3): `0` = scene-table materials without back-face
/// culling, `flat` = their flat (mode 2) materials only.
fn dyn_cull_env() -> &'static Option<String> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| std::env::var("SR_DYN_CULL").ok().map(|v| v.trim().to_string()))
}

/// What kind of object a mesh is built for (decides culling and decal pulls).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    World,
    Car,
    Shadow,
    Smoke,
    Marks,
    Other,
}

impl Kind {
    fn of_scene_name(name: &str) -> Kind {
        let n = |p: &str| name.starts_with(p);
        if n("auto") {
            Kind::Car
        } else if n("ashadow") {
            Kind::Shadow
        } else if n("asmoke") || n("a2smoke") {
            Kind::Smoke
        } else if n("aztri") || n("az1tri") {
            Kind::Marks
        } else {
            Kind::Other
        }
    }
    fn cull(self) -> Cull {
        if self == Kind::World {
            WORLD_CULL
        } else if dyn_cull_env().as_deref() == Some("0") {
            // diagnostics (wave 3): the scene-table models without back-face culling
            Cull::None
        } else {
            INSTANCE_CULL
        }
    }
    /// Mode-7 decals: depth pull in world units (the Glide path's: shadow 250, smoke 500,
    /// marks 120; other objects 120).
    fn pull(self) -> f32 {
        match self {
            Kind::Shadow => 250.0,
            Kind::Smoke => 500.0,
            _ => 120.0,
        }
    }
    /// Decals that stand on the ground (`Layer::Decal::stand`): the smoke and dust puffs,
    /// camera-facing cards of 600 x 580 units centred on their position, which the game
    /// places 20 units over the surface (median of the race's puffs, `dynvis` statistics;
    /// `docs/re/native/dynamics.md`, "Smoke").
    fn stand(self) -> Option<f32> {
        (self == Kind::Smoke).then_some(SMOKE_STAND)
    }
}

/// Height of a puff's position over the ground it stands on (`Kind::stand`).
pub const SMOKE_STAND: f32 = 20.0;

// ---------------------------------------------------------------------------------------
// What the build knows about the guest side (for the check; never leaves this module).

#[derive(Clone, Debug, Default)]
struct PolyInfo {
    /// The polygon record (guest address).
    record: u32,
    material: u32,
    /// First index of its triangles in the mesh's index list (n - 2 triangles).
    first: u32,
    /// Its vertices in record order: mesh vertex ids and the record's vertex offsets.
    vids: Vec<u32>,
    offs: Vec<u16>,
}

#[derive(Clone, Debug, Default)]
struct PartInfo {
    slot: u32,
    /// The part data (the highest level present) the mesh was built from.
    data: u32,
    /// Mesh vertices vbase .. vbase + nverts are the part's model vertices, in order.
    vbase: u32,
    nverts: u32,
    polys: Vec<PolyInfo>,
}

#[derive(Clone, Debug, Default)]
struct MeshInfo {
    model: u32,
    kind: Option<Kind>,
    name: String,
    parts: Vec<PartInfo>,
}

#[derive(Clone, Debug, Default)]
struct Extra {
    meshes: Vec<MeshInfo>,
    /// Per static: the object's address.
    static_obj: Vec<u32>,
    /// Per material: the game's builder mode.
    mat_mode: Vec<u8>,
    /// Record id -> texture index.
    tex_of_record: HashMap<u32, u32>,
    /// (model index, part slot) -> mesh of a scene-table model's part (wave 3).
    part_mesh: HashMap<(u32, u32), u32>,
    /// Every level of the scene-table models' parts (wave 3, `native::dynamics`).
    part_levels: Vec<PartMesh>,
    /// Per static: the ground test.
    ground: Vec<Ground>,
    sky_hash: u64,
    summary: String,
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------------------------------
// The build.

#[derive(Default)]
struct Counts {
    textures_skipped: u32,
    frames_missing: u32,
    chroma_differs: u32,
    keyed_without_texels: Vec<String>,
    modes: BTreeMap<u8, u32>,
    mode3: u32,
    untextured: u32,
    unknown_type: BTreeMap<u32, u32>,
    bad_polygons: u32,
    no_uv: u32,
    normals: u32,
    animated_models: u32,
    billboards: u32,
    missing_models: u32,
    empty_meshes: u32,
    multi_part: u32,
    placed_parts: u32,
    /// Scene-table polygons: the angle between a later fan triangle and the first, in
    /// bands < 1, < 5, < 15, < 45 degrees and more; second triangles made double-sided.
    folds: [u32; 5],
    folded: u32,
}

/// A later fan triangle of a scene-table polygon at least this far (degrees) out of the
/// first triangle's plane is drawn from both sides (`build_mesh`).
const FOLD_DEGREES: f64 = 5.0;

struct Builder<'a> {
    m: &'a Machine,
    scene: SceneData,
    extra: Extra,
    /// (material address, culling, decal pull) -> material.
    materials: HashMap<(u32, Cull, u32), u32>,
    /// Data block of the environment-map library ([aa6a8]), 0 when none.
    env_base: u32,
    counts: Counts,
    /// `build_mesh` takes this detail level instead of the highest present (the
    /// scene-table models' level variants, wave 3).
    level: Option<usize>,
}

fn rd32(a: u32) -> u32 {
    unsafe { mem::rd32(a) }
}

/// Model -> world of a rotation by game angles (topic D's float model: true sine and
/// cosine, `R = Ry(-a1) Rx(a0) Rz(a2)`).
fn rotation(a: [i32; 3]) -> transforms::Mat3 {
    transforms::rotation(a.map(|x| (x & 0x7ff) as f64))
}

impl<'a> Builder<'a> {
    fn add_textures(&mut self) {
        for rec in textures::records() {
            let Some(t) = textures::texture(&rec) else {
                self.counts.textures_skipped += 1;
                continue;
            };
            let format = match t.format {
                textures::TexFormat::Indexed8 { palette } => TexFormat::Indexed8 { palette },
                textures::TexFormat::Rgb565 => TexFormat::Rgb565,
                textures::TexFormat::Argb4444 => TexFormat::Argb4444,
            };
            let idx = self.scene.textures.len() as u32;
            self.scene.textures.push(TextureData { width: t.width, height: t.height, format, pixels: t.pixels, mipmaps: Mipmaps::None });
            self.extra.tex_of_record.insert(rec.id, idx);
        }
    }

    /// The background picture (256 x 256 RGB565), from the emulated texture memory at
    /// [173f80] (its RAM copy [14734c] is freed once downloaded, `textures.md`).
    fn add_sky(&mut self) -> Option<u32> {
        let m = self.m;
        let info = unsafe { crate::glide::tex::TexInfo::read(m.img.link(textures::SKY_INFO)) };
        if info.format != GR_TEXFMT_RGB_565 || info.small_lod != info.large_lod {
            return None;
        }
        let (w, h) = crate::glide::tex::level_size(info.large_lod, info.aspect);
        let start = rd32(m.img.link(textures::SKY_TMU)) as usize;
        let n = (w * h * 2) as usize;
        let pixels = m.glide.as_ref()?.tmu.ram.get(start..start + n)?.to_vec();
        self.extra.sky_hash = fnv(&pixels);
        let idx = self.scene.textures.len() as u32;
        self.scene.textures.push(TextureData { width: w, height: h, format: TexFormat::Rgb565, pixels, mipmaps: Mipmaps::None });
        Some(idx)
    }

    /// The scene material of game material `mat` (an entry of the directory at `base`) for
    /// objects of `kind`, and the builder mode of its polygons.
    fn material(&mut self, base: u32, mat: u32, kind: Kind) -> Option<(u32, u8)> {
        let cull = kind.cull();
        let pull = kind.pull();
        let key = (mat, cull, pull.to_bits());
        if let Some(&i) = self.materials.get(&key) {
            return Some((i, self.extra.mat_mode[i as usize]));
        }
        if !mem::readable(mat, 0x20) {
            return None;
        }
        let ty = rd32(mat);
        let Some(mode) = polygons::mode_of_type(self.m, ty) else {
            *self.counts.unknown_type.entry(ty).or_default() += 1;
            return None;
        };
        // the frame list: mode 5 takes material 0 of the environment library (d59c0)
        let (lbase, lmat) = if mode == 5 && self.env_base & 0x0fff_ffff != 0 { (self.env_base, self.env_base) } else { (base, mat) };
        let list_off = rd32(lmat + 4);
        let mut records = Vec::new();
        if mode != 2 && list_off != 0 && mem::readable(lbase.wrapping_add(list_off), 8) {
            let list = lbase.wrapping_add(list_off);
            let count = rd32(list);
            if count <= 256 && mem::readable(list + 8, 4 * count) {
                records = (0..count).map(|f| rd32(list + 8 + 4 * f)).collect();
            }
        }
        let mut texs = Vec::with_capacity(records.len());
        for id in &records {
            match self.extra.tex_of_record.get(id) {
                Some(&t) => texs.push(t),
                None => {
                    self.counts.frames_missing += 1;
                    if let Some(&t) = texs.first() {
                        texs.push(t);
                    }
                }
            }
        }
        let rec0 = records.first().map(|&id| unsafe { textures::Record::read(id) });
        let tex0 = texs.first().copied();
        let textured = |transparency: Transparency, wrap: Wrap| Material {
            texture: tex0,
            transparency,
            combine: Combine::Replace,
            wrap: [wrap; 2],
            bilinear: true,
            ..Default::default()
        };
        let mut out = match mode {
            2 => Material::flat(),
            _ if tex0.is_none() => {
                self.counts.untextured += 1;
                Material::flat()
            }
            1 | 3 => {
                let key = rec0.map_or(0, |r| r.chroma);
                if records.iter().any(|&id| unsafe { textures::Record::read(id) }.chroma != key) {
                    self.counts.chroma_differs += 1;
                }
                if let Some(t) = rec0.as_ref().and_then(textures::texture) {
                    let k = (key >> 16) & 0xff | ((key >> 8) & 0xff) << 8 | (key & 0xff) << 16;
                    if !t.rgba().iter().any(|&p| p & 0xff_ffff == k) {
                        self.counts.keyed_without_texels.push(format!("record {} key {key:06x}", t.record.id));
                    }
                }
                if mode == 1 {
                    textured(Transparency::ChromaKey { key }, Wrap::Clamp)
                } else {
                    // colour-keyed and blended (ONE, SRC_COLOR): the contract cannot carry
                    // both; the blend is kept (not seen on China)
                    self.counts.mode3 += 1;
                    textured(Transparency::Blended(Blend { src: BlendFactor::One, dst: BlendFactor::SrcColor }), Wrap::Clamp)
                }
            }
            5 => Material { texgen: TexGen::Reflection { scale: [1.0, 1.0], offset: [0.0, 0.0] }, ..textured(Transparency::Opaque, Wrap::Repeat) },
            6 => textured(Transparency::Blended(Blend::ALPHA), Wrap::Clamp),
            7 => {
                // texture record +34h != 0: (DST_COLOR, SRC_COLOR) = 2 x src x dst
                let blend = if rec0.is_some_and(|r| r.grey != 0) { Blend { src: BlendFactor::DstColor, dst: BlendFactor::SrcColor } } else { Blend::ALPHA };
                Material { layer: Layer::Decal { pull, stand: kind.stand() }, ..textured(Transparency::Blended(blend), Wrap::Clamp) }
            }
            // 0 and 4 (identical in the builder)
            _ => textured(Transparency::Opaque, Wrap::Clamp),
        };
        out.cull = cull;
        if mode == 2 && kind != Kind::World && dyn_cull_env().as_deref() == Some("flat") {
            out.cull = Cull::None;
        }
        // shadows, smoke and tyre marks lie on other surfaces whatever their mode (China's
        // tyre marks are mode 0): decals, as the Glide path draws them (wave 3)
        if matches!(kind, Kind::Shadow | Kind::Smoke | Kind::Marks) && out.layer == Layer::World {
            out.layer = Layer::Decal { pull, stand: kind.stand() };
        }
        if texs.len() > 1 {
            let first = self.scene.texture_frames.len() as u32;
            self.scene.texture_frames.extend_from_slice(&texs);
            out.anim = Some(TexAnim { first, count: texs.len() as u32, source: if mode == 5 { AnimSource::Env } else { AnimSource::Phase } });
        }
        let i = self.scene.materials.len() as u32;
        self.scene.materials.push(out);
        self.extra.mat_mode.push(mode);
        *self.counts.modes.entry(mode).or_default() += 1;
        self.materials.insert(key, i);
        Some((i, mode))
    }

    /// A part's placement in its object, from the model's placement block (reached through
    /// the slot's level-0 data, `827d8`; an entry of 0 reads the model itself), or the
    /// current animation frame's matrix for animated models (`826b4`).
    fn part_placement(&mut self, w: &World, r: &mut Reader, model: &Model, slot: usize) -> Option<transforms::Affine> {
        let part0 = model.addr.wrapping_add(model.levels[0][slot]);
        let placement = model.addr.wrapping_add(r.u32(part0 + 0xc)?);
        if model.anim_frames >= 0 {
            self.counts.animated_models += 1;
            let frame = (model.anim_frames.wrapping_mul(w.clock) >> 16).wrapping_mul(0x30) as u32;
            let at = placement.wrapping_add(0x1c).wrapping_add(frame);
            let mut e = [0i32; 12];
            for (k, v) in e.iter_mut().enumerate() {
                *v = r.i16(at + 2 * k as u32)? as i32;
            }
            return Some(transforms::Affine::from_engine(&e));
        }
        let a = [r.i32(placement + 4)?, r.i32(placement + 8)?, r.i32(placement + 0xc)?];
        let p = [r.i32(placement + 0x10)?, r.i32(placement + 0x14)?, r.i32(placement + 0x18)?];
        if a.iter().all(|&x| x & 0x7ff == 0) && p == [0; 3] {
            return Some(transforms::Affine::IDENTITY);
        }
        self.counts.placed_parts += 1;
        Some(transforms::Affine::new(rotation(a), p.map(|v| v as f64)))
    }

    /// A mesh from parts of a model: `slots` = (slot, placement baked into the vertices).
    fn build_mesh(&mut self, w: &World, r: &mut Reader, model: &Model, slots: &[(usize, transforms::Affine)], kind: Kind) -> (Mesh, MeshInfo) {
        let mut mesh = Mesh::default();
        let mut info = MeshInfo { model: model.index, kind: Some(kind), ..Default::default() };
        // triangles per material, and per polygon (part, polygon, material, offset in its list)
        let mut by_mat: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        let mut placed: Vec<(usize, usize, u32, u32)> = Vec::new();
        for &(slot, ref place) in slots {
            let pick = match self.level {
                Some(l) => (model.levels[l][slot] != 0).then(|| (l, model.addr.wrapping_add(model.levels[l][slot]))),
                None => world::best_level(model, slot),
            };
            let Some((_, data)) = pick else { continue };
            let Some(part) = w.part(data).cloned() else { continue };
            if info.name.is_empty() {
                info.name = part.name.clone();
            }
            let Some(verts) = world::vertices(r, model.addr.wrapping_add(part.verts)) else { continue };
            let n = verts.len() as u32;
            let vbase = mesh.vertices.len() as u32;
            // vertex normals ([7bad0]: count, then 3 floats per vertex) where the part has
            // them, texture coordinates ([7bad4]: u, v, a third word per vertex)
            let normals = read_normals(r, model.addr.wrapping_add(part.f20), n);
            if normals.is_some() {
                self.counts.normals += 1;
            }
            let uv_at = model.addr.wrapping_add(part.f24);
            let has_uv = r.ok(uv_at, 6 * n.max(1));
            if !has_uv {
                self.counts.no_uv += 1;
            }
            for (i, v) in verts.iter().enumerate() {
                let p = place.apply(v.map(|c| c as f64));
                let normal = normals.as_ref().map_or([0.0; 3], |ns| {
                    let q = place.r;
                    let nn = ns[i];
                    [0, 1, 2].map(|k| (q[k][0] * nn[0] + q[k][1] * nn[1] + q[k][2] * nn[2]) as f32)
                });
                let uv = if has_uv {
                    let a = uv_at + 6 * i as u32;
                    [r.i16(a).unwrap_or(0) as f32 / 256.0, r.i16(a + 2).unwrap_or(0) as f32 / 256.0]
                } else {
                    [0.0; 2]
                };
                mesh.vertices.push(Vertex { pos: p.map(|c| c as f32), normal, uv, color: [255; 4] });
            }
            let mut pinfo = PartInfo { slot: slot as u32, data, vbase, nverts: n, polys: Vec::new() };
            let Some((records, _)) = world::polygons(r, &part) else {
                info.parts.push(pinfo);
                continue;
            };
            for rec in records {
                let Some((mat_off, offs)) = world::polygon(r, rec) else {
                    self.counts.bad_polygons += 1;
                    continue;
                };
                if offs.len() < 3 || offs.iter().any(|&o| o % 6 != 0 || (o / 6) as u32 >= n) {
                    self.counts.bad_polygons += 1;
                    continue;
                }
                let maddr = model.poly_base.wrapping_add(mat_off as i16 as i32 as u32);
                let Some((mi, mode)) = self.material(model.poly_base, maddr, kind) else {
                    self.counts.bad_polygons += 1;
                    continue;
                };
                let mut vids: Vec<u32> = offs.iter().map(|&o| vbase + (o / 6) as u32).collect();
                if mode == 2 {
                    // flat colour: the record's 5:6:5 as the builder iterates it (cbcd9)
                    let c = r.u16(rec + 2).unwrap_or(0) as u32;
                    let color = [((c & 0xf800) >> 8) as u8, ((c & 0x7e0) >> 3) as u8, ((c & 0x1f) << 3) as u8, 255];
                    for v in vids.iter_mut() {
                        let mut copy = mesh.vertices[*v as usize];
                        copy.color = color;
                        *v = mesh.vertices.len() as u32;
                        mesh.vertices.push(copy);
                    }
                }
                let list = by_mat.entry(mi).or_default();
                placed.push((info.parts.len(), pinfo.polys.len(), mi, list.len() as u32));
                // fan (v0, vi, vi+1), as the builder: the record's winding is kept
                for t in 1..vids.len() - 1 {
                    list.extend_from_slice(&[vids[0], vids[t], vids[t + 1]]);
                }
                // scene-table models (wave 3): the set-ups cull a polygon by its first
                // triangle's winding and then draw all of it; a folded (non-planar) quad's
                // second triangle can face away while the first faces the camera (a car's
                // C-pillar seen from behind and above). Such triangles are added a second time
                // reversed, so they are drawn whichever way they face (hidden by the depth
                // buffer when the whole polygon faces away).
                if kind != Kind::World && vids.len() >= 4 {
                    let p = |i: u32| mesh.vertices[i as usize].pos.map(|c| c as f64);
                    let n = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| {
                        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
                        [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
                    };
                    let n0 = n(p(vids[0]), p(vids[1]), p(vids[2]));
                    for t in 2..vids.len() - 1 {
                        let nt = n(p(vids[0]), p(vids[t]), p(vids[t + 1]));
                        let (l0, lt) = ((n0[0] * n0[0] + n0[1] * n0[1] + n0[2] * n0[2]).sqrt(), (nt[0] * nt[0] + nt[1] * nt[1] + nt[2] * nt[2]).sqrt());
                        let cos = if l0 > 0.0 && lt > 0.0 { (n0[0] * nt[0] + n0[1] * nt[1] + n0[2] * nt[2]) / (l0 * lt) } else { 1.0 };
                        let deg = cos.clamp(-1.0, 1.0).acos().to_degrees();
                        self.counts.folds[[1.0, 5.0, 15.0, 45.0].iter().position(|&d| deg < d).unwrap_or(4)] += 1;
                        if deg >= FOLD_DEGREES {
                            list.extend_from_slice(&[vids[0], vids[t + 1], vids[t]]);
                            self.counts.folded += 1;
                        }
                    }
                }
                pinfo.polys.push(PolyInfo { record: rec, material: mi, first: 0, vids, offs });
            }
            info.parts.push(pinfo);
        }
        let mut start: HashMap<u32, u32> = HashMap::new();
        for (mi, list) in by_mat {
            let s = mesh.indices.len() as u32;
            start.insert(mi, s);
            mesh.indices.extend_from_slice(&list);
            mesh.ranges.push(MeshRange { material: mi, indices: s..mesh.indices.len() as u32 });
        }
        for (pi, qi, mi, off) in placed {
            info.parts[pi].polys[qi].first = start[&mi] + off;
        }
        (mesh, info)
    }

    fn push_mesh(&mut self, mesh: Mesh, info: MeshInfo) -> u32 {
        if mesh.indices.is_empty() {
            self.counts.empty_meshes += 1;
        }
        let i = self.scene.meshes.len() as u32;
        self.scene.meshes.push(mesh);
        self.extra.meshes.push(info);
        i
    }
}

/// A part's vertex normals: `count`, then `n` x 3 floats; `None` when the part has none
/// (offset 0 or -1, unreadable, a different count or values that are not unit vectors).
fn read_normals(r: &mut Reader, at: u32, n: u32) -> Option<Vec<[f64; 3]>> {
    if n == 0 || !r.ok(at, 4 + 12 * n) || r.u32(at)? != n {
        return None;
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let b = at + 4 + 12 * i;
        let v = [0, 4, 8].map(|o| f32::from_bits(r.u32(b + o).unwrap_or(0)) as f64);
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if !(0.9..=1.1).contains(&len) {
            return None;
        }
        out.push(v);
    }
    Some(out)
}

/// The scene and the build's guest-side bookkeeping.
fn build(m: &Machine) -> Option<(SceneBuild, Extra)> {
    build_with(m, None)
}

/// `menu`: None = a race; Some(world) = a menu's scene (`build_menu_scene`).
fn build_with(m: &Machine, menu: Option<bool>) -> Option<(SceneBuild, Extra)> {
    let t0 = Instant::now();
    let mut r = Reader::default();
    let cells = menu.unwrap_or(true);
    let (w, _cells, objects) = match world::read_world_with(m, &mut r, cells) {
        Ok(x) => x,
        Err(e) => {
            crate::warn!("extract: the world cannot be read: {e}");
            return None;
        }
    };
    let world_objects = objects.iter().filter(|o| matches!(o.source, Source::Cell { .. })).count();
    if world_objects == 0 && menu.is_none() {
        crate::warn!("extract: no world objects");
        return None;
    }
    let mut b = Builder {
        m,
        scene: SceneData::default(),
        extra: Extra::default(),
        materials: HashMap::new(),
        env_base: rd32(m.img.link(textures::ENV_BASE)),
        counts: Counts::default(),
        level: None,
    };
    b.add_textures();
    let sky_texture = if cells { b.add_sky() } else { None };
    let t_tex = t0.elapsed();

    // world meshes (one per model, all parts placed) and the statics
    let mut world_mesh: HashMap<u32, Option<u32>> = HashMap::new();
    let mut statics = Vec::new();
    for o in objects.iter().filter(|o| matches!(o.source, Source::Cell { .. })) {
        let Source::Cell { row, col, list, .. } = o.source else { continue };
        let mesh = *world_mesh.entry(o.model).or_insert_with(|| {
            let model = w.model(o.model)?.clone();
            let n = model.parts.min(13) as usize;
            if n > 1 {
                b.counts.multi_part += 1;
            }
            let mut slots = Vec::new();
            for s in 0..n {
                if world::best_level(&model, s).is_some() {
                    slots.push((s, b.part_placement(&w, &mut r, &model, s)?));
                }
            }
            let (mesh, info) = b.build_mesh(&w, &mut r, &model, &slots, Kind::World);
            Some(b.push_mesh(mesh, info))
        });
        let Some(mesh) = mesh else {
            b.counts.missing_models += 1;
            continue;
        };
        if o.flags & 0x8000 != 0 {
            b.counts.billboards += 1;
        }
        let transform = object_transform(o.angles, o.pos);
        statics.push((Instance { mesh, transform, phase: phase_of(o.anim), scroll: [0.0; 2] }, (row, col), o.addr, list == 0));
    }
    let t_world = t0.elapsed();
    // scene-table models (cars, shadows, smoke, marks): one mesh per part, in the part's
    // own space (placed per frame from instance B, wave 3)
    let mut scene_models: BTreeMap<u32, Kind> = BTreeMap::new();
    for o in objects.iter().filter(|o| matches!(o.source, Source::Scene { .. })) {
        scene_models.entry(o.model).or_insert(Kind::of_scene_name(&o.name));
    }
    // a menu: every model its cells do not use (the instance list's objects)
    if menu.is_some() {
        for mo in w.models.iter().flatten() {
            if !world_mesh.contains_key(&mo.index) {
                scene_models.entry(mo.index).or_insert(Kind::Other);
            }
        }
    }
    for (&mi, &kind) in &scene_models {
        let Some(model) = w.model(mi).cloned() else { continue };
        for s in 0..model.parts.min(13) as usize {
            let Some((best, _)) = world::best_level(&model, s) else { continue };
            let (mesh, info) = b.build_mesh(&w, &mut r, &model, &[(s, transforms::Affine::IDENTITY)], kind);
            let i = b.push_mesh(mesh, info);
            b.extra.part_mesh.insert((mi, s as u32), i);
            // every level present (wave 3: the car's stand-in is slot 0 at level 2); a level
            // with the same part data as a higher one shares its mesh
            let mut built: Vec<(u32, u32)> = vec![(model.levels[best][s], i)];
            for l in 0..3usize {
                let off = model.levels[l][s];
                if off == 0 {
                    continue;
                }
                let mesh = match built.iter().find(|e| e.0 == off) {
                    Some(&(_, m)) => m,
                    None => {
                        b.level = Some(l);
                        let (mesh, info) = b.build_mesh(&w, &mut r, &model, &[(s, transforms::Affine::IDENTITY)], kind);
                        b.level = None;
                        let m = b.push_mesh(mesh, info);
                        built.push((off, m));
                        m
                    }
                };
                b.extra.part_levels.push(PartMesh { model: mi, slot: s as u8, level: l as u8, mesh });
            }
        }
    }

    // statics, bounds and the ground test
    let mut metas = Vec::with_capacity(statics.len());
    let mut seeds = Vec::with_capacity(statics.len());
    for (inst, cell, addr, seed) in statics {
        seeds.push(seed);
        let mesh = &b.scene.meshes[inst.mesh as usize];
        let pts: Vec<[f64; 3]> = mesh.vertices.iter().map(|v| inst.transform.apply(v.pos)).collect();
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for p in &pts {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let center = if pts.is_empty() { inst.transform.origin } else { [0, 1, 2].map(|k| (lo[k] + hi[k]) / 2.0) };
        let radius = pts.iter().map(|p| ((p[0] - center[0]).powi(2) + (p[1] - center[1]).powi(2) + (p[2] - center[2]).powi(2)).sqrt()).fold(0.0, f64::max);
        metas.push(StaticMeta { cell, grounded: false, center, radius: radius as f32 });
        b.scene.statics.push(inst);
        b.extra.static_obj.push(addr);
    }
    let t_statics = t0.elapsed();
    let ground = ground_test(&b.scene, &seeds);
    for (meta, g) in metas.iter_mut().zip(&ground) {
        meta.grounded = g.grounded;
    }
    b.extra.ground = ground;
    let t_ground = t0.elapsed();

    // the game's fake water reflections (`native::water`): the water's statics, the reflection
    // cards and the flat objects standing in the water each get their own copy of their mesh:
    // the water's materials drawn as `Layer::Water`, the cards' as `Layer::Reflection`, and a
    // standing object's faces twice, as they are and as `Layer::Reflection`
    if menu.is_none() {
        let names: Vec<String> = b.scene.statics.iter().map(|s| b.extra.meshes[s.mesh as usize].name.clone()).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (water, cards) = super::water::find(&b.scene, &refs);
        // (static, water / card / standing)
        let jobs: Vec<(usize, u8)> = water.iter().map(|&si| (si, 0)).chain(cards.iter().map(|c| (c.static_index, if c.standing { 2 } else { 1 }))).collect();
        for (si, job) in jobs {
            let src = b.scene.statics[si].mesh as usize;
            let mut mesh = b.scene.meshes[src].clone();
            let ranges = mesh.ranges.clone();
            if job == 2 {
                mesh.ranges.clear();
            }
            for r in &ranges {
                let mi = r.material as usize;
                let m = &b.scene.materials[mi];
                let mat = if job == 0 { super::water::water_material(m) } else { super::water::reflection_material(m) };
                let new = b.scene.materials.len() as u32;
                b.scene.materials.push(mat);
                let mode = b.extra.mat_mode[mi];
                b.extra.mat_mode.push(mode);
                if job == 2 {
                    // the faces as they are, then again shown on the water
                    mesh.ranges.push(r.clone());
                    let s = mesh.indices.len() as u32;
                    let idx: Vec<u32> = mesh.indices[r.indices.start as usize..r.indices.end as usize].to_vec();
                    mesh.indices.extend(idx);
                    mesh.ranges.push(sr_scene::MeshRange { material: new, indices: s..mesh.indices.len() as u32 });
                } else {
                    for q in mesh.ranges.iter_mut().filter(|q| q.material as usize == mi && q.indices == r.indices) {
                        q.material = new;
                    }
                }
            }
            let info = b.extra.meshes[src].clone();
            b.scene.statics[si].mesh = b.push_mesh(mesh, info);
        }
        if !water.is_empty() {
            let names_of = |standing: bool| {
                let mut v: Vec<&str> = cards.iter().filter(|c| c.standing == standing).map(|c| refs[c.static_index]).collect();
                v.sort();
                v.dedup();
                v.join(", ")
            };
            let mut kinds: Vec<String> = water
                .iter()
                .flat_map(|&si| b.scene.meshes[b.scene.statics[si].mesh as usize].ranges.iter().map(|r| format!("{:?}", b.scene.materials[r.material as usize].transparency)))
                .collect();
            kinds.sort();
            kinds.dedup();
            let n = |standing: bool| cards.iter().filter(|c| c.standing == standing).count();
            crate::info!(
                "water: {} water statics ({}); shown on it: {} reflection cards ({}), {} flat objects standing in it ({})",
                water.len(),
                kinds.join(", "),
                n(false),
                names_of(false),
                n(true),
                names_of(true)
            );
        }
    }

    // summary
    let c = &b.counts;
    let tex_bytes: usize = b.scene.textures.iter().map(|t| t.pixels.len() + if matches!(t.format, TexFormat::Indexed8 { .. }) { 1024 } else { 0 }).sum();
    let tris: usize = b.scene.meshes.iter().map(|m| m.indices.len() / 3).sum();
    let verts: usize = b.scene.meshes.iter().map(|m| m.vertices.len()).sum();
    let world_meshes = world_mesh.values().flatten().count();
    let world_tris: usize = world_mesh.values().flatten().map(|&i| b.scene.meshes[i as usize].indices.len() / 3).sum();
    let static_tris: usize = b.scene.statics.iter().map(|s| b.scene.meshes[s.mesh as usize].indices.len() / 3).sum();
    let grounded = metas.iter().filter(|s| s.grounded).count();
    let mut placed_polys = [0usize; 3];
    for st in &b.scene.statics {
        for p in &b.extra.meshes[st.mesh as usize].parts {
            for q in &p.polys {
                placed_polys[match q.vids.len() {
                    3 => 0,
                    4 => 1,
                    _ => 2,
                }] += 1;
            }
        }
    }
    let mut s = String::new();
    let _ = write!(
        s,
        "extract: scene built in {:.0} ms (textures {:.0}, world meshes {:.0}, part meshes and statics {:.0}, ground test {:.0}): {} textures ({:.2} MB{}), {} frame entries, {} materials (by mode {:?}), \
         {} meshes ({} world: {} triangles; {} scene-table parts), {} triangles, {} vertices ({:.2} MB vertex + index data), {} statics of {} world objects ({} triangles placed, from {} polygons: {:?} with 3 / 4 / more vertices), {} grounded",
        t_ground.as_secs_f64() * 1000.0,
        t_tex.as_secs_f64() * 1000.0,
        (t_world - t_tex).as_secs_f64() * 1000.0,
        (t_statics - t_world).as_secs_f64() * 1000.0,
        (t_ground - t_statics).as_secs_f64() * 1000.0,
        b.scene.textures.len(),
        tex_bytes as f64 / 1048576.0,
        if sky_texture.is_some() { ", sky included" } else { ", no sky" },
        b.scene.texture_frames.len(),
        b.scene.materials.len(),
        c.modes,
        b.scene.meshes.len(),
        world_meshes,
        world_tris,
        b.extra.part_mesh.len(),
        tris,
        verts,
        (verts * std::mem::size_of::<Vertex>() + tris * 12) as f64 / 1048576.0,
        b.scene.statics.len(),
        world_objects,
        static_tris,
        placed_polys.iter().sum::<usize>(),
        placed_polys,
        grounded,
    );
    let _ = write!(
        s,
        "; parts with normals {}, without uv table {}, multi-part world models {}, placed parts {}, animated {}, world billboards {}, missing models {}, empty meshes {}, \
         bad polygons {}, unknown material types {:?}, untextured textured materials {}, mode 3 {}, records not exported {}, frames without record {}, keyed materials with frames of other keys {}, \
         keyed textures without key texels {:?}; scene-table fan triangles by angle to the first (<1/<5/<15/<45/more degrees) {:?}, made double-sided {}",
        c.normals,
        c.no_uv,
        c.multi_part,
        c.placed_parts,
        c.animated_models,
        c.billboards,
        c.missing_models,
        c.empty_meshes,
        c.bad_polygons,
        c.unknown_type,
        c.untextured,
        c.mode3,
        c.textures_skipped,
        c.frames_missing,
        c.chroma_differs,
        c.keyed_without_texels,
        c.folds,
        c.folded,
    );
    crate::info!("{s}");
    crate::info!("{}", ground_summary(&b.extra.ground));
    b.extra.summary = s;
    let part_meshes = b.extra.part_levels.clone();
    Some((SceneBuild { scene: b.scene, statics: metas, sky_texture, part_meshes }, b.extra))
}

/// An object's model -> world transform: R(angles) (row-major, f32) and its position (f64).
fn object_transform(angles: [i32; 3], pos: [i32; 3]) -> Transform {
    let ro = rotation(angles);
    let mut basis = [0f32; 9];
    for i in 0..3 {
        for j in 0..3 {
            basis[i * 3 + j] = ro[i][j] as f32;
        }
    }
    Transform { basis, origin: pos.map(|v| v as f64) }
}

/// The object's animation value as the engine computes [7bb94] (`824bf`): instance +20h
/// >> 2 (its low 16 bits when the tint flags 60000h are set, `ca8f4`), -1 (the clock) when
/// +20h is negative. The contract's frame formula then gives the game's frame with
/// `FrameData::clock` = [1473a0] & 3FFFh.
pub(crate) fn phase_of(anim: i32) -> i32 {
    if anim < 0 {
        return -1;
    }
    let p = anim >> 2;
    if p & 0x60000 != 0 { p & 0xffff } else { p }
}

/// The ground test of one static.
#[derive(Clone, Copy, Debug, Default)]
struct Ground {
    grounded: bool,
    /// A track section (cell list 0): the road is the ground.
    seed: bool,
    /// Base points, those with a surface of any other static within the band, and those
    /// with a surface of a grounded static (the test).
    points: u32,
    supported_any: u32,
    supported: u32,
    /// Median over the base points of the signed distance down to the nearest surface of
    /// another static (any distance; negative: the surface lies above); None when most
    /// base points have no surface above or below at all.
    gap: Option<f64>,
}

/// The ground test (constants above): the track sections (cell list 0) are the ground;
/// another static is grounded when at least `GROUND_SHARE` of its base points lie on a
/// grounded static's surface (at most `GROUND_BELOW` under the point or `GROUND_ABOVE`
/// over it), repeated until nothing changes: ground is what connects to the road, and an
/// object standing on a floating hill floats with it.
fn ground_test(scene: &SceneData, seeds: &[bool]) -> Vec<Ground> {
    let cell = |v: f64| (v / GROUND_GRID).floor() as i32;
    // every triangle of every static in world space, bucketed by (x, z)
    let mut tris: Vec<(u32, [[f64; 3]; 3])> = Vec::new();
    let mut world_pts: Vec<Vec<[f64; 3]>> = Vec::with_capacity(scene.statics.len());
    for (si, s) in scene.statics.iter().enumerate() {
        let mesh = &scene.meshes[s.mesh as usize];
        let pts: Vec<[f64; 3]> = mesh.vertices.iter().map(|v| s.transform.apply(v.pos)).collect();
        for t in mesh.indices.chunks_exact(3) {
            tris.push((si as u32, [pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]]));
        }
        world_pts.push(pts);
    }
    let mut buckets: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
    for (k, (_, t)) in tris.iter().enumerate() {
        let (x0, x1) = (t.iter().map(|v| v[0]).fold(f64::MAX, f64::min), t.iter().map(|v| v[0]).fold(f64::MIN, f64::max));
        let (z0, z1) = (t.iter().map(|v| v[2]).fold(f64::MAX, f64::min), t.iter().map(|v| v[2]).fold(f64::MIN, f64::max));
        for cz in cell(z0)..=cell(z1).min(cell(z0) + 64) {
            for cx in cell(x0)..=cell(x1).min(cell(x0) + 64) {
                buckets.entry((cx, cz)).or_default().push(k as u32);
            }
        }
    }
    let height_at = |t: &[[f64; 3]; 3], x: f64, z: f64| -> Option<f64> {
        let (a, b, c) = (t[0], t[1], t[2]);
        let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
        if d.abs() < 1e-6 {
            return None;
        }
        let l1 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / d;
        let l2 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / d;
        let l3 = 1.0 - l1 - l2;
        (l1 >= -1e-6 && l2 >= -1e-6 && l3 >= -1e-6).then(|| l1 * a[1] + l2 * b[1] + l3 * c[1])
    };
    // per static and base point: the statics whose surface supports it
    let mut out: Vec<Ground> = Vec::with_capacity(world_pts.len());
    let mut support: Vec<Vec<Vec<u32>>> = Vec::with_capacity(world_pts.len());
    for (si, pts) in world_pts.iter().enumerate() {
        let min_y = pts.iter().map(|p| p[1]).fold(f64::MAX, f64::min);
        let base: Vec<&[f64; 3]> = pts.iter().filter(|p| p[1] <= min_y + BASE_BAND).collect();
        let picked: Vec<&[f64; 3]> = if base.len() > BASE_POINTS { (0..BASE_POINTS).map(|i| base[i * base.len() / BASE_POINTS]).collect() } else { base };
        let mut gaps: Vec<f64> = Vec::with_capacity(picked.len());
        let mut sup: Vec<Vec<u32>> = Vec::with_capacity(picked.len());
        for p in &picked {
            let mut nearest: Option<f64> = None;
            let mut owners = Vec::new();
            if let Some(list) = buckets.get(&(cell(p[0]), cell(p[2]))) {
                for &k in list {
                    let (owner, t) = &tris[k as usize];
                    if *owner as usize == si {
                        continue;
                    }
                    if let Some(h) = height_at(t, p[0], p[2]) {
                        let d = p[1] - h;
                        if nearest.is_none_or(|n| d.abs() < n.abs()) {
                            nearest = Some(d);
                        }
                        if (-GROUND_ABOVE..=GROUND_BELOW).contains(&d) && !owners.contains(owner) {
                            owners.push(*owner);
                        }
                    }
                }
            }
            gaps.push(nearest.unwrap_or(f64::INFINITY));
            sup.push(owners);
        }
        gaps.sort_by(|a, b| a.abs().total_cmp(&b.abs()));
        let gap = gaps.get(gaps.len() / 2).copied().filter(|g| g.is_finite());
        let supported_any = sup.iter().filter(|o| !o.is_empty()).count() as u32;
        out.push(Ground { grounded: seeds[si], seed: seeds[si], points: picked.len() as u32, supported_any, supported: 0, gap });
        support.push(sup);
    }
    // propagate from the road
    loop {
        let mut changed = false;
        for si in 0..out.len() {
            let supported = support[si].iter().filter(|o| o.iter().any(|&k| out[k as usize].grounded)).count() as u32;
            out[si].supported = supported;
            if !out[si].grounded && out[si].points > 0 && supported as f64 >= GROUND_SHARE * out[si].points as f64 {
                out[si].grounded = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// The distribution behind the ground test's constants: objects by the median signed gap
/// of their base points to the nearest surface below (negative: above).
fn ground_summary(ground: &[Ground]) -> String {
    const BINS: [f64; 12] = [-2000.0, -1000.0, -500.0, -200.0, -50.0, 0.0, 50.0, 200.0, 500.0, 1000.0, 2000.0, f64::INFINITY];
    let labels = ["<-2000", "-2000..-1000", "-1000..-500", "-500..-200", "-200..-50", "-50..0", "0..50", "50..200", "200..500", "500..1000", "1000..2000", ">=2000", "none"];
    let mut hist = [0u32; 13];
    for g in ground {
        let k = g.gap.map_or(12, |d| BINS.iter().position(|&b| d < b).unwrap_or(11));
        hist[k] += 1;
    }
    let alone = ground.iter().filter(|g| g.seed || (g.points > 0 && g.supported_any as f64 >= GROUND_SHARE * g.points as f64)).count();
    format!(
        "extract: ground test (base points within {BASE_BAND} of the lowest, at most {BASE_POINTS}; a surface supports one at most {GROUND_BELOW} below or {GROUND_ABOVE} above; grounded = track section, \
         or at least {:.0} % of the base points on grounded statics): {} of {} grounded ({} track sections; {} by the support test without propagation); objects by median signed gap to the nearest surface below [{}]",
        GROUND_SHARE * 100.0,
        ground.iter().filter(|g| g.grounded).count(),
        ground.len(),
        ground.iter().filter(|g| g.seed).count(),
        alone,
        labels.iter().zip(hist).map(|(l, n)| format!("{l}: {n}")).collect::<Vec<_>>().join(", "),
    )
}

// ---------------------------------------------------------------------------------------
// SR_EXTRACT_CHECK

struct CheckCfg {
    first: u64,
    last: u64,
    dir: Option<std::path::PathBuf>,
}

fn check_cfg() -> Option<&'static CheckCfg> {
    static C: OnceLock<Option<CheckCfg>> = OnceLock::new();
    C.get_or_init(|| {
        let v = std::env::var("SR_EXTRACT_CHECK").ok()?;
        let mut it = v.split(',');
        let first = it.next().and_then(|f| f.trim().parse().ok()).unwrap_or(1500);
        let last = it.next().and_then(|f| f.trim().parse().ok()).unwrap_or(u64::MAX);
        let dir = it.next().map(str::trim).filter(|d| !d.is_empty()).map(std::path::PathBuf::from);
        Some(CheckCfg { first, last, dir })
    })
    .as_ref()
}

// Link addresses the check reads (docs/re/native/world.md, polygons.md, transforms.md).
/// After the part transform, before the polygon list: `mov dword ptr [82eb8h], 17700000h`
/// (78e62; 78e4e and 78e58 are taken by `subpixel.rs` and the wave-1 checks).
const AFTER_TRANSFORM: u32 = 0x78e62;
/// After the part's polygons were drawn: `inc dword ptr [7babc]` (78e84; also reached for
/// parts not drawn, `je 78e7c` at 78e3f).
const AFTER_PART: u32 = 0x78e84;
const CUR_INSTANCE: u32 = 0x7bb8c;
const CUR_SLOT: u32 = 0x7babc;
const CUR_PART: u32 = 0x7bac4;
const PART_ORDER: u32 = 0x81d44;
const CAMERA_PTR: u32 = 0x81e0c;
const VIEW_POS: u32 = 0x68fc8;
const PACKED_XY: u32 = 0x6ed94;
const VARIANT: u32 = 0xaa5b0;
const CENTER_IMM: [u32; 2] = [0x7ab1a, 0x7ac1e];
const CENTER_Y: u32 = 0x81e3c;
const VIEW_W: u32 = 0x81e50;
const VIEW_H: u32 = 0x81e24;
const FAR: u32 = 0xcf474;
/// The part's polygon list (7ac50): (material offset | variant, record) pairs, -1 ends it.
const POLY_LIST: u32 = 0x67088;
const UV_PTR: u32 = 0x7bad4;
const BUILDER_MODE: u32 = 0xaa5c8;
const POLY_TEXTURE: u32 = 0xaa5c0;
const PHASE2: u32 = 0x7bb98;
const ANIM_CLOCK: u32 = 0x1473a0;
/// Polygon set-ups (variant 0, eax = record) and their modes; mode 2 one instruction pair
/// into d02cc (esi = record), as `polygons.rs`.
const SETUPS: [(u32, u8); 7] = [(0xca670, 0), (0xd5f30, 1), (0xce9f0, 3), (0xd25c0, 4), (0xd567c, 5), (0xce190, 6), (0xd2e20, 7)];
const FLAT_SETUP: u32 = 0xd02d2;
/// Set-ups of near-clipped polygons (variant 4; d01c4 is `depth.rs`'s, so d01ca) and the
/// sky routine, as `polygons.rs`.
const CLIPPED_SETUPS: [u32; 8] = [0xca4a0, 0xd5d60, 0xd01ca, 0xce820, 0xd23f0, 0xd54bc, 0xcdfc0, 0xd2c50];
const SKY_ENTRY: u32 = 0x7b218;
/// The builder's triangle loop (before `grDrawTriangle`).
const TRI_LOOP: u32 = 0xcc6df;
/// The engine's 32-bit matrix products wrap by multiples of 2^32 >> 14 (`world.md`).
const WRAP: f64 = 262144.0;
/// The culling comparison's distance bands: eye distance below China's original far
/// distance (50 000 x 0.8, `transforms.md`), and beyond it (the runtime's longer draw
/// distance).
const CULL_NEAR: f64 = 40000.0;

#[derive(Default)]
struct ViewStats {
    parts: u64,
    vertices: u64,
    max: f64,
    sum2: f64,
    wrapped: u64,
    /// By eye distance < 1000, < 4000, < 16000, more: vertices, max difference.
    bands: [(u64, f64); 4],
    onscreen: u64,
    max_px: f64,
    sum_px2: f64,
    /// The worst vertices (view, grid).
    worst: String,
    worst_px: String,
}

impl ViewStats {
    fn line(&self) -> String {
        let rms = |s: f64, n: u64| if n > 0 { (s / n as f64).sqrt() } else { 0.0 };
        format!(
            "{} parts, {} vertices: view max {:.2} units, rms {:.2}; by eye distance <1000/<4000/<16000/more: {} vertices, max {} units; {} vertices of engine overflows (2^18) left out; on screen {} vertices: grid max {:.2} px, rms {:.2}\n      worst view: {}\n      worst grid: {}",
            self.parts,
            self.vertices,
            self.max,
            rms(self.sum2, self.vertices * 3),
            self.bands.iter().map(|b| b.0.to_string()).collect::<Vec<_>>().join("/"),
            self.bands.iter().map(|b| format!("{:.2}", b.1)).collect::<Vec<_>>().join("/"),
            self.wrapped,
            self.onscreen,
            self.max_px,
            rms(self.sum_px2, self.onscreen * 2),
            self.worst,
            self.worst_px
        )
    }
}

#[derive(Default)]
struct PolyStats {
    polygons: u64,
    uv_vertices: u64,
    uv_bad: u64,
    fan_bad: u64,
    color_bad: u64,
    drawn: u64,
    texture_bad: u64,
    mode_bad: u64,
    chroma_bad: u64,
    blend_bad: u64,
    clamp_bad: u64,
    /// Drawn polygons of animated materials, by where the frame came from: the object's
    /// phase, the clock, the environment phase.
    anim_drawn: [u64; 3],
    by_mode: BTreeMap<u8, u64>,
}

#[derive(Default)]
struct CheckStats {
    /// Per kind (world / car / shadow / smoke / marks / other) and view (main, mirror).
    view: BTreeMap<(Kind, bool), ViewStats>,
    statics_seen: HashSet<u32>,
    level_other: u64,
    unknown: u64,
    /// Per kind.
    poly: BTreeMap<Kind, PolyStats>,
    poly_unknown: u64,
    poly_missing: u64,
    /// World parts in the main view, by distance band (every vertex nearer than
    /// `CULL_NEAR`, or not): triangles of polygons in the engine's list / not, by the
    /// scene's winding on screen as the eye sees it (clockwise = front, counter-clockwise,
    /// degenerate).
    cull: [[[u64; 3]; 2]; 2],
    /// Per band, list / not and winding (clockwise, counter-clockwise): triangles wholly on
    /// screen and their total area (grid px^2); the largest of a disagreeing triangle
    /// (drawn counter-clockwise, skipped clockwise).
    cull_onscreen: [[[u64; 2]; 2]; 2],
    cull_area_sum: [[[f64; 2]; 2]; 2],
    cull_area: [f64; 2],
    /// Disagreeing triangles (drawn counter-clockwise, skipped clockwise) with the eye and
    /// the view origin (128 units in front of it) on opposite sides of their plane.
    cull_origin: [u64; 2],
    /// Triangles whose screen winding and side of the eye agree (the orientation used).
    cull_side_agree: [u64; 2],
    /// Polygons left out (a vertex behind the near plane or beyond the far plane), parts
    /// left out (the engine's product overflowed).
    cull_skipped: u64,
    cull_overflow_parts: u64,
    sky_checks: u64,
    sky_bad: u64,
    first_bad: Vec<String>,
}

struct Armed {
    /// Static index or None (scene instance), mesh, part index in its MeshInfo.
    mesh: u32,
    part: usize,
    /// Grid positions (scene, float) of the part's vertices, None behind the near plane;
    /// their view positions (scene, float, the game's view space); the engine's view z.
    screen: Vec<Option<(f64, f64)>>,
    view: Vec<[f64; 3]>,
    z: Vec<i32>,
    /// The view's grid size.
    size: (i32, i32),
}

struct Pending {
    kind: Kind,
    material: u32,
    phase: i32,
}

#[derive(Default)]
struct Check {
    build: Option<(SceneBuild, Extra)>,
    key: (u32, u32, u32),
    builds: u32,
    by_obj: HashMap<u32, u32>,
    poly_of: HashMap<(u32, u32), (usize, usize)>,
    armed: Option<Armed>,
    pending: Option<Pending>,
    stats: CheckStats,
    race_frames: u64,
    done: bool,
}

static CHECK: Mutex<Option<Check>> = Mutex::new(None);

fn with_check(f: impl FnOnce(&mut Check)) {
    if let Ok(mut g) = CHECK.lock() {
        if let Some(c) = g.as_mut() {
            if c.build.is_some() && !c.done {
                f(c)
            }
        }
    }
}

/// Installs the check's observers (only with `SR_EXTRACT_CHECK`).
pub fn install(m: &mut Machine) {
    let Some(cfg) = check_cfg() else { return };
    *CHECK.lock().unwrap() = Some(Check::default());
    let mut failed = Vec::new();
    let mut add = |m: &mut Machine, name: &'static str, at: u32, h: crate::hooks::Handler| {
        if let Err(e) = m.install_observer(name, at, h) {
            failed.push(e);
        }
    };
    add(m, "extract check: after transform", AFTER_TRANSFORM, after_transform);
    add(m, "extract check: after part", AFTER_PART, after_part);
    for (at, mode) in SETUPS {
        let h: crate::hooks::Handler = match mode {
            0 => |m, f, t| setup(m, t, 0, f.eax),
            1 => |m, f, t| setup(m, t, 1, f.eax),
            3 => |m, f, t| setup(m, t, 3, f.eax),
            4 => |m, f, t| setup(m, t, 4, f.eax),
            5 => |m, f, t| setup(m, t, 5, f.eax),
            6 => |m, f, t| setup(m, t, 6, f.eax),
            _ => |m, f, t| setup(m, t, 7, f.eax),
        };
        add(m, "extract check: set-up", at, h);
    }
    add(m, "extract check: flat set-up", FLAT_SETUP, |m, f, t| setup(m, t, 2, f.esi));
    // near-clipped polygons (rebuilt in screen space) and the sky are not compared: their
    // triangles must not be taken for the last set-up's
    for at in CLIPPED_SETUPS.into_iter().chain([SKY_ENTRY]) {
        add(m, "extract check: other polygons", at, |_m, _f, t| {
            with_check(|c| c.pending = None);
            HookResult::Resume(t)
        });
    }
    add(m, "extract check: triangle", TRI_LOOP, triangle);
    for e in &failed {
        crate::warn!("extract check: {e}");
    }
    crate::info!("extract check (SR_EXTRACT_CHECK): game frames {}..{}, {} observers failed", cfg.first, cfg.last, failed.len());
}

/// Start of every race frame: (re)build the scene for the check when the world changed.
pub fn race_frame(m: &mut Machine) {
    let Some(cfg) = check_cfg() else { return };
    let frame = crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed);
    let mut g = CHECK.lock().unwrap();
    let Some(c) = g.as_mut() else { return };
    if c.done {
        return;
    }
    if frame > cfg.last {
        c.done = true;
        report(c, cfg, frame);
        return;
    }
    if frame < cfg.first {
        return;
    }
    c.race_frames += 1;
    let libs = textures::libraries();
    let key = (rd32(m.img.link(0x123720)), rd32(m.img.link(0x147390)), libs.last().map_or(0, |l| l.data));
    if c.build.is_none() || key != c.key {
        if c.build.is_some() {
            report(c, cfg, frame);
        }
        c.key = key;
        c.build = build(m);
        c.builds += 1;
        c.stats = CheckStats::default();
        c.armed = None;
        c.pending = None;
        if let Some((b, x)) = c.build.as_ref() {
            c.by_obj = x.static_obj.iter().enumerate().map(|(i, &a)| (a, i as u32)).collect();
            c.poly_of.clear();
            for (mi, info) in x.meshes.iter().enumerate() {
                for (pi, p) in info.parts.iter().enumerate() {
                    for (qi, q) in p.polys.iter().enumerate() {
                        c.poly_of.insert((mi as u32, q.record), (pi, qi));
                    }
                }
            }
            if let Some(dir) = cfg.dir.as_ref() {
                let _ = std::fs::create_dir_all(dir);
                let _ = std::fs::write(dir.join(format!("grounded{}.tsv", c.builds)), grounded_tsv(b, x));
            }
        }
    }
    // the background picture stays in texture memory unchanged
    if c.race_frames % 50 == 0 {
        if let (Some((b, x)), Some(gl)) = (c.build.as_ref(), m.glide.as_ref()) {
            if let Some(t) = b.sky_texture {
                let start = rd32(m.img.link(textures::SKY_TMU)) as usize;
                let n = b.scene.textures[t as usize].pixels.len();
                c.stats.sky_checks += 1;
                if gl.tmu.ram.get(start..start + n).map(fnv) != Some(x.sky_hash) {
                    c.stats.sky_bad += 1;
                }
            }
        }
    }
    if c.race_frames % 250 == 0 {
        report(c, cfg, frame);
    }
}

fn grounded_tsv(b: &SceneBuild, x: &Extra) -> String {
    let mut s = String::from("static\trow\tcol\tmodel\tname\tclass\tgrounded\ttrack\tmedian_gap\tbase_points\tsupported_any\tsupported\tcenter_x\tcenter_y\tcenter_z\tradius\n");
    for (i, (meta, inst)) in b.statics.iter().zip(&b.scene.statics).enumerate() {
        let name = &x.meshes[inst.mesh as usize].name;
        let model = x.meshes[inst.mesh as usize].model;
        let class: String = name.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>().replace("##", "#").replace("##", "#");
        let g = x.ground[i];
        let _ = writeln!(
            s,
            "{i}\t{}\t{}\t{model}\t{name}\t{class}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.0}",
            meta.cell.0,
            meta.cell.1,
            meta.grounded as u8,
            g.seed as u8,
            g.gap.map_or("-".into(), |d| format!("{d:.1}")),
            g.points,
            g.supported_any,
            g.supported,
            meta.center[0],
            meta.center[1],
            meta.center[2],
            meta.radius
        );
    }
    s
}

fn report(c: &mut Check, cfg: &CheckCfg, frame: u64) {
    let Some((b, x)) = c.build.as_ref() else { return };
    let s = &c.stats;
    let mut out = String::new();
    let _ = writeln!(out, "extract: check of build {} at game frame {frame} ({} race frames)", c.builds, c.race_frames);
    let _ = writeln!(out, "  {}", x.summary);
    let _ = writeln!(out, "  {}", ground_summary(&x.ground));
    let _ = writeln!(
        out,
        "  statics compared: {} of {}; parts at another level than the scene's {}; parts of objects not in the scene {}",
        s.statics_seen.len(),
        b.scene.statics.len(),
        s.level_other,
        s.unknown
    );
    for ((kind, mirror), v) in &s.view {
        let _ = writeln!(out, "  view {:?}{}: {}", kind, if *mirror { " (mirror)" } else { "" }, v.line());
    }
    for (kind, p) in &s.poly {
        let _ = writeln!(
            out,
            "  polygons {:?}: {} set up (by mode {:?}): uv {} vertices, {} differ; fan {} differ; flat colour {} differ; {} drawn: texture {} differ, mode {}, chroma key {}, blend {}, clamp {}; animated (frame from phase / clock / env) {:?}",
            kind,
            p.polygons,
            p.by_mode,
            p.uv_vertices,
            p.uv_bad,
            p.fan_bad,
            p.color_bad,
            p.drawn,
            p.texture_bad,
            p.mode_bad,
            p.chroma_bad,
            p.blend_bad,
            p.clamp_bad,
            p.anim_drawn
        );
    }
    let _ = writeln!(out, "  polygons of objects not in the scene {}, records not in the object's mesh {}", s.poly_unknown, s.poly_missing);
    for (band, label) in [(0, format!("eye distance < {CULL_NEAR}")), (1, "farther".to_string())] {
        let (n, on, area) = (&s.cull[band], &s.cull_onscreen[band], &s.cull_area_sum[band]);
        let _ = writeln!(
            out,
            "  culling, {label} (triangles of world parts, main view, by the scene's winding on screen clockwise / counter-clockwise / degenerate): of polygons in the engine's list {:?}, \
             of polygons not in it {:?}; wholly on screen (count, total grid px^2): drawn clockwise {} / {:.0}, drawn counter-clockwise {} / {:.0}, skipped clockwise {} / {:.0}, skipped counter-clockwise {} / {:.0}",
            n[0], n[1], on[0][0], area[0][0], on[0][1], area[0][1], on[1][0], area[1][0], on[1][1], area[1][1]
        );
    }
    let _ = writeln!(
        out,
        "  culling: largest wholly on-screen triangle drawn counter-clockwise {:.1} grid px^2, skipped clockwise {:.1}; with the eye and the view origin on opposite sides of their plane: {} drawn counter-clockwise, {} skipped clockwise; \
         winding = side of the eye for {} triangles, opposite for {}; {} polygons left out (near / far), {} parts left out (engine overflow)",
        s.cull_area[0],
        s.cull_area[1],
        s.cull_origin[0],
        s.cull_origin[1],
        s.cull_side_agree[0],
        s.cull_side_agree[1],
        s.cull_skipped,
        s.cull_overflow_parts
    );
    let _ = writeln!(out, "  sky: {} checks of the texture memory against the scene's picture, {} differ", s.sky_checks, s.sky_bad);
    for l in &s.first_bad {
        let _ = writeln!(out, "  first mismatch: {l}");
    }
    for l in out.lines() {
        crate::info!("{l}");
    }
    if let Some(dir) = cfg.dir.as_ref() {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join("extract_check.txt"), &out);
    }
}

fn note(s: &mut CheckStats, msg: impl FnOnce() -> String) {
    if s.first_bad.len() < 12 {
        s.first_bad.push(msg());
    }
}

/// The transform of a part of a scene-table instance (topic D's recipe; billboard angles
/// from the camera of the view) and the mesh of that part.
fn instance_part(m: &Machine, x: &Extra, inst: u32, slot: u32, cam_angles: [i32; 3]) -> Option<(u32, transforms::Affine)> {
    let _ = m;
    if !mem::readable(inst, 0x40) {
        return None;
    }
    let model = rd32(inst);
    let &mesh = x.part_mesh.get(&(model, slot))?;
    let flags = rd32(inst + 4);
    let a = [rd32(inst + 0x14) as i32, rd32(inst + 0x18) as i32, rd32(inst + 0x1c) as i32];
    let angles = if flags & 0x8000 != 0 { transforms::billboard_angles(a[0], a[1], cam_angles) } else { a };
    let pos = [rd32(inst + 8) as i32 as f64, rd32(inst + 0xc) as i32 as f64, rd32(inst + 0x10) as i32 as f64];
    let b = inst + 0x40 + 0x20 * slot;
    if !mem::readable(b, 0x20) {
        return None;
    }
    let pa = [rd32(b + 8) as i32, rd32(b + 0xc) as i32, rd32(b + 0x10) as i32];
    let pp = [rd32(b + 0x14) as i32 as f64, rd32(b + 0x18) as i32 as f64, rd32(b + 0x1c) as i32 as f64];
    let part = transforms::Affine::new(rotation(pa), pp);
    Some((mesh, transforms::Affine::new(rotation(angles), pos).after(&part)))
}

fn after_transform(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    with_check(|c| check_part(m, c));
    HookResult::Resume(tramp)
}

fn check_part(m: &Machine, c: &mut Check) {
    c.armed = None;
    let l = |a: u32| m.img.link(a);
    let Some((b, x)) = c.build.as_ref() else { return };
    let mirror = rd32(l(VARIANT)) & 1 != 0;
    let raw = rd32(l(CUR_INSTANCE));
    let inst = raw & !0x4000_0000;
    let scene_inst = raw & 0x4000_0000 != 0;
    let slot = unsafe { mem::rd16(l(PART_ORDER) + 4 * rd32(l(CUR_SLOT))) } as u32;
    let data = rd32(l(CUR_PART));
    let cam = rd32(l(CAMERA_PTR));
    if !mem::readable(cam, 0x18) {
        return;
    }
    let cam_angles = [rd32(cam) as i32, rd32(cam + 4) as i32, rd32(cam + 8) as i32];
    let cam_pos = [rd32(cam + 0xc) as i32 as f64, rd32(cam + 0x10) as i32 as f64, rd32(cam + 0x14) as i32 as f64];
    // topic D: view = Rc^T (p - pc), the camera's rotation in float
    let to_view = transforms::world_to_view(cam_pos, cam_angles.map(|a| a as f64));
    // the scene's mesh and model -> world transform of this part
    let (kind, mesh, world_xf, part_index): (Kind, u32, Box<dyn Fn([f32; 3]) -> [f64; 3]>, usize) = if !scene_inst {
        let Some(&si) = c.by_obj.get(&inst) else {
            c.stats.unknown += 1;
            return;
        };
        let st = b.scene.statics[si as usize];
        let info = &x.meshes[st.mesh as usize];
        let Some(pi) = info.parts.iter().position(|p| p.slot == slot) else {
            c.stats.level_other += 1;
            return;
        };
        c.stats.statics_seen.insert(si);
        (Kind::World, st.mesh, Box::new(move |p| st.transform.apply(p)), pi)
    } else {
        let Some((mesh, xf)) = instance_part(m, x, inst, slot, cam_angles) else {
            c.stats.unknown += 1;
            return;
        };
        let kind = x.meshes[mesh as usize].kind.unwrap_or(Kind::Other);
        (kind, mesh, Box::new(move |p: [f32; 3]| xf.apply(p.map(|v| v as f64))), 0)
    };
    let info = &x.meshes[mesh as usize];
    let part = &info.parts[part_index];
    if part.data != data {
        c.stats.level_other += 1;
        return;
    }
    let centre = rd32(l(CENTER_IMM[mirror as usize])) as i32 as f64 / 65536.0;
    let cy = rd32(l(CENTER_Y)) as i32 as f64;
    let (width, height) = (rd32(l(VIEW_W)) as i32, rd32(l(VIEW_H)) as i32);
    let vs = c.stats.view.entry((kind, mirror)).or_default();
    vs.parts += 1;
    let mesh_v = &b.scene.meshes[mesh as usize].vertices;
    let mut screen = Vec::with_capacity(part.nverts as usize);
    let mut views = Vec::with_capacity(part.nverts as usize);
    let mut zs = Vec::with_capacity(part.nverts as usize);
    let mut worst = (0.0f64, 0u32);
    let mut overflowed = false;
    for i in 0..part.nverts {
        let w = world_xf(mesh_v[(part.vbase + i) as usize].pos);
        let mut v = to_view.apply(w);
        if mirror {
            v[0] = -v[0];
        }
        let p = l(VIEW_POS) + 12 * i;
        let e = [rd32(p) as i32 as f64, rd32(p + 4) as i32 as f64, rd32(p + 8) as i32 as f64];
        let mut d = 0f64;
        let mut raw_d = 0f64;
        for k in 0..3 {
            let dk = v[k] - e[k];
            raw_d = raw_d.max(dk.abs());
            d = d.max((dk - (dk / WRAP).round() * WRAP).abs());
        }
        let fp = transforms::project(v, centre, cy, mirror);
        screen.push(fp);
        views.push(v);
        zs.push(e[2] as i32);
        if raw_d > 1000.0 && d < 1000.0 {
            // the engine's 32-bit product wrapped (parts beyond ~122 000 units): its
            // numbers are off by multiples of 2^18 and its z too; counted apart
            vs.wrapped += 1;
            overflowed = true;
            continue;
        }
        vs.vertices += 1;
        vs.sum2 += (0..3).map(|k| (v[k] - e[k]).powi(2)).sum::<f64>();
        let dist = 128.0 - v[2];
        if d > vs.max {
            vs.max = d;
            vs.worst = format!(
                "frame {} {} slot {slot} vertex {i}: scene view ({:.1}, {:.1}, {:.1}), engine ({}, {}, {}), eye distance {dist:.0}",
                crate::glide::FRAME.load(std::sync::atomic::Ordering::Relaxed),
                info.name,
                v[0],
                v[1],
                v[2],
                e[0],
                e[1],
                e[2]
            );
        }
        if d > worst.0 {
            worst = (d, i);
        }
        let band = [1000.0, 4000.0, 16000.0].iter().position(|&bnd| dist < bnd).unwrap_or(3);
        vs.bands[band].0 += 1;
        vs.bands[band].1 = vs.bands[band].1.max(d);
        // on screen in both (the engine's y wraps for points far to the side of the near
        // plane, |y| > 256 |z - 128|: compared only where the scene's point is in the view)
        let packed = rd32(l(PACKED_XY) + 6 * i);
        if let (Some((gx, gy)), false) = (fp, packed == transforms::BEHIND) {
            let (px, py) = transforms::unpack(packed);
            let inside = |x: f64, y: f64| x >= -1.0 && x <= width as f64 + 1.0 && y >= -1.0 && y <= height as f64 + 1.0;
            if px >= 0 && px < width && py >= 0 && py <= height && inside(gx, gy) {
                let (dx, dy) = (gx - px as f64 - 0.5, gy - py as f64 - 0.5);
                vs.onscreen += 1;
                if dx.abs().max(dy.abs()) > vs.max_px {
                    vs.max_px = dx.abs().max(dy.abs());
                    vs.worst_px = format!("{} vertex {i}: grid ({gx:.2}, {gy:.2}), engine ({px}, {py}), eye distance {dist:.0}", info.name);
                }
                vs.sum_px2 += dx * dx + dy * dy;
            }
        }
    }
    if worst.0 > 50.0 {
        let name = info.name.clone();
        note(&mut c.stats, || format!("{kind:?} {name} slot {slot}: vertex {} differs by {:.1} units from the engine's view position", worst.1, worst.0));
    }
    // the culling comparison: main view, world parts the engine placed right (an overflowed
    // part's camera in model space, which its back-face test uses, is wrong too)
    if overflowed {
        c.stats.cull_overflow_parts += 1;
    } else if kind == Kind::World && !mirror {
        c.armed = Some(Armed { mesh, part: part_index, screen, view: views, z: zs, size: (width, height) });
    }
}

fn after_part(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    with_check(|c| {
        let Some(a) = c.armed.take() else { return };
        let Some((_, x)) = c.build.as_ref() else { return };
        let l = |v: u32| m.img.link(v);
        // the records the list builder kept (variant 0: all vertices in front)
        let mut kept = HashSet::new();
        let list = l(POLY_LIST);
        for k in 0..16384u32 {
            let e = rd32(list + 8 * k);
            if e == u32::MAX {
                break;
            }
            if e & 0xf == 0 {
                kept.insert(rd32(list + 8 * k + 4));
            }
        }
        let far = rd32(l(FAR)) as i32;
        let part = &x.meshes[a.mesh as usize].parts[a.part];
        for p in &part.polys {
            let ids: Vec<usize> = p.offs.iter().map(|&o| (o / 6) as usize).collect();
            // in front of the near plane and before the far plane, by the engine's z
            if ids.iter().any(|&i| a.z[i] >= -8 || a.z[i] <= -far || a.screen[i].is_none()) {
                c.stats.cull_skipped += 1;
                continue;
            }
            let skipped = !kept.contains(&p.record) as usize;
            let band = ids.iter().any(|&i| 128.0 - a.z[i] as f64 >= CULL_NEAR) as usize;
            let on = |q: (f64, f64)| q.0 >= 0.0 && q.0 <= a.size.0 as f64 && q.1 >= 0.0 && q.1 <= a.size.1 as f64;
            // each triangle of the fan, as the renderer would cull it
            for t in 1..ids.len() - 1 {
                let (i0, i1, i2) = (ids[0], ids[t], ids[t + 1]);
                let (p0, p1, p2) = (a.screen[i0].unwrap(), a.screen[i1].unwrap(), a.screen[i2].unwrap());
                let cross = (p1.0 - p0.0) * (p2.1 - p0.1) - (p1.1 - p0.1) * (p2.0 - p0.0);
                let w = if cross > 0.0 { 0 } else if cross < 0.0 { 1 } else { 2 };
                c.stats.cull[band][skipped][w] += 1;
                if w < 2 && on(p0) && on(p1) && on(p2) {
                    c.stats.cull_onscreen[band][skipped][w] += 1;
                    c.stats.cull_area_sum[band][skipped][w] += cross.abs() / 2.0;
                }
                // the plane's side of the eye (view z 128) and of the view origin
                let (v0, v1, v2) = (a.view[i0], a.view[i1], a.view[i2]);
                let (e1, e2) = ([v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]], [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]]);
                let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                let side = |q: [f64; 3]| (q[0] - v0[0]) * n[0] + (q[1] - v0[1]) * n[1] + (q[2] - v0[2]) * n[2];
                let (s_eye, s_origin) = (side([0.0, 0.0, 128.0]), side([0.0; 3]));
                if w != 2 && s_eye != 0.0 {
                    c.stats.cull_side_agree[((cross > 0.0) != (s_eye > 0.0)) as usize] += 1;
                }
                if (skipped, w) == (0, 1) || (skipped, w) == (1, 0) {
                    if s_eye * s_origin < 0.0 {
                        c.stats.cull_origin[skipped] += 1;
                    }
                    if on(p0) && on(p1) && on(p2) {
                        c.stats.cull_area[skipped] = c.stats.cull_area[skipped].max(cross.abs() / 2.0);
                    }
                }
            }
        }
    });
    HookResult::Resume(tramp)
}

fn setup(m: &mut Machine, tramp: u32, mode: u8, record: u32) -> HookResult {
    with_check(|c| {
        c.pending = None;
        let l = |a: u32| m.img.link(a);
        let Some((b, x)) = c.build.as_ref() else { return };
        let raw = rd32(l(CUR_INSTANCE));
        let inst = raw & !0x4000_0000;
        let (kind, mesh, phase) = if raw & 0x4000_0000 == 0 {
            let Some(&si) = c.by_obj.get(&inst) else {
                c.stats.poly_unknown += 1;
                return;
            };
            let st = &b.scene.statics[si as usize];
            (Kind::World, st.mesh, st.phase)
        } else {
            let slot = unsafe { mem::rd16(l(PART_ORDER) + 4 * rd32(l(CUR_SLOT))) } as u32;
            let Some(&mesh) = (mem::readable(inst, 0x24)).then(|| x.part_mesh.get(&(rd32(inst), slot))).flatten() else {
                c.stats.poly_unknown += 1;
                return;
            };
            (x.meshes[mesh as usize].kind.unwrap_or(Kind::Other), mesh, phase_of(rd32(inst + 0x20) as i32))
        };
        let Some(&(pi, qi)) = c.poly_of.get(&(mesh, record)) else {
            c.stats.poly_missing += 1;
            let name = x.meshes[mesh as usize].name.clone();
            note(&mut c.stats, || format!("{kind:?} {name}: record {record:#x} (mode {mode}) is not in the scene's mesh"));
            return;
        };
        let poly = &x.meshes[mesh as usize].parts[pi].polys[qi];
        let sm = &b.scene.meshes[mesh as usize];
        let ps = c.stats.poly.entry(kind).or_default();
        ps.polygons += 1;
        *ps.by_mode.entry(mode).or_default() += 1;
        let n = unsafe { mem::rd16(record + 6) } as usize;
        let uvt = rd32(l(UV_PTR));
        let mut bad = Vec::new();
        for j in 0..n.min(4).min(poly.vids.len()) {
            let off = unsafe { mem::rd16(record + 8 + 2 * j as u32) };
            let vert = sm.vertices[poly.vids[j] as usize];
            if off != poly.offs[j] {
                ps.fan_bad += 1;
                bad.push(format!("vertex {j} offset {off} != {}", poly.offs[j]));
            }
            if mode == 2 {
                let c565 = unsafe { mem::rd16(record + 2) } as u32;
                let want = [((c565 & 0xf800) >> 8) as u8, ((c565 & 0x7e0) >> 3) as u8, ((c565 & 0x1f) << 3) as u8, 255];
                if vert.color != want {
                    ps.color_bad += 1;
                    bad.push(format!("colour {:?} != {want:?}", vert.color));
                }
            } else if mode != 5 && mem::readable(uvt.wrapping_add(off as u32), 4) {
                // the game's (u, v) of the vertex, as the set-up copies them (polygons.md)
                let (u, v) = unsafe { (mem::rd16(uvt + off as u32) as i16, mem::rd16(uvt + off as u32 + 2) as i16) };
                ps.uv_vertices += 1;
                if vert.uv != [u as f32 / 256.0, v as f32 / 256.0] {
                    ps.uv_bad += 1;
                    bad.push(format!("vertex {j} uv {:?} x 256 != ({u}, {v})", vert.uv));
                }
            }
        }
        // the scene's triangles: the fan (v0, vi, vi+1) over the record's vertices
        let idx = &sm.indices[poly.first as usize..];
        if (1..poly.vids.len() - 1).any(|t| idx.get(3 * (t - 1)..3 * t) != Some(&[poly.vids[0], poly.vids[t], poly.vids[t + 1]][..])) {
            ps.fan_bad += 1;
            bad.push("fan".into());
        }
        if !bad.is_empty() {
            let name = x.meshes[mesh as usize].name.clone();
            note(&mut c.stats, || format!("{kind:?} {name} record {record:#x} mode {mode}: {}", bad.join(", ")));
        }
        c.pending = Some(Pending { kind, material: poly.material, phase });
    });
    HookResult::Resume(tramp)
}

/// The Glide factor of a blend factor (as source or destination: Glide's DST_COLOR source
/// and SRC_COLOR destination share the value 2).
fn glide_factor(f: BlendFactor) -> u32 {
    match f {
        BlendFactor::Zero => GR_BLEND_ZERO,
        BlendFactor::One => GR_BLEND_ONE,
        BlendFactor::SrcAlpha => GR_BLEND_SRC_ALPHA,
        BlendFactor::InvSrcAlpha => GR_BLEND_ONE_MINUS_SRC_ALPHA,
        BlendFactor::SrcColor | BlendFactor::DstColor => GR_BLEND_SRC_COLOR,
        BlendFactor::InvSrcColor | BlendFactor::InvDstColor => GR_BLEND_ONE_MINUS_SRC_COLOR,
        BlendFactor::DstAlpha => GR_BLEND_DST_ALPHA,
        BlendFactor::InvDstAlpha => GR_BLEND_ONE_MINUS_DST_ALPHA,
    }
}

fn triangle(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let Some(st) = m.glide.as_ref().map(|g| g.st) else { return HookResult::Resume(tramp) };
    with_check(|c| {
        let Some(p) = c.pending.take() else { return };
        let Some((b, x)) = c.build.as_ref() else { return };
        let l = |a: u32| m.img.link(a);
        let mode = rd32(l(BUILDER_MODE)) as u8;
        let mat = b.scene.materials[p.material as usize];
        let ps = c.stats.poly.entry(p.kind).or_default();
        ps.drawn += 1;
        let mut bad = Vec::new();
        if x.mat_mode[p.material as usize] != mode {
            ps.mode_bad += 1;
            bad.push(format!("mode {} != the game's {mode}", x.mat_mode[p.material as usize]));
        }
        if mode != 2 {
            // the texture the game draws ([aa5c0]) against the scene's frame
            let game = rd32(l(POLY_TEXTURE));
            let want = x.tex_of_record.get(&game).copied();
            let got = match mat.anim {
                Some(a) => {
                    // the contract's frame: phase, else FrameData::clock = [1473a0] & 3FFFh;
                    // Env: [7bb98] (the object's +3Ch)
                    let (v, shift, k) = match a.source {
                        AnimSource::Phase if p.phase >= 0 => (p.phase as i64, 14, 0),
                        AnimSource::Phase => ((rd32(l(ANIM_CLOCK)) & 0x3fff) as i64, 14, 1),
                        AnimSource::Env => (rd32(l(PHASE2)) as i32 as i64, 16, 2),
                    };
                    ps.anim_drawn[k] += 1;
                    let f = ((a.count as i64 * v) >> shift).clamp(0, a.count as i64 - 1) as u32;
                    b.scene.texture_frames.get((a.first + f) as usize).copied()
                }
                None => mat.texture,
            };
            if want.is_none() || got != want {
                ps.texture_bad += 1;
                bad.push(format!("texture {got:?} != the game's record {game} (texture {want:?})"));
            }
            // clamp / wrap
            let clamp = if mat.wrap[0] == Wrap::Repeat { GR_TEXTURECLAMP_WRAP } else { GR_TEXTURECLAMP_CLAMP };
            if st.clamp != [clamp; 2] {
                ps.clamp_bad += 1;
                bad.push(format!("clamp {:?}", st.clamp));
            }
        }
        // chroma key and blending
        let (key, blend) = match mat.transparency {
            Transparency::Opaque => (None, (GR_BLEND_ONE, GR_BLEND_ZERO)),
            Transparency::ChromaKey { key } => (Some(key), (GR_BLEND_ONE, GR_BLEND_ZERO)),
            Transparency::Blended(bl) => (None, (glide_factor(bl.src), glide_factor(bl.dst))),
        };
        if (st.chromakey != 0) != key.is_some() || key.is_some_and(|k| k != st.chromakey_value & 0xff_ffff) {
            ps.chroma_bad += 1;
            bad.push(format!("chroma key {} {:#x} vs {key:?}", st.chromakey, st.chromakey_value));
        }
        if (st.blend[0], st.blend[1]) != blend {
            ps.blend_bad += 1;
            bad.push(format!("blend {:?} vs {blend:?}", st.blend));
        }
        if !bad.is_empty() {
            let kind = p.kind;
            note(&mut c.stats, || format!("{kind:?} material {}: {}", p.material, bad.join(", ")));
        }
    });
    HookResult::Resume(tramp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_follows_the_engine() {
        assert_eq!(phase_of(-5), -1);
        assert_eq!(phase_of(0), 0);
        assert_eq!(phase_of(0x4000), 0x1000);
        // +20h >= 80000h: tint flags in bits 17-18 of +20h >> 2, low 16 bits used
        assert_eq!(phase_of(0x8_4000), 0x1000);
    }

    #[test]
    fn object_transform_is_topic_d_placement() {
        // world = Ro v + to (world.md), Ro = R(angles) of topic D's float model
        for (angles, pos) in [([0, 0, 0], [10, -20, 30]), ([0, 512, 0], [-104289, 300, 40994]), ([1948, 1543, 44], [5, 6, 7]), ([2047, 1, 1024], [0, 0, 0])] {
            let t = object_transform(angles, pos);
            let d = transforms::part_to_world(pos.map(|v| v as f64), angles.map(|a| a as f64), &transforms::Affine::IDENTITY);
            for v in [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [-667.0, 264.0, -1609.0]] {
                let (a, b) = (t.apply(v), d.apply(v.map(|c| c as f64)));
                for k in 0..3 {
                    assert!((a[k] - b[k]).abs() < 1e-3, "{angles:?} {v:?}: {a:?} vs {b:?}");
                }
            }
        }
        // heading 512 looks along +x (transforms.md): the model's -z goes to world +x
        let t = object_transform([0, 512, 0], [0, 0, 0]);
        let p = t.apply([0.0, 0.0, -1.0]);
        assert!((p[0] - 1.0).abs() < 1e-6 && p[1].abs() < 1e-6 && p[2].abs() < 1e-6, "{p:?}");
    }

    #[test]
    fn glide_factors() {
        assert_eq!(glide_factor(Blend::ALPHA.src), GR_BLEND_SRC_ALPHA);
        assert_eq!(glide_factor(Blend::ALPHA.dst), GR_BLEND_ONE_MINUS_SRC_ALPHA);
        // the game's 2 x src x dst: (DST_COLOR, SRC_COLOR) = (2, 2)
        assert_eq!((glide_factor(BlendFactor::DstColor), glide_factor(BlendFactor::SrcColor)), (2, 2));
    }
}
