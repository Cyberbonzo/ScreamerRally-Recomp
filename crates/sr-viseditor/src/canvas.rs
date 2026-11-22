//! A small software canvas painted in "dots" with the game overlay's pixel font
//! (`sr_render::pixel_font`) and colours, shown at an integer scale (crisp, like the F12
//! settings overlay).

use sr_render::pixel_font::{self as font, ADVANCE};

/// Straight (not premultiplied) colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

// The settings overlay's palette (sr_render::overlay).
pub const PANEL: Rgba = Rgba(8, 10, 16, 255);
pub const BORDER: Rgba = Rgba(66, 74, 92, 255);
pub const WHITE: Rgba = Rgba(255, 255, 255, 255);
pub const MUTED: Rgba = Rgba(120, 127, 140, 255);
pub const ACCENT: Rgba = Rgba(120, 186, 255, 255);
pub const TAB_BOX: Rgba = Rgba(110, 170, 255, 48);
pub const SEPARATOR: Rgba = Rgba(54, 60, 75, 255);
pub const SECTION: Rgba = Rgba(236, 239, 244, 255);
pub const SECTION_LINE: Rgba = Rgba(46, 52, 66, 255);
pub const LABEL: Rgba = Rgba(190, 196, 205, 255);
pub const LABEL_OFF: Rgba = Rgba(96, 102, 112, 255);
pub const VALUE: Rgba = Rgba(120, 186, 255, 255);
pub const BAR: Rgba = Rgba(120, 170, 255, 40);
pub const HINT: Rgba = Rgba(255, 212, 84, 255);
pub const SAVED: Rgba = Rgba(120, 228, 120, 255);
pub const FAILED: Rgba = Rgba(255, 128, 96, 255);
pub const LABEL_BG: Rgba = Rgba(0, 0, 0, 170);

/// A premultiplied RGBA image (bytes r, g, b, a) painted in dots.
#[derive(Clone, Debug)]
pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub px: Vec<[u8; 4]>,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Canvas {
        let (w, h) = (w.max(1), h.max(1));
        Canvas { w, h, px: vec![[0; 4]; (w * h) as usize] }
    }

    /// Paint one dot over what is there.
    pub fn dot(&mut self, x: i32, y: i32, c: Rgba) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let d = &mut self.px[(y * self.w + x) as usize];
        let (a, ia) = (c.3 as u32, 255 - c.3 as u32);
        let over = |s: u8, d: u8| ((s as u32 * a + d as u32 * ia + 127) / 255) as u8;
        *d = [over(c.0, d[0]), over(c.1, d[1]), over(c.2, d[2]), ((255 * a + d[3] as u32 * ia + 127) / 255) as u8];
    }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                self.dot(xx, yy, c);
            }
        }
    }

    /// A one-dot frame inside the rectangle.
    pub fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        self.fill(x, y, w, 1, c);
        self.fill(x, y + h - 1, w, 1, c);
        self.fill(x, y + 1, 1, h - 2, c);
        self.fill(x + w - 1, y + 1, 1, h - 2, c);
    }

    /// `s` with its top-left corner at (x, y).
    pub fn text(&mut self, x: i32, y: i32, s: &str, c: Rgba) {
        for (i, ch) in s.chars().enumerate() {
            let g = font::glyph(ch);
            let gx = x + i as i32 * ADVANCE;
            for (r, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        self.dot(gx + col, y + r as i32, c);
                    }
                }
            }
        }
    }

    /// `s` ending at x (right-aligned).
    pub fn text_right(&mut self, x: i32, y: i32, s: &str, c: Rgba) {
        self.text(x - font::width(s), y, s, c);
    }

    /// `s` centred on x.
    pub fn text_centred(&mut self, x: i32, y: i32, s: &str, c: Rgba) {
        self.text(x - font::width(s) / 2, y, s, c);
    }

    /// Whether any dot of the rectangle is painted (tests).
    #[cfg(test)]
    pub fn painted(&self, x: i32, y: i32, w: i32, h: i32) -> usize {
        (y..y + h).flat_map(|yy| (x..x + w).map(move |xx| (xx, yy))).filter(|&(xx, yy)| xx >= 0 && yy >= 0 && xx < self.w && yy < self.h && self.px[(yy * self.w + xx) as usize][3] > 0).count()
    }
}

/// `s` with the characters the font has no glyph for replaced by `?`.
pub fn printable(s: &str) -> String {
    s.chars().map(|c| if font::find(c).is_some() { c } else { '?' }).collect()
}

/// A short text on a dark box (messages over the map).
pub fn label(lines: &[(&str, Rgba)]) -> Canvas {
    let w = lines.iter().map(|(s, _)| font::width(s)).max().unwrap_or(0) + 12;
    let h = lines.len() as i32 * 11 + 8;
    let mut c = Canvas::new(w, h);
    c.fill(0, 0, w, h, LABEL_BG);
    for (i, (s, col)) in lines.iter().enumerate() {
        c.text_centred(w / 2, 5 + i as i32 * 11, s, *col);
    }
    c
}
