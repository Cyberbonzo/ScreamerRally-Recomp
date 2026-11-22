//! A synthetic scene for `SR_NATIVE_TEST` (no game data): a large textured ground far from
//! the world's origin, a few hundred boxes (static and moving), colour-keyed trees, glass
//! panes, additive glows, mirror-like spheres with the game's reflection (mode 5: per-vertex
//! normals, scroll, the env map animated by the env phase), animated flags (fixed phases,
//! the clock, and moving boxes with their own phases), road markings (static decals),
//! shadows built per frame (alpha and 2 x src x dst multiply decals), screen-door fades
//! (blinking boxes, trees fading with distance), the game's sky and ground quads
//! (`View::sky`), fog, and a camera flying a loop with a rear-view mirror. It exercises
//! every material kind, every texture format family and every part of the frame
//! description.
//!
//! `SR_NATIVE_TEST_OFF=fade,anim,reflect,sky,multiply` (any of them) turns features off, to
//! measure what each costs.
//!
//! World axes of the test: x east, y up, z north (right-handed). Model meshes wind their
//! triangles counter-clockwise around outward normals, which the renderer sees as
//! counter-clockwise on screen from the front: materials cull `Cull::Clockwise`.

use super::scene::*;
use std::f64::consts::TAU;

/// Where the test world lies: far from the origin, where world positions in f32 would be
/// off by a quarter unit and more (the renderer must work camera-relative).
const WORLD: [f64; 3] = [3.0e6, 0.0, -2.0e6];
const TILES: i32 = 32;
const TILE: f64 = 2048.0;
const TILE_QUADS: usize = 8;
const LOOP_RADIUS: f64 = 20000.0;
const LOOP_SECONDS: f64 = 50.0;

/// Deterministic pseudo-random numbers (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in `lo..hi`.
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.next() >> 11) as f64 / (1u64 << 53) as f64 * (hi - lo)
    }
}

/// Ground height at local position (x, z) (relative to `WORLD`).
fn height(x: f64, z: f64) -> f64 {
    180.0 * (x * 0.00031).sin() * (z * 0.00027).cos() + 60.0 * (x * 0.0011 + z * 0.0007).sin()
}

/// Camera position on its loop at time `t` (local coordinates), and the direction of travel.
fn camera_path(t: f64) -> ([f64; 3], [f64; 3]) {
    let a = t / LOOP_SECONDS * TAU;
    // a slightly squashed loop around the middle of the ground
    let (x, z) = (LOOP_RADIUS * a.cos(), 0.8 * LOOP_RADIUS * a.sin());
    let y = height(x, z) + 380.0 + 120.0 * (a * 3.0).sin();
    let (dx, dz) = (-LOOP_RADIUS * a.sin(), 0.8 * LOOP_RADIUS * a.cos());
    let l = (dx * dx + dz * dz).sqrt();
    ([x, y, z], [dx / l, 0.0, dz / l])
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// World-to-view rotation looking along `fwd` (unit) with world y up: rows right, down,
/// forward.
fn look(fwd: [f64; 3], pitch: f64) -> [f32; 9] {
    let f = {
        let (c, s) = (pitch.cos(), pitch.sin());
        let h = (fwd[0] * fwd[0] + fwd[2] * fwd[2]).sqrt().max(1e-9);
        [fwd[0] / h * c, s, fwd[2] / h * c]
    };
    let up = [0.0, 1.0, 0.0];
    // right = f x up
    let mut r = [f[1] * up[2] - f[2] * up[1], f[2] * up[0] - f[0] * up[2], f[0] * up[1] - f[1] * up[0]];
    let l = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt().max(1e-9);
    r = [r[0] / l, r[1] / l, r[2] / l];
    // down = f x right... (right x down = forward): down = forward x right
    let d = [f[1] * r[2] - f[2] * r[1], f[2] * r[0] - f[0] * r[2], f[0] * r[1] - f[1] * r[0]];
    [r[0] as f32, r[1] as f32, r[2] as f32, d[0] as f32, d[1] as f32, d[2] as f32, f[0] as f32, f[1] as f32, f[2] as f32]
}

/// Rotation about y by `a` radians, scaled per axis.
fn yaw_scale(a: f64, s: [f32; 3]) -> [f32; 9] {
    let (c, n) = (a.cos() as f32, a.sin() as f32);
    [c * s[0], 0.0, n * s[2], 0.0, s[1], 0.0, -n * s[0], 0.0, c * s[2]]
}

fn v(pos: [f32; 3], uv: [f32; 2], color: [u8; 4]) -> Vertex {
    Vertex { pos, uv, color, ..Default::default() }
}

/// A unit cube (-0.5..0.5) with per-face uv and a little fake light in the vertex colour.
fn cube(material: u32, tint: [u8; 3]) -> Mesh {
    let faces: [([f32; 3], [f32; 3], [f32; 3], f32); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0], 0.8),  // +x
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0], 0.7),  // -x
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0], 1.0),  // +y
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0.4),  // -y
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.9),   // +z
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.6), // -z
    ];
    let mut m = Mesh::default();
    for (n, u, w, light) in faces {
        let base = m.vertices.len() as u32;
        let c = [(tint[0] as f32 * light) as u8, (tint[1] as f32 * light) as u8, (tint[2] as f32 * light) as u8, 255];
        for (su, sw, uv) in [(-1.0f32, -1.0f32, [0.0, 1.0]), (1.0, -1.0, [1.0, 1.0]), (1.0, 1.0, [1.0, 0.0]), (-1.0, 1.0, [0.0, 0.0])] {
            let p = [0.5 * (n[0] + su * u[0] + sw * w[0]), 0.5 * (n[1] + su * u[1] + sw * w[1]), 0.5 * (n[2] + su * u[2] + sw * w[2])];
            m.vertices.push(v(p, uv, c));
        }
        // u x w = n: (0, 1, 2) is counter-clockwise around n
        m.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    m.ranges.push(MeshRange { material, indices: 0..36 });
    m
}

/// A vertical quad in the xy plane, `w` x `h`, base at y = 0, both sides (no culling).
fn quad(material: u32, w: f32, h: f32, color: [u8; 4]) -> Mesh {
    Mesh {
        vertices: vec![v([-w / 2.0, 0.0, 0.0], [0.0, 1.0], color), v([w / 2.0, 0.0, 0.0], [1.0, 1.0], color), v([w / 2.0, h, 0.0], [1.0, 0.0], color), v([-w / 2.0, h, 0.0], [0.0, 0.0], color)],
        indices: vec![0, 1, 2, 0, 2, 3],
        ranges: vec![MeshRange { material, indices: 0..6 }],
    }
}

/// Two crossed quads (a tree).
fn cross(material: u32) -> Mesh {
    let mut m = quad(material, 1.0, 1.0, [255; 4]);
    let n = m.vertices.len() as u32;
    for i in 0..4 {
        let p = m.vertices[i].pos;
        m.vertices.push(v([0.0, p[1], p[0]], m.vertices[i].uv, [235, 235, 235, 255]));
    }
    m.indices.extend_from_slice(&[n, n + 1, n + 2, n, n + 2, n + 3]);
    m.ranges[0].indices = 0..12;
    m
}

/// A UV sphere of radius 0.5, outward winding.
fn sphere(material: u32, color: [u8; 4], slices: u32, stacks: u32, inside: bool) -> Mesh {
    let mut m = Mesh::default();
    for i in 0..=stacks {
        let th = std::f32::consts::PI * i as f32 / stacks as f32;
        for j in 0..=slices {
            let ph = std::f32::consts::TAU * j as f32 / slices as f32;
            let p = [0.5 * th.sin() * ph.cos(), 0.5 * th.cos(), 0.5 * th.sin() * ph.sin()];
            let n = if inside { [-2.0 * p[0], -2.0 * p[1], -2.0 * p[2]] } else { [2.0 * p[0], 2.0 * p[1], 2.0 * p[2]] };
            m.vertices.push(Vertex { normal: n, ..v(p, [j as f32 / slices as f32 * 4.0, i as f32 / stacks as f32 * 2.0], color) });
        }
    }
    let row = slices + 1;
    for i in 0..stacks {
        for j in 0..slices {
            let (a, b, c, d) = (i * row + j, i * row + j + 1, (i + 1) * row + j + 1, (i + 1) * row + j);
            // outward: (a, b, c) counter-clockwise seen from outside
            if inside {
                m.indices.extend_from_slice(&[a, c, b, a, d, c]);
            } else {
                m.indices.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
    }
    let n = m.indices.len() as u32;
    m.ranges.push(MeshRange { material, indices: 0..n });
    m
}

fn palette(f: impl Fn(u32) -> (u32, u32, u32)) -> Box<[u32; 256]> {
    let mut p = Box::new([0u32; 256]);
    for (i, e) in p.iter_mut().enumerate() {
        let (r, g, b) = f(i as u32);
        *e = r.min(255) << 16 | g.min(255) << 8 | b.min(255);
    }
    p
}

fn textures() -> Vec<TextureData> {
    let mut rng = Rng(0x5eed_1997);
    let mut t = Vec::new();
    // 0: ground, 256 x 256 paletted grass with gravel and tile seams
    let grass = palette(|i| (40 + i / 3, 70 + i / 2, 25 + i / 5));
    let mut px = vec![0u8; 256 * 256];
    for y in 0..256 {
        for x in 0..256 {
            let n = (rng.next() % 64) as i32;
            let wave = ((x as f32 * 0.09).sin() * (y as f32 * 0.07).cos() * 60.0) as i32;
            let seam = if x % 128 < 2 || y % 128 < 2 { -90 } else { 0 };
            px[y * 256 + x] = (128 + n - 32 + wave + seam).clamp(0, 255) as u8;
        }
    }
    t.push(TextureData { width: 256, height: 256, format: TexFormat::Indexed8 { palette: grass }, pixels: px, mipmaps: Mipmaps::Generate });
    // 1: checker, 128 x 128 paletted (gradient palette)
    let pal = palette(|i| (i, 255 - i, 128 + i / 2));
    let px: Vec<u8> = (0..128 * 128).map(|i| { let (x, y) = (i % 128, i / 128); if (x / 16 + y / 16) % 2 == 0 { (x * 2) as u8 } else { 255 - (y * 2) as u8 } }).collect();
    t.push(TextureData { width: 128, height: 128, format: TexFormat::Indexed8 { palette: pal }, pixels: px, mipmaps: Mipmaps::Generate });
    // 2: bricks, 128 x 64 RGB565 (16-bit, not square)
    let mut px = Vec::with_capacity(128 * 64 * 2);
    for y in 0..64u32 {
        for x in 0..128u32 {
            let row = y / 8;
            let mortar = y % 8 == 0 || (x + if row % 2 == 0 { 0 } else { 8 }) % 16 == 0;
            let (r, g, b) = if mortar { (24u32, 48u32, 24u32) } else { (22 + (x * 7 + y * 3) % 6, 18 + (x * 5) % 8, 6 + y % 4) };
            let v = (r << 11 | g << 5 | b) as u16;
            px.extend_from_slice(&v.to_le_bytes());
        }
    }
    t.push(TextureData { width: 128, height: 64, format: TexFormat::Rgb565, pixels: px, mipmaps: Mipmaps::None });
    // 3: tree, 64 x 128 paletted; index 0 = magenta (the chroma key)
    let pal = palette(|i| if i == 0 { (255, 0, 255) } else if i < 128 { (20 + i / 4, 60 + i, 20) } else { (90 + (i - 128) / 3, 60 + (i - 128) / 4, 30) });
    let mut px = vec![0u8; 64 * 128];
    for y in 0..128i32 {
        for x in 0..64i32 {
            let crown = ((x - 32) * (x - 32)) as f32 / 900.0 + ((y - 50) * (y - 50)) as f32 / 2500.0 < 1.0;
            let trunk = (x - 32).abs() < 4 && y >= 80;
            px[(y * 64 + x) as usize] = if crown { 1 + (rng.next() % 126) as u8 } else if trunk { 128 + (rng.next() % 100) as u8 } else { 0 };
        }
    }
    t.push(TextureData { width: 64, height: 128, format: TexFormat::Indexed8 { palette: pal }, pixels: px, mipmaps: Mipmaps::Generate });
    // 4: glass, 64 x 64 ARGB4444 with alpha varying
    let mut px = Vec::with_capacity(64 * 64 * 2);
    for y in 0..64u32 {
        for x in 0..64u32 {
            let frame = x < 3 || y < 3 || x > 60 || y > 60;
            let (a, r, g, b) = if frame { (15u32, 3u32, 3u32, 4u32) } else { (6 + (x + y) / 32, 8, 12, 15) };
            px.extend_from_slice(&((a << 12 | r << 8 | g << 4 | b) as u16).to_le_bytes());
        }
    }
    t.push(TextureData { width: 64, height: 64, format: TexFormat::Argb4444, pixels: px, mipmaps: Mipmaps::None });
    // 5: environment map, 128 x 128 RGB565: sky above, ground below, a sun
    let mut px = Vec::with_capacity(128 * 128 * 2);
    for y in 0..128i32 {
        for x in 0..128i32 {
            let (r, g, b) = if ((x - 80) * (x - 80) + (y - 40) * (y - 40)) < 64 { (31u32, 63u32, 28u32) } else if y < 64 { (10 + y as u32 / 8, 30 + y as u32 / 4, 31) } else { (8, 20 + (x as u32 % 16), 6) };
            px.extend_from_slice(&((r << 11 | g << 5 | b) as u16).to_le_bytes());
        }
    }
    t.push(TextureData { width: 128, height: 128, format: TexFormat::Rgb565, pixels: px, mipmaps: Mipmaps::Generate });
    // 6: clouds, 256 x 128 intensity
    let px: Vec<u8> = (0..256 * 128)
        .map(|i| {
            let (x, y) = ((i % 256) as f32, (i / 256) as f32);
            let c = (x * 0.05).sin() * (y * 0.11).cos() + (x * 0.13 + y * 0.05).sin() * 0.5 + (x * 0.021 - y * 0.037).cos() * 0.7;
            (170.0 + c * 40.0).clamp(0.0, 255.0) as u8
        })
        .collect();
    t.push(TextureData { width: 256, height: 128, format: TexFormat::Intensity8, pixels: px, mipmaps: Mipmaps::Generate });
    // 7: soft spot, 32 x 32 alpha (shadows, glows)
    let px: Vec<u8> = (0..32 * 32)
        .map(|i| {
            let (x, y) = ((i % 32) as f32 - 15.5, (i / 32) as f32 - 15.5);
            ((1.0 - (x * x + y * y).sqrt() / 16.0).clamp(0.0, 1.0) * 255.0) as u8
        })
        .collect();
    t.push(TextureData { width: 32, height: 32, format: TexFormat::Alpha8, pixels: px, mipmaps: Mipmaps::None });
    // 8: fence, 64 x 64 ARGB1555 (1-bit alpha), blended
    let mut px = Vec::with_capacity(64 * 64 * 2);
    for y in 0..64u32 {
        for x in 0..64u32 {
            let wire = (x + y) % 16 < 2 || (x + 64 - y) % 16 < 2 || y < 4;
            let v: u16 = if wire { 0x8000 | 20 << 10 | 20 << 5 | 22 } else { 0 };
            px.extend_from_slice(&v.to_le_bytes());
        }
    }
    t.push(TextureData { width: 64, height: 64, format: TexFormat::Argb1555, pixels: px, mipmaps: Mipmaps::None });
    // 9..=16: flag animation, 8 frames of 64 x 32 paletted stripes waving, a bar showing the
    // frame number; frame 3 is 32 x 16 (resampled into the frames' array)
    for k in 0..FLAG_FRAMES {
        let (w, h) = if k == 3 { (32u32, 16u32) } else { (64, 32) };
        let pal = palette(|i| match i {
            0 => (250, 250, 250),
            1 => (200, 30, 40),
            2 => (30, 60, 200),
            _ => (20, 20, 20),
        });
        let mut px = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / w as f32;
                let wave = (fx * 9.0 + k as f32 * std::f32::consts::TAU / FLAG_FRAMES as f32).sin() * 0.08 * fx;
                let fy = y as f32 / h as f32 + wave;
                let bar = fy > 0.86 && fx < (k + 1) as f32 / FLAG_FRAMES as f32;
                px[(y * w + x) as usize] = if bar { 3 } else if fy < 0.33 { 1 } else if fy < 0.66 { 0 } else { 2 };
            }
        }
        t.push(TextureData { width: w, height: h, format: TexFormat::Indexed8 { palette: pal }, pixels: px, mipmaps: Mipmaps::None });
    }
    // 17..=19: the env map with other tints (the env phase cycles through 5, 17, 18, 19)
    for tint in [(1.0f32, 0.6f32, 0.4f32), (0.5, 1.0, 0.5), (1.0, 0.9, 0.3)] {
        let px: Vec<u8> = t[5]
            .pixels
            .chunks(2)
            .flat_map(|c| {
                let v = u16::from_le_bytes([c[0], c[1]]) as u32;
                let (r, g, b) = (((v >> 11) as f32 * tint.0) as u32, ((((v >> 5) & 63) as f32) * tint.1) as u32, (((v & 31) as f32) * tint.2) as u32);
                ((r.min(31) << 11 | g.min(63) << 5 | b.min(31)) as u16).to_le_bytes()
            })
            .collect();
        t.push(TextureData { width: 128, height: 128, format: TexFormat::Rgb565, pixels: px, mipmaps: Mipmaps::Generate });
    }
    // 20: the sky, 256 x 256 RGB565 like the game's: clouds tiling in s, darker blue up top
    let mut px = Vec::with_capacity(256 * 256 * 2);
    for y in 0..256u32 {
        for x in 0..256u32 {
            let a = x as f32 / 256.0 * std::f32::consts::TAU;
            let c = ((a * 3.0).sin() * (y as f32 * 0.05).cos() + (a * 5.0 + y as f32 * 0.03).sin() * 0.6).max(0.0) * (y as f32 / 255.0);
            let k = y as f32 / 255.0;
            let (r, g, b) = (6.0 + 14.0 * k + 14.0 * c, 20.0 + 30.0 * k + 26.0 * c, 22.0 + 8.0 * k + 6.0 * c);
            px.extend_from_slice(&(((r.min(31.0) as u32) << 11 | (g.min(63.0) as u32) << 5 | b.min(31.0) as u32) as u16).to_le_bytes());
        }
    }
    t.push(TextureData { width: 256, height: 256, format: TexFormat::Rgb565, pixels: px, mipmaps: Mipmaps::None });
    // 21: multiply spot, 32 x 32 intensity: 64 (halves what is under it) in the middle,
    // 128 (no change under 2 x src x dst) at the rim
    let px: Vec<u8> = (0..32 * 32)
        .map(|i| {
            let (x, y) = ((i % 32) as f32 - 15.5, (i / 32) as f32 - 15.5);
            (128.0 - 64.0 * (1.0 - (x * x + y * y).sqrt() / 16.0).clamp(0.0, 1.0)) as u8
        })
        .collect();
    t.push(TextureData { width: 32, height: 32, format: TexFormat::Intensity8, pixels: px, mipmaps: Mipmaps::None });
    t
}

/// Frames of the flag animation (textures 9..).
const FLAG_FRAMES: u32 = 8;
const SKY_TEXTURE: u32 = 20;

/// The animation frame lists (`SceneData::texture_frames`): the flag, then the env map.
fn texture_frames() -> Vec<u32> {
    let mut f: Vec<u32> = (0..FLAG_FRAMES).map(|k| 9 + k).collect();
    f.extend_from_slice(&[5, 17, 18, 19]);
    f
}

/// Features `SR_NATIVE_TEST_OFF` turns off.
#[derive(Clone, Copy, Debug, Default)]
pub struct Off {
    pub fade: bool,
    pub anim: bool,
    pub reflect: bool,
    pub sky: bool,
    pub multiply: bool,
}

impl Off {
    pub fn from_env() -> Off {
        let v = std::env::var("SR_NATIVE_TEST_OFF").unwrap_or_default().to_ascii_lowercase();
        let has = |k: &str| v.split(',').any(|p| p.trim() == k);
        Off { fade: has("fade"), anim: has("anim"), reflect: has("reflect"), sky: has("sky"), multiply: has("multiply") }
    }
}

/// Material indices.
mod mat {
    pub const GROUND: u32 = 0;
    pub const CHECKER: u32 = 1;
    pub const BRICKS: u32 = 2;
    pub const FLAT: u32 = 3;
    pub const TREE: u32 = 4;
    pub const GLASS: u32 = 5;
    pub const GLOW: u32 = 6;
    pub const CHROME: u32 = 7;
    pub const SHADOW: u32 = 8;
    pub const SKY: u32 = 9;
    pub const MARKING: u32 = 10;
    pub const FENCE: u32 = 11;
    pub const FLAG: u32 = 12;
    pub const MULTIPLY: u32 = 13;
}

fn materials(off: Off) -> Vec<Material> {
    let cw = Cull::Clockwise;
    // the game's mode 5: opaque, repeat, reflection coordinates, the env map animated by
    // the env phase
    let chrome = Material {
        anim: (!off.anim).then_some(TexAnim { first: FLAG_FRAMES, count: 4, source: AnimSource::Env }),
        texgen: if off.reflect { TexGen::Vertex } else { TexGen::Reflection { scale: [1.0, 1.0], offset: [0.0, 0.0] } },
        bilinear: true,
        ..Material::opaque(5).culled(cw)
    };
    let flag = Material { anim: (!off.anim).then_some(TexAnim { first: 0, count: FLAG_FRAMES, source: AnimSource::Phase }), ..Material::opaque(9) };
    let multiply = Material::decal(Some(21), Transparency::Blended(Blend { src: BlendFactor::DstColor, dst: BlendFactor::SrcColor }), 40.0);
    vec![
        Material { bilinear: true, ..Material::opaque(0).culled(cw) },
        Material::opaque(1).culled(cw),
        Material::opaque(2).culled(cw),
        Material::flat().culled(cw),
        Material::chroma_key(3, 0xff00ff),
        Material::blended(Some(4), Blend::ALPHA),
        Material::blended(Some(7), Blend::ADD),
        chrome,
        Material::decal(Some(7), Transparency::Blended(Blend::ALPHA), 40.0),
        Material { combine: Combine::Modulate, ..Material::sky(Some(6)) },
        Material::decal(None, Transparency::Opaque, 12.0),
        Material { wrap: [Wrap::Repeat, Wrap::Clamp], ..Material::blended(Some(8), Blend::ALPHA) },
        flag,
        multiply,
    ]
}

/// Mesh indices fixed by `TestScene::build`.
struct Meshes {
    /// Mirror spheres by radius (200, 350, 500): the reflection needs a rotation (no scale)
    /// as the instance basis, like the game's part matrices.
    chrome: [u32; 3],
}

/// What kind of static each entry of `SceneData::statics` is (for the fades).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Ground,
    Box,
    Tree,
    Other,
}

/// A box that moves every frame.
struct Mover {
    local: [f64; 3],
    size: [f32; 3],
    phase: f64,
    spin: f64,
    mesh: u32,
    /// A mirror sphere (unit basis, scroll from its motion) instead of a box.
    chrome: bool,
}

/// The synthetic scene and its animation.
pub struct TestScene {
    pub scene: SceneData,
    meshes: Meshes,
    movers: Vec<Mover>,
    kinds: Vec<Kind>,
    /// Per static: its world position (fade by distance).
    centres: Vec<[f64; 3]>,
    off: Off,
}

impl TestScene {
    pub fn build() -> TestScene {
        TestScene::build_with(Off::from_env())
    }

    pub fn build_with(off: Off) -> TestScene {
        let mut rng = Rng(0x0dd_ba11_c0de);
        let mut meshes: Vec<Mesh> = Vec::new();
        let mut statics: Vec<Instance> = Vec::new();
        let half = TILES as f64 * TILE / 2.0;
        let at = |x: f64, y: f64, z: f64| add(WORLD, [x, y, z]);

        // ground: one mesh per tile (heights continue across tiles), vertices relative to
        // the tile's corner
        for ty in 0..TILES {
            for tx in 0..TILES {
                let (x0, z0) = (tx as f64 * TILE - half, ty as f64 * TILE - half);
                let mut m = Mesh::default();
                let n = TILE_QUADS + 1;
                for j in 0..n {
                    for i in 0..n {
                        let (lx, lz) = (i as f64 * TILE / TILE_QUADS as f64, j as f64 * TILE / TILE_QUADS as f64);
                        let h = height(x0 + lx, z0 + lz);
                        let shade = (200.0 + h * 0.25).clamp(120.0, 255.0) as u8;
                        m.vertices.push(v([lx as f32, h as f32, lz as f32], [((x0 + lx) / 1024.0) as f32, ((z0 + lz) / 1024.0) as f32], [shade, shade, shade, 255]));
                    }
                }
                for j in 0..TILE_QUADS {
                    for i in 0..TILE_QUADS {
                        let a = (j * n + i) as u32;
                        let (b, c, d) = (a + 1, a + 1 + n as u32, a + n as u32);
                        // counter-clockwise seen from above (+y): a, d, c (x east, z north)
                        m.indices.extend_from_slice(&[a, d, c, a, c, b]);
                    }
                }
                let count = m.indices.len() as u32;
                m.ranges.push(MeshRange { material: mat::GROUND, indices: 0..count });
                meshes.push(m);
                statics.push(Instance { phase: 0, scroll: [0.0; 2], mesh: meshes.len() as u32 - 1, transform: Transform::at(at(x0, 0.0, z0)) });
            }
        }
        let boxes = [meshes.len() as u32, meshes.len() as u32 + 1, meshes.len() as u32 + 2];
        meshes.push(cube(mat::CHECKER, [255, 255, 255]));
        meshes.push(cube(mat::BRICKS, [255, 240, 230]));
        meshes.push(cube(mat::FLAT, [200, 90, 40]));
        let flag_box = meshes.len() as u32;
        meshes.push(cube(mat::FLAG, [255, 255, 255]));
        let tree = meshes.len() as u32;
        meshes.push(cross(mat::TREE));
        let glass = meshes.len() as u32;
        meshes.push(quad(mat::GLASS, 1.0, 1.0, [255, 255, 255, 255]));
        let glow = meshes.len() as u32;
        meshes.push(quad(mat::GLOW, 1.0, 1.0, [255, 200, 120, 200]));
        let chrome_meshes = [meshes.len() as u32, meshes.len() as u32 + 1, meshes.len() as u32 + 2];
        for r in [200.0f32, 350.0, 500.0] {
            let mut m = sphere(mat::CHROME, [255; 4], 24, 16, false);
            for p in &mut m.vertices {
                p.pos = [p.pos[0] * 2.0 * r, p.pos[1] * 2.0 * r, p.pos[2] * 2.0 * r];
            }
            meshes.push(m);
        }
        let flag = meshes.len() as u32;
        meshes.push(quad(mat::FLAG, 1.0, 0.5, [255; 4]));
        let fence = meshes.len() as u32;
        meshes.push(quad(mat::FENCE, 1.0, 1.0, [255; 4]));
        {
            // a sky dome (sky layer), kept in the scene but not placed: the game's sky is the
            // view's screen-aligned quad (`View::sky`)
            let mut m = sphere(mat::SKY, [255; 4], 32, 16, true);
            for p in &mut m.vertices {
                let up = (p.pos[1] * 2.0).clamp(-1.0, 1.0);
                let k = up.max(0.0);
                p.color = [(168.0 - 110.0 * k) as u8, (192.0 - 90.0 * k) as u8, (216.0 - 20.0 * k) as u8, 255];
            }
            meshes.push(m);
        }
        let mut kinds = vec![Kind::Ground; statics.len()];

        // scatter: keep the camera's loop (a band around LOOP_RADIUS) mostly clear
        let place = |rng: &mut Rng, clear: f64| loop {
            let (x, z) = (rng.range(-half + 500.0, half - 500.0), rng.range(-half + 500.0, half - 500.0));
            let r = ((x / LOOP_RADIUS).powi(2) + (z / (0.8 * LOOP_RADIUS)).powi(2)).sqrt();
            if (r - 1.0).abs() * LOOP_RADIUS > clear {
                return (x, z);
            }
        };
        for i in 0..420 {
            let (x, z) = place(&mut rng, 700.0);
            let s = [rng.range(200.0, 900.0) as f32, rng.range(200.0, 1400.0) as f32, rng.range(200.0, 900.0) as f32];
            let basis = yaw_scale(rng.range(0.0, TAU), s);
            statics.push(Instance { phase: 0, scroll: [0.0; 2], mesh: boxes[i % 3], transform: Transform { basis, origin: at(x, height(x, z) + s[1] as f64 * 0.5 - 20.0, z) } });
            kinds.push(Kind::Box);
        }
        for _ in 0..320 {
            let (x, z) = place(&mut rng, 400.0);
            let s = rng.range(500.0, 1100.0) as f32;
            statics.push(Instance { phase: 0, scroll: [0.0; 2], mesh: tree, transform: Transform { basis: yaw_scale(rng.range(0.0, TAU), [s, s, s]), origin: at(x, height(x, z) - 10.0, z) } });
            kinds.push(Kind::Tree);
        }
        for _ in 0..70 {
            let (x, z) = place(&mut rng, 300.0);
            let (w, h) = (rng.range(300.0, 900.0) as f32, rng.range(250.0, 700.0) as f32);
            statics.push(Instance { phase: 0, scroll: [0.0; 2], mesh: glass, transform: Transform { basis: yaw_scale(rng.range(0.0, TAU), [w, h, 1.0]), origin: at(x, height(x, z) + 30.0, z) } });
        }
        for _ in 0..50 {
            let (x, z) = place(&mut rng, 200.0);
            let s = rng.range(150.0, 400.0) as f32;
            statics.push(Instance { phase: 0, scroll: [0.0; 2], mesh: glow, transform: Transform { basis: yaw_scale(rng.range(0.0, TAU), [s, s, s]), origin: at(x, height(x, z) + 200.0, z) } });
        }
        for i in 0..36 {
            let (x, z) = place(&mut rng, 500.0);
            let k = i % 3;
            let r = [200.0, 350.0, 500.0][k];
            // a fixed scroll per sphere (statics keep theirs)
            let scroll = [(rng.next() % 256) as f32, (rng.next() % 256) as f32];
            statics.push(Instance { phase: 0, scroll, mesh: chrome_meshes[k], transform: Transform { basis: yaw_scale(rng.range(0.0, TAU), [1.0; 3]), origin: at(x, height(x, z) + r, z) } });
        }
        // flags on poles: fixed phases (each shows one frame) and phase -1 (the clock)
        for i in 0..80 {
            let (x, z) = place(&mut rng, 350.0);
            let s = rng.range(500.0, 900.0) as f32;
            let phase = if i % 2 == 0 { -1 } else { ((i / 2) % FLAG_FRAMES as i32) * 16384 / FLAG_FRAMES as i32 + 1000 };
            statics.push(Instance { phase, scroll: [0.0; 2], mesh: flag, transform: Transform { basis: yaw_scale(rng.range(0.0, TAU), [s, s, s]), origin: at(x, height(x, z) + 500.0, z) } });
        }
        // fences and road markings along the loop
        let mut marks = Mesh::default();
        let steps = 720;
        for k in 0..steps {
            let a0 = k as f64 / steps as f64 * TAU;
            let a1 = (k as f64 + 0.5) / steps as f64 * TAU;
            let p = |a: f64, r: f64| (r * LOOP_RADIUS * a.cos(), r * 0.8 * LOOP_RADIUS * a.sin());
            for r in [0.985, 1.015] {
                let (x0, z0) = p(a0, r);
                let (x1, z1) = p(a1, r);
                let base = marks.vertices.len() as u32;
                let (nx, nz) = (x1 - x0, z1 - z0);
                let l = (nx * nx + nz * nz).sqrt();
                let (ox, oz) = (-nz / l * 25.0, nx / l * 25.0);
                for (x, z) in [(x0 - ox, z0 - oz), (x1 - ox, z1 - oz), (x1 + ox, z1 + oz), (x0 + ox, z0 + oz)] {
                    marks.vertices.push(v([x as f32, height(x, z) as f32, z as f32], [0.0, 0.0], [235, 235, 220, 255]));
                }
                marks.indices.extend_from_slice(&[base, base + 3, base + 2, base, base + 2, base + 1]);
            }
            if k % 6 == 0 {
                let (x, z) = p(a0, 1.06);
                let ang = -(a0 + std::f64::consts::FRAC_PI_2);
                statics.push(Instance { phase: 0, scroll: [0.0; 2], mesh: fence, transform: Transform { basis: yaw_scale(ang, [900.0, 160.0, 1.0]), origin: at(x, height(x, z), z) } });
            }
        }
        let n = marks.indices.len() as u32;
        marks.ranges.push(MeshRange { material: mat::MARKING, indices: 0..n });
        meshes.push(marks);
        statics.push(Instance { phase: 0, scroll: [0.0; 2], mesh: meshes.len() as u32 - 1, transform: Transform::at(WORLD) });
        kinds.resize(statics.len(), Kind::Other);
        let centres: Vec<[f64; 3]> = statics.iter().map(|i| i.transform.origin).collect();

        // movers near the loop
        let mut movers = Vec::new();
        for i in 0..140 {
            let a = rng.range(0.0, TAU);
            let r = 1.0 + rng.range(-0.06, 0.06) * if i % 2 == 0 { 1.0 } else { -1.0 };
            let (x, z) = (r * LOOP_RADIUS * a.cos(), r * 0.8 * LOOP_RADIUS * a.sin());
            let s = rng.range(80.0, 260.0) as f32;
            let chrome = i % 10 == 9;
            let mesh = if chrome { chrome_meshes[0] } else if i % 7 == 3 { flag_box } else { boxes[i % 3] };
            movers.push(Mover { local: [x, height(x, z), z], size: [s, s, s], phase: rng.range(0.0, TAU), spin: rng.range(-2.0, 2.0), mesh, chrome });
        }
        TestScene {
            scene: SceneData { texture_frames: texture_frames(), textures: textures(), materials: materials(off), meshes, statics },
            meshes: Meshes { chrome: chrome_meshes },
            movers,
            kinds,
            centres,
            off,
        }
    }

    /// The frame at time `t` seconds: main view and mirror with the game's sky, moving boxes
    /// and spheres, their shadows, the static fades, the animation clock and env phase.
    pub fn frame(&self, t: f64, out: &mut FrameData) {
        out.views.clear();
        out.dynamics.clear();
        out.meshes.clear();
        out.static_fade.clear();
        let (pos, fwd) = camera_path(t);
        let cam = add(WORLD, pos);
        let fog = Fog { w_scale: 1.0, ..Fog::linear(0xa8c0d8, 14000.0, 46000.0) };
        let main = Camera { pos: cam, rot: look(fwd, -0.12), proj: Projection { center: [320.0, 240.0], scale: [400.0, 400.0], near: 16.0, far: 48000.0 } };
        // rear-view mirror: looking back, top centre
        let back = [-fwd[0], 0.0, -fwd[2]];
        let mirror = Camera { pos: add(cam, [0.0, 20.0, 0.0]), rot: look(back, -0.05), proj: Projection { center: [320.0, 46.0], scale: [150.0, 150.0], near: 16.0, far: 30000.0 } };
        let (main_vp, mirror_vp) = ([0.0, 0.0, 640.0, 480.0], [230.0, 14.0, 410.0, 78.0]);
        let sky = |c: &Camera, vp: [f32; 4], swap: bool| {
            (!self.off.sky).then(|| {
                let s = sky_for(c, vp);
                Sky { s: if swap { [s.s[1], s.s[0]] } else { s.s }, ..s }
            })
        };
        out.views.push(View { sky: sky(&main, main_vp, false), camera: main, viewport: main_vp, mirror: false, clear: Some([168, 192, 216, 255]), fog: Some(fog) });
        // the game hands the mirror its s pair swapped (the picture is flipped)
        out.views.push(View { sky: sky(&mirror, mirror_vp, true), camera: mirror, viewport: mirror_vp, mirror: true, clear: Some([0, 0, 0, 255]), fog: Some(fog) });

        // animation: the clock cycles in 8 s, the env phase in 4 s
        out.clock = (t * 2048.0) as u32 & 0x3fff;
        out.env_phase = (t * 16384.0) as u32 & 0xffff;

        // fades: boxes 0, 5, 10, ... blink (fully out, dithered, fully in); trees fade out
        // between 24000 and 30000 units from the camera
        if !self.off.fade {
            out.static_fade.extend(self.kinds.iter().zip(&self.centres).enumerate().map(|(i, (k, c))| match k {
                Kind::Box if i % 5 == 0 => (((t * 0.9 + i as f64 * 0.37).sin() * 1.4 + 0.5).clamp(0.0, 1.0) * 255.0) as u8,
                Kind::Tree => {
                    let d = ((c[0] - cam[0]).powi(2) + (c[2] - cam[2]).powi(2)).sqrt();
                    ((30000.0 - d) / 6000.0 * 255.0).clamp(0.0, 255.0) as u8
                }
                _ => 255,
            }));
        }

        let mut shadows = Mesh::default();
        let mut dark = Mesh::default();
        for (i, m) in self.movers.iter().enumerate() {
            let bob = (t * 1.7 + m.phase).sin() * 0.5 + 0.5;
            let lift = 60.0 + bob * 400.0;
            let origin = add(WORLD, [m.local[0], m.local[1] + lift + m.size[1] as f64 * 0.5, m.local[2]]);
            if m.chrome {
                // a mirror sphere moving along: unit basis, scroll from its motion (the game:
                // 0.008 x the displacement, rounded, & FFh)
                let travel = t * 900.0 + m.phase * 1000.0;
                let scroll = [((0.008 * travel).round() as i64 & 255) as f32, ((0.008 * lift * 4.0).round() as i64 & 255) as f32];
                out.dynamics.push(Instance { phase: 0, scroll, mesh: self.meshes.chrome[0], transform: Transform { basis: yaw_scale(t * m.spin, [1.0; 3]), origin: add(origin, [0.0, 120.0, 0.0]) } });
            } else {
                // each box its own animation phase (the boxes with the flag texture)
                let phase = ((t * 3000.0 + m.phase * 5000.0) as i64 & 0x3fff) as i32;
                out.dynamics.push(Instance { phase, scroll: [0.0; 2], mesh: m.mesh, transform: Transform { basis: yaw_scale(t * m.spin + m.phase, m.size), origin } });
            }
            // shadow on the ground, fainter as the box rises; every other one a multiply decal
            let multiply = !self.off.multiply && i % 2 == 1;
            let mesh = if multiply { &mut dark } else { &mut shadows };
            let base = mesh.vertices.len() as u32;
            let r = m.size[0] * (1.0 - 0.3 * bob as f32);
            let alpha = (200.0 * (1.0 - 0.6 * bob)) as u8;
            for (dx, dz, uv) in [(-1.0f32, -1.0f32, [0.0, 0.0]), (1.0, -1.0, [1.0, 0.0]), (1.0, 1.0, [1.0, 1.0]), (-1.0, 1.0, [0.0, 1.0])] {
                let (x, z) = (m.local[0] + (dx * r) as f64, m.local[2] + (dz * r) as f64);
                let color = if multiply { [255, 255, 255, 255] } else { [0, 0, 0, alpha] };
                mesh.vertices.push(v([x as f32, height(x, z) as f32, z as f32], uv, color));
            }
            mesh.indices.extend_from_slice(&[base, base + 3, base + 2, base, base + 2, base + 1]);
        }
        for (mut mesh, material) in [(shadows, mat::SHADOW), (dark, mat::MULTIPLY)] {
            let n = mesh.indices.len() as u32;
            if n == 0 {
                continue;
            }
            mesh.ranges.push(MeshRange { material, indices: 0..n });
            // vertices relative to the world's corner: small numbers, exact in f32
            out.meshes.push(FrameMesh { origin: WORLD, mesh });
        }
    }
}

/// The game's sky for a camera (`View::sky`): s from the heading (256 texels per 180
/// degrees, as the game), spanning the view's horizontal field over the viewport; t from
/// 255 at the horizon, 0.5 texel per game pixel above it (the game's slope); the horizon
/// where horizontal directions project; the ground colour below.
fn sky_for(c: &Camera, vp: [f32; 4]) -> Sky {
    // the view's forward in the world (row 2 of the rotation)
    let f = [c.rot[6] as f64, c.rot[7] as f64, c.rot[8] as f64];
    let heading = (-f[0]).atan2(f[2]);
    let p = &c.proj;
    let half_left = ((p.center[0] - vp[0]) / p.scale[0]) as f64;
    let half_right = ((vp[2] - p.center[0]) / p.scale[0]) as f64;
    let texels = |a: f64| a / std::f64::consts::PI * 256.0;
    let s = [texels(heading - half_left.atan()) as f32, texels(heading + half_right.atan()) as f32];
    // the horizon: a far point straight ahead at the camera's height
    let h = [f[0], 0.0, f[2]];
    let (y, z) = (c.rot[3] as f64 * h[0] + c.rot[5] as f64 * h[2], c.rot[6] as f64 * h[0] + c.rot[8] as f64 * h[2]);
    let horizon = p.center[1] + p.scale[1] * (y / z) as f32;
    Sky { texture: SKY_TEXTURE, s, t: [255.0 - 0.5 * (horizon - vp[1]), 255.0], horizon, ground: Some(0x405040) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_rotation_is_proper() {
        let r = look([0.6, 0.0, 0.8], -0.1);
        let row = |i: usize| [r[i * 3] as f64, r[i * 3 + 1] as f64, r[i * 3 + 2] as f64];
        let (x, y, z) = (row(0), row(1), row(2));
        let cross = [x[1] * y[2] - x[2] * y[1], x[2] * y[0] - x[0] * y[2], x[0] * y[1] - x[1] * y[0]];
        // right x down = forward, unit rows
        for k in 0..3 {
            assert!((cross[k] - z[k]).abs() < 1e-5);
        }
        assert!(y[1] < 0.0, "view y points down the world");
    }

    #[test]
    fn scene_is_consistent() {
        let t = TestScene::build();
        let s = &t.scene;
        for m in &s.meshes {
            assert_eq!(m.indices.len() % 3, 0);
            assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
            for r in &m.ranges {
                assert!((r.material as usize) < s.materials.len());
                assert!(r.indices.end as usize <= m.indices.len());
            }
        }
        for tex in &s.textures {
            assert_eq!(tex.pixels.len(), (tex.width * tex.height) as usize * tex.format.bytes_per_pixel());
        }
        assert!(s.statics.iter().all(|i| (i.mesh as usize) < s.meshes.len()));
        assert!(s.texture_frames.iter().all(|&f| (f as usize) < s.textures.len()));
        for m in &s.materials {
            if let Some(a) = m.anim {
                assert!((a.first + a.count) as usize <= s.texture_frames.len());
                assert_eq!(m.texture, Some(s.texture_frames[a.first as usize]));
            }
        }
        let mut f = FrameData::default();
        t.frame(12.5, &mut f);
        assert_eq!(f.views.len(), 2);
        assert!(f.dynamics.len() > 100);
        assert_eq!(f.static_fade.len(), s.statics.len());
        assert!(f.static_fade.iter().any(|&x| x == 0) && f.static_fade.iter().any(|&x| x > 0 && x < 255));
        let sky = f.views[0].sky.unwrap();
        // 77 degrees of view = 110 texels at 256 per 180 degrees
        assert!(sky.horizon > 0.0 && sky.horizon < 480.0 && (sky.s[1] - sky.s[0] - 110.0).abs() < 5.0, "{sky:?}");
        // the mirror's pair swapped
        let m = f.views[1].sky.unwrap();
        assert!(m.s[0] > m.s[1]);
        // features off
        let off = TestScene::build_with(Off { fade: true, anim: true, reflect: true, sky: true, multiply: true });
        let mut f = FrameData::default();
        off.frame(1.0, &mut f);
        assert!(f.static_fade.is_empty() && f.views[0].sky.is_none());
        assert!(off.scene.materials.iter().all(|m| m.anim.is_none() && m.texgen == TexGen::Vertex));
    }
}
