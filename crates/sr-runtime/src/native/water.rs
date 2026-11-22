//! The game's fake water reflections, for the native renderer (`docs/re/native/water.md`).
//!
//! China's lake and Arizona's river show what stands on their banks reflected in the water
//! with **reflection cards**: flat polygons painted with the reflected picture, hanging under
//! the water's surface (China `areflex`, one under each group of boats; Arizona `a(051lk` ..
//! `a(058lk`, in pairs one under the other below the canyon's wall cards `a(031lk` ..). The
//! game draws without a depth buffer, polygons sorted back to front (Arizona's cards with a
//! sort bias, `world.md` +C4h): a card comes after the water, over it, and so does the
//! submerged foot of a flat object standing in the water (Arizona's wall cards reach a little
//! under the river, China's tree screen and reeds stand in the lake). With a depth buffer
//! the water in front hides them. Here the water's materials become [`Layer::Water`]; a card's
//! become [`Layer::Reflection`] (drawn only over the water's pixels, whatever the depth); a
//! standing object keeps its own and gets a copy of its faces as `Layer::Reflection`, so its
//! foot shows on the water as in the game.
//!
//! Found from the data: the water is the `lag` family (the designers' "lago": China `a)201lag`
//! .., Arizona `a)601lag` ..; not China's flags `a1flag`); then among the other statics whose
//! faces lie in one plane and that have water under them (the water's height there from the
//! water's triangles, at the corners and middle of their top edge): a card hangs at least
//! [`DEPTH`] below that water and reaches at most [`ABOVE`] above it; a standing object
//! reaches below it otherwise. The lake's water pieces are not level (China's slope by up to
//! 500 units under one card), so the cards' tops are near the water, not on it.

use sr_scene::{Instance, Layer, SceneData};

/// A card hangs at least this far below the water over its top edge ...
pub const DEPTH: f64 = 1000.0;
/// ... and reaches at most this far above it (China's cards' tops: up to 346 above the water
/// polygons under them, which slope; the reeds and trees standing in the lake reach higher
/// or end above the water).
pub const ABOVE: f64 = 1000.0;
/// Faces within this angle of one plane (either side) make a flat object.
const FLAT_DEGREES: f64 = 10.0;

/// A static shown on the water: a reflection card, or a flat object standing in the water.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Card {
    pub static_index: usize,
    /// The water's world height over it.
    pub water: f64,
    /// Standing in the water (drawn as itself, its faces also over the water), not a card.
    pub standing: bool,
}

/// The water statics and the statics shown on the water (`names`: each static's mesh name).
pub fn find(scene: &SceneData, names: &[&str]) -> (Vec<usize>, Vec<Card>) {
    let water: Vec<usize> = (0..scene.statics.len()).filter(|&i| super::objects::family_of(names[i]).0 == "lag").collect();
    if water.is_empty() {
        return (water, Vec::new());
    }
    let water_tris: Vec<[[f64; 3]; 3]> = water.iter().flat_map(|&i| super::objects::world_tris(scene, &scene.statics[i])).map(|t| t.1).collect();
    let mut cards = Vec::new();
    for i in 0..scene.statics.len() {
        if water.contains(&i) {
            continue;
        }
        if let Some(c) = card(scene, &scene.statics[i], &water_tris) {
            cards.push(Card { static_index: i, ..c });
        }
    }
    (water, cards)
}

/// A flat static shown on the water: a card hanging under it or an object standing in it
/// (`static_index` left 0).
fn card(scene: &SceneData, inst: &Instance, water: &[[[f64; 3]; 3]]) -> Option<Card> {
    let tris = super::objects::world_tris(scene, inst);
    if tris.is_empty() {
        return None;
    }
    let normal = |t: &[[f64; 3]; 3]| {
        let (u, v) = ([0, 1, 2].map(|k| t[1][k] - t[0][k]), [0, 1, 2].map(|k| t[2][k] - t[0][k]));
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        (l > 0.0).then(|| n.map(|c| c / l))
    };
    let ns: Vec<[f64; 3]> = tris.iter().filter_map(|t| normal(&t.1)).collect();
    let n0 = *ns.first()?;
    let cos = FLAT_DEGREES.to_radians().cos();
    if !ns.iter().all(|n| (n[0] * n0[0] + n[1] * n0[1] + n[2] * n0[2]).abs() >= cos) {
        return None;
    }
    let pts: Vec<[f64; 3]> = tris.iter().flat_map(|t| t.1).collect();
    let top = pts.iter().map(|p| p[1]).fold(f64::MIN, f64::max);
    let bottom = pts.iter().map(|p| p[1]).fold(f64::MAX, f64::min);
    // the water over the top edge (its corners and middle)
    let edge: Vec<[f64; 3]> = pts.iter().filter(|p| p[1] >= top - 1.0).copied().collect();
    let mid = [0, 1, 2].map(|k| edge.iter().map(|p| p[k]).sum::<f64>() / edge.len() as f64);
    let h = edge.iter().chain(std::iter::once(&mid)).filter_map(|p| height_at(water, p[0], p[2])).fold(None, |a: Option<f64>, h| Some(a.map_or(h, |a| a.max(h))))?;
    if top <= h + ABOVE && bottom <= h - DEPTH {
        Some(Card { static_index: 0, water: h, standing: false })
    } else {
        (bottom < h).then_some(Card { static_index: 0, water: h, standing: true })
    }
}

/// The highest water triangle's height at (x, z), if one lies over that point.
fn height_at(water: &[[[f64; 3]; 3]], x: f64, z: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    for t in water {
        let (a, b, c) = (t[0], t[1], t[2]);
        let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
        if d.abs() < 1e-9 {
            continue;
        }
        let w0 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / d;
        let w1 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / d;
        let w2 = 1.0 - w0 - w1;
        if w0 >= -1e-6 && w1 >= -1e-6 && w2 >= -1e-6 {
            let y = w0 * a[1] + w1 * b[1] + w2 * c[1];
            best = Some(best.map_or(y, |h| h.max(y)));
        }
    }
    best
}

/// `m` as a water surface's material.
pub fn water_material(m: &sr_scene::Material) -> sr_scene::Material {
    sr_scene::Material { layer: Layer::Water, ..m.clone() }
}

/// `m` as a reflection card's material.
pub fn reflection_material(m: &sr_scene::Material) -> sr_scene::Material {
    sr_scene::Material { layer: Layer::Reflection, ..m.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sr_scene::{Material, Mesh, MeshRange, Transform, Vertex};

    fn mesh(tris: &[[[f32; 3]; 3]]) -> Mesh {
        let mut m = Mesh::default();
        for t in tris {
            for p in t {
                m.indices.push(m.vertices.len() as u32);
                m.vertices.push(Vertex { pos: *p, ..Default::default() });
            }
        }
        m.ranges.push(MeshRange { material: 0, indices: 0..m.indices.len() as u32 });
        m
    }

    /// A water quad at height 200 over x, z in 0..1000; a card hanging under it, a card
    /// standing in it (reeds), an object under it that is not flat, a card named like a flag
    /// (`lag` in the name, not the family: not water) and a card above the water: the hanging
    /// cards are cards, the reeds stand in it, all at the water's height.
    #[test]
    fn finds_the_cards_under_the_water() {
        let mut s = SceneData::default();
        s.materials.push(Material::default());
        let water = mesh(&[[[0.0, 200.0, 0.0], [1000.0, 200.0, 0.0], [1000.0, 200.0, 1000.0]], [[0.0, 200.0, 0.0], [1000.0, 200.0, 1000.0], [0.0, 200.0, 1000.0]]]);
        let card = |top: f32| mesh(&[[[400.0, top, 500.0], [600.0, top, 500.0], [600.0, top - 3000.0, 500.0]], [[400.0, top, 500.0], [600.0, top - 3000.0, 500.0], [400.0, top - 3000.0, 500.0]]]);
        let reeds = mesh(&[[[400.0, 150.0, 500.0], [600.0, 150.0, 500.0], [600.0, 900.0, 500.0]], [[400.0, 150.0, 500.0], [600.0, 900.0, 500.0], [400.0, 900.0, 500.0]]]);
        let lump = mesh(&[[[400.0, 0.0, 500.0], [600.0, 0.0, 500.0], [500.0, 100.0, 600.0]], [[400.0, 0.0, 500.0], [500.0, 100.0, 600.0], [500.0, 50.0, 400.0]]]);
        let above = mesh(&[[[400.0, 300.0, 500.0], [600.0, 300.0, 500.0], [600.0, 900.0, 500.0]], [[400.0, 300.0, 500.0], [600.0, 900.0, 500.0], [400.0, 900.0, 500.0]]]);
        for m in [water, card(230.0), reeds, lump, card(150.0), above] {
            s.statics.push(Instance { mesh: s.meshes.len() as u32, transform: Transform::at([0.0; 3]), phase: 0, scroll: [0.0; 2] });
            s.meshes.push(m);
        }
        let names = ["a)201lag", "areflex", "a2giunco", "aboat", "a1flag", "a1sign"];
        let (w, cards) = find(&s, &names);
        assert_eq!(w, vec![0]);
        assert_eq!(cards.iter().map(|c| (c.static_index, c.standing)).collect::<Vec<_>>(), vec![(1, false), (2, true), (4, false)]);
        assert!(cards.iter().all(|c| (c.water - 200.0).abs() < 1e-6));
    }
}
