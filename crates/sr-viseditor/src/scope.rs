//! Which sectors a rectangle takes (an edit applies to the editing camera cell only).

use sr_runtime::visedit::{CELL_SIZE, Cell, GRID, WORLD_MIN};

/// The cells a world rectangle with corners `a` and `b` (x, z) touches, by row then column.
pub fn cells_in_rect(a: [f64; 2], b: [f64; 2]) -> Vec<Cell> {
    let k = |v: f64| ((v - WORLD_MIN) / CELL_SIZE).floor().clamp(0.0, (GRID - 1) as f64) as u16;
    let (c0, c1) = (k(a[0].min(b[0])), k(a[0].max(b[0])));
    let (r0, r1) = (k(a[1].min(b[1])), k(a[1].max(b[1])));
    (r0..=r1).flat_map(|r| (c0..=c1).map(move |c| (r, c))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_selection() {
        // one point: its cell
        assert_eq!(cells_in_rect([10.0, 10.0], [10.0, 10.0]), vec![(128, 128)]);
        // corners in either order, every cell touched
        let r = cells_in_rect([8192.0 * 2.5, -100.0], [-100.0, 8192.0 * 1.2]);
        assert_eq!(r.len(), 3 * 4);
        assert_eq!(r.first(), Some(&(127, 127)));
        assert_eq!(r.last(), Some(&(129, 130)));
        // clamped to the grid
        assert_eq!(cells_in_rect([-5.0e6, -5.0e6], [WORLD_MIN + 1.0, WORLD_MIN + 1.0]), vec![(0, 0)]);
    }
}
