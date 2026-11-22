//! Access to the original Screamer Rally disc.
//!
//! The game needs two things from the CD: the ISO 9660 file system on the data track and
//! the Red Book audio tracks (the music). This crate exposes both, independent of how the
//! user stored the disc.
//!
//! Supported sources:
//! * Alcohol 120% `MDS` + `MDF` (raw 2352-byte sectors)
//! * CUE sheets with one or more BIN files (raw data or 2048-byte data sectors, audio)
//! * plain ISO 9660 images (data only: no music)

mod cue;
pub mod dat;
mod iso9660;
mod mds;
pub mod sha256;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use iso9660::{DirEntry, IsoFs};

/// Bytes in a raw CD sector.
pub const RAW_SECTOR: usize = 2352;
/// Sync pattern at the start of every data sector.
const SYNC: [u8; 12] = [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0];
/// Bytes of user data in a Mode 1 sector.
pub const DATA_SECTOR: usize = 2048;
/// CD-DA frames per second (sectors per second at 1x).
pub const FRAMES_PER_SECOND: u32 = 75;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Format(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Format(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn format_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Format(msg.into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    /// Mode 1 data (2048 bytes of user data per sector).
    Data,
    /// Red Book audio: 2352 bytes = 588 stereo 16-bit little-endian samples at 44.1 kHz.
    Audio,
}

/// One track of the disc, in absolute sector addresses (LBA, no 150-sector lead-in offset).
#[derive(Clone, Debug)]
pub struct Track {
    pub number: u8,
    pub kind: TrackKind,
    /// LBA of index 1 (where playback starts).
    pub start: u32,
    /// Number of sectors from `start` to the start of the next track (or the end of disc).
    pub length: u32,
}

impl Track {
    pub fn end(&self) -> u32 {
        self.start + self.length
    }
}

/// A run of consecutive sectors stored in one image file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Segment {
    /// First LBA.
    pub start: u32,
    pub count: u32,
    /// Index into the image's files.
    pub file: usize,
    /// Byte offset of `start` in the file.
    pub offset: u64,
    /// Bytes per stored sector: 2352 (raw) or 2048 (data only).
    pub stride: u32,
}

/// A disc image: a table of contents plus raw sector access.
pub struct Disc {
    path: PathBuf,
    files: Vec<Mutex<File>>,
    segments: Vec<Segment>,
    tracks: Vec<Track>,
    total_sectors: u32,
}

impl Disc {
    /// Open a disc image: `.mds` / `.mdf`, `.cue` or `.iso`.
    pub fn open(path: impl AsRef<Path>) -> Result<Disc> {
        let path = path.as_ref();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "mds" | "mdf" => {
                let mds_path = path.with_extension("mds");
                let mdf_path = path.with_extension("mdf");
                let desc = mds::parse(&std::fs::read(&mds_path)?)?;
                let file = File::open(&mdf_path)?;
                let size = file.metadata()?.len();
                if desc.sector_size as usize != RAW_SECTOR {
                    return format_err(format!(
                        "unsupported MDF sector size {} (only raw 2352-byte images are supported)",
                        desc.sector_size
                    ));
                }
                let (segments, tracks, total_sectors) = desc.layout(size)?;
                Ok(Disc { path: mdf_path, files: vec![Mutex::new(file)], segments, tracks, total_sectors })
            }
            "cue" => {
                let text = std::fs::read_to_string(path)?;
                let dir = path.parent().unwrap_or(Path::new("."));
                let size_of = |p: &Path| -> Result<u64> { Ok(std::fs::metadata(p)?.len()) };
                let (names, segments, tracks, total_sectors) = cue::layout(&text, dir, &size_of)?;
                let files = names.iter().map(|n| File::open(n).map(Mutex::new)).collect::<std::io::Result<Vec<_>>>()?;
                Ok(Disc { path: path.to_path_buf(), files, segments, tracks, total_sectors })
            }
            "iso" => {
                let file = File::open(path)?;
                let total_sectors = (file.metadata()?.len() / DATA_SECTOR as u64) as u32;
                Ok(Disc {
                    path: path.to_path_buf(),
                    files: vec![Mutex::new(file)],
                    segments: vec![Segment { start: 0, count: total_sectors, file: 0, offset: 0, stride: DATA_SECTOR as u32 }],
                    tracks: vec![Track { number: 1, kind: TrackKind::Data, start: 0, length: total_sectors }],
                    total_sectors,
                })
            }
            _ => format_err(format!("unsupported disc image type: {} (use .mds, .cue or .iso)", path.display())),
        }
    }

    fn segment(&self, lba: u32) -> Option<&Segment> {
        self.segments.iter().find(|s| lba >= s.start && lba < s.start + s.count)
    }

    fn read_stored(&self, seg: &Segment, lba: u32, count: u32, out: &mut [u8]) -> Result<()> {
        let mut f = self.files[seg.file].lock().unwrap();
        f.seek(SeekFrom::Start(seg.offset + (lba - seg.start) as u64 * seg.stride as u64))?;
        f.read_exact(&mut out[..(count * seg.stride) as usize])?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn track(&self, number: u8) -> Option<&Track> {
        self.tracks.iter().find(|t| t.number == number)
    }

    /// Total number of sectors of the disc.
    pub fn total_sectors(&self) -> u32 {
        self.total_sectors
    }

    /// Read `count` raw 2352-byte sectors starting at `lba`. Sectors not stored in the image
    /// (pregaps) read as silence; data sectors stored without their headers get a Mode 1
    /// header (sync, address, mode) and zero error-correction bytes.
    pub fn read_raw(&self, lba: u32, count: u32, out: &mut [u8]) -> Result<()> {
        let need = count as usize * RAW_SECTOR;
        if out.len() < need {
            return format_err("buffer too small for raw read");
        }
        if lba.checked_add(count).is_none_or(|e| e > self.total_sectors) {
            return format_err(format!("read past end of disc (lba {lba}, count {count})"));
        }
        let mut done = 0;
        while done < count {
            let at = lba + done;
            let dst = &mut out[done as usize * RAW_SECTOR..];
            match self.segment(at) {
                Some(seg) if seg.stride as usize == RAW_SECTOR => {
                    let n = (count - done).min(seg.start + seg.count - at);
                    self.read_stored(seg, at, n, dst)?;
                    done += n;
                }
                Some(seg) => {
                    let sector = &mut dst[..RAW_SECTOR];
                    sector.fill(0);
                    sector[..12].copy_from_slice(&SYNC);
                    let (m, s, f) = lba_to_msf(at);
                    let bcd = |v: u8| (v / 10) << 4 | (v % 10);
                    sector[12..16].copy_from_slice(&[bcd(m), bcd(s), bcd(f), 1]);
                    self.read_stored(seg, at, 1, &mut sector[16..16 + DATA_SECTOR])?;
                    done += 1;
                }
                None => {
                    dst[..RAW_SECTOR].fill(0);
                    done += 1;
                }
            }
        }
        Ok(())
    }

    /// Read the 2048 bytes of user data of a Mode 1 sector.
    pub fn read_data_sector(&self, lba: u32, out: &mut [u8; DATA_SECTOR]) -> Result<()> {
        if let Some(seg) = self.segment(lba).filter(|s| s.stride as usize == DATA_SECTOR) {
            return self.read_stored(seg, lba, 1, out);
        }
        let mut raw = [0u8; RAW_SECTOR];
        self.read_raw(lba, 1, &mut raw)?;
        if raw[..12] != SYNC {
            return format_err(format!("sector {lba} has no data sync pattern"));
        }
        if raw[15] != 1 {
            return format_err(format!("sector {lba} is mode {}, expected mode 1", raw[15]));
        }
        out.copy_from_slice(&raw[16..16 + DATA_SECTOR]);
        Ok(())
    }

    /// Read user data spanning `size` bytes from consecutive Mode 1 sectors.
    pub fn read_data(&self, lba: u32, size: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(size.div_ceil(DATA_SECTOR) * DATA_SECTOR);
        let mut sector = [0u8; DATA_SECTOR];
        for i in 0..size.div_ceil(DATA_SECTOR) as u32 {
            self.read_data_sector(lba + i, &mut sector)?;
            out.extend_from_slice(&sector);
        }
        out.truncate(size);
        Ok(out)
    }

    /// Mount the ISO 9660 file system of the first data track.
    pub fn filesystem(&self) -> Result<IsoFs<'_>> {
        IsoFs::mount(self)
    }
}

/// Convert an LBA to the MSF address used by MSCDEX/Red Book (with the 2-second lead-in).
pub fn lba_to_msf(lba: u32) -> (u8, u8, u8) {
    let a = lba + 150;
    ((a / (60 * 75)) as u8, ((a / 75) % 60) as u8, (a % 75) as u8)
}

/// Convert an MSF address (with lead-in) to an LBA.
pub fn msf_to_lba(m: u8, s: u8, f: u8) -> u32 {
    (m as u32 * 60 + s as u32) * 75 + f as u32 - 150
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msf_roundtrip() {
        assert_eq!(lba_to_msf(0), (0, 2, 0));
        for lba in [0, 1, 74, 75, 4499, 116140, 295_000] {
            let (m, s, f) = lba_to_msf(lba);
            assert_eq!(msf_to_lba(m, s, f), lba);
        }
    }
}
