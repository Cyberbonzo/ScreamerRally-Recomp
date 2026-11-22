//! Scene description for the native renderer (`docs/04-native-renderer.md`): what the
//! runtime extracts from the game's memory, in neutral form. No guest addresses and no
//! Direct3D types; `std` only. `sr-runtime` fills it, `sr-render` draws it.
//!
//! Where each piece comes from in the game (verified in wave 1, `docs/re/native/`):
//! world objects and models (`world.md`), polygons, materials, texture coordinates,
//! reflection and sky (`polygons.md`), textures, palettes and animation frames
//! (`textures.md`), transforms, cameras and projections (`transforms.md`).
//!
//! Two parts:
//! * [`SceneData`], uploaded once per track or menu scene: textures, materials, meshes and
//!   the static instances (track, scenery);
//! * [`FrameData`], given every drawn frame: the views with their cameras, the moving
//!   instances (cars, wheels) and the geometry built per frame (shadows, smoke, tyre marks).
//!
//! # Units and axes
//!
//! World positions are the game's world units, in `f64` where they are absolute (camera,
//! instance origins), so that the renderer can work camera-relative (no precision loss
//! far from the world's origin). Model-space vertex positions are `f32`.
//!
//! World and model space are the game's own: x left, y up, z back (left-handed, topic D).
//! The renderer does not depend on that: a [`Camera`]'s rotation maps the world into
//! **view space**, which is fixed: x to the right of the screen, y down the screen, z
//! forward (depth, positive in front of the camera). The [`Projection`] then maps view
//! space to the game's screen space, 640 x 480 game pixels (the game's 640 x 400 grid with
//! y scaled by 1.2), exactly like the game: `x_screen = cx + sx * x / z`,
//! `y_screen = cy + sy * y / z`.
//!
//! The runtime fills cameras and projections from the game's camera structures (topic D:
//! the eye is 128 units behind the game's view origin, `pos` is the eye, `near` = 128 in
//! the game's terms) so that every view reproduces the game's own projection
//! (`transforms.md`, "Recipe"); a unit test in the runtime checks this against D's
//! reference projection.
//!
//! # Triangles
//!
//! Meshes are indexed triangle lists. "Clockwise" in [`Cull`] means clockwise as seen on
//! the screen (y down), after projection; with the view space above, a triangle whose
//! corners `a, b, c` follow the right-hand rule around its outward normal appears
//! counter-clockwise on screen when seen from the front.

use std::ops::Range;

/// The game's eye sits this many world units behind its view origin along the view axis
/// (`transforms.md`: the projection divides by `z - 128`, the near plane is view z = 0).
/// A [`Camera`]'s `pos` is the eye, i.e. the game's camera position moved back by this.
pub const EYE_OFFSET: f64 = 128.0;

/// Everything uploaded once per track or menu scene.
#[derive(Clone, Debug, Default)]
pub struct SceneData {
    pub textures: Vec<TextureData>,
    /// Animation frame lists of the materials ([`TexAnim`] indexes ranges of it): texture
    /// indices into `textures`.
    pub texture_frames: Vec<u32>,
    pub materials: Vec<Material>,
    pub meshes: Vec<Mesh>,
    /// Objects that never move (track, scenery), placed once. The renderer may merge them
    /// into spatial chunks at upload, but must still honour [`FrameData::static_fade`] per
    /// instance (visibility is decided by the runtime). They cannot be changed afterwards
    /// (upload again).
    pub statics: Vec<Instance>,
}

/// A texture as the game holds it.
#[derive(Clone, Debug)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub format: TexFormat,
    /// Pixels of the largest level, row by row without padding, in the format's layout
    /// (see [`TexFormat::bytes_per_pixel`]; 16-bit texels little-endian).
    pub pixels: Vec<u8>,
    pub mipmaps: Mipmaps,
}

/// Texel formats (the Glide formats the game can use, plus already expanded RGBA).
/// Everything is expanded to 8-bit RGBA at upload.
#[derive(Clone, Debug)]
pub enum TexFormat {
    /// 8-bit palette index. Palette entries are `0x00RRGGBB` (the alpha byte is ignored:
    /// paletted texels are opaque, as with Glide's `GR_TEXFMT_P_8`).
    Indexed8 { palette: Box<[u32; 256]> },
    /// 16 bits: palette index in the low byte, alpha in the high byte (`GR_TEXFMT_AP_88`).
    IndexedAlpha88 { palette: Box<[u32; 256]> },
    /// 16 bits r5 g6 b5.
    Rgb565,
    /// 16 bits a1 r5 g5 b5.
    Argb1555,
    /// 16 bits a4 r4 g4 b4.
    Argb4444,
    /// 8 bits r3 g3 b2.
    Rgb332,
    /// 8 bits alpha, white colour.
    Alpha8,
    /// 8 bits intensity (grey), opaque.
    Intensity8,
    /// 8 bits: alpha in the high nibble, intensity in the low.
    AlphaIntensity44,
    /// 16 bits: alpha in the high byte, intensity in the low.
    AlphaIntensity88,
    /// 32 bits, bytes r, g, b, a (already expanded).
    Rgba8,
}

impl TexFormat {
    /// Bytes per texel in [`TextureData::pixels`].
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            TexFormat::Indexed8 { .. } | TexFormat::Rgb332 | TexFormat::Alpha8 | TexFormat::Intensity8 | TexFormat::AlphaIntensity44 => 1,
            TexFormat::IndexedAlpha88 { .. } | TexFormat::Rgb565 | TexFormat::Argb1555 | TexFormat::Argb4444 | TexFormat::AlphaIntensity88 => 2,
            TexFormat::Rgba8 => 4,
        }
    }
}

/// Smaller levels of a texture.
#[derive(Clone, Debug, Default)]
pub enum Mipmaps {
    /// One level only (sampled without mipmapping).
    #[default]
    None,
    /// The renderer builds the full chain (box filter, alpha-aware for colour-keyed use).
    Generate,
    /// The game's own smaller levels, largest first, each half the previous size (at least
    /// 1 x 1), in the texture's format.
    Levels(Vec<Vec<u8>>),
}

/// How a polygon is drawn: what the game's polygon mode means for the GPU.
///
/// The draft contract's `Mode` mixed four independent things; they are separate fields
/// here (`texture: None` = flat colour, [`Transparency`], [`TexGen::Reflection`] = the
/// environment map, [`Layer::Decal`]), with constructors for the draft's modes:
/// [`Material::opaque`], [`Material::chroma_key`], [`Material::blended`],
/// [`Material::env_map`], [`Material::decal`], [`Material::flat`], plus [`Material::sky`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// Index into [`SceneData::textures`]; `None` = flat colour (vertex colour only). With
    /// [`Material::anim`], the texture of frame 0.
    pub texture: Option<u32>,
    /// Animated textures and car states (flags, smoke, blurred wheels: `textures.md`).
    pub anim: Option<TexAnim>,
    pub transparency: Transparency,
    pub texgen: TexGen,
    pub layer: Layer,
    pub cull: Cull,
    pub combine: Combine,
    /// Texture addressing in u and v.
    pub wrap: [Wrap; 2],
    /// The game asks for bilinear filtering (used when the filter setting is "game").
    pub bilinear: bool,
}

impl Default for Material {
    fn default() -> Self {
        Material {
            texture: None,
            anim: None,
            transparency: Transparency::Opaque,
            texgen: TexGen::Vertex,
            layer: Layer::World,
            cull: Cull::None,
            combine: Combine::Modulate,
            wrap: [Wrap::Repeat; 2],
            bilinear: false,
        }
    }
}

impl Material {
    /// Opaque textured polygons.
    pub fn opaque(texture: u32) -> Material {
        Material { texture: Some(texture), ..Default::default() }
    }
    /// Textured polygons whose texels of colour `key` (`0xRRGGBB`, compared after the
    /// palette) are holes. Drawn with the opaque polygons (they write depth).
    pub fn chroma_key(texture: u32, key: u32) -> Material {
        Material { texture: Some(texture), transparency: Transparency::ChromaKey { key }, ..Default::default() }
    }
    /// Blended polygons (sorted back to front, no depth written).
    pub fn blended(texture: Option<u32>, blend: Blend) -> Material {
        Material { texture, transparency: Transparency::Blended(blend), ..Default::default() }
    }
    /// Environment-mapped reflection, blended over what is behind (car glass, chrome).
    pub fn env_map(texture: u32, blend: Blend) -> Material {
        Material {
            texture: Some(texture),
            transparency: Transparency::Blended(blend),
            texgen: TexGen::Reflection { scale: [1.0, 1.0], offset: [0.0, 0.0] },
            ..Default::default()
        }
    }
    /// Polygons lying on other surfaces (shadows, tyre marks, road markings): depth tested
    /// as if `pull` world units nearer, no depth written, drawn after the opaque surfaces.
    pub fn decal(texture: Option<u32>, transparency: Transparency, pull: f32) -> Material {
        Material { texture, transparency, layer: Layer::Decal { pull, stand: None }, ..Default::default() }
    }
    /// Flat colour (vertex colours, no texture), opaque.
    pub fn flat() -> Material {
        Material::default()
    }
    /// Sky / background: drawn first, around the camera, behind everything.
    pub fn sky(texture: Option<u32>) -> Material {
        Material { texture, layer: Layer::Sky, ..Default::default() }
    }
    /// The same material with back faces culled (`cull` = the on-screen winding removed).
    pub fn culled(self, cull: Cull) -> Material {
        Material { cull, ..self }
    }
}

/// A material's animation: its frame list is `SceneData::texture_frames[first ..
/// first + count]`. The frame drawn is `min(count - 1, (count * v) >> shift)` (the game's
/// `count * [7bb94] >> 14`, `textures.md`), with `v` and `shift` from [`AnimSource`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TexAnim {
    pub first: u32,
    pub count: u32,
    pub source: AnimSource,
}

/// Where a material's animation value `v` comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimSource {
    /// The instance's [`Instance::phase`] when it is not negative, else the frame's
    /// [`FrameData::clock`]; shift 14 (flags, smoke, car states).
    Phase,
    /// The frame's [`FrameData::env_phase`]; shift 16 (the reflection map, `[7bb98]`).
    Env,
}

/// Whether and how a polygon lets what is behind it show. (The game's mode 3, a colour
/// key and a blend together, cannot be expressed yet; it was never seen in a race.)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transparency {
    Opaque,
    /// Texels whose colour is `key` (`0xRRGGBB`) are not drawn (Glide's chroma key).
    ChromaKey { key: u32 },
    /// Blended with the frame buffer.
    Blended(Blend),
}

/// Frame buffer blending: `result = src * src_factor + dst * dst_factor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Blend {
    pub src: BlendFactor,
    pub dst: BlendFactor,
}

impl Blend {
    /// Classic transparency by the polygon's alpha.
    pub const ALPHA: Blend = Blend { src: BlendFactor::SrcAlpha, dst: BlendFactor::InvSrcAlpha };
    /// Additive (lights, glow), weighted by alpha.
    pub const ADD: Blend = Blend { src: BlendFactor::SrcAlpha, dst: BlendFactor::One };
    /// Darkens what is behind by the polygon's colour (shadows).
    pub const MULTIPLY: Blend = Blend { src: BlendFactor::DstColor, dst: BlendFactor::Zero };
}

/// Blend factors (Glide's `GR_BLEND_*` that have a Direct3D equivalent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    Zero,
    One,
    SrcAlpha,
    InvSrcAlpha,
    SrcColor,
    InvSrcColor,
    DstColor,
    InvDstColor,
    DstAlpha,
    InvDstAlpha,
}

/// Where texture coordinates come from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TexGen {
    /// The vertices' `uv`.
    Vertex,
    /// Reflection (the game's mode 5, car windows; `polygons.md`, verified exactly): per
    /// vertex, in the game's view space (x left, y up, z back), with `n` the vertex normal
    /// ([`Vertex::normal`]) rotated by the model-to-view rotation, `e` the normalised view
    /// position of the vertex and `r = 2 (e . n) n - e`:
    /// `u = round(127 r.x) + 127 - scroll[0]`, `v = round(127 r.z) + 127 - scroll[1]`, in
    /// texels of the 256-unit coordinate range (`uv = (u, v) / 256`, wrapped), with
    /// `scroll` = [`Instance::scroll`]. `scale` and `offset` stay for test scenes:
    /// (1, 1) and (0, 0) for the game's formula. Instances drawn with it need a
    /// rotation-only `basis` (the normal is rotated by it); the mirror view's handling is
    /// in `docs/re/native/renderer.md`.
    Reflection { scale: [f32; 2], offset: [f32; 2] },
}

/// Drawing order and depth behaviour. Decals (`Decal`) are pulled over statics only and
/// drawn in [`FrameData::dynamics`] order; blended dynamics (car glass, overlays) are drawn
/// before the decals with the overlay depth rules of `docs/re/native/dynamics.md`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layer {
    /// Background: drawn first in each view, centred on the camera (instance translation
    /// ignored, rotation kept), no depth test or write, in submission order.
    Sky,
    /// Ordinary surfaces: opaque and chroma-keyed ones write depth; blended ones are
    /// sorted back to front and only tested.
    World,
    /// Lies on other surfaces: tested as if `pull` world units nearer to the camera (for
    /// vertices behind the eye the renderer uses the ratio's limit, `near.md`), no
    /// depth written, drawn after the opaque surfaces in submission order (blended
    /// decals too).
    ///
    /// `stand: Some(h)`: a camera-facing card standing on the ground `h` units under its
    /// instance's origin (the smoke and dust puffs): seen from above that height, its part
    /// below the horizontal plane through it is tested at the depth where the view ray meets
    /// the plane (then pulled by `pull`), so the ground it stands on does not cut it, while
    /// whatever rises above the plane in front of it still hides it. The game draws every
    /// scene instance over the scenery near it (`docs/re/native/dynamics.md`, "Smoke").
    Decal { pull: f32, stand: Option<f32> },
    /// A water surface: drawn as [`Layer::World`], its pixels marked for the reflection
    /// cards ([`Layer::Reflection`]) drawn over it.
    Water,
    /// Shows on the water (the game's fake reflections: reflection cards under the water, and
    /// the submerged feet of flat objects standing in it): drawn after the opaque surfaces in
    /// submission order, only over the pixels where water ([`Layer::Water`]) is the
    /// front-most surface, whatever the depth (anything in front of that water hides it
    /// already); no depth written. The game, drawing without a depth buffer, shows these over
    /// the water by its drawing order (`docs/re/native/water.md`).
    Reflection,
}

/// Back-face culling: which on-screen winding is removed (screen y down).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cull {
    None,
    Clockwise,
    CounterClockwise,
}

/// Texture addressing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wrap {
    Repeat,
    Clamp,
}

/// How the texel and the vertex colour make the pixel (Glide's colour / alpha combine).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Combine {
    /// colour = texel * vertex colour, alpha = texel alpha * vertex alpha.
    Modulate,
    /// colour = texel, alpha = texel alpha * vertex alpha.
    Replace,
}

/// A mesh vertex.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Vertex {
    /// Model-space position (world units).
    pub pos: [f32; 3],
    /// Model-space vertex normal (unit length), for [`TexGen::Reflection`]; zero when the
    /// model has none.
    pub normal: [f32; 3],
    /// Texture coordinates, 0..1 across the texture (beyond: per [`Material::wrap`]); the
    /// game's per-vertex (u, v) / 256.
    pub uv: [f32; 2],
    /// Colour r, g, b, a (255 = full intensity / opaque). The game has no lighting:
    /// white for textured polygons; flat polygons (mode 2) carry their 5:6:5 colour here.
    pub color: [u8; 4],
}

/// Indexed triangles with one material per index range.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    /// Triangle list (three indices per triangle) into `vertices`.
    pub indices: Vec<u32>,
    /// Index ranges (multiples of three) and the material each is drawn with.
    pub ranges: Vec<MeshRange>,
}

/// A part of a mesh's index list drawn with one material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshRange {
    /// Index into [`SceneData::materials`].
    pub material: u32,
    pub indices: Range<u32>,
}

/// Model to world: `world = basis * model + origin`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Row-major 3 x 3: `world[i] = sum_j basis[i * 3 + j] * model[j] + origin[i]`. Any
    /// linear map (rotation, scale, mirror).
    pub basis: [f32; 9],
    /// World position of the model's origin.
    pub origin: [f64; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Transform::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Transform = Transform { basis: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], origin: [0.0; 3] };

    /// A translation.
    pub fn at(origin: [f64; 3]) -> Transform {
        Transform { origin, ..Transform::IDENTITY }
    }

    /// From the draft contract's row-major 3 x 4 matrix (translation in the last column).
    pub fn from_3x4(m: [f32; 12]) -> Transform {
        Transform {
            basis: [m[0], m[1], m[2], m[4], m[5], m[6], m[8], m[9], m[10]],
            origin: [m[3] as f64, m[7] as f64, m[11] as f64],
        }
    }

    /// Apply to a model-space point (f64).
    pub fn apply(&self, p: [f32; 3]) -> [f64; 3] {
        let b = &self.basis;
        let r = |i: usize| b[i * 3] as f64 * p[0] as f64 + b[i * 3 + 1] as f64 * p[1] as f64 + b[i * 3 + 2] as f64 * p[2] as f64 + self.origin[i];
        [r(0), r(1), r(2)]
    }
}

/// A placed mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Instance {
    /// Index into [`SceneData::meshes`].
    pub mesh: u32,
    pub transform: Transform,
    /// The object's animation value as the engine computes it (`[7bb94]`, `textures.md`):
    /// instance +20h >> 2 (arithmetic; its low 16 bits when the tint flags 60000h are
    /// set). Negative (+20h < 0): the frame's [`FrameData::clock`] is used. For
    /// [`AnimSource::Phase`]. (Not the raw +20h: that would animate 4x too fast.)
    pub phase: i32,
    /// Reflection scroll offsets in texels (the game's instance +30h / +38h accumulators,
    /// rounded, & FFh), for [`TexGen::Reflection`]; zero for objects that are not scene
    /// instances.
    pub scroll: [f32; 2],
}

impl Instance {
    pub fn new(mesh: u32, transform: Transform) -> Instance {
        Instance { mesh, transform, ..Default::default() }
    }
}

/// Everything that changes per drawn frame (or per simulation step, interpolated by the
/// runtime).
#[derive(Clone, Debug, Default)]
pub struct FrameData {
    /// The views in drawing order: main, rear-view mirror.
    pub views: Vec<View>,
    /// Per static instance (index as in [`SceneData::statics`]): 0 = not drawn, 255 =
    /// fully drawn, between = fading in or out (screen-door dither, so faded objects stay
    /// in the opaque pass). Empty = every static drawn. The runtime decides visibility:
    /// the designers' list of the camera's cell, plus candidates from the lists of the
    /// track in view drawn only when their base is backed by drawn world (the backing
    /// test, [`FrameData::static_backing`]; `docs/re/native/visibility.md`,
    /// `docs/04-native-renderer.md` "Decisions").
    pub static_fade: Vec<u8>,
    /// The game's animation clock as the engine uses it: `[1473a0] & 3FFFh`, for
    /// [`AnimSource::Phase`].
    pub clock: u32,
    /// The reflection map's animation value, for [`AnimSource::Env`]. In the game it is
    /// per object (`[7bb98]` = instance +3Ch); Rally China's reflection map has a single
    /// frame, so one value per frame (the player's car's +3Ch) is enough until a track
    /// with an animated reflection map shows otherwise.
    pub env_phase: u32,
    /// Moving objects (cars, wheels, moving parts), meshes of the scene. The order matters:
    /// cars, marks, shadows, smoke far to near (as the runtime fills it; decals draw in this
    /// order).
    pub dynamics: Vec<Instance>,
    /// Geometry built this frame (shadows, smoke, tyre marks), with the scene's materials.
    pub meshes: Vec<FrameMesh>,
    /// The backing test (`docs/re/native/visibility.md`), per static instance (index as in
    /// [`SceneData::statics`]): the opaque pass that draws it (`& BACKING_PASS`: 0 = first,
    /// with the world everything is tested against; 1 = after the first test; 2 = after the
    /// second) and whether its base is tested ([`BACKING_TEST1`]: against what the first
    /// pass drew; [`BACKING_TEST2`]: against the first two passes). Empty: every static in
    /// the first pass, nothing tested. The results come back through the backend, tagged
    /// with [`FrameData::backing_tag`] ([`BackingResults`]).
    pub static_backing: Vec<u8>,
    /// Returned with this frame's backing results (the runtime: which view they belong to).
    pub backing_tag: u64,
    /// Per [`FrameData::dynamics`] entry: 0 = not drawn, 255 = fully drawn, between = fading
    /// in or out (the screen-door dither of `static_fade`). Empty = every entry drawn. The
    /// runtime draws a moving object only where the world under it is drawn
    /// (`docs/re/native/visibility.md`, "Moving objects").
    pub dynamic_fade: Vec<u8>,
    /// The backing test of moving objects (the floating detector), per
    /// [`FrameData::dynamics`] entry: the object it belongs to (a car's parts share one) and
    /// that object's base height. Tested against everything static drawn (after the opaque
    /// statics, before the moving objects); results per object in
    /// [`BackingResults::dynamics`]. Empty: nothing tested.
    pub dynamic_backing: Vec<DynamicBacking>,
    /// The game's drawing order among moving objects, per [`FrameData::dynamics`] entry
    /// (`docs/re/native/dynamics.md`, "Smoke"). The game has no depth buffer: it draws the
    /// scene instances far to near by the distance of their positions from the camera
    /// (`7a8c0`), so a puff nearer than a car's position covers the car where they overlap,
    /// and a car nearer than a puff hides it. For an entry of an object with opaque surfaces
    /// (a car): that object's place by distance among those objects, 1 = the nearest
    /// (1..=63). For a decal entry: 0 = the ordinary decal rule; `m` > 0 = drawn over every
    /// object of place `m` or more (farther than the decal) whatever the depth, never over
    /// those nearer (place below `m`), over statics as usual. Other entries 0. Empty: no
    /// order (the ordinary rules everywhere).
    pub dynamic_order: Vec<u8>,
    /// Per [`FrameData::dynamics`] entry, in frames between two simulation steps: a dissolve
    /// between the two steps' pictures of the entry (its [`AnimSource::Phase`] texture frame;
    /// [`Dissolve`]). Empty or [`Dissolve::NONE`]: the frame of [`Instance::phase`] only.
    pub dynamic_dissolve: Vec<Dissolve>,
}

/// [`FrameData::dynamic_dissolve`]: the texels drawn are `(1 - t)` of the picture at the
/// earlier step mixed with `t` of the picture at the later one (alpha weighted), where a
/// picture is the texture frame the step's animation value selects, or nothing (alpha 0)
/// when the object is not there at that step: an object appearing at the later step fades
/// in, one disappearing fades out, and a texture frame that changes at the step dissolves
/// into the next instead of switching at once. At the steps themselves (t = 0 or 1) the
/// picture is the game's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dissolve {
    /// The animation value (as [`Instance::phase`]) at the earlier step; `None`: not there.
    pub from: Option<i32>,
    /// The animation value at the later step; `None`: not there.
    pub to: Option<i32>,
    /// How far the frame lies from the earlier step to the later (0..=1); negative: no
    /// dissolve.
    pub t: f32,
}

impl Dissolve {
    pub const NONE: Dissolve = Dissolve { from: None, to: None, t: -1.0 };

    /// A dissolve is drawn.
    pub fn active(&self) -> bool {
        self.t >= 0.0 && (self.from.is_some() || self.to.is_some())
    }
}

impl Default for Dissolve {
    fn default() -> Self {
        Dissolve::NONE
    }
}

/// [`FrameData::dynamic_backing`]: one entry's object and base.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicBacking {
    /// The object's index in this frame's results (objects are numbered from 0);
    /// [`DynamicBacking::NONE`]: the entry is not tested.
    pub object: u32,
    /// World height (y): the object's surfaces below it are its base (its lowest part).
    pub base_top: f64,
}

impl DynamicBacking {
    pub const NONE: DynamicBacking = DynamicBacking { object: u32::MAX, base_top: 0.0 };
}

impl Default for DynamicBacking {
    fn default() -> Self {
        DynamicBacking::NONE
    }
}

/// [`FrameData::static_backing`]: the pass bits.
pub const BACKING_PASS: u8 = 3;
/// [`FrameData::static_backing`]: test the base against the first pass's depth.
pub const BACKING_TEST1: u8 = 4;
/// [`FrameData::static_backing`]: test the base against the first two passes' depth.
pub const BACKING_TEST2: u8 = 8;

/// What the backing test measured for one static's base (its lowest part, defined by the
/// renderer: `docs/re/native/visibility.md`), in target pixels (fragments: overlapping base
/// triangles count each).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackingCounts {
    /// The base's area inside the view, whatever is in front of it.
    pub area: u32,
    /// Per test (first, second): pixels where the base is not behind what was drawn ...
    pub visible: [u32; 2],
    /// ... and of those, pixels where nothing was drawn (sky, background, void).
    pub sky: [u32; 2],
    /// Which tests ran (bit 0: first, bit 1: second).
    pub tested: u8,
}

/// The backing test of one drawn view, read back from the GPU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackingResults {
    /// [`FrameData::backing_tag`] of the frame, and the view (index in its `views`).
    pub tag: u64,
    pub view: u32,
    /// Per tested static: its index and counts, in index order.
    pub statics: Vec<(u32, BackingCounts)>,
    /// Per tested moving object ([`FrameData::dynamic_backing`]): its index and counts
    /// (`area`, and in `visible[0]` / `sky[0]` against every static drawn; `tested` = 1).
    pub dynamics: Vec<(u32, BackingCounts)>,
}

/// Geometry built for one frame; vertex positions are relative to `origin`.
#[derive(Clone, Debug, Default)]
pub struct FrameMesh {
    pub origin: [f64; 3],
    pub mesh: Mesh,
}

/// One drawing of the world with one camera.
#[derive(Clone, Debug)]
pub struct View {
    pub camera: Camera,
    /// Screen rectangle `[x0, y0, x1, y1]` in game pixels (640 x 480 space). A rectangle
    /// spanning the game's whole width (`x0 <= 0`, `x1 >= 640`) covers the whole widescreen
    /// view (the game's Hor+ extension), like the Glide path's clip windows.
    pub viewport: [f32; 4],
    /// The picture is a mirror image (rear-view mirror): flipped horizontally inside the
    /// viewport; the renderer swaps the culled winding to match.
    pub mirror: bool,
    /// Fill the viewport with this colour (r, g, b, a) before drawing; `None` keeps what
    /// is there (the sky layer usually covers it).
    pub clear: Option<[u8; 4]>,
    pub fog: Option<Fog>,
    /// The game's sky and ground behind the world (`polygons.md`, "Sky and background").
    pub sky: Option<Sky>,
}

/// The game's background: a screen-aligned textured quad (the sky) from the top of the
/// viewport down to the horizon, then a flat ground colour below it. The runtime computes
/// the coordinates from the camera exactly as the game does (`7b218`); a wider viewport
/// extends `s` proportionally.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sky {
    /// Index into [`SceneData::textures`] (256 x 256, wrapped).
    pub texture: u32,
    /// Texture s (texels, 0..256 per repeat) at the left and right edge of the game's
    /// 640-pixel width (the game: s0 and s0 + 159). Drawn as given: the runtime swaps
    /// them for the mirror view as the game does.
    pub s: [f32; 2],
    /// Texture t (texels) at the top of the viewport and at the horizon.
    pub t: [f32; 2],
    /// The horizon's y, game pixels (640 x 480 space).
    pub horizon: f32,
    /// Ground colour below the horizon (`0xRRGGBB`), `None` = no ground quad.
    pub ground: Option<u32>,
}

/// A view's camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// World position.
    pub pos: [f64; 3],
    /// World to view rotation, row-major: row i is view axis i (x right, y down,
    /// z forward) in world coordinates; `view = rot * (world - pos)`.
    pub rot: [f32; 9],
    pub proj: Projection,
}

/// The game's perspective projection, by its own parameters (topic D supplies them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    /// Where the view axis meets the screen, game pixels (640 x 480 space).
    pub center: [f32; 2],
    /// Game pixels per unit of `x / z` and `y / z` (the 640 x 400 grid's y scale times
    /// 1.2 for `scale[1]`).
    pub scale: [f32; 2],
    /// Nearest and farthest view depth drawn (world units). Statics and instances reach
    /// behind the eye (walls and road beside the camera): the renderer must clip them at
    /// `near` with clip-space values that stay finite there (`docs/re/native/near.md`).
    pub near: f32,
    pub far: f32,
}

impl Projection {
    /// Where a view-space point lands on the screen (game pixels), and its depth; `None`
    /// behind the camera. The reference for the GPU path (tests).
    pub fn project(&self, p: [f32; 3]) -> Option<[f32; 3]> {
        (p[2] > 0.0).then(|| [self.center[0] + self.scale[0] * p[0] / p[2], self.center[1] + self.scale[1] * p[1] / p[2], p[2]])
    }
}

/// Distance fog, as Glide's fog table: 64 entries indexed by the view depth w (entry i at
/// `w = 2^(3 + i / 4) / (8 - i % 4)`, from 1 to 52428), 0 = no fog, 255 = fog colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    /// `0xRRGGBB`.
    pub color: u32,
    pub table: [u8; 64],
    /// View depth to the table's w (1 when view depth is the game's w).
    pub w_scale: f32,
}

impl Fog {
    /// The table's w for entry `i` (Glide's `guFogTableIndexToW`).
    pub fn index_to_w(i: usize) -> f32 {
        (2.0f32).powi(3 + (i / 4) as i32) / (8 - (i % 4) as i32) as f32
    }

    /// Linear fog from `start` to `end` (view depth), like `guFogGenerateLinear`.
    pub fn linear(color: u32, start: f32, end: f32) -> Fog {
        let mut table = [0u8; 64];
        for (i, t) in table.iter_mut().enumerate() {
            let w = Fog::index_to_w(i);
            *t = (((w - start) / (end - start)).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        Fog { color, table, w_scale: 1.0 }
    }
}
