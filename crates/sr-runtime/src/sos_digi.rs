//! HMI SOS 4 digital audio API, replaced at API level (like Glide).
//!
//! The original library loads a sound-card driver from `HMIDRV.386`, and that driver mixes
//! the active voices into a DMA buffer from a 120 Hz timer service. Here the same API is
//! implemented on the native mixer: the game's own sample data, volumes, pans and rates
//! are played as they are, mixed at 44.1 kHz.
//!
//! Voices are the 0F0h-byte sample records the game builds (`ba414`) and passes to
//! `sosDIGIStartSample`; the handle is a voice slot index, as in the original (`d88cd`).
//! Calling convention: Watcom register calls (EAX, EDX, EBX, ECX), result in EAX.
//! Names follow the HMI SOS 4 API where the function's role is verified from its code.

use std::sync::Arc;

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

const SOS_NO_ERROR: u32 = 0;
const SOS_INVALID_HANDLE: u32 = 10;
const VOICES: usize = 32;

/// Sample record fields (offsets into the 0F0h-byte record). Set by the game in `ba414`
/// from its sound list and by `d88cd`; meanings checked against the records and data
/// the game actually plays (rates 9200-11025 Hz, 8-bit unsigned mono data).
pub mod rec {
    pub const DATA: u32 = 0x00;
    pub const CURRENT: u32 = 0x04; // = DATA when started
    pub const SIZE: u32 = 0x0c; // bytes
    pub const ID: u32 = 0x1c; // the game's sample ID
    pub const HANDLE: u32 = 0x28;
    pub const VOLUME: u32 = 0x2c; // left << 16 | right, 0..7FFFh
    pub const LOOPS: u32 = 0x30; // -1 = loop until stopped (engines), 0 = once
    pub const RATE: u32 = 0x34; // Hz
    pub const BITS: u32 = 0x38; // 8 or 16
    pub const CHANNELS: u32 = 0x3c; // 1 or 2
    pub const FORMAT: u32 = 0x40; // 8000h = unsigned samples (game sets it unless told otherwise)
    pub const PAN: u32 = 0x44; // 0..FFFFh, 8000h = center
}

/// Convert sample data to interleaved floats.
fn decode(bytes: &[u8], bits: u32, unsigned: bool) -> Vec<f32> {
    if bits == 16 {
        bytes
            .chunks_exact(2)
            .map(|c| {
                let v = u16::from_le_bytes([c[0], c[1]]);
                let v = if unsigned { v as i32 - 0x8000 } else { v as i16 as i32 };
                v as f32 / 32768.0
            })
            .collect()
    } else {
        bytes.iter().map(|&b| if unsigned { (b as f32 - 128.0) / 128.0 } else { b as i8 as f32 / 128.0 }).collect()
    }
}

struct Voice {
    record: [u8; 0xf0],
    audio_id: u32,
    /// Deterministic mode: frames played so far by virtual time, when last counted, and
    /// the sample's length in frames.
    vpos: std::cell::Cell<f64>,
    vlast: std::cell::Cell<std::time::Duration>,
    frames: u64,
}

impl Voice {
    fn field(&self, off: u32) -> u32 {
        u32::from_le_bytes(self.record[off as usize..off as usize + 4].try_into().unwrap())
    }
    fn set_field(&mut self, off: u32, v: u32) {
        if off == rec::RATE {
            self.advance();
        }
        self.record[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
    }
    /// Deterministic mode: count the frames played since the last call at the current rate.
    fn advance(&self) {
        let now = crate::time::uptime();
        let dt = now.saturating_sub(self.vlast.get()).as_secs_f64();
        self.vpos.set(self.vpos.get() + dt * self.field(rec::RATE).max(1) as f64);
        self.vlast.set(now);
    }
    fn playing_virtual(&self) -> bool {
        self.advance();
        self.field(rec::LOOPS) == u32::MAX || (self.vpos.get() as u64) < self.frames
    }
}

pub struct Digi {
    audio: Option<sr_audio::Audio>,
    voices: Vec<Option<Voice>>,
    master: u32,
    /// Sample data converted to float, by (data address, size).
    cache: std::collections::HashMap<(u32, u32, u32, bool, u64), Arc<Vec<f32>>>,
    pub starts: u64,
    logged: u32,
}

impl Digi {
    pub fn new(audio: Option<sr_audio::Audio>) -> Digi {
        Digi { audio, voices: (0..VOICES).map(|_| None).collect(), master: 0x7fff, cache: Default::default(), starts: 0, logged: 0 }
    }

    fn playing(&self, h: usize) -> bool {
        if crate::time::deterministic() {
            return self.voices[h].as_ref().is_some_and(|v| v.playing_virtual());
        }
        match (&self.voices[h], &self.audio) {
            (Some(v), Some(a)) => a.is_playing(v.audio_id),
            _ => false,
        }
    }

    fn gains(&self, v: &Voice) -> (f32, f32) {
        let vol = v.field(rec::VOLUME);
        let m = (self.master & 0x7fff) as f32 / 32767.0;
        let l = ((vol >> 16) & 0x7fff) as f32 / 32767.0 * m;
        let r = (vol & 0x7fff) as f32 / 32767.0 * m;
        let pan = (v.field(rec::PAN) & 0xffff) as f32;
        let pl = ((65535.0 - pan) / 32768.0).min(1.0);
        let pr = (pan / 32768.0).min(1.0);
        (l * pl, r * pr)
    }

    fn update(&self, h: usize) {
        if let (Some(v), Some(a)) = (&self.voices[h], &self.audio) {
            let (l, r) = self.gains(v);
            let rate = v.field(rec::RATE).max(1);
            a.update_voice(v.audio_id, |x| {
                x.left = l;
                x.right = r;
                x.rate = rate;
            });
        }
    }
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x100_0000_01b3))
}

fn handle(f: &HookFrame) -> Option<usize> {
    let h = f.edx as usize;
    (h < VOICES).then_some(h)
}

/// `d6c89 sosDIGIInitSystem`, `d6ce5 sosDIGIUnInitSystem`, `d74b3 sosDIGIUnInitDriver`,
/// and the card-detection calls: nothing to do on the native mixer.
pub fn ok(_m: &mut Machine, f: &mut HookFrame, name: u32) -> HookResult {
    crate::debug!("SOS digital: {} -> ok", NAMES.get(name as usize).copied().unwrap_or("?"));
    f.eax = SOS_NO_ERROR;
    HookResult::Return(0)
}

pub const NAMES: &[&str] = &[
    "sosDIGIInitSystem",
    "sosDIGIUnInitSystem",
    "sosDIGIUnInitDriver",
    "sosDetectInitSystem",
    "sosDetectGetSettings",
    "sosDetectUnInitSystem",
    "detection da2bf",
    "detection da4fc",
    "detection da70d",
    "detection da145",
    "detection da88d",
];

/// `d7005 sosDIGIInitDriver(&driver_info, &handle)`: driver 0. The driver's timer service
/// pointer (`info + 108h`, registered by the game at 120 Hz) gets a plain `ret`.
pub fn init_driver(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    let (info, out) = (f.eax, f.edx);
    let ret = m.arena.ret_gadget(0);
    unsafe {
        mem::wr32(out, 0);
        mem::wr32(info + 0x108, ret);
    }
    crate::info!(
        "sosDIGIInitDriver: device {:#x}, rate {} Hz, buffer {:#x} -> native mixer{}",
        unsafe { mem::rd32(info) },
        unsafe { mem::rd32(info + 4) },
        unsafe { mem::rd32(info + 0x44) },
        if m.digi.audio.is_some() { "" } else { " (no audio output)" }
    );
    f.eax = SOS_NO_ERROR;
    HookResult::Return(0)
}

/// `d8267 sosDIGISetMasterVolume(driver, volume)` -> previous volume.
pub fn set_master_volume(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    if crate::interp::in_extra_draw() {
        f.eax = SOS_NO_ERROR;
        return HookResult::Return(0);
    }
    let old = m.digi.master;
    m.digi.master = f.edx & 0x7fff;
    for h in 0..VOICES {
        m.digi.update(h);
    }
    f.eax = old;
    HookResult::Return(0)
}

/// `d88cd sosDIGIStartSample(driver, &record)` -> handle, or -1 if no voice is free.
pub fn start_sample(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    if crate::interp::in_extra_draw() {
        f.eax = u32::MAX;
        return HookResult::Return(0);
    }
    let src = f.edx;
    let d = &mut m.digi;
    let Some(h) = (0..VOICES).find(|&h| !d.playing(h)) else {
        f.eax = u32::MAX;
        return HookResult::Return(0);
    };
    let mut record = [0u8; 0xf0];
    record.copy_from_slice(unsafe { mem::slice(src, 0xf0) });
    let mut v = Voice { record, audio_id: 0, vpos: Default::default(), vlast: std::cell::Cell::new(crate::time::uptime()), frames: 0 };
    v.set_field(rec::CURRENT, v.field(rec::DATA));
    v.set_field(rec::HANDLE, h as u32);
    let (data, size, rate) = (v.field(rec::DATA), v.field(rec::SIZE), v.field(rec::RATE));
    let bits = if v.field(rec::BITS) == 16 { 16 } else { 8 };
    let channels = if v.field(rec::CHANNELS) == 2 { 2 } else { 1 };
    let unsigned = v.field(rec::FORMAT) & 0x8000 != 0;
    v.frames = size as u64 / (bits as u64 / 8 * channels as u64).max(1);
    let loops = v.field(rec::LOOPS);
    d.starts += 1;
    if d.logged < 48 {
        d.logged += 1;
        let words: Vec<String> = (0..0x50).step_by(4).map(|o| format!("{:x}", v.field(o))).collect();
        let head = if data != 0 && size > 0 { unsafe { mem::slice(data, size.min(16)) }.to_vec() } else { Vec::new() };
        crate::info!("sosDIGIStartSample -> {h}: record [{}] +a8={:x} data {:02x?}", words.join(" "), v.field(0xa8), head);
    }
    if let Some(a) = &d.audio {
        if data != 0 && size > 0 && size < 64 << 20 {
            // the game may load another sample at the same address: key on the bytes too
            let bytes = unsafe { mem::slice(data, size) };
            let key = (data, size, bits, unsigned, fnv(bytes));
            let pcm = d.cache.entry(key).or_insert_with(|| Arc::new(decode(bytes, bits, unsigned))).clone();
            if loops != 0 && loops != u32::MAX {
                crate::debug!("sample {:#x}: loop count {loops} played once", v.field(rec::ID));
            }
            let (l, r) = d.gains(&v);
            v.audio_id = a.play(sr_audio::Voice {
                id: 0,
                samples: pcm,
                channels,
                rate: rate.max(1),
                pos: 0.0,
                left: l,
                right: r,
                looping: loops == u32::MAX,
            });
        }
    }
    if let Some(old) = d.voices[h].take() {
        if let Some(a) = &d.audio {
            a.stop(old.audio_id);
        }
    }
    d.voices[h] = Some(v);
    f.eax = h as u32;
    HookResult::Return(0)
}

/// `d8a4b sosDIGIStopSample(driver, handle)`.
pub fn stop_sample(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    if crate::interp::in_extra_draw() {
        f.eax = SOS_NO_ERROR;
        return HookResult::Return(0);
    }
    let d = &mut m.digi;
    f.eax = match handle(f) {
        Some(h) => {
            if let (Some(v), Some(a)) = (&d.voices[h], &d.audio) {
                a.stop(v.audio_id);
            }
            SOS_NO_ERROR
        }
        None => SOS_INVALID_HANDLE,
    };
    HookResult::Return(0)
}

/// `d8826 sosDIGISampleDone(driver, handle)` -> 1 when the voice has finished.
pub fn sample_done(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    f.eax = match handle(f) {
        Some(h) => (!m.digi.playing(h)) as u32,
        None => SOS_INVALID_HANDLE,
    };
    HookResult::Return(0)
}

/// `d86f8 sosDIGIGetSampleID(driver, handle)` -> the record's ID (kept after it ends).
pub fn get_sample_id(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    f.eax = match handle(f).and_then(|h| m.digi.voices[h].as_ref()) {
        Some(v) => v.field(rec::ID),
        None => SOS_INVALID_HANDLE,
    };
    HookResult::Return(0)
}

/// Set (`arg` = field offset) or get (`arg | 0x8000_0000`) one voice field:
/// `d82a0 / d8328` volume, `d848a / d8510` pan, `d8586 / d860c` rate.
pub fn voice_field(m: &mut Machine, f: &mut HookFrame, arg: u32) -> HookResult {
    if crate::interp::in_extra_draw() {
        f.eax = SOS_NO_ERROR;
        return HookResult::Return(0);
    }
    let get = arg & 0x8000_0000 != 0;
    let off = arg & 0xff;
    let Some(h) = handle(f) else {
        f.eax = SOS_INVALID_HANDLE;
        return HookResult::Return(0);
    };
    let d = &mut m.digi;
    f.eax = match d.voices[h].as_mut() {
        Some(v) if get => v.field(off),
        Some(v) => {
            v.set_field(off, f.ebx);
            d.update(h);
            SOS_NO_ERROR
        }
        None => SOS_INVALID_HANDLE,
    };
    HookResult::Return(0)
}

/// Install the digital API replacement (link addresses of `STRT3FX.EXE`).
pub fn install(m: &mut Machine) -> Result<(), String> {
    const P5: &[u8] = &[0x53, 0x51, 0x56, 0x57, 0x55]; // push ebx, ecx, esi, edi, ebp
    const P5D: &[u8] = &[0x53, 0x51, 0x52, 0x56, 0x57]; // push ebx, ecx, edx, esi, edi
    const P6: &[u8] = &[0x51, 0x56, 0x57, 0x55, 0x89, 0xe5]; // push ecx, esi, edi, ebp; mov ebp, esp
    m.install_hook_arg("sosDIGIInitSystem", 0xd6c89, P5, ok, 0)?;
    m.install_hook_arg("sosDIGIUnInitSystem", 0xd6ce5, P5D, ok, 1)?;
    m.install_hook_arg("sosDIGIUnInitDriver", 0xd74b3, P6, ok, 2)?;
    m.install_hook_arg("sosDetectInitSystem", 0xd9cf9, P5D, ok, 3)?;
    m.install_hook_arg("sosDetectGetSettings", 0xd9ece, P5, ok, 4)?;
    m.install_hook_arg("sosDetectUnInitSystem", 0xd9e46, P5D, ok, 5)?;
    // the rest of the detection interface used by the game's setup switch (baf34);
    // the device is fixed by SETUP.CFG, nothing is probed
    m.install_hook_arg("detection da2bf", 0xda2bf, P5D, ok, 6)?;
    m.install_hook_arg("detection da4fc", 0xda4fc, P5D, ok, 7)?;
    m.install_hook_arg("detection da70d", 0xda70d, P5D, ok, 8)?;
    m.install_hook_arg("detection da145", 0xda145, P5, ok, 9)?;
    m.install_hook_arg("detection da88d", 0xda88d, P5D, ok, 10)?;
    m.install_hook("sosDIGIInitDriver", 0xd7005, P5, init_driver)?;
    m.install_hook("sosDIGISetMasterVolume", 0xd8267, P5, set_master_volume)?;
    m.install_hook("sosDIGIStartSample", 0xd88cd, P5, start_sample)?;
    m.install_hook("sosDIGIStopSample", 0xd8a4b, P5, stop_sample)?;
    m.install_hook("sosDIGISampleDone", 0xd8826, P5, sample_done)?;
    m.install_hook("sosDIGIGetSampleID", 0xd86f8, P5, get_sample_id)?;
    m.install_hook_arg("sosDIGISetSampleVolume", 0xd82a0, P6, voice_field, rec::VOLUME)?;
    m.install_hook_arg("sosDIGIGetSampleVolume", 0xd8328, P5, voice_field, rec::VOLUME | 0x8000_0000)?;
    m.install_hook_arg("sosDIGISetPanLocation", 0xd848a, P6, voice_field, rec::PAN)?;
    m.install_hook_arg("sosDIGIGetPanLocation", 0xd8510, P5, voice_field, rec::PAN | 0x8000_0000)?;
    m.install_hook_arg("sosDIGISetSampleRate", 0xd8586, P6, voice_field, rec::RATE)?;
    m.install_hook_arg("sosDIGIGetSampleRate", 0xd860c, P5, voice_field, rec::RATE | 0x8000_0000)?;
    Ok(())
}
