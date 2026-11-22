//! Linear Executable (LE) support for Watcom / DOS/4GW programs.
//!
//! An LE file is an MZ stub followed by an LE header, an object table (sections), an
//! object page map, fixup tables and the page data. DOS/4GW loads every object at an
//! arbitrary linear address and patches all absolute references with the fixup records;
//! this crate does the same so the runtime can place the original image anywhere.
//!
//! Reference: "LX - Linear eXecutable Module Format Description" (IBM, 1992) and the
//! Watcom `exeflat`/`wdump` sources; only the subset used by DOS/4GW programs is handled.

use std::fmt;

#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

/// Object flags (subset).
pub mod flags {
    pub const READ: u32 = 0x0001;
    pub const WRITE: u32 = 0x0002;
    pub const EXEC: u32 = 0x0004;
    pub const ALIAS_1616: u32 = 0x1000;
    pub const BIG: u32 = 0x2000;
}

#[derive(Clone, Debug)]
pub struct Object {
    /// 1-based object number as used by fixups.
    pub number: u32,
    /// Size in memory (pages beyond the stored ones are zero-filled).
    pub virtual_size: u32,
    /// Preferred (link-time) linear base address.
    pub link_base: u32,
    pub flags: u32,
    /// Stored page data, already zero-padded to whole pages, at most `virtual_size` rounded up.
    pub data: Vec<u8>,
}

impl Object {
    pub fn is_code(&self) -> bool {
        self.flags & flags::EXEC != 0
    }
    pub fn is_32bit(&self) -> bool {
        self.flags & flags::BIG != 0
    }
    /// Size the object occupies in memory, rounded up to a page.
    pub fn mapped_size(&self, page: u32) -> u32 {
        self.virtual_size.max(self.data.len() as u32).div_ceil(page) * page
    }
}

/// What a fixup writes at its source location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixupKind {
    /// 8-bit offset (type 0x00).
    Byte,
    /// 16-bit selector of the target object (type 0x02).
    Selector16,
    /// 16:16 pointer (type 0x03).
    Pointer1616,
    /// 16-bit offset (type 0x05).
    Offset16,
    /// 16:32 pointer (type 0x06).
    Pointer1632,
    /// 32-bit offset — the common case in flat-model code (type 0x07).
    Offset32,
    /// 32-bit self-relative offset (type 0x08).
    Relative32,
}

#[derive(Clone, Copy, Debug)]
pub struct Fixup {
    pub kind: FixupKind,
    /// The selector part refers to the target's 16:16 alias selector.
    pub alias: bool,
    /// Object containing the location to patch, and the offset in that object.
    /// The offset can be negative or run past a page: records belong to the page where
    /// the patched field *starts*, but a field may straddle the page boundary.
    pub src_object: u32,
    pub src_offset: i64,
    pub target_object: u32,
    /// Offset in the target object. May be "negative" (wraps modulo 2^32).
    pub target_offset: u32,
}

#[derive(Debug)]
pub struct LeImage {
    pub page_size: u32,
    pub objects: Vec<Object>,
    pub fixups: Vec<Fixup>,
    pub entry_object: u32,
    pub entry_offset: u32,
    pub stack_object: u32,
    pub stack_offset: u32,
    pub auto_data_object: u32,
}

struct Reader<'a> {
    d: &'a [u8],
}

impl Reader<'_> {
    fn u8(&self, o: usize) -> Result<u8> {
        self.d.get(o).copied().ok_or_else(|| Error(format!("truncated LE file at 0x{o:x}")))
    }
    fn u16(&self, o: usize) -> Result<u16> {
        Ok(u16::from_le_bytes([self.u8(o)?, self.u8(o + 1)?]))
    }
    fn i16(&self, o: usize) -> Result<i16> {
        Ok(self.u16(o)? as i16)
    }
    fn u32(&self, o: usize) -> Result<u32> {
        Ok(u32::from_le_bytes([self.u8(o)?, self.u8(o + 1)?, self.u8(o + 2)?, self.u8(o + 3)?]))
    }
}

/// Locate the LE header. Bound DOS/4GW executables put the DOS/4GW stub (itself an MZ
/// program) in front, so `e_lfanew` of the first MZ header does not always point at it.
fn find_header(d: &[u8]) -> Option<usize> {
    let r = Reader { d };
    if let Ok(lfanew) = r.u32(0x3c) {
        let o = lfanew as usize;
        if d.get(o..o + 2) == Some(b"LE") {
            return Some(o);
        }
    }
    let mut pos = 0;
    while let Some(i) = d[pos..].windows(4).position(|w| w == b"LE\0\0") {
        let o = pos + i;
        // byte/word order 0, CPU type 1..4 (286..586), OS type 1 (OS/2) or 3 (DOS)
        if let (Ok(cpu), Ok(os)) = (r.u16(o + 8), r.u16(o + 10)) {
            if (1..=4).contains(&cpu) && os <= 4 {
                return Some(o);
            }
        }
        pos = o + 1;
    }
    None
}

impl LeImage {
    pub fn parse(file: &[u8]) -> Result<LeImage> {
        if file.get(..2) != Some(b"MZ") {
            return err("not an MZ executable");
        }
        let h = find_header(file).ok_or_else(|| Error("no LE header found".into()))?;
        let r = Reader { d: file };
        let g = |o: usize| r.u32(h + o);
        let num_pages = g(0x14)?;
        let page_size = g(0x28)?;
        let last_page_bytes = g(0x2c)?;
        let obj_table = h + g(0x40)? as usize;
        let num_objects = g(0x44)?;
        let page_map = h + g(0x48)? as usize;
        let fixup_pages = h + g(0x68)? as usize;
        let fixup_records = h + g(0x6c)? as usize;
        let num_imports = g(0x74)?;
        let data_pages = g(0x80)? as usize;
        if page_size == 0 || page_size > 0x10000 {
            return err(format!("implausible page size {page_size}"));
        }

        // Physical page number of each logical page (LE: 24-bit big-endian number + flags).
        let mut phys = Vec::with_capacity(num_pages as usize);
        for i in 0..num_pages as usize {
            let o = page_map + i * 4;
            let n = (r.u8(o)? as u32) << 16 | (r.u8(o + 1)? as u32) << 8 | r.u8(o + 2)? as u32;
            let fl = r.u8(o + 3)?;
            if fl != 0 {
                return err(format!("page {} has unsupported type {fl} (iterated/compressed)", i + 1));
            }
            phys.push(n);
        }

        let mut objects = Vec::with_capacity(num_objects as usize);
        let mut page_owner = vec![(0u32, 0u32); num_pages as usize]; // (object, page index in object)
        for i in 0..num_objects as usize {
            let o = obj_table + i * 24;
            let (virtual_size, link_base, flags, first_page, page_count) =
                (r.u32(o)?, r.u32(o + 4)?, r.u32(o + 8)?, r.u32(o + 12)?, r.u32(o + 16)?);
            let mut data = Vec::with_capacity((page_count * page_size) as usize);
            for k in 0..page_count {
                let logical = first_page + k; // 1-based
                if logical == 0 || logical > num_pages {
                    return err(format!("object {} references page {logical}", i + 1));
                }
                page_owner[(logical - 1) as usize] = (i as u32 + 1, k);
                let p = phys[(logical - 1) as usize];
                let len = if p == num_pages { last_page_bytes } else { page_size } as usize;
                let start = data_pages + (p as usize - 1) * page_size as usize;
                let bytes = file
                    .get(start..start + len)
                    .ok_or_else(|| Error(format!("page {logical} lies outside the file")))?;
                data.extend_from_slice(bytes);
                data.resize(((k + 1) * page_size) as usize, 0);
            }
            objects.push(Object { number: i as u32 + 1, virtual_size, link_base, flags, data });
        }

        if num_imports != 0 {
            return err("imported modules are not supported (DOS/4GW programs have none)");
        }

        let mut fixups = Vec::new();
        for page in 0..num_pages as usize {
            let start = fixup_records + r.u32(fixup_pages + page * 4)? as usize;
            let end = fixup_records + r.u32(fixup_pages + (page + 1) * 4)? as usize;
            let (src_object, page_in_obj) = page_owner[page];
            if src_object == 0 && start != end {
                return err(format!("fixups for page {} which belongs to no object", page + 1));
            }
            let page_base = page_in_obj as i64 * page_size as i64;
            let mut o = start;
            while o < end {
                let src = r.u8(o)?;
                let flg = r.u8(o + 1)?;
                o += 2;
                let kind = match src & 0x0f {
                    0x00 => FixupKind::Byte,
                    0x02 => FixupKind::Selector16,
                    0x03 => FixupKind::Pointer1616,
                    0x05 => FixupKind::Offset16,
                    0x06 => FixupKind::Pointer1632,
                    0x07 => FixupKind::Offset32,
                    0x08 => FixupKind::Relative32,
                    t => return err(format!("unsupported fixup source type 0x{t:x} on page {}", page + 1)),
                };
                // 0x10 = "fixup to 16:16 alias": the selector stored is the target object's
                // 16-bit alias. DOS/4GW-only detail; recorded, offsets are unaffected.
                let alias = src & 0x10 != 0;
                let list = src & 0x20 != 0;
                let mut offsets = Vec::new();
                let count = if list {
                    let c = r.u8(o)?;
                    o += 1;
                    c as usize
                } else {
                    offsets.push(r.i16(o)? as i64);
                    o += 2;
                    0
                };
                if flg & 0x03 != 0 {
                    return err(format!("fixup target type {} (imports/entries) is not supported", flg & 3));
                }
                if flg & 0x04 != 0 {
                    return err("additive fixups are not supported");
                }
                let target_object = if flg & 0x40 != 0 {
                    let v = r.u16(o)? as u32;
                    o += 2;
                    v
                } else {
                    let v = r.u8(o)? as u32;
                    o += 1;
                    v
                };
                let target_offset = if kind == FixupKind::Selector16 {
                    0
                } else if flg & 0x10 != 0 {
                    let v = r.u32(o)?;
                    o += 4;
                    v
                } else {
                    let v = r.u16(o)? as u32;
                    o += 2;
                    v
                };
                for _ in 0..count {
                    offsets.push(r.i16(o)? as i64);
                    o += 2;
                }
                if target_object == 0 || target_object > num_objects {
                    return err(format!("fixup targets object {target_object}"));
                }
                for so in offsets {
                    fixups.push(Fixup {
                        kind,
                        alias,
                        src_object,
                        src_offset: page_base + so,
                        target_object,
                        target_offset,
                    });
                }
            }
        }

        Ok(LeImage {
            page_size,
            objects,
            fixups,
            entry_object: g(0x18)?,
            entry_offset: g(0x1c)?,
            stack_object: g(0x20)?,
            stack_offset: g(0x24)?,
            auto_data_object: g(0x94)?,
        })
    }

    pub fn object(&self, number: u32) -> Option<&Object> {
        self.objects.get(number.checked_sub(1)? as usize)
    }

    /// Compute load addresses that keep the objects' relative layout and move the lowest
    /// one to `base`.
    pub fn layout_at(&self, base: u32) -> Vec<u32> {
        let min = self.objects.iter().map(|o| o.link_base).min().unwrap_or(0);
        self.objects.iter().map(|o| base.wrapping_add(o.link_base - min)).collect()
    }

    /// Total span of memory needed by `layout_at`.
    pub fn span(&self) -> u32 {
        let min = self.objects.iter().map(|o| o.link_base).min().unwrap_or(0);
        self.objects.iter().map(|o| o.link_base - min + o.mapped_size(self.page_size)).max().unwrap_or(0)
    }

    /// Produce the relocated contents of every object for the given load addresses
    /// (`bases[i]` for object `i + 1`). `selector_of` maps an object number to the 16-bit
    /// selector to store for selector fixups.
    pub fn relocate(&self, bases: &[u32], selector_of: impl Fn(u32) -> u16) -> Result<Vec<Vec<u8>>> {
        if bases.len() != self.objects.len() {
            return err("one base address per object is required");
        }
        let mut images: Vec<Vec<u8>> = self
            .objects
            .iter()
            .map(|o| {
                let mut v = o.data.clone();
                v.resize(o.mapped_size(self.page_size) as usize, 0);
                v
            })
            .collect();
        for f in &self.fixups {
            let src = (f.src_object - 1) as usize;
            let tgt_addr = bases[(f.target_object - 1) as usize].wrapping_add(f.target_offset);
            let site = bases[src].wrapping_add(f.src_offset as u32);
            let img = &mut images[src];
            let mut put = |off: i64, bytes: &[u8]| {
                for (k, b) in bytes.iter().enumerate() {
                    let p = off + k as i64;
                    // a field straddling the object's start/end is clipped, as DOS/4GW does
                    if p >= 0 && (p as usize) < img.len() {
                        img[p as usize] = *b;
                    }
                }
            };
            let off = f.src_offset;
            match f.kind {
                FixupKind::Offset32 => put(off, &tgt_addr.to_le_bytes()),
                FixupKind::Relative32 => put(off, &tgt_addr.wrapping_sub(site.wrapping_add(4)).to_le_bytes()),
                FixupKind::Selector16 => put(off, &selector_of(f.target_object).to_le_bytes()),
                // 16-bit offsets are relative to the target object's own selector, not flat
                FixupKind::Offset16 => put(off, &(f.target_offset as u16).to_le_bytes()),
                FixupKind::Byte => put(off, &[f.target_offset as u8]),
                FixupKind::Pointer1616 => {
                    put(off, &(f.target_offset as u16).to_le_bytes());
                    put(off + 2, &selector_of(f.target_object).to_le_bytes());
                }
                FixupKind::Pointer1632 => {
                    put(off, &tgt_addr.to_le_bytes());
                    put(off + 4, &selector_of(f.target_object).to_le_bytes());
                }
            }
        }
        Ok(images)
    }
}
