//! The scenery's pieces as the designers named them: the world's objects are pieces (one
//! model each) named after the object they belong to (`a12tian`, `a3casa`, `a)012for`:
//! families `tian`, `casa`, `for`; China's lake pieces `lag`); terrain and road pieces are
//! numbered (`ad999s`, `ar1000s`, `ax12s`, `ac7s`). Used by the water's reflections
//! (`native::water`).

use sr_scene::{Instance, SceneData};

/// A piece's family from its name, and whether it is a numbered terrain or road piece
/// (`a` + one of `xdrmc` + digits + `s`; the family is then "terrain").
pub fn family_of(name: &str) -> (String, bool) {
    if numbered(name).is_some() {
        return ("terrain".to_string(), true);
    }
    let core = name.strip_prefix('a').unwrap_or(name).trim_start_matches(|c: char| !c.is_ascii_alphabetic()).trim_end_matches(|c: char| c.is_ascii_digit());
    (if core.is_empty() { name.to_string() } else { core.to_string() }, false)
}

/// The letter of a numbered piece (`ax12s` -> `x`: road surface; `d`, `r`, `m`: verges and
/// edges; `c`: the large rock walls beside the track), `None` for a named piece.
pub fn numbered(name: &str) -> Option<u8> {
    let by = name.as_bytes();
    (by.len() >= 4 && by[0] == b'a' && b"xdrmc".contains(&by[1]) && name.ends_with('s') && name[2..name.len() - 1].bytes().all(|c| c.is_ascii_digit())).then_some(by[1])
}

/// A static's triangles in world coordinates, with their materials.
pub fn world_tris(scene: &SceneData, inst: &Instance) -> Vec<(u32, [[f64; 3]; 3])> {
    let me = &scene.meshes[inst.mesh as usize];
    let t = &inst.transform;
    let w = |q: [f32; 3]| [0, 1, 2].map(|k| t.basis[k * 3] as f64 * q[0] as f64 + t.basis[k * 3 + 1] as f64 * q[1] as f64 + t.basis[k * 3 + 2] as f64 * q[2] as f64 + t.origin[k]);
    let mut out = Vec::new();
    for r in &me.ranges {
        for tr in me.indices[r.indices.start as usize..r.indices.end as usize].chunks_exact(3) {
            out.push((r.material, [0, 1, 2].map(|k| w(me.vertices[tr[k] as usize].pos))));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_from_piece_names() {
        assert_eq!(family_of("a12tian"), ("tian".to_string(), false));
        assert_eq!(family_of("a3casa"), ("casa".to_string(), false));
        assert_eq!(family_of("a)012for"), ("for".to_string(), false));
        assert_eq!(family_of("ax12s"), ("terrain".to_string(), true));
        assert_eq!(numbered("ac7s"), Some(b'c'));
        assert_eq!(numbered("ax12s"), Some(b'x'));
        assert_eq!(numbered("acasa"), None);
        assert_eq!(numbered("a3casa"), None);
    }
}
