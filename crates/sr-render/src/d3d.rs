//! Direct3D 11 implementation of the Glide backend.
//!
//! The game renders into a "scene" target whose size is the 4:3 area of the window (so the
//! game's 640x480 screen space is rasterised directly at output resolution, not upscaled),
//! then the scene is presented letterboxed with the game's gamma.
//!
//! Widescreen: the scene target takes the window's aspect (or an aspect preset's, with
//! black bars where the window differs; `display`) and shows the game's screen space from
//! `-ext` to `640 + ext` horizontally (the game projects geometry beyond its 4:3 edges; the
//! runtime widens its culling to match). Full-width clip windows and clears are extended
//! to the whole target; 2D (LFB) images stay 4:3 in the centre.
//!
//! Render scale (other than 100 %): the scene target holds the 3D at that percentage of
//! the picture's size, and a second target (`out`) the picture at full resolution. 2D
//! (LFB images, 2D-marked triangles) and clears are drawn into both; 3D polygons mark a
//! coverage target as they draw, and before the next 2D drawing (and the present) the 3D
//! drawn so far is composed over `out` where it covered, filtered to its size
//! (`ps_resolve`). The HUD and the menus' pictures so stay crisp at any render scale.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sr_runtime::glide::consts::*;
use sr_runtime::glide::tex::{self, TexInfo};
use sr_runtime::glide::{Backend, DrawContext, GlideState, LfbPart, Picture, Sprite2D, Vertex};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, PCSTR, s};

#[path = "crt.rs"]
mod crt;

const SHADER: &str = include_str!("voodoo.hlsl");
const VB_VERTICES: u32 = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Always nearest-neighbour (sharp texels). Default.
    Nearest,
    /// Always bilinear.
    Bilinear,
    /// Whatever the game asks for (the 3dfx look).
    Game,
}

/// Settings the overlay changes while the game runs (read every frame).
#[derive(Debug)]
pub struct LiveRender {
    /// 0 nearest, 1 bilinear, 2 game.
    pub filter: std::sync::atomic::AtomicU8,
    pub vsync: std::sync::atomic::AtomicBool,
    /// `Aspect::index`.
    pub aspect: std::sync::atomic::AtomicU8,
    pub depth: std::sync::atomic::AtomicBool,
    pub show_fps: std::sync::atomic::AtomicBool,
    /// Percent (`RenderSettings::render_scale`).
    pub render_scale: std::sync::atomic::AtomicU32,
    /// `display::pack_mode` of `RenderSettings::exclusive_mode`.
    pub exclusive_mode: AtomicU64,
}

impl LiveRender {
    pub fn new(s: &RenderSettings) -> Arc<LiveRender> {
        Arc::new(LiveRender {
            filter: std::sync::atomic::AtomicU8::new(match s.filter {
                Filter::Nearest => 0,
                Filter::Bilinear => 1,
                Filter::Game => 2,
            }),
            vsync: std::sync::atomic::AtomicBool::new(s.vsync),
            aspect: std::sync::atomic::AtomicU8::new(s.aspect.index() as u8),
            depth: std::sync::atomic::AtomicBool::new(s.depth),
            show_fps: std::sync::atomic::AtomicBool::new(s.show_fps),
            render_scale: std::sync::atomic::AtomicU32::new(s.render_scale),
            exclusive_mode: AtomicU64::new(crate::display::pack_mode(s.exclusive_mode)),
        })
    }
}

#[derive(Clone, Debug)]
pub struct RenderSettings {
    pub filter: Filter,
    pub vsync: bool,
    /// Write every `n`-th presented frame as PNG into this folder (diagnostics).
    pub dump: Option<(std::path::PathBuf, u64)>,
    /// Shape of the picture: the window's (wider windows show more, Hor+) or a preset's
    /// with black bars where the window differs.
    pub aspect: crate::display::Aspect,
    /// The 3D scene drawn at this percentage of the picture's size (50-200; 100 = as is).
    /// 2D stays at the picture's resolution.
    pub render_scale: u32,
    /// Exclusive fullscreen in this display mode (width, height, refresh rate in Hz);
    /// `None` = the desktop's.
    pub exclusive_mode: Option<(u32, u32, u32)>,
    /// Hide surfaces with a depth buffer instead of relying on the game's polygon sorting
    /// (painter's algorithm, which misorders overlapping polygons).
    pub depth: bool,
    /// Frame rate counter in the top-left corner.
    pub show_fps: bool,
    /// Close hairline cracks between 3D polygons (gs_grow).
    pub grow_cracks: bool,
    /// Live changes from the settings overlay.
    pub live: Option<Arc<LiveRender>>,
    /// The settings overlay, drawn over the game.
    pub menu: Option<crate::overlay::SharedMenu>,
    /// CRT emulation (`crt`), changed by the overlay.
    pub crt: Option<sr_crt::SharedCrt>,
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings {
            filter: Filter::Nearest,
            vsync: true,
            dump: None,
            aspect: crate::display::Aspect::Classic4x3,
            render_scale: 100,
            exclusive_mode: None,
            depth: true,
            show_fps: false,
            grow_cracks: true,
            live: None,
            menu: None,
            crt: None,
        }
    }
}

/// What the last 3D draw left bound (raw COM pointers / values).
#[derive(Clone, Copy, PartialEq, Default)]
struct Bound {
    srv: usize,
    samp: usize,
    blend: usize,
    depth: usize,
    stencil_ref: u32,
    raster: usize,
    scissor: (i32, i32, i32, i32),
}

/// Kinds of depth-stencil state for depth-buffered 3D polygons (`forced_depth_state`).
#[derive(Clone, Copy)]
enum ForcedDepth {
    Opaque = 1,
    BackFace = 2,
    OwnPart = 3,
    OtherPart = 4,
    /// Decals over pixels of scenery / of cars (stencil bit 7).
    OverScenery = 5,
    OverCars = 6,
    /// A car's blended polygons over the same car (stencil bits 4-7: car, object), with
    /// the narrow depth window.
    SameCar = 7,
    /// ... over their own part's pixels that `SameCar` rejects (the reverse depth test with
    /// the same bias): over its own part a car's glass keeps the game's order (each part's
    /// polygons come in a precomputed order for the view direction).
    OwnPartRest = 8,
    /// ... over everything else (walls, other cars): depth tested, rounding only.
    OtherObject = 9,
}

/// Stencil bits of a car's object: the car bit and the object number.
const CAR_OBJECT_BITS: u8 = 0xf0;

/// Stencil bit of pixels drawn by cars (the Glide layer sets it in the part id).
const CAR_BIT: u8 = 0x80;
use sr_runtime::depth::Kind;

/// A fast hasher for the renderer's caches (small integer keys; no hash flooding concern).
#[derive(Default, Clone, Copy)]
struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7cc1_b727_220a_95);
        }
    }
    fn write_u32(&mut self, v: u32) {
        self.0 = (self.0.rotate_left(5) ^ v as u64).wrapping_mul(0x51_7cc1_b727_220a_95);
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7cc1_b727_220a_95);
    }
    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
}

type FastMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<FxHasher>>;

fn bytes_of<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: plain-old-data constant buffer contents
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct StateCb {
    cc: [u32; 4],
    ac: [u32; 4],
    tc: [u32; 4],
    flags: [u32; 4],
    constant: [f32; 4],
    fog_color: [f32; 4],
    chroma: [f32; 4],
    tex_range: [f32; 4],
    screen: [f32; 4],
    fog_table: [[f32; 4]; 16],
    /// x: grow opaque 3D triangles by this many target pixels (gs_grow), yz: target size.
    grow: [f32; 4],
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TexKey {
    start: u32,
    even_odd: u32,
    info: TexInfo,
    palette_gen: u64,
    mipmaps: bool,
}

struct CachedTex {
    srv: ID3D11ShaderResourceView,
    end: u32,
    range: (f32, f32),
}

struct Targets {
    width: u32,
    height: u32,
    rtv: ID3D11RenderTargetView,
    srv: ID3D11ShaderResourceView,
    dsv: ID3D11DepthStencilView,
    /// Render scale: where 3D polygons drew since the last composition (R8, 1 = drawn).
    cov: Option<(ID3D11RenderTargetView, ID3D11ShaderResourceView)>,
}

/// Render scale: the picture at full resolution (see the module documentation).
struct OutTarget {
    width: u32,
    height: u32,
    rtv: ID3D11RenderTargetView,
    srv: ID3D11ShaderResourceView,
}

fn viewport(w: u32, h: u32) -> D3D11_VIEWPORT {
    D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: w as f32, Height: h as f32, MinDepth: 0.0, MaxDepth: 1.0 }
}

/// Widescreen: the columns of a `width`-pixel target outside the game's 4:3 area (the
/// game's `game_w` columns widened by `ext` on each side): `0..left` and `right..width`.
/// The 4:3 area spans `ext * sx .. (ext + game_w) * sx` as `lfb_write` places a 640 x 480
/// picture there; a pixel is the picture's when its centre is inside. (Rounding the left
/// edge down instead left a column on each side that neither the picture nor the clear
/// touched at 1280 x 720, where the edge comes out at 159.99998: trails of the name entry's
/// spinning letters stayed there.)
fn side_columns(width: u32, game_w: u32, ext: f32) -> (i32, i32) {
    let sx = width as f32 / (game_w as f32 + 2.0 * ext);
    let left = ext * sx;
    let right = left + game_w as f32 * sx;
    (((left - 0.5).ceil() as i32).max(0), ((right - 0.5).ceil() as i32).min(width as i32))
}

/// Swap chain flags (the same at creation and on every resize).
/// The frame-latency waitable object is not allowed in exclusive fullscreen, so the swap
/// chain is made without it for that mode.
fn swap_flags(tearing: bool, waitable: bool) -> DXGI_SWAP_CHAIN_FLAG {
    let t = if tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 } else { 0 };
    let w = if waitable { DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 } else { 0 };
    DXGI_SWAP_CHAIN_FLAG(t | w)
}

/// Create the flip-model swap chain for `hwnd`; with `waitable`, also its frame-latency
/// object (latency 1). DXGI must not handle Alt+Enter (it would switch to exclusive
/// fullscreen and change the display mode) or react to window changes: the window and the
/// renderer handle the modes themselves.
unsafe fn create_swap_chain(
    factory: &IDXGIFactory2,
    device: &ID3D11Device,
    hwnd: HWND,
    size: (u32, u32),
    tearing: bool,
    waitable: bool,
) -> Result<(IDXGISwapChain1, Option<windows::Win32::Foundation::HANDLE>), String> {
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: size.0.max(1),
        Height: size.1.max(1),
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: swap_flags(tearing, waitable).0 as u32,
        ..Default::default()
    };
    unsafe {
        let swap = factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None).map_err(err)?;
        let parent: IDXGIFactory1 = swap.GetParent().unwrap_or_else(|_| factory.cast().unwrap());
        let _ = parent.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);
        let latency_wait = if waitable {
            swap.cast::<IDXGISwapChain2>().ok().and_then(|s2| {
                let _ = s2.SetMaximumFrameLatency(1);
                let h = s2.GetFrameLatencyWaitableObject();
                (!h.is_invalid()).then_some(h)
            })
        } else {
            // blocking presents, at most one frame queued
            if let Ok(d1) = device.cast::<IDXGIDevice1>() {
                let _ = d1.SetMaximumFrameLatency(1);
            }
            None
        };
        Ok((swap, latency_wait))
    }
}

/// The current display mode (width, height, refresh rate in Hz) of the monitor `hwnd` is on.
fn desktop_mode(hwnd: HWND) -> Option<(u32, u32, u32)> {
    use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow};
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(mon, &mut mi as *mut _ as *mut _).as_bool() {
            return None;
        }
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if !EnumDisplaySettingsW(windows::core::PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() {
            return None;
        }
        // the size from the monitor rectangle (the window covers it in real pixels; the
        // display-settings size can be scaled for DPI), the refresh rate from the settings
        let r = mi.monitorInfo.rcMonitor;
        Some(((r.right - r.left) as u32, (r.bottom - r.top) as u32, dm.dmDisplayFrequency))
    }
}

/// Refresh interval of the monitor `hwnd` is on (its current display mode).
fn refresh_period(hwnd: HWND) -> std::time::Duration {
    use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow};
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let mut hz = 60;
        if GetMonitorInfoW(mon, &mut mi as *mut _ as *mut _).as_bool() {
            let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
            if EnumDisplaySettingsW(windows::core::PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() && dm.dmDisplayFrequency > 1 {
                hz = dm.dmDisplayFrequency;
            }
        }
        std::time::Duration::from_secs_f64(1.0 / hz as f64)
    }
}

pub struct D3dBackend {
    _hwnd: HWND,
    window_size: Arc<AtomicU64>,
    settings: RenderSettings,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    ctx1: ID3D11DeviceContext1,
    swap: IDXGISwapChain1,
    tearing: bool,
    /// Signalled when the swap chain can take a frame without blocking (frame latency 1).
    latency_wait: Option<windows::Win32::Foundation::HANDLE>,
    /// The window wants exclusive fullscreen (see `Window::exclusive`), and whether the
    /// swap chain has it.
    want_exclusive: Arc<std::sync::atomic::AtomicBool>,
    exclusive: bool,
    /// For recreating the swap chain (exclusive fullscreen needs other flags).
    factory: IDXGIFactory2,
    hwnd: HWND,
    /// Exclusive fullscreen: the display's refresh interval (VSync pacing without the
    /// waitable object).
    refresh: std::time::Duration,
    /// Exclusive fullscreen: when the last present returned (about a vertical blank).
    last_present: Option<std::time::Instant>,
    /// Exclusive fullscreen: the display mode's size (the buffers follow it).
    exclusive_size: Option<(u32, u32)>,
    /// Exclusive fullscreen: the desktop's mode when it was taken (to return to "desktop"),
    /// the mode asked for when the display was last set (`RenderSettings::exclusive_mode`),
    /// and when the request last changed (a change applies once it has rested a second, so
    /// that stepping through the list does not switch the display at every step).
    desktop: Option<(u32, u32, u32)>,
    mode_applied: Option<(u32, u32, u32)>,
    mode_changed: std::time::Instant,
    /// Swap chains made after the first (mode changes).
    recreations: u32,
    /// Exclusive fullscreen wanted but not held: when taking it last failed (tried again
    /// half a second later), and whether waiting for the foreground was logged.
    exclusive_failed: Option<std::time::Instant>,
    exclusive_waiting: bool,
    /// `wait_ready` already took the slot for the next present. (Every present takes one,
    /// or the object's count runs ahead and waiting stops meaning anything.)
    ready_taken: bool,
    back: Option<ID3D11RenderTargetView>,
    back_size: (u32, u32),
    scene: Option<Targets>,
    /// Render scale other than 100 %: the full-resolution picture.
    out: Option<OutTarget>,
    /// Render scale: 3D drew into the scene since its last composition into `out`.
    pending_3d: bool,
    /// Render scale: the scene's coverage target is bound with it (3D drawing).
    cov_bound: bool,
    vs: ID3D11VertexShader,
    gs: ID3D11GeometryShader,
    /// Raw pointers of the pipeline objects bound for the last 3D draw: unchanged state is
    /// not sent again (None = unknown, e.g. after a blit or the overlay).
    bound: Option<Bound>,
    ps: ID3D11PixelShader,
    /// `ps` that also marks the coverage target (render scale, 3D).
    ps_cov: ID3D11PixelShader,
    /// Render scale: the 3D layer composed over `out` (premultiplied by its coverage).
    ps_resolve: ID3D11PixelShader,
    blend_premul: ID3D11BlendState,
    layout: ID3D11InputLayout,
    vs_blit: ID3D11VertexShader,
    ps_blit: ID3D11PixelShader,
    /// 2D pictures at their own resolution (`draw_picture`, `sr_runtime::ui2d`).
    ps_picture: ID3D11PixelShader,
    cb_picture: ID3D11Buffer,
    /// The game's pictures as textures (with a transparent border of one texel), by
    /// `Picture::key`, with the frame they were last drawn in.
    pictures: FastMap<u64, (ID3D11ShaderResourceView, u64)>,
    cb: ID3D11Buffer,
    cb_blit: ID3D11Buffer,
    vb: ID3D11Buffer,
    vb_pos: u32,
    raster: ID3D11RasterizerState,
    /// Depth-buffered 3D polygons: rasterizer with a slope-scaled depth bias (by scene
    /// scale) so that later-drawn coplanar polygons (decals, road overlays) still win.
    raster_depth: Vec<(u32, ID3D11RasterizerState)>,
    blends: FastMap<[u32; 6], ID3D11BlendState>,
    depths: FastMap<(u32, u32, u32), ID3D11DepthStencilState>,
    samplers: FastMap<(u32, u32, u32, u32), ID3D11SamplerState>,
    textures: FastMap<TexKey, CachedTex>,
    /// The constant buffer's contents as last uploaded (skip identical uploads).
    cb_last: Option<StateCb>,
    /// Triangles per object kind: [opaque, opaque back, blended, blended back] (diagnostics).
    kind_stats: [[u64; 4]; 8],
    lfb: Option<(ID3D11Texture2D, ID3D11ShaderResourceView, u32, u32)>,
    /// Kept LFB images (the HUD, drawn again on interpolated frames): texture, view, size
    /// and the generation uploaded.
    lfb_slots: Vec<Option<(ID3D11Texture2D, ID3D11ShaderResourceView, u32, u32, u64)>>,
    game: (u32, u32),
    /// Widescreen extension in game pixels on each side (0 = 4:3).
    ext: f32,
    /// The sides outside the game's 4:3 area are cleared before the next frame's first
    /// drawing (`clear_sides`), not right after the present: the frame shown stays whole
    /// until then, so showing it again (the settings overlay over a held game, `hold`)
    /// shows all of it.
    sides_pending: bool,
    painter: Option<crate::overlay::Painter>,
    /// Frame rate counter: window start, presents in it, the value shown.
    fps_count: (Option<std::time::Instant>, u32, f32),
    gamma: f32,
    pub frames: u64,
    /// The native renderer, created by the first `native_upload`.
    native: Option<crate::native::NativeRenderer>,
    /// `SR_NATIVE_TEST`: the synthetic scene and its timing.
    native_test: Option<crate::native::TestDriver>,
    /// `SR_D3D_DEBUG`: the debug layer's message queue, and messages logged so far.
    info_queue: Option<ID3D11InfoQueue>,
    debug_messages: u64,
    /// CRT emulation (`crt`).
    crt: sr_crt::Stage,
}

// The backend is created on the main thread and then used only by the guest thread.
unsafe impl Send for D3dBackend {}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let r = D3DCompile(
            SHADER.as_ptr() as *const _,
            SHADER.len(),
            s!("voodoo.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        );
        if let Err(e) = r {
            let msg = errors
                .map(|b: windows::Win32::Graphics::Direct3D::ID3DBlob| {
                    String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).to_string()
                })
                .unwrap_or_default();
            return Err(format!("shader compile failed: {e} {msg}"));
        }
        let b = code.unwrap();
        Ok(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec())
    }
}

fn err(e: windows::core::Error) -> String {
    e.to_string()
}

impl D3dBackend {
    pub fn new(window: &crate::window::Window, settings: RenderSettings) -> Result<D3dBackend, String> {
        unsafe {
            let hwnd = HWND(window.hwnd as *mut _);
            let mut device = None;
            let mut ctx = None;
            // SR_D3D_DEBUG=1 (diagnostics): the Direct3D debug layer, its messages in the log
            let debug = std::env::var_os("SR_D3D_DEBUG").is_some();
            let flags = D3D11_CREATE_DEVICE_FLAG(D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 | if debug { D3D11_CREATE_DEVICE_DEBUG.0 } else { 0 });
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                flags,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .map_err(err)?;
            let device: ID3D11Device = device.unwrap();
            let ctx: ID3D11DeviceContext = ctx.unwrap();
            let ctx1: ID3D11DeviceContext1 = ctx.cast().map_err(err)?;

            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(err)?;
            let tearing = factory
                .cast::<IDXGIFactory5>()
                .map(|f5| {
                    let mut allow = windows::core::BOOL(0);
                    f5.CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING, &mut allow as *mut _ as *mut _, 4).is_ok() && allow.as_bool()
                })
                .unwrap_or(false);
            // exclusive fullscreen at start: no waitable object (see swap_flags)
            let waitable = !window.exclusive.load(Ordering::Relaxed);
            let (swap, latency_wait) = create_swap_chain(&factory, &device, hwnd, window.client_size(), tearing, waitable)?;

            let vs_code = compile(s!("vs_main"), s!("vs_5_0"))?;
            let ps_code = compile(s!("ps_main"), s!("ps_5_0"))?;
            let gs_code = compile(s!("gs_grow"), s!("gs_5_0"))?;
            let mut gs = None;
            device.CreateGeometryShader(&gs_code, None, Some(&mut gs)).map_err(err)?;
            let vsb_code = compile(s!("vs_blit"), s!("vs_5_0"))?;
            let psb_code = compile(s!("ps_blit"), s!("ps_5_0"))?;
            let psc_code = compile(s!("ps_main_cov"), s!("ps_5_0"))?;
            let psr_code = compile(s!("ps_resolve"), s!("ps_5_0"))?;
            let psp_code = compile(s!("ps_picture"), s!("ps_5_0"))?;
            let mut ps_picture = None;
            device.CreatePixelShader(&psp_code, None, Some(&mut ps_picture)).map_err(err)?;
            let mut vs = None;
            let mut ps = None;
            let mut vs_blit = None;
            let mut ps_blit = None;
            let mut ps_cov = None;
            let mut ps_resolve = None;
            device.CreateVertexShader(&vs_code, None, Some(&mut vs)).map_err(err)?;
            device.CreatePixelShader(&ps_code, None, Some(&mut ps)).map_err(err)?;
            device.CreateVertexShader(&vsb_code, None, Some(&mut vs_blit)).map_err(err)?;
            device.CreatePixelShader(&psb_code, None, Some(&mut ps_blit)).map_err(err)?;
            device.CreatePixelShader(&psc_code, None, Some(&mut ps_cov)).map_err(err)?;
            device.CreatePixelShader(&psr_code, None, Some(&mut ps_resolve)).map_err(err)?;
            // premultiplied "over" (the 3D layer's composition)
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend_premul = None;
            device.CreateBlendState(&bd, Some(&mut blend_premul)).map_err(err)?;

            let el = |name: PCSTR, fmt: DXGI_FORMAT, off: u32| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: fmt,
                InputSlot: 0,
                AlignedByteOffset: off,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            };
            let elements = [
                el(s!("POSITION"), DXGI_FORMAT_R32G32_FLOAT, 0),
                el(s!("OOZ"), DXGI_FORMAT_R32_FLOAT, 8),
                el(s!("OOW"), DXGI_FORMAT_R32_FLOAT, 12),
                el(s!("COLOR"), DXGI_FORMAT_R32G32B32A32_FLOAT, 16),
                el(s!("TEXCOORD"), DXGI_FORMAT_R32G32B32_FLOAT, 32),
            ];
            let mut layout = None;
            device.CreateInputLayout(&elements, &vs_code, Some(&mut layout)).map_err(err)?;

            let buffer = |size: u32, bind: D3D11_BIND_FLAG| -> Result<ID3D11Buffer, String> {
                let desc = D3D11_BUFFER_DESC {
                    ByteWidth: size,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: bind.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                };
                let mut b = None;
                device.CreateBuffer(&desc, None, Some(&mut b)).map_err(err)?;
                Ok(b.unwrap())
            };
            let cb = buffer(std::mem::size_of::<StateCb>().div_ceil(16) as u32 * 16, D3D11_BIND_CONSTANT_BUFFER)?;
            let cb_blit = buffer(16, D3D11_BIND_CONSTANT_BUFFER)?;
            let cb_picture = buffer(32, D3D11_BIND_CONSTANT_BUFFER)?;
            let vb = buffer(VB_VERTICES * std::mem::size_of::<Vertex>() as u32, D3D11_BIND_VERTEX_BUFFER)?;

            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: false.into(),
                ScissorEnable: true.into(),
                ..Default::default()
            };
            let mut raster = None;
            device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(err)?;

            sr_runtime::depth::ACTIVE.store(settings.depth, Ordering::Relaxed);
            let crt = sr_crt::Stage::new(settings.crt.clone());
            let mut b = D3dBackend {
                _hwnd: hwnd,
                window_size: window.size.clone(),
                want_exclusive: window.exclusive.clone(),
                exclusive: false,
                factory: factory.clone(),
                hwnd,
                refresh: {
                    let r = refresh_period(hwnd);
                    sr_runtime::info!("display: the window's monitor refreshes at {:.0} Hz", 1.0 / r.as_secs_f64());
                    r
                },
                last_present: None,
                exclusive_size: None,
                desktop: None,
                mode_applied: None,
                mode_changed: std::time::Instant::now(),
                recreations: 0,
                exclusive_failed: None,
                exclusive_waiting: false,
                settings,
                device,
                ctx,
                ctx1,
                swap,
                tearing,
                latency_wait,
                ready_taken: false,
                back: None,
                back_size: (0, 0),
                scene: None,
                out: None,
                pending_3d: false,
                cov_bound: false,
                vs: vs.unwrap(),
                gs: gs.unwrap(),
                bound: None,
                ps: ps.unwrap(),
                ps_cov: ps_cov.unwrap(),
                ps_resolve: ps_resolve.unwrap(),
                blend_premul: blend_premul.unwrap(),
                layout: layout.unwrap(),
                vs_blit: vs_blit.unwrap(),
                ps_blit: ps_blit.unwrap(),
                ps_picture: ps_picture.unwrap(),
                cb_picture,
                pictures: FastMap::default(),
                cb,
                cb_blit,
                vb,
                vb_pos: 0,
                raster: raster.unwrap(),
                raster_depth: Vec::new(),
                blends: FastMap::default(),
                depths: FastMap::default(),
                samplers: FastMap::default(),
                textures: FastMap::default(),
                cb_last: None,
                kind_stats: [[0; 4]; 8],
                lfb: None,
                lfb_slots: Vec::new(),
                game: (640, 480),
                ext: 0.0,
                sides_pending: false,
                painter: None,
                fps_count: (None, 0, 0.0),
                gamma: 1.0,
                frames: 0,
                native: None,
                native_test: crate::native::TestDriver::from_env(),
                info_queue: None,
                debug_messages: 0,
                crt,
            };
            if debug {
                b.info_queue = b.device.cast::<ID3D11InfoQueue>().ok();
                sr_runtime::info!("display: Direct3D debug layer {}", if b.info_queue.is_some() { "on" } else { "not available" });
            }
            b.resize_if_needed()?;
            Ok(b)
        }
    }

    /// The rectangle of the window the picture is shown in: the whole window (Auto, at
    /// least 4:3) or the aspect preset's shape with black bars (`display::view_rect`).
    fn present_rect(&self) -> (u32, u32, u32, u32) {
        crate::display::view_rect(self.back_size, self.game, self.settings.aspect)
    }

    /// The output's display mode closest to `(w, h, hz)` if it has that resolution.
    fn find_mode(&self, out: &IDXGIOutput, (w, h, hz): (u32, u32, u32)) -> Option<DXGI_MODE_DESC> {
        let want = DXGI_MODE_DESC {
            Width: w,
            Height: h,
            RefreshRate: DXGI_RATIONAL { Numerator: hz, Denominator: 1 },
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..Default::default()
        };
        let mut mode = DXGI_MODE_DESC::default();
        let found = unsafe { out.FindClosestMatchingMode(&want, &mut mode, &self.device) }.is_ok();
        (found && mode.Width == w && mode.Height == h).then_some(mode)
    }

    /// Exclusive fullscreen: the mode to use, the one asked for or else the desktop's.
    fn exclusive_target(&self, out: &IDXGIOutput) -> Option<DXGI_MODE_DESC> {
        if let Some(m) = self.settings.exclusive_mode {
            match self.find_mode(out, m) {
                Some(d) => return Some(d),
                None => sr_runtime::warn!("display: the display offers no {}x{} at {} Hz: using the desktop's mode", m.0, m.1, m.2),
            }
        }
        let d = self.desktop?;
        let found = self.find_mode(out, d);
        if found.is_none() {
            sr_runtime::warn!("display: no exclusive mode matches the desktop ({}x{} at {} Hz)", d.0, d.1, d.2);
        }
        found
    }

    /// Exclusive fullscreen is on: switch the display to `mode`, the buffers follow.
    fn set_exclusive_mode(&mut self, mode: &DXGI_MODE_DESC) -> String {
        let _ = unsafe { self.swap.ResizeTarget(mode) };
        let r = mode.RefreshRate;
        if r.Numerator > 0 && r.Denominator > 0 {
            self.refresh = std::time::Duration::from_secs_f64(r.Denominator as f64 / r.Numerator as f64);
        }
        self.exclusive_size = Some((mode.Width, mode.Height));
        self.back_size = (0, 0);
        format!("{}x{} at {:.2} Hz", mode.Width, mode.Height, r.Numerator as f64 / r.Denominator.max(1) as f64)
    }

    /// Exclusive fullscreen is on and another mode was asked for (and has rested a second):
    /// switch to it.
    fn follow_exclusive_mode(&mut self) {
        if !self.exclusive || self.mode_applied == self.settings.exclusive_mode || self.mode_changed.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.mode_applied = self.settings.exclusive_mode;
        let Ok(out) = (unsafe { self.swap.GetContainingOutput() }) else { return };
        if let Some(mode) = self.exclusive_target(&out) {
            let text = self.set_exclusive_mode(&mode);
            sr_runtime::info!("display: exclusive fullscreen now {text}");
        }
    }

    /// Take or give back exclusive fullscreen as the window asks.
    fn follow_exclusive(&mut self) {
        let want = self.want_exclusive.load(Ordering::Relaxed);
        // (not exclusive but still without the waitable object, e.g. exclusive was refused:
        // make the usual swap chain again, a few attempts at most)
        let restore_waitable = !want && self.latency_wait.is_none() && self.recreations < 4;
        if want == self.exclusive && !restore_waitable {
            return;
        }
        // DXGI gives the display only to the window in the foreground (otherwise
        // DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, e.g. started from another program while the user
        // works in a third one): until the game is in front it stays a borderless window and
        // presents as such; a refused attempt is repeated at most twice a second (each one
        // resets the pipeline, the window's size and the buffers; they were made at every
        // present, thousands in a minute)
        if want && !self.exclusive {
            let front = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } == self.hwnd;
            if !front {
                if !std::mem::replace(&mut self.exclusive_waiting, true) {
                    sr_runtime::debug!("display: exclusive fullscreen waits for the game to be in the foreground");
                }
                return;
            }
            self.exclusive_waiting = false;
            if self.exclusive_failed.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(500)) {
                return;
            }
        }
        unsafe {
            self.ctx.OMSetRenderTargets(None, None);
            self.back = None;
            self.ctx.ClearState();
            self.ctx.Flush();
            if !want {
                let t = std::time::Instant::now();
                let r = self.swap.SetFullscreenState(false, None);
                sr_runtime::debug!("display: leaving exclusive fullscreen: {r:?} in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
                self.exclusive = false;
            }
            // the swap chain flags differ between the modes: make a new one
            let waitable = !want;
            if waitable != self.latency_wait.is_some() {
                let size = (self.back_size.0.max(1), self.back_size.1.max(1));
                // a window can have only one flip-model swap chain: release the old one
                // before making the new one (and fall back to the old flags if that fails)
                // SAFETY: self.swap is overwritten below before it is used again.
                drop(std::ptr::read(&self.swap));
                self.ctx.Flush();
                let made = create_swap_chain(&self.factory, &self.device, self.hwnd, size, self.tearing, waitable)
                    .or_else(|e| {
                        sr_runtime::warn!("display: cannot recreate the swap chain ({e})");
                        create_swap_chain(&self.factory, &self.device, self.hwnd, size, self.tearing, !waitable)
                    })
                    .expect("cannot recreate the swap chain");
                std::ptr::write(&mut self.swap, made.0);
                self.recreations += 1;
                self.latency_wait = made.1;
                self.ready_taken = false;
                if waitable != self.latency_wait.is_some() {
                    self.want_exclusive.store(self.exclusive, Ordering::Relaxed);
                    self.back_size = (0, 0);
                    return;
                }
            }
            if !want {
                sr_runtime::info!("display: exclusive fullscreen off");
                self.back_size = (0, 0);
                return;
            }
            // the display mode asked for (`exclusive_mode`), by default the desktop's (DXGI
            // would otherwise pick a refresh rate itself, e.g. 24 Hz on a 4K display); never
            // a resolution that was not asked for
            let output = self.swap.GetContainingOutput().ok();
            self.exclusive_size = None;
            self.desktop = desktop_mode(self.hwnd);
            self.mode_applied = self.settings.exclusive_mode;
            let Some(mode) = output.as_ref().and_then(|out| self.exclusive_target(out)) else {
                sr_runtime::warn!("display: no exclusive display mode to use: staying borderless");
                self.want_exclusive.store(false, Ordering::Relaxed);
                self.back_size = (0, 0);
                return;
            };
            let mode_text = self.set_exclusive_mode(&mode);
            match self.swap.SetFullscreenState(want, output.as_ref()) {
                Ok(()) => {
                    self.exclusive = want;
                    self.last_present = None;
                    self.exclusive_failed = None;
                    sr_runtime::info!("display: exclusive fullscreen on ({mode_text})");
                }
                Err(e) => {
                    // not possible now: stay as we are and try again half a second later
                    self.exclusive_failed = Some(std::time::Instant::now());
                    if want {
                        sr_runtime::debug!("display: exclusive fullscreen not available now ({e})");
                    }
                }
            }
        }
        // the buffers are recreated at the new size
        self.back_size = (0, 0);
    }

    fn resize_if_needed(&mut self) -> Result<(), String> {
        self.follow_exclusive();
        self.follow_exclusive_mode();
        let v = self.window_size.load(Ordering::Relaxed);
        let (w, h) = match (self.exclusive, self.exclusive_size) {
            (true, Some(s)) => s,
            _ => (((v >> 32) as u32).max(1), (v as u32).max(1)),
        };
        if (w, h) != self.back_size || self.back.is_none() {
            unsafe {
                self.ctx.OMSetRenderTargets(None, None);
                self.back = None;
                self.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, swap_flags(self.tearing, self.latency_wait.is_some())).map_err(err)?;
                let bb: ID3D11Texture2D = self.swap.GetBuffer(0).map_err(err)?;
                let mut rtv = None;
                self.device.CreateRenderTargetView(&bb, None, Some(&mut rtv)).map_err(err)?;
                self.back = rtv;
            }
            self.back_size = (w, h);
        }
        // the picture (pw x ph) and the 3D scene at the render scale (sw x sh)
        let (_, _, pw, ph) = self.present_rect();
        // CRT emulation on: the whole frame at the shader's input size (`crt`)
        let crt = self.crt_input((pw, ph));
        let scaled = crt.is_none() && self.settings.render_scale != 100;
        let (sw, sh) = crt.unwrap_or_else(|| crate::display::render_size((pw, ph), self.settings.render_scale));
        let new_scene = self.scene.as_ref().is_none_or(|s| s.width != sw || s.height != sh || s.cov.is_some() != scaled);
        let new_out = scaled && self.out.as_ref().is_none_or(|o| o.width != pw || o.height != ph);
        if new_scene || new_out || (!scaled && self.out.is_some()) {
            // the picture so far, carried over scaled (a paused game shows it while the
            // settings change)
            let old = self.out.as_ref().map(|o| (o.srv.clone(), o.width, o.height)).or_else(|| self.scene.as_ref().map(|s| (s.srv.clone(), s.width, s.height)));
            if new_scene {
                self.create_scene(sw, sh, scaled)?;
            }
            self.out = None;
            if scaled {
                self.out = Some(self.create_out(pw, ph)?);
            }
            if let Some(old) = old {
                self.carry_over(&old, new_scene);
            }
            self.pending_3d = false;
            if new_scene || new_out {
                sr_runtime::info!(
                    "display: picture {pw}x{ph}, 3D scene {sw}x{sh} ({}%), {:?}",
                    self.settings.render_scale,
                    self.settings.aspect
                );
            }
        }
        self.ext = crate::display::extension(self.game, self.settings.aspect, (pw, ph));
        sr_runtime::view::set_ext(self.ext);
        sr_runtime::view::set_scale(sh as f32 / self.game.1 as f32);
        Ok(())
    }

    /// Draw the previous picture (`old`: view, width, height) into the new targets, scaled to
    /// fit with its shape kept: the scene (when it was replaced) and the full-resolution
    /// picture.
    fn carry_over(&mut self, (old, ow, oh): &(ID3D11ShaderResourceView, u32, u32), scene: bool) {
        let mut targets = Vec::new();
        if let (true, Some(s)) = (scene, &self.scene) {
            targets.push((s.rtv.clone(), s.width, s.height));
        }
        if let Some(o) = &self.out {
            targets.push((o.rtv.clone(), o.width, o.height));
        }
        let gamma = std::mem::replace(&mut self.gamma, 1.0);
        for (rtv, w, h) in targets {
            let k = (w as f32 / (*ow).max(1) as f32).min(h as f32 / (*oh).max(1) as f32);
            let (vw, vh) = (*ow as f32 * k, *oh as f32 * k);
            unsafe {
                self.ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
                self.ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    TopLeftX: ((w as f32 - vw) / 2.0).floor(),
                    TopLeftY: ((h as f32 - vh) / 2.0).floor(),
                    Width: vw,
                    Height: vh,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                }]));
                self.ctx.RSSetScissorRects(Some(&[RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 }]));
            }
            self.draw_fullscreen(old, 0.0, true);
        }
        self.gamma = gamma;
        unsafe { self.ctx.PSSetShaderResources(0, Some(&[None])) };
        self.bind_scene();
    }

    fn create_out(&mut self, w: u32, h: u32) -> Result<OutTarget, String> {
        unsafe {
            let td = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                ..Default::default()
            };
            let mut color = None;
            self.device.CreateTexture2D(&td, None, Some(&mut color)).map_err(err)?;
            let color = color.unwrap();
            let (mut rtv, mut srv) = (None, None);
            self.device.CreateRenderTargetView(&color, None, Some(&mut rtv)).map_err(err)?;
            self.device.CreateShaderResourceView(&color, None, Some(&mut srv)).map_err(err)?;
            let o = OutTarget { width: w, height: h, rtv: rtv.unwrap(), srv: srv.unwrap() };
            self.ctx.ClearRenderTargetView(&o.rtv, &[0.0, 0.0, 0.0, 1.0]);
            Ok(o)
        }
    }

    /// Scene pixels per game pixel, horizontally and vertically.
    fn scene_scale(&self) -> (f32, f32) {
        let s = self.scene.as_ref().unwrap();
        (s.width as f32 / (self.game.0 as f32 + 2.0 * self.ext), s.height as f32 / self.game.1 as f32)
    }

    fn create_scene(&mut self, w: u32, h: u32, coverage: bool) -> Result<(), String> {
        unsafe {
            let td = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                ..Default::default()
            };
            let mut color = None;
            self.device.CreateTexture2D(&td, None, Some(&mut color)).map_err(err)?;
            let color = color.unwrap();
            let mut rtv = None;
            let mut srv = None;
            self.device.CreateRenderTargetView(&color, None, Some(&mut rtv)).map_err(err)?;
            self.device.CreateShaderResourceView(&color, None, Some(&mut srv)).map_err(err)?;
            let dd = D3D11_TEXTURE2D_DESC {
                Format: DXGI_FORMAT_D32_FLOAT_S8X24_UINT,
                BindFlags: D3D11_BIND_DEPTH_STENCIL.0 as u32,
                ..td
            };
            let mut depth = None;
            self.device.CreateTexture2D(&dd, None, Some(&mut depth)).map_err(err)?;
            let mut dsv = None;
            self.device.CreateDepthStencilView(&depth.unwrap(), None, Some(&mut dsv)).map_err(err)?;
            let cov = if coverage {
                let cd = D3D11_TEXTURE2D_DESC { Format: DXGI_FORMAT_R8_UNORM, ..td };
                let mut c = None;
                self.device.CreateTexture2D(&cd, None, Some(&mut c)).map_err(err)?;
                let c = c.unwrap();
                let (mut crtv, mut csrv) = (None, None);
                self.device.CreateRenderTargetView(&c, None, Some(&mut crtv)).map_err(err)?;
                self.device.CreateShaderResourceView(&c, None, Some(&mut csrv)).map_err(err)?;
                self.ctx.ClearRenderTargetView(crtv.as_ref().unwrap(), &[0.0; 4]);
                Some((crtv.unwrap(), csrv.unwrap()))
            } else {
                None
            };
            let t = Targets { width: w, height: h, rtv: rtv.unwrap(), srv: srv.unwrap(), dsv: dsv.unwrap(), cov };
            self.ctx.ClearRenderTargetView(&t.rtv, &[0.0, 0.0, 0.0, 1.0]);
            self.ctx.ClearDepthStencilView(&t.dsv, (D3D11_CLEAR_DEPTH.0 | D3D11_CLEAR_STENCIL.0) as u32, 0.0, 0);
            self.scene = Some(t);
        }
        Ok(())
    }

    /// Render scale: bind the scene with its coverage target (3D drawing) or without (2D).
    fn bind_scene_cov(&mut self, on: bool) {
        if self.cov_bound == on {
            return;
        }
        let Some(s) = &self.scene else { return };
        let Some((cov, _)) = &s.cov else { return };
        unsafe { self.ctx.OMSetRenderTargets(Some(&[Some(s.rtv.clone()), on.then(|| cov.clone())]), &s.dsv) };
        self.cov_bound = on;
        self.bound = None;
    }

    /// Render scale: compose the 3D drawn into the scene since the last time over the
    /// full-resolution picture, where the coverage target marks it, filtered from the
    /// scene's size to the picture's (`ps_resolve`); then start the next layer.
    fn resolve_3d(&mut self) {
        if !std::mem::take(&mut self.pending_3d) {
            return;
        }
        let (Some(s), Some(o)) = (&self.scene, &self.out) else { return };
        let Some((cov_rtv, cov_srv)) = &s.cov else { return };
        let k = [s.width as f32 / o.width as f32, s.height as f32 / o.height as f32, 0.0, 0.0];
        let (o_rtv, ow, oh) = (o.rtv.clone(), o.width, o.height);
        let (scene_srv, cov_rtv, cov_srv) = (s.srv.clone(), cov_rtv.clone(), cov_srv.clone());
        self.upload_cb(&self.cb_blit, &k);
        unsafe {
            self.ctx.OMSetRenderTargets(Some(&[Some(o_rtv)]), None);
            self.ctx.RSSetViewports(Some(&[viewport(ow, oh)]));
            self.ctx.RSSetScissorRects(Some(&[RECT { left: 0, top: 0, right: ow as i32, bottom: oh as i32 }]));
            self.ctx.GSSetShader(None, None);
            self.ctx.IASetInputLayout(None);
            self.ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.VSSetShader(&self.vs_blit, None);
            self.ctx.PSSetShader(&self.ps_resolve, None);
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb_blit.clone())]));
            self.ctx.PSSetShaderResources(0, Some(&[Some(scene_srv), Some(cov_srv)]));
            self.ctx.OMSetBlendState(&self.blend_premul, None, 0xffff_ffff);
            self.ctx.OMSetDepthStencilState(None, 0);
            self.ctx.RSSetState(&self.raster);
            self.ctx.Draw(3, 0);
            self.ctx.PSSetShaderResources(0, Some(&[None, None]));
            self.ctx.ClearRenderTargetView(&cov_rtv, &[0.0; 4]);
        }
        self.bind_scene();
    }

    /// Render scale: the 2D triangles just drawn into the scene (`n` vertices from `start`
    /// with the bound state), again into the full-resolution picture.
    fn redraw_2d_in_out(&mut self, st: &GlideState, n: u32, start: u32) {
        let Some(o) = &self.out else { return };
        let (rtv, size) = (o.rtv.clone(), (o.width, o.height));
        let rect = self.rect_in(st, size);
        unsafe {
            self.ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            self.ctx.RSSetViewports(Some(&[viewport(size.0, size.1)]));
            self.ctx.RSSetScissorRects(Some(&[rect]));
            self.ctx.Draw(n, start);
        }
        self.bind_scene();
    }

    fn bind_scene(&mut self) {
        self.bound = None;
        self.cov_bound = false;
        let Some(s) = &self.scene else { return };
        unsafe {
            self.ctx.OMSetRenderTargets(Some(&[Some(s.rtv.clone())]), &s.dsv);
            self.ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: s.width as f32,
                Height: s.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
        }
    }

    fn scale_rect(&self, st: &GlideState) -> RECT {
        let s = self.scene.as_ref().unwrap();
        self.rect_in(st, (s.width, s.height))
    }

    /// The game's clip window in a target of `size` pixels showing the (widened) view.
    fn rect_in(&self, st: &GlideState, size: (u32, u32)) -> RECT {
        let (sx, sy) = (size.0 as f32 / (self.game.0 as f32 + 2.0 * self.ext), size.1 as f32 / self.game.1 as f32);
        let [x0, y0, x1, y1] = st.clip;
        // a clip window spanning the game's whole width spans the whole (wider) view
        let (left, right) = if x0 == 0 && x1 >= self.game.0 {
            (0, size.0 as i32)
        } else {
            (((x0 as f32 + self.ext) * sx).round() as i32, ((x1.min(self.game.0) as f32 + self.ext) * sx).round() as i32)
        };
        RECT { left, top: (y0 as f32 * sy).round() as i32, right, bottom: (y1.min(self.game.1) as f32 * sy).round() as i32 }
    }

    fn blend_state(&mut self, st: &GlideState) -> ID3D11BlendState {
        let key = [st.blend[0], st.blend[1], st.blend[2], st.blend[3], st.color_mask as u32, st.alpha_mask as u32];
        if let Some(b) = self.blends.get(&key) {
            return b.clone();
        }
        let src = |f: u32, alpha: bool| match f {
            GR_BLEND_ZERO => D3D11_BLEND_ZERO,
            GR_BLEND_SRC_ALPHA => D3D11_BLEND_SRC_ALPHA,
            GR_BLEND_SRC_COLOR => if alpha { D3D11_BLEND_DEST_ALPHA } else { D3D11_BLEND_DEST_COLOR },
            GR_BLEND_DST_ALPHA => D3D11_BLEND_DEST_ALPHA,
            GR_BLEND_ONE => D3D11_BLEND_ONE,
            GR_BLEND_ONE_MINUS_SRC_ALPHA => D3D11_BLEND_INV_SRC_ALPHA,
            GR_BLEND_ONE_MINUS_SRC_COLOR => if alpha { D3D11_BLEND_INV_DEST_ALPHA } else { D3D11_BLEND_INV_DEST_COLOR },
            GR_BLEND_ONE_MINUS_DST_ALPHA => D3D11_BLEND_INV_DEST_ALPHA,
            GR_BLEND_ALPHA_SATURATE => D3D11_BLEND_SRC_ALPHA_SAT,
            _ => D3D11_BLEND_ONE,
        };
        let dst = |f: u32, alpha: bool| match f {
            GR_BLEND_ZERO => D3D11_BLEND_ZERO,
            GR_BLEND_SRC_ALPHA => D3D11_BLEND_SRC_ALPHA,
            GR_BLEND_SRC_COLOR => if alpha { D3D11_BLEND_SRC_ALPHA } else { D3D11_BLEND_SRC_COLOR },
            GR_BLEND_DST_ALPHA => D3D11_BLEND_DEST_ALPHA,
            GR_BLEND_ONE => D3D11_BLEND_ONE,
            GR_BLEND_ONE_MINUS_SRC_ALPHA => D3D11_BLEND_INV_SRC_ALPHA,
            GR_BLEND_ONE_MINUS_SRC_COLOR => if alpha { D3D11_BLEND_INV_SRC_ALPHA } else { D3D11_BLEND_INV_SRC_COLOR },
            GR_BLEND_ONE_MINUS_DST_ALPHA => D3D11_BLEND_INV_DEST_ALPHA,
            _ => D3D11_BLEND_ZERO,
        };
        let enable = !(st.blend[0] == GR_BLEND_ONE && st.blend[1] == GR_BLEND_ZERO && st.blend[2] == GR_BLEND_ONE && st.blend[3] == GR_BLEND_ZERO);
        let mask = (if st.color_mask != 0 { D3D11_COLOR_WRITE_ENABLE_RED.0 | D3D11_COLOR_WRITE_ENABLE_GREEN.0 | D3D11_COLOR_WRITE_ENABLE_BLUE.0 } else { 0 })
            | if st.alpha_mask != 0 { D3D11_COLOR_WRITE_ENABLE_ALPHA.0 } else { 0 };
        let mut desc = D3D11_BLEND_DESC::default();
        desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: enable.into(),
            SrcBlend: src(st.blend[0], false),
            DestBlend: dst(st.blend[1], false),
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: src(st.blend[2], true),
            DestBlendAlpha: dst(st.blend[3], true),
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: mask as u8,
        };
        let mut b = None;
        unsafe { self.device.CreateBlendState(&desc, Some(&mut b)).expect("blend state") };
        let b = b.unwrap();
        self.blends.insert(key, b.clone());
        b
    }

    fn depth_state(&mut self, st: &GlideState) -> ID3D11DepthStencilState {
        let key = (st.depth_mode, st.depth_func, st.depth_mask as u32);
        if let Some(d) = self.depths.get(&key) {
            return d.clone();
        }
        let wbuf = st.depth_mode == GR_DEPTHBUFFER_WBUFFER || st.depth_mode == 4;
        // our depth is larger-is-nearer; Glide's W buffer stores larger-is-farther
        let func = match (st.depth_func & 7, wbuf) {
            (1, true) => 4,
            (3, true) => 6,
            (4, true) => 1,
            (6, true) => 3,
            (f, _) => f,
        };
        let enabled = st.depth_mode != GR_DEPTHBUFFER_DISABLE;
        let desc = D3D11_DEPTH_STENCIL_DESC {
            DepthEnable: enabled.into(),
            DepthWriteMask: if enabled && st.depth_mask != 0 { D3D11_DEPTH_WRITE_MASK_ALL } else { D3D11_DEPTH_WRITE_MASK_ZERO },
            DepthFunc: D3D11_COMPARISON_FUNC(func as i32 + 1),
            ..Default::default()
        };
        let mut d = None;
        unsafe { self.device.CreateDepthStencilState(&desc, Some(&mut d)).expect("depth state") };
        let d = d.unwrap();
        self.depths.insert(key, d.clone());
        d
    }

    fn sampler(&mut self, st: &GlideState, mipmaps: bool) -> ID3D11SamplerState {
        let linear = match self.settings.filter {
            Filter::Nearest => (false, false),
            Filter::Bilinear => (true, true),
            Filter::Game => (st.filter[0] == GR_TEXTUREFILTER_BILINEAR, st.filter[1] == GR_TEXTUREFILTER_BILINEAR),
        };
        let key = (linear.0 as u32 | (linear.1 as u32) << 1, st.clamp[0], st.clamp[1], mipmaps as u32);
        if let Some(s) = self.samplers.get(&key) {
            return s.clone();
        }
        let filter = match linear {
            (false, false) => D3D11_FILTER_MIN_MAG_MIP_POINT,
            (true, false) => D3D11_FILTER_MIN_LINEAR_MAG_MIP_POINT,
            (false, true) => D3D11_FILTER_MIN_POINT_MAG_LINEAR_MIP_POINT,
            (true, true) => D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT,
        };
        let addr = |c: u32| if c == GR_TEXTURECLAMP_CLAMP { D3D11_TEXTURE_ADDRESS_CLAMP } else { D3D11_TEXTURE_ADDRESS_WRAP };
        let desc = D3D11_SAMPLER_DESC {
            Filter: filter,
            AddressU: addr(st.clamp[0]),
            AddressV: addr(st.clamp[1]),
            AddressW: D3D11_TEXTURE_ADDRESS_WRAP,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            MinLOD: 0.0,
            MaxLOD: if mipmaps { f32::MAX } else { 0.0 },
            ..Default::default()
        };
        let mut s = None;
        unsafe { self.device.CreateSamplerState(&desc, Some(&mut s)).expect("sampler") };
        let s = s.unwrap();
        self.samplers.insert(key, s.clone());
        s
    }

    fn texture(&mut self, c: &DrawContext) -> Option<(ID3D11ShaderResourceView, (f32, f32), bool)> {
        let b = c.st.tex;
        if b.valid == 0 {
            return None;
        }
        let info = b.info;
        let paletted = matches!(info.format, GR_TEXFMT_P_8 | GR_TEXFMT_AP_88);
        let mipmaps = c.st.mipmap != GR_MIPMAP_DISABLE && info.small_lod > info.large_lod;
        let key = TexKey { start: b.start, even_odd: b.even_odd, info, palette_gen: if paletted { c.palette_gen } else { 0 }, mipmaps };
        if let Some(t) = self.textures.get(&key) {
            return Some((t.srv.clone(), t.range, mipmaps));
        }
        let last = if mipmaps { info.small_lod.min(8) } else { info.large_lod };
        let levels: Vec<(u32, u32, Vec<u32>)> =
            (info.large_lod..=last).map(|lod| tex::decode_level(c.tmu, b.start, &info, b.even_odd, lod, c.palette, c.ncc, 0)).collect();
        let (w, h) = (levels[0].0, levels[0].1);
        let td = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: levels.len() as u32,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let init: Vec<D3D11_SUBRESOURCE_DATA> = levels
            .iter()
            .map(|(lw, _, px)| D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as *const _, SysMemPitch: lw * 4, SysMemSlicePitch: 0 })
            .collect();
        let mut t = None;
        let mut srv = None;
        unsafe {
            self.device.CreateTexture2D(&td, Some(init.as_ptr()), Some(&mut t)).ok()?;
            self.device.CreateShaderResourceView(&t.unwrap(), None, Some(&mut srv)).ok()?;
        }
        let range = if w >= h { (256.0, 256.0 * h as f32 / w as f32) } else { (256.0 * w as f32 / h as f32, 256.0) };
        let end = b.start + tex::mem_required(info.small_lod, info.large_lod, info.aspect, info.format, b.even_odd);
        let srv = srv.unwrap();
        if self.textures.len() >= 8192 {
            // bounded: a texture is rebuilt from TMU memory when needed again
            self.textures.clear();
        }
        self.textures.insert(key, CachedTex { srv: srv.clone(), end, range });
        Some((srv, range, mipmaps))
    }

    fn upload_cb<T: Copy>(&self, buf: &ID3D11Buffer, v: &T) {
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if self.ctx.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_ok() {
                std::ptr::copy_nonoverlapping(v as *const T as *const u8, m.pData as *mut u8, std::mem::size_of::<T>());
                self.ctx.Unmap(buf, 0);
            }
        }
    }

    /// Read the scene back and save it as PNG.
    fn dump_scene(&self, path: &std::path::Path) -> Result<(), String> {
        let Some(s) = &self.scene else { return Ok(()) };
        unsafe {
            let res: ID3D11Resource = s.srv.GetResource().map_err(err)?;
            self.dump_texture(&res.cast().map_err(err)?, path)
        }
    }

    /// `SR_SHOT="dir,every"`: save every n-th presented image (window contents with the
    /// overlay) as PNG (diagnostics).
    fn dump_presented(&self) {
        // "dir,every[,first,last]": presents first..last only; "dir,@a+b+c": every present
        // of the game's frames a, b, c (interpolated frames included)
        type Spec = (std::path::PathBuf, u64, u64, u64, Vec<u64>);
        static SPEC: std::sync::OnceLock<Option<Spec>> = std::sync::OnceLock::new();
        let spec = SPEC.get_or_init(|| {
            let v = std::env::var("SR_SHOT").ok()?;
            let mut it = v.split(',');
            let d = it.next()?.to_string();
            let second = it.next().unwrap_or("");
            let list: Vec<u64> = second.strip_prefix('@').map(|l| l.split('+').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
            let every = second.parse().unwrap_or(60);
            let first = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let last = it.next().and_then(|n| n.parse().ok()).unwrap_or(u64::MAX);
            let _ = std::fs::create_dir_all(&d);
            Some((d.into(), every, first, last, list))
        });
        static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if let Some((dir, every, first, last, list)) = spec {
            let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let wanted = if list.is_empty() {
                n % every.max(&1) == 0 && n >= *first && n <= *last
            } else {
                list.contains(&sr_runtime::glide::FRAME.load(Ordering::Relaxed))
            };
            if wanted {
                if let Ok(t) = unsafe { self.swap.GetBuffer::<ID3D11Texture2D>(0) } {
                    let gf = sr_runtime::glide::FRAME.load(Ordering::Relaxed);
                    let _ = self.dump_texture(&t, &dir.join(format!("shot{n:06}_g{gf}.png")));
                }
            }
        }
    }

    fn dump_texture(&self, tex: &ID3D11Texture2D, path: &std::path::Path) -> Result<(), String> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            let sd = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut staging = None;
            self.device.CreateTexture2D(&sd, None, Some(&mut staging)).map_err(err)?;
            let staging = staging.unwrap();
            self.ctx.CopyResource(&staging, tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(err)?;
            let mut px = Vec::with_capacity((desc.Width * desc.Height) as usize);
            for y in 0..desc.Height {
                let row = (m.pData as *const u8).add((y * m.RowPitch) as usize) as *const u32;
                px.extend_from_slice(std::slice::from_raw_parts(row, desc.Width as usize));
            }
            self.ctx.Unmap(&staging, 0);
            std::fs::write(path, crate::png::encode(desc.Width, desc.Height, &px)).map_err(|e| e.to_string())
        }
    }

    /// Pick up settings changed in the overlay.
    fn apply_live(&mut self) {
        if let Some(l) = &self.settings.live {
            use std::sync::atomic::Ordering::Relaxed;
            self.settings.filter = match l.filter.load(Relaxed) {
                1 => Filter::Bilinear,
                2 => Filter::Game,
                _ => Filter::Nearest,
            };
            self.settings.vsync = l.vsync.load(Relaxed);
            self.settings.aspect = crate::display::Aspect::from_index(l.aspect.load(Relaxed) as usize);
            self.settings.render_scale = l.render_scale.load(Relaxed).clamp(crate::display::RENDER_SCALE_MIN, crate::display::RENDER_SCALE_MAX);
            let mode = crate::display::unpack_mode(l.exclusive_mode.load(Relaxed));
            if mode != self.settings.exclusive_mode {
                self.settings.exclusive_mode = mode;
                self.mode_changed = std::time::Instant::now();
            }
            self.settings.depth = l.depth.load(Relaxed);
            sr_runtime::depth::ACTIVE.store(self.settings.depth, Relaxed);
            self.settings.show_fps = l.show_fps.load(Relaxed);
        }
    }

    /// A frame starts drawing into the scene (any drawing call, or a swap with nothing
    /// drawn): the side clear left pending by the last swap (`sides_pending`) is done now.
    fn begin_drawing(&mut self) {
        if std::mem::take(&mut self.sides_pending) {
            self.clear_sides();
        }
    }

    /// Widescreen: clear the scene outside the game's 4:3 area for the next frame. Race
    /// views and the menus' 3D backgrounds cover it every frame; menus with a 640 x 480
    /// picture do not, and 3D objects reaching past the picture (the spinning car) would
    /// leave trails there. Done before the next frame's first drawing (`begin_drawing`),
    /// never between a present and a repeat of it (`hold`).
    fn clear_sides(&mut self) {
        let Some(s) = &self.scene else { return };
        if self.ext < 0.5 {
            return;
        }
        // the scene, and at a render scale the full-resolution picture too
        let targets = std::iter::once((&s.rtv, s.width, s.height)).chain(self.out.as_ref().map(|o| (&o.rtv, o.width, o.height)));
        for (rtv, w, h) in targets {
            let (left, right) = side_columns(w, self.game.0, self.ext);
            let (w, h) = (w as i32, h as i32);
            if left <= 0 {
                continue;
            }
            let rects = [RECT { left: 0, top: 0, right: left, bottom: h }, RECT { left: right, top: 0, right: w, bottom: h }];
            unsafe { self.ctx1.ClearView(rtv, &[0.0, 0.0, 0.0, 1.0], Some(&rects)) };
        }
    }

    /// Forget the depths of the previous view (a new frame, the mirror view).
    fn clear_view_depth(&mut self) {
        if let Some(s) = &self.scene {
            unsafe { self.ctx.ClearDepthStencilView(&s.dsv, (D3D11_CLEAR_DEPTH.0 | D3D11_CLEAR_STENCIL.0) as u32, 0.0, 0) };
        }
    }

    /// Depth-stencil states for the depth-buffered 3D polygons: nearer or equal passes (our
    /// depth is larger-is-nearer). The stencil holds the id of the model part that drew each
    /// pixel (reference value = the polygons' part id):
    /// * `Opaque`: write depth and the part id;
    /// * `BackFace`: test only (back faces of objects the game does not cull);
    /// * `OwnPart` / `OtherPart`: blended polygons over pixels of their own part / of any
    ///   other, test only (drawn with different depth windows, see `depth_raster`).
    fn forced_depth_state(&mut self, kind: ForcedDepth) -> ID3D11DepthStencilState {
        let key = (0xffff, kind as u32, 0);
        let mask = match kind {
            ForcedDepth::OverScenery | ForcedDepth::OverCars => CAR_BIT,
            ForcedDepth::SameCar | ForcedDepth::OtherObject => CAR_OBJECT_BITS,
            _ => 0xff,
        };
        if let Some(d) = self.depths.get(&key) {
            return d.clone();
        }
        let op = |func: D3D11_COMPARISON_FUNC, pass: D3D11_STENCIL_OP| D3D11_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D11_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D11_STENCIL_OP_KEEP,
            StencilPassOp: pass,
            StencilFunc: func,
        };
        let (write, stencil) = match kind {
            ForcedDepth::Opaque => (true, Some(op(D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_REPLACE))),
            ForcedDepth::BackFace => (false, None),
            ForcedDepth::OwnPart => (false, Some(op(D3D11_COMPARISON_EQUAL, D3D11_STENCIL_OP_KEEP))),
            ForcedDepth::OtherPart => (false, Some(op(D3D11_COMPARISON_NOT_EQUAL, D3D11_STENCIL_OP_KEEP))),
            // decals: the reference is 0 (pixels of scenery) or CAR_BIT (pixels of cars)
            ForcedDepth::OverScenery | ForcedDepth::OverCars => (false, Some(op(D3D11_COMPARISON_EQUAL, D3D11_STENCIL_OP_KEEP))),
            ForcedDepth::SameCar | ForcedDepth::OwnPartRest => (false, Some(op(D3D11_COMPARISON_EQUAL, D3D11_STENCIL_OP_KEEP))),
            ForcedDepth::OtherObject => (false, Some(op(D3D11_COMPARISON_NOT_EQUAL, D3D11_STENCIL_OP_KEEP))),
        };
        let desc = D3D11_DEPTH_STENCIL_DESC {
            DepthEnable: true.into(),
            DepthWriteMask: if write { D3D11_DEPTH_WRITE_MASK_ALL } else { D3D11_DEPTH_WRITE_MASK_ZERO },
            DepthFunc: if matches!(kind, ForcedDepth::OwnPartRest) { D3D11_COMPARISON_LESS } else { D3D11_COMPARISON_GREATER_EQUAL },
            StencilEnable: stencil.is_some().into(),
            StencilReadMask: mask,
            StencilWriteMask: 0xff,
            FrontFace: stencil.unwrap_or_default(),
            BackFace: stencil.unwrap_or_default(),
        };
        let mut d = None;
        unsafe { self.device.CreateDepthStencilState(&desc, Some(&mut d)).expect("depth state") };
        let d = d.unwrap();
        self.depths.insert(key, d.clone());
        d
    }

    /// Rasterizers for depth-buffered 3D polygons. The game snaps every vertex to its
    /// 640 x 400 grid, so surfaces on the same plane can differ in depth by up to about one
    /// game pixel's worth of depth gradient, and its models rely on drawing order for
    /// overlays: a car's window glass is painted over body parts of the same model that sit
    /// slightly in front of it; shadows and tyre marks lie on the road. Blended polygons
    /// (those overlays; they write no depth) therefore keep the game's order within a depth
    /// window: over their own part a wide one (a relative bias of 1.5-3 % plus a slope bias
    /// of a bit over one game pixel), over other parts a narrow one (0.4-0.8 % plus the same
    /// slope), so that e.g. the rear window does not show through the spoiler in front of
    /// it. Opaque polygons get a tiny constant bias for rounding only: a larger one would
    /// let parts just behind a steep surface (wheel arches inside a car's side) show
    /// through it.
    fn depth_raster(&mut self, blended: bool, own_part: bool) -> ID3D11RasterizerState {
        let (sx, sy) = self.scene_scale();
        let slope = if blended { 1.25 * sx.max(sy) } else { 0.0 };
        // float depth: the constant bias is DepthBias * 2^(exponent of the depth - 23)
        let bias = match (blended, own_part) {
            (false, _) => 1 << 6,
            (true, true) => 1 << 17,
            (true, false) => 1 << 15,
        };
        let key = slope.to_bits() ^ bias as u32;
        if let Some(r) = self.raster_depth.iter().find(|r| r.0 == key) {
            return r.1.clone();
        }
        let rd = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: false.into(),
            ScissorEnable: true.into(),
            DepthBias: bias,
            SlopeScaledDepthBias: slope,
            DepthBiasClamp: 0.0,
            ..Default::default()
        };
        let mut r = None;
        unsafe { self.device.CreateRasterizerState(&rd, Some(&mut r)).expect("rasterizer state") };
        let r = r.unwrap();
        if self.raster_depth.len() > 8 {
            self.raster_depth.clear();
        }
        self.raster_depth.push((key, r.clone()));
        r
    }

    /// Show the scene in the window (scaled, with the game's gamma), the overlay on top.
    fn present(&mut self) {
        self.apply_live();
        if let Err(e) = self.resize_if_needed() {
            sr_runtime::error!("display resize failed: {e}");
        }
        let Some(back) = self.back.clone() else { return };
        // at a render scale the full-resolution picture with the last 3D composed in
        self.resolve_3d();
        let Some(scene_srv) = self.out.as_ref().map(|o| o.srv.clone()).or_else(|| self.scene.as_ref().map(|s| s.srv.clone())) else { return };
        let (x, y, w, h) = self.present_rect();
        unsafe {
            self.ctx.OMSetRenderTargets(Some(&[Some(back.clone())]), None);
            self.ctx.ClearRenderTargetView(&back, &[0.0, 0.0, 0.0, 1.0]);
        }
        // CRT emulation: the shader draws the scene instead (`crt`)
        if !self.crt_draw(&scene_srv, &back, (x, y, w, h)) {
            unsafe {
                self.ctx.OMSetRenderTargets(Some(&[Some(back.clone())]), None);
                self.ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    TopLeftX: x as f32,
                    TopLeftY: y as f32,
                    Width: w as f32,
                    Height: h as f32,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                }]));
                let (bw, bh) = self.back_size;
                self.ctx.RSSetScissorRects(Some(&[RECT { left: 0, top: 0, right: bw as i32, bottom: bh as i32 }]));
            }
            self.draw_fullscreen(&scene_srv, 0.0, false);
        }
        if let Some(menu) = self.settings.menu.clone() {
            let m = menu.lock().unwrap();
            if m.open || m.toast_until.is_some_and(|t| std::time::Instant::now() < t) {
                if self.painter.is_none() {
                    self.painter = crate::overlay::Painter::new().map_err(|e| sr_runtime::warn!("overlay: {e}")).ok();
                }
                unsafe {
                    self.ctx.OMSetRenderTargets(None, None);
                    self.ctx.Flush();
                    if let (Some(p), Ok(surface)) = (&self.painter, self.swap.GetBuffer::<IDXGISurface>(0)) {
                        p.draw(&surface, self.back_size, &m);
                    }
                }
            }
        }
        if self.settings.show_fps {
            let now = std::time::Instant::now();
            let start = *self.fps_count.0.get_or_insert(now);
            self.fps_count.1 += 1;
            let dt = (now - start).as_secs_f32();
            if dt >= 0.5 {
                self.fps_count = (Some(now), 0, (self.fps_count.1 - 1) as f32 / dt);
            }
            if self.painter.is_none() {
                self.painter = crate::overlay::Painter::new().map_err(|e| sr_runtime::warn!("overlay: {e}")).ok();
            }
            unsafe {
                self.ctx.OMSetRenderTargets(None, None);
                self.ctx.Flush();
                if let (Some(p), Ok(surface)) = (&self.painter, self.swap.GetBuffer::<IDXGISurface>(0)) {
                    p.draw_fps(&surface, self.back_size, &format!("{:.0} fps", self.fps_count.2));
                }
            }
        } else {
            self.fps_count = (None, 0, 0.0);
        }
        self.dump_presented();
        if let Some(h) = self.latency_wait {
            if !std::mem::take(&mut self.ready_taken) {
                // VSync on: waits for the display like a blocking present would
                let t = std::time::Instant::now();
                unsafe { windows::Win32::System::Threading::WaitForSingleObjectEx(h, 1000, false) };
                if t.elapsed().as_millis() >= 250 {
                    sr_runtime::debug!("display: waited {:.0} ms for the display to take a frame", t.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        unsafe {
            // tearing is implicit in exclusive fullscreen (the flag is not allowed there)
            let (sync, flags) = if self.settings.vsync {
                (1, DXGI_PRESENT(0))
            } else if self.tearing && !self.exclusive {
                (0, DXGI_PRESENT_ALLOW_TEARING)
            } else {
                (0, DXGI_PRESENT(0))
            };
            let tp = std::time::Instant::now();
            let hr = self.swap.Present(sync, flags);
            if tp.elapsed().as_millis() >= 250 {
                sr_runtime::debug!("display: Present took {:.0} ms ({hr:?}, exclusive {})", tp.elapsed().as_secs_f64() * 1000.0, self.exclusive);
            }
            if std::env::var_os("SR_FLUSH").is_some() {
                self.ctx.Flush();
            }
            // SR_PRESENT_LOG=n (diagnostics): timing of n presents from the 2000th on
            {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                static LIMIT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
                static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
                let limit = *LIMIT.get_or_init(|| std::env::var("SR_PRESENT_LOG").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
                let n = N.fetch_add(1, Ordering::Relaxed);
                let mut last = LAST.lock().unwrap();
                if limit > 0 && n >= 2000 && n < 2000 + limit {
                    let since = last.map_or(0.0, |l| (tp - l).as_secs_f64() * 1000.0);
                    sr_runtime::info!("present {n}: {since:.2} ms after the previous, Present() took {:.2} ms, hr {hr:?}", tp.elapsed().as_secs_f64() * 1000.0);
                }
                *last = Some(tp);
            }
            if self.exclusive {
                self.last_present = Some(std::time::Instant::now());
            }
            if hr != windows::core::HRESULT(0) {
                // e.g. DXGI_STATUS_OCCLUDED: not shown (another window is in front, minimised)
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                if n < 5 || n % 500 == 0 {
                    let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() == self.hwnd;
                    let mode = if self.exclusive { "exclusive fullscreen" } else { "a window" };
                    sr_runtime::debug!("display: present in {mode} returned {hr:?} (foreground {fg}, {n} so far)");
                }
                // (a window that is leaving exclusive fullscreen anyway, deactivated, is left to
                // `follow_exclusive`)
                if self.exclusive && self.want_exclusive.load(Ordering::Relaxed) {
                    let mut full = windows::core::BOOL(0);
                    if self.swap.GetFullscreenState(Some(&mut full), None).is_ok() && !full.as_bool() {
                        // the display was taken from the swap chain without the window being
                        // deactivated first (another program took it): a borderless window
                        // again, exclusive taken again once the game is in the foreground
                        sr_runtime::info!("display: exclusive fullscreen lost ({hr:?}); taken again when the game is in the foreground");
                        self.exclusive = false;
                        self.back_size = (0, 0);
                    } else if hr == windows::Win32::Foundation::DXGI_STATUS_OCCLUDED {
                        // not shown, and the present did not wait for the display: wait a
                        // refresh here so that frames are not drawn as fast as possible
                        std::thread::sleep(self.refresh.min(std::time::Duration::from_millis(20)));
                    } else if hr == DXGI_ERROR_INVALID_CALL {
                        // (seen once as the display was taken away) the buffers are made
                        // again before the next present
                        self.back_size = (0, 0);
                    }
                }
            }
            {
                // presents per second and the longest gap between two presents, every 500
                // presents or 2 seconds (diagnostics; t: seconds since the first present)
                static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
                static MARK: std::sync::Mutex<Option<(std::time::Instant, u32, std::time::Instant, f64)>> = std::sync::Mutex::new(None);
                let now = std::time::Instant::now();
                let start = *START.get_or_init(|| now);
                let mut g = MARK.lock().unwrap();
                let (t, n, last, worst) = g.get_or_insert((now, 0, now, 0.0));
                *n += 1;
                *worst = worst.max((now - *last).as_secs_f64());
                *last = now;
                if *n == 500 || (now - *t).as_secs_f64() >= 2.0 {
                    let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() == self.hwnd;
                    sr_runtime::debug!(
                        "presents: {:.1} per second, longest gap {:.1} ms (vsync {}, foreground {fg}, exclusive {}, t {:.1} s)",
                        *n as f64 / (now - *t).as_secs_f64().max(1e-6),
                        *worst * 1000.0,
                        self.settings.vsync,
                        self.exclusive,
                        (now - start).as_secs_f64()
                    );
                    *g = Some((now, 0, now, 0.0));
                }
            }
            // unbind the scene texture before it becomes a render target again
            self.ctx.PSSetShaderResources(0, Some(&[None]));
        }
        self.bind_scene();
    }

    /// The frame buffer image of `lfb_write` in a texture of the game's screen size (only
    /// its rectangle updated; `slot`: a kept HUD image, uploaded once per generation).
    fn lfb_image(&mut self, rect: (u32, u32, u32, u32), pixels: &[u16], mask: &[bool], slot: Option<(u32, u64)>) -> Option<ID3D11ShaderResourceView> {
        // textures of the game's screen size; each call updates only its rectangle
        let (w, h) = self.game;
        let create = |device: &ID3D11Device| -> Option<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
            let td = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            let mut t = None;
            let mut srv = None;
            unsafe {
                device.CreateTexture2D(&td, None, Some(&mut t)).ok()?;
                let t = t?;
                device.CreateShaderResourceView(&t, None, Some(&mut srv)).ok()?;
                Some((t, srv?))
            }
        };
        let (tex, srv, upload) = match slot {
            None => {
                if self.lfb.as_ref().is_none_or(|l| l.2 != w || l.3 != h) {
                    let Some((t, v)) = create(&self.device) else { return None };
                    self.lfb = Some((t, v, w, h));
                }
                let (t, v, _, _) = self.lfb.clone().unwrap();
                (t, v, true)
            }
            Some((n, generation)) => {
                // a kept image: uploaded once per generation, drawn again from the texture
                let n = n as usize;
                if self.lfb_slots.len() <= n {
                    self.lfb_slots.resize(n + 1, None);
                }
                if self.lfb_slots[n].as_ref().is_none_or(|l| l.2 != w || l.3 != h) {
                    let Some((t, v)) = create(&self.device) else { return None };
                    self.lfb_slots[n] = Some((t, v, w, h, u64::MAX));
                }
                let l = self.lfb_slots[n].as_mut().unwrap();
                let upload = l.4 != generation;
                l.4 = generation;
                (l.0.clone(), l.1.clone(), upload)
            }
        };
        if upload {
            let rgba: Vec<u32> = pixels
                .iter()
                .zip(mask)
                .map(|(&p, &m)| {
                    let p = p as u32;
                    let r = ((p >> 11) << 3) | (p >> 13);
                    let g = (((p >> 5) & 63) << 2) | ((p >> 9) & 3);
                    let b = ((p & 31) << 3) | ((p >> 2) & 7);
                    if m { r | g << 8 | b << 16 | 0xff00_0000 } else { 0 }
                })
                .collect();
            // the rectangle with a transparent border of one texel: the texture keeps older
            // images outside the rectangle, and a part drawn at slightly under one texel per
            // pixel (CRT emulation's 853-pixel input of a 16:9 picture 853.33 game pixels
            // wide) samples the texel just beyond its edge in its outermost pixels: that
            // showed a line of an old image (white beside the lap, orange beside the
            // position) where the border is now
            let (rx, ry, rw, rh) = rect;
            let (bx0, by0) = (rx.saturating_sub(1), ry.saturating_sub(1));
            let (bx1, by1) = ((rx + rw + 1).min(w), (ry + rh + 1).min(h));
            if bx1 > bx0 && by1 > by0 {
                let bw = (bx1 - bx0) as usize;
                let mut framed = vec![0u32; bw * (by1 - by0) as usize];
                let cols = ((rx + rw).min(w) - rx) as usize;
                for y in 0..rh.min(h.saturating_sub(ry)) as usize {
                    let dst = (y + (ry - by0) as usize) * bw + (rx - bx0) as usize;
                    framed[dst..dst + cols].copy_from_slice(&rgba[y * rw as usize..][..cols]);
                }
                unsafe {
                    let bx = D3D11_BOX { left: bx0, top: by0, front: 0, right: bx1, bottom: by1, back: 1 };
                    self.ctx.UpdateSubresource(&tex, 0, Some(&bx), framed.as_ptr() as *const _, (bw * 4) as u32, 0);
                }
            }
        }
        Some(srv)
    }

    /// One of the game's pictures (`Sprite2D`) into `rtv` (a target of `size` pixels showing
    /// the widened view): its box mapped as the frame buffer image maps the screen, with its
    /// element's anchor, cut to its clip rectangle.
    fn draw_sprite(&mut self, sp: &Sprite2D, rtv: &ID3D11RenderTargetView, (t_width, t_height): (u32, u32), original: bool) {
        let Some(srv) = self.picture_texture(&sp.pic) else { return };
        let (gw, gh) = (self.game.0 as f32, self.game.1 as f32);
        let sx = t_width as f32 / (gw + 2.0 * self.ext);
        let sy = t_height as f32 / gh;
        let (left, width) = if sp.anchor == 2 { (0.0, t_width as f32) } else { ((self.ext + sp.anchor as f32 * self.ext) * sx, gw * sx) };
        let kx = width / gw;
        let [bx, by, bw, bh] = sp.dst;
        let (pw, ph) = (sp.pic.w.max(1) as f32, sp.pic.h.max(1) as f32);
        // the texture's transparent border: one texel more on each side
        let (ex, ey) = (bw / pw, bh / ph);
        let [cx0, cy0, cx1, cy1] = sp.clip;
        let scissor = RECT {
            left: ((left + cx0 as f32 * kx).floor() as i32).max(0),
            top: ((cy0 as f32 * sy).floor() as i32).max(0),
            right: ((left + cx1 as f32 * kx).ceil() as i32).min(t_width as i32),
            bottom: ((cy1 as f32 * sy).ceil() as i32).min(t_height as i32),
        };
        if scissor.right <= scissor.left || scissor.bottom <= scissor.top {
            return;
        }
        unsafe {
            self.ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            self.ctx.RSSetScissorRects(Some(&[scissor]));
            self.ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: left + (bx - ex) * kx,
                TopLeftY: (by - ey) * sy,
                Width: (bw + 2.0 * ex) * kx,
                Height: (bh + 2.0 * ey) * sy,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
        }
        self.draw_picture(&srv, (pw + 2.0, ph + 2.0), (bw * kx / pw, bh * sy / ph), !original);
    }

    /// A picture's texture (made once, with a transparent border of one texel).
    fn picture_texture(&mut self, pic: &Picture) -> Option<ID3D11ShaderResourceView> {
        if let Some(t) = self.pictures.get_mut(&pic.key) {
            t.1 = self.frames;
            return Some(t.0.clone());
        }
        let (pw, ph) = (pic.w + 2, pic.h + 2);
        let mut px = vec![0u32; (pw * ph) as usize];
        for y in 0..pic.h as usize {
            px[(y + 1) * pw as usize + 1..][..pic.w as usize].copy_from_slice(&pic.rgba[y * pic.w as usize..][..pic.w as usize]);
        }
        let td = D3D11_TEXTURE2D_DESC {
            Width: pw,
            Height: ph,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as *const _, SysMemPitch: pw * 4, SysMemSlicePitch: 0 };
        let mut t = None;
        let mut srv = None;
        unsafe {
            self.device.CreateTexture2D(&td, Some(&init), Some(&mut t)).ok()?;
            self.device.CreateShaderResourceView(&t?, None, Some(&mut srv)).ok()?;
        }
        let srv = srv?;
        // bounded: pictures not drawn for a while (another screen's) go
        if self.pictures.len() >= 1024 {
            let now = self.frames;
            self.pictures.retain(|_, t| now.saturating_sub(t.1) < 300);
        }
        self.pictures.insert(pic.key, (srv.clone(), self.frames));
        Some(srv)
    }

    /// A premultiplied picture of `size` texels over the viewport (`ps_picture`), `scale`
    /// target pixels per texel, blended over the target: `sharp` (texel blocks of one size,
    /// their edges smoothed) or nearest.
    fn draw_picture(&mut self, srv: &ID3D11ShaderResourceView, size: (f32, f32), scale: (f32, f32), sharp: bool) {
        self.bound = None;
        let cb = [size.0, size.1, scale.0, scale.1, sharp as u32 as f32, 0.0, 0.0, 0.0];
        self.upload_cb(&self.cb_picture, &cb);
        let st = GlideState { clamp: [GR_TEXTURECLAMP_CLAMP; 2], ..GlideState::default() };
        let saved = self.settings.filter;
        self.settings.filter = Filter::Bilinear;
        let samp = self.sampler(&st, false);
        self.settings.filter = saved;
        unsafe {
            self.ctx.GSSetShader(None, None);
            self.ctx.IASetInputLayout(None);
            self.ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.VSSetShader(&self.vs_blit, None);
            self.ctx.PSSetShader(&self.ps_picture, None);
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb_picture.clone())]));
            self.ctx.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
            self.ctx.PSSetSamplers(0, Some(&[Some(samp)]));
            self.ctx.OMSetBlendState(&self.blend_premul, None, 0xffff_ffff);
            self.ctx.OMSetDepthStencilState(None, 0);
            self.ctx.RSSetState(&self.raster);
            self.ctx.Draw(3, 0);
        }
    }

    fn draw_fullscreen(&mut self, srv: &ID3D11ShaderResourceView, mode: f32, linear: bool) {
        self.bound = None;
        let cb = [1.0 / self.gamma.max(0.1), mode, 0.0, 0.0];
        self.upload_cb(&self.cb_blit, &cb);
        let st = GlideState { clamp: [GR_TEXTURECLAMP_CLAMP; 2], ..GlideState::default() };
        let saved = self.settings.filter;
        self.settings.filter = if linear { Filter::Bilinear } else { Filter::Nearest };
        let samp = self.sampler(&st, false);
        self.settings.filter = saved;
        unsafe {
            self.ctx.GSSetShader(None, None);
            self.ctx.IASetInputLayout(None);
            self.ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.VSSetShader(&self.vs_blit, None);
            self.ctx.PSSetShader(&self.ps_blit, None);
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb_blit.clone())]));
            self.ctx.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
            self.ctx.PSSetSamplers(0, Some(&[Some(samp)]));
            self.ctx.OMSetBlendState(None, None, 0xffff_ffff);
            self.ctx.OMSetDepthStencilState(None, 0);
            self.ctx.RSSetState(&self.raster);
            self.ctx.Draw(3, 0);
        }
    }

    // ---- native renderer: the integration point (see crate::native) ----

    /// Upload a scene for the native renderer (once per track or menu scene; replaces the
    /// previous one). The renderer is created on first use.
    pub fn native_upload(&mut self, scene: &crate::native::SceneData) -> Result<crate::native::SceneStats, String> {
        if self.native.is_none() {
            self.native = Some(crate::native::NativeRenderer::new(&self.device, &self.ctx)?);
        }
        let r = self.native.as_mut().unwrap().upload(scene);
        self.bound = None;
        r
    }

    /// Set up a frame for the native renderer (transforms, instances, per-frame geometry):
    /// once per frame, before its views.
    pub fn native_begin_frame(&mut self, frame: &crate::native::FrameData) -> Result<(), String> {
        let Some(n) = self.native.as_mut() else { return Ok(()) };
        let r = n.begin_frame(frame);
        self.bound = None;
        r
    }

    /// Draw view `view` of the frame into the scene render target, now. Call it where the
    /// game would draw its world for that view: after the frame's clear and `set_view`
    /// (`Backend::begin_view`), before the 2D and HUD. What the Glide path draws afterwards
    /// lands on top; its depth-tested 3D polygons are hidden where the native world is
    /// nearer (shared depth buffer, `native::DEPTH_SCALE`). The Glide pipeline state is
    /// restored (rebound by the next Glide draw).
    pub fn native_draw_view(&mut self, frame: &crate::native::FrameData, view: usize) -> Result<crate::native::DrawStats, String> {
        if self.native.is_some() && self.scene.is_some() {
            self.begin_drawing();
        }
        let (Some(n), Some(s)) = (self.native.as_mut(), self.scene.as_ref()) else { return Ok(Default::default()) };
        // at a render scale the view marks the coverage target as it draws (3D layer)
        let cov = s.cov.as_ref().map(|c| &c.0);
        self.pending_3d |= cov.is_some();
        let target = crate::native::Target { rtv: &s.rtv, dsv: &s.dsv, size: (s.width, s.height), game: self.game, ext: self.ext, cov };
        let r = n.draw_view(frame, view, &target, self.settings.filter);
        n.unbind();
        let stats = n.last;
        self.bound = None;
        self.bind_scene();
        r.map(|_| stats)
    }

    /// `native_begin_frame`, then every view of `frame` in order.
    pub fn native_draw_frame(&mut self, frame: &crate::native::FrameData) -> Result<crate::native::DrawStats, String> {
        self.native_begin_frame(frame)?;
        let mut total = crate::native::DrawStats::default();
        for v in 0..frame.views.len() {
            let s = self.native_draw_view(frame, v)?;
            total.draws += s.draws;
            total.triangles += s.triangles;
            total.chunks_visible += s.chunks_visible;
            total.instances_visible += s.instances_visible;
        }
        Ok(total)
    }

    /// `SR_D3D_DEBUG`: log the debug layer's messages (the first 200; then a count every
    /// 1000 frames).
    fn drain_debug_messages(&mut self) {
        let Some(q) = &self.info_queue else { return };
        unsafe {
            let n = q.GetNumStoredMessages();
            for i in 0..n {
                let mut len = 0usize;
                if q.GetMessage(i, None, &mut len).is_err() || len == 0 {
                    continue;
                }
                let mut buf = vec![0u64; len.div_ceil(8)];
                let msg = buf.as_mut_ptr() as *mut D3D11_MESSAGE;
                if q.GetMessage(i, Some(msg), &mut len).is_ok() {
                    self.debug_messages += 1;
                    if self.debug_messages <= 200 {
                        let m = &*msg;
                        let text = String::from_utf8_lossy(std::slice::from_raw_parts(m.pDescription, m.DescriptionByteLength.saturating_sub(1)));
                        sr_runtime::warn!("d3d debug (severity {}, id {}): {text}", m.Severity.0, m.ID.0);
                    }
                }
            }
            q.ClearStoredMessages();
        }
        if self.frames % 1000 == 0 {
            sr_runtime::info!("d3d debug: {} messages so far (frame {})", self.debug_messages, self.frames);
        }
    }

    /// `SR_NATIVE_TEST`: draw the synthetic scene now (once per frame), timed.
    fn native_test_draw(&mut self) {
        let Some(mut t) = self.native_test.take() else { return };
        t.drawn = true;
        if !t.uploaded {
            t.uploaded = true;
            match self.native_upload(&t.scene.scene) {
                Ok(s) => sr_runtime::info!(
                    "native test: scene uploaded in {:.1} ms: {} textures in {} arrays ({:.1} MB RGBA8), {} materials in {} batches, {} chunks, {} static + {} mesh triangles, {} vertices",
                    s.upload_ms,
                    s.textures,
                    s.texture_arrays,
                    s.texture_bytes as f64 / 1048576.0,
                    s.materials,
                    s.batches,
                    s.chunks,
                    s.static_triangles,
                    s.mesh_triangles,
                    s.vertices
                ),
                Err(e) => {
                    sr_runtime::error!("native test: upload failed: {e}; test off");
                    return;
                }
            }
            t.reset_interval();
        }
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        let b0 = std::time::Instant::now();
        t.scene.frame(t.start.elapsed().as_secs_f64(), &mut t.frame);
        t.build.add(ms(b0));
        let c0 = std::time::Instant::now();
        if let Some(n) = self.native.as_mut() {
            n.timer.begin(&self.ctx);
        }
        let r = self.native_draw_frame(&t.frame);
        if let Some(n) = self.native.as_mut() {
            n.timer.end(&self.ctx);
        }
        t.cpu.add(ms(c0));
        // Submit now: the GPU draws the world while the game finishes its frame. (Without
        // this the driver holds the commands until the present, and the measured GPU time
        // includes waiting for them: 0.6-2.9 ms instead of 0.1-0.35 ms at 1080p.)
        unsafe { self.ctx.Flush() };
        if let Some(n) = self.native.as_mut() {
            for g in n.timer.poll(&self.ctx) {
                t.gpu.add(g);
            }
        }
        match r {
            Ok(s) => {
                t.draws.add(s.draws as f64);
                t.tris.add(s.triangles as f64);
                if t.total == 0 {
                    sr_runtime::info!("native test: first frame: {} draws, {} triangles, {} chunks and {} instances visible (all views)", s.draws, s.triangles, s.chunks_visible, s.instances_visible);
                }
            }
            Err(e) => {
                if t.total < 3 {
                    sr_runtime::warn!("native test: drawing failed: {e}");
                }
            }
        }
        t.total += 1;
        self.native_test = Some(t);
    }

    /// `SR_NATIVE_TEST`: after a present: next frame, interval report, the benchmark.
    fn native_test_end_frame(&mut self) {
        let size = self.scene.as_ref().map_or((0, 0), |s| (s.width, s.height));
        let Some(t) = self.native_test.as_mut() else { return };
        let drew = std::mem::take(&mut t.drawn);
        t.seen_3d = false;
        if !drew {
            return;
        }
        t.presents += 1;
        if t.report_start.is_some_and(|s| s.elapsed().as_secs_f64() >= 5.0) {
            t.report(size, "");
        }
        if t.total >= 300 {
            if let Some(n) = t.bench.take() {
                self.native_bench(n);
            }
        }
    }

    /// `SR_NATIVE_BENCH=n`: n frames of the test scene alone, back to back (the game waits),
    /// each drawn and presented: the native renderer's own frame rate at this size.
    fn native_bench(&mut self, n: u32) {
        let size = self.scene.as_ref().map_or((0, 0), |s| (s.width, s.height));
        sr_runtime::info!("native bench: {n} frames of the test scene alone at {}x{} (vsync {})", size.0, size.1, self.settings.vsync);
        if let Some(t) = self.native_test.as_mut() {
            t.report(size, "");
            t.reset_interval();
        }
        let start = std::time::Instant::now();
        for _ in 0..n {
            self.native_test_draw();
            self.present();
            if let Some(t) = self.native_test.as_mut() {
                t.drawn = false;
                t.presents += 1;
            }
        }
        let secs = start.elapsed().as_secs_f64();
        sr_runtime::info!("native bench: {n} frames in {secs:.3} s = {:.1} fps ({:.3} ms per frame, everything included)", n as f64 / secs, secs * 1000.0 / n as f64);
        if let Some(t) = self.native_test.as_mut() {
            t.report(size, " bench");
        }
    }
}

impl Backend for D3dBackend {
    fn open(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.game = (width, height);
        self.resize_if_needed()?;
        self.bind_scene();
        Ok(())
    }

    fn triangles(&mut self, c: &DrawContext, verts: &[Vertex]) {
        if self.scene.is_none() || verts.is_empty() {
            return;
        }
        self.begin_drawing();
        // SR_NATIVE_TEST=1: the game's 3D polygons (the vertex builder's, not marked 2D) are
        // replaced by the test scene, drawn before the first 2D drawing after them
        if let Some((seen_3d, drawn)) = self.native_test.as_ref().filter(|t| t.mode == crate::native::TestMode::Replace).map(|t| (t.seen_3d, t.drawn)) {
            if verts[0].ooz >= -1.5 {
                self.native_test.as_mut().unwrap().seen_3d = true;
                return;
            }
            if seen_3d && !drawn {
                self.native_test_draw();
            }
        }
        let st = c.st;
        self.gamma = f32::from_bits(st.gamma_bits);
        // Render scale: 2D (marked by the Glide layer) goes over the 3D drawn so far, which
        // is composed into the full-resolution picture first; 3D marks the coverage target.
        let scaled = self.out.is_some();
        if scaled {
            if verts[0].ooz < -1.5 {
                self.resolve_3d();
                self.bind_scene_cov(false);
            } else {
                self.bind_scene_cov(true);
                self.pending_3d = true;
            }
        }
        let tex = self.texture(c);
        let uses_tex = |other: u32, factor: u32| other == GR_COMBINE_OTHER_TEXTURE || matches!(factor, 4 | 5 | 0xc | 0xd);
        let textured = tex.is_some() && (uses_tex(st.color_combine[3], st.color_combine[1]) || uses_tex(st.alpha_combine[3], st.alpha_combine[1]));
        let argb = |v: u32| [((v >> 16) & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0, (v >> 24) as f32 / 255.0];
        let mut cb = StateCb {
            cc: [st.color_combine[0], st.color_combine[1], st.color_combine[2], st.color_combine[3]],
            ac: [st.alpha_combine[0], st.alpha_combine[1], st.alpha_combine[2], st.alpha_combine[3]],
            tc: [st.tex_combine[0], st.tex_combine[1], st.tex_combine[2], st.tex_combine[3]],
            flags: [
                (st.color_combine[4] != 0) as u32
                    | ((st.alpha_combine[4] != 0) as u32) << 1
                    | ((st.tex_combine[4] != 0) as u32) << 2
                    | ((st.tex_combine[5] != 0) as u32) << 3,
                textured as u32,
                st.alpha_test & 7,
                st.alpha_ref as u32,
            ],
            constant: argb(st.constant_color),
            fog_color: {
                let f = argb(st.fog_color);
                let mode = st.fog_mode & 0xff;
                [f[0], f[1], f[2], if mode == GR_FOG_WITH_TABLE { 2.0 } else if mode == GR_FOG_WITH_ITERATED_ALPHA { 1.0 } else { 0.0 }]
            },
            chroma: {
                let v = st.chromakey_value;
                [((v >> 16) & 0xff) as f32, ((v >> 8) & 0xff) as f32, (v & 0xff) as f32, st.chromakey as f32]
            },
            tex_range: {
                let r = tex.as_ref().map(|t| t.1).unwrap_or((256.0, 256.0));
                // w-buffer depth = oow * DEPTH_SCALE: the native renderer's depth for the
                // same view depth (hybrid frames share the depth buffer)
                [r.0, r.1, crate::native::DEPTH_SCALE, 0.0]
            },
            screen: [2.0 / (self.game.0 as f32 + 2.0 * self.ext), -2.0 / self.game.1 as f32, (st.depth_mode == GR_DEPTHBUFFER_WBUFFER || st.depth_mode == 4) as u32 as f32, self.ext],
            fog_table: [[0.0; 4]; 16],
            grow: [0.0; 4],
        };
        for i in 0..64 {
            cb.fog_table[i / 4][i % 4] = st.fog_table[i] as f32 / 255.0;
        }
        let blend = self.blend_state(st);
        // Depth buffer for the game's 3D polygons (the game itself draws without one, in
        // sorted order): those carry a perspective 1/w well below 1; the sky and ground
        // background quads (1/w = 1) and 2D shapes (0) are drawn as the game orders them.
        let oow = verts[0].oow;
        let flat_2d = verts[0].ooz < -1.5; // marked 2D by the Glide layer
        let forced = self.settings.depth && st.depth_mode == GR_DEPTHBUFFER_DISABLE && !flat_2d && oow > 0.0 && oow < 0.5;
        let blended = !(st.blend[0] == GR_BLEND_ONE && st.blend[1] == GR_BLEND_ZERO);
        // back faces of objects the game does not cull: tested, no depth written (marked
        // by the Glide layer with a negative 1/z, which the game never uses)
        let back = verts[0].ooz < 0.0 && !flat_2d;
        if forced {
            let k = c.kind as usize;
            self.kind_stats[k][(blended as usize) * 2 + back as usize] += n_tris(verts);
        }
        // (depth-stencil state, rasterizer) per draw: blended 3D polygons are drawn twice,
        // over their own part's pixels and over the rest (disjoint by the stencil test)
        // (depth-stencil state, rasterizer, stencil reference, depth pull)
        let mut passes: Vec<(ID3D11DepthStencilState, ID3D11RasterizerState, u32, f32)> = Vec::with_capacity(3);
        let decal = matches!(c.kind, Kind::Shadow | Kind::Smoke | Kind::Marks);
        let car_part = c.part & CAR_BIT != 0;
        if forced && decal {
            // Shadows, smoke and tyre marks (drawn opaque with a chroma key, or blended): over
            // scenery as if `pull` units nearer, so the ground under them does not cut into
            // them; over cars as they are, so they stay under the cars. No depth written.
            cb.screen[2] = 1.0;
            let pull = match c.kind {
                Kind::Smoke => 500.0,
                Kind::Shadow => 250.0,
                _ => 120.0,
            };
            passes.push((self.forced_depth_state(ForcedDepth::OverScenery), self.depth_raster(false, true), 0, pull));
            passes.push((self.forced_depth_state(ForcedDepth::OverCars), self.depth_raster(false, true), CAR_BIT as u32, 0.0));
        } else if forced {
            cb.screen[2] = 1.0;
            // opaque 3D polygons of the scenery: crack fill (gs_grow), 2 target pixels (not
            // cars: their models are closed, and a grown edge would paint over the car's
            // own glass overlays, which write no depth)
            if !blended && !back && !car_part && self.settings.grow_cracks {
                if let Some(s) = &self.scene {
                    cb.grow = [2.0, s.width as f32, s.height as f32, 0.0];
                }
            }
            let r = c.part as u32;
            if blended && car_part && !back {
                // A car's glass and overlays (three disjoint passes): over the same car with
                // the narrow window (its overlays lie on its other parts, up to about 0.5 %
                // behind where the game splits a quad differently; the rear window must not
                // paint over the spoiler in front of it); over its own part's remaining
                // pixels in the game's order (glass just behind the window's inner layer);
                // over walls and other cars rounding only.
                let narrow = self.depth_raster(true, false);
                passes.push((self.forced_depth_state(ForcedDepth::SameCar), narrow.clone(), r, 0.0));
                passes.push((self.forced_depth_state(ForcedDepth::OwnPartRest), narrow, r, 0.0));
                passes.push((self.forced_depth_state(ForcedDepth::OtherObject), self.depth_raster(false, true), r, 0.0));
            } else if blended && c.part != 0 {
                passes.push((self.forced_depth_state(ForcedDepth::OwnPart), self.depth_raster(true, true), r, 0.0));
                passes.push((self.forced_depth_state(ForcedDepth::OtherPart), self.depth_raster(true, false), r, 0.0));
            } else if blended || back {
                passes.push((self.forced_depth_state(ForcedDepth::BackFace), self.depth_raster(blended, true), r, 0.0));
            } else {
                passes.push((self.forced_depth_state(ForcedDepth::Opaque), self.depth_raster(false, true), r, 0.0));
            }
        } else {
            passes.push((self.depth_state(st), self.raster.clone(), 0, 0.0));
        }
        // the game sets the same state for many polygons in a row
        cb.grow[3] = passes[0].3;
        if self.cb_last.as_ref().is_none_or(|l| bytes_of(l) != bytes_of(&cb)) {
            self.upload_cb(&self.cb, &cb);
            self.cb_last = Some(cb);
        }
        let samp = self.sampler(st, tex.as_ref().is_some_and(|t| t.2));
        let scissor = self.scale_rect(st);

        // vertex upload: append, wrap with DISCARD when full
        let n = verts.len() as u32;
        if n > VB_VERTICES {
            return;
        }
        let (map, start) = if self.vb_pos + n > VB_VERTICES { (D3D11_MAP_WRITE_DISCARD, 0) } else { (D3D11_MAP_WRITE_NO_OVERWRITE, self.vb_pos) };
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if self.ctx.Map(&self.vb, 0, map, 0, Some(&mut m)).is_err() {
                return;
            }
            let dst = (m.pData as *mut Vertex).add(start as usize);
            std::ptr::copy_nonoverlapping(verts.as_ptr(), dst, verts.len());
            self.ctx.Unmap(&self.vb, 0);
            self.vb_pos = start + n;

            // the pipeline (shaders, buffers, layout) once after anything else drew
            let mut b = match self.bound {
                Some(b) => b,
                None => {
                    let stride = std::mem::size_of::<Vertex>() as u32;
                    let offset = 0u32;
                    let vb = Some(self.vb.clone());
                    self.ctx.IASetVertexBuffers(0, 1, Some(&vb), Some(&stride), Some(&offset));
                    self.ctx.IASetInputLayout(&self.layout);
                    self.ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                    self.ctx.VSSetShader(&self.vs, None);
                    self.ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
                    self.ctx.GSSetShader(&self.gs, None);
                    self.ctx.GSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
                    self.ctx.PSSetShader(if self.cov_bound { &self.ps_cov } else { &self.ps }, None);
                    self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
                    Bound { scissor: (i32::MIN, 0, 0, 0), ..Default::default() }
                }
            };
            let srv_raw = tex.as_ref().map_or(0, |t| t.0.as_raw() as usize);
            if b.srv != srv_raw || srv_raw == 0 {
                self.ctx.PSSetShaderResources(0, Some(&[tex.map(|t| t.0)]));
                b.srv = srv_raw;
            }
            if b.samp != samp.as_raw() as usize {
                self.ctx.PSSetSamplers(0, Some(&[Some(samp.clone())]));
                b.samp = samp.as_raw() as usize;
            }
            if b.blend != blend.as_raw() as usize {
                self.ctx.OMSetBlendState(&blend, None, 0xffff_ffff);
                b.blend = blend.as_raw() as usize;
            }
            let sc = (scissor.left, scissor.top, scissor.right, scissor.bottom);
            if b.scissor != sc {
                self.ctx.RSSetScissorRects(Some(&[scissor]));
                b.scissor = sc;
            }
            for (i, (depth, raster, sref, pull)) in passes.iter().enumerate() {
                if i > 0 && cb.grow[3] != *pull {
                    cb.grow[3] = *pull;
                    self.upload_cb(&self.cb, &cb);
                    self.cb_last = Some(cb);
                }
                if b.depth != depth.as_raw() as usize || b.stencil_ref != *sref {
                    self.ctx.OMSetDepthStencilState(depth, *sref);
                    b.depth = depth.as_raw() as usize;
                    b.stencil_ref = *sref;
                }
                if b.raster != raster.as_raw() as usize {
                    self.ctx.RSSetState(raster);
                    b.raster = raster.as_raw() as usize;
                }
                self.ctx.Draw(n, start);
            }
            self.bound = Some(b);
        }
        if scaled && flat_2d {
            self.redraw_2d_in_out(st, n, start);
        }
    }

    fn clear(&mut self, c: &DrawContext, color: u32, alpha: u8, depth: u16) {
        if self.scene.is_none() {
            return;
        }
        self.begin_drawing();
        let Some(s) = &self.scene else { return };
        let rect = self.scale_rect(c.st);
        let col = [((color >> 16) & 0xff) as f32 / 255.0, ((color >> 8) & 0xff) as f32 / 255.0, (color & 0xff) as f32 / 255.0, alpha as f32 / 255.0];
        let wbuf = c.st.depth_mode == GR_DEPTHBUFFER_WBUFFER || c.st.depth_mode == 4;
        let z = if wbuf { 1.0 - depth as f32 / 65535.0 } else { depth as f32 / 65535.0 };
        // SR_NATIVE_TEST=1: keep the test scene drawn this frame
        let keep = self.native_test.as_ref().is_some_and(|t| t.mode == crate::native::TestMode::Replace && t.drawn);
        // render scale: the full-resolution picture too (the scene keeps its coverage: the
        // scene's pixels there are the cleared colour as well)
        if let (Some(o), true) = (&self.out, c.st.color_mask != 0 && !keep) {
            let r = self.rect_in(c.st, (o.width, o.height));
            unsafe { self.ctx1.ClearView(&o.rtv, &col, Some(&[r])) };
        }
        unsafe {
            if c.st.color_mask != 0 && !keep {
                self.ctx1.ClearView(&s.rtv, &col, Some(&[rect]));
            }
            if c.st.depth_mask != 0 || c.st.depth_mode != GR_DEPTHBUFFER_DISABLE {
                self.ctx.ClearDepthStencilView(&s.dsv, D3D11_CLEAR_DEPTH.0 as u32, z.clamp(0.0, 1.0), 0);
            }
        }
    }

    fn lfb_write(&mut self, rect: (u32, u32, u32, u32), pixels: &[u16], mask: &[bool], parts: &[LfbPart], slot: Option<(u32, u64)>, sprites: &[Sprite2D]) {
        if self.scene.is_none() {
            return;
        }
        self.begin_drawing();
        if self.native_test.as_ref().is_some_and(|t| t.mode == crate::native::TestMode::Replace && t.seen_3d && !t.drawn) {
            self.native_test_draw();
        }
        let (w, h) = self.game;
        // the frame buffer image (none when every pixel written was a picture's)
        let srv = if pixels.is_empty() { None } else { self.lfb_image(rect, pixels, mask, slot) };
        // SR_UI_SCALING=original (diagnostics): the image scaled as before, nearest
        let original = sr_runtime::ui2d::original();
        // the scene; at a render scale the 3D drawn so far goes into the full-resolution
        // picture first, and the image is drawn there too (crisp)
        self.resolve_3d();
        let s = self.scene.as_ref().unwrap();
        let mut targets = vec![(s.rtv.clone(), s.width, s.height)];
        targets.extend(self.out.as_ref().map(|o| (o.rtv.clone(), o.width, o.height)));
        for (rtv, t_width, t_height) in targets {
            // the game's pictures first, from their own pixels (`sr_runtime::ui2d`)
            for sp in sprites {
                self.draw_sprite(sp, &rtv, (t_width, t_height), original);
            }
            let Some(srv) = &srv else { continue };
            let sx = t_width as f32 / (self.game.0 as f32 + 2.0 * self.ext);
            let sy = t_height as f32 / h.max(1) as f32;
            for p in parts {
                // the 2D picture covers the game's 4:3 area in the middle of the view; HUD
                // elements anchored to a side move by the widescreen extension, full-width
                // bars are stretched across it
                let (left, width) = if p.anchor == 2 {
                    (0.0, t_width as f32)
                } else {
                    ((self.ext + p.anchor as f32 * self.ext) * sx, self.game.0 as f32 * sx)
                };
                let kx = width / w.max(1) as f32;
                unsafe {
                    self.ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
                    self.ctx.RSSetScissorRects(Some(&[RECT {
                        left: (left + p.x0 as f32 * kx).floor() as i32,
                        top: (p.y0 as f32 * sy).floor() as i32,
                        right: (left + p.x1 as f32 * kx).ceil() as i32,
                        bottom: (p.y1 as f32 * sy).ceil() as i32,
                    }]));
                    self.ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                        TopLeftX: left,
                        TopLeftY: 0.0,
                        Width: width,
                        Height: t_height as f32,
                        MinDepth: 0.0,
                        MaxDepth: 1.0,
                    }]));
                }
                if original {
                    self.draw_fullscreen(srv, 1.0, false);
                } else {
                    // the clean scaling: the image's pixels (text, fills) as blocks of one size
                    self.draw_picture(srv, (w as f32, h as f32), (kx, sy), true);
                }
            }
        }
        self.bind_scene();
    }

    fn swap(&mut self, _interval: u32) {
        // a frame that drew nothing shows the scene with its sides cleared, as before the
        // clear was deferred (only `hold` shows a frame again without a swap)
        if self.scene.is_some() {
            self.begin_drawing();
        }
        // SR_NATIVE_TEST: "over" draws over the finished frame; "replace" draws here if
        // nothing 2D followed the game's 3D polygons
        if let Some(t) = &self.native_test {
            if t.mode == crate::native::TestMode::Over || (t.seen_3d && !t.drawn) {
                self.native_test_draw();
            }
        }
        if let Some((dir, every)) = &self.settings.dump {
            if self.frames % every.max(&1) == 0 {
                let path = dir.join(format!("frame{:06}.png", self.frames));
                if let Err(e) = self.dump_scene(&path) {
                    sr_runtime::warn!("frame dump failed: {e}");
                }
            }
        }
        self.present();
        self.native_test_end_frame();
        self.drain_debug_messages();
        self.frames += 1;
        self.clear_view_depth();
        if self.frames % 1000 == 0 && std::env::var_os("SR_KIND_STATS").is_some() {
            if self.kind_stats.iter().any(|k| k.iter().any(|&n| n > 0)) {
                let names = ["scenery", "car", "wheel", "shadow", "smoke", "marks"];
            for (i, n) in names.iter().enumerate() {
                let k = self.kind_stats[i];
                sr_runtime::info!("  3D triangles of {n}: opaque {} (back {}), blended {} (back {})", k[0] + k[1], k[1], k[2] + k[3], k[3]);
            }
        }
        }
        // the sides are cleared when the next frame starts drawing (`begin_drawing`): a
        // frame held and shown again by `hold` (settings overlay) keeps them
        self.sides_pending = true;
    }

    fn begin_view(&mut self) {
        self.clear_view_depth();
    }

    fn wait_ready(&mut self, timeout: std::time::Duration) -> bool {
        let Some(h) = self.latency_wait else {
            // exclusive fullscreen: the present blocks until the next vertical blank once a
            // frame is queued; start a frame only if that blank comes before the timeout
            if !self.settings.vsync {
                return true;
            }
            let now = std::time::Instant::now();
            let next = match self.last_present {
                Some(t) => {
                    // the first blank after now, counted from the last one seen
                    let mut n = t + self.refresh;
                    while n <= now {
                        n += self.refresh;
                    }
                    n
                }
                None => now + self.refresh,
            };
            return next <= now + timeout;
        };
        if self.ready_taken {
            return true;
        }
        if !self.settings.vsync {
            // nothing to wait for: present takes the slot (at once)
            return true;
        }
        let ms = timeout.as_millis().min(1000) as u32;
        let t = std::time::Instant::now();
        let ok = unsafe { windows::Win32::System::Threading::WaitForSingleObjectEx(h, ms, false) == windows::Win32::Foundation::WAIT_OBJECT_0 };
        if std::env::var_os("SR_PRESENT_LOG").is_some() && self.frames > 1500 && self.frames < 1520 {
            sr_runtime::info!("wait_ready({ms} ms): {ok} after {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
        self.ready_taken = ok;
        ok
    }

    fn hold(&mut self) -> bool {
        let open = self.settings.menu.as_ref().is_some_and(|m| m.lock().unwrap().open);
        if open {
            // the game waits at this frame: show it again with the overlay
            self.present();
            if !self.settings.vsync {
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
        }
        open
    }

    fn invalidate_textures(&mut self, start: u32, end: u32) {
        self.textures.retain(|k, t| t.end <= start || k.start >= end);
    }

    // the native renderer, as the runtime calls it: forwarded to the impl block below
    fn native_scene(&mut self, scene: &crate::native::SceneData) -> Result<(), String> {
        self.runtime_native_scene(scene)
    }
    fn native_frame(&mut self, frame: &crate::native::FrameData) -> Result<(), String> {
        self.native_begin_frame(frame)
    }
    fn native_view(&mut self, frame: &crate::native::FrameData, view: usize) -> Result<sr_runtime::glide::NativeViewStats, String> {
        self.runtime_native_view(frame, view)
    }
    fn native_backing(&mut self, wait: bool) -> Vec<crate::native::BackingResults> {
        self.native.as_mut().map_or_else(Vec::new, |n| n.backing_results(wait))
    }
}

// ---- native renderer: the runtime's calls (wave 2, W2-3: `Backend::native_*`) ----
impl D3dBackend {
    /// Upload the scene of a race (`Backend::native_scene`).
    fn runtime_native_scene(&mut self, scene: &crate::native::SceneData) -> Result<(), String> {
        let s = self.native_upload(scene)?;
        sr_runtime::info!(
            "native renderer: {} textures in {} arrays ({:.1} MB RGBA8), {} materials in {} batches, {} chunks, {} static + {} mesh triangles, {} vertices, uploaded in {:.1} ms",
            s.textures,
            s.texture_arrays,
            s.texture_bytes as f64 / 1048576.0,
            s.materials,
            s.batches,
            s.chunks,
            s.static_triangles,
            s.mesh_triangles,
            s.vertices,
            s.upload_ms
        );
        Ok(())
    }

    /// Draw a view now (`Backend::native_view`; the renderer flushes after view 0 of a
    /// frame, `flush_after_main`).
    fn runtime_native_view(&mut self, frame: &crate::native::FrameData, view: usize) -> Result<sr_runtime::glide::NativeViewStats, String> {
        let s = self.native_draw_view(frame, view)?;
        Ok(sr_runtime::glide::NativeViewStats { draws: s.draws, triangles: s.triangles.min(u32::MAX as u64) as u32 })
    }
}

fn n_tris(v: &[Vertex]) -> u64 {
    (v.len() / 3) as u64
}

impl Drop for D3dBackend {
    fn drop(&mut self) {
        // a swap chain must not be released while it holds the display
        if self.exclusive {
            unsafe {
                let _ = self.swap.SetFullscreenState(false, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_side_clear_meets_the_picture() {
        let ext = |w, h| crate::display::extension((640, 480), crate::display::Aspect::Auto, (w, h));
        // the 4:3 area centred, h * 4 / 3 wide
        assert_eq!(side_columns(1280, 640, ext(1280, 720)), (160, 1120));
        assert_eq!(side_columns(1920, 640, ext(1920, 1080)), (240, 1680));
        assert_eq!(side_columns(1366, 640, ext(1366, 768)), (171, 1195));
        assert_eq!(side_columns(2560, 640, ext(2560, 1080)), (560, 2000));
        assert_eq!(side_columns(3840, 640, ext(3840, 2160)), (480, 3360));
        // an odd margin (170.5 pixels): the picture's edge at a pixel centre takes the pixel
        assert_eq!(side_columns(1365, 640, ext(1365, 768)), (170, 1194));
    }
}
