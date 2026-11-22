//! The Direct3D 11 side: a scene's GPU resources, the pipeline states of the materials,
//! per-frame data and the drawing of views.

use super::prepare::{self, Chunk, FlatRange, GpuStatic, GpuVertex, Sphere, StaticRange};
use super::scene::*;
use super::timing::GpuTimer;
use crate::Filter;
use std::collections::HashMap;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface as _, PCSTR, s};

pub(crate) const SHADER: &str = include_str!("native.hlsl");

/// Depth written for view depth z is `DEPTH_SCALE / z` (reversed, infinite far plane). The
/// Glide path writes `oow * DEPTH_SCALE` for its depth-buffered 3D polygons (`voodoo.hlsl`
/// `tex_range.z`, set from this constant in `d3d.rs`), where the game's `oow = 1 / (128 -
/// z')` is one over its view depth w (`transforms.md` step 7). The contract's camera is the
/// game's eye (128 units behind the view origin), so view depth z = w and both paths write
/// the same value for the same point: hybrid frames (native world, Glide-drawn cars) share
/// the depth buffer (verified on the GPU by `gpu_tests::glide_and_native_depth_agree`).
/// Geometry nearer than this is clipped by the GPU.
pub const DEPTH_SCALE: f32 = 1.0 / 256.0;

/// Most layers of one texture array (Direct3D 11 allows 2048).
const MAX_ARRAY_LAYERS: usize = 2048;

pub(super) fn err(e: windows::core::Error) -> String {
    e.to_string()
}

/// Compile `entry` of HLSL `source` for `target` (`vs_5_0`, `ps_5_0`, `cs_5_0`).
pub(crate) fn compile_source(source: &str, entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let r = D3DCompile(source.as_ptr() as *const _, source.len(), s!("native.hlsl"), None, None, entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut errors));
        if let Err(e) = r {
            let msg = errors
                .map(|b: ID3DBlob| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).to_string())
                .unwrap_or_default();
            return Err(format!("native shader compile failed: {e} {msg}"));
        }
        let b = code.unwrap();
        Ok(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec())
    }
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    compile_source(SHADER, entry, target)
}

pub(crate) fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    // SAFETY: plain-old-data GPU structures
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

const F_TEXTURED: u32 = 1;
const F_KEYED: u32 = 2;
const F_REFLECT: u32 = 4;
const F_SKY: u32 = 8;
const F_REPLACE: u32 = 16;
const F_ANIM: u32 = 32;
const F_ANIM_ENV: u32 = 64;
/// `Layer::Decal::stand` (the puffs): tested at the ground plane's depth below it.
const F_STAND: u32 = 128;
/// Not fogged: blends that multiply by the frame buffer (the game's 2 x src x dst decals,
/// `docs/re/native/polygons.md`: it turns the fog off for them; fogging their colour toward
/// the night's black fog darkens instead of lighting, the headlights' beams).
const F_NOFOG: u32 = 256;

const V_MIRROR: u32 = 1;
const V_FADES: u32 = 2;
const V_BACKING: u32 = 4;
const V_DYN_FADES: u32 = 8;

/// Stencil classes the opaque pass leaves per pixel (the front-most opaque surface's):
/// statics 0, moving instances (`FrameData::dynamics`, frame meshes) `ST_DYN`, and their
/// reflective surfaces (`TexGen::Reflection`: the game's car windows) `ST_DYN | ST_WINDOW`.
/// Decals are pulled over statics only (over moving objects they are tested as they are,
/// as the Glide path's decal rules); blended surfaces of moving instances (the game's car
/// glass, stickers and number plates, mode 6) lie over reflective surfaces of moving
/// instances (`docs/re/native/dynamics.md`, "Depth rules").
const ST_DYN: u32 = 0x80;
const ST_WINDOW: u32 = 0x40;
/// The low six bits of a moving object's pixels: its place in the game's drawing order
/// (`FrameData::dynamic_order`, 1 = the nearest car; 0 = none). A decal with an order `m` is
/// drawn over the pixels of places `m` and more whatever the depth (mask `ST_ORDER_MASK`,
/// reference `ST_DYN | m`, less or equal), never over those of nearer places, over statics
/// as any decal: the game draws a puff nearer than a car's position over the car
/// (`docs/re/native/dynamics.md`, "Smoke").
const ST_ORDER: u32 = 0x3f;
const ST_ORDER_MASK: u32 = ST_DYN | ST_ORDER;
/// Statics' pixels of water (`Layer::Water`; the bit is `ST_WINDOW`'s, which only counts
/// with `ST_DYN`): `Layer::Reflection` is drawn over them, whatever the depth, and nowhere
/// else. For the other classes' tests (masks `ST_DYN`, `ST_DYN | ST_WINDOW`,
/// `ST_ORDER_MASK`) water is a static like any other.
const ST_WATER: u32 = 0x40;
/// Blended surfaces of moving instances: tested as if this much nearer over reflective
/// surfaces of moving instances (the game draws a car's glass over its window layer, which
/// lies up to 14.4 units in front of it on China's cars and 29.1 on the Shield, more along a
/// grazing view ray: 128 covers the Shield's side glass seen up to 77 degrees off its
/// normal) ...
pub const OVERLAY_WINDOW_PULL: f32 = 128.0;
/// ... and this much elsewhere (coplanar overlays: within 0.05 units on China's cars).
pub const OVERLAY_PULL: f32 = 1.0;
/// Per-draw pull constants (`DrawCb`: pull = material pull x .0 + .1).
const DRAW_DEFAULT: usize = 0;
const DRAW_NO_PULL: usize = 1;
const DRAW_WINDOW: usize = 2;
const DRAW_OVERLAY: usize = 3;
/// The opaque texels of an overlay (alpha at least .z) write depth and the stencil class,
/// nothing else: an overlay with nothing of its object behind it (the Shield's C-pillar
/// skin over the road) then covers what lies behind it for the decals and the smoke drawn
/// later, as the game's order does.
const DRAW_OVERLAY_DEPTH: usize = 4;
const OVERLAY_OPAQUE_ALPHA: f32 = 0.9;
/// The backing test's opaque passes 0, 1, 2 (`backing.rs`): statics of that pass only
/// (`.w` = pass + 1).
const DRAW_PASS: usize = 5;
const DRAW_CBS: [[f32; 4]; 8] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0, 0.0],
    [0.0, OVERLAY_WINDOW_PULL, 0.0, 0.0],
    [0.0, OVERLAY_PULL, 0.0, 0.0],
    [0.0, 0.0, OVERLAY_OPAQUE_ALPHA, 0.0],
    [1.0, 0.0, 0.0, 1.0],
    [1.0, 0.0, 0.0, 2.0],
    [1.0, 0.0, 0.0, 3.0],
];
/// Submission order of dynamic instances' and frame meshes' draws (after the statics').
const ORDER_DYN: u32 = 1 << 24;
const ORDER_FRAME: u32 = 1 << 30;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GpuMaterial {
    flags: u32,
    slice: u32,
    pull: f32,
    anim_first: u32,
    anim_count: u32,
    /// `Layer::Decal::stand` (with `F_STAND`): the ground's depth under the origin.
    stand: f32,
    pad: [u32; 2],
    env: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Xform {
    rows: [[f32; 4]; 3],
    /// Dynamic instances: reflection scroll (xy), phase (z, integer bits).
    extra: [f32; 4],
    /// Dynamic instances: `FrameData::dynamic_dissolve` (x, y: the earlier and later
    /// phase, integer bits; z: t; w: `DISSOLVE_*` bits).
    dissolve: [f32; 4],
}

/// `Xform::dissolve` w bits: a dissolve, the object is there at the earlier / later step.
const DISSOLVE_ON: u32 = 1;
const DISSOLVE_FROM: u32 = 2;
const DISSOLVE_TO: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ViewCb {
    rot: [[f32; 4]; 3],
    offset: [f32; 4],
    proj: [f32; 4],
    clip: [f32; 4],
    fog_color: [f32; 4],
    fog_table: [[f32; 4]; 16],
    /// Animation clock, env phase, `V_*` flags, number of static fades given.
    misc: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ScreenCb {
    rect: [f32; 4],
    st: [f32; 4],
    color: [f32; 4],
    mode: [u32; 4],
}

/// Drawing passes of a view, in order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum Pass {
    Sky,
    Opaque,
    Decal,
    /// `Layer::Reflection`: after the decals, in submission order.
    Reflection,
    Blended,
}

/// What one draw call shares: pipeline state and texture array.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
struct BatchKey {
    pass: Pass,
    /// `Layer::Water`: its pixels marked `ST_WATER`.
    water: bool,
    /// (src, dst) blend factors; `None` = no blending.
    blend: Option<(u8, u8)>,
    cull: u8,
    wrap: (u8, u8),
    bilinear: bool,
    /// Texture array; `u16::MAX` = none (flat colour).
    array: u16,
}

fn blend_of(code: (u8, u8)) -> Blend {
    let f = |c: u8| match c {
        0 => BlendFactor::Zero,
        1 => BlendFactor::One,
        2 => BlendFactor::SrcAlpha,
        3 => BlendFactor::InvSrcAlpha,
        4 => BlendFactor::SrcColor,
        5 => BlendFactor::InvSrcColor,
        6 => BlendFactor::DstColor,
        7 => BlendFactor::InvDstColor,
        8 => BlendFactor::DstAlpha,
        _ => BlendFactor::InvDstAlpha,
    };
    Blend { src: f(code.0), dst: f(code.1) }
}

/// Upload statistics of a scene.
#[derive(Clone, Copy, Debug, Default)]
pub struct SceneStats {
    pub textures: usize,
    pub texture_arrays: usize,
    /// Bytes of texel data on the GPU (all levels, RGBA8).
    pub texture_bytes: usize,
    pub materials: usize,
    /// Materials with an animation (frames in one array each).
    pub animated: usize,
    pub batches: usize,
    pub chunks: usize,
    pub static_triangles: usize,
    pub mesh_triangles: usize,
    pub vertices: usize,
    /// Vertices with a normal on the GPU (up to the last vertex of a reflection material).
    pub normals: usize,
    /// Triangles of the statics' bases (the backing test).
    pub base_triangles: usize,
    pub upload_ms: f64,
}

/// What the last view drew.
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawStats {
    pub draws: u32,
    pub triangles: u64,
    pub chunks_visible: u32,
    pub instances_visible: u32,
}

/// Where views are drawn: the backend's scene render target.
pub struct Target<'a> {
    pub rtv: &'a ID3D11RenderTargetView,
    pub dsv: &'a ID3D11DepthStencilView,
    /// Target size in pixels.
    pub size: (u32, u32),
    /// The game's screen size (640 x 480).
    pub game: (u32, u32),
    /// Widescreen extension, game pixels on each side.
    pub ext: f32,
    /// Render scale: the coverage target the view marks where it draws (R8, bound as the
    /// second render target; see `d3d.rs`), `None` otherwise.
    pub cov: Option<&'a ID3D11RenderTargetView>,
}

struct GpuMesh {
    ranges: Vec<FlatRange>,
    bounds: Sphere,
}

struct GpuScene {
    vb: ID3D11Buffer,
    ib: ID3D11Buffer,
    arrays: Vec<ID3D11ShaderResourceView>,
    mat_srv: ID3D11ShaderResourceView,
    /// Per static instance: chunk slot, phase, scroll (`GpuStatic`).
    static_srv: ID3D11ShaderResourceView,
    /// Per vertex of `vb` (up to the last one a reflection material uses): its normal.
    normal_srv: ID3D11ShaderResourceView,
    /// The animated materials' frames: layers in their array.
    frame_srv: ID3D11ShaderResourceView,
    /// Texture -> (array, layer) for `View::sky` (the plain variant when there is one).
    sky_layers: HashMap<u32, (u16, u32)>,
    /// Batch of each material.
    mat_batch: Vec<u32>,
    /// Per material: `TexGen::Reflection` (stencil class of moving instances' windows).
    mat_reflect: Vec<bool>,
    batches: Vec<BatchKey>,
    chunks: Vec<Chunk>,
    /// Per chunk: its static instances (fades).
    chunk_statics: Vec<Vec<u32>>,
    statics: Vec<StaticRange>,
    /// Static instances in the scene.
    statics_count: usize,
    meshes: Vec<GpuMesh>,
    stats: SceneStats,
}

/// A dynamic buffer that grows as needed (written whole with `Map(DISCARD)`).
pub(super) struct DynBuf {
    pub(super) buf: Option<ID3D11Buffer>,
    pub(super) srv: Option<ID3D11ShaderResourceView>,
    cap: usize,
    bind: D3D11_BIND_FLAG,
    structured: bool,
}

impl DynBuf {
    pub(super) fn new(bind: D3D11_BIND_FLAG, structured: bool) -> DynBuf {
        DynBuf { buf: None, srv: None, cap: 0, bind, structured }
    }

    pub(super) fn write<T: Copy>(&mut self, device: &ID3D11Device, ctx: &ID3D11DeviceContext, data: &[T]) -> Result<(), String> {
        let size = std::mem::size_of::<T>();
        let n = data.len().max(1);
        if n > self.cap || self.buf.is_none() {
            let cap = n.next_power_of_two().max(64);
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: (cap * size) as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: self.bind.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: if self.structured { D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32 } else { 0 },
                StructureByteStride: if self.structured { size as u32 } else { 0 },
            };
            let mut b = None;
            unsafe { device.CreateBuffer(&desc, None, Some(&mut b)) }.map_err(err)?;
            let b = b.unwrap();
            if self.structured {
                let mut v = None;
                unsafe { device.CreateShaderResourceView(&b, None, Some(&mut v)) }.map_err(err)?;
                self.srv = v;
            }
            self.buf = Some(b);
            self.cap = cap;
        }
        let buf = self.buf.as_ref().unwrap();
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).map_err(err)?;
            std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, m.pData as *mut u8, std::mem::size_of_val(data));
            ctx.Unmap(buf, 0);
        }
        Ok(())
    }
}

/// Per-frame state from `begin_frame`.
#[derive(Default)]
struct FrameState {
    origin: [f64; 3],
    /// Dynamic instances grouped by mesh: (mesh, first entry in the instance buffer,
    /// instance indices into `FrameData::dynamics`).
    groups: Vec<(u32, u32, Vec<u32>)>,
    /// Frame-space bounding sphere of each dynamic instance.
    dyn_bounds: Vec<Sphere>,
    /// Per frame mesh: its ranges in the frame buffers and bounds (frame space).
    frame_meshes: Vec<(Vec<FlatRange>, Sphere)>,
    /// Per chunk: some static instance in it is not faded out (empty = all).
    chunk_live: Vec<bool>,
    /// Static fades given this frame (0 = none: all drawn).
    fades: u32,
    /// The backing test is on this frame (`FrameData::static_backing`): opaque statics in
    /// three passes with the tests between.
    backing: bool,
    /// Per backing pass, per chunk: some static of that pass in it is not faded out.
    chunk_live_pass: [Vec<bool>; 3],
    backing_tag: u64,
    /// `FrameData::dynamic_fade` given (moving objects faded per instance, `V_DYN_FADES`).
    dyn_fades: bool,
    /// `FrameData::dynamic_order` (empty: none).
    order: Vec<u8>,
    clock: u32,
    env_phase: u32,
    begun: bool,
}

/// One draw call of a view.
#[derive(Clone, Copy)]
struct Item {
    batch: u32,
    /// Index buffer of the frame's own geometry instead of the scene's.
    frame_geo: bool,
    start: u32,
    count: u32,
    inst_start: u32,
    inst_count: u32,
    /// View depth of the item's centre (blended sorting).
    dist: f32,
    order: u32,
    /// A moving instance's (or frame mesh's) draw, and whether its material is reflective
    /// (stencil classes, depth rules).
    dynamic: bool,
    reflect: bool,
    /// Opaque statics with the backing test: their pass + 1 (0: not split).
    spass: u8,
    /// Moving instances: `FrameData::dynamic_order` (opaque surfaces and overlays: their
    /// place, written in the stencil's low bits; decals: the first place they cover).
    place: u8,
}

/// Ranks of the opaque draws (`Item::rank`).
const RANK_OPAQUE: u8 = 10;
/// With the backing test: the opaque statics of pass 0, 1, 2 at 10, 12, 14 (the tests run
/// before 12 and 14), the opaque moving instances after them.
const RANK_OPAQUE_DYN: u8 = 16;

impl Item {
    /// Drawing order rank: sky, opaque, blended surfaces of moving instances (overlays, in
    /// submission order), decals (in submission order), other blended (far to near). With
    /// the backing test the opaque statics come first, in their passes.
    fn rank(&self, pass: Pass, backing: bool) -> u8 {
        match pass {
            Pass::Sky => 0,
            Pass::Opaque if self.spass > 0 => RANK_OPAQUE + 2 * (self.spass - 1),
            Pass::Opaque if backing => RANK_OPAQUE_DYN,
            Pass::Opaque => RANK_OPAQUE,
            Pass::Blended if self.dynamic => 20,
            Pass::Decal => 30,
            Pass::Reflection => 32,
            Pass::Blended => 40,
        }
    }
}

/// Texture variants (texture, chroma key, shape) packed into texture arrays by shape.
#[derive(Default)]
struct TexturePacker {
    /// (texture, key, w, h, levels) -> (array, layer)
    variants: HashMap<(u32, Option<u32>, u32, u32, u32), (u16, u32)>,
    groups: Vec<((u32, u32, u32), Vec<Vec<(u32, u32, Vec<u32>)>>)>,
    bytes: usize,
}

impl TexturePacker {
    fn natural(t: &TextureData) -> (u32, u32, u32) {
        (t.width, t.height, prepare::level_count(t))
    }

    /// A group of `shape` with room for `n` more layers.
    fn group_with_room(&mut self, shape: (u32, u32, u32), n: usize) -> usize {
        match self.groups.iter().position(|(s, l)| *s == shape && l.len() + n <= MAX_ARRAY_LAYERS) {
            Some(g) => g,
            None => {
                self.groups.push((shape, Vec::new()));
                self.groups.len() - 1
            }
        }
    }

    /// A new layer in group `g` (remembered as the variant when there is none yet).
    fn add_in(&mut self, g: usize, scene: &SceneData, t: u32, key: Option<u32>) -> (u16, u32) {
        let shape = self.groups[g].0;
        let levels = prepare::texture_levels_as(&scene.textures[t as usize], key, shape.0, shape.1, shape.2);
        self.bytes += levels.iter().map(|l| l.2.len() * 4).sum::<usize>();
        let at = (g as u16, self.groups[g].1.len() as u32);
        self.groups[g].1.push(levels);
        self.variants.entry((t, key, shape.0, shape.1, shape.2)).or_insert(at);
        at
    }

    /// The variant of texture `t` with its own shape (added if new).
    fn add(&mut self, scene: &SceneData, t: u32, key: Option<u32>) -> Option<(u16, u32)> {
        let tex = scene.textures.get(t as usize)?;
        if tex.width == 0 || tex.height == 0 {
            return None;
        }
        let shape = Self::natural(tex);
        if let Some(v) = self.variants.get(&(t, key, shape.0, shape.1, shape.2)) {
            return Some(*v);
        }
        let g = self.group_with_room(shape, 1);
        Some(self.add_in(g, scene, t, key))
    }

    /// The frames of an animation, all in one array (shaped as the first frame): the array
    /// and each frame's layer.
    fn add_frames(&mut self, scene: &SceneData, frames: &[u32], key: Option<u32>) -> Option<(u16, Vec<u32>)> {
        let first = scene.textures.get(*frames.first()? as usize)?;
        if first.width == 0 || first.height == 0 {
            return None;
        }
        let shape = Self::natural(first);
        let mut distinct: Vec<u32> = frames.to_vec();
        distinct.sort();
        distinct.dedup();
        let existing: Vec<Option<(u16, u32)>> = distinct.iter().map(|&t| self.variants.get(&(t, key, shape.0, shape.1, shape.2)).copied()).collect();
        let mut used: Vec<u16> = existing.iter().flatten().map(|v| v.0).collect();
        used.sort();
        used.dedup();
        let new = existing.iter().filter(|e| e.is_none()).count();
        // reuse the layers already made when they are all in one array with room for the
        // rest; otherwise all frames go into one array afresh
        let (g, reuse) = match used.as_slice() {
            [g] if self.groups[*g as usize].1.len() + new <= MAX_ARRAY_LAYERS => (*g as usize, true),
            [] => (self.group_with_room(shape, new), true),
            _ => (self.group_with_room(shape, distinct.len()), false),
        };
        let mut layer_of: HashMap<u32, u32> = HashMap::new();
        for (t, e) in distinct.iter().zip(&existing) {
            let layer = match e {
                Some(v) if reuse => v.1,
                _ => self.add_in(g, scene, *t, key).1,
            };
            layer_of.insert(*t, layer);
        }
        Some((g as u16, frames.iter().map(|t| layer_of[t]).collect()))
    }
}

/// The native renderer's GPU side. Owns no window or target: it draws into the target it
/// is given (the backend's scene render target).
pub struct NativeRenderer {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    ctx1: ID3D11DeviceContext1,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    vs_screen: ID3D11VertexShader,
    ps_screen: ID3D11PixelShader,
    /// `ps` / `ps_screen` that also mark `Target::cov`, and whether this view uses them.
    ps_cov: ID3D11PixelShader,
    ps_screen_cov: ID3D11PixelShader,
    cov_view: bool,
    layout: ID3D11InputLayout,
    cb_view: ID3D11Buffer,
    cb_screen: ID3D11Buffer,
    blends: HashMap<Option<(u8, u8)>, ID3D11BlendState>,
    depth_test: ID3D11DepthStencilState,
    depth_off: ID3D11DepthStencilState,
    /// Opaque: depth written, the stencil class written (reference).
    depth_write_class: ID3D11DepthStencilState,
    /// Tested, nothing written, over pixels whose class bits (mask 80h / C0h) equal the
    /// reference / differ from it.
    test_class_dyn: ID3D11DepthStencilState,
    test_class_window: ID3D11DepthStencilState,
    test_not_window: ID3D11DepthStencilState,
    /// No depth test, over moving objects' pixels of the reference's place or farther
    /// (`ST_ORDER`).
    test_order_far: ID3D11DepthStencilState,
    /// No depth test, nothing written, over statics' water pixels (`ST_WATER`):
    /// `Layer::Reflection`.
    over_water: ID3D11DepthStencilState,
    /// Per-draw constants (`DRAW_*`).
    cb_draw: Vec<ID3D11Buffer>,
    /// No colour written (the overlays' depth pass).
    blend_no_write: ID3D11BlendState,
    rasters: HashMap<(u8, bool), ID3D11RasterizerState>,
    samplers: HashMap<(u8, u8, bool), ID3D11SamplerState>,
    dummy_array: ID3D11ShaderResourceView,
    scene: Option<GpuScene>,
    xforms: DynBuf,
    inst: DynBuf,
    fades: DynBuf,
    frame_vb: DynBuf,
    frame_normals: DynBuf,
    frame_ib: DynBuf,
    frame: FrameState,
    /// The backing test (`backing.rs`) and its per-static words for the shaders.
    backing: super::backing::BackingTest,
    backing_buf: DynBuf,
    /// Scratch lists reused between views.
    items: Vec<Item>,
    visible_chunks: Vec<(bool, f32)>,
    packed_fades: Vec<u32>,
    /// Test 3's draws (moving objects' bases) of the view.
    dyn_draws: Vec<super::backing::DynDraw>,
    /// `Flush()` after drawing view 0 (on by default): the GPU starts on the world while
    /// the game's CPU work for the rest of the frame goes on (`renderer.md`, "Flush").
    pub flush_after_main: bool,
    pub timer: GpuTimer,
    /// What the last drawn view did.
    pub last: DrawStats,
}

impl NativeRenderer {
    pub fn new(device: &ID3D11Device, ctx: &ID3D11DeviceContext) -> Result<NativeRenderer, String> {
        use windows::core::Interface;
        unsafe {
            let vs_code = compile(s!("vs_main"), s!("vs_5_0"))?;
            let ps_code = compile(s!("ps_main"), s!("ps_5_0"))?;
            let vss_code = compile(s!("vs_screen"), s!("vs_5_0"))?;
            let pss_code = compile(s!("ps_screen"), s!("ps_5_0"))?;
            let psc_code = compile(s!("ps_main_cov"), s!("ps_5_0"))?;
            let pssc_code = compile(s!("ps_screen_cov"), s!("ps_5_0"))?;
            let backing = super::backing::BackingTest::new(
                device,
                &compile(s!("vs_base"), s!("vs_5_0"))?,
                &compile(s!("ps_base"), s!("ps_5_0"))?,
                &compile(s!("vs_dyn_base"), s!("vs_5_0"))?,
                &compile(s!("ps_dyn_base"), s!("ps_5_0"))?,
            )?;
            let mut vs = None;
            let mut ps = None;
            let mut vs_screen = None;
            let mut ps_screen = None;
            let mut ps_cov = None;
            let mut ps_screen_cov = None;
            device.CreateVertexShader(&vs_code, None, Some(&mut vs)).map_err(err)?;
            device.CreatePixelShader(&ps_code, None, Some(&mut ps)).map_err(err)?;
            device.CreateVertexShader(&vss_code, None, Some(&mut vs_screen)).map_err(err)?;
            device.CreatePixelShader(&pss_code, None, Some(&mut ps_screen)).map_err(err)?;
            device.CreatePixelShader(&psc_code, None, Some(&mut ps_cov)).map_err(err)?;
            device.CreatePixelShader(&pssc_code, None, Some(&mut ps_screen_cov)).map_err(err)?;
            let el = |name: PCSTR, fmt: DXGI_FORMAT, slot: u32, off: u32, inst: bool| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: fmt,
                InputSlot: slot,
                AlignedByteOffset: off,
                InputSlotClass: if inst { D3D11_INPUT_PER_INSTANCE_DATA } else { D3D11_INPUT_PER_VERTEX_DATA },
                InstanceDataStepRate: inst as u32,
            };
            let elements = [
                el(s!("POSITION"), DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, false),
                el(s!("TEXCOORD"), DXGI_FORMAT_R32G32_FLOAT, 0, 12, false),
                el(s!("COLOR"), DXGI_FORMAT_R8G8B8A8_UNORM, 0, 20, false),
                el(s!("SLOT"), DXGI_FORMAT_R32_UINT, 0, 24, false),
                el(s!("MATERIAL"), DXGI_FORMAT_R32_UINT, 0, 28, false),
                el(s!("INSTSLOT"), DXGI_FORMAT_R32_UINT, 1, 0, true),
            ];
            let mut layout = None;
            device.CreateInputLayout(&elements, &vs_code, Some(&mut layout)).map_err(err)?;
            let cbuf = |size: usize| -> Result<ID3D11Buffer, String> {
                let desc = D3D11_BUFFER_DESC {
                    ByteWidth: size.div_ceil(16) as u32 * 16,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                };
                let mut b = None;
                device.CreateBuffer(&desc, None, Some(&mut b)).map_err(err)?;
                Ok(b.unwrap())
            };
            let depth = |enable: bool, write: bool| -> Result<ID3D11DepthStencilState, String> {
                let d = D3D11_DEPTH_STENCIL_DESC {
                    DepthEnable: enable.into(),
                    DepthWriteMask: if write { D3D11_DEPTH_WRITE_MASK_ALL } else { D3D11_DEPTH_WRITE_MASK_ZERO },
                    // larger is nearer (reversed depth, like the Glide path)
                    DepthFunc: D3D11_COMPARISON_GREATER_EQUAL,
                    ..Default::default()
                };
                let mut s = None;
                device.CreateDepthStencilState(&d, Some(&mut s)).map_err(err)?;
                Ok(s.unwrap())
            };
            // depth with the stencil classes (ST_*)
            let classed = |write: bool, func: D3D11_COMPARISON_FUNC, pass: D3D11_STENCIL_OP, mask: u32| -> Result<ID3D11DepthStencilState, String> {
                let op = D3D11_DEPTH_STENCILOP_DESC { StencilFailOp: D3D11_STENCIL_OP_KEEP, StencilDepthFailOp: D3D11_STENCIL_OP_KEEP, StencilPassOp: pass, StencilFunc: func };
                let d = D3D11_DEPTH_STENCIL_DESC {
                    DepthEnable: true.into(),
                    DepthWriteMask: if write { D3D11_DEPTH_WRITE_MASK_ALL } else { D3D11_DEPTH_WRITE_MASK_ZERO },
                    DepthFunc: D3D11_COMPARISON_GREATER_EQUAL,
                    StencilEnable: true.into(),
                    StencilReadMask: mask as u8,
                    StencilWriteMask: 0xff,
                    FrontFace: op,
                    BackFace: op,
                };
                let mut s = None;
                device.CreateDepthStencilState(&d, Some(&mut s)).map_err(err)?;
                Ok(s.unwrap())
            };
            let test_order_far = {
                let op = D3D11_DEPTH_STENCILOP_DESC {
                    StencilFailOp: D3D11_STENCIL_OP_KEEP,
                    StencilDepthFailOp: D3D11_STENCIL_OP_KEEP,
                    StencilPassOp: D3D11_STENCIL_OP_KEEP,
                    // (reference & mask) <= (stencil & mask): places at the reference's or more
                    StencilFunc: D3D11_COMPARISON_LESS_EQUAL,
                };
                let d = D3D11_DEPTH_STENCIL_DESC {
                    DepthEnable: false.into(),
                    DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ZERO,
                    DepthFunc: D3D11_COMPARISON_ALWAYS,
                    StencilEnable: true.into(),
                    StencilReadMask: ST_ORDER_MASK as u8,
                    StencilWriteMask: 0,
                    FrontFace: op,
                    BackFace: op,
                };
                let mut s = None;
                device.CreateDepthStencilState(&d, Some(&mut s)).map_err(err)?;
                s.unwrap()
            };
            let over_water = {
                let op = D3D11_DEPTH_STENCILOP_DESC {
                    StencilFailOp: D3D11_STENCIL_OP_KEEP,
                    StencilDepthFailOp: D3D11_STENCIL_OP_KEEP,
                    StencilPassOp: D3D11_STENCIL_OP_KEEP,
                    StencilFunc: D3D11_COMPARISON_EQUAL,
                };
                let d = D3D11_DEPTH_STENCIL_DESC {
                    DepthEnable: false.into(),
                    DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ZERO,
                    DepthFunc: D3D11_COMPARISON_ALWAYS,
                    StencilEnable: true.into(),
                    StencilReadMask: (ST_DYN | ST_WATER) as u8,
                    StencilWriteMask: 0,
                    FrontFace: op,
                    BackFace: op,
                };
                let mut s = None;
                device.CreateDepthStencilState(&d, Some(&mut s)).map_err(err)?;
                s.unwrap()
            };
            let mut cb_draw = Vec::with_capacity(DRAW_CBS.len());
            for v in DRAW_CBS {
                let desc = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_IMMUTABLE, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
                let init = D3D11_SUBRESOURCE_DATA { pSysMem: v.as_ptr() as *const _, ..Default::default() };
                let mut b = None;
                device.CreateBuffer(&desc, Some(&init), Some(&mut b)).map_err(err)?;
                cb_draw.push(b.unwrap());
            }
            let blend_no_write = {
                let mut desc = D3D11_BLEND_DESC::default();
                desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: false.into(),
                    SrcBlend: D3D11_BLEND_ONE,
                    DestBlend: D3D11_BLEND_ZERO,
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ONE,
                    DestBlendAlpha: D3D11_BLEND_ZERO,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: 0,
                };
                let mut b = None;
                device.CreateBlendState(&desc, Some(&mut b)).map_err(err)?;
                b.unwrap()
            };
            let white = [0xffff_ffffu32];
            let (_, dummy_array) = create_array(device, 1, 1, &[vec![(1, 1, white.to_vec())]])?;
            Ok(NativeRenderer {
                device: device.clone(),
                ctx: ctx.clone(),
                ctx1: ctx.cast().map_err(err)?,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                vs_screen: vs_screen.unwrap(),
                ps_screen: ps_screen.unwrap(),
                ps_cov: ps_cov.unwrap(),
                ps_screen_cov: ps_screen_cov.unwrap(),
                cov_view: false,
                layout: layout.unwrap(),
                cb_view: cbuf(std::mem::size_of::<ViewCb>())?,
                cb_screen: cbuf(std::mem::size_of::<ScreenCb>())?,
                blends: HashMap::new(),
                depth_test: depth(true, false)?,
                depth_off: depth(false, false)?,
                depth_write_class: classed(true, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_REPLACE, 0xff)?,
                test_class_dyn: classed(false, D3D11_COMPARISON_EQUAL, D3D11_STENCIL_OP_KEEP, ST_DYN)?,
                test_class_window: classed(false, D3D11_COMPARISON_EQUAL, D3D11_STENCIL_OP_KEEP, ST_DYN | ST_WINDOW)?,
                test_not_window: classed(false, D3D11_COMPARISON_NOT_EQUAL, D3D11_STENCIL_OP_KEEP, ST_DYN | ST_WINDOW)?,
                test_order_far,
                over_water,
                cb_draw,
                blend_no_write,
                rasters: HashMap::new(),
                samplers: HashMap::new(),
                dummy_array,
                scene: None,
                xforms: DynBuf::new(D3D11_BIND_SHADER_RESOURCE, true),
                inst: DynBuf::new(D3D11_BIND_VERTEX_BUFFER, false),
                fades: DynBuf::new(D3D11_BIND_SHADER_RESOURCE, true),
                frame_vb: DynBuf::new(D3D11_BIND_VERTEX_BUFFER, false),
                frame_normals: DynBuf::new(D3D11_BIND_SHADER_RESOURCE, true),
                frame_ib: DynBuf::new(D3D11_BIND_INDEX_BUFFER, false),
                frame: FrameState::default(),
                backing,
                backing_buf: DynBuf::new(D3D11_BIND_SHADER_RESOURCE, true),
                items: Vec::new(),
                visible_chunks: Vec::new(),
                packed_fades: Vec::new(),
                dyn_draws: Vec::new(),
                // SR_NATIVE_FLUSH=0 (diagnostics): no flush after the main view
                flush_after_main: std::env::var("SR_NATIVE_FLUSH").map_or(true, |v| v.trim() != "0"),
                timer: GpuTimer::new(device)?,
                last: DrawStats::default(),
            })
        }
    }

    /// A scene is uploaded.
    pub fn has_scene(&self) -> bool {
        self.scene.is_some()
    }

    /// Statistics of the uploaded scene.
    pub fn scene_stats(&self) -> Option<SceneStats> {
        self.scene.as_ref().map(|s| s.stats)
    }

    /// Upload a scene (replacing the previous one): textures expanded to RGBA8 in texture
    /// arrays by size (an animation's frames in one array), statics merged into chunks,
    /// meshes for instancing, materials.
    pub fn upload(&mut self, scene: &SceneData) -> Result<SceneStats, String> {
        let t0 = std::time::Instant::now();
        self.scene = None;
        let mut stats = SceneStats { textures: scene.textures.len(), materials: scene.materials.len(), ..Default::default() };
        let key_of = |m: &Material| match m.transparency {
            Transparency::ChromaKey { key } => Some(key & 0xff_ffff),
            _ => None,
        };

        // textures: animated materials first (each one's frames in one array), then the
        // plain ones, then textures no material names (the sky)
        let mut packer = TexturePacker::default();
        let mut referenced = vec![false; scene.textures.len()];
        // per material: (array, layer of frame 0) and the animation (first entry, count)
        let mut mat_tex: Vec<Option<(u16, u32)>> = vec![None; scene.materials.len()];
        let mut mat_anim: Vec<Option<(u32, u32)>> = vec![None; scene.materials.len()];
        let mut frame_slices: Vec<u32> = Vec::new();
        for (i, m) in scene.materials.iter().enumerate() {
            let (Some(_), Some(a)) = (m.texture, m.anim) else { continue };
            let lo = (a.first as usize).min(scene.texture_frames.len());
            let hi = (a.first as usize).saturating_add(a.count.min(65535) as usize).min(scene.texture_frames.len());
            let frames: Vec<u32> = scene.texture_frames[lo..hi].iter().copied().filter(|&t| (t as usize) < scene.textures.len()).collect();
            if frames.is_empty() {
                continue;
            }
            if let Some((g, layers)) = packer.add_frames(scene, &frames, key_of(m)) {
                for &t in &frames {
                    referenced[t as usize] = true;
                }
                mat_tex[i] = Some((g, layers[0]));
                mat_anim[i] = Some((frame_slices.len() as u32, layers.len() as u32));
                frame_slices.extend(layers);
                stats.animated += 1;
            }
        }
        for (i, m) in scene.materials.iter().enumerate() {
            let Some(t) = m.texture else { continue };
            if mat_tex[i].is_some() {
                continue;
            }
            mat_tex[i] = packer.add(scene, t, key_of(m));
            if let Some(r) = referenced.get_mut(t as usize) {
                *r = true;
            }
        }
        for t in 0..scene.textures.len() {
            if !referenced[t] {
                packer.add(scene, t as u32, None);
            }
        }
        let mut sky_layers: HashMap<u32, (u16, u32)> = HashMap::new();
        for (&(t, key, w, h, l), &v) in &packer.variants {
            let plain = key.is_none() && scene.textures.get(t as usize).is_some_and(|tex| TexturePacker::natural(tex) == (w, h, l));
            if plain {
                sky_layers.insert(t, v);
            } else {
                sky_layers.entry(t).or_insert(v);
            }
        }
        let mut arrays = Vec::with_capacity(packer.groups.len());
        for ((w, h, _), layers) in &packer.groups {
            arrays.push(create_array(&self.device, *w, *h, layers)?.1);
        }
        stats.texture_arrays = arrays.len();
        stats.texture_bytes = packer.bytes;

        // materials: GPU table and batch keys
        let mut gpu_mats = Vec::with_capacity(scene.materials.len().max(1));
        let mut keys = Vec::with_capacity(scene.materials.len());
        let mut reflective = vec![false; scene.materials.len()];
        for (i, m) in scene.materials.iter().enumerate() {
            let var = mat_tex[i];
            let mut flags = 0;
            if var.is_some() {
                flags |= F_TEXTURED;
            }
            if key_of(m).is_some() {
                flags |= F_KEYED;
            }
            let mut env = [1.0, 1.0, 0.0, 0.0];
            if let TexGen::Reflection { scale, offset } = m.texgen {
                flags |= F_REFLECT;
                env = [scale[0], scale[1], offset[0], offset[1]];
                reflective[i] = true;
            }
            if m.layer == Layer::Sky {
                flags |= F_SKY;
            }
            if m.combine == Combine::Replace {
                flags |= F_REPLACE;
            }
            if matches!(m.transparency, Transparency::Blended(b) if b.src == BlendFactor::DstColor || b.dst == BlendFactor::SrcColor) {
                flags |= F_NOFOG;
            }
            let (anim_first, anim_count) = mat_anim[i].unwrap_or((0, 0));
            if mat_anim[i].is_some() {
                flags |= F_ANIM;
                if m.anim.is_some_and(|a| a.source == AnimSource::Env) {
                    flags |= F_ANIM_ENV;
                }
            }
            let (pull, stand) = match m.layer {
                Layer::Decal { pull, stand } => (pull.max(1e-3), stand),
                _ => (0.0, None),
            };
            if stand.is_some() {
                flags |= F_STAND;
            }
            gpu_mats.push(GpuMaterial { flags, slice: var.map_or(0, |v| v.1), pull, anim_first, anim_count, stand: stand.unwrap_or(0.0), pad: [0; 2], env });
            let blend = match m.transparency {
                Transparency::Blended(b) => Some((b.src as u8, b.dst as u8)),
                _ => None,
            };
            let pass = match (m.layer, blend.is_some()) {
                (Layer::Sky, _) => Pass::Sky,
                (Layer::Decal { .. }, _) => Pass::Decal,
                (Layer::Reflection, _) => Pass::Reflection,
                (Layer::World | Layer::Water, true) => Pass::Blended,
                (Layer::World | Layer::Water, false) => Pass::Opaque,
            };
            let w = |x: Wrap| (x == Wrap::Clamp) as u8;
            keys.push(BatchKey {
                pass,
                water: m.layer == Layer::Water,
                blend,
                cull: m.cull as u8,
                wrap: (w(m.wrap[0]), w(m.wrap[1])),
                bilinear: m.bilinear,
                array: var.map_or(u16::MAX, |v| v.0),
            });
        }
        if gpu_mats.is_empty() {
            gpu_mats.push(GpuMaterial::default());
        }
        let mut batches: Vec<BatchKey> = keys.clone();
        batches.sort();
        batches.dedup();
        let mat_batch: Vec<u32> = keys.iter().map(|k| batches.binary_search(k).unwrap() as u32).collect();
        stats.batches = batches.len();

        // geometry: every mesh for instancing, then the merged statics (meshes first, so that
        // the normals, needed up to the last vertex of a reflection material, usually stop
        // before the statics)
        let mut verts: Vec<GpuVertex> = Vec::new();
        let mut normals: Vec<[f32; 3]> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        let mut meshes = Vec::with_capacity(scene.meshes.len());
        for m in &scene.meshes {
            let ranges = prepare::flatten_mesh(m, 0, &mut verts, &mut normals, &mut idx);
            stats.mesh_triangles += ranges.iter().map(|r| r.count as usize / 3).sum::<usize>();
            meshes.push(GpuMesh { ranges, bounds: prepare::bounds(m.vertices.iter().map(|v| v.pos)) });
        }
        let g = prepare::build_statics(scene, &|m| mat_batch.get(m as usize).copied().unwrap_or(0));
        // the backing test's bases: from the polygons that write depth (opaque, colour-keyed)
        let writes_depth = |m: u32| mat_batch.get(m as usize).is_some_and(|&b| batches[b as usize].pass == Pass::Opaque);
        stats.base_triangles = self.backing.upload(scene, &g, &writes_depth)?;
        stats.chunks = g.chunks.len();
        stats.static_triangles = g.indices.len() / 3;
        let (vbase, ibase) = (verts.len() as u32, idx.len() as u32);
        verts.extend_from_slice(&g.vertices);
        normals.extend_from_slice(&g.normals);
        idx.extend(g.indices.iter().map(|i| i + vbase));
        let static_ranges: Vec<StaticRange> = g.ranges.iter().map(|r| StaticRange { start: r.start + ibase, ..*r }).collect();
        stats.vertices = verts.len();
        let normal_end = verts.iter().rposition(|v| reflective.get(v.material as usize).copied().unwrap_or(false)).map_or(0, |i| i + 1);
        stats.normals = normal_end;
        normals.truncate(normal_end);
        if normals.is_empty() {
            normals.push([0.0; 3]);
        }
        if verts.is_empty() {
            verts.push(GpuVertex::default());
        }
        if idx.is_empty() {
            idx.push(0);
        }
        let mut static_table = g.statics.clone();
        if static_table.is_empty() {
            static_table.push(GpuStatic::default());
        }
        if frame_slices.is_empty() {
            frame_slices.push(0);
        }
        let vb = immutable_buffer(&self.device, as_bytes(&verts), D3D11_BIND_VERTEX_BUFFER, 0)?;
        let ib = immutable_buffer(&self.device, as_bytes(&idx), D3D11_BIND_INDEX_BUFFER, 0)?;
        let srv = |data: &[u8], stride: usize| -> Result<ID3D11ShaderResourceView, String> {
            let b = immutable_buffer(&self.device, data, D3D11_BIND_SHADER_RESOURCE, stride as u32)?;
            let mut v = None;
            unsafe { self.device.CreateShaderResourceView(&b, None, Some(&mut v)) }.map_err(err)?;
            v.ok_or_else(|| "no view".into())
        };
        let mat_srv = srv(as_bytes(&gpu_mats), std::mem::size_of::<GpuMaterial>())?;
        let static_srv = srv(as_bytes(&static_table), std::mem::size_of::<GpuStatic>())?;
        let normal_srv = srv(as_bytes(&normals), 12)?;
        let frame_srv = srv(as_bytes(&frame_slices), 4)?;
        stats.upload_ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.scene = Some(GpuScene {
            vb,
            ib,
            arrays,
            mat_srv,
            static_srv,
            normal_srv,
            frame_srv,
            sky_layers,
            mat_batch,
            mat_reflect: reflective,
            batches,
            chunks: g.chunks,
            chunk_statics: g.chunk_statics,
            statics: static_ranges,
            statics_count: scene.statics.len(),
            meshes,
            stats,
        });
        self.frame = FrameState::default();
        Ok(stats)
    }

    /// Set up a frame: transforms (camera-relative, computed in f64) with each dynamic
    /// instance's phase and scroll, the static fades, the instance list and the frame's own
    /// geometry. Call once before the frame's views.
    pub fn begin_frame(&mut self, frame: &FrameData) -> Result<(), String> {
        let Some(sc) = &self.scene else { return Ok(()) };
        let origin = frame.views.first().map_or([0.0; 3], |v| v.camera.pos);
        let rel = |p: [f64; 3]| [(p[0] - origin[0]) as f32, (p[1] - origin[1]) as f32, (p[2] - origin[2]) as f32];
        let xf = |basis: &[f32; 9], t: [f32; 3], extra: [f32; 4]| Xform {
            rows: [[basis[0], basis[1], basis[2], t[0]], [basis[3], basis[4], basis[5], t[1]], [basis[6], basis[7], basis[8], t[2]]],
            extra,
            dissolve: [0.0; 4],
        };
        let phase_bits = |p: i32| f32::from_bits(p as u32);
        let ident = Transform::IDENTITY.basis;
        let mut xforms: Vec<Xform> = Vec::with_capacity(sc.chunks.len() + frame.dynamics.len() + frame.meshes.len());
        xforms.extend(sc.chunks.iter().map(|c| xf(&ident, rel(c.origin), [0.0; 4])));
        let dyn_slot = xforms.len() as u32;
        let mut dyn_bounds = Vec::with_capacity(frame.dynamics.len());
        for (i, d) in frame.dynamics.iter().enumerate() {
            // w: the instance's fade and its tested object + 1 (native.hlsl, Xform)
            let fade = frame.dynamic_fade.get(i).copied().unwrap_or(255) as u32;
            let obj = frame.dynamic_backing.get(i).map_or(0, |b| if b.object == DynamicBacking::NONE.object { 0 } else { b.object + 1 });
            let mut x = xf(&d.transform.basis, rel(d.transform.origin), [d.scroll[0], d.scroll[1], phase_bits(d.phase), f32::from_bits(fade | obj << 8)]);
            if let Some(ds) = frame.dynamic_dissolve.get(i).filter(|ds| ds.active()) {
                let bits = DISSOLVE_ON | if ds.from.is_some() { DISSOLVE_FROM } else { 0 } | if ds.to.is_some() { DISSOLVE_TO } else { 0 };
                let (from, to) = (ds.from.or(ds.to).unwrap_or(d.phase), ds.to.or(ds.from).unwrap_or(d.phase));
                x.dissolve = [phase_bits(from), phase_bits(to), ds.t.clamp(0.0, 1.0), f32::from_bits(bits)];
            }
            xforms.push(x);
            let b = sc.meshes.get(d.mesh as usize).map_or(Sphere::default(), |m| m.bounds);
            let c = d.transform.apply(b.center);
            dyn_bounds.push(Sphere { center: rel(c), radius: b.radius * prepare::max_scale(&d.transform.basis) });
        }
        // instances grouped by mesh; entry 0 is slot 0 (static and frame geometry)
        let mut order: Vec<u32> = (0..frame.dynamics.len() as u32).filter(|&i| (frame.dynamics[i as usize].mesh as usize) < sc.meshes.len()).collect();
        order.sort_by_key(|&i| frame.dynamics[i as usize].mesh);
        let mut inst: Vec<u32> = Vec::with_capacity(order.len() + 1);
        inst.push(0);
        let mut groups: Vec<(u32, u32, Vec<u32>)> = Vec::new();
        for &i in &order {
            let mesh = frame.dynamics[i as usize].mesh;
            if groups.last().is_none_or(|g| g.0 != mesh) {
                groups.push((mesh, inst.len() as u32, Vec::new()));
            }
            groups.last_mut().unwrap().2.push(i);
            inst.push(dyn_slot + i);
        }
        // the frame's own geometry, one slot per mesh (no instance: the clock animates it)
        let mut fv: Vec<GpuVertex> = Vec::new();
        let mut fnorm: Vec<[f32; 3]> = Vec::new();
        let mut fi: Vec<u32> = Vec::new();
        let mut frame_meshes = Vec::with_capacity(frame.meshes.len());
        for fm in &frame.meshes {
            let slot = xforms.len() as u32;
            xforms.push(xf(&ident, rel(fm.origin), [0.0, 0.0, phase_bits(-1), f32::from_bits(255)]));
            let ranges = prepare::flatten_mesh(&fm.mesh, slot, &mut fv, &mut fnorm, &mut fi);
            let mut b = prepare::bounds(fm.mesh.vertices.iter().map(|v| v.pos));
            let o = rel(fm.origin);
            b.center = [b.center[0] + o[0], b.center[1] + o[1], b.center[2] + o[2]];
            frame_meshes.push((ranges, b));
        }
        // static fades: four per uint; chunks whose statics are all faded out are skipped
        let fades = frame.static_fade.len().min(prepare::STATIC_BIT as usize - 1);
        let mut chunk_live = Vec::new();
        if fades > 0 {
            let fade = |s: u32| frame.static_fade.get(s as usize).copied().unwrap_or(255);
            chunk_live = sc.chunk_statics.iter().map(|list| list.iter().any(|&s| fade(s) > 0)).collect();
            let packed = &mut self.packed_fades;
            packed.clear();
            packed.extend(frame.static_fade[..fades].chunks(4).map(|c| c.iter().enumerate().fold(0u32, |a, (k, &f)| a | (f as u32) << (8 * k))));
        }
        // the backing test: the statics' passes and tests (backing.rs)
        let backing = self.backing.begin_frame(&self.device, &self.ctx, frame, sc.statics_count, origin[1])?;
        let mut chunk_live_pass: [Vec<bool>; 3] = Default::default();
        if backing {
            let fade = |s: u32| frame.static_fade.get(s as usize).copied().unwrap_or(255);
            let words = &self.backing.words;
            for (p, live) in chunk_live_pass.iter_mut().enumerate() {
                *live = sc.chunk_statics.iter().map(|list| list.iter().any(|&s| fade(s) > 0 && words.get(s as usize).is_some_and(|w| (w & 3) as usize == p))).collect();
            }
        }
        let (device, ctx) = (self.device.clone(), self.ctx.clone());
        if backing {
            let words = std::mem::take(&mut self.backing.words);
            let r = self.backing_buf.write(&device, &ctx, &words);
            self.backing.words = words;
            r?;
        }
        self.xforms.write(&device, &ctx, &xforms)?;
        self.inst.write(&device, &ctx, &inst)?;
        if fades > 0 {
            let packed = std::mem::take(&mut self.packed_fades);
            let r = self.fades.write(&device, &ctx, &packed);
            self.packed_fades = packed;
            r?;
        }
        if !fi.is_empty() {
            self.frame_vb.write(&device, &ctx, &fv)?;
            self.frame_normals.write(&device, &ctx, &fnorm)?;
            self.frame_ib.write(&device, &ctx, &fi)?;
        }
        self.frame = FrameState {
            origin,
            groups,
            dyn_bounds,
            frame_meshes,
            chunk_live,
            fades: fades as u32,
            backing,
            chunk_live_pass,
            backing_tag: frame.backing_tag,
            dyn_fades: !frame.dynamic_fade.is_empty(),
            order: frame.dynamic_order.iter().map(|&o| o.min(ST_ORDER as u8 + 1)).collect(),
            clock: frame.clock,
            env_phase: frame.env_phase,
            begun: true,
        };
        Ok(())
    }

    /// Draw view `index` of the frame set up by `begin_frame` into `target`: depth and
    /// stencil are cleared first (the whole target: views are drawn one after the other),
    /// then the view's colour clear, its sky and ground, the world; the depth is left for
    /// whatever is drawn after (see [`DEPTH_SCALE`]). After view 0 the commands are flushed
    /// (`flush_after_main`).
    pub fn draw_view(&mut self, frame: &FrameData, index: usize, target: &Target, filter: Filter) -> Result<(), String> {
        self.last = DrawStats::default();
        let Some(sc) = self.scene.take() else { return Ok(()) };
        let r = self.draw_view_in(&sc, frame, index, target, filter);
        self.scene = Some(sc);
        if index == 0 && self.flush_after_main {
            unsafe { self.ctx.Flush() };
        }
        r
    }

    fn draw_view_in(&mut self, sc: &GpuScene, frame: &FrameData, index: usize, target: &Target, filter: Filter) -> Result<(), String> {
        let (Some(view), true) = (frame.views.get(index), self.frame.begun) else { return Ok(()) };
        let setup = prepare::view_setup(view, target.ext);
        let cam = &view.camera;
        let o = self.frame.origin;
        let offset = [(o[0] - cam.pos[0]) as f32, (o[1] - cam.pos[1]) as f32, (o[2] - cam.pos[2]) as f32];
        let to_view = |c: [f32; 3]| prepare::rotate(&cam.rot, [c[0] + offset[0], c[1] + offset[1], c[2] + offset[2]]);

        // constants
        let r = &cam.rot;
        let mut cb = ViewCb {
            rot: [[r[0], r[1], r[2], 0.0], [r[3], r[4], r[5], 0.0], [r[6], r[7], r[8], 0.0]],
            offset: [offset[0], offset[1], offset[2], DEPTH_SCALE],
            proj: [setup.px[0], setup.px[1], setup.py[0], setup.py[1]],
            clip: [cam.proj.near, cam.proj.far, 0.0, 1.0],
            misc: [
                self.frame.clock,
                self.frame.env_phase,
                if view.mirror { V_MIRROR } else { 0 } | if self.frame.fades > 0 { V_FADES } else { 0 } | if self.frame.dyn_fades { V_DYN_FADES } else { 0 },
                self.frame.fades,
            ],
            ..Default::default()
        };
        if let Some(f) = &view.fog {
            cb.clip[2] = 1.0;
            cb.clip[3] = f.w_scale;
            cb.fog_color = [((f.color >> 16) & 0xff) as f32 / 255.0, ((f.color >> 8) & 0xff) as f32 / 255.0, (f.color & 0xff) as f32 / 255.0, 1.0];
            for i in 0..64 {
                cb.fog_table[i / 4][i % 4] = f.table[i] as f32 / 255.0;
            }
        }

        // the viewport in target pixels
        let sx = target.size.0 as f32 / (target.game.0 as f32 + 2.0 * target.ext);
        let sy = target.size.1 as f32 / target.game.1 as f32;
        let vp = setup.viewport;
        let (x0, y0) = (((vp[0] + target.ext) * sx).max(0.0), (vp[1] * sy).max(0.0));
        let (x1, y1) = (((vp[2] + target.ext) * sx).min(target.size.0 as f32), (vp[3] * sy).min(target.size.1 as f32));
        if x1 <= x0 || y1 <= y0 {
            return Ok(());
        }
        // NDC spans the unclipped rectangle (the clamp above only matters off-target)
        let d3d_vp = viewport_in_target(vp, target.ext, (sx, sy));

        // culling and draw list
        let mut items = std::mem::take(&mut self.items);
        items.clear();
        let mut order = 0u32;
        let mut push = |items: &mut Vec<Item>, mut it: Item| {
            it.order = order;
            order += 1;
            items.push(it);
        };
        let mut vis = std::mem::take(&mut self.visible_chunks);
        vis.clear();
        let live = &self.frame.chunk_live;
        for (ci, c) in sc.chunks.iter().enumerate() {
            let rc = [(c.origin[0] - o[0]) as f32 + c.bounds.center[0], (c.origin[1] - o[1]) as f32 + c.bounds.center[1], (c.origin[2] - o[2]) as f32 + c.bounds.center[2]];
            let v = to_view(rc);
            let alive = live.get(ci).copied().unwrap_or(true);
            vis.push((alive && prepare::sphere_visible(&setup.planes, v, c.bounds.radius), v[2]));
        }
        let backing = self.frame.backing;
        // the backing test's passes: a chunk is drawn in a pass when a static of that pass in
        // it is not faded out
        let live_pass = |sp: u8, ci: usize| if sp == 0 { live.get(ci).copied().unwrap_or(true) } else { self.frame.chunk_live_pass[sp as usize - 1].get(ci).copied().unwrap_or(false) };
        self.last.chunks_visible = vis.iter().filter(|v| v.0).count() as u32;
        // statics: consecutive visible chunks of a batch are one draw (blended: one per chunk,
        // sorted); with the backing test, opaque ones once per pass
        let mut runs: [Option<Item>; 4] = [None; 4];
        for r in &sc.statics {
            let (visible, dist) = vis[r.chunk as usize];
            let pass = sc.batches[r.batch as usize].pass;
            let split: &[u8] = if backing && pass == Pass::Opaque { &[1, 2, 3] } else { &[0] };
            for &sp in split {
                let run = &mut runs[sp as usize];
                // the sky is around the camera wherever its instance was placed
                let ok = (visible || pass == Pass::Sky) && live_pass(sp, r.chunk as usize);
                if !ok {
                    if let Some(it) = run.take() {
                        push(&mut items, it);
                    }
                    continue;
                }
                let it = Item { batch: r.batch, frame_geo: false, start: r.start, count: r.count, inst_start: 0, inst_count: 1, dist, order: 0, dynamic: false, reflect: false, spass: sp, place: 0 };
                if pass == Pass::Blended {
                    if let Some(prev) = run.take() {
                        push(&mut items, prev);
                    }
                    push(&mut items, it);
                    continue;
                }
                match run {
                    Some(prev) if prev.batch == r.batch && prev.start + prev.count == r.start => prev.count += r.count,
                    _ => {
                        if let Some(prev) = run.replace(it) {
                            push(&mut items, prev);
                        }
                    }
                }
            }
        }
        for run in &mut runs {
            if let Some(it) = run.take() {
                push(&mut items, it);
            }
        }
        // dynamic instances: runs of visible instances of a mesh are one instanced draw
        let mut inst_visible = 0;
        let mut dyn_draws = std::mem::take(&mut self.dyn_draws);
        dyn_draws.clear();
        // the instances' places in the game's order (FrameData::dynamic_order): one stencil
        // reference per draw, so a run holds one place; decals with a place (the puffs) are
        // drawn exactly in submission order (far to near: they blend over each other)
        let place = |i: u32| self.frame.order.get(i as usize).copied().unwrap_or(0);
        for (mesh, first, members) in &self.frame.groups {
            let gm = &sc.meshes[*mesh as usize];
            let decal = gm.ranges.iter().any(|r| sc.batches[sc.mat_batch[r.material as usize] as usize].pass == Pass::Decal);
            let mut k = 0;
            while k < members.len() {
                let b = self.frame.dyn_bounds[members[k] as usize];
                let v = to_view(b.center);
                let sky = gm.ranges.first().is_some_and(|r| sc.batches[sc.mat_batch[r.material as usize] as usize].pass == Pass::Sky);
                if !sky && !prepare::sphere_visible(&setup.planes, v, b.radius) {
                    k += 1;
                    continue;
                }
                let start = k;
                let mut dists = vec![v[2]];
                k += 1;
                while k < members.len() {
                    let b = self.frame.dyn_bounds[members[k] as usize];
                    let v = to_view(b.center);
                    if !sky && !prepare::sphere_visible(&setup.planes, v, b.radius) {
                        break;
                    }
                    let p = place(members[start]);
                    if place(members[k]) != p || (decal && p > 0 && members[k] != members[k - 1] + 1) {
                        break;
                    }
                    dists.push(v[2]);
                    k += 1;
                }
                inst_visible += (k - start) as u32;
                if self.backing.objects() > 0 {
                    // test 3 (moving objects' bases): the mesh's ranges, merged where they
                    // follow each other in the index buffer
                    for fr in &gm.ranges {
                        match dyn_draws.last_mut() {
                            Some(d) if d.inst_start == first + start as u32 && d.inst_count == (k - start) as u32 && d.start + d.count == fr.start => d.count += fr.count,
                            _ => dyn_draws.push(super::backing::DynDraw { start: fr.start, count: fr.count, inst_start: first + start as u32, inst_count: (k - start) as u32 }),
                        }
                    }
                }
                for (ri, fr) in gm.ranges.iter().enumerate() {
                    let batch = sc.mat_batch[fr.material as usize];
                    let reflect = sc.mat_reflect.get(fr.material as usize).copied().unwrap_or(false);
                    // submission order: the instance's place in FrameData::dynamics (decals and
                    // overlays are drawn in it), then the mesh's ranges
                    let order = |j: usize| ORDER_DYN + members[j] * 64 + ri.min(63) as u32;
                    let base = Item {
                        batch,
                        frame_geo: false,
                        start: fr.start,
                        count: fr.count,
                        inst_start: first + start as u32,
                        inst_count: (k - start) as u32,
                        dist: dists[0],
                        order: order(start),
                        dynamic: true,
                        reflect,
                        spass: 0,
                        place: place(members[start]),
                    };
                    if sc.batches[batch as usize].pass == Pass::Blended {
                        for (j, d) in dists.iter().enumerate() {
                            items.push(Item { inst_start: first + (start + j) as u32, inst_count: 1, dist: *d, order: order(start + j), ..base });
                        }
                    } else {
                        items.push(base);
                    }
                }
            }
        }
        self.last.instances_visible = inst_visible;
        // the frame's own geometry
        for (mi, (ranges, b)) in self.frame.frame_meshes.iter().enumerate() {
            let v = to_view(b.center);
            if !prepare::sphere_visible(&setup.planes, v, b.radius) {
                continue;
            }
            for (ri, fr) in ranges.iter().enumerate() {
                let batch = sc.mat_batch[fr.material as usize];
                let reflect = sc.mat_reflect.get(fr.material as usize).copied().unwrap_or(false);
                let order = ORDER_FRAME + mi as u32 * 64 + ri.min(63) as u32;
                items.push(Item { batch, frame_geo: true, start: fr.start, count: fr.count, inst_start: 0, inst_count: 1, dist: v[2], order, dynamic: true, reflect, spass: 0, place: 0 });
            }
        }
        // order: sky, opaque by state, moving instances' blended surfaces and decals as
        // submitted, other blended back to front
        items.sort_by(|a, b| {
            let (pa, pb) = (sc.batches[a.batch as usize].pass, sc.batches[b.batch as usize].pass);
            a.rank(pa, backing).cmp(&b.rank(pb, backing)).then_with(|| match a.rank(pa, backing) {
                RANK_OPAQUE..=RANK_OPAQUE_DYN => a.batch.cmp(&b.batch).then(a.order.cmp(&b.order)),
                40 => b.dist.total_cmp(&a.dist).then(a.order.cmp(&b.order)),
                _ => a.order.cmp(&b.order),
            })
        });

        // clears, then the game's sky and ground
        unsafe {
            let ctx = &self.ctx;
            if let Some(c) = view.clear {
                let col = [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0];
                let rect = RECT { left: x0 as i32, top: y0 as i32, right: x1.ceil() as i32, bottom: y1.ceil() as i32 };
                self.ctx1.ClearView(target.rtv, &col, Some(&[rect]));
                if let Some(cov) = target.cov {
                    self.ctx1.ClearView(cov, &[1.0; 4], Some(&[rect]));
                }
            }
            ctx.ClearDepthStencilView(target.dsv, (D3D11_CLEAR_DEPTH.0 | D3D11_CLEAR_STENCIL.0) as u32, 0.0, 0);
            match target.cov {
                Some(cov) => ctx.OMSetRenderTargets(Some(&[Some(target.rtv.clone()), Some(cov.clone())]), target.dsv),
                None => ctx.OMSetRenderTargets(Some(&[Some(target.rtv.clone())]), target.dsv),
            }
            ctx.RSSetViewports(Some(&[d3d_vp]));
        }
        self.cov_view = target.cov.is_some();
        if let Some(sky) = &view.sky {
            self.draw_sky(sc, sky, view, &setup, filter)?;
        }

        // the world
        if backing {
            cb.misc[2] |= V_BACKING;
        }
        unsafe {
            let ctx = &self.ctx;
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&self.cb_view, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).map_err(err)?;
            std::ptr::copy_nonoverlapping(&cb as *const ViewCb as *const u8, m.pData as *mut u8, std::mem::size_of::<ViewCb>());
            ctx.Unmap(&self.cb_view, 0);
        }
        self.bind_world(sc);
        let mirror = view.mirror;
        let mut cur_batch = u32::MAX;
        let mut cur_geo: Option<bool> = None;
        let mut cur_depth: Option<(usize, u32)> = None;
        let mut cur_cb: Option<usize> = None;
        let mut stats = DrawStats { chunks_visible: self.last.chunks_visible, instances_visible: self.last.instances_visible, ..Default::default() };
        // the backing tests run before the opaque statics of pass 1 and of pass 2, and the
        // moving objects' (test 3) after every opaque static, before the opaque moving objects
        let dyn_test = backing && self.backing.objects() > 0 && !dyn_draws.is_empty();
        let mut next_test = if backing && self.backing.tested() > 0 {
            1
        } else if dyn_test {
            3
        } else {
            4
        };
        let tests_due = |rank: u8, next: u32| next <= 3 && rank >= RANK_OPAQUE + 2 * next as u8;
        for it in &items {
            let rank = it.rank(sc.batches[it.batch as usize].pass, backing);
            while tests_due(rank, next_test) {
                self.run_test(sc, next_test, &dyn_draws, target, d3d_vp)?;
                next_test += 1;
                if next_test == 3 && !dyn_test {
                    next_test = 4;
                }
                self.bind_world(sc);
                (cur_batch, cur_geo, cur_depth, cur_cb) = (u32::MAX, None, None, None);
            }
            if it.batch != cur_batch {
                let key = sc.batches[it.batch as usize];
                let blend = self.blend(key.blend)?;
                let raster = self.raster(key.cull, mirror)?;
                let linear = match filter {
                    Filter::Nearest => false,
                    Filter::Bilinear => true,
                    Filter::Game => key.bilinear,
                };
                let samp = self.sampler(key.wrap.0, key.wrap.1, linear)?;
                let srv = if key.array == u16::MAX { self.dummy_array.clone() } else { sc.arrays[key.array as usize].clone() };
                unsafe {
                    self.ctx.OMSetBlendState(&blend, None, 0xffff_ffff);
                    self.ctx.RSSetState(&raster);
                    self.ctx.PSSetSamplers(0, Some(&[Some(samp)]));
                    self.ctx.PSSetShaderResources(2, Some(&[Some(srv)]));
                }
                cur_batch = it.batch;
            }
            // depth and stencil (ST_*): one or two draws, each (state, stencil reference,
            // per-draw constants)
            let pass = sc.batches[it.batch as usize].pass;
            // (moving objects' pixels carry their place in the game's order in the low bits)
            let place = it.place as u32 & ST_ORDER;
            let class = if it.dynamic {
                ST_DYN | if it.reflect { ST_WINDOW } else { 0 } | place
            } else if sc.batches[it.batch as usize].water {
                ST_WATER
            } else {
                0
            };
            let draws: [Option<(&ID3D11DepthStencilState, u32, usize)>; 3] = match pass {
                Pass::Sky => [Some((&self.depth_off, 0, DRAW_DEFAULT)), None, None],
                Pass::Opaque if it.spass > 0 => [Some((&self.depth_write_class, class, DRAW_PASS + it.spass as usize - 1)), None, None],
                Pass::Opaque => [Some((&self.depth_write_class, class, DRAW_DEFAULT)), None, None],
                // with a place in the game's order (the puffs): pulled over statics, over the
                // moving objects of that place or farther whatever the depth, not over nearer
                // ones (`ST_ORDER`)
                Pass::Decal if it.dynamic && it.place > 0 => [
                    Some((&self.test_class_dyn, 0, DRAW_DEFAULT)),
                    (it.place as u32 <= ST_ORDER).then_some((&self.test_order_far, ST_DYN | it.place as u32, DRAW_NO_PULL)),
                    None,
                ],
                // pulled over statics, as they are over moving objects
                Pass::Decal => [Some((&self.test_class_dyn, 0, DRAW_DEFAULT)), Some((&self.test_class_dyn, ST_DYN, DRAW_NO_PULL)), None],
                // shown on the water: over the water whatever the depth, nowhere else
                Pass::Reflection => [Some((&self.over_water, ST_WATER, DRAW_NO_PULL)), None, None],
                // moving instances' glass and overlays: over their windows, elsewhere; then
                // their opaque texels' depth and class (no colour)
                Pass::Blended if it.dynamic => [
                    Some((&self.test_class_window, ST_DYN | ST_WINDOW, DRAW_WINDOW)),
                    Some((&self.test_not_window, ST_DYN | ST_WINDOW, DRAW_OVERLAY)),
                    Some((&self.depth_write_class, ST_DYN | place, DRAW_OVERLAY_DEPTH)),
                ],
                Pass::Blended => [Some((&self.depth_test, 0, DRAW_DEFAULT)), None, None],
            };
            if cur_geo != Some(it.frame_geo) {
                let (vb, ib, nrm) = if it.frame_geo {
                    (self.frame_vb.buf.clone(), self.frame_ib.buf.clone(), self.frame_normals.srv.clone())
                } else {
                    (Some(sc.vb.clone()), Some(sc.ib.clone()), Some(sc.normal_srv.clone()))
                };
                let bufs = [vb, self.inst.buf.clone()];
                let strides = [std::mem::size_of::<GpuVertex>() as u32, 4];
                let offsets = [0u32, 0];
                unsafe {
                    self.ctx.IASetVertexBuffers(0, 2, Some(bufs.as_ptr()), Some(strides.as_ptr()), Some(offsets.as_ptr()));
                    self.ctx.IASetIndexBuffer(ib.as_ref(), DXGI_FORMAT_R32_UINT, 0);
                    self.ctx.VSSetShaderResources(5, Some(&[nrm]));
                }
                cur_geo = Some(it.frame_geo);
            }
            for (state, stencil_ref, cb) in draws.into_iter().flatten() {
                let key = (state.as_raw() as usize, stencil_ref, cb);
                unsafe {
                    if cur_depth != Some((key.0, key.1)) {
                        self.ctx.OMSetDepthStencilState(state, stencil_ref);
                        cur_depth = Some((key.0, key.1));
                    }
                    if cur_cb != Some(cb) {
                        let b = [Some(self.cb_draw[cb].clone())];
                        self.ctx.VSSetConstantBuffers(2, Some(&b));
                        self.ctx.PSSetConstantBuffers(2, Some(&b));
                        cur_cb = Some(cb);
                    }
                    if cb == DRAW_OVERLAY_DEPTH {
                        // the batch's blend state is set again for the next item
                        self.ctx.OMSetBlendState(&self.blend_no_write, None, 0xffff_ffff);
                        cur_batch = u32::MAX;
                    }
                    self.ctx.DrawIndexedInstanced(it.count, it.inst_count, it.start, 0, it.inst_start);
                }
                stats.draws += 1;
                stats.triangles += (it.count / 3) as u64 * it.inst_count as u64;
            }
        }
        // tests not run yet (no opaque statics or moving objects after them): now, for their
        // results
        while next_test <= 3 {
            self.run_test(sc, next_test, &dyn_draws, target, d3d_vp)?;
            next_test += if next_test == 2 && !dyn_test { 2 } else { 1 };
        }
        if backing || self.backing.objects() > 0 {
            self.backing.end_view(&self.ctx, self.frame.backing_tag, index as u32)?;
        }
        self.last = stats;
        self.items = items;
        self.visible_chunks = vis;
        self.dyn_draws = dyn_draws;
        Ok(())
    }

    /// Backing test `level` (1, 2: statics; 3: moving objects), then the view's render
    /// targets again (with the coverage target at a render scale).
    fn run_test(&mut self, sc: &GpuScene, level: u32, dyn_draws: &[super::backing::DynDraw], target: &Target, vp: D3D11_VIEWPORT) -> Result<(), String> {
        if level == 3 {
            // the scene's geometry with the instance buffer, the world's constants and
            // resources (the test's shaders read the transforms)
            self.bind_world(sc);
            let bufs = [Some(sc.vb.clone()), self.inst.buf.clone()];
            let strides = [std::mem::size_of::<GpuVertex>() as u32, 4];
            let offsets = [0u32, 0];
            unsafe {
                self.ctx.IASetVertexBuffers(0, 2, Some(bufs.as_ptr()), Some(strides.as_ptr()), Some(offsets.as_ptr()));
                self.ctx.IASetIndexBuffer(&sc.ib, DXGI_FORMAT_R32_UINT, 0);
            }
            self.backing.run_dynamics(&self.ctx, dyn_draws, target.rtv, target.dsv, vp)?;
        } else {
            self.backing.run(&self.ctx, level, target.rtv, target.dsv, vp)?;
        }
        if let Some(cov) = target.cov {
            unsafe { self.ctx.OMSetRenderTargets(Some(&[Some(target.rtv.clone()), Some(cov.clone())]), target.dsv) };
        }
        Ok(())
    }

    /// Bind the world's pipeline (shaders, constants, the per-frame buffers).
    fn bind_world(&self, sc: &GpuScene) {
        unsafe {
            let ctx = &self.ctx;
            ctx.IASetInputLayout(&self.layout);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.GSSetShader(None, None);
            ctx.PSSetShader(if self.cov_view { &self.ps_cov } else { &self.ps }, None);
            let cbs = [Some(self.cb_view.clone())];
            ctx.VSSetConstantBuffers(0, Some(&cbs));
            ctx.PSSetConstantBuffers(0, Some(&cbs));
            ctx.VSSetShaderResources(
                0,
                Some(&[
                    self.xforms.srv.clone(),
                    Some(sc.mat_srv.clone()),
                    None,
                    Some(sc.static_srv.clone()),
                    self.fades.srv.clone(),
                    Some(sc.normal_srv.clone()),
                    Some(sc.frame_srv.clone()),
                    if self.frame.backing { self.backing_buf.srv.clone() } else { None },
                ]),
            );
            ctx.PSSetShaderResources(1, Some(&[Some(sc.mat_srv.clone())]));
        }
    }

    /// The backing test's results finished since the last call (`backing.rs`), oldest first;
    /// `wait`: block until every pending one is done.
    pub fn backing_results(&mut self, wait: bool) -> Vec<BackingResults> {
        self.backing.results(&self.ctx, wait)
    }

    /// The game's sky and ground (`View::sky`): screen-aligned quads over the view's
    /// viewport (already set), no depth test or write, before everything else.
    fn draw_sky(&mut self, sc: &GpuScene, sky: &Sky, view: &View, setup: &prepare::ViewSetup, filter: Filter) -> Result<(), String> {
        let q = prepare::sky_quads(sky, setup.viewport, view.viewport);
        let vp = setup.viewport;
        let ndc = |r: [f32; 4]| {
            let x = |v: f32| 2.0 * (v - vp[0]) / (vp[2] - vp[0]).max(1e-3) - 1.0;
            let y = |v: f32| 1.0 - 2.0 * (v - vp[1]) / (vp[3] - vp[1]).max(1e-3);
            [x(r[0]), y(r[1]), x(r[2]), y(r[3])]
        };
        let layer = sc.sky_layers.get(&sky.texture).copied();
        let mut quads: Vec<ScreenCb> = Vec::with_capacity(2);
        if let (Some((rect, st)), Some((_, l))) = (q.sky, layer) {
            quads.push(ScreenCb { rect: ndc(rect), st: [st[0] / 256.0, st[1] / 256.0, st[2] / 256.0, st[3] / 256.0], color: [1.0; 4], mode: [1, l, 0, 0] });
        }
        if let (Some(rect), Some(g)) = (q.ground, sky.ground) {
            let c = [((g >> 16) & 0xff) as f32 / 255.0, ((g >> 8) & 0xff) as f32 / 255.0, (g & 0xff) as f32 / 255.0, 1.0];
            quads.push(ScreenCb { rect: ndc(rect), st: [0.0; 4], color: c, mode: [0; 4] });
        }
        if quads.is_empty() {
            return Ok(());
        }
        let blend = self.blend(None)?;
        let raster = self.raster(0, false)?;
        // the game draws its sky wrapped and bilinear
        let samp = self.sampler(0, 0, !matches!(filter, Filter::Nearest))?;
        let srv = layer.map_or(self.dummy_array.clone(), |(a, _)| sc.arrays[a as usize].clone());
        unsafe {
            let ctx = &self.ctx;
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&self.vs_screen, None);
            ctx.GSSetShader(None, None);
            ctx.PSSetShader(if self.cov_view { &self.ps_screen_cov } else { &self.ps_screen }, None);
            let cbs = [Some(self.cb_screen.clone())];
            ctx.VSSetConstantBuffers(1, Some(&cbs));
            ctx.PSSetConstantBuffers(1, Some(&cbs));
            ctx.OMSetBlendState(&blend, None, 0xffff_ffff);
            ctx.OMSetDepthStencilState(&self.depth_off, 0);
            ctx.RSSetState(&raster);
            ctx.PSSetSamplers(0, Some(&[Some(samp)]));
            ctx.PSSetShaderResources(2, Some(&[Some(srv)]));
            for qd in &quads {
                let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(&self.cb_screen, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).map_err(err)?;
                std::ptr::copy_nonoverlapping(qd as *const ScreenCb as *const u8, m.pData as *mut u8, std::mem::size_of::<ScreenCb>());
                ctx.Unmap(&self.cb_screen, 0);
                ctx.Draw(4, 0);
            }
        }
        Ok(())
    }

    /// Unbind what the native pipeline left bound that the Glide path does not set itself.
    pub fn unbind(&self) {
        unsafe {
            self.ctx.VSSetShaderResources(0, Some(&[None, None, None, None, None, None, None, None]));
            self.ctx.VSSetConstantBuffers(3, Some(&[None]));
            self.ctx.PSSetShaderResources(1, Some(&[None, None]));
            let none: Option<ID3D11Buffer> = None;
            let zero = 0u32;
            self.ctx.IASetVertexBuffers(1, 1, Some(&none as *const _), Some(&zero as *const _), Some(&zero as *const _));
            self.ctx.IASetIndexBuffer(None, DXGI_FORMAT_R32_UINT, 0);
            self.ctx.VSSetConstantBuffers(1, Some(&[None]));
            self.ctx.PSSetConstantBuffers(1, Some(&[None]));
        }
    }

    fn blend(&mut self, key: Option<(u8, u8)>) -> Result<ID3D11BlendState, String> {
        if let Some(b) = self.blends.get(&key) {
            return Ok(b.clone());
        }
        let f = |x: BlendFactor| match x {
            BlendFactor::Zero => D3D11_BLEND_ZERO,
            BlendFactor::One => D3D11_BLEND_ONE,
            BlendFactor::SrcAlpha => D3D11_BLEND_SRC_ALPHA,
            BlendFactor::InvSrcAlpha => D3D11_BLEND_INV_SRC_ALPHA,
            BlendFactor::SrcColor => D3D11_BLEND_SRC_COLOR,
            BlendFactor::InvSrcColor => D3D11_BLEND_INV_SRC_COLOR,
            BlendFactor::DstColor => D3D11_BLEND_DEST_COLOR,
            BlendFactor::InvDstColor => D3D11_BLEND_INV_DEST_COLOR,
            BlendFactor::DstAlpha => D3D11_BLEND_DEST_ALPHA,
            BlendFactor::InvDstAlpha => D3D11_BLEND_INV_DEST_ALPHA,
        };
        let mut desc = D3D11_BLEND_DESC::default();
        desc.RenderTarget[0] = match key {
            None => D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: false.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_ZERO,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_ZERO,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            },
            Some(code) => {
                let b = blend_of(code);
                // colour: any pair, e.g. (DstColor, SrcColor) = 2 x src x dst (mode 7's
                // multiply variant), (One, SrcColor) = src + src x dst (mode 3); alpha: the
                // game's (ONE, ZERO) in every mode (`polygons.md`, mode table)
                D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: true.into(),
                    SrcBlend: f(b.src),
                    DestBlend: f(b.dst),
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ONE,
                    DestBlendAlpha: D3D11_BLEND_ZERO,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
                }
            }
        };
        let mut b = None;
        unsafe { self.device.CreateBlendState(&desc, Some(&mut b)) }.map_err(err)?;
        let b = b.unwrap();
        self.blends.insert(key, b.clone());
        Ok(b)
    }

    /// Rasterizer for a material's culling; a mirrored view swaps the winding.
    fn raster(&mut self, cull: u8, mirror: bool) -> Result<ID3D11RasterizerState, String> {
        let key = (cull, mirror);
        if let Some(r) = self.rasters.get(&key) {
            return Ok(r.clone());
        }
        // Cull::None = 0, Clockwise = 1, CounterClockwise = 2 (removed on-screen winding)
        let removed = match (cull, mirror) {
            (0, _) => 0,
            (c, false) => c,
            (1, true) => 2,
            (_, true) => 1,
        };
        let rd = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: if removed == 0 { D3D11_CULL_NONE } else { D3D11_CULL_BACK },
            // back = the removed winding: clockwise removed -> counter-clockwise is front
            FrontCounterClockwise: (removed == 1).into(),
            DepthClipEnable: true.into(),
            ScissorEnable: false.into(),
            ..Default::default()
        };
        let mut r = None;
        unsafe { self.device.CreateRasterizerState(&rd, Some(&mut r)) }.map_err(err)?;
        let r = r.unwrap();
        self.rasters.insert(key, r.clone());
        Ok(r)
    }

    fn sampler(&mut self, wrap_u: u8, wrap_v: u8, linear: bool) -> Result<ID3D11SamplerState, String> {
        let key = (wrap_u, wrap_v, linear);
        if let Some(s) = self.samplers.get(&key) {
            return Ok(s.clone());
        }
        let addr = |c: u8| if c == 1 { D3D11_TEXTURE_ADDRESS_CLAMP } else { D3D11_TEXTURE_ADDRESS_WRAP };
        let desc = D3D11_SAMPLER_DESC {
            Filter: if linear { D3D11_FILTER_MIN_MAG_MIP_LINEAR } else { D3D11_FILTER_MIN_MAG_MIP_POINT },
            AddressU: addr(wrap_u),
            AddressV: addr(wrap_v),
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut s = None;
        unsafe { self.device.CreateSamplerState(&desc, Some(&mut s)) }.map_err(err)?;
        let s = s.unwrap();
        self.samplers.insert(key, s.clone());
        Ok(s)
    }
}

/// An immutable buffer with `data` (structured with `stride` > 0).
pub(crate) fn immutable_buffer(device: &ID3D11Device, data: &[u8], bind: D3D11_BIND_FLAG, stride: u32) -> Result<ID3D11Buffer, String> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: data.len() as u32,
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: if stride > 0 { D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32 } else { 0 },
        StructureByteStride: stride,
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as *const _, SysMemPitch: 0, SysMemSlicePitch: 0 };
    let mut b = None;
    unsafe { device.CreateBuffer(&desc, Some(&init as *const _), Some(&mut b)) }.map_err(err)?;
    b.ok_or_else(|| "no buffer".into())
}

/// The Direct3D viewport of a view whose rectangle in game pixels (widened) is `vp`, in a
/// target showing the game's screen widened by `ext` at `scale` target pixels per game
/// pixel. Direct3D clips to the viewport's whole pixels (its right and bottom edges rounded
/// down), and in f32 a full-width view's right and bottom edges come out a hair short of
/// the target's (1279.9999 and 719.9999 at 1280 x 720 with the widescreen extension, the
/// runtime's rectangle ending at y 479.99994): the last column and row were left undrawn.
/// An end less than a hundredth of a pixel short of a whole pixel is put on it (the length
/// grows by an f32 step or two); everything else keeps its values exactly (the start edges,
/// whose rounding down loses nothing, and the mirror's rectangle at half pixels).
pub(crate) fn viewport_in_target(vp: [f32; 4], ext: f32, (sx, sy): (f32, f32)) -> D3D11_VIEWPORT {
    // (start, length) along one axis
    let axis = |start: f32, len: f32| {
        let end = start + len;
        let whole = end.ceil();
        if !(whole > end && whole - end < 0.01) {
            return (start, len);
        }
        // the smallest length that reaches the whole pixel in f32
        let mut l = whole - start;
        while start + l < whole {
            l = f32::from_bits(l.to_bits() + 1);
        }
        (start, l)
    };
    let (x, w) = axis((vp[0] + ext) * sx, (vp[2] - vp[0]) * sx);
    let (y, h) = axis(vp[1] * sy, (vp[3] - vp[1]) * sy);
    D3D11_VIEWPORT { TopLeftX: x, TopLeftY: y, Width: w, Height: h, MinDepth: 0.0, MaxDepth: 1.0 }
}

/// A texture array of `w` x `h` RGBA8 layers, each with the same number of levels.
fn create_array(device: &ID3D11Device, w: u32, h: u32, layers: &[Vec<(u32, u32, Vec<u32>)>]) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView), String> {
    let levels = layers[0].len() as u32;
    let td = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: levels,
        ArraySize: layers.len() as u32,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        ..Default::default()
    };
    // subresource = level + layer * levels
    let init: Vec<D3D11_SUBRESOURCE_DATA> = layers
        .iter()
        .flat_map(|l| l.iter().map(|(lw, _, px)| D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as *const _, SysMemPitch: lw * 4, SysMemSlicePitch: 0 }))
        .collect();
    let mut t = None;
    unsafe { device.CreateTexture2D(&td, Some(init.as_ptr()), Some(&mut t)) }.map_err(err)?;
    let t = t.unwrap();
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2DArray: D3D11_TEX2D_ARRAY_SRV { MostDetailedMip: 0, MipLevels: levels, FirstArraySlice: 0, ArraySize: layers.len() as u32 } },
    };
    let mut srv = None;
    unsafe { device.CreateShaderResourceView(&t, Some(&desc as *const _), Some(&mut srv)) }.map_err(err)?;
    Ok((t, srv.unwrap()))
}
