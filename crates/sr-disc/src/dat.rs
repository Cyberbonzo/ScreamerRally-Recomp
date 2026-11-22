//! The game's `.DAT` archives (`DATA\LOCAL.DAT`, `GFX\GRAF2.DAT`, ...), read as the game
//! does (open / find entry at `52ec9` / `52f21`, decompression at `c9c40` .. `c9fb0`;
//! docs/re/english.md, "The `.DAT` archives"):
//!
//! * the last dword of the file is the size of the directory, which lies just before it:
//!   32-byte records {name: 16 bytes, each XOR 3Ah, NUL-terminated after the XOR; data
//!   offset; stored size; size; flags}; an empty name ends the list;
//! * flag 2: the entry is LZW compressed (below); flags 0: stored as is.

use crate::{Result, format_err};

/// One entry of an archive's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Name as stored (e.g. `STRINGS.TXT`).
    pub name: String,
    /// Byte offset of the data in the archive.
    pub offset: u32,
    /// Bytes stored in the archive.
    pub stored: u32,
    /// Bytes after decompression.
    pub size: u32,
    pub flags: u32,
}

impl Entry {
    pub fn compressed(&self) -> bool {
        self.flags & 2 != 0
    }
}

/// An archive held in memory.
pub struct Archive<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

const RECORD: usize = 32;
const NAME_KEY: u8 = 0x3a;

/// The directory's records, up to the first empty name.
fn records(dir: &[u8]) -> Vec<Entry> {
    let mut entries = Vec::new();
    for r in dir.chunks_exact(RECORD) {
        let name: Vec<u8> = r[..16].iter().map(|b| b ^ NAME_KEY).take_while(|&b| b != 0).collect();
        if name.is_empty() {
            break;
        }
        let field = |i: usize| u32::from_le_bytes(r[16 + i * 4..20 + i * 4].try_into().unwrap());
        entries.push(Entry { name: String::from_utf8_lossy(&name).into_owned(), offset: field(0), stored: field(1), size: field(2), flags: field(3) });
    }
    entries
}

/// The size of an archive's directory, from the archive's last four bytes.
pub fn directory_size(last4: [u8; 4]) -> usize {
    u32::from_le_bytes(last4) as usize
}

/// An archive's entry names, from the archive's last bytes alone: its directory and the size
/// after it (`directory_size + 4` bytes; more are ignored). For a look at what an archive
/// holds without reading the data.
pub fn names_from_tail(tail: &[u8]) -> Result<Vec<String>> {
    let len = tail.len();
    if len < 4 {
        return format_err("archive too short");
    }
    let dir_size = directory_size(tail[len - 4..].try_into().unwrap());
    if dir_size > len - 4 {
        return format_err(format!("archive directory size {dir_size} exceeds the {} bytes read", len - 4));
    }
    Ok(records(&tail[len - 4 - dir_size..len - 4]).into_iter().map(|e| e.name).collect())
}

impl<'a> Archive<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Archive<'a>> {
        let len = data.len();
        if len < 4 {
            return format_err("archive too short");
        }
        let dir_size = u32::from_le_bytes(data[len - 4..].try_into().unwrap()) as usize;
        if dir_size > len - 4 {
            return format_err(format!("archive directory size {dir_size} exceeds the file ({len} bytes)"));
        }
        let entries = records(&data[len - 4 - dir_size..len - 4]);
        if let Some(e) = entries.iter().find(|e| e.offset as usize + e.stored as usize > len - 4 - dir_size) {
            return format_err(format!("archive entry {} lies outside the data", e.name));
        }
        Ok(Archive { data, entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Find an entry by name (case-insensitive).
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))
    }

    /// The entry's contents, decompressed.
    pub fn read(&self, e: &Entry) -> Result<Vec<u8>> {
        let stored = &self.data[e.offset as usize..(e.offset + e.stored) as usize];
        if e.compressed() { lzw_decode(stored, e.size as usize) } else { Ok(stored.to_vec()) }
    }
}

/// The game's LZW (`c9d30`, bit reader `c9fb0`): codes packed least significant bit first,
/// 9 bits growing to 12; 100h clears the dictionary, 101h ends the stream, the first free
/// code is 102h, and the width grows when the next free code reaches 2^width. Decodes
/// `size` bytes.
pub fn lzw_decode(input: &[u8], size: usize) -> Result<Vec<u8>> {
    const CLEAR: u16 = 0x100;
    const END: u16 = 0x101;
    const FIRST: usize = 0x102;
    const LIMIT: usize = 4096;
    let mut out = Vec::with_capacity(size);
    let mut prefix = vec![0u16; LIMIT];
    let mut suffix = vec![0u8; LIMIT];
    let mut stack = Vec::new();
    let (mut width, mut next) = (9u32, FIRST);
    let mut old: Option<u16> = None;
    let mut first_byte = 0u8;
    let mut bit = 0usize;
    while out.len() < size {
        if bit + width as usize > input.len() * 8 {
            return format_err(format!("compressed data ends after {} of {size} bytes", out.len()));
        }
        let mut code = 0u16;
        for i in 0..width as usize {
            let b = bit + i;
            code |= ((input[b / 8] >> (b % 8)) as u16 & 1) << i;
        }
        bit += width as usize;
        if code == END {
            break;
        }
        if code == CLEAR {
            (width, next, old) = (9, FIRST, None);
            continue;
        }
        let Some(prev) = old else {
            if code > 0xff {
                return format_err(format!("compressed data starts with code {code:x}"));
            }
            first_byte = code as u8;
            out.push(first_byte);
            old = Some(code);
            continue;
        };
        let mut c = code as usize;
        if c >= next {
            if c > next {
                return format_err(format!("compressed data has an undefined code {code:x}"));
            }
            // the code being defined: the previous string plus its own first byte
            stack.push(first_byte);
            c = prev as usize;
        }
        while c > 0xff {
            stack.push(suffix[c]);
            c = prefix[c] as usize;
        }
        first_byte = c as u8;
        stack.push(first_byte);
        out.extend(stack.drain(..).rev());
        if next < LIMIT {
            prefix[next] = prev;
            suffix[next] = first_byte;
            next += 1;
        }
        if next >= 1 << width && width < 12 {
            width += 1;
        }
        old = Some(code);
    }
    if out.len() < size {
        return format_err(format!("compressed data holds {} of {size} bytes", out.len()));
    }
    out.truncate(size);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An encoder matching the decoder above (the tests' counterpart of the game's packer):
    /// starts with a clear code, ends with the end code, clears when the dictionary is full.
    pub fn lzw_encode(data: &[u8]) -> Vec<u8> {
        use std::collections::HashMap;
        let mut bits: Vec<bool> = Vec::new();
        let put = |code: usize, width: u32, bits: &mut Vec<bool>| (0..width).for_each(|i| bits.push(code >> i & 1 != 0));
        let mut dict: HashMap<(usize, u8), usize> = HashMap::new();
        let (mut width, mut next) = (9u32, 0x102usize);
        put(0x100, width, &mut bits);
        let mut cur: Option<usize> = None;
        for &b in data {
            let Some(c) = cur else {
                cur = Some(b as usize);
                continue;
            };
            if let Some(&n) = dict.get(&(c, b)) {
                cur = Some(n);
                continue;
            }
            put(c, width, &mut bits);
            // The decoder defines each entry one code later (when it sees the next code's
            // first byte), so it widens one code after the encoder's count would.
            if next < 4096 {
                dict.insert((c, b), next);
                next += 1;
                if next > 1 << width && width < 12 {
                    width += 1;
                }
            } else {
                put(0x100, width, &mut bits);
                dict.clear();
                (width, next) = (9, 0x102);
            }
            cur = Some(b as usize);
        }
        if let Some(c) = cur {
            put(c, width, &mut bits);
        }
        put(0x101, width, &mut bits);
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (i, &b) in bits.iter().enumerate() {
            out[i / 8] |= (b as u8) << (i % 8);
        }
        out
    }

    /// An archive with the given entries, compressed (flags 3) or stored.
    pub fn archive(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut data = Vec::new();
        let mut dir = Vec::new();
        for (name, content, compress) in entries {
            let stored = if *compress { lzw_encode(content) } else { content.to_vec() };
            let mut rec = [0u8; 32];
            for (i, b) in name.bytes().enumerate() {
                rec[i] = b ^ NAME_KEY;
            }
            rec[name.len()] = NAME_KEY;
            for (i, v) in [data.len() as u32, stored.len() as u32, content.len() as u32, if *compress { 3 } else { 0 }].iter().enumerate() {
                rec[16 + i * 4..20 + i * 4].copy_from_slice(&v.to_le_bytes());
            }
            dir.extend_from_slice(&rec);
            data.extend_from_slice(&stored);
        }
        dir.extend_from_slice(&[NAME_KEY; 32]);
        data.extend_from_slice(&dir);
        data.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        data
    }

    #[test]
    fn lzw_round_trip() {
        // repetitive text (long dictionary strings, the KwKwK case) and noise long enough to
        // fill the dictionary and force a clear
        let mut text = b"*max chars 16\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\nabababababababababab\n".repeat(40);
        let mut x = 12345u32;
        for _ in 0..20000 {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            text.push((x >> 16) as u8);
        }
        let packed = lzw_encode(&text);
        assert_eq!(lzw_decode(&packed, text.len()).unwrap(), text);
        // the stream holds fewer bytes than the directory claims
        assert!(lzw_decode(&packed, text.len() + 1).is_err());
        assert!(lzw_decode(&packed[..packed.len() / 2], text.len()).is_err());
    }

    #[test]
    fn archive_entries() {
        let a = archive(&[("STRINGS.TXT", b"MENU\nMENU\nMENU\n", true), ("RAW.BIN", &[1, 2, 3], false)]);
        let arc = Archive::parse(&a).unwrap();
        assert_eq!(arc.entries().len(), 2);
        let e = arc.find("strings.txt").unwrap();
        assert!(e.compressed());
        assert_eq!(arc.read(e).unwrap(), b"MENU\nMENU\nMENU\n");
        assert_eq!(arc.read(arc.find("RAW.BIN").unwrap()).unwrap(), [1, 2, 3]);
        assert!(arc.find("GAME.TXT").is_none());
        assert!(Archive::parse(&[1, 2]).is_err());
        assert!(Archive::parse(&[0, 0, 0, 0, 0xff, 0, 0, 0]).is_err());
        // the names from the directory alone (the archive's last bytes)
        let dir = directory_size(a[a.len() - 4..].try_into().unwrap());
        assert_eq!(names_from_tail(&a[a.len() - 4 - dir..]).unwrap(), ["STRINGS.TXT", "RAW.BIN"]);
        assert_eq!(names_from_tail(&a).unwrap(), ["STRINGS.TXT", "RAW.BIN"]);
        assert!(names_from_tail(&a[a.len() - 8..]).is_err());
    }
}
