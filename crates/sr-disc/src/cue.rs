//! CUE sheets (`.cue` + one or more `.bin` files), as written by most ripping tools.
//!
//! Supported: `FILE "name" BINARY`, `TRACK nn MODE1/2352 | MODE1/2048 | AUDIO`,
//! `INDEX 00/01 mm:ss:ff`, `PREGAP mm:ss:ff` (sectors not stored in the file: silence).
//! Single-file and one-file-per-track layouts both work.

use std::path::{Path, PathBuf};

use crate::{RAW_SECTOR, Result, Segment, Track, TrackKind, format_err};

struct CueTrack {
    number: u8,
    kind: TrackKind,
    stride: u32,
    file: usize,
    /// Sector in the file of the first index (00 or 01).
    first: u32,
    /// Sector in the file of index 01.
    index1: u32,
    pregap: u32,
}

fn msf(s: &str) -> Result<u32> {
    let p: Vec<&str> = s.split(':').collect();
    let n = |i: usize| p.get(i).and_then(|v| v.parse::<u32>().ok()).ok_or(crate::Error::Format(format!("bad time {s}")));
    if p.len() != 3 {
        return format_err(format!("bad time {s}"));
    }
    Ok((n(0)? * 60 + n(1)?) * 75 + n(2)?)
}

/// Words of a CUE line, with quoted names kept whole.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parse `text` (the sheet in `dir`); `size_of` gives each data file's size in bytes.
pub(crate) fn layout(text: &str, dir: &Path, size_of: &dyn Fn(&Path) -> Result<u64>) -> Result<(Vec<PathBuf>, Vec<Segment>, Vec<Track>, u32)> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut tracks: Vec<CueTrack> = Vec::new();
    for line in text.lines() {
        let w = words(line);
        let Some(cmd) = w.first() else { continue };
        match cmd.to_ascii_uppercase().as_str() {
            "FILE" => {
                let name = w.get(1).ok_or(crate::Error::Format("FILE without a name".into()))?;
                files.push(dir.join(name));
            }
            "TRACK" => {
                if files.is_empty() {
                    return format_err("TRACK before FILE");
                }
                let number: u8 = w.get(1).and_then(|n| n.parse().ok()).ok_or(crate::Error::Format("bad TRACK number".into()))?;
                let mode = w.get(2).map(|m| m.to_ascii_uppercase()).unwrap_or_default();
                let (kind, stride) = match mode.as_str() {
                    "AUDIO" => (TrackKind::Audio, RAW_SECTOR as u32),
                    "MODE1/2352" => (TrackKind::Data, RAW_SECTOR as u32),
                    "MODE1/2048" => (TrackKind::Data, 2048),
                    m => return format_err(format!("track {number}: unsupported mode {m}")),
                };
                tracks.push(CueTrack { number, kind, stride, file: files.len() - 1, first: u32::MAX, index1: u32::MAX, pregap: 0 });
            }
            "INDEX" => {
                let t = tracks.last_mut().ok_or(crate::Error::Format("INDEX before TRACK".into()))?;
                let idx: u32 = w.get(1).and_then(|n| n.parse().ok()).unwrap_or(1);
                let at = msf(w.get(2).map(String::as_str).unwrap_or(""))?;
                t.first = t.first.min(at);
                if idx == 1 {
                    t.index1 = at;
                }
            }
            "PREGAP" => {
                let t = tracks.last_mut().ok_or(crate::Error::Format("PREGAP before TRACK".into()))?;
                t.pregap = msf(w.get(1).map(String::as_str).unwrap_or(""))?;
            }
            _ => {}
        }
    }
    if tracks.is_empty() {
        return format_err("CUE sheet has no tracks");
    }
    if let Some(t) = tracks.iter().find(|t| t.index1 == u32::MAX) {
        return format_err(format!("track {} has no INDEX 01", t.number));
    }
    let mut segments = Vec::new();
    let mut out = Vec::new();
    let mut lba = 0u32;
    for (fi, path) in files.iter().enumerate() {
        let in_file: Vec<&CueTrack> = tracks.iter().filter(|t| t.file == fi).collect();
        let Some(first) = in_file.first() else { continue };
        let stride = first.stride;
        if in_file.iter().any(|t| t.stride != stride) {
            return format_err(format!("{}: tracks with different sector sizes in one file", path.display()));
        }
        let sectors = (size_of(path)? / stride as u64) as u32;
        let base = lba;
        let mut shift = 0;
        for (j, t) in in_file.iter().enumerate() {
            shift += t.pregap;
            let end = in_file.get(j + 1).map_or(sectors, |n| n.first);
            if end < t.first || t.first > sectors {
                return format_err(format!("track {}: indexes outside its file", t.number));
            }
            segments.push(Segment { start: base + t.first + shift, count: end - t.first, file: fi, offset: t.first as u64 * stride as u64, stride });
            out.push(Track { number: t.number, kind: t.kind, start: base + t.index1 + shift, length: 0 });
        }
        lba = base + sectors + shift;
    }
    for i in 0..out.len() {
        let end = out.get(i + 1).map_or(lba, |n| n.start);
        out[i].length = end.saturating_sub(out[i].start);
    }
    Ok((files, segments, out, lba))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes(map: &'static [(&'static str, u64)]) -> impl Fn(&Path) -> Result<u64> {
        move |p: &Path| {
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            map.iter().find(|(n, _)| *n == name).map(|(_, s)| *s).ok_or(crate::Error::Format("missing".into()))
        }
    }

    #[test]
    fn single_file() {
        let cue = "FILE \"Game Disc.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 01:00:00\n    INDEX 01 01:02:00\n";
        let f = sizes(&[("Game Disc.bin", 2352 * 9000)]);
        let (files, segs, tracks, total) = layout(cue, Path::new("d"), &f).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(total, 9000);
        assert_eq!(tracks[0].start, 0);
        assert_eq!(tracks[0].length, 4650); // up to index 01 of track 2
        assert_eq!(tracks[1].start, 4650);
        assert_eq!(tracks[1].length, 9000 - 4650);
        assert_eq!(segs[1].start, 4500);
        assert_eq!(segs[1].offset, 4500 * 2352);
    }

    #[test]
    fn clonecd_style_sheet() {
        // as CloneCD writes it next to an .img: one file, track and index numbers without
        // leading zeros, no INDEX 00 (the English Screamer Rally image's sheet looks like this)
        let cue = "FILE \"disc.img\" BINARY\n   TRACK 1 MODE1/2352\n   INDEX 1 00:00:00\n   TRACK 2 AUDIO\n   INDEX 1 18:17:41\n   TRACK 3 AUDIO\n   INDEX 1 22:43:35\n";
        let f = sizes(&[("disc.img", 2352 * 120_000)]);
        let (files, segs, tracks, total) = layout(cue, Path::new("d"), &f).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(total, 120_000);
        assert_eq!(tracks.iter().map(|t| t.number).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!((tracks[1].start, tracks[1].length), (82316, 102260 - 82316));
        assert_eq!((tracks[2].start, tracks[2].length), (102260, 120_000 - 102260));
        assert_eq!(tracks[1].kind, TrackKind::Audio);
        assert_eq!((segs[2].start, segs[2].offset), (102260, 102260 * 2352));
    }

    #[test]
    fn file_per_track_with_pregap() {
        let cue = "FILE \"t1.bin\" BINARY\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\nFILE \"t2.bin\" BINARY\nTRACK 02 AUDIO\nPREGAP 00:02:00\nINDEX 01 00:00:00\nFILE \"t3.bin\" BINARY\nTRACK 03 AUDIO\nINDEX 00 00:00:00\nINDEX 01 00:02:00\n";
        let f = sizes(&[("t1.bin", 2048 * 1000), ("t2.bin", 2352 * 300), ("t3.bin", 2352 * 400)]);
        let (_, segs, tracks, total) = layout(cue, Path::new("d"), &f).unwrap();
        assert_eq!(tracks[0].length, 1150); // up to track 2, including its pregap
        assert_eq!(tracks[1].start, 1150);
        assert_eq!(segs[1].start, 1150);
        assert_eq!(tracks[2].start, 1150 + 300 + 150);
        assert_eq!(segs[2].start, 1450);
        assert_eq!(total, 1450 + 400);
        assert_eq!(segs[0].stride, 2048);
    }
}
