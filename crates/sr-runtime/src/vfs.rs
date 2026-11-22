//! The game's view of the file system.
//!
//! * `C:` is a writable host folder (the "install"). Under `C:\SR` it is overlaid on the
//!   CD's root directory, so the game sees a complete installation while every write
//!   (configuration, replays, high scores) lands in the host folder and the CD image is
//!   never modified.
//! * `D:` is the CD (read-only), served straight from the user's disc image.
//! * Without a CD (the DOS release installed in a folder, whose executable runs without its
//!   CD: [`Vfs::from_folder`]) `C:\SR` is overlaid on that folder instead, read-only too (a
//!   file the game writes is copied into the host folder first), and there is no `D:`.

use std::fs;
use std::path::PathBuf;

use sr_disc::{DirEntry, Disc, IsoFs};

pub const CD_DRIVE: u8 = 3; // D:
pub const INSTALL_DIR: &str = "C:\\SR";

#[derive(Clone, Debug)]
pub enum Node {
    HostFile(PathBuf),
    HostDir(PathBuf),
    CdFile(DirEntry),
    CdDir(DirEntry),
}

impl Node {
    pub fn is_dir(&self) -> bool {
        matches!(self, Node::HostDir(_) | Node::CdDir(_))
    }
}

/// One directory listing entry (merged from host and CD).
#[derive(Clone, Debug)]
pub struct ListEntry {
    pub name: String,
    pub size: u32,
    pub is_dir: bool,
    pub read_only: bool,
    /// DOS packed date and time.
    pub date: u16,
    pub time: u16,
}

pub struct Vfs {
    /// The CD's file system and the disc (`None`: a game folder, no CD).
    pub iso: Option<IsoFs<'static>>,
    pub disc: Option<&'static Disc>,
    /// The installed game's folder that `C:\SR` shows (`None`: the CD's root does).
    pub game_dir: Option<PathBuf>,
    /// Host folder that is drive C:.
    pub c_root: PathBuf,
    pub cur_drive: u8,
    /// Current directory per drive, without drive letter, e.g. `\SR`.
    pub cwd: [String; 26],
}

pub fn dos_datetime(y: u16, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> (u16, u16) {
    let date = ((y.saturating_sub(1980)) << 9) | ((mo as u16) << 5) | d as u16;
    let time = ((h as u16) << 11) | ((mi as u16) << 5) | (s as u16 / 2);
    (date, time)
}

fn host_datetime(meta: &fs::Metadata) -> (u16, u16) {
    use std::time::UNIX_EPOCH;
    let secs = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let (y, mo, d, h, mi, s) = crate::time::civil_from_unix(secs as i64);
    dos_datetime(y as u16, mo, d, h, mi, s)
}

impl Vfs {
    pub fn new(disc: &'static Disc, c_root: PathBuf) -> Result<Vfs, String> {
        let iso = disc.filesystem().map_err(|e| e.to_string())?;
        Self::make(Some(iso), Some(disc), None, c_root)
    }

    /// `C:\SR` over the installed game in `game_dir` (its `STRT3FX.EXE`, `DATA`, `GFX` ...),
    /// no CD.
    pub fn from_folder(game_dir: PathBuf, c_root: PathBuf) -> Result<Vfs, String> {
        if !game_dir.is_dir() {
            return Err(format!("{} is not a folder", game_dir.display()));
        }
        Self::make(None, None, Some(game_dir), c_root)
    }

    fn make(iso: Option<IsoFs<'static>>, disc: Option<&'static Disc>, game_dir: Option<PathBuf>, c_root: PathBuf) -> Result<Vfs, String> {
        fs::create_dir_all(c_root.join("SR")).map_err(|e| format!("cannot create {}: {e}", c_root.display()))?;
        let mut cwd: [String; 26] = Default::default();
        for c in cwd.iter_mut() {
            *c = "\\".into();
        }
        cwd[2] = "\\SR".into();
        Ok(Vfs { iso, disc, game_dir, c_root, cur_drive: 2, cwd })
    }

    /// The host path of `rel` (a path below `C:\SR`) in the game folder.
    fn game_path(&self, rel: &str) -> Option<PathBuf> {
        let mut p = self.game_dir.clone()?;
        for part in rel.split('\\').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        Some(p)
    }

    /// Make a DOS path absolute and canonical: `C:\SR\GFX\MAP11.DAT`.
    pub fn absolute(&self, path: &[u8]) -> String {
        let p = String::from_utf8_lossy(path).replace('/', "\\").to_ascii_uppercase();
        let (drive, rest) = if p.len() >= 2 && p.as_bytes()[1] == b':' {
            (p.as_bytes()[0] - b'A', p[2..].to_string())
        } else {
            (self.cur_drive, p)
        };
        let full = if rest.starts_with('\\') {
            rest
        } else {
            let cwd = &self.cwd[drive as usize % 26];
            if cwd.ends_with('\\') { format!("{cwd}{rest}") } else { format!("{cwd}\\{rest}") }
        };
        let mut parts: Vec<&str> = Vec::new();
        for part in full.split('\\') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        format!("{}:\\{}", (b'A' + drive) as char, parts.join("\\"))
    }

    fn split(abs: &str) -> (u8, &str) {
        (abs.as_bytes()[0] - b'A', abs[3..].trim_end_matches('\\'))
    }

    fn host_path(&self, rel: &str) -> PathBuf {
        let mut p = self.c_root.clone();
        for part in rel.split('\\').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        p
    }

    /// The CD path an install-relative path maps to, if it is inside `C:\SR`.
    fn overlay_cd_path(rel: &str) -> Option<&str> {
        if rel.eq_ignore_ascii_case("SR") {
            Some("")
        } else if rel.len() > 3 && rel[..3].eq_ignore_ascii_case("SR\\") {
            Some(&rel[3..])
        } else {
            None
        }
    }

    fn cd_lookup(&self, path: &str) -> Option<Node> {
        match self.iso.as_ref()?.lookup(path) {
            Ok(Some(e)) if e.is_dir => Some(Node::CdDir(e)),
            Ok(Some(e)) => Some(Node::CdFile(e)),
            _ => None,
        }
    }

    pub fn lookup(&self, abs: &str) -> Option<Node> {
        let (drive, rel) = Self::split(abs);
        match drive {
            2 => {
                let host = self.host_path(rel);
                if let Ok(meta) = fs::metadata(&host) {
                    return Some(if meta.is_dir() { Node::HostDir(host) } else { Node::HostFile(host) });
                }
                let below = Self::overlay_cd_path(rel)?;
                if self.game_dir.is_some() {
                    let p = self.game_path(below)?;
                    let meta = fs::metadata(&p).ok()?;
                    return Some(if meta.is_dir() { Node::HostDir(p) } else { Node::HostFile(p) });
                }
                self.cd_lookup(below)
            }
            CD_DRIVE => self.cd_lookup(rel),
            _ => None,
        }
    }

    /// The last `len` bytes of file `abs` (all of it when shorter); `None` when there is no
    /// such file or it cannot be read.
    pub fn read_tail(&self, abs: &str, len: usize) -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        match self.lookup(abs)? {
            Node::HostFile(p) => {
                let mut f = fs::File::open(p).ok()?;
                let size = f.metadata().ok()?.len();
                let n = (len as u64).min(size);
                f.seek(SeekFrom::Start(size - n)).ok()?;
                let mut out = vec![0; n as usize];
                f.read_exact(&mut out).ok()?;
                Some(out)
            }
            Node::CdFile(e) => {
                let n = len.min(e.size as usize);
                let pos = e.size as usize - n;
                let skip = pos % 2048;
                let data = self.disc?.read_data(e.lba + (pos / 2048) as u32, skip + n).ok()?;
                Some(data.get(skip..skip + n)?.to_vec())
            }
            _ => None,
        }
    }

    /// The entry names of the game's `.DAT` archive `abs` (its directory only is read).
    pub fn archive_names(&self, abs: &str) -> Option<Vec<String>> {
        let last = self.read_tail(abs, 4)?;
        let dir = sr_disc::dat::directory_size(last.as_slice().try_into().ok()?);
        sr_disc::dat::names_from_tail(&self.read_tail(abs, dir + 4)?).ok()
    }

    pub fn drive_exists(&self, drive: u8) -> bool {
        drive == 2 || (drive == CD_DRIVE && self.iso.is_some())
    }

    /// Host path for writing `abs` (only drive C: is writable). Creates parent folders.
    /// If the file only exists on the CD overlay and `copy_existing`, it is copied first.
    pub fn writable(&self, abs: &str, copy_existing: bool) -> Result<PathBuf, u16> {
        let (drive, rel) = Self::split(abs);
        if drive != 2 {
            return Err(5); // access denied
        }
        let host = self.host_path(rel);
        if let Some(parent) = host.parent() {
            if !parent.exists() {
                // only create folders that exist in the merged view
                let parent_abs = abs.rsplit_once('\\').map(|(p, _)| p).unwrap_or("C:\\");
                let parent_abs = if parent_abs.len() == 2 { format!("{parent_abs}\\") } else { parent_abs.to_string() };
                match self.lookup(&parent_abs) {
                    Some(n) if n.is_dir() => fs::create_dir_all(parent).map_err(|_| 3u16)?,
                    _ => return Err(3), // path not found
                }
            }
        }
        if copy_existing && !host.exists() {
            match self.lookup(abs) {
                Some(Node::CdFile(e)) => {
                    let data = self.iso.as_ref().ok_or(5u16)?.read_file(&e).map_err(|_| 5u16)?;
                    fs::write(&host, data).map_err(|_| 5u16)?;
                }
                // the game folder's file: the folder is never written
                Some(Node::HostFile(src)) if src != host => {
                    fs::copy(&src, &host).map_err(|_| 5u16)?;
                }
                _ => {}
            }
        }
        Ok(host)
    }

    /// List the directory `dir_abs`, merging the host folder and the CD overlay.
    pub fn list(&self, dir_abs: &str) -> Vec<ListEntry> {
        let mut out: Vec<ListEntry> = Vec::new();
        let (drive, rel) = Self::split(dir_abs);
        let push = |e: ListEntry, out: &mut Vec<ListEntry>| {
            if !out.iter().any(|x| x.name == e.name) {
                out.push(e);
            }
        };
        if drive == 2 {
            if let Ok(rd) = fs::read_dir(self.host_path(rel)) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_ascii_uppercase();
                    if let Ok(meta) = e.metadata() {
                        let (date, time) = host_datetime(&meta);
                        push(
                            ListEntry {
                                name,
                                size: meta.len().min(u32::MAX as u64) as u32,
                                is_dir: meta.is_dir(),
                                read_only: false,
                                date,
                                time,
                            },
                            &mut out,
                        );
                    }
                }
            }
        }
        let cd_dir = match drive {
            2 => Self::overlay_cd_path(rel),
            CD_DRIVE => Some(rel),
            _ => None,
        };
        // the game folder below the host folder
        if let Some(dir) = cd_dir.filter(|_| drive == 2).and_then(|r| self.game_path(r)) {
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() {
                    if let Ok(meta) = e.metadata() {
                        let (date, time) = host_datetime(&meta);
                        let name = e.file_name().to_string_lossy().to_ascii_uppercase();
                        push(ListEntry { name, size: meta.len().min(u32::MAX as u64) as u32, is_dir: meta.is_dir(), read_only: false, date, time }, &mut out);
                    }
                }
            }
            return out;
        }
        if let (Some(Node::CdDir(d)), Some(iso)) = (cd_dir.and_then(|p| self.cd_lookup(p)), self.iso.as_ref()) {
            if let Ok(entries) = iso.read_dir(&d) {
                for e in entries {
                    let (y, mo, da, h, mi, s) = e.time;
                    let (date, time) = dos_datetime(y, mo, da, h, mi, s);
                    push(
                        ListEntry { name: e.name.clone(), size: e.size, is_dir: e.is_dir, read_only: drive == CD_DRIVE, date, time },
                        &mut out,
                    );
                }
            }
        }
        out
    }
}

/// DOS wildcard match on 8.3 names (`*.RPL`, `REP????.RPL`, `*.*`).
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    fn split83(s: &str) -> (String, String) {
        match s.rsplit_once('.') {
            Some((b, e)) => (b.to_string(), e.to_string()),
            None => (s.to_string(), String::new()),
        }
    }
    fn part(p: &str, n: &str, len: usize) -> bool {
        let p: Vec<char> = p.chars().collect();
        let n: Vec<char> = n.chars().collect();
        let mut i = 0;
        while i < len {
            match p.get(i) {
                Some('*') => return true,
                Some('?') => {}
                Some(&c) => {
                    if n.get(i) != Some(&c) {
                        return false;
                    }
                }
                None => return n.get(i).is_none(),
            }
            i += 1;
        }
        true
    }
    let (pb, pe) = split83(&pattern.to_ascii_uppercase());
    let (nb, ne) = split83(&name.to_ascii_uppercase());
    part(&pb, &nb, 8) && part(&pe, &ne, 3)
}

#[cfg(test)]
mod tests {
    use super::wildcard_match;

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.*", "GAME.DAT"));
        assert!(wildcard_match("*.RPL", "REP0001.RPL"));
        assert!(!wildcard_match("*.RPL", "REP0001.DAT"));
        assert!(wildcard_match("REP????.RPL", "REP0001.RPL"));
        assert!(wildcard_match("MAP?1.DAT", "MAP21.DAT"));
        assert!(!wildcard_match("MAP?1.DAT", "MAP22.DAT"));
        assert!(wildcard_match("*", "README"));
    }
}
