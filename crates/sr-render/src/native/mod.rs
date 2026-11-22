//! Native renderer: draws the game's world from GPU-resident data instead of replaying the
//! game's Glide triangles (design: `docs/04-native-renderer.md`, notes:
//! `docs/re/native/renderer.md`).
//!
//! * [`scene`]: the neutral scene description ([`SceneData`] once per scene,
//!   [`FrameData`] per frame); no guest addresses, no Direct3D types.
//! * [`NativeRenderer`]: the Direct3D 11 side. [`NativeRenderer::upload`] puts a scene on
//!   the GPU (textures expanded to RGBA8 in texture arrays, static instances merged into
//!   camera-relative chunks, meshes for instancing, materials);
//!   [`NativeRenderer::begin_frame`] sends the frame's transforms (computed in f64,
//!   camera-relative), instances (phase, reflection scroll), static fades and per-frame
//!   geometry; [`NativeRenderer::draw_view`] draws one view (culling, batching; the view's
//!   sky and ground quads, then passes sky / opaque / decal / sorted blended) and flushes
//!   after view 0. Animated textures, the game's reflection coordinates and the fades'
//!   screen-door dither are computed in the shaders (`native.hlsl`), checked against CPU
//!   references on a GPU device (`gpu_tests`). Depth is the Glide path's convention
//!   ([`DEPTH_SCALE`]), so Glide-drawn polygons that follow are tested against the world.
//! * Integration: the backend's scene render target is the target; the backend exposes
//!   `D3dBackend::native_upload`, `native_begin_frame`, `native_draw_view` and
//!   `native_draw_frame` (see `crate::d3d`). A view is drawn at the point where the
//!   game would draw its world, after the frame's clear / `set_view` and before the 2D and
//!   HUD drawn through the Glide path, which then land on top.
//! * `SR_NATIVE_TEST`: a synthetic scene ([`test_scene`]) drawn inside the real program,
//!   with timing in the log (see [`TestDriver`]).

mod backing;
#[cfg(test)]
mod gpu_tests;
mod prepare;
mod renderer;
/// The scene description (crate `sr-scene`, shared with the runtime).
pub use sr_scene as scene;
mod test_scene;
mod timing;

pub use renderer::{DEPTH_SCALE, DrawStats, NativeRenderer, SceneStats, Target};
pub use scene::*;
use timing::Accum;

/// What `SR_NATIVE_TEST` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestMode {
    /// `SR_NATIVE_TEST=1`: the game's 3D polygons are dropped and the test scene is drawn
    /// in their place (once per frame, before the first 2D drawing that follows them):
    /// what the switch to the native renderer will look like, with the game's HUD on top.
    Replace,
    /// `SR_NATIVE_TEST=over`: the test scene is drawn over the finished frame just before
    /// it is shown (visible over the menus' 2D pictures too).
    Over,
}

/// Drives the synthetic test scene and collects its timing (`SR_NATIVE_TEST`,
/// `SR_NATIVE_BENCH`).
pub(crate) struct TestDriver {
    pub mode: TestMode,
    pub scene: test_scene::TestScene,
    pub frame: FrameData,
    pub start: std::time::Instant,
    pub uploaded: bool,
    /// This frame: the game drew 3D polygons (dropped), the test scene was drawn.
    pub seen_3d: bool,
    pub drawn: bool,
    /// Per reporting interval: CPU time to build the frame description, CPU time to
    /// submit the views, GPU time of the views (ms), draws and triangles per frame.
    pub build: Accum,
    pub cpu: Accum,
    pub gpu: Accum,
    pub draws: Accum,
    pub tris: Accum,
    pub presents: u64,
    pub report_start: Option<std::time::Instant>,
    /// Frames drawn in total.
    pub total: u64,
    /// `SR_NATIVE_BENCH=n`: n native-only frames back to back, once.
    pub bench: Option<u32>,
}

impl TestDriver {
    pub fn from_env() -> Option<TestDriver> {
        let mode = match std::env::var("SR_NATIVE_TEST").ok()?.to_ascii_lowercase().as_str() {
            "" | "0" | "off" => return None,
            "over" | "2" => TestMode::Over,
            _ => TestMode::Replace,
        };
        let t0 = std::time::Instant::now();
        let scene = test_scene::TestScene::build();
        sr_runtime::info!(
            "native test: {:?} mode, synthetic scene built in {:.1} ms ({} textures, {} materials, {} meshes, {} static instances)",
            mode,
            t0.elapsed().as_secs_f64() * 1000.0,
            scene.scene.textures.len(),
            scene.scene.materials.len(),
            scene.scene.meshes.len(),
            scene.scene.statics.len()
        );
        Some(TestDriver {
            mode,
            scene,
            frame: FrameData::default(),
            start: std::time::Instant::now(),
            uploaded: false,
            seen_3d: false,
            drawn: false,
            build: Accum::default(),
            cpu: Accum::default(),
            gpu: Accum::default(),
            draws: Accum::default(),
            tris: Accum::default(),
            presents: 0,
            report_start: None,
            total: 0,
            bench: std::env::var("SR_NATIVE_BENCH").ok().and_then(|v| v.parse().ok()).filter(|&n| n > 0),
        })
    }

    /// Log the interval's averages and start a new one.
    pub fn report(&mut self, size: (u32, u32), what: &str) {
        let Some(start) = self.report_start else { return };
        let secs = start.elapsed().as_secs_f64();
        sr_runtime::info!(
            "native test{what}: {}x{}: {} frames in {:.2} s = {:.1} fps shown; per frame: build {:.3} ms, submit {:.3} ms (max {:.3}), GPU {:.3} ms (max {:.3}, {} samples), {:.0} draws, {:.0} triangles",
            size.0,
            size.1,
            self.presents,
            secs,
            self.presents as f64 / secs.max(1e-9),
            self.build.mean(),
            self.cpu.mean(),
            self.cpu.max,
            self.gpu.mean(),
            self.gpu.max,
            self.gpu.n,
            self.draws.mean(),
            self.tris.mean()
        );
        self.reset_interval();
    }

    pub fn reset_interval(&mut self) {
        self.build = Accum::default();
        self.cpu = Accum::default();
        self.gpu = Accum::default();
        self.draws = Accum::default();
        self.tris = Accum::default();
        self.presents = 0;
        self.report_start = Some(std::time::Instant::now());
    }
}
