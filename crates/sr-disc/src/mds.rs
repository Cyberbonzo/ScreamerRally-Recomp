//! Alcohol 120% Media Descriptor Sidecar (`.mds`) parser, version 1.x.
//!
//! Layout (all little endian):
//! * header, 0x58 bytes: `"MEDIA DESCRIPTOR"`, …, session count at 0x14,
//!   session block offset at 0x50
//! * session block, 24 bytes: start, end, number, block count, …, track block offset
//! * track block, 80 bytes: mode, sub-channel, ADR/CTL, TNO, point, …, extra offset (0x0C),
//!   sector size (0x10), start sector (0x24), start offset in the MDF (0x28, 64-bit)
//! * extra block, 8 bytes: pregap, length
//!
//! The MDF does not always hold every sector of the disc: the Italian Screamer Rally image
//! leaves out the 150-sector pregap of track 2, so from track 2 on a sector's place in the
//! file is its LBA minus 150. Each track is therefore read from its own start offset.

use crate::{RAW_SECTOR, Result, Segment, Track, TrackKind, format_err};

pub(crate) struct Descriptor {
    pub sector_size: u16,
    entries: Vec<Entry>,
}

struct Entry {
    point: u8,
    mode: u8,
    start: u32,
    /// Byte offset of the track's first stored sector (the sector at `start`) in the MDF.
    offset: u64,
}

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(crate::Error::Format("truncated MDS".into()))
}

fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(crate::Error::Format("truncated MDS".into()))
}

fn u64_at(d: &[u8], o: usize) -> Result<u64> {
    Ok(u32_at(d, o)? as u64 | (u32_at(d, o + 4)? as u64) << 32)
}

pub(crate) fn parse(d: &[u8]) -> Result<Descriptor> {
    if d.len() < 0x58 || &d[..16] != b"MEDIA DESCRIPTOR" {
        return format_err("not an MDS descriptor");
    }
    let sessions = u16_at(d, 0x14)?;
    let session_off = u32_at(d, 0x50)? as usize;
    let mut entries = Vec::new();
    let mut sector_size = 0;
    for s in 0..sessions as usize {
        let so = session_off + s * 24;
        let blocks = *d.get(so + 10).ok_or(crate::Error::Format("truncated MDS".into()))? as usize;
        let tb_off = u32_at(d, so + 20)? as usize;
        for b in 0..blocks {
            let o = tb_off + b * 80;
            let mode = *d.get(o).ok_or(crate::Error::Format("truncated MDS".into()))?;
            let point = *d.get(o + 4).ok_or(crate::Error::Format("truncated MDS".into()))?;
            if point >= 0xa0 {
                continue; // lead-in descriptors (A0/A1/A2)
            }
            let ss = u16_at(d, o + 0x10)?;
            if sector_size == 0 {
                sector_size = ss;
            } else if ss != sector_size {
                return format_err("mixed sector sizes in one MDF are not supported");
            }
            entries.push(Entry { point, mode, start: u32_at(d, o + 0x24)?, offset: u64_at(d, o + 0x28)? });
        }
    }
    if entries.is_empty() {
        return format_err("MDS has no tracks");
    }
    entries.sort_by_key(|e| e.point);
    Ok(Descriptor { sector_size, entries })
}

impl Descriptor {
    /// The tracks, the sectors stored in the MDF (one segment per track, from the track's
    /// start offset up to the next track's, the last one up to the end of the file) and the
    /// disc's total number of sectors. Sectors between segments (pregaps the image left out)
    /// read as silence.
    pub fn layout(self, file_size: u64) -> Result<(Vec<Segment>, Vec<Track>, u32)> {
        let raw = RAW_SECTOR as u64;
        let mut segments = Vec::with_capacity(self.entries.len());
        for (i, e) in self.entries.iter().enumerate() {
            let end = self.entries.get(i + 1).map_or(file_size, |n| n.offset);
            if end < e.offset || e.offset % raw != 0 || end > file_size {
                return format_err(format!("track {}: start offset {} outside the MDF or out of order", e.point, e.offset));
            }
            let count = ((end - e.offset) / raw) as u32;
            let overlaps = |n: &&Entry| e.start.checked_add(count).is_none_or(|last| last > n.start);
            if let Some(n) = self.entries.get(i + 1).filter(overlaps) {
                return format_err(format!("track {} overlaps track {}", e.point, n.point));
            }
            segments.push(Segment { start: e.start, count, file: 0, offset: e.offset, stride: RAW_SECTOR as u32 });
        }
        let last = segments.last().expect("at least one track");
        let total_sectors = last.start + last.count;
        let mut tracks = Vec::with_capacity(self.entries.len());
        for (i, e) in self.entries.iter().enumerate() {
            let kind = match e.mode {
                0xa9 => TrackKind::Audio,
                0xaa => TrackKind::Data,
                m => return format_err(format!("track {} has unsupported mode 0x{m:02x}", e.point)),
            };
            let end = self.entries.get(i + 1).map_or(total_sectors, |n| n.start);
            tracks.push(Track { number: e.point, kind, start: e.start, length: end - e.start });
        }
        Ok((segments, tracks, total_sectors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-session MDS with the given (point, mode, start LBA, start offset) blocks.
    fn mds(blocks: &[(u8, u8, u32, u64)]) -> Vec<u8> {
        let mut d = vec![0u8; 0x58 + 24 + 80 * blocks.len()];
        d[..16].copy_from_slice(b"MEDIA DESCRIPTOR");
        d[0x14..0x16].copy_from_slice(&1u16.to_le_bytes());
        d[0x50..0x54].copy_from_slice(&0x58u32.to_le_bytes());
        d[0x58 + 10] = blocks.len() as u8;
        d[0x58 + 20..0x58 + 24].copy_from_slice(&(0x58u32 + 24).to_le_bytes());
        for (i, &(point, mode, start, offset)) in blocks.iter().enumerate() {
            let o = 0x58 + 24 + 80 * i;
            d[o] = mode;
            d[o + 4] = point;
            d[o + 0x10..o + 0x12].copy_from_slice(&(RAW_SECTOR as u16).to_le_bytes());
            d[o + 0x24..o + 0x28].copy_from_slice(&start.to_le_bytes());
            d[o + 0x28..o + 0x30].copy_from_slice(&offset.to_le_bytes());
        }
        d
    }

    const RAW: u64 = RAW_SECTOR as u64;

    #[test]
    fn contiguous_image() {
        // every sector stored: file sector = LBA (lead-out entry A2 ignored)
        let d = mds(&[(1, 0xaa, 0, 0), (2, 0xa9, 1150, 1150 * RAW), (0xa2, 0, 0, 0)]);
        let (segs, tracks, total) = parse(&d).unwrap().layout(1550 * RAW).unwrap();
        assert_eq!(total, 1550);
        assert_eq!((tracks[0].start, tracks[0].length), (0, 1150));
        assert_eq!((tracks[1].start, tracks[1].length), (1150, 400));
        assert_eq!((segs[0].start, segs[0].count, segs[0].offset), (0, 1150, 0));
        assert_eq!((segs[1].start, segs[1].count, segs[1].offset), (1150, 400, 1150 * RAW));
    }

    #[test]
    fn pregap_left_out() {
        // as the Italian Screamer Rally MDF: track 2's 150-sector pregap (LBA 1000..1150)
        // is not in the file, so from track 2 on file sector = LBA - 150
        let d = mds(&[(1, 0xaa, 0, 0), (2, 0xa9, 1150, 1000 * RAW), (3, 0xa9, 1450, 1300 * RAW)]);
        let (segs, tracks, total) = parse(&d).unwrap().layout(1700 * RAW).unwrap();
        assert_eq!(total, 1850);
        assert_eq!((segs[0].start, segs[0].count, segs[0].offset), (0, 1000, 0));
        assert_eq!((segs[1].start, segs[1].count, segs[1].offset), (1150, 300, 1000 * RAW));
        assert_eq!((segs[2].start, segs[2].count, segs[2].offset), (1450, 400, 1300 * RAW));
        assert_eq!((tracks[0].length, tracks[1].length, tracks[2].length), (1150, 300, 400));
    }

    #[test]
    fn overlapping_offsets_rejected() {
        // track 1 would need more sectors than there are LBAs before track 2
        let d = mds(&[(1, 0xaa, 0, 0), (2, 0xa9, 1000, 1150 * RAW)]);
        assert!(parse(&d).unwrap().layout(1500 * RAW).is_err());
    }
}
