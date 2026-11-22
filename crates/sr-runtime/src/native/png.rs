//! PNG files for the diagnostics' pictures (catalog, hole report, exports): compressed
//! (zlib / deflate with LZ77 and the fixed Huffman codes; no crate needed), RGB or RGBA,
//! each row filtered with the filter that leaves the smallest sum of magnitudes (as most
//! encoders do). Unlike the frame captures' stored PNGs (`SR_SHOT`), every image reader
//! opens them (System.Drawing included).

use std::path::Path;

/// Encode `w` x `h` pixels of `channels` bytes (3: RGB, 4: RGBA), rows top to bottom.
pub fn encode(w: usize, h: usize, channels: usize, px: &[u8]) -> Vec<u8> {
    assert!(channels == 3 || channels == 4);
    assert!(px.len() >= w * h * channels);
    let stride = w * channels;
    let mut raw = Vec::with_capacity((stride + 1) * h);
    let mut cand = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    for y in 0..h {
        let row = &px[y * stride..(y + 1) * stride];
        let up = if y > 0 { Some(&px[(y - 1) * stride..y * stride]) } else { None };
        let mut best_ty = 0u8;
        let mut best_sum = u64::MAX;
        for ty in 0..5u8 {
            for i in 0..stride {
                let a = if i >= channels { row[i - channels] } else { 0 };
                let b = up.map_or(0, |u| u[i]);
                let c = if i >= channels { up.map_or(0, |u| u[i - channels]) } else { 0 };
                cand[i] = row[i].wrapping_sub(match ty {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((a as u16 + b as u16) / 2) as u8,
                    _ => paeth(a, b, c),
                });
            }
            let sum: u64 = cand.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum();
            if sum < best_sum {
                best_sum = sum;
                best_ty = ty;
                best.copy_from_slice(&cand);
            }
        }
        raw.push(best_ty);
        raw.extend_from_slice(&best);
    }
    let z = zlib(&raw);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, if channels == 4 { 6 } else { 2 }, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Write a PNG file (see [`encode`]).
pub fn write(path: &Path, w: usize, h: usize, channels: usize, px: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, encode(w, h, channels, px))
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &x in data {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Bits written least significant first (deflate's order).
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, v: u32, n: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// A Huffman code (`n` bits, most significant first).
    fn code(&mut self, c: u32, n: u32) {
        self.put(c.reverse_bits() >> (32 - n), n);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// A literal or length symbol with the fixed Huffman code.
fn lit(b: &mut Bits, sym: u32) {
    match sym {
        0..=143 => b.code(0x30 + sym, 8),
        144..=255 => b.code(0x190 + sym - 144, 9),
        256..=279 => b.code(sym - 256, 7),
        _ => b.code(0xc0 + sym - 280, 8),
    }
}

/// zlib stream of `data`: one fixed-Huffman deflate block, LZ77 over a 32 KiB window
/// (hash chains, greedy matches of 3..258 bytes).
pub fn zlib(data: &[u8]) -> Vec<u8> {
    const WINDOW: usize = 32768;
    const HASH: usize = 1 << 15;
    const CHAIN: usize = 48;
    let mut b = Bits { out: vec![0x78, 0x01], acc: 0, n: 0 };
    b.put(1, 1); // final block
    b.put(1, 2); // fixed Huffman codes
    let mut head = vec![usize::MAX; HASH];
    let mut prev = vec![usize::MAX; WINDOW];
    let hash = |i: usize| ((data[i] as usize) << 10 ^ (data[i + 1] as usize) << 5 ^ data[i + 2] as usize) & (HASH - 1);
    let insert = |i: usize, head: &mut [usize], prev: &mut [usize]| {
        if i + 2 < data.len() {
            let h = hash(i);
            prev[i % WINDOW] = head[h];
            head[h] = i;
        }
    };
    let mut i = 0;
    while i < data.len() {
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        if i + 2 < data.len() {
            let mut cand = head[hash(i)];
            let max = (data.len() - i).min(258);
            let mut steps = 0;
            while cand != usize::MAX && i - cand <= WINDOW && steps < CHAIN {
                if data[cand + best_len.min(max - 1)] == data[i + best_len.min(max - 1)] {
                    let mut l = 0;
                    while l < max && data[cand + l] == data[i + l] {
                        l += 1;
                    }
                    if l > best_len {
                        best_len = l;
                        best_dist = i - cand;
                        if l == max {
                            break;
                        }
                    }
                }
                let next = prev[cand % WINDOW];
                if next == usize::MAX || next >= cand {
                    break;
                }
                cand = next;
                steps += 1;
            }
        }
        if best_len >= 3 {
            let li = LEN_BASE.iter().rposition(|&x| x as usize <= best_len).unwrap();
            lit(&mut b, 257 + li as u32);
            b.put((best_len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
            let di = DIST_BASE.iter().rposition(|&x| x as usize <= best_dist).unwrap();
            b.code(di as u32, 5);
            b.put((best_dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
            for k in i..i + best_len {
                insert(k, &mut head, &mut prev);
            }
            i += best_len;
        } else {
            lit(&mut b, data[i] as u32);
            insert(i, &mut head, &mut prev);
            i += 1;
        }
    }
    lit(&mut b, 256);
    let mut out = b.finish();
    let (mut s1, mut s2) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            s1 += x as u32;
            s2 += s1;
        }
        s1 %= 65521;
        s2 %= 65521;
    }
    out.extend_from_slice(&(s2 << 16 | s1).to_be_bytes());
    out
}

/// Standard base64 (for pictures embedded in a self-contained HTML page).
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(T[(v >> (18 - 6 * k)) as usize & 63] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoder for what [`zlib`] writes (fixed Huffman blocks only).
    fn inflate_fixed(z: &[u8]) -> Vec<u8> {
        assert_eq!(z[0], 0x78);
        let d = &z[2..z.len() - 4];
        let mut pos = 0usize;
        let mut bit = || {
            let v = (d[pos / 8] >> (pos % 8)) & 1;
            pos += 1;
            v as u32
        };
        let mut out: Vec<u8> = Vec::new();
        loop {
            let last = bit();
            let ty = bit() | bit() << 1;
            assert_eq!(ty, 1);
            loop {
                let mut code = 0u32;
                let mut sym = None;
                for len in 1..=9 {
                    code = code << 1 | bit();
                    sym = match len {
                        7 if code <= 23 => Some(256 + code),
                        8 if (0x30..=0xbf).contains(&code) => Some(code - 0x30),
                        8 if (0xc0..=0xc7).contains(&code) => Some(280 + code - 0xc0),
                        9 if code >= 0x190 => Some(144 + code - 0x190),
                        _ => None,
                    };
                    if sym.is_some() {
                        break;
                    }
                }
                let sym = sym.expect("a symbol");
                let mut extra = |n: u8| (0..n).fold(0u32, |a, k| a | bit() << k);
                if sym < 256 {
                    out.push(sym as u8);
                } else if sym == 256 {
                    break;
                } else {
                    let li = (sym - 257) as usize;
                    let len = LEN_BASE[li] as usize + extra(LEN_EXTRA[li]) as usize;
                    let mut dc = 0u32;
                    for _ in 0..5 {
                        dc = dc << 1 | bit();
                    }
                    let dist = DIST_BASE[dc as usize] as usize + (0..DIST_EXTRA[dc as usize]).fold(0u32, |a, k| a | bit() << k) as usize;
                    for _ in 0..len {
                        out.push(out[out.len() - dist]);
                    }
                }
            }
            if last == 1 {
                break;
            }
        }
        out
    }

    #[test]
    fn zlib_round_trip() {
        let mut data = Vec::new();
        let mut x = 12345u32;
        for i in 0..200_000u32 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            // runs, repeats far back and noise
            data.push(match i % 1000 {
                0..=399 => 7,
                400..=699 => (i % 37) as u8,
                _ => (x >> 24) as u8,
            });
        }
        for d in [&data[..], &[][..], &[1u8][..], &[1u8, 2][..], &[5u8; 1000][..]] {
            let z = zlib(d);
            assert_eq!(inflate_fixed(&z), d);
            let (mut s1, mut s2) = (1u32, 0u32);
            for &v in d {
                s1 = (s1 + v as u32) % 65521;
                s2 = (s2 + s1) % 65521;
            }
            assert_eq!(&z[z.len() - 4..], &(s2 << 16 | s1).to_be_bytes());
        }
        assert!(zlib(&data).len() < data.len() / 2);
    }

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
