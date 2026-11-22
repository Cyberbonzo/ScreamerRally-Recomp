//! Read-only ISO 9660 (level 1, no Joliet/Rock Ridge needed for this disc).

use crate::{DATA_SECTOR, Disc, Result, format_err};

#[derive(Clone, Debug)]
pub struct DirEntry {
    /// Name without the `;1` version suffix, upper case as stored on the disc.
    pub name: String,
    pub lba: u32,
    pub size: u32,
    pub is_dir: bool,
    /// Recording time: year, month, day, hour, minute, second.
    pub time: (u16, u8, u8, u8, u8, u8),
}

pub struct IsoFs<'a> {
    disc: &'a Disc,
    pub volume_id: String,
    pub volume_blocks: u32,
    root: DirEntry,
}

fn parse_record(r: &[u8]) -> Option<DirEntry> {
    let len = r[0] as usize;
    if len < 34 || r.len() < len {
        return None;
    }
    let lba = u32::from_le_bytes(r[2..6].try_into().unwrap());
    let size = u32::from_le_bytes(r[10..14].try_into().unwrap());
    let time = (1900 + r[18] as u16, r[19], r[20], r[21], r[22], r[23]);
    let flags = r[25];
    let nlen = r[32] as usize;
    let raw = &r[33..33 + nlen];
    let name = match raw {
        [0] => ".".to_string(),
        [1] => "..".to_string(),
        _ => {
            let s: String = raw.iter().map(|&c| c as char).collect();
            let s = s.split(';').next().unwrap_or("").to_string();
            s.strip_suffix('.').map(str::to_string).unwrap_or(s)
        }
    };
    Some(DirEntry { name, lba, size, is_dir: flags & 2 != 0, time })
}

impl<'a> IsoFs<'a> {
    pub fn mount(disc: &'a Disc) -> Result<IsoFs<'a>> {
        let data = disc
            .tracks()
            .iter()
            .find(|t| t.kind == crate::TrackKind::Data)
            .ok_or(crate::Error::Format("disc has no data track".into()))?;
        let mut pvd = [0u8; DATA_SECTOR];
        disc.read_data_sector(data.start + 16, &mut pvd)?;
        if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
            return format_err("no ISO 9660 primary volume descriptor at sector 16");
        }
        let volume_id = String::from_utf8_lossy(&pvd[40..72]).trim().to_string();
        let volume_blocks = u32::from_le_bytes(pvd[80..84].try_into().unwrap());
        let root = parse_record(&pvd[156..190]).ok_or(crate::Error::Format("bad root record".into()))?;
        Ok(IsoFs { disc, volume_id, volume_blocks, root })
    }

    pub fn root(&self) -> &DirEntry {
        &self.root
    }

    /// List a directory (without `.` and `..`).
    pub fn read_dir(&self, dir: &DirEntry) -> Result<Vec<DirEntry>> {
        if !dir.is_dir {
            return format_err(format!("{} is not a directory", dir.name));
        }
        let data = self.disc.read_data(dir.lba, dir.size as usize)?;
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            let len = data[pos] as usize;
            if len == 0 {
                // records never straddle sectors; skip the padding to the next sector
                pos = (pos / DATA_SECTOR + 1) * DATA_SECTOR;
                continue;
            }
            if let Some(e) = parse_record(&data[pos..]) {
                if e.name != "." && e.name != ".." {
                    out.push(e);
                }
            }
            pos += len;
        }
        Ok(out)
    }

    /// Find an entry by a DOS-style path (`GFX\MAP11.DAT`, `/data/game.dat`), case-insensitive.
    pub fn lookup(&self, path: &str) -> Result<Option<DirEntry>> {
        let mut cur = self.root.clone();
        for part in path.split(['\\', '/']).filter(|p| !p.is_empty() && *p != ".") {
            if !cur.is_dir {
                return Ok(None);
            }
            match self.read_dir(&cur)?.into_iter().find(|e| e.name.eq_ignore_ascii_case(part)) {
                Some(e) => cur = e,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    pub fn read_file(&self, entry: &DirEntry) -> Result<Vec<u8>> {
        self.disc.read_data(entry.lba, entry.size as usize)
    }

    /// Walk the whole tree, calling `f(path, entry)` for every file and directory.
    pub fn walk(&self, mut f: impl FnMut(&str, &DirEntry)) -> Result<()> {
        fn rec(fs: &IsoFs<'_>, dir: &DirEntry, prefix: &str, f: &mut dyn FnMut(&str, &DirEntry)) -> Result<()> {
            for e in fs.read_dir(dir)? {
                let path = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}\\{}", e.name) };
                f(&path, &e);
                if e.is_dir {
                    rec(fs, &e, &path, f)?;
                }
            }
            Ok(())
        }
        rec(self, &self.root, "", &mut f)
    }
}
