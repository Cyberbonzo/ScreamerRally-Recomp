//! The map's view: which part of the world the map area shows, and the conversions between
//! the map area's pixels and the world's ground plane (x, z).
//!
//! The map is seen from above, x to the right and z up: the game's world is left-handed
//! (its view space is x left, y up, z back; `sr_runtime::native::frame`), so seen from above
//! with x to the right its z points up, and a car turning right turns clockwise on the map
//! (checked against the game, `docs/05-visibility-editor.md`).

use sr_runtime::visedit::{CELL_SIZE, Cell, GRID, WORLD_MIN};

/// Closest zoom: pixels per world unit (a cell 4 096 pixels wide).
pub const MAX_SCALE: f64 = 0.5;
/// Farthest zoom: the whole 256 x 256 grid in 100 pixels.
pub const MIN_SCALE: f64 = 100.0 / (GRID as f64 * CELL_SIZE);
/// One zoom step (wheel notch, `+` / `-`).
pub const ZOOM_STEP: f64 = 1.25;

/// The part of the world shown in the map area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapView {
    /// World (x, z) at the centre of the map area.
    pub center: [f64; 2],
    /// Pixels per world unit.
    pub scale: f64,
    /// The map area's size in pixels.
    pub size: [f64; 2],
}

impl Default for MapView {
    fn default() -> Self {
        MapView { center: [0.0, 0.0], scale: 1.0 / 64.0, size: [800.0, 600.0] }
    }
}

impl MapView {
    /// Map-area pixel (x right, y down) of world point (x, z).
    pub fn to_screen(&self, x: f64, z: f64) -> [f64; 2] {
        [(x - self.center[0]) * self.scale + self.size[0] * 0.5, self.size[1] * 0.5 - (z - self.center[1]) * self.scale]
    }

    /// World point (x, z) under map-area pixel (px, py).
    pub fn to_world(&self, px: f64, py: f64) -> [f64; 2] {
        [self.center[0] + (px - self.size[0] * 0.5) / self.scale, self.center[1] - (py - self.size[1] * 0.5) / self.scale]
    }

    /// The cell under map-area pixel (px, py) (`None`: outside the grid).
    pub fn cell_at(&self, px: f64, py: f64) -> Option<Cell> {
        let [x, z] = self.to_world(px, py);
        cell_at_world(x, z)
    }

    /// A cell's rectangle in map-area pixels: left, top, right, bottom.
    pub fn cell_rect(&self, c: Cell) -> [f64; 4] {
        let r = sr_runtime::visedit::cell_rect(c);
        let a = self.to_screen(r[0], r[3]);
        let b = self.to_screen(r[2], r[1]);
        [a[0], a[1], b[0], b[1]]
    }

    /// A cell's side in pixels.
    pub fn cell_px(&self) -> f64 {
        CELL_SIZE * self.scale
    }

    /// Zoom by `factor` (> 1: closer) keeping the world point under pixel (px, py) there.
    pub fn zoom_at(&mut self, px: f64, py: f64, factor: f64) {
        let w = self.to_world(px, py);
        self.scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        self.center = [w[0] - (px - self.size[0] * 0.5) / self.scale, w[1] + (py - self.size[1] * 0.5) / self.scale];
    }

    /// Move the picture by (dx, dy) pixels (a drag).
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.center[0] -= dx / self.scale;
        self.center[1] += dy / self.scale;
    }

    /// Show the world rectangle `b` (x0, z0, x1, z1) whole, centred, with a margin.
    pub fn fit(&mut self, b: [f64; 4]) {
        let (w, h) = ((b[2] - b[0]).max(CELL_SIZE), (b[3] - b[1]).max(CELL_SIZE));
        self.center = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5];
        self.scale = ((self.size[0] / w).min(self.size[1] / h) * 0.94).clamp(MIN_SCALE, MAX_SCALE);
    }

    /// The world rectangle shown: x0, z0, x1, z1.
    pub fn visible_world(&self) -> [f64; 4] {
        let a = self.to_world(0.0, self.size[1]);
        let b = self.to_world(self.size[0], 0.0);
        [a[0], a[1], b[0], b[1]]
    }

    /// The cells at least partly shown: first row, first column, last row, last column.
    pub fn visible_cells(&self) -> [u16; 4] {
        let w = self.visible_world();
        let k = |v: f64| ((v - WORLD_MIN) / CELL_SIZE).floor().clamp(0.0, (GRID - 1) as f64) as u16;
        [k(w[1]), k(w[0]), k(w[3]), k(w[2])]
    }

    /// Keep world point `target` in view: when it is outside the middle half of the map
    /// area, the view moves towards it, smoothly (`dt` seconds since the last step); far
    /// outside the map area it jumps there. Returns whether the view moved.
    pub fn follow(&mut self, target: [f64; 2], dt: f64) -> bool {
        let p = self.to_screen(target[0], target[1]);
        let (w, h) = (self.size[0], self.size[1]);
        if p[0] < -w || p[0] > 2.0 * w || p[1] < -h || p[1] > 2.0 * h {
            self.center = target;
            return true;
        }
        // how far outside the middle half (pixels)
        let out = |v: f64, len: f64| {
            let (lo, hi) = (len * 0.25, len * 0.75);
            if v < lo { v - lo } else if v > hi { v - hi } else { 0.0 }
        };
        let (ox, oy) = (out(p[0], w), out(p[1], h));
        if ox.abs() < 0.5 && oy.abs() < 0.5 {
            return false;
        }
        // most of the way within a quarter of a second; a whole pixel at least
        let k = 1.0 - (-dt.max(0.0) * 8.0).exp();
        let step = |o: f64| if o.abs() * k < 1.0 { o.signum() * o.abs().min(1.0) } else { o * k };
        self.pan(-step(ox), -step(oy));
        true
    }
}

/// The cell at world point (x, z) (`None`: outside the grid).
pub fn cell_at_world(x: f64, z: f64) -> Option<Cell> {
    let span = GRID as f64 * CELL_SIZE;
    if x < WORLD_MIN || z < WORLD_MIN || x >= WORLD_MIN + span || z >= WORLD_MIN + span {
        return None;
    }
    Some(sr_runtime::visedit::cell_of(x, z))
}

/// A cell's centre in the world (x, z).
pub fn cell_center(c: Cell) -> [f64; 2] {
    let r = sr_runtime::visedit::cell_rect(c);
    [(r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> MapView {
        MapView { center: [0.0, 0.0], scale: 1.0 / 64.0, size: [800.0, 600.0] }
    }

    #[test]
    fn screen_and_world_round_trip() {
        let v = view();
        // the centre of the map area is the view's centre
        assert_eq!(v.to_world(400.0, 300.0), [0.0, 0.0]);
        // x to the right, z up
        assert_eq!(v.to_world(464.0, 300.0), [64.0 * 64.0, 0.0]);
        assert_eq!(v.to_world(400.0, 236.0), [0.0, 64.0 * 64.0]);
        for (px, py) in [(0.0, 0.0), (123.5, 456.25), (799.0, 1.0)] {
            let w = v.to_world(px, py);
            let s = v.to_screen(w[0], w[1]);
            assert!((s[0] - px).abs() < 1e-9 && (s[1] - py).abs() < 1e-9);
        }
    }

    #[test]
    fn picking_cells() {
        let v = view();
        // world (0, 0) is the corner of cell (128, 128): just right of and above it
        assert_eq!(v.cell_at(400.5, 299.5), Some((128, 128)));
        assert_eq!(v.cell_at(399.5, 299.5), Some((128, 127)));
        assert_eq!(v.cell_at(400.5, 300.5), Some((127, 128)));
        // a cell is 128 pixels at this zoom; its rectangle contains the pixel picked
        let r = v.cell_rect((128, 128));
        assert_eq!(r, [400.0, 172.0, 528.0, 300.0]);
        assert_eq!(v.cell_px(), 128.0);
        // outside the grid
        let mut far = v;
        far.center = [-2.0e6, 0.0];
        assert_eq!(far.cell_at(400.0, 300.0), None);
        assert_eq!(cell_center((128, 128)), [4096.0, 4096.0]);
        assert_eq!(cell_at_world(WORLD_MIN, WORLD_MIN), Some((0, 0)));
        assert_eq!(cell_at_world(-WORLD_MIN, 0.0), None);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let mut v = view();
        let before = v.to_world(100.0, 500.0);
        v.zoom_at(100.0, 500.0, ZOOM_STEP);
        let after = v.to_world(100.0, 500.0);
        assert!((before[0] - after[0]).abs() < 1e-6 && (before[1] - after[1]).abs() < 1e-6);
        assert!((v.scale - ZOOM_STEP / 64.0).abs() < 1e-12);
        // limits
        for _ in 0..200 {
            v.zoom_at(10.0, 10.0, ZOOM_STEP);
        }
        assert_eq!(v.scale, MAX_SCALE);
        for _ in 0..400 {
            v.zoom_at(10.0, 10.0, 1.0 / ZOOM_STEP);
        }
        assert_eq!(v.scale, MIN_SCALE);
    }

    #[test]
    fn fit_shows_the_whole_rectangle() {
        let mut v = view();
        let b = [-100_000.0, 20_000.0, 300_000.0, 120_000.0];
        v.fit(b);
        let a = v.to_screen(b[0], b[1]);
        let c = v.to_screen(b[2], b[3]);
        assert!(a[0] >= 0.0 && c[0] <= 800.0 && c[1] >= 0.0 && a[1] <= 600.0, "{a:?} {c:?}");
        // the wider side nearly fills the width
        assert!(c[0] - a[0] > 700.0);
        let vis = v.visible_world();
        assert!(vis[0] <= b[0] && vis[2] >= b[2] && vis[1] <= b[1] && vis[3] >= b[3]);
        let cells = v.visible_cells();
        assert!(cells[0] <= sr_runtime::visedit::cell_of(0.0, b[1]).0 && cells[3] >= sr_runtime::visedit::cell_of(b[2], 0.0).1);
    }

    #[test]
    fn pan_and_follow() {
        let mut v = view();
        v.pan(64.0, 0.0);
        // dragging right shows what is left
        assert_eq!(v.center, [-64.0 * 64.0, 0.0]);
        let mut v = view();
        // inside the middle half: nothing moves
        assert!(!v.follow([5000.0, 5000.0], 0.1));
        // near the right edge: the view moves towards it, never past
        let target = [380.0 * 64.0, 0.0];
        let mut moves = 0;
        while v.follow(target, 0.05) {
            moves += 1;
            assert!(moves < 100);
        }
        let p = v.to_screen(target[0], target[1]);
        assert!(p[0] <= 600.5 && p[0] > 590.0, "{p:?}");
        // far away: a jump
        assert!(v.follow([500_000.0, -400_000.0], 0.05));
        assert_eq!(v.center, [500_000.0, -400_000.0]);
    }
}
