//! Glide rendering state and vertices.

use super::consts::*;
use super::tex::TexInfo;
use crate::mem;

/// One `GrVertex` as the game passed it (screen space, Glide 2.x layout, 72 bytes).
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct Vertex {
    pub x: f32,
    pub y: f32,
    /// 65535/z (Z-buffering).
    pub ooz: f32,
    /// 1/w (W-buffering, fog, perspective).
    pub oow: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
    /// TMU0 s/w, t/w and its own 1/w.
    pub sow: f32,
    pub tow: f32,
    pub tmu_oow: f32,
}

impl Vertex {
    pub const SIZE: u32 = 72;

    /// # Safety
    /// `p` must point at a `GrVertex` in guest memory.
    pub unsafe fn read(p: u32) -> Vertex {
        let f = |o: u32| f32::from_bits(unsafe { mem::rd32(p + o) });
        Vertex {
            x: f(0),
            y: f(4),
            r: f(12),
            g: f(16),
            b: f(20),
            ooz: f(24),
            a: f(28),
            oow: f(32),
            sow: f(36),
            tow: f(40),
            tmu_oow: f(44),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct TexBinding {
    /// 0 = no texture source set yet.
    pub valid: u32,
    pub start: u32,
    pub even_odd: u32,
    pub info: TexInfo,
}

/// Everything `grGlideGetState` / `grGlideSetState` save and restore.
///
/// It is copied verbatim into the game's 312-byte `GrState`, so it is `#[repr(C)]` and
/// made only of plain integers (any bit pattern the guest hands back is a valid value).
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct GlideState {
    pub color_combine: [u32; 5],
    pub alpha_combine: [u32; 5],
    pub tex_combine: [u32; 6],
    pub blend: [u32; 4],
    pub constant_color: u32,
    pub chromakey_value: u32,
    pub fog_color: u32,
    pub clip: [u32; 4],
    pub tex: TexBinding,
    pub alpha_test: u32,
    pub cull: u32,
    pub depth_mode: u32,
    pub depth_func: u32,
    pub fog_mode: u32,
    pub origin: u32,
    pub render_buffer: u32,
    pub filter: [u32; 2],
    pub clamp: [u32; 2],
    pub mipmap: u32,
    pub gamma_bits: u32,
    pub depth_bias: i16,
    pub alpha_ref: u8,
    pub color_mask: u8,
    pub alpha_mask: u8,
    pub depth_mask: u8,
    pub chromakey: u8,
    pub _pad: u8,
    pub fog_table: [u8; 64],
}

const _: () = assert!(std::mem::size_of::<GlideState>() <= 312);

impl Default for GlideState {
    fn default() -> Self {
        GlideState {
            color_combine: [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_ITERATED, 0],
            alpha_combine: [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_CONSTANT, 0],
            tex_combine: [GR_COMBINE_FUNCTION_LOCAL, GR_COMBINE_FACTOR_ZERO, GR_COMBINE_FUNCTION_LOCAL, GR_COMBINE_FACTOR_ZERO, 0, 0],
            blend: [GR_BLEND_ONE, GR_BLEND_ZERO, GR_BLEND_ONE, GR_BLEND_ZERO],
            constant_color: 0xffff_ffff,
            chromakey_value: 0,
            fog_color: 0,
            clip: [0, 0, 640, 480],
            tex: TexBinding::default(),
            alpha_test: GR_CMP_ALWAYS,
            cull: GR_CULL_DISABLE,
            depth_mode: GR_DEPTHBUFFER_DISABLE,
            depth_func: 1, // GR_CMP_LESS
            fog_mode: GR_FOG_DISABLE,
            origin: GR_ORIGIN_UPPER_LEFT,
            render_buffer: 1,
            filter: [GR_TEXTUREFILTER_POINT_SAMPLED; 2],
            clamp: [GR_TEXTURECLAMP_WRAP; 2],
            mipmap: GR_MIPMAP_DISABLE,
            gamma_bits: 1.0f32.to_bits(),
            depth_bias: 0,
            alpha_ref: 0,
            color_mask: 1,
            alpha_mask: 0,
            depth_mask: 0,
            chromakey: 0,
            _pad: 0,
            fog_table: [0; 64],
        }
    }
}

impl GlideState {
    pub fn disable_all_effects(&mut self) {
        self.blend = [GR_BLEND_ONE, GR_BLEND_ZERO, GR_BLEND_ONE, GR_BLEND_ZERO];
        self.alpha_test = GR_CMP_ALWAYS;
        self.chromakey = 0;
        self.depth_mode = GR_DEPTHBUFFER_DISABLE;
        self.fog_mode = GR_FOG_DISABLE;
    }

    /// # Safety
    /// `p` must point at a writable `GrState` (312 bytes) in guest memory.
    pub unsafe fn save(&self, p: u32) {
        let n = std::mem::size_of::<GlideState>();
        unsafe { std::ptr::copy_nonoverlapping(self as *const _ as *const u8, mem::ptr(p), n) };
    }

    /// # Safety
    /// `p` must point at a readable `GrState` in guest memory.
    pub unsafe fn load(&mut self, p: u32) {
        let n = std::mem::size_of::<GlideState>();
        unsafe { std::ptr::copy_nonoverlapping(mem::ptr(p), self as *mut _ as *mut u8, n) };
    }
}
