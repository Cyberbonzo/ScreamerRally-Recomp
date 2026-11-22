//! The game's 2D pictures drawn from their own pixels.
//!
//! The 2D art of Screamer Rally is made for 640 x 400. The 3dfx build stretches every
//! picture to 480 lines when it loads it (`db470`, a nearest-neighbour row DDA that repeats
//! about one row in five and drops the last one) and from then on works in 480-line
//! screen space: sprites (`.SPR`, blitted by `db690`), full-screen backgrounds (`.BKK`,
//! `c483e`) and the rear-view mirror's frame (`MIRROR_x.PKC`, run-length coded, `c42a4`).
//! Text (`.FNT`, `db8c0`), fills and the needle are not stretched (docs/re/strt3fx.md,
//! "2D pictures").
//!
//! This module keeps a copy of each picture as it was before the stretch (observers on
//! the loaders) and records each blit of a stretched
//! picture as a [`Sprite2D`]: the picture's own pixels and the box the game fills with it
//! (in 480-line screen coordinates, exactly the original's box). The game's blitter still
//! runs: the Glide layer keeps the stretched pixels for splitting the HUD into elements
//! (same widescreen anchors as before) and leaves out of the frame-buffer image the pixels
//! a recorded blit wrote last, which the renderer draws from the picture instead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::Machine;
use crate::cpu::HookFrame;
use crate::glide::{Picture, Sprite2D};
use crate::hooks::HookResult;
use crate::mem;

/// `SR_UI_SCALING=original` (diagnostics): the 2D as the 3dfx build drew it, its
/// 480-line frame buffer scaled up as before (rows doubled); nothing is recorded and the
/// frames are those of the versions before the clean scaling.
static ORIGINAL: AtomicBool = AtomicBool::new(false);

pub fn set_original(on: bool) {
    ORIGINAL.store(on, Ordering::Relaxed);
}

/// The 2D is drawn as the 3dfx build drew it (`SR_UI_SCALING=original`).
pub fn original() -> bool {
    ORIGINAL.load(Ordering::Relaxed)
}

// ---- the game's routines (link addresses; docs/re/strt3fx.md, "2D pictures") ----

/// `resize(src_w = eax, src_h = edx, dst_w = ebx, dst_h = ecx, src, dst)`, stack args.
const RESIZE: u32 = 0xdb470;
/// Return addresses of the resize in the three picture loaders.
const RESIZE_SPR: u32 = 0xbf6a8;
const RESIZE_BKK: u32 = 0xc099c;
const RESIZE_PKC: u32 = 0xc056f;
/// `.BKK` loader (c07fe) after its 8-bit to RGB565 conversion: esi = picture, [esp+B4h] the
/// stretched 8-bit pixels, [esi] the RGB565 ones, [esi+4] the palette.
const BKK_LOADED: u32 = 0xc09c9;
/// `.PKC` loader (c040b) at its end: [esp] = picture, [esp+BCh] the stretched 8-bit pixels,
/// [picture] the run-length data, [picture+4] the palette.
const PKC_LOADED: u32 = 0xc078e;
/// Sprite blitter: the request at [DB86C] (enabled, x, y, sprite), sprite records at
/// [DB868], pixels [DB874], palettes [DB878], clip [DB87C..DB888].
const SPRITE_BLIT: u32 = 0xdb690;
/// Full-screen picture copy (eax = x, edx = y, ebx = header, ecx = RGB565 pixels).
const PICTURE_BLIT: u32 = 0xc483e;
/// Run-length blit (eax = frame-buffer address, edx = data, ebx = palette).
const RLE_BLIT: u32 = 0xc42a4;
/// The frame buffer pointer and pitch the 2D code uses.
const LFB_PTR: u32 = 0x14738c;
const LFB_PITCH: u32 = 0xc448c;

/// A picture as its file holds it (palette indices), before the 480-line stretch.
pub struct Source {
    id: u64,
    w: u32,
    h: u32,
    /// Rows the stretched copy has (the game's table: `trunc(1.2 h + 0.4999)`).
    stretched_h: u32,
    pixels: Vec<u8>,
    /// Pictures converted to RGB565 when loaded (backgrounds): their palette.
    palette: Option<Vec<u16>>,
    /// The stretched copy was compared with the row DDA once: Some(matches).
    checked: Mutex<Option<bool>>,
}

#[derive(Default)]
pub struct Ui2d {
    /// Pictures by the guest address of their stretched copy (sprites: the sprite's pixels;
    /// backgrounds: the RGB565 pixels; the mirror frame: its run-length data).
    sources: HashMap<u32, (u64, Arc<Source>)>,
    bytes: usize,
    next_id: u64,
    /// The pictures with a palette applied (by source and palette).
    pictures: HashMap<u64, Arc<Picture>>,
    /// Blits recorded (diagnostics).
    pub blits: u64,
    warned: u32,
}

/// Source rows shown in each of `hh` rows by the game's resize (db470) of `h` rows:
/// a 0.32 fixed-point step `((h - 1) << 16) / hh << 16`, added before each row, a row
/// further on each carry (the last source row is never reached).
pub fn stretch_rows(h: u32, hh: u32) -> Vec<u32> {
    if hh <= h || h == 0 {
        return (0..hh).map(|r| r.min(h.saturating_sub(1))).collect();
    }
    let frac = (((h - 1) << 16) / hh) << 16;
    let mut acc = 0u32;
    let mut row = 0u32;
    (0..hh)
        .map(|_| {
            let (n, carry) = acc.overflowing_add(frac);
            acc = n;
            if carry {
                row += 1;
            }
            row
        })
        .collect()
}

/// RGB565 as the frame-buffer image is converted (`d3d.rs`), premultiplied by `alpha`
/// (0 or 255), R in the low byte.
pub fn rgba(p: u16, opaque: bool) -> u32 {
    if !opaque {
        return 0;
    }
    let p = p as u32;
    let r = ((p >> 11) << 3) | (p >> 13);
    let g = (((p >> 5) & 63) << 2) | ((p >> 9) & 3);
    let b = ((p & 31) << 3) | ((p >> 2) & 7);
    r | g << 8 | b << 16 | 0xff00_0000
}

fn fnv(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x0100_0000_01b3)
}

unsafe fn palette_at(addr: u32) -> Vec<u16> {
    (0..256).map(|i| unsafe { mem::rd16(addr + 2 * i) }).collect()
}

pub fn install(m: &mut Machine) -> Result<(), String> {
    if std::env::var("SR_UI_SCALING").is_ok_and(|v| v.eq_ignore_ascii_case("original")) {
        set_original(true);
        crate::info!("2D pictures: drawn as the 3dfx build drew them (SR_UI_SCALING=original)");
    }
    m.install_observer("2D pictures: resize", RESIZE, resize)?;
    m.install_observer("2D pictures: background loaded", BKK_LOADED, bkk_loaded)?;
    m.install_observer("2D pictures: mirror frame loaded", PKC_LOADED, pkc_loaded)?;
    m.install_observer("2D pictures: sprite blit", SPRITE_BLIT, sprite_blit)?;
    m.install_observer("2D pictures: picture blit", PICTURE_BLIT, picture_blit)?;
    m.install_observer("2D pictures: run-length blit", RLE_BLIT, rle_blit)?;
    Ok(())
}

impl Ui2d {
    fn register(&mut self, key: u32, w: u32, h: u32, stretched_h: u32, pixels: Vec<u8>, palette: Option<Vec<u16>>) {
        self.next_id += 1;
        let id = self.next_id;
        self.bytes += pixels.len();
        let src = Arc::new(Source { id, w, h, stretched_h, pixels, palette, checked: Mutex::new(None) });
        if let Some((_, old)) = self.sources.insert(key, (id, src)) {
            self.bytes -= old.pixels.len();
        }
        // bounded: the oldest pictures go first (screens loaded long ago)
        if self.bytes > 96 << 20 {
            let mut by_age: Vec<(u64, u32)> = self.sources.iter().map(|(&k, (n, _))| (*n, k)).collect();
            by_age.sort();
            for (_, k) in by_age {
                if self.bytes <= 48 << 20 {
                    break;
                }
                if let Some((_, s)) = self.sources.remove(&k) {
                    self.bytes -= s.pixels.len();
                }
            }
        }
    }

    /// The source at `key` moves to `new_key` (the loader's final copy).
    fn rekey(&mut self, key: u32, new_key: u32, palette: Option<Vec<u16>>) -> bool {
        let Some((_, s)) = self.sources.remove(&key) else { return false };
        self.bytes -= s.pixels.len();
        let s = Arc::try_unwrap(s).unwrap_or_else(|a| Source {
            id: a.id,
            w: a.w,
            h: a.h,
            stretched_h: a.stretched_h,
            pixels: a.pixels.clone(),
            palette: a.palette.clone(),
            checked: Mutex::new(None),
        });
        self.register(new_key, s.w, s.h, s.stretched_h, s.pixels, palette);
        true
    }

    fn source(&self, key: u32) -> Option<Arc<Source>> {
        self.sources.get(&key).map(|(_, s)| s.clone())
    }

    /// The picture of `src` with `palette` (index 0 transparent unless `opaque`).
    fn picture(&mut self, src: &Source, palette: &[u16], opaque: bool) -> Arc<Picture> {
        let mut key = fnv(0xcbf2_9ce4_8422_2325, src.id);
        key = fnv(key, opaque as u64);
        for &c in palette {
            key = fnv(key, c as u64);
        }
        if let Some(p) = self.pictures.get(&key) {
            return p.clone();
        }
        let rgba = src.pixels.iter().map(|&i| rgba(palette[i as usize], opaque || i != 0)).collect();
        let p = Arc::new(Picture { key, w: src.w, h: src.h, rgba });
        if self.pictures.len() > 2048 {
            self.pictures.clear();
        }
        self.pictures.insert(key, p.clone());
        p
    }

    fn warn(&mut self, what: std::fmt::Arguments) {
        if self.warned < 20 {
            self.warned += 1;
            crate::warn!("2D pictures: {what}");
        }
    }
}

/// db470 entry: keep the picture the three loaders are about to stretch (same width,
/// more rows).
fn resize(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    let caller = m.img.to_link(f.stack(0)).unwrap_or(0);
    let (sw, sh, dw, dh) = (f.eax, f.edx, f.ebx, f.ecx);
    if matches!(caller, RESIZE_SPR | RESIZE_BKK | RESIZE_PKC) && sw == dw && dh >= sh && sw > 0 && sh > 0 && sw <= 4096 && sh <= 4096 {
        let (src, dst) = (f.stack(1), f.stack(2));
        let pixels = unsafe { mem::slice(src, sw * sh) }.to_vec();
        m.ui2d.register(dst, sw, sh, dh, pixels, None);
    }
    HookResult::Resume(tramp)
}

fn bkk_loaded(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    let obj = f.esi;
    let stretched = f.stack(0xb4 / 4);
    let (rgb, pal) = unsafe { (mem::rd32(obj), mem::rd32(obj + 4)) };
    let palette = unsafe { palette_at(pal) };
    if !m.ui2d.rekey(stretched, rgb, Some(palette)) {
        m.ui2d.warn(format_args!("background at {obj:#x}: its picture was not seen"));
    }
    HookResult::Resume(tramp)
}

fn pkc_loaded(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    let obj = f.stack(0);
    let stretched = f.stack(0xbc / 4);
    let rle = unsafe { mem::rd32(obj) };
    if !m.ui2d.rekey(stretched, rle, None) {
        m.ui2d.warn(format_args!("mirror frame at {obj:#x}: its picture was not seen"));
    }
    HookResult::Resume(tramp)
}

/// Compare a stretched copy with the row DDA once per picture (a wrong guess about the
/// game's resize, or a stale entry, falls back to the frame buffer).
fn checked(m: &mut Machine, src: &Source, what: &str, check: impl FnOnce(&[u32]) -> bool) -> bool {
    let mut c = src.checked.lock().unwrap();
    if let Some(ok) = *c {
        return ok;
    }
    let rows = stretch_rows(src.h, src.stretched_h);
    let ok = check(&rows);
    *c = Some(ok);
    drop(c);
    if ok {
        crate::debug!("2D pictures: {what} {}x{} (stretched to {} rows) matches its source", src.w, src.h, src.stretched_h);
    } else {
        m.ui2d.warn(format_args!("{what} {}x{}: the stretched copy differs from the row DDA; left to the frame buffer", src.w, src.h));
    }
    ok
}

/// Marks the pixels a blit writes (`x`, `y` in the 640 x 480 screen, RGB565 `v`) as
/// belonging to recorded blit `n` (the Glide layer leaves them out of its image when they
/// are still the blit's at the next flush).
struct Marks<'a> {
    g: &'a mut crate::glide::Glide,
    n: u16,
}

impl Marks<'_> {
    #[inline]
    fn mark(&mut self, x: i32, y: i32, v: u16) {
        self.g.ui_mark(self.n, x, y, v);
    }
}

fn writing(m: &Machine) -> bool {
    m.glide.as_ref().is_some_and(|g| g.lfb_locked())
}

/// db690 entry: one sprite of a `.SPR` (index 0 transparent).
fn sprite_blit(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    // its callers draw each sprite twice, under the frame-buffer lock and again after
    // unlocking it: the second is the same pixels
    if original() || !writing(m) {
        return HookResult::Resume(tramp);
    }
    let rd = |a: u32| unsafe { mem::rd32(a) };
    let l = |a: u32| m.img.link(a);
    let req = rd(l(0xdb86c));
    if rd(req) & 1 == 0 {
        return HookResult::Resume(tramp);
    }
    let rec = rd(l(0xdb868)) + rd(req + 12) * 24;
    let x = rd(req + 4) as i32 + rd(rec) as i32;
    let y = rd(req + 8) as i32 + rd(rec + 4) as i32;
    let (w, hh) = (rd(rec + 8), rd(rec + 12));
    let pix = rd(l(0xdb874)).wrapping_add(rd(rec + 16));
    let pal_addr = rd(l(0xdb878)).wrapping_add(rd(rec + 20) * 512);
    let (xmin, xmax, ymin, ymax) = (rd(l(0xdb87c)) as i32, rd(l(0xdb880)) as i32, rd(l(0xdb884)) as i32, rd(l(0xdb888)) as i32);
    let Some(src) = m.ui2d.source(pix) else { return HookResult::Resume(tramp) };
    if src.w != w || src.stretched_h != hh || src.palette.is_some() || w == 0 || hh == 0 {
        return HookResult::Resume(tramp);
    }
    if !checked(m, &src, "sprite", |rows| {
        let stretched = unsafe { mem::slice(pix, w * hh) };
        rows.iter().enumerate().all(|(r, &s)| stretched[r * w as usize..][..w as usize] == src.pixels[(s * w) as usize..][..w as usize])
    }) {
        return HookResult::Resume(tramp);
    }
    // the blitter's clipping (db6bf): a sprite past the left (top) edge is not clipped on
    // the right (bottom)
    let (wi, hi) = (w as i32, hh as i32);
    let (xs, xe) = if x < xmin {
        (xmin, x + wi)
    } else if x >= xmax {
        return HookResult::Resume(tramp);
    } else {
        (x, (x + wi).min(xmax))
    };
    let (ys, ye) = if y < ymin {
        (ymin, y + hi)
    } else if y >= ymax {
        return HookResult::Resume(tramp);
    } else {
        (y, (y + hi).min(ymax))
    };
    if xe <= xs || ye <= ys {
        return HookResult::Resume(tramp);
    }
    let palette = unsafe { palette_at(pal_addr) };
    let pic = m.ui2d.picture(&src, &palette, false);
    m.ui2d.blits += 1;
    let g = m.glide.as_mut().unwrap();
    let n = g.ui_op(Sprite2D { pic, dst: [x as f32, y as f32, w as f32, hh as f32], clip: [xs, ys, xe, ye], anchor: 0 });
    let stretched = unsafe { mem::slice(pix, w * hh) };
    let mut mk = Marks { g, n };
    for r in ys..ye {
        let row = &stretched[((r - y) * wi) as usize..][..w as usize];
        for c in xs..xe {
            let i = row[(c - x) as usize];
            if i != 0 {
                mk.mark(c, r, palette[i as usize]);
            }
        }
    }
    HookResult::Resume(tramp)
}

/// c483e entry: a full-screen background (opaque RGB565 copy, x = y = 0, 640 wide).
fn picture_blit(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    if original() || !writing(m) {
        return HookResult::Resume(tramp);
    }
    let (hdr, pix) = (f.ebx, f.ecx);
    let rd = |a: u32| unsafe { mem::rd32(a) };
    // the routine's own conditions (it returns at once otherwise)
    if f.eax | f.edx != 0 || rd(hdr + 8) != 640 || rd(hdr + 0x14) != 0 {
        return HookResult::Resume(tramp);
    }
    let hh = rd(hdr + 12);
    let Some(src) = m.ui2d.source(pix) else { return HookResult::Resume(tramp) };
    let Some(palette) = src.palette.clone() else { return HookResult::Resume(tramp) };
    if src.w != 640 || src.stretched_h != hh || hh == 0 {
        return HookResult::Resume(tramp);
    }
    if !checked(m, &src, "background", |rows| {
        rows.iter().enumerate().all(|(r, &s)| {
            let row = &src.pixels[(s * 640) as usize..][..640];
            (0..640).all(|c| unsafe { mem::rd16(pix + 2 * (r as u32 * 640 + c)) } == palette[row[c as usize] as usize])
        })
    }) {
        return HookResult::Resume(tramp);
    }
    // the copy runs one row past the picture (rows 0..=hh, at most 480 + 1): only the
    // picture's rows are the picture's
    let rows = hh.min(480) as i32;
    let pic = m.ui2d.picture(&src, &palette, true);
    m.ui2d.blits += 1;
    let g = m.glide.as_mut().unwrap();
    let n = g.ui_op(Sprite2D { pic, dst: [0.0, 0.0, 640.0, hh as f32], clip: [0, 0, 640, rows], anchor: 0 });
    let mut mk = Marks { g, n };
    for r in 0..rows {
        for c in 0..640 {
            mk.mark(c, r, unsafe { mem::rd16(pix + 2 * (r as u32 * 640 + c as u32)) });
        }
    }
    HookResult::Resume(tramp)
}

/// The pixels a run-length blit writes: (frame-buffer pixel index from the start, palette
/// index). Counts: negative = skip, positive = that many index bytes, 0 = end.
fn rle_pixels(data: u32, mut each: impl FnMut(u32, u8)) {
    let (mut p, mut at) = (0u32, data);
    for _ in 0..1_000_000 {
        let n = unsafe { mem::rd32(at) } as i32;
        at += 4;
        if n == 0 {
            return;
        }
        if n < 0 {
            p = p.wrapping_add(n.unsigned_abs());
            continue;
        }
        for k in 0..n as u32 {
            each(p + k, unsafe { mem::rd8(at + k) });
        }
        p += n as u32;
        at += n as u32;
    }
}

/// c42a4 entry: the rear-view mirror's frame (`MIRROR_x.PKC`, index 0 transparent).
fn rle_blit(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    if original() || !writing(m) {
        return HookResult::Resume(tramp);
    }
    let (dst, data, pal_addr) = (f.eax, f.edx, f.ebx);
    let Some(src) = m.ui2d.source(data) else { return HookResult::Resume(tramp) };
    let (lfb, pitch) = unsafe { (mem::rd32(m.img.link(LFB_PTR)), mem::rd32(m.img.link(LFB_PITCH))) };
    let stride = pitch / 2;
    if pitch == 0 || dst < lfb || (dst - lfb) % 2 != 0 || src.palette.is_some() || stride < src.w {
        return HookResult::Resume(tramp);
    }
    let start = (dst - lfb) / 2;
    let (x0, y0) = ((start % stride) as i32, (start / stride) as i32);
    let hh = src.stretched_h;
    // the stretched copy was laid out with the frame buffer's stride (c05b6)
    if !checked(m, &src, "mirror frame", |rows| {
        let w = src.w as usize;
        let mut expect = vec![0u8; stride as usize * hh as usize];
        for (r, &s) in rows.iter().enumerate() {
            expect[r * stride as usize..][..w].copy_from_slice(&src.pixels[s as usize * w..][..w]);
        }
        let mut got = vec![0u8; expect.len()];
        let mut inside = true;
        rle_pixels(data, |p, i| match got.get_mut(p as usize) {
            Some(g) => *g = i,
            None => inside = false,
        });
        inside && got == expect
    }) {
        return HookResult::Resume(tramp);
    }
    let palette = unsafe { palette_at(pal_addr) };
    let pic = m.ui2d.picture(&src, &palette, false);
    m.ui2d.blits += 1;
    let g = m.glide.as_mut().unwrap();
    let n = g.ui_op(Sprite2D {
        pic,
        dst: [x0 as f32, y0 as f32, src.w as f32, hh as f32],
        clip: [x0.max(0), y0.max(0), (x0 + src.w as i32).min(640), (y0 + hh as i32).min(480)],
        anchor: 0,
    });
    let mut mk = Marks { g, n };
    rle_pixels(data, |p, i| {
        let q = start + p;
        mk.mark((q % stride) as i32, (q / stride) as i32, palette[i as usize]);
    });
    HookResult::Resume(tramp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The game's table: `trunc(float(1.2 i) + 0.4999)`.
    fn y480(i: u32) -> u32 {
        ((i as f64 * 1.2) as f32 as f64 + 0.4999) as u32
    }

    #[test]
    fn the_resize_repeats_rows_and_drops_the_last() {
        // the speedometer's face: 113 rows to 136
        let rows = stretch_rows(113, y480(113));
        assert_eq!(rows.len(), 136);
        assert_eq!(rows[0], 0);
        assert_eq!(*rows.last().unwrap(), 111, "row 112 is never shown");
        // every row up to the last but one appears, in order, once or twice
        for w in rows.windows(2) {
            assert!(w[1] == w[0] || w[1] == w[0] + 1);
        }
        let twice = rows.windows(2).filter(|w| w[0] == w[1]).count();
        assert_eq!(twice, 136 - 112);
        // a full-screen background: 400 rows to 480, row 399 dropped
        let rows = stretch_rows(400, y480(400));
        assert_eq!((rows.len(), rows[479]), (480, 398));
        // same height: copied
        assert_eq!(stretch_rows(1, y480(1)), vec![0]);
    }
}
