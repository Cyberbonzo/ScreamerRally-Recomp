//! CRT emulation in the renderer (a child module of `d3d`, see `sr_crt`).
//!
//! While it is on, the whole frame (3D, HUD, the menus' pictures; both renderers) is drawn
//! at the shader's input size, the game's own 480 lines by default: `resize_if_needed`
//! asks `crt_input` for the scene's size, which then replaces the picture's size and the
//! render scale. The present runs the preset on the scene into the back buffer
//! (`crt_draw`) instead of the plain copy; the settings overlay is drawn over it after.
//! Off, nothing here runs and the frames are those of the renderer without it.

use super::*;

impl D3dBackend {
    /// The scene's size with CRT emulation on, for a picture of `picture` pixels.
    pub(super) fn crt_input(&mut self, picture: (u32, u32)) -> Option<(u32, u32)> {
        self.crt.input_size(picture)
    }

    /// The present's copy of the scene into the back buffer, through the CRT shader.
    /// Returns false when CRT emulation is off (the caller copies as usual).
    pub(super) fn crt_draw(&mut self, scene: &ID3D11ShaderResourceView, back: &ID3D11RenderTargetView, picture: (u32, u32, u32, u32)) -> bool {
        let Some(s) = &self.scene else { return false };
        let size = (s.width, s.height);
        let Ok(back_tex) = (unsafe { self.swap.GetBuffer::<ID3D11Texture2D>(0) }) else { return false };
        let time = sr_runtime::time::uptime().as_secs_f64();
        let drew = self.crt.draw(&self.device, &self.ctx, scene, size, back, &back_tex, self.back_size, picture, self.gamma, time);
        for m in self.crt.take_log() {
            sr_runtime::info!("{m} (output {:?}, input {}x{}, GPU textures {:.1} MB)", self.crt.last_rect, size.0, size.1, self.crt.gpu_bytes() as f64 / 1048576.0);
        }
        // the shader's messages (a preset that failed, a snapshot saved) on the overlay
        if let Some(live) = &self.settings.crt {
            if let Some(msg) = live.status.lock().unwrap().take() {
                if let Some(menu) = &self.settings.menu {
                    menu.lock().unwrap().status = msg;
                }
            }
        }
        self.bound = None;
        drew
    }
}
