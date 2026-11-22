//! The CD-ROM drive as MSCDEX presents it: device-driver requests (`int 2Fh AX=1510h`),
//! answered from the user's disc image.
//!
//! Data reads, the table of contents and the audio play state are complete. Audio output
//! itself is added with the mixer (Phase 4); until then playback advances silently in real
//! time, so the game's track-end detection and position queries behave as on hardware.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::Machine;
use crate::mem;
use sr_disc::{TrackKind, lba_to_msf};

const DONE: u16 = 0x0100;
const BUSY: u16 = 0x0200;
const ERROR: u16 = 0x8000;
const ERR_NOT_READY: u16 = 2;
const ERR_UNKNOWN_COMMAND: u16 = 3;
const ERR_SECTOR_NOT_FOUND: u16 = 8;

/// Streams CD-DA sectors from the disc image into the mixer.
struct CdStream {
    disc: &'static sr_disc::Disc,
    next: u32,
    end: u32,
    pos: Arc<AtomicU32>,
    volume: Arc<AtomicU32>,
    buf: Vec<f32>,
    at: usize,
}

const READ_AHEAD: u32 = 25; // sectors per disc read (1/3 s)

impl sr_audio::Stream for CdStream {
    fn read(&mut self, out: &mut [f32]) -> usize {
        let frames = out.len() / 2;
        let vol = self.volume.load(Ordering::Relaxed);
        let (gl, gr) = ((vol & 0xff) as f32 / 255.0, ((vol >> 8) & 0xff) as f32 / 255.0);
        for f in 0..frames {
            if self.at >= self.buf.len() {
                if self.next >= self.end {
                    return f;
                }
                let n = READ_AHEAD.min(self.end - self.next);
                let mut raw = vec![0u8; n as usize * sr_disc::RAW_SECTOR];
                if self.disc.read_raw(self.next, n, &mut raw).is_err() {
                    return f;
                }
                self.buf.clear();
                self.buf.extend(raw.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0));
                self.at = 0;
                self.next += n;
            }
            out[f * 2] = self.buf[self.at] * gl;
            out[f * 2 + 1] = self.buf[self.at + 1] * gr;
            self.at += 2;
            // 588 frames per sector
            if self.at % (588 * 2) == 0 {
                self.pos.fetch_add(1, Ordering::Relaxed);
            }
        }
        frames
    }
}

pub struct CdAudio {
    audio: Option<sr_audio::Audio>,
    disc: Option<&'static sr_disc::Disc>,
    /// Play range [start, end) and the host time it started (used without audio output).
    playing: Option<(u32, u32, Duration)>,
    /// Current sector of the stream being played.
    pos: Arc<AtomicU32>,
    /// Paused position and end.
    paused: Option<(u32, u32)>,
    last_start: u32,
    last_end: u32,
    pub volume: [u8; 4],
    gain: Arc<AtomicU32>,
}

impl Default for CdAudio {
    fn default() -> Self {
        CdAudio {
            audio: None,
            disc: None,
            playing: None,
            pos: Arc::default(),
            paused: None,
            last_start: 0,
            last_end: 0,
            volume: [255; 4],
            gain: Arc::new(AtomicU32::new(0xffff)),
        }
    }
}

impl CdAudio {
    /// `disc`: `None` without a CD (a game folder: no music).
    pub fn new(disc: Option<&'static sr_disc::Disc>, audio: Option<sr_audio::Audio>) -> CdAudio {
        CdAudio { audio, disc, ..Default::default() }
    }

    /// Current position if audio is playing; clears the state at the end of the range.
    fn position(&mut self) -> Option<u32> {
        let (start, end, t0) = self.playing?;
        let pos = match &self.audio {
            Some(a) => {
                if !a.music_playing() {
                    end
                } else {
                    self.pos.load(Ordering::Relaxed)
                }
            }
            None => start + ((crate::time::uptime() - t0).as_secs_f64() * 75.0) as u32,
        };
        if pos >= end {
            self.playing = None;
            return None;
        }
        Some(pos)
    }

    fn start_stream(&mut self, start: u32, end: u32) {
        self.pos.store(start, Ordering::Relaxed);
        if let (Some(a), Some(disc)) = (&self.audio, self.disc) {
            a.set_music(Some(Box::new(CdStream {
                disc,
                next: start,
                end,
                pos: self.pos.clone(),
                volume: self.gain.clone(),
                buf: Vec::new(),
                at: 0,
            })));
        }
        self.playing = Some((start, end, crate::time::uptime()));
    }

    pub fn play(&mut self, start: u32, count: u32) {
        crate::info!("CD audio: play LBA {start} for {count} sectors ({:.1} s)", count as f64 / 75.0);
        self.paused = None;
        self.last_start = start;
        self.last_end = start + count;
        self.start_stream(start, start + count);
    }

    pub fn stop(&mut self) {
        if let Some(pos) = self.position() {
            self.paused = Some((pos, self.playing.unwrap().1));
            self.playing = None;
            if let Some(a) = &self.audio {
                a.set_music(None);
            }
        } else {
            // a second stop resets the resume point
            self.paused = None;
        }
    }

    pub fn resume(&mut self) {
        if let Some((pos, end)) = self.paused.take() {
            self.start_stream(pos, end);
        }
    }

    pub fn set_volume(&mut self, v: [u8; 4]) {
        self.volume = v;
        self.gain.store(v[0] as u32 | (v[1] as u32) << 8, Ordering::Relaxed);
    }
}

fn redbook(lba: u32) -> u32 {
    let (m, s, f) = lba_to_msf(lba);
    f as u32 | (s as u32) << 8 | (m as u32) << 16
}

fn from_redbook(v: u32) -> u32 {
    sr_disc::msf_to_lba((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn bcd(v: u8) -> u8 {
    (v / 10) << 4 | v % 10
}

/// Linear address of a real-mode far pointer stored at `at`.
fn far(at: u32) -> u32 {
    let (off, seg) = unsafe { (mem::rd16(at) as u32, mem::rd16(at + 2) as u32) };
    seg * 16 + off
}

pub fn device_request(m: &mut Machine, _drive: u8, req: u32) {
    let cmd = unsafe { mem::rd8(req + 2) };
    // without a CD (a game folder) there is no drive: the installation check says so
    // (`crate::services::bios::int2f`), a request anyway finds the drive not ready
    if m.vfs.disc.is_none() {
        unsafe { mem::wr16(req + 3, ERROR | DONE | ERR_NOT_READY) };
        return;
    }
    let status = match cmd {
        3 => ioctl_input(m, req),
        12 => ioctl_output(m, req),
        13 | 14 => DONE, // device open / close
        128 => read_long(m, req),
        131 => DONE, // seek
        132 => {
            let mode = unsafe { mem::rd8(req + 0x0d) };
            let (start, count) = unsafe { (mem::rd32(req + 0x0e), mem::rd32(req + 0x12)) };
            let start = if mode == 1 { from_redbook(start) } else { start };
            m.cd.play(start, count);
            DONE
        }
        133 => {
            m.cd.stop();
            crate::debug!("CD audio: stop");
            DONE
        }
        136 => {
            m.cd.resume();
            DONE
        }
        _ => {
            crate::warn!("MSCDEX: unknown device command {cmd}");
            ERROR | ERR_UNKNOWN_COMMAND
        }
    };
    let busy = if m.cd.position().is_some() { BUSY } else { 0 };
    unsafe { mem::wr16(req + 3, status | busy) };
}

fn read_long(m: &mut Machine, req: u32) -> u16 {
    let (mode, count, start) = unsafe { (mem::rd8(req + 0x0d), mem::rd16(req + 0x12), mem::rd32(req + 0x14)) };
    let start = if mode == 1 { from_redbook(start) } else { start };
    let Some(dest) = m.lowmem.translate(far(req + 0x0e)) else {
        crate::warn!("MSCDEX read: transfer address outside DOS memory");
        return ERROR | ERR_SECTOR_NOT_FOUND;
    };
    crate::debug!("MSCDEX read {count} sector(s) at {start}");
    let mut sector = [0u8; 2048];
    let Some(disc) = m.vfs.disc else { return ERROR | ERR_NOT_READY };
    for i in 0..count as u32 {
        if disc.read_data_sector(start + i, &mut sector).is_err() {
            return ERROR | ERR_SECTOR_NOT_FOUND;
        }
        unsafe { mem::slice_mut(dest + i * 2048, 2048).copy_from_slice(&sector) };
    }
    DONE
}

fn ioctl_input(m: &mut Machine, req: u32) -> u16 {
    let Some(cb) = m.lowmem.translate(far(req + 0x0e)) else { return ERROR | ERR_SECTOR_NOT_FOUND };
    let code = unsafe { mem::rd8(cb) };
    let Some(disc) = m.vfs.disc else { return ERROR | ERR_NOT_READY };
    let tracks = disc.tracks();
    let last_track = tracks.iter().map(|t| t.number).max().unwrap_or(1);
    let leadout = disc.total_sectors();
    unsafe {
        match code {
            0 => mem::wr32(cb + 1, 0), // device header address
            1 => {
                let pos = m.cd.position().unwrap_or(0);
                let mode = mem::rd8(cb + 1);
                mem::wr32(cb + 2, if mode == 1 { redbook(pos) } else { pos });
            }
            4 => {
                for ch in 0..4u32 {
                    mem::wr8(cb + 1 + ch * 2, ch as u8);
                    mem::wr8(cb + 2 + ch * 2, m.cd.volume[ch as usize]);
                }
            }
            6 => mem::wr32(cb + 1, 0x0000_0210), // door closed, data+audio, raw reads, no prefetch
            7 => mem::wr16(cb + 2, 2048),
            8 => mem::wr32(cb + 1, leadout),
            9 => mem::wr8(cb + 1, 1), // media not changed
            10 => {
                mem::wr8(cb + 1, 1);
                mem::wr8(cb + 2, last_track);
                mem::wr32(cb + 3, redbook(leadout));
            }
            11 => {
                let n = mem::rd8(cb + 1);
                match tracks.iter().find(|t| t.number == n) {
                    Some(t) => {
                        mem::wr32(cb + 2, redbook(t.start));
                        mem::wr8(cb + 6, if t.kind == TrackKind::Data { 0x41 } else { 0x01 });
                    }
                    None => return ERROR | ERR_SECTOR_NOT_FOUND,
                }
            }
            12 => {
                let pos = m.cd.position().unwrap_or(m.cd.last_start);
                let t = tracks.iter().rev().find(|t| t.start <= pos).unwrap_or(&tracks[0]);
                let (rm, rs, rf) = lba_to_msf(pos - t.start);
                let (am, as_, af) = lba_to_msf(pos);
                let rel = [rm, rs.saturating_sub(2), rf]; // relative time has no 2 s lead-in
                mem::wr8(cb + 1, if t.kind == TrackKind::Data { 0x41 } else { 0x01 });
                mem::wr8(cb + 2, bcd(t.number));
                mem::wr8(cb + 3, 1);
                mem::wr8(cb + 4, rel[0]);
                mem::wr8(cb + 5, rel[1]);
                mem::wr8(cb + 6, rel[2]);
                mem::wr8(cb + 7, 0);
                mem::wr8(cb + 8, am);
                mem::wr8(cb + 9, as_);
                mem::wr8(cb + 10, af);
            }
            15 => {
                mem::wr16(cb + 1, m.cd.paused.is_some() as u16);
                mem::wr32(cb + 3, redbook(m.cd.last_start));
                mem::wr32(cb + 7, redbook(m.cd.last_end));
            }
            c => {
                crate::warn!("MSCDEX IOCTL input {c} not implemented");
                return ERROR | ERR_UNKNOWN_COMMAND;
            }
        }
    }
    DONE
}

fn ioctl_output(m: &mut Machine, req: u32) -> u16 {
    let Some(cb) = m.lowmem.translate(far(req + 0x0e)) else { return ERROR | ERR_SECTOR_NOT_FOUND };
    let code = unsafe { mem::rd8(cb) };
    match code {
        3 => {
            let mut v = [0u8; 4];
            for ch in 0..4u32 {
                v[ch as usize] = unsafe { mem::rd8(cb + 2 + ch * 2) };
            }
            m.cd.set_volume(v);
            crate::debug!("CD audio volume {:?}", m.cd.volume);
        }
        0 | 1 | 2 | 5 => {}
        c => {
            crate::warn!("MSCDEX IOCTL output {c} not implemented");
            return ERROR | ERR_UNKNOWN_COMMAND;
        }
    }
    DONE
}

/// `int 2Fh AX=1501h`: list of CD drives (subunit + device header far pointer).
pub fn drive_device_list(m: &mut Machine, buf_linear: u32) {
    if let Some(h) = m.lowmem.translate(buf_linear) {
        unsafe {
            mem::wr8(h, 0);
            mem::wr32(h + 1, 0);
        }
    }
}
