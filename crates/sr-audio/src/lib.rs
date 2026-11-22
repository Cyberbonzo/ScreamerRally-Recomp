//! Audio output and mixing.
//!
//! One WASAPI shared-mode stream at 44.1 kHz stereo float (the CD's native rate; Windows'
//! own sample-rate converter adapts it to the device). The mixer has one music stream (the
//! game's CD audio) and any number of sound-effect voices, with separate music and SFX
//! volumes on top of the game's own levels.
//!
//! If no audio device is available the mixer still runs at real time (output discarded),
//! so streams advance exactly as they would with sound.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const RATE: u32 = 44_100;
/// The rate the game asks HMI's digital driver to mix at (one quarter of [`RATE`]).
pub const ORIGINAL_RATE: u32 = 11_025;
const HOLD: u32 = RATE / ORIGINAL_RATE;

/// A music source: fills interleaved stereo frames at [`RATE`]; returns frames written
/// (fewer than requested = finished).
pub trait Stream: Send {
    fn read(&mut self, out: &mut [f32]) -> usize;
}

/// A sound effect: mono or stereo PCM at any rate, played with volume and pan.
pub struct Voice {
    pub id: u32,
    /// Interleaved samples, -1..1.
    pub samples: Arc<Vec<f32>>,
    pub channels: u32,
    pub rate: u32,
    pub pos: f64,
    pub left: f32,
    pub right: f32,
    pub looping: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Volumes {
    pub master: f32,
    pub music: f32,
    pub sfx: f32,
}

impl Default for Volumes {
    fn default() -> Self {
        Volumes { master: 1.0, music: 1.0, sfx: 1.0 }
    }
}

struct MixerState {
    volumes: Volumes,
    music: Option<Box<dyn Stream>>,
    voices: Vec<Voice>,
    next_id: u32,
    scratch: Vec<f32>,
    /// Silence without advancing music or voices (the game is paused).
    paused: bool,
    /// Sound effects mixed as the 1997 driver did: at 11025 Hz, nearest sample, hard
    /// clipping, each mixed sample held for four output samples (an unfiltered DAC).
    original: bool,
    /// Original mixing: the sample being held and how many output frames it has left.
    held: (f32, f32),
    hold_left: u32,
}

impl MixerState {
    fn mix(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        if self.paused {
            return;
        }
        let v = self.volumes;
        if let Some(m) = &mut self.music {
            if self.scratch.len() < out.len() {
                self.scratch.resize(out.len(), 0.0);
            }
            let buf = &mut self.scratch[..out.len()];
            let frames = m.read(buf);
            let g = v.master * v.music;
            for (o, s) in out.iter_mut().zip(buf[..frames * 2].iter()) {
                *o += s * g;
            }
            if frames * 2 < out.len() {
                self.music = None;
            }
        }
        let g = v.master * v.sfx;
        let frames = out.len() / 2;
        if self.original {
            self.mix_original(out, g);
            for s in out.iter_mut() {
                *s = soft_clip(*s);
            }
            return;
        }
        self.voices.retain_mut(|voice| {
            let step = voice.rate as f64 / RATE as f64;
            let n = voice.samples.len() / voice.channels as usize;
            for f in 0..frames {
                let mut i = voice.pos as usize;
                if i >= n {
                    if voice.looping && n > 0 {
                        voice.pos -= n as f64;
                        i = voice.pos as usize;
                    } else {
                        return false;
                    }
                }
                let (l, r) = if voice.channels == 2 {
                    (voice.samples[i * 2], voice.samples[i * 2 + 1])
                } else {
                    (voice.samples[i], voice.samples[i])
                };
                out[f * 2] += l * voice.left * g;
                out[f * 2 + 1] += r * voice.right * g;
                voice.pos += step;
            }
            true
        });
        // Several full-volume engine loops can sum past full scale: saturate smoothly above
        // 0.75 instead of clipping hard (below that the signal is untouched).
        for s in out.iter_mut() {
            *s = soft_clip(*s);
        }
    }
}

impl MixerState {
    /// Sound effects at 11025 Hz into `out` (44.1 kHz stereo, added).
    fn mix_original(&mut self, out: &mut [f32], gain: f32) {
        for f in 0..out.len() / 2 {
            if self.hold_left == 0 {
                let (mut l, mut r) = (0.0f32, 0.0f32);
                for voice in self.voices.iter_mut() {
                    let n = voice.samples.len() / voice.channels.max(1) as usize;
                    let mut i = voice.pos as usize;
                    if i >= n {
                        if voice.looping && n > 0 {
                            voice.pos %= n as f64;
                            i = voice.pos as usize;
                        } else {
                            continue;
                        }
                    }
                    let (sl, sr) = if voice.channels == 2 { (voice.samples[i * 2], voice.samples[i * 2 + 1]) } else { (voice.samples[i], voice.samples[i]) };
                    l += sl * voice.left;
                    r += sr * voice.right;
                    voice.pos += voice.rate as f64 / ORIGINAL_RATE as f64;
                }
                // the driver's 16-bit mix saturates
                self.held = ((l * gain).clamp(-1.0, 1.0), (r * gain).clamp(-1.0, 1.0));
                self.hold_left = HOLD;
                self.voices.retain(|v| v.looping || (v.pos as usize) < v.samples.len() / v.channels.max(1) as usize);
            }
            out[f * 2] += self.held.0;
            out[f * 2 + 1] += self.held.1;
            self.hold_left -= 1;
        }
    }
}

/// Identity up to 0.75, then a tanh knee that approaches 1.0.
pub fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.75;
    let a = x.abs();
    if a <= KNEE { x } else { (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh()).copysign(x) }
}

#[derive(Clone)]
pub struct Audio {
    state: Arc<Mutex<MixerState>>,
    pub device_ok: Arc<AtomicBool>,
    pub underruns: Arc<AtomicU32>,
}

impl Audio {
    /// Start the output thread.
    pub fn start() -> Audio {
        let audio = Audio {
            state: Arc::new(Mutex::new(MixerState {
                volumes: Volumes::default(),
                music: None,
                voices: Vec::new(),
                next_id: 1,
                scratch: Vec::new(),
                paused: false,
                original: true,
                held: (0.0, 0.0),
                hold_left: 0,
            })),
            device_ok: Arc::default(),
            underruns: Arc::default(),
        };
        let a = audio.clone();
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                if let Err(e) = wasapi::run(&a) {
                    eprintln!("audio: no output device ({e}); continuing silently");
                    a.device_ok.store(false, Ordering::Relaxed);
                    silent_clock(&a);
                }
            })
            .expect("audio thread");
        audio
    }

    fn mix(&self, out: &mut [f32]) {
        self.state.lock().unwrap().mix(out);
        capture(out);
    }

    pub fn set_music(&self, s: Option<Box<dyn Stream>>) {
        self.state.lock().unwrap().music = s;
    }

    pub fn music_playing(&self) -> bool {
        self.state.lock().unwrap().music.is_some()
    }

    pub fn volumes(&self) -> Volumes {
        self.state.lock().unwrap().volumes
    }

    pub fn set_volumes(&self, v: Volumes) {
        self.state.lock().unwrap().volumes = v;
    }

    /// Sound effects mixed as the original driver did (11025 Hz) or at the full rate.
    pub fn set_original_mixing(&self, on: bool) {
        self.state.lock().unwrap().original = on;
    }

    /// Pause: output silence and hold music and sound effects where they are.
    pub fn set_paused(&self, paused: bool) {
        self.state.lock().unwrap().paused = paused;
    }

    pub fn play(&self, mut voice: Voice) -> u32 {
        let mut s = self.state.lock().unwrap();
        voice.id = s.next_id;
        s.next_id = s.next_id.wrapping_add(1).max(1);
        let id = voice.id;
        s.voices.push(voice);
        id
    }

    pub fn stop(&self, id: u32) {
        self.state.lock().unwrap().voices.retain(|v| v.id != id);
    }

    pub fn is_playing(&self, id: u32) -> bool {
        self.state.lock().unwrap().voices.iter().any(|v| v.id == id)
    }

    pub fn update_voice(&self, id: u32, f: impl FnOnce(&mut Voice)) {
        if let Some(v) = self.state.lock().unwrap().voices.iter_mut().find(|v| v.id == id) {
            f(v);
        }
    }
}

/// `SR_AUDIO_CAPTURE=path.wav`: write the first 60 s of mixed output (diagnostics).
fn capture(out: &[f32]) {
    use std::io::Write;
    static CAP: Mutex<Option<(std::fs::File, u32)>> = Mutex::new(None);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Some(p) = std::env::var_os("SR_AUDIO_CAPTURE") {
            if let Ok(mut f) = std::fs::File::create(p) {
                let _ = f.write_all(&[0u8; 44]);
                *CAP.lock().unwrap() = Some((f, 0));
            }
        }
    });
    let mut g = CAP.lock().unwrap();
    if let Some((f, frames)) = g.as_mut() {
        if *frames < RATE * 60 {
            let pcm: Vec<u8> = out.iter().flat_map(|s| ((s * 32767.0) as i16).to_le_bytes()).collect();
            let _ = f.write_all(&pcm);
            *frames += (out.len() / 2) as u32;
            // keep a valid header at all times
            use std::io::Seek;
            let data = *frames * 4;
            let mut h = Vec::with_capacity(44);
            h.extend_from_slice(b"RIFF");
            h.extend_from_slice(&(36 + data).to_le_bytes());
            h.extend_from_slice(b"WAVEfmt ");
            h.extend_from_slice(&16u32.to_le_bytes());
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&2u16.to_le_bytes());
            h.extend_from_slice(&RATE.to_le_bytes());
            h.extend_from_slice(&(RATE * 4).to_le_bytes());
            h.extend_from_slice(&4u16.to_le_bytes());
            h.extend_from_slice(&16u16.to_le_bytes());
            h.extend_from_slice(b"data");
            h.extend_from_slice(&data.to_le_bytes());
            let _ = f.seek(std::io::SeekFrom::Start(0)).and_then(|_| f.write_all(&h)).and_then(|_| f.seek(std::io::SeekFrom::End(0)).map(|_| ()));
        }
    }
}

/// Without a device: mix at real time into a scratch buffer.
fn silent_clock(a: &Audio) {
    let mut buf = vec![0f32; (RATE / 100 * 2) as usize];
    let start = Instant::now();
    let mut frames: u64 = 0;
    loop {
        let due = (start.elapsed().as_secs_f64() * RATE as f64) as u64;
        while frames + (RATE / 100) as u64 <= due {
            a.mix(&mut buf);
            frames += (RATE / 100) as u64;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

mod wasapi {
    use super::*;
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    pub fn run(a: &Audio) -> Result<(), String> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().map_err(|e| e.to_string())?;
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| e.to_string())?;
            let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
            let fmt = WAVEFORMATEX {
                wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
                nChannels: 2,
                nSamplesPerSec: RATE,
                nAvgBytesPerSec: RATE * 8,
                nBlockAlign: 8,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            // 40 ms buffer: low latency, safe against scheduling hiccups
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 400_000, 0, &fmt, None).map_err(|e| e.to_string())?;
            let event = CreateEventW(None, false, false, None).map_err(|e| e.to_string())?;
            client.SetEventHandle(event).map_err(|e| e.to_string())?;
            let size = client.GetBufferSize().map_err(|e| e.to_string())?;
            let render: IAudioRenderClient = client.GetService().map_err(|e| e.to_string())?;
            client.Start().map_err(|e| e.to_string())?;
            a.device_ok.store(true, Ordering::Relaxed);
            let mut buf = vec![0f32; size as usize * 2];
            loop {
                if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                    a.underruns.fetch_add(1, Ordering::Relaxed);
                }
                let padding = client.GetCurrentPadding().unwrap_or(size);
                let frames = size.saturating_sub(padding);
                if frames == 0 {
                    continue;
                }
                let out = &mut buf[..frames as usize * 2];
                a.mix(out);
                if let Ok(p) = render.GetBuffer(frames) {
                    std::ptr::copy_nonoverlapping(out.as_ptr(), p as *mut f32, out.len());
                    let _ = render.ReleaseBuffer(frames, 0);
                }
            }
            #[allow(unreachable_code)]
            {
                let _ = CloseHandle(event);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_clip_shape() {
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(soft_clip(-0.75), -0.75);
        assert!(soft_clip(1.0) > 0.9 && soft_clip(1.0) < 1.0);
        assert!(soft_clip(4.0) <= 1.0 && soft_clip(-4.0) >= -1.0);
        let mut prev = -2.0;
        for i in -400..=400 {
            let y = soft_clip(i as f32 / 100.0);
            assert!(y >= prev);
            prev = y;
        }
    }

    fn state(original: bool) -> MixerState {
        MixerState { volumes: Volumes::default(), music: None, voices: Vec::new(), next_id: 1, scratch: Vec::new(), paused: false, original, held: (0.0, 0.0), hold_left: 0 }
    }

    fn voice(samples: Vec<f32>, rate: u32) -> Voice {
        Voice { id: 1, samples: Arc::new(samples), channels: 1, rate, pos: 0.0, left: 1.0, right: 1.0, looping: false }
    }

    #[test]
    fn original_mixing_holds_each_11k_sample_for_four_frames() {
        let mut m = state(true);
        m.voices.push(voice(vec![0.1, 0.2, 0.3], ORIGINAL_RATE));
        let mut out = vec![0.0f32; 2 * 16];
        m.mix(&mut out);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        assert_eq!(&left[..12], &[0.1, 0.1, 0.1, 0.1, 0.2, 0.2, 0.2, 0.2, 0.3, 0.3, 0.3, 0.3]);
        assert_eq!(&left[12..], &[0.0; 4]);
        assert!(m.voices.is_empty());
    }

    #[test]
    fn original_mixing_steps_faster_voices_by_nearest_sample() {
        let mut m = state(true);
        m.voices.push(voice(vec![0.1, 0.2, 0.3, 0.4], 2 * ORIGINAL_RATE));
        let mut out = vec![0.0f32; 2 * 8];
        m.mix(&mut out);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        assert_eq!(left, vec![0.1, 0.1, 0.1, 0.1, 0.3, 0.3, 0.3, 0.3]);
    }

    #[test]
    fn original_mixing_saturates_like_a_16_bit_mix() {
        let mut m = state(true);
        m.voices.push(voice(vec![0.7; 4], ORIGINAL_RATE));
        m.voices.push(voice(vec![0.7; 4], ORIGINAL_RATE));
        let mut out = vec![0.0f32; 2 * 4];
        m.mix(&mut out);
        // clamped to 1.0 by the driver mix, then the output knee
        assert!((out[0] - soft_clip(1.0)).abs() < 1e-6);
    }
}
