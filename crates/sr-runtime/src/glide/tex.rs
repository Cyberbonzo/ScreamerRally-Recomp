//! Voodoo texture memory (TMU RAM), Glide texture layout rules and texel decoding.

use super::consts::*;
use crate::mem;

/// `GrTexInfo` without the data pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct TexInfo {
    pub small_lod: u32,
    pub large_lod: u32,
    pub aspect: u32,
    pub format: u32,
    pub data: u32,
}

impl TexInfo {
    /// # Safety
    /// `p` must point at a `GrTexInfo` in guest memory.
    pub unsafe fn read(p: u32) -> TexInfo {
        unsafe {
            TexInfo {
                small_lod: mem::rd32(p) & 0xf,
                large_lod: mem::rd32(p + 4) & 0xf,
                aspect: mem::rd32(p + 8) & 0x7,
                format: mem::rd32(p + 12) & 0xf,
                data: mem::rd32(p + 16),
            }
        }
    }

    /// The binding key ignores the data pointer.
    pub fn layout(&self) -> TexInfo {
        TexInfo { data: 0, ..*self }
    }
}

/// Width and height of mip level `lod` (GR_LOD_256 = 0 … GR_LOD_1 = 8).
pub fn level_size(lod: u32, aspect: u32) -> (u32, u32) {
    let s = 256u32 >> lod.min(8);
    let d = |n: u32| (s >> n).max(1);
    match aspect {
        0 => (s, d(3)),
        1 => (s, d(2)),
        2 => (s, d(1)),
        3 => (s, s),
        4 => (d(1), s),
        5 => (d(2), s),
        _ => (d(3), s),
    }
}

pub fn bytes_per_texel(format: u32) -> u32 {
    if format >= GR_TEXFMT_ARGB_8332 { 2 } else { 1 }
}

fn level_bytes(lod: u32, aspect: u32, format: u32) -> u32 {
    let (w, h) = level_size(lod, aspect);
    w * h * bytes_per_texel(format)
}

fn level_selected(lod: u32, even_odd: u32) -> bool {
    match even_odd & 3 {
        1 => lod % 2 == 0,
        2 => lod % 2 == 1,
        _ => true,
    }
}

/// Byte offset of `lod` within a texture whose largest level is `large` (levels are stored
/// largest first, only those selected by `even_odd`).
pub fn level_offset(lod: u32, large: u32, aspect: u32, format: u32, even_odd: u32) -> u32 {
    (large..lod).filter(|l| level_selected(*l, even_odd)).map(|l| level_bytes(l, aspect, format)).sum()
}

/// `grTexCalcMemRequired` / `grTexTextureMemRequired`.
pub fn mem_required(small: u32, large: u32, aspect: u32, format: u32, even_odd: u32) -> u32 {
    let (small, large) = (small.min(8), large.min(8));
    let total: u32 = (large..=small).filter(|l| level_selected(*l, even_odd)).map(|l| level_bytes(l, aspect, format)).sum();
    total.div_ceil(8) * 8
}

pub struct TexMemory {
    pub ram: Vec<u8>,
    /// Bumped on every download; lets backends notice changes cheaply.
    pub generation: u64,
}

impl TexMemory {
    pub fn new(size: u32) -> TexMemory {
        TexMemory { ram: vec![0; size as usize], generation: 0 }
    }

    pub fn size(&self) -> u32 {
        self.ram.len() as u32
    }

    fn write(&mut self, addr: u32, src: u32, len: u32) {
        let end = (addr as usize + len as usize).min(self.ram.len());
        if (addr as usize) < end {
            let n = end - addr as usize;
            unsafe { self.ram[addr as usize..end].copy_from_slice(mem::slice(src, n as u32)) };
        }
        self.generation += 1;
    }

    /// `grTexDownloadMipMap`. Returns the TMU address range written.
    pub fn download_mipmap(&mut self, start: u32, even_odd: u32, info: &TexInfo) -> (u32, u32) {
        let (mut src, mut dst) = (info.data, start);
        for lod in info.large_lod..=info.small_lod.min(8) {
            let n = level_bytes(lod, info.aspect, info.format);
            if level_selected(lod, even_odd) {
                self.write(dst, src, n);
                dst += n;
            }
            src += n;
        }
        (start, dst)
    }

    /// `grTexDownloadMipMapLevel(Partial)`; rows `first..=last` of level `lod`, `data`
    /// points at row `first`.
    #[allow(clippy::too_many_arguments)]
    pub fn download_level(&mut self, start: u32, lod: u32, large: u32, aspect: u32, format: u32, even_odd: u32, data: u32, first: u32, last: u32) -> (u32, u32) {
        let (w, h) = level_size(lod, aspect);
        let bpt = bytes_per_texel(format);
        let row_bytes = w * bpt;
        let last = last.min(h - 1);
        if first > last {
            return (start, start);
        }
        let base = start + level_offset(lod, large, aspect, format, even_odd);
        let at = base + first * row_bytes;
        let len = (last - first + 1) * row_bytes;
        self.write(at, data, len);
        (at, at + len)
    }
}

fn expand5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}
fn expand6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}
fn rgba(r: u32, g: u32, b: u32, a: u32) -> u32 {
    r | g << 8 | b << 16 | a << 24
}

fn ncc(table: &[u8; 48], v: u32) -> u32 {
    // GuNccTable: yRGB[16] u8, iRGB[4][3] i16, qRGB[4][3] i16
    let y = table[(v >> 4) as usize & 15] as i32;
    let i16at = |o: usize| i16::from_le_bytes([table[o], table[o + 1]]) as i32;
    let ii = ((v >> 2) & 3) as usize;
    let qi = (v & 3) as usize;
    let ch = |c: usize| (y + i16at(16 + ii * 6 + c * 2) + i16at(40 + qi * 6 + c * 2).min(i16::MAX as i32)).clamp(0, 255) as u32;
    rgba(ch(0), ch(1), ch(2), 255)
}

/// Decode one mip level to RGBA8 (`r | g<<8 | b<<16 | a<<24`).
pub fn decode_level(tmu: &TexMemory, start: u32, info: &TexInfo, even_odd: u32, lod: u32, palette: &[u32; 256], ncc_tables: &[[u8; 48]; 2], ncc_sel: usize) -> (u32, u32, Vec<u32>) {
    let (w, h) = level_size(lod, info.aspect);
    let base = (start + level_offset(lod, info.large_lod, info.aspect, info.format, even_odd)) as usize;
    let n = (w * h) as usize;
    let ram = &tmu.ram;
    let b8 = |i: usize| *ram.get(base + i).unwrap_or(&0) as u32;
    let b16 = |i: usize| *ram.get(base + i * 2).unwrap_or(&0) as u32 | (*ram.get(base + i * 2 + 1).unwrap_or(&0) as u32) << 8;
    let pal = |i: u32| {
        let p = palette[i as usize & 255];
        rgba((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff, 255)
    };
    let c332 = |v: u32| rgba(((v >> 5) & 7) * 255 / 7, ((v >> 2) & 7) * 255 / 7, (v & 3) * 255 / 3, 255);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let px = match info.format {
            GR_TEXFMT_RGB_332 => c332(b8(i)),
            GR_TEXFMT_YIQ_422 => ncc(&ncc_tables[ncc_sel], b8(i)),
            GR_TEXFMT_ALPHA_8 => rgba(255, 255, 255, b8(i)),
            GR_TEXFMT_INTENSITY_8 => {
                let v = b8(i);
                rgba(v, v, v, 255)
            }
            GR_TEXFMT_ALPHA_INTENSITY_44 => {
                let v = b8(i);
                let (a, l) = ((v >> 4) * 17, (v & 15) * 17);
                rgba(l, l, l, a)
            }
            GR_TEXFMT_P_8 => pal(b8(i)),
            GR_TEXFMT_ARGB_8332 => {
                let v = b16(i);
                (c332(v & 0xff) & 0x00ff_ffff) | (v >> 8) << 24
            }
            GR_TEXFMT_AYIQ_8422 => {
                let v = b16(i);
                (ncc(&ncc_tables[ncc_sel], v & 0xff) & 0x00ff_ffff) | (v >> 8) << 24
            }
            GR_TEXFMT_RGB_565 => {
                let v = b16(i);
                rgba(expand5(v >> 11), expand6((v >> 5) & 63), expand5(v & 31), 255)
            }
            GR_TEXFMT_ARGB_1555 => {
                let v = b16(i);
                rgba(expand5((v >> 10) & 31), expand5((v >> 5) & 31), expand5(v & 31), if v & 0x8000 != 0 { 255 } else { 0 })
            }
            GR_TEXFMT_ARGB_4444 => {
                let v = b16(i);
                rgba(((v >> 8) & 15) * 17, ((v >> 4) & 15) * 17, (v & 15) * 17, (v >> 12) * 17)
            }
            GR_TEXFMT_ALPHA_INTENSITY_88 => {
                let v = b16(i);
                let l = v & 0xff;
                rgba(l, l, l, v >> 8)
            }
            GR_TEXFMT_AP_88 => {
                let v = b16(i);
                (pal(v & 0xff) & 0x00ff_ffff) | (v >> 8) << 24
            }
            _ => rgba(255, 0, 255, 255),
        };
        out.push(px);
    }
    (w, h, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(level_size(0, 3), (256, 256));
        assert_eq!(level_size(2, 2), (64, 32));
        assert_eq!(level_size(2, 4), (32, 64));
        assert_eq!(level_size(8, 0), (1, 1));
        // 256x256 16-bit, all levels: 2 * 87381 bytes, rounded up to 8
        assert_eq!(mem_required(8, 0, 3, GR_TEXFMT_RGB_565, 3), 174768);
        assert_eq!(level_offset(1, 0, 3, GR_TEXFMT_RGB_565, 3), 131072);
    }
}
