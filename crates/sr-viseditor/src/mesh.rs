//! The map's picture on the CPU: the track's triangles as one vertex list, in draw batches
//! by texture, and the textures as the map draws them (chroma key, mipmaps).

use std::collections::HashMap;

use sr_runtime::visedit::{GRID, MapTexture, TrackMap};

/// A vertex of the map (28 bytes; the layout `gpu::Renderer` declares).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct MapVertex {
    /// World position minus [`MapMesh::origin`] (y up).
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub color: [u8; 4],
    /// The static's cell as `row * 256 + column` (its sector's state tints it).
    pub cell: u32,
}

/// How a texture is used: as the game's materials draw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    /// Opaque: the texel alpha is ignored.
    Opaque,
    /// Chroma key `0xRRGGBB`: texels of that colour are holes.
    Keyed(u32),
    /// Blended with what is under it: the texel alpha counts.
    Blended,
}

/// Triangles drawn with one texture variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Batch {
    /// Index into [`MapMesh::textures`]; `None`: vertex colour only.
    pub texture: Option<usize>,
    pub keyed: bool,
    pub blended: bool,
    /// First vertex and vertex count.
    pub start: u32,
    pub count: u32,
}

/// The map's triangles ready for the GPU.
#[derive(Clone, Debug, Default)]
pub struct MapMesh {
    pub vertices: Vec<MapVertex>,
    /// Opaque batches first, then the blended ones.
    pub batches: Vec<Batch>,
    /// The textures the batches use: (index into `TrackMap::textures`, variant).
    pub textures: Vec<(u32, Variant)>,
    /// Subtracted from every position (f32 precision near the track).
    pub origin: [f64; 3],
    /// Lowest and highest y (relative to `origin`): the depth range.
    pub y_range: [f32; 2],
}

/// The map's triangles grouped in batches.
pub fn build(map: &TrackMap) -> MapMesh {
    let origin = [((map.bounds[0] + map.bounds[2]) * 0.5) as f64, 0.0, ((map.bounds[1] + map.bounds[3]) * 0.5) as f64];
    // (blended, texture variant) -> triangles
    let mut groups: HashMap<(bool, Option<(u32, Variant)>), Vec<usize>> = HashMap::new();
    let mut y_range = [f32::MAX, f32::MIN];
    for (i, t) in map.tris.iter().enumerate() {
        let m = map.materials.get(t.material as usize).copied().unwrap_or_default();
        let tex = m.texture.filter(|&k| (k as usize) < map.textures.len());
        let variant = match (m.key, m.blended) {
            (Some(k), _) => Variant::Keyed(k & 0xff_ffff),
            (None, true) => Variant::Blended,
            (None, false) => Variant::Opaque,
        };
        groups.entry((m.blended && m.key.is_none(), tex.map(|k| (k, variant)))).or_default().push(i);
        for p in &t.pos {
            y_range[0] = y_range[0].min(p[1]);
            y_range[1] = y_range[1].max(p[1]);
        }
    }
    if y_range[0] > y_range[1] {
        y_range = [0.0, 1.0];
    }
    // opaque first; then by texture for a stable order
    let mut keys: Vec<_> = groups.keys().copied().collect();
    keys.sort_by_key(|(blended, tex)| (*blended, tex.map(|t| (t.0, format!("{:?}", t.1)))));
    let mut mesh = MapMesh { origin, y_range, ..Default::default() };
    let mut tex_index: HashMap<(u32, Variant), usize> = HashMap::new();
    let statics_cell = |s: u32| map.statics.get(s as usize).map_or(0, |st| st.cell.0 as u32 * GRID as u32 + st.cell.1 as u32);
    for key in keys {
        let tris = &groups[&key];
        let texture = key.1.map(|tv| {
            *tex_index.entry(tv).or_insert_with(|| {
                mesh.textures.push(tv);
                mesh.textures.len() - 1
            })
        });
        let start = mesh.vertices.len() as u32;
        for &i in tris {
            let t = &map.tris[i];
            let cell = statics_cell(t.statik);
            for k in 0..3 {
                let p = t.pos[k];
                mesh.vertices.push(MapVertex {
                    pos: [(p[0] as f64 - origin[0]) as f32, p[1], (p[2] as f64 - origin[2]) as f32],
                    uv: t.uv[k],
                    color: t.color[k],
                    cell,
                });
            }
        }
        mesh.batches.push(Batch { texture, keyed: matches!(key.1, Some((_, Variant::Keyed(_)))), blended: key.0, start, count: mesh.vertices.len() as u32 - start });
    }
    mesh
}

/// One mip level: width, height, texels (`r | g << 8 | b << 16 | a << 24`).
pub type Level = (u32, u32, Vec<u32>);

/// Every mip level of a texture as `variant` draws it, largest first, down to 1 x 1.
/// Chroma-keyed texels get alpha 0 and their opaque neighbours' colour (no fringes when
/// filtered); smaller levels average the texels by their alpha.
pub fn texture_levels(t: &MapTexture, variant: Variant) -> Vec<Level> {
    let (w, h) = (t.width.max(1), t.height.max(1));
    let mut px: Vec<u32> = (0..(w * h) as usize).map(|i| t.rgba.get(i).copied().unwrap_or(0)).collect();
    match variant {
        Variant::Opaque => px.iter_mut().for_each(|c| *c |= 0xff00_0000),
        Variant::Keyed(key) => apply_key(&mut px, w, h, key),
        Variant::Blended => {}
    }
    let mut levels = vec![(w, h, px)];
    while let Some((lw, lh, _)) = levels.last() {
        if *lw == 1 && *lh == 1 {
            break;
        }
        let next = downsample(levels.last().unwrap());
        levels.push(next);
    }
    levels
}

fn chan(c: u32, k: u32) -> u32 {
    (c >> (8 * k)) & 0xff
}

/// Keyed texels: alpha 0, the average colour of their opaque neighbours (else of the whole
/// texture's); the others opaque (the game's chroma key ignores the texel alpha).
fn apply_key(px: &mut [u32], w: u32, h: u32, key: u32) {
    let key_rgb = ((key >> 16) & 0xff) | (key & 0xff00) | (key & 0xff) << 16;
    let keyed: Vec<bool> = px.iter().map(|&c| c & 0x00ff_ffff == key_rgb).collect();
    let mut all = [0u64; 4];
    for (c, k) in px.iter().zip(&keyed) {
        if !k {
            for (i, a) in all.iter_mut().take(3).enumerate() {
                *a += chan(*c, i as u32) as u64;
            }
            all[3] += 1;
        }
    }
    let fallback = if all[3] > 0 { (0..3).fold(0u32, |acc, i| acc | ((all[i] / all[3]) as u32) << (8 * i)) } else { 0 };
    let src = px.to_vec();
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let i = (y * w as i64 + x) as usize;
            if !keyed[i] {
                px[i] = src[i] | 0xff00_0000;
                continue;
            }
            let mut sum = [0u32; 4];
            for dy in -1..=1i64 {
                for dx in -1..=1i64 {
                    let (nx, ny) = ((x + dx).rem_euclid(w as i64), (y + dy).rem_euclid(h as i64));
                    let j = (ny * w as i64 + nx) as usize;
                    if !keyed[j] {
                        for (k, s) in sum.iter_mut().take(3).enumerate() {
                            *s += chan(src[j], k as u32);
                        }
                        sum[3] += 1;
                    }
                }
            }
            px[i] = if sum[3] > 0 { (0..3).fold(0u32, |acc, k| acc | (sum[k] / sum[3]) << (8 * k)) } else { fallback };
        }
    }
}

/// Half the size (at least 1), each texel the alpha-weighted average of the 2 x 2 below it.
fn downsample((w, h, px): &Level) -> Level {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = Vec::with_capacity((nw * nh) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let mut rgb = [0u32; 3];
            let (mut a, mut plain, mut n) = (0u32, [0u32; 3], 0u32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = ((2 * x + dx).min(w - 1), (2 * y + dy).min(h - 1));
                let c = px[(sy * w + sx) as usize];
                let ca = chan(c, 3);
                for k in 0..3 {
                    rgb[k] += chan(c, k as u32) * ca;
                    plain[k] += chan(c, k as u32);
                }
                a += ca;
                n += 1;
            }
            let colour = |k: usize| if a > 0 { (rgb[k] + a / 2) / a } else { (plain[k] + n / 2) / n };
            out.push(colour(0) | colour(1) << 8 | colour(2) << 16 | ((a + n / 2) / n) << 24);
        }
    }
    (nw, nh, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sr_runtime::visedit::{MapMaterial, MapStatic, MapTri};

    #[test]
    fn batches_by_texture_opaque_first() {
        let tri = |material: u32, y: f32, statik: u32| MapTri { pos: [[0.0, y, 0.0], [100.0, y, 0.0], [0.0, y, 100.0]], material, statik, ..Default::default() };
        let map = TrackMap {
            textures: vec![MapTexture { width: 1, height: 1, rgba: vec![0xff00_00ff] }; 2],
            materials: vec![
                MapMaterial { texture: Some(0), key: None, blended: false },
                MapMaterial { texture: Some(1), key: Some(0xff00ff), blended: false },
                MapMaterial { texture: Some(0), key: None, blended: true },
                MapMaterial { texture: None, key: None, blended: false },
                MapMaterial { texture: Some(9), key: None, blended: false }, // no such texture
            ],
            tris: vec![tri(2, 5.0, 0), tri(0, 1.0, 1), tri(1, -3.0, 1), tri(0, 2.0, 0), tri(3, 0.0, 0), tri(4, 0.0, 0)],
            statics: vec![MapStatic { cell: (3, 4), ..Default::default() }, MapStatic { cell: (200, 7), ..Default::default() }],
            bounds: [0.0, 0.0, 100.0, 100.0],
            ..Default::default()
        };
        let m = build(&map);
        assert_eq!(m.vertices.len(), 18);
        assert_eq!(m.origin, [50.0, 0.0, 50.0]);
        assert_eq!(m.y_range, [-3.0, 5.0]);
        // untextured (two: material 3 and the one with a missing texture), texture 0, the
        // keyed texture 1, then the blended one
        let b: Vec<(Option<usize>, bool, bool, u32)> = m.batches.iter().map(|b| (b.texture, b.keyed, b.blended, b.count)).collect();
        assert_eq!(b.len(), 4);
        assert_eq!(b[0], (None, false, false, 6));
        assert_eq!(b.last().unwrap().2, true);
        assert!(b.iter().any(|x| x.1 && x.3 == 3));
        assert_eq!(m.batches.iter().map(|b| b.count).sum::<u32>(), 18);
        // texture variants: 0 opaque, 1 keyed, 0 blended
        assert_eq!(m.textures.len(), 3);
        assert!(m.textures.contains(&(1, Variant::Keyed(0xff00ff))) && m.textures.contains(&(0, Variant::Blended)));
        // positions relative to the origin, cells packed
        let v = m.vertices[0];
        assert_eq!(v.pos[0], -50.0);
        assert!(m.vertices.iter().any(|v| v.cell == 200 * 256 + 7) && m.vertices.iter().any(|v| v.cell == 3 * 256 + 4));
    }

    #[test]
    fn keyed_textures_and_mipmaps() {
        // 2 x 2: red, key (magenta), blue, green; alpha 0 in the source must not matter
        let key = 0xff00ff;
        let t = MapTexture { width: 2, height: 2, rgba: vec![0x0000_00ff, 0xffff_00ff, 0xffff_0000, 0xff00_ff00] };
        let l = texture_levels(&t, Variant::Keyed(key));
        assert_eq!(l.len(), 2);
        let px = &l[0].2;
        assert_eq!(px[0], 0xff00_00ff, "opaque texels get alpha 255");
        assert_eq!(px[1] >> 24, 0, "the key is a hole");
        // its colour: its opaque neighbours' average (3 x 3, wrapping round the 2 x 2: red
        // twice, green twice, blue four times): no magenta fringe
        assert_eq!(px[1] & 0xff_ffff, 63 | 63 << 8 | 127 << 16);
        // the 1 x 1 level: the opaque texels' average colour, alpha 3/4
        let top = l[1].2[0];
        assert_eq!(top & 0xff_ffff, 0x55 | 0x55 << 8 | 0x55 << 16);
        assert_eq!(top >> 24, (3 * 255 + 2) / 4);
        // opaque: alpha forced; levels down to 1 x 1 for odd sizes too
        let t = MapTexture { width: 5, height: 3, rgba: vec![0x0012_3456; 15] };
        let l = texture_levels(&t, Variant::Opaque);
        assert_eq!(l.iter().map(|l| (l.0, l.1)).collect::<Vec<_>>(), vec![(5, 3), (2, 1), (1, 1)]);
        assert!(l.iter().all(|l| l.2.iter().all(|&c| c == 0xff12_3456)));
        // blended: the alpha stays
        let t = MapTexture { width: 1, height: 1, rgba: vec![0x8012_3456] };
        assert_eq!(texture_levels(&t, Variant::Blended)[0].2[0], 0x8012_3456);
    }
}
