//! The CRT stage of the renderer: follows the shared settings (`CrtLive`), loads the
//! chosen preset from the pack, and runs it on the finished frame when it is presented.
//!
//! The renderer asks `input_size` for the size to draw the game at (the shader's input:
//! the game's 480 lines by default, the whole frame with its HUD and 2D pictures), and
//! calls `draw` where it would otherwise copy its picture to the back buffer, before the
//! settings overlay is drawn over it.
//!
//! The behaviour follows ShaderGlass's (Copyright (C) 2021-2025 mausimus, GPL-3.0): the
//! "none" passthrough while the hold key is down (`ID_QUICK_TOGGLE`), the output box
//! (`UpdateOutputSize`, `Process`), snapshots of the output (`GrabOutput`).

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::chain::{Chain, ChainDesc};
use crate::config::{CrtSettings, SharedCrt, input_size, output_rect};

/// A preset change applies once the choice has rested this long (stepping through the
/// library in the overlay does not load every preset on the way).
const SETTLE: Duration = Duration::from_millis(180);

pub struct Stage {
    live: Option<SharedCrt>,
    settings: CrtSettings,
    seen: u64,
    chain: Option<Chain>,
    /// The passthrough, while the hold key is down.
    none: Option<Chain>,
    /// What `chain` was made from: preset key and orientation (or what failed to load).
    loaded: Option<(String, bool)>,
    changed: Instant,
    started: bool,
    log: Vec<String>,
    /// The output box of the last frame drawn.
    pub last_rect: (i32, i32, u32, u32),
}

impl Stage {
    pub fn new(live: Option<SharedCrt>) -> Stage {
        Stage {
            settings: live.as_ref().map(|l| l.get()).unwrap_or_default(),
            seen: 0,
            live,
            chain: None,
            none: None,
            loaded: None,
            changed: Instant::now(),
            started: false,
            log: Vec::new(),
            last_rect: (0, 0, 0, 0),
        }
    }

    /// Messages for the log since the last call.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    fn sync(&mut self) {
        let Some(live) = &self.live else { return };
        let g = live.generation.load(std::sync::atomic::Ordering::Relaxed);
        if g != self.seen {
            self.seen = g;
            let s = live.get();
            if s.preset != self.settings.preset || s.vertical != self.settings.vertical {
                self.changed = Instant::now();
            }
            self.settings = s;
            let params = self.settings.params.clone();
            if let Some(c) = &mut self.chain {
                c.reset_params();
                for (n, v) in &params {
                    c.set_param(n, *v);
                }
            }
        }
    }

    /// CRT emulation is on (and the library is there): the size to render the game at for
    /// a picture of `picture` pixels on screen; None when off.
    pub fn input_size(&mut self, picture: (u32, u32)) -> Option<(u32, u32)> {
        self.sync();
        let live = self.live.as_ref()?;
        if !self.settings.active || live.pack.is_none() {
            if self.chain.is_some() || self.none.is_some() {
                // off: give the GPU memory back
                self.chain = None;
                self.none = None;
                self.loaded = None;
                self.log.push("crt: off".into());
            }
            return None;
        }
        Some(input_size(self.settings.pixel_size(), picture))
    }

    fn load(&mut self, device: &ID3D11Device, ctx: &ID3D11DeviceContext) {
        let want = (self.settings.preset.clone(), self.settings.vertical);
        if self.loaded.as_ref() == Some(&want) {
            return;
        }
        if self.started && self.changed.elapsed() < SETTLE {
            return;
        }
        self.started = true;
        self.loaded = Some(want.clone());
        self.chain = None;
        self.none = None;
        let Some(pack) = self.live.as_ref().and_then(|l| l.pack.clone()) else { return };
        let t = Instant::now();
        let result = (|| -> Result<Chain, String> {
            let i = pack.index.find(&want.0).or(pack.index.default_preset.map(|d| d as usize)).ok_or_else(|| format!("no preset {}", want.0))?;
            let data = pack.load(i, true).map_err(|e| e.to_string())?;
            Chain::new(device, ctx, &ChainDesc { preset: &data, vertical: want.1 })
        })();
        match result {
            Ok(mut c) => {
                for (n, v) in &self.settings.params {
                    c.set_param(n, *v);
                }
                self.log.push(format!(
                    "crt: {} loaded in {:.0} ms: {} passes, {} parameters, {:.1} MB of lookup textures",
                    want.0,
                    t.elapsed().as_secs_f64() * 1000.0,
                    c.pass_count(),
                    c.params.len(),
                    c.texture_bytes as f64 / 1048576.0
                ));
                self.chain = Some(c);
            }
            Err(e) => {
                self.log.push(format!("crt: {} failed: {e}", want.0));
                if let Some(l) = &self.live {
                    l.say(format!("Shader failed: {e}"));
                }
            }
        }
    }

    /// Draw the frame `input` (`input_size`, the game's picture at the CRT input size)
    /// through the shader into `back` (`back_size`), the game's picture being `picture` (x,
    /// y, w, h) in it. Returns false if nothing was drawn (CRT off).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        input: &ID3D11ShaderResourceView,
        input_size: (u32, u32),
        back: &ID3D11RenderTargetView,
        back_tex: &ID3D11Texture2D,
        back_size: (u32, u32),
        picture: (u32, u32, u32, u32),
        gamma: f32,
        time: f64,
    ) -> bool {
        let Some(live) = self.live.clone() else { return false };
        if !self.settings.active || live.pack.is_none() {
            return false;
        }
        self.load(device, ctx);
        let hold = live.hold.load(std::sync::atomic::Ordering::Relaxed);
        if (hold || self.chain.is_none()) && self.none.is_none() {
            match crate::chain::passthrough_preset().and_then(|p| Chain::new(device, ctx, &ChainDesc { preset: &p, vertical: false })) {
                Ok(c) => self.none = Some(c),
                Err(e) => {
                    self.log.push(format!("crt: passthrough: {e}"));
                    return false;
                }
            }
        }
        let s = &self.settings;
        let rect = output_rect(back_size, picture, s.aspect_ratio(), s.output_scale(), s.vertical);
        self.last_rect = rect;
        let (flip_h, flip_v) = (s.flip_h, s.flip_v);
        let chain = if hold || self.chain.is_none() { self.none.as_mut() } else { self.chain.as_mut() };
        let Some(chain) = chain else { return false };
        chain.set_flip(flip_h, flip_v);
        if let Err(e) = chain.process(ctx, input, input_size, back, back_tex, back_size, rect, time, 1.0 / gamma.max(0.1)) {
            self.log.push(format!("crt: {e}"));
            live.say(format!("Shader failed: {e}"));
            self.chain = None;
            return false;
        }
        if let Some(dir) = live.snapshot.lock().unwrap().take() {
            match snapshot(device, ctx, back_tex, back_size, rect, &dir, &self.settings.preset) {
                Ok(p) => {
                    self.log.push(format!("crt: snapshot {}", p.display()));
                    live.say(format!("Saved {}", p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()));
                }
                Err(e) => {
                    self.log.push(format!("crt: snapshot failed: {e}"));
                    live.say(format!("Snapshot failed: {e}"));
                }
            }
        }
        true
    }

    /// GPU memory of the chain's textures (estimate, bytes).
    pub fn gpu_bytes(&self) -> u64 {
        self.chain.as_ref().map_or(0, |c| c.gpu_bytes) + self.none.as_ref().map_or(0, |c| c.gpu_bytes)
    }
}

/// ShaderGlass "Take Snapshot": the shader's output (its box in the back buffer, clipped
/// to it) as PNG in `dir`.
fn snapshot(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    back: &ID3D11Texture2D,
    back_size: (u32, u32),
    rect: (i32, i32, u32, u32),
    dir: &std::path::Path,
    preset: &str,
) -> Result<std::path::PathBuf, String> {
    let (x0, y0) = (rect.0.max(0) as u32, rect.1.max(0) as u32);
    let x1 = ((rect.0 + rect.2 as i32).max(0) as u32).min(back_size.0);
    let y1 = ((rect.1 + rect.3 as i32).max(0) as u32).min(back_size.1);
    if x1 <= x0 || y1 <= y0 {
        return Err("nothing on screen".into());
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { back.GetDesc(&mut desc) };
    let sd = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: desc.Format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    unsafe {
        let mut st = None;
        device.CreateTexture2D(&sd, None, Some(&mut st)).map_err(|e| e.to_string())?;
        let st = st.unwrap();
        let b = D3D11_BOX { left: x0, top: y0, front: 0, right: x1, bottom: y1, back: 1 };
        ctx.CopySubresourceRegion(&st, 0, 0, 0, 0, back, 0, Some(&b));
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(|e| e.to_string())?;
        for y in 0..h {
            let row = std::slice::from_raw_parts((m.pData as *const u8).add((y * m.RowPitch) as usize), (w * 4) as usize);
            px.extend_from_slice(row);
        }
        ctx.Unmap(&st, 0);
    }
    if desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM {
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
    }
    for p in px.chunks_exact_mut(4) {
        p[3] = 255;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // named after the preset and the local time
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let (y, mo, d, hh, mi, ss) = (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond);
    let name: String = preset.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("crt").chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let mut path = dir.join(format!("{name}-{y:04}{mo:02}{d:02}-{hh:02}{mi:02}{ss:02}.png"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{name}-{y:04}{mo:02}{d:02}-{hh:02}{mi:02}{ss:02}-{n}.png"));
        n += 1;
    }
    crate::image::save_png(&path, w, h, &px)?;
    Ok(path)
}
