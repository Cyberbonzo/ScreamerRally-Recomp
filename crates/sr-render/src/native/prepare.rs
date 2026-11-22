//! CPU side of the upload and of the per-view set-up: texture expansion, mip chains,
//! merging static instances into chunks, flattening meshes, projection and culling math.
//! No Direct3D here (unit-tested).

use super::scene::*;
use std::collections::HashMap;

/// Edge of a static chunk (world units). Static instances are grouped by the chunk (a
/// square column in the world's horizontal x / z plane: world y is up, `sr-scene`) their
/// origin falls in; each chunk's vertices are stored relative to the chunk's centre (small
/// `f32` values anywhere in the world) and get one camera-relative translation per frame.
pub const CHUNK_SIZE: f64 = 8192.0;

/// [`GpuVertex::slot`] of a merged static vertex: this bit plus the static instance's index
/// (the shader finds the chunk's transform, the instance's phase, scroll and fade from it).
pub const STATIC_BIT: u32 = 0x8000_0000;

/// The game's eye lies this far behind its view origin (`sr-scene`, "Units and axes"; topic
/// D): the reflection formula works in the game's view space, whose origin is the view
/// origin, not the eye.
#[cfg(test)] // CPU reference (the shader has the same, `native.hlsl`)
pub const GAME_EYE: f64 = 128.0;

/// Vertex as the GPU reads it (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    /// r | g << 8 | b << 16 | a << 24 (R8G8B8A8_UNORM).
    pub color: u32,
    /// Merged static vertices: [`STATIC_BIT`] | the static instance's index. Others: the
    /// transform slot (0 for meshes drawn as instances: the instance's slot is added).
    pub slot: u32,
    /// Index into the material table.
    pub material: u32,
}

/// A static instance as the shader reads it (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuStatic {
    /// Transform slot of its chunk.
    pub chunk: u32,
    /// [`Instance::phase`] (negative: the frame's clock).
    pub phase: i32,
    /// [`Instance::scroll`].
    pub scroll: [f32; 2],
}

/// RGBA8 texel `r | g << 8 | b << 16 | a << 24`.
fn rgba(r: u32, g: u32, b: u32, a: u32) -> u32 {
    r | g << 8 | b << 16 | a << 24
}
fn expand5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}
fn expand6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}

/// Expand one level (`w` x `h` texels in `format`) to RGBA8, the same bit replication as
/// the Glide path (`sr_runtime::glide::tex`). With `key` (`0xRRGGBB`), texels of that colour
/// get alpha 0 and the colour of their opaque neighbours (no dark fringes under bilinear
/// filtering); every other texel's alpha is 255 (Glide's chroma key ignores texel alpha,
/// and the shader then only tests alpha).
pub fn expand_level(format: &TexFormat, px: &[u8], w: u32, h: u32, key: Option<u32>) -> Vec<u32> {
    let n = (w * h) as usize;
    let b8 = |i: usize| *px.get(i).unwrap_or(&0) as u32;
    let b16 = |i: usize| b8(i * 2) | b8(i * 2 + 1) << 8;
    let pal = |p: &[u32; 256], i: u32| {
        let c = p[(i & 0xff) as usize];
        rgba((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff, 255)
    };
    let mut out: Vec<u32> = (0..n)
        .map(|i| match format {
            TexFormat::Indexed8 { palette } => pal(palette, b8(i)),
            TexFormat::IndexedAlpha88 { palette } => {
                let v = b16(i);
                (pal(palette, v) & 0x00ff_ffff) | (v >> 8) << 24
            }
            TexFormat::Rgb565 => {
                let v = b16(i);
                rgba(expand5(v >> 11), expand6((v >> 5) & 63), expand5(v & 31), 255)
            }
            TexFormat::Argb1555 => {
                let v = b16(i);
                rgba(expand5((v >> 10) & 31), expand5((v >> 5) & 31), expand5(v & 31), if v & 0x8000 != 0 { 255 } else { 0 })
            }
            TexFormat::Argb4444 => {
                let v = b16(i);
                rgba(((v >> 8) & 15) * 17, ((v >> 4) & 15) * 17, (v & 15) * 17, (v >> 12) * 17)
            }
            TexFormat::Rgb332 => {
                let v = b8(i);
                rgba(((v >> 5) & 7) * 255 / 7, ((v >> 2) & 7) * 255 / 7, (v & 3) * 255 / 3, 255)
            }
            TexFormat::Alpha8 => rgba(255, 255, 255, b8(i)),
            TexFormat::Intensity8 => {
                let l = b8(i);
                rgba(l, l, l, 255)
            }
            TexFormat::AlphaIntensity44 => {
                let v = b8(i);
                let (a, l) = ((v >> 4) * 17, (v & 15) * 17);
                rgba(l, l, l, a)
            }
            TexFormat::AlphaIntensity88 => {
                let v = b16(i);
                let l = v & 0xff;
                rgba(l, l, l, v >> 8)
            }
            TexFormat::Rgba8 => b8(i * 4) | b8(i * 4 + 1) << 8 | b8(i * 4 + 2) << 16 | b8(i * 4 + 3) << 24,
        })
        .collect();
    if let Some(key) = key {
        apply_chroma_key(&mut out, w, h, key);
    }
    out
}

/// Texels of colour `key` become alpha 0 with the average colour of their opaque
/// 8-neighbours (black if none); all others alpha 255.
fn apply_chroma_key(px: &mut [u32], w: u32, h: u32, key: u32) {
    let key_rgb = ((key >> 16) & 0xff) | (key & 0xff00) | (key & 0xff) << 16;
    let keyed: Vec<bool> = px.iter().map(|&c| c & 0x00ff_ffff == key_rgb).collect();
    let src = px.to_vec();
    let (w, h) = (w as i32, h as i32);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if !keyed[i] {
                px[i] = src[i] | 0xff00_0000;
                continue;
            }
            let (mut r, mut g, mut b, mut k) = (0u32, 0u32, 0u32, 0u32);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    // neighbours wrap like a repeating texture
                    let j = (((y + dy).rem_euclid(h)) * w + (x + dx).rem_euclid(w)) as usize;
                    if !keyed[j] {
                        r += src[j] & 0xff;
                        g += (src[j] >> 8) & 0xff;
                        b += (src[j] >> 16) & 0xff;
                        k += 1;
                    }
                }
            }
            px[i] = if k > 0 { rgba(r / k, g / k, b / k, 0) } else { 0 };
        }
    }
}

/// Number of levels in a full chain for `w` x `h`.
pub fn full_chain(w: u32, h: u32) -> u32 {
    32 - w.max(h).max(1).leading_zeros()
}

/// Halve an RGBA8 level (2 x 2 box filter; colours weighted by alpha so that holes of a
/// colour-keyed texture do not darken its edges).
pub fn halve(px: &[u32], w: u32, h: u32) -> (Vec<u32>, u32, u32) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = Vec::with_capacity((nw * nh) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            let mut wsum = 0u32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let sx = (x * 2 + dx).min(w - 1);
                let sy = (y * 2 + dy).min(h - 1);
                let c = px[(sy * w + sx) as usize];
                let a = c >> 24;
                acc[0] += (c & 0xff) * a;
                acc[1] += ((c >> 8) & 0xff) * a;
                acc[2] += ((c >> 16) & 0xff) * a;
                acc[3] += a;
                wsum += a;
            }
            out.push(if wsum == 0 {
                // all transparent: keep the plain average colour, alpha 0
                let c: Vec<u32> = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().map(|&(dx, dy)| px[((y * 2 + dy).min(h - 1) * w + (x * 2 + dx).min(w - 1)) as usize]).collect();
                let avg = |s: u32| c.iter().map(|v| (v >> s) & 0xff).sum::<u32>() / 4;
                rgba(avg(0), avg(8), avg(16), 0)
            } else {
                rgba(acc[0] / wsum, acc[1] / wsum, acc[2] / wsum, (acc[3] + 2) / 4)
            });
        }
    }
    (out, nw, nh)
}

/// How many levels [`texture_levels`] makes for `t`.
pub fn level_count(t: &TextureData) -> u32 {
    match &t.mipmaps {
        Mipmaps::None => 1,
        Mipmaps::Generate => full_chain(t.width, t.height),
        Mipmaps::Levels(more) => {
            let (mut w, mut h, mut n) = (t.width, t.height, 1);
            for _ in more {
                (w, h) = ((w / 2).max(1), (h / 2).max(1));
                n += 1;
                if w == 1 && h == 1 {
                    break;
                }
            }
            n
        }
    }
}

/// All levels of a texture in RGBA8, largest first (`key`: colour-keyed variant).
pub fn texture_levels(t: &TextureData, key: Option<u32>) -> Vec<(u32, u32, Vec<u32>)> {
    let first = expand_level(&t.format, &t.pixels, t.width, t.height, key);
    let mut levels = vec![(t.width, t.height, first)];
    match &t.mipmaps {
        Mipmaps::None => {}
        Mipmaps::Generate => {
            for _ in 1..full_chain(t.width, t.height) {
                let (w, h, px) = levels.last().unwrap();
                let next = halve(px, *w, *h);
                levels.push((next.1, next.2, next.0));
            }
        }
        Mipmaps::Levels(more) => {
            for data in more {
                let (w, h) = { let l = levels.last().unwrap(); ((l.0 / 2).max(1), (l.1 / 2).max(1)) };
                levels.push((w, h, expand_level(&t.format, data, w, h, key)));
                if w == 1 && h == 1 {
                    break;
                }
            }
        }
    }
    levels
}

/// The levels of `t` in RGBA8 shaped as `w` x `h` with `levels` levels: the animation frames
/// of one material share one texture array, so a frame of another size is resampled
/// (nearest texel; uv are relative to the texture, so the mapping is unchanged) and its
/// chain generated or cut to `levels`. With the texture's own shape this is
/// [`texture_levels`].
pub fn texture_levels_as(t: &TextureData, key: Option<u32>, w: u32, h: u32, levels: u32) -> Vec<(u32, u32, Vec<u32>)> {
    let mut own = texture_levels(t, key);
    let levels = levels.max(1);
    if t.width == w && t.height == h && own.len() as u32 >= levels {
        own.truncate(levels as usize);
        return own;
    }
    let (sw, sh, src) = own.swap_remove(0);
    let first: Vec<u32> = if (sw, sh) == (w, h) {
        src
    } else {
        (0..w * h).map(|i| {
            let (x, y) = (i % w, i / w);
            src[((y as u64 * sh as u64 / h as u64) as u32 * sw + (x as u64 * sw as u64 / w as u64) as u32) as usize]
        }).collect()
    };
    let mut out = vec![(w, h, first)];
    while (out.len() as u32) < levels {
        let (lw, lh, px) = out.last().unwrap();
        let (px, nw, nh) = halve(px, *lw, *lh);
        out.push((nw, nh, px));
    }
    out
}

/// The animation frame drawn: `min(count - 1, (count * v) >> shift)` (`sr-scene`
/// [`TexAnim`]), exactly (64-bit). The reference for the shader's `anim_frame`.
#[cfg(test)] // CPU reference (the shader has the same, `native.hlsl`)
pub fn anim_frame(count: u32, v: u32, shift: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    ((count as u64 * v as u64) >> shift).min(count as u64 - 1) as u32
}

/// The value and shift of a material's animation for an instance (`sr-scene`
/// [`AnimSource`]).
#[cfg(test)] // CPU reference (the shader has the same, `native.hlsl`)
pub fn anim_value(source: AnimSource, phase: i32, clock: u32, env_phase: u32) -> (u32, u32) {
    match source {
        AnimSource::Phase => (if phase >= 0 { phase as u32 } else { clock }, 14),
        AnimSource::Env => (env_phase, 16),
    }
}

/// Reflection texture coordinates in texels (the game's mode 5, `sr-scene`
/// [`TexGen::Reflection`]), the CPU reference for the shader's `reflect_texel`. Inputs are
/// in the renderer's view space (x right, y down, z forward, the eye at the origin): `p`
/// the vertex's position, `n` its normal rotated by the model-to-view rotation (not
/// renormalised, as the game). In the game's view space (x left, y up, z back, origin
/// [`GAME_EYE`] units in front of the eye) the position is `(-x, -y, 128 - z)` (in a mirror
/// view the game negates x after the transform, `7ab44`: `(x, -y, 128 - z)`) and a
/// direction `-n`; then `e = p / |p|`, `r = 2 (e . n) n - e`, `u = round(127 r.x) + 127 -
/// scroll[0]`, `v = round(127 r.z) + 127 - scroll[1]`, rounding half to even (`fistp`).
#[cfg(test)] // CPU reference (the shader has the same, `native.hlsl`)
pub fn reflection_texel(p: [f64; 3], n: [f64; 3], scroll: [f32; 2], mirror: bool) -> [f64; 2] {
    let g = [if mirror { p[0] } else { -p[0] }, -p[1], GAME_EYE - p[2]];
    let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
    let e = [g[0] / l, g[1] / l, g[2] / l];
    let n = [-n[0], -n[1], -n[2]];
    let k = 2.0 * (e[0] * n[0] + e[1] * n[1] + e[2] * n[2]);
    let r = [k * n[0] - e[0], k * n[1] - e[1], k * n[2] - e[2]];
    [(127.0 * r[0]).round_ties_even() + 127.0 - scroll[0] as f64, (127.0 * r[2]).round_ties_even() + 127.0 - scroll[1] as f64]
}

/// The 4 x 4 ordered-dither thresholds of the static fade (screen door).
#[cfg(test)] // CPU reference (the shader has the same, `native.hlsl`)
pub const BAYER4: [[u32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// A pixel of a static faded to `fade` (0..255) at screen position (x, y) is drawn
/// (the shader's test): `16 fade > 255 level + 128` for the pixel's dither level, so 0
/// draws nothing, 255 everything and 128 half of the pixels.
#[cfg(test)] // CPU reference (the shader has the same, `native.hlsl`)
pub fn fade_draws(fade: u32, x: u32, y: u32) -> bool {
    fade * 16 > BAYER4[(y & 3) as usize][(x & 3) as usize] * 255 + 128
}

/// Pack a vertex colour.
pub fn pack_color(c: [u8; 4]) -> u32 {
    c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16 | (c[3] as u32) << 24
}

/// A bounding sphere.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sphere {
    pub center: [f32; 3],
    pub radius: f32,
}

/// Bounding sphere of points (the box's centre and half diagonal).
pub fn bounds<I: Iterator<Item = [f32; 3]>>(points: I) -> Sphere {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    let mut any = false;
    for p in points {
        any = true;
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    if !any {
        return Sphere::default();
    }
    let c = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    let r = ((hi[0] - c[0]).powi(2) + (hi[1] - c[1]).powi(2) + (hi[2] - c[2]).powi(2)).sqrt();
    Sphere { center: c, radius: r }
}

/// A static chunk: origin (world) and bounds relative to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chunk {
    pub origin: [f64; 3],
    pub bounds: Sphere,
}

/// Static geometry of one batch in one chunk: `count` indices from `start`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticRange {
    pub batch: u32,
    pub chunk: u32,
    pub start: u32,
    pub count: u32,
}

/// All static instances merged: vertices relative to their chunk's origin, sorted by
/// (batch, chunk) so that a batch's chunks are contiguous in the index list.
#[derive(Default)]
pub struct StaticGeometry {
    pub vertices: Vec<GpuVertex>,
    /// Per vertex: the normal rotated by its instance's basis (world orientation), for
    /// reflection materials.
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub chunks: Vec<Chunk>,
    pub ranges: Vec<StaticRange>,
    /// Per static instance (as in `SceneData::statics`): its chunk, phase and scroll.
    pub statics: Vec<GpuStatic>,
    /// Per chunk: the static instances in it (for fades: a chunk whose instances are all
    /// faded out is skipped).
    pub chunk_statics: Vec<Vec<u32>>,
}

/// Interleave the bits of a (32-bit) value with a zero bit (Morton order).
fn spread2(v: u64) -> u64 {
    let mut x = v & 0xffff_ffff;
    x = (x | x << 16) & 0x0000_ffff_0000_ffff;
    x = (x | x << 8) & 0x00ff_00ff_00ff_00ff;
    x = (x | x << 4) & 0x0f0f_0f0f_0f0f_0f0f;
    x = (x | x << 2) & 0x3333_3333_3333_3333;
    x = (x | x << 1) & 0x5555_5555_5555_5555;
    x
}

/// `basis * n` for a row-major 3 x 3.
pub fn rotate_normal(b: &[f32; 9], n: [f32; 3]) -> [f32; 3] {
    rotate(b, n)
}

/// Merge the static instances. `batch_of(material)` gives the batch (pipeline state and
/// texture array) a material is drawn in; materials of one batch share index ranges.
pub fn build_statics(scene: &SceneData, batch_of: &dyn Fn(u32) -> u32) -> StaticGeometry {
    // chunk keys in the horizontal plane (world y is up: a 3D key would split every chunk
    // whose objects straddle a horizontal boundary, such as y = 0), numbered in Morton
    // order (chunks seen together are mostly contiguous)
    let key_of = |o: [f64; 3]| [(o[0] / CHUNK_SIZE).floor() as i64, (o[2] / CHUNK_SIZE).floor() as i64];
    let mut keys: Vec<[i64; 2]> = scene.statics.iter().map(|i| key_of(i.transform.origin)).collect();
    keys.sort_by_key(|k| (spread2((k[0] + (1 << 31)) as u64) | spread2((k[1] + (1 << 31)) as u64) << 1, *k));
    keys.dedup();
    let chunk_index: HashMap<[i64; 2], u32> = keys.iter().enumerate().map(|(i, k)| (*k, i as u32)).collect();
    // a chunk's height: the middle of its instances' origins, whole units (exact)
    let mut y_range = vec![(f64::MAX, f64::MIN); keys.len()];
    for inst in &scene.statics {
        let c = chunk_index[&key_of(inst.transform.origin)] as usize;
        let y = inst.transform.origin[1];
        y_range[c] = (y_range[c].0.min(y), y_range[c].1.max(y));
    }
    let origins: Vec<[f64; 3]> = keys.iter().zip(&y_range).map(|(k, y)| [(k[0] as f64 + 0.5) * CHUNK_SIZE, ((y.0 + y.1) * 0.5).round(), (k[1] as f64 + 0.5) * CHUNK_SIZE]).collect();

    // geometry per (batch, chunk)
    let mut buckets: HashMap<(u32, u32), (Vec<GpuVertex>, Vec<[f32; 3]>, Vec<u32>)> = HashMap::new();
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut g = StaticGeometry { chunk_statics: vec![Vec::new(); keys.len()], ..Default::default() };
    for (si, inst) in scene.statics.iter().enumerate() {
        let chunk = chunk_index[&key_of(inst.transform.origin)];
        g.statics.push(GpuStatic { chunk, phase: inst.phase, scroll: inst.scroll });
        g.chunk_statics[chunk as usize].push(si as u32);
        let Some(mesh) = scene.meshes.get(inst.mesh as usize) else { continue };
        let o = origins[chunk as usize];
        for r in &mesh.ranges {
            let batch = batch_of(r.material);
            let (verts, normals, idx) = buckets.entry((batch, chunk)).or_default();
            remap.clear();
            for &i in mesh.indices.get(r.indices.start as usize..r.indices.end as usize).unwrap_or(&[]) {
                let Some(v) = mesh.vertices.get(i as usize) else { continue };
                let n = *remap.entry(i).or_insert_with(|| {
                    let w = inst.transform.apply(v.pos);
                    verts.push(GpuVertex {
                        pos: [(w[0] - o[0]) as f32, (w[1] - o[1]) as f32, (w[2] - o[2]) as f32],
                        uv: v.uv,
                        color: pack_color(v.color),
                        slot: STATIC_BIT | si as u32,
                        material: r.material,
                    });
                    normals.push(rotate_normal(&inst.transform.basis, v.normal));
                    verts.len() as u32 - 1
                });
                idx.push(n);
            }
        }
    }
    let mut order: Vec<(u32, u32)> = buckets.keys().copied().collect();
    order.sort();
    let mut chunk_points: Vec<Vec<[f32; 3]>> = vec![Vec::new(); keys.len()];
    for key in order {
        let (verts, normals, idx) = buckets.remove(&key).unwrap();
        if idx.is_empty() {
            continue;
        }
        let base = g.vertices.len() as u32;
        chunk_points[key.1 as usize].extend(verts.iter().map(|v| v.pos));
        g.ranges.push(StaticRange { batch: key.0, chunk: key.1, start: g.indices.len() as u32, count: idx.len() as u32 });
        g.indices.extend(idx.iter().map(|i| i + base));
        g.vertices.extend(verts);
        g.normals.extend(normals);
    }
    g.chunks = origins.iter().zip(&chunk_points).map(|(o, pts)| Chunk { origin: *o, bounds: bounds(pts.iter().copied()) }).collect();
    g
}

/// The backing test's base of a static (`docs/re/native/visibility.md`): its depth-writing
/// geometry below `lowest + max(BASE_FRACTION * height, BASE_MIN)` in world height. A fifth
/// of the height is the part that stands on something (a tree's trunk foot, a tower's
/// ground floor, a road section's lower end on a slope); flat objects (road, terrain on
/// level ground) are all base.
pub const BASE_FRACTION: f64 = 0.2;
pub const BASE_MIN: f64 = 100.0;

/// A vertex of the bases: chunk-relative position (like the merged statics') and the static
/// instance's index.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BaseVertex {
    pub pos: [f32; 3],
    pub slot: u32,
}

/// The bases of all statics as a triangle list (three vertices per triangle). `writes_depth`
/// tells the materials whose polygons write depth (opaque and colour-keyed world surfaces:
/// what can stand in front of the world); only those make a base. Triangles are clipped at
/// the base's height (the part below it kept).
pub fn build_bases(scene: &SceneData, g: &StaticGeometry, writes_depth: &dyn Fn(u32) -> bool) -> Vec<BaseVertex> {
    let mut out = Vec::new();
    let mut tris: Vec<[[f64; 3]; 3]> = Vec::new();
    for (si, inst) in scene.statics.iter().enumerate() {
        let Some(mesh) = scene.meshes.get(inst.mesh as usize) else { continue };
        let Some(gs) = g.statics.get(si) else { continue };
        let Some(chunk) = g.chunks.get(gs.chunk as usize) else { continue };
        tris.clear();
        for r in mesh.ranges.iter().filter(|r| writes_depth(r.material)) {
            for t in mesh.indices.get(r.indices.start as usize..r.indices.end as usize).unwrap_or(&[]).chunks_exact(3) {
                let p = |i: u32| mesh.vertices.get(i as usize).map(|v| inst.transform.apply(v.pos));
                if let (Some(a), Some(b), Some(c)) = (p(t[0]), p(t[1]), p(t[2])) {
                    tris.push([a, b, c]);
                }
            }
        }
        let (lo, hi) = tris.iter().flatten().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p[1]), hi.max(p[1])));
        if lo > hi {
            continue;
        }
        let top = lo + (BASE_FRACTION * (hi - lo)).max(BASE_MIN);
        let o = chunk.origin;
        for t in &tris {
            let poly = clip_below(t, top);
            for k in 1..poly.len().saturating_sub(1) {
                for p in [poly[0], poly[k], poly[k + 1]] {
                    out.push(BaseVertex { pos: [(p[0] - o[0]) as f32, (p[1] - o[1]) as f32, (p[2] - o[2]) as f32], slot: si as u32 });
                }
            }
        }
    }
    out
}

/// The part of a triangle at or below height `top` (world y), as a convex polygon (0, 3 or
/// 4 corners, in the triangle's order).
pub fn clip_below(t: &[[f64; 3]; 3], top: f64) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(4);
    for i in 0..3 {
        let (a, b) = (t[i], t[(i + 1) % 3]);
        let (ia, ib) = (a[1] <= top, b[1] <= top);
        if ia {
            out.push(a);
        }
        if ia != ib {
            let s = (top - a[1]) / (b[1] - a[1]);
            out.push([a[0] + (b[0] - a[0]) * s, top, a[2] + (b[2] - a[2]) * s]);
        }
    }
    out
}

/// One material range of a flattened mesh: `count` indices from `start`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatRange {
    pub material: u32,
    pub start: u32,
    pub count: u32,
}

/// Append a mesh's ranges to `verts` / `idx` (and the normals to `normals`, kept parallel
/// to `verts`) with the material (and `slot`) in every vertex (vertices shared by ranges of
/// different materials are duplicated); indices absolute.
pub fn flatten_mesh(mesh: &Mesh, slot: u32, verts: &mut Vec<GpuVertex>, normals: &mut Vec<[f32; 3]>, idx: &mut Vec<u32>) -> Vec<FlatRange> {
    let mut out = Vec::with_capacity(mesh.ranges.len());
    let mut remap: HashMap<u32, u32> = HashMap::new();
    for r in &mesh.ranges {
        remap.clear();
        let start = idx.len() as u32;
        for &i in mesh.indices.get(r.indices.start as usize..r.indices.end as usize).unwrap_or(&[]) {
            let Some(v) = mesh.vertices.get(i as usize) else { continue };
            let n = *remap.entry(i).or_insert_with(|| {
                verts.push(GpuVertex { pos: v.pos, uv: v.uv, color: pack_color(v.color), slot, material: r.material });
                normals.push(v.normal);
                verts.len() as u32 - 1
            });
            idx.push(n);
        }
        let count = idx.len() as u32 - start;
        if count > 0 {
            out.push(FlatRange { material: r.material, start, count });
        }
    }
    out
}

/// A view's set-up for the GPU: the rotation, projection rows and the culling planes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewSetup {
    /// The viewport in game pixels after widening (`[x0, y0, x1, y1]`).
    pub viewport: [f32; 4],
    /// `clip_x = px[0] * x + px[1] * z`, `clip_y = py[0] * y + py[1] * z` (view space);
    /// `clip_w = z`.
    pub px: [f32; 2],
    pub py: [f32; 2],
    /// Frustum planes in view space (`n . p + d >= 0` inside): left, right, top, bottom,
    /// near, far.
    pub planes: [[f32; 4]; 6],
}

/// The viewport in game pixels with the widescreen extension `ext` (game pixels on each
/// side): a full-width rectangle covers the whole wide view.
pub fn widen(viewport: [f32; 4], ext: f32) -> [f32; 4] {
    if viewport[0] <= 0.0 && viewport[2] >= 640.0 { [-ext, viewport[1], 640.0 + ext, viewport[3]] } else { viewport }
}

pub fn view_setup(view: &View, ext: f32) -> ViewSetup {
    let vp = widen(view.viewport, ext);
    let p = &view.camera.proj;
    let (vw, vh) = ((vp[2] - vp[0]).max(1e-3), (vp[3] - vp[1]).max(1e-3));
    let mut px = [2.0 * p.scale[0] / vw, 2.0 * (p.center[0] - vp[0]) / vw - 1.0];
    let py = [-2.0 * p.scale[1] / vh, 1.0 - 2.0 * (p.center[1] - vp[1]) / vh];
    if view.mirror {
        px = [-px[0], -px[1]];
    }
    // -z <= clip <= z for x and y: (a, 0, b + 1) and (-a, 0, 1 - b) . (x, y, z) >= 0
    let norm = |v: [f32; 4]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
        [v[0] / l, v[1] / l, v[2] / l, v[3] / l]
    };
    let planes = [
        norm([px[0], 0.0, px[1] + 1.0, 0.0]),
        norm([-px[0], 0.0, 1.0 - px[1], 0.0]),
        norm([0.0, py[0], py[1] + 1.0, 0.0]),
        norm([0.0, -py[0], 1.0 - py[1], 0.0]),
        [0.0, 0.0, 1.0, -p.near],
        [0.0, 0.0, -1.0, p.far],
    ];
    ViewSetup { viewport: vp, px, py, planes }
}

/// Normalised device coordinates of a view-space point (reference for the shader).
#[cfg(test)]
pub fn to_ndc(s: &ViewSetup, p: [f32; 3]) -> [f32; 2] {
    [(s.px[0] * p[0] + s.px[1] * p[2]) / p[2], (s.py[0] * p[1] + s.py[1] * p[2]) / p[2]]
}

/// The game's sky in one view (`sr-scene` [`Sky`]), as screen rectangles in game pixels
/// with texture coordinates in texels: the sky quad from the top of the viewport to the
/// horizon, the ground below. `vp` is the widened viewport, `nominal` the view's own
/// (`View::viewport`): `s` is given at the nominal left and right edges and extends
/// proportionally over a wider one; it is drawn as given (not flipped in a mirror view:
/// the game's pair is already swapped there).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyQuads {
    /// `[x0, y0, x1, y1]` and texels `[s0, t0, s1, t1]` at those corners; `None` when the
    /// horizon is at or above the top.
    pub sky: Option<([f32; 4], [f32; 4])>,
    /// `[x0, y0, x1, y1]`; `None` without a ground colour or with the horizon at or below
    /// the bottom.
    pub ground: Option<[f32; 4]>,
}

pub fn sky_quads(sky: &Sky, vp: [f32; 4], nominal: [f32; 4]) -> SkyQuads {
    let w = (nominal[2] - nominal[0]).max(1e-3);
    let s_at = |x: f32| sky.s[0] + (sky.s[1] - sky.s[0]) * (x - nominal[0]) / w;
    let top = vp[1];
    let bottom = vp[3];
    let quad = (sky.horizon > top).then(|| ([vp[0], top, vp[2], sky.horizon], [s_at(vp[0]), sky.t[0], s_at(vp[2]), sky.t[1]]));
    let ground = (sky.ground.is_some() && sky.horizon < bottom).then(|| [vp[0], sky.horizon.max(top), vp[2], bottom]);
    SkyQuads { sky: quad, ground }
}

/// The sphere (view space) touches the frustum.
pub fn sphere_visible(planes: &[[f32; 4]; 6], c: [f32; 3], r: f32) -> bool {
    planes.iter().all(|n| n[0] * c[0] + n[1] * c[1] + n[2] * c[2] + n[3] >= -r)
}

/// `rot * v` for a row-major 3 x 3.
pub fn rotate(rot: &[f32; 9], v: [f32; 3]) -> [f32; 3] {
    [
        rot[0] * v[0] + rot[1] * v[1] + rot[2] * v[2],
        rot[3] * v[0] + rot[4] * v[1] + rot[5] * v[2],
        rot[6] * v[0] + rot[7] * v[1] + rot[8] * v[2],
    ]
}

/// Largest absolute row scale of a basis (bounding sphere radius factor).
pub fn max_scale(b: &[f32; 9]) -> f32 {
    let col = |j: usize| (b[j] * b[j] + b[3 + j] * b[3 + j] + b[6 + j] * b[6 + j]).sqrt();
    col(0).max(col(1)).max(col(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Box<[u32; 256]> {
        let mut p = Box::new([0u32; 256]);
        for (i, c) in p.iter_mut().enumerate() {
            *c = (i as u32) << 16 | (255 - i as u32) << 8 | 0x40 | 0xff00_0000;
        }
        p
    }

    #[test]
    fn indexed_expands_through_the_palette_opaque() {
        let px = expand_level(&TexFormat::Indexed8 { palette: palette() }, &[0, 1, 200, 255], 2, 2, None);
        assert_eq!(px[0], rgba(0, 255, 0x40, 255));
        assert_eq!(px[2], rgba(200, 55, 0x40, 255));
        assert_eq!(px[3], rgba(255, 0, 0x40, 255));
    }

    #[test]
    fn sixteen_bit_formats_replicate_bits() {
        // white, pure red, pure green in RGB565
        let px = expand_level(&TexFormat::Rgb565, &[0xff, 0xff, 0x00, 0xf8, 0xe0, 0x07, 0x1f, 0x00], 4, 1, None);
        assert_eq!(px, vec![rgba(255, 255, 255, 255), rgba(255, 0, 0, 255), rgba(0, 255, 0, 255), rgba(0, 0, 255, 255)]);
        let px = expand_level(&TexFormat::Argb1555, &[0x00, 0x7c, 0x1f, 0x80], 2, 1, None);
        assert_eq!(px, vec![rgba(255, 0, 0, 0), rgba(0, 0, 255, 255)]);
        let px = expand_level(&TexFormat::Argb4444, &[0x21, 0x8f], 1, 1, None);
        assert_eq!(px, vec![rgba(0xff, 0x22, 0x11, 0x88)]);
    }

    #[test]
    fn chroma_key_makes_holes_with_neighbour_colours() {
        // 3 x 1: blue, key (magenta), red; palette entries chosen directly
        let mut p = Box::new([0u32; 256]);
        p[1] = 0x0000ff;
        p[2] = 0xff00ff;
        p[3] = 0xff0000;
        let px = expand_level(&TexFormat::Indexed8 { palette: p }, &[1, 2, 3], 3, 1, Some(0xff00ff));
        assert_eq!(px[0], rgba(0, 0, 255, 255));
        assert_eq!(px[2], rgba(255, 0, 0, 255));
        // the hole: alpha 0, colour = average of blue and red
        assert_eq!(px[1], rgba(127, 0, 127, 0));
    }

    #[test]
    fn mip_chain_sizes_and_alpha_weighting() {
        assert_eq!(full_chain(256, 256), 9);
        assert_eq!(full_chain(256, 64), 9);
        assert_eq!(full_chain(1, 1), 1);
        let t = TextureData { width: 8, height: 2, format: TexFormat::Intensity8, pixels: vec![128; 16], mipmaps: Mipmaps::Generate };
        let levels = texture_levels(&t, None);
        let sizes: Vec<(u32, u32)> = levels.iter().map(|l| (l.0, l.1)).collect();
        assert_eq!(sizes, vec![(8, 2), (4, 1), (2, 1), (1, 1)]);
        // a transparent black texel does not darken the average of opaque white ones
        let (out, _, _) = halve(&[rgba(255, 255, 255, 255), 0, rgba(255, 255, 255, 255), rgba(255, 255, 255, 255)], 2, 2);
        assert_eq!(out[0] & 0xff_ffff, 0xff_ffff);
        assert_eq!(out[0] >> 24, 191); // (3 * 255 + 0) / 4 rounded
    }

    #[test]
    fn projection_matches_the_game_formula() {
        let proj = Projection { center: [320.0, 240.0], scale: [400.0, 400.0], near: 10.0, far: 10000.0 };
        let view = View { sky: None, camera: Camera { pos: [0.0; 3], rot: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], proj }, viewport: [0.0, 0.0, 640.0, 480.0], mirror: false, clear: None, fog: None };
        for ext in [0.0f32, 106.67] {
            let s = view_setup(&view, ext);
            let p = [123.0, -45.0, 800.0];
            let scr = proj.project(p).unwrap();
            let ndc = to_ndc(&s, p);
            // NDC back to game pixels through the widened viewport
            let x = s.viewport[0] + (ndc[0] + 1.0) * 0.5 * (s.viewport[2] - s.viewport[0]);
            let y = s.viewport[1] + (1.0 - ndc[1]) * 0.5 * (s.viewport[3] - s.viewport[1]);
            assert!((x - scr[0]).abs() < 1e-3 && (y - scr[1]).abs() < 1e-3, "ext {ext}: {x},{y} vs {scr:?}");
        }
        // mirror: flipped about the viewport's centre
        let m = View { mirror: true, viewport: [220.0, 20.0, 420.0, 95.0], ..view.clone() };
        let s = view_setup(&m, 50.0);
        assert_eq!(s.viewport, [220.0, 20.0, 420.0, 95.0]);
        let p = [10.0, 0.0, 100.0];
        let ndc = to_ndc(&s, p);
        let x = s.viewport[0] + (ndc[0] + 1.0) * 0.5 * 200.0;
        let unflipped = proj.project(p).unwrap()[0];
        assert!((x - (640.0 - unflipped)).abs() < 1e-3);
    }

    #[test]
    fn frustum_culls_spheres() {
        let proj = Projection { center: [320.0, 240.0], scale: [320.0, 320.0], near: 10.0, far: 1000.0 };
        let view = View { sky: None, camera: Camera { pos: [0.0; 3], rot: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], proj }, viewport: [0.0, 0.0, 640.0, 480.0], mirror: false, clear: None, fog: None };
        let s = view_setup(&view, 0.0);
        assert!(sphere_visible(&s.planes, [0.0, 0.0, 500.0], 1.0));
        assert!(!sphere_visible(&s.planes, [0.0, 0.0, -50.0], 10.0)); // behind
        assert!(!sphere_visible(&s.planes, [0.0, 0.0, 1200.0], 100.0)); // beyond far
        assert!(sphere_visible(&s.planes, [0.0, 0.0, 1050.0], 100.0)); // reaches into it
        // 90 degree horizontal field (scale = half width): x = z is the edge
        assert!(!sphere_visible(&s.planes, [600.0, 0.0, 500.0], 50.0));
        assert!(sphere_visible(&s.planes, [530.0, 0.0, 500.0], 50.0));
    }

    #[test]
    fn statics_merge_into_precise_chunks() {
        let quad = Mesh {
            vertices: vec![
                Vertex { pos: [0.0, 0.0, 0.0], ..Default::default() },
                Vertex { pos: [0.25, 0.0, 0.0], ..Default::default() },
                Vertex { pos: [0.0, 0.0, 0.25], ..Default::default() },
            ],
            indices: vec![0, 1, 2],
            ranges: vec![MeshRange { material: 0, indices: 0..3 }, MeshRange { material: 1, indices: 0..3 }],
        };
        // far from the world's origin: 0.25-unit offsets would be lost in f32 world positions
        let far = 3.0e7;
        let scene = SceneData {
            texture_frames: Vec::new(),
            meshes: vec![quad],
            statics: vec![Instance { phase: 0, scroll: [0.0; 2], mesh: 0, transform: Transform::at([far, 0.0, far]) }, Instance { phase: 0, scroll: [0.0; 2], mesh: 0, transform: Transform::at([far + 100.0, 0.0, far]) }, Instance { phase: 0, scroll: [0.0; 2], mesh: 0, transform: Transform::at([0.0; 3]) }],
            ..Default::default()
        };
        let g = build_statics(&scene, &|m| m);
        assert_eq!(g.chunks.len(), 2);
        // two batches x two chunks
        assert_eq!(g.ranges.len(), 4);
        // sorted by batch then chunk, contiguous
        for w in g.ranges.windows(2) {
            assert!((w[0].batch, w[0].chunk) < (w[1].batch, w[1].chunk));
            assert_eq!(w[0].start + w[0].count, w[1].start);
        }
        // exact relative positions survive
        let far_chunk = g.chunks.iter().position(|c| c.origin[0] > 1.0e6).unwrap() as u32;
        let r = g.ranges.iter().find(|r| r.chunk == far_chunk && r.batch == 0).unwrap();
        let o = g.chunks[far_chunk as usize].origin;
        let v: Vec<[f64; 3]> = g.indices[r.start as usize..(r.start + r.count) as usize]
            .iter()
            .map(|&i| {
                let p = g.vertices[i as usize].pos;
                [p[0] as f64 + o[0], p[1] as f64 + o[1], p[2] as f64 + o[2]]
            })
            .collect();
        assert_eq!(v[1][0] - v[0][0], 0.25);
        assert_eq!(v[2][2] - v[0][2], 0.25);
        assert_eq!(v[3][0], far + 100.0);
        assert!(g.vertices.iter().all(|v| v.pos[0].abs() <= CHUNK_SIZE as f32));
        // every vertex names its static instance; the table names the chunk
        assert_eq!(g.statics.len(), 3);
        assert!(g.vertices.iter().all(|v| v.slot & STATIC_BIT != 0 && ((v.slot & !STATIC_BIT) as usize) < 3));
        assert_eq!(g.statics[0].chunk, far_chunk);
        assert_eq!(g.chunk_statics[far_chunk as usize], vec![0, 1]);
        assert_eq!(g.normals.len(), g.vertices.len());
    }

    #[test]
    fn chunks_are_columns() {
        // objects above and below y = 0 in one 8192-unit column share a chunk
        let tri = Mesh { vertices: vec![Vertex::default(); 3], indices: vec![0, 1, 2], ranges: vec![MeshRange { material: 0, indices: 0..3 }] };
        let scene = SceneData {
            meshes: vec![tri],
            statics: vec![Instance::new(0, Transform::at([100.0, -40.0, 100.0])), Instance::new(0, Transform::at([200.0, 3000.0, 300.0])), Instance::new(0, Transform::at([9000.0, 0.0, 100.0]))],
            ..Default::default()
        };
        let g = build_statics(&scene, &|_| 0);
        assert_eq!(g.chunks.len(), 2);
        let c = g.statics[0].chunk as usize;
        assert_eq!(g.statics[1].chunk as usize, c);
        assert_eq!(g.chunks[c].origin[1], 1480.0); // middle of -40 and 3000
    }

    #[test]
    fn normals_follow_the_instance_basis() {
        let tri = Mesh {
            vertices: vec![Vertex { normal: [1.0, 0.0, 0.0], ..Default::default() }; 3],
            indices: vec![0, 1, 2],
            ranges: vec![MeshRange { material: 0, indices: 0..3 }],
        };
        // 90 degrees about y: x -> -z
        let basis = [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0];
        let scene = SceneData { meshes: vec![tri.clone()], statics: vec![Instance::new(0, Transform { basis, origin: [0.0; 3] })], ..Default::default() };
        let g = build_statics(&scene, &|_| 0);
        assert!(g.normals.iter().all(|n| *n == [0.0, 0.0, -1.0]));
        let (mut v, mut n, mut i) = (Vec::new(), Vec::new(), Vec::new());
        flatten_mesh(&tri, 5, &mut v, &mut n, &mut i);
        assert_eq!(n, vec![[1.0, 0.0, 0.0]; 3]);
        assert!(v.iter().all(|v| v.slot == 5));
    }

    #[test]
    fn anim_frame_is_exact() {
        // the contract's formula, including values whose product needs more than 32 bits
        assert_eq!(anim_frame(10, 0, 14), 0);
        assert_eq!(anim_frame(10, 1638, 14), 0);
        assert_eq!(anim_frame(10, 1639, 14), 1);
        assert_eq!(anim_frame(10, 16383, 14), 9);
        assert_eq!(anim_frame(10, 16384, 14), 9);
        assert_eq!(anim_frame(128, u32::MAX, 14), 127);
        assert_eq!(anim_frame(4, 0xc000, 16), 3);
        assert_eq!(anim_frame(4, 0xbfff, 16), 2);
        assert_eq!(anim_frame(1, 12345, 14), 0);
        assert_eq!(anim_value(AnimSource::Phase, 100, 7, 9), (100, 14));
        assert_eq!(anim_value(AnimSource::Phase, -1, 7, 9), (7, 14));
        assert_eq!(anim_value(AnimSource::Env, 100, 7, 9), (9, 16));
    }

    #[test]
    fn reflection_reference_cases() {
        // a surface facing the camera, straight ahead: the reflection points back at the
        // eye; game view r = (0, 0, -1): u = 127, v = round(-127) + 127 = 0
        let t = reflection_texel([0.0, 0.0, 1000.0], [0.0, 0.0, -1.0], [0.0, 0.0], false);
        assert_eq!(t, [127.0, 0.0]);
        // scroll subtracts
        let t = reflection_texel([0.0, 0.0, 1000.0], [0.0, 0.0, -1.0], [10.0, 3.0], false);
        assert_eq!(t, [117.0, -3.0]);
        // a floor seen from above (normal up = view -y), right of the centre: game e.x < 0
        // (x is left), r.x = -e.x > 0; the mirror view negates x
        let p = [300.0, 200.0, 1000.0];
        let a = reflection_texel(p, [0.0, -1.0, 0.0], [0.0; 2], false);
        let b = reflection_texel(p, [0.0, -1.0, 0.0], [0.0; 2], true);
        assert!(a[0] > 127.0 && b[0] < 127.0 && a[0] + b[0] == 254.0, "{a:?} {b:?}");
        assert_eq!(a[1], b[1]);
        // the game's view origin is 128 units in front of the eye: a point 128 units ahead
        // on the axis is at the origin's depth, so e is sideways
        let t = reflection_texel([-10.0, 0.0, 128.0], [0.0; 3], [0.0; 2], false);
        // game p = (10, 0, 0): e = (1, 0, 0); n = 0: r = -e -> u = -127 + 127 = 0, v = 127
        assert_eq!(t, [0.0, 127.0]);
    }

    #[test]
    fn fade_dither_coverage() {
        let coverage = |f: u32| (0..4).flat_map(|y| (0..4).map(move |x| (x, y))).filter(|&(x, y)| fade_draws(f, x, y)).count();
        assert_eq!(coverage(0), 0);
        assert_eq!(coverage(8), 0);
        assert_eq!(coverage(9), 1);
        assert_eq!(coverage(128), 8);
        assert_eq!(coverage(254), 16);
        assert_eq!(coverage(255), 16);
        // monotonic
        assert!((0..255).all(|f| coverage(f) <= coverage(f + 1)));
    }

    #[test]
    fn frames_of_other_sizes_are_resampled() {
        let t = TextureData { width: 2, height: 1, format: TexFormat::Intensity8, pixels: vec![10, 200], mipmaps: Mipmaps::None };
        let l = texture_levels_as(&t, None, 4, 2, 3);
        assert_eq!(l.iter().map(|l| (l.0, l.1)).collect::<Vec<_>>(), vec![(4, 2), (2, 1), (1, 1)]);
        let grey = |c: u32| c & 0xff;
        assert_eq!(l[0].2.iter().map(|&c| grey(c)).collect::<Vec<_>>(), vec![10, 10, 200, 200, 10, 10, 200, 200]);
        // the texture's own shape: as texture_levels
        let own = texture_levels_as(&t, None, 2, 1, 1);
        assert_eq!(own, texture_levels(&t, None));
    }

    #[test]
    fn sky_quads_extend_s_over_wide_views() {
        let sky = Sky { texture: 0, s: [100.0, 259.0], t: [20.0, 255.0], horizon: 200.0, ground: Some(0x405040) };
        let q = sky_quads(&sky, [0.0, 0.0, 640.0, 480.0], [0.0, 0.0, 640.0, 480.0]);
        assert_eq!(q.sky, Some(([0.0, 0.0, 640.0, 200.0], [100.0, 20.0, 259.0, 255.0])));
        assert_eq!(q.ground, Some([0.0, 200.0, 640.0, 480.0]));
        // widened by 80 game pixels on each side: s extends at the same rate
        let q = sky_quads(&sky, [-80.0, 0.0, 720.0, 480.0], [0.0, 0.0, 640.0, 480.0]);
        let (_, st) = q.sky.unwrap();
        assert!((st[0] - (100.0 - 159.0 / 8.0)).abs() < 1e-4 && (st[2] - (259.0 + 159.0 / 8.0)).abs() < 1e-4, "{st:?}");
        // mirror: the pair as given (already swapped by the game), at the viewport's edges
        let m = Sky { s: [259.0, 100.0], ..sky };
        let q = sky_quads(&m, [161.0, 33.6, 481.0, 117.6], [161.0, 33.6, 481.0, 117.6]);
        assert_eq!(q.sky.unwrap().1[0], 259.0);
        // horizon above the view: ground only; below: sky only
        let q = sky_quads(&Sky { horizon: -5.0, ..sky }, [0.0, 0.0, 640.0, 480.0], [0.0, 0.0, 640.0, 480.0]);
        assert!(q.sky.is_none() && q.ground == Some([0.0, 0.0, 640.0, 480.0]));
        let q = sky_quads(&Sky { horizon: 500.0, ground: None, ..sky }, [0.0, 0.0, 640.0, 480.0], [0.0, 0.0, 640.0, 480.0]);
        assert!(q.sky.is_some() && q.ground.is_none());
    }

    #[test]
    fn bases_are_the_lowest_part() {
        // a triangle from y 0 to 1000: clipped at 200 (a fifth); a flat one is whole
        let t = [[0.0, 0.0, 0.0], [100.0, 1000.0, 0.0], [200.0, 0.0, 0.0]];
        let p = clip_below(&t, 200.0);
        assert_eq!(p, vec![[0.0, 0.0, 0.0], [20.0, 200.0, 0.0], [180.0, 200.0, 0.0], [200.0, 0.0, 0.0]]);
        assert!(clip_below(&t, -1.0).is_empty());
        assert_eq!(clip_below(&t, 2000.0).len(), 3);
        // a tower (0..1000, two triangles) and a flat slab at 5000 (all base); a blended
        // material makes no base; vertices relative to the chunk, slot = the static
        let wall = Mesh {
            vertices: [[0.0, 0.0], [100.0, 0.0], [100.0, 1000.0], [0.0, 1000.0]].iter().map(|p| Vertex { pos: [p[0], p[1], 0.0], ..Default::default() }).collect(),
            indices: vec![0, 1, 2, 0, 2, 3],
            ranges: vec![MeshRange { material: 0, indices: 0..6 }],
        };
        let flat = Mesh {
            vertices: [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]].iter().map(|p| Vertex { pos: [p[0], 30.0, p[1]], ..Default::default() }).collect(),
            indices: vec![0, 1, 2],
            ranges: vec![MeshRange { material: 0, indices: 0..3 }],
        };
        let glass = Mesh { ranges: vec![MeshRange { material: 1, indices: 0..3 }], ..flat.clone() };
        let scene = SceneData {
            materials: vec![Material::flat(), Material::blended(None, Blend::ALPHA)],
            meshes: vec![wall, flat, glass],
            statics: vec![Instance::new(0, Transform::at([10.0, 0.0, 0.0])), Instance::new(1, Transform::at([0.0, 5000.0, 0.0])), Instance::new(2, Transform::IDENTITY)],
            ..Default::default()
        };
        let g = build_statics(&scene, &|m| m);
        let b = build_bases(&scene, &g, &|m| m == 0);
        let o = g.chunks[0].origin;
        let world = |v: &BaseVertex| [v.pos[0] as f64 + o[0], v.pos[1] as f64 + o[1], v.pos[2] as f64 + o[2]];
        let tower: Vec<[f64; 3]> = b.iter().filter(|v| v.slot == 0).map(world).collect();
        assert!(!tower.is_empty() && tower.len() % 3 == 0);
        assert!(tower.iter().all(|p| p[1] <= 200.0 + 1e-3), "a fifth of 1000");
        assert!(tower.iter().any(|p| (p[1] - 200.0).abs() < 1e-3) && tower.iter().any(|p| (p[0] - 110.0).abs() < 1e-3));
        // the flat slab is all base (height 0 < BASE_MIN), at its place
        let slab: Vec<[f64; 3]> = b.iter().filter(|v| v.slot == 1).map(world).collect();
        assert_eq!(slab.len(), 3);
        assert!(slab.iter().all(|p| (p[1] - 5030.0).abs() < 1e-3));
        assert!(b.iter().all(|v| v.slot != 2), "no base from blended polygons");
    }
}
