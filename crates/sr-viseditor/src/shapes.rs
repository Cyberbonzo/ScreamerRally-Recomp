//! Flat-coloured shapes over the map (outlines, grid, rectangle, car arrow), as triangles in
//! window pixels.

/// A vertex of the shapes: window pixel and straight RGBA colour (16 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct ShapeVertex {
    pub pos: [f32; 2],
    pub color: [u8; 4],
    pub pad: u32,
}

#[derive(Default)]
pub struct Shapes {
    pub v: Vec<ShapeVertex>,
}

impl Shapes {
    pub fn tri(&mut self, a: [f64; 2], b: [f64; 2], c: [f64; 2], color: [u8; 4]) {
        for p in [a, b, c] {
            self.v.push(ShapeVertex { pos: [p[0] as f32, p[1] as f32], color, pad: 0 });
        }
    }

    pub fn quad(&mut self, a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2], color: [u8; 4]) {
        self.tri(a, b, c, color);
        self.tri(a, c, d, color);
    }

    /// A filled rectangle: left, top, right, bottom.
    pub fn rect(&mut self, r: [f64; 4], color: [u8; 4]) {
        self.quad([r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]], color);
    }

    /// A frame `width` pixels wide inside the rectangle.
    pub fn frame(&mut self, r: [f64; 4], width: f64, color: [u8; 4]) {
        let w = width.min((r[2] - r[0]) * 0.5).min((r[3] - r[1]) * 0.5).max(0.0);
        self.rect([r[0], r[1], r[2], r[1] + w], color);
        self.rect([r[0], r[3] - w, r[2], r[3]], color);
        self.rect([r[0], r[1] + w, r[0] + w, r[3] - w], color);
        self.rect([r[2] - w, r[1] + w, r[2], r[3] - w], color);
    }

    /// A line `width` pixels wide.
    pub fn line(&mut self, a: [f64; 2], b: [f64; 2], width: f64, color: [u8; 4]) {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-9 {
            return;
        }
        let (nx, ny) = (-dy / len * width * 0.5, dx / len * width * 0.5);
        self.quad([a[0] + nx, a[1] + ny], [b[0] + nx, b[1] + ny], [b[0] - nx, b[1] - ny], [a[0] - nx, a[1] - ny], color);
    }

    /// The car: an arrow at `p` pointing along `dir` (window pixels, y down), `len` long, with
    /// a dark edge.
    pub fn arrow(&mut self, p: [f64; 2], dir: [f64; 2], len: f64, fill: [u8; 4], edge: [u8; 4]) {
        let n = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt();
        let (fx, fy) = if n > 1e-9 { (dir[0] / n, dir[1] / n) } else { (0.0, -1.0) };
        let (sx, sy) = (-fy, fx);
        let pts = |l: f64| {
            let tip = [p[0] + fx * l * 0.62, p[1] + fy * l * 0.62];
            let back = [p[0] - fx * l * 0.38, p[1] - fy * l * 0.38];
            let notch = [p[0] - fx * l * 0.12, p[1] - fy * l * 0.12];
            let left = [back[0] + sx * l * 0.3, back[1] + sy * l * 0.3];
            let right = [back[0] - sx * l * 0.3, back[1] - sy * l * 0.3];
            (tip, left, notch, right)
        };
        let (t, l, nn, r) = pts(len * 1.3);
        self.tri(t, l, nn, edge);
        self.tri(t, nn, r, edge);
        let (t, l, nn, r) = pts(len);
        self.tri(t, l, nn, fill);
        self.tri(t, nn, r, fill);
    }

    /// A wedge (the camera's view): from `p` along `dir`, `len` long, `half` radians each side.
    pub fn wedge(&mut self, p: [f64; 2], dir: [f64; 2], len: f64, half: f64, fill: [u8; 4], edge: [u8; 4]) {
        let base = dir[1].atan2(dir[0]);
        let steps = 8;
        let at = |a: f64| [p[0] + a.cos() * len, p[1] + a.sin() * len];
        for i in 0..steps {
            let a0 = base - half + 2.0 * half * i as f64 / steps as f64;
            let a1 = base - half + 2.0 * half * (i + 1) as f64 / steps as f64;
            self.tri(p, at(a0), at(a1), fill);
        }
        self.line(p, at(base - half), 1.5, edge);
        self.line(p, at(base + half), 1.5, edge);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_make_triangles() {
        let mut s = Shapes::default();
        s.rect([0.0, 0.0, 10.0, 10.0], [255; 4]);
        assert_eq!(s.v.len(), 6);
        s.frame([0.0, 0.0, 10.0, 10.0], 2.0, [255; 4]);
        assert_eq!(s.v.len(), 6 + 24);
        s.line([0.0, 0.0], [0.0, 0.0], 1.0, [255; 4]);
        assert_eq!(s.v.len(), 30, "a zero-length line draws nothing");
        // the arrow's tip is ahead of the point along the direction (y down: up the screen)
        let mut a = Shapes::default();
        a.arrow([100.0, 100.0], [0.0, -1.0], 20.0, [255; 4], [0, 0, 0, 255]);
        let min_y = a.v.iter().map(|v| v.pos[1]).fold(f32::MAX, f32::min);
        assert!(min_y < 90.0, "{min_y}");
        assert_eq!(std::mem::size_of::<ShapeVertex>(), 16);
    }
}
