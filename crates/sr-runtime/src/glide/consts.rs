//! Glide 2.x constants (from the public `glide.h`) and the `gu*` helper semantics.

pub const GR_SSTTYPE_VOODOO: u32 = 0;

// combine function / factor / local / other
pub const GR_COMBINE_FUNCTION_ZERO: u32 = 0x0;
pub const GR_COMBINE_FUNCTION_LOCAL: u32 = 0x1;
pub const GR_COMBINE_FUNCTION_LOCAL_ALPHA: u32 = 0x2;
pub const GR_COMBINE_FUNCTION_SCALE_OTHER: u32 = 0x3;
pub const GR_COMBINE_FUNCTION_SCALE_OTHER_ADD_LOCAL: u32 = 0x4;
pub const GR_COMBINE_FUNCTION_SCALE_OTHER_ADD_LOCAL_ALPHA: u32 = 0x5;
pub const GR_COMBINE_FUNCTION_SCALE_OTHER_MINUS_LOCAL: u32 = 0x6;
pub const GR_COMBINE_FUNCTION_SCALE_OTHER_MINUS_LOCAL_ADD_LOCAL: u32 = 0x7;
pub const GR_COMBINE_FUNCTION_SCALE_OTHER_MINUS_LOCAL_ADD_LOCAL_ALPHA: u32 = 0x8;
pub const GR_COMBINE_FUNCTION_SCALE_MINUS_LOCAL_ADD_LOCAL: u32 = 0x9;
pub const GR_COMBINE_FUNCTION_SCALE_MINUS_LOCAL_ADD_LOCAL_ALPHA: u32 = 0x10;

pub const GR_COMBINE_FACTOR_ZERO: u32 = 0x0;
pub const GR_COMBINE_FACTOR_LOCAL: u32 = 0x1;
pub const GR_COMBINE_FACTOR_OTHER_ALPHA: u32 = 0x2;
pub const GR_COMBINE_FACTOR_LOCAL_ALPHA: u32 = 0x3;
pub const GR_COMBINE_FACTOR_TEXTURE_ALPHA: u32 = 0x4;
pub const GR_COMBINE_FACTOR_TEXTURE_RGB: u32 = 0x5;
pub const GR_COMBINE_FACTOR_ONE: u32 = 0x8;
pub const GR_COMBINE_FACTOR_ONE_MINUS_LOCAL: u32 = 0x9;
pub const GR_COMBINE_FACTOR_ONE_MINUS_OTHER_ALPHA: u32 = 0xa;
pub const GR_COMBINE_FACTOR_ONE_MINUS_LOCAL_ALPHA: u32 = 0xb;
pub const GR_COMBINE_FACTOR_ONE_MINUS_TEXTURE_ALPHA: u32 = 0xc;

pub const GR_COMBINE_LOCAL_ITERATED: u32 = 0x0;
pub const GR_COMBINE_LOCAL_CONSTANT: u32 = 0x1;
pub const GR_COMBINE_LOCAL_DEPTH: u32 = 0x2;

pub const GR_COMBINE_OTHER_ITERATED: u32 = 0x0;
pub const GR_COMBINE_OTHER_TEXTURE: u32 = 0x1;
pub const GR_COMBINE_OTHER_CONSTANT: u32 = 0x2;

// blend factors
pub const GR_BLEND_ZERO: u32 = 0x0;
pub const GR_BLEND_SRC_ALPHA: u32 = 0x1;
pub const GR_BLEND_SRC_COLOR: u32 = 0x2;
pub const GR_BLEND_DST_ALPHA: u32 = 0x3;
pub const GR_BLEND_ONE: u32 = 0x4;
pub const GR_BLEND_ONE_MINUS_SRC_ALPHA: u32 = 0x5;
pub const GR_BLEND_ONE_MINUS_SRC_COLOR: u32 = 0x6;
pub const GR_BLEND_ONE_MINUS_DST_ALPHA: u32 = 0x7;
pub const GR_BLEND_ALPHA_SATURATE: u32 = 0xf;

pub const GR_CMP_NEVER: u32 = 0;
pub const GR_CMP_ALWAYS: u32 = 7;

pub const GR_CULL_DISABLE: u32 = 0;
pub const GR_CULL_NEGATIVE: u32 = 1;
pub const GR_CULL_POSITIVE: u32 = 2;

pub const GR_DEPTHBUFFER_DISABLE: u32 = 0;
pub const GR_DEPTHBUFFER_ZBUFFER: u32 = 1;
pub const GR_DEPTHBUFFER_WBUFFER: u32 = 2;

pub const GR_FOG_DISABLE: u32 = 0;
pub const GR_FOG_WITH_ITERATED_ALPHA: u32 = 1;
pub const GR_FOG_WITH_TABLE: u32 = 2;

pub const GR_LFBWRITEMODE_565: u32 = 0x0;
pub const GR_LFBWRITEMODE_ANY: u32 = 0xff;
pub const GR_ORIGIN_UPPER_LEFT: u32 = 0;
pub const GR_ORIGIN_ANY: u32 = 0xff;

pub const GR_LFB_SRC_FMT_565: u32 = 0x00;
pub const GR_LFB_SRC_FMT_555: u32 = 0x01;
pub const GR_LFB_SRC_FMT_1555: u32 = 0x02;

pub const GR_TEXTUREFILTER_POINT_SAMPLED: u32 = 0;
pub const GR_TEXTUREFILTER_BILINEAR: u32 = 1;
pub const GR_TEXTURECLAMP_WRAP: u32 = 0;
pub const GR_TEXTURECLAMP_CLAMP: u32 = 1;
pub const GR_MIPMAP_DISABLE: u32 = 0;

// texture formats
pub const GR_TEXFMT_RGB_332: u32 = 0x0;
pub const GR_TEXFMT_YIQ_422: u32 = 0x1;
pub const GR_TEXFMT_ALPHA_8: u32 = 0x2;
pub const GR_TEXFMT_INTENSITY_8: u32 = 0x3;
pub const GR_TEXFMT_ALPHA_INTENSITY_44: u32 = 0x4;
pub const GR_TEXFMT_P_8: u32 = 0x5;
pub const GR_TEXFMT_ARGB_8332: u32 = 0x8;
pub const GR_TEXFMT_AYIQ_8422: u32 = 0x9;
pub const GR_TEXFMT_RGB_565: u32 = 0xa;
pub const GR_TEXFMT_ARGB_1555: u32 = 0xb;
pub const GR_TEXFMT_ARGB_4444: u32 = 0xc;
pub const GR_TEXFMT_ALPHA_INTENSITY_88: u32 = 0xd;
pub const GR_TEXFMT_AP_88: u32 = 0xe;

pub const GR_TEXTABLE_NCC0: u32 = 0;
pub const GR_TEXTABLE_NCC1: u32 = 1;
pub const GR_TEXTABLE_PALETTE: u32 = 2;

// texture combine functions (grTexCombineFunction / guTexCombineFunction)
pub const GR_TEXTURECOMBINE_ZERO: u32 = 0x0;
pub const GR_TEXTURECOMBINE_DECAL: u32 = 0x1;
pub const GR_TEXTURECOMBINE_OTHER: u32 = 0x2;
pub const GR_TEXTURECOMBINE_ADD: u32 = 0x3;
pub const GR_TEXTURECOMBINE_MULTIPLY: u32 = 0x4;
pub const GR_TEXTURECOMBINE_SUBTRACT: u32 = 0x5;
pub const GR_TEXTURECOMBINE_ONE: u32 = 0xa;

/// `GrScreenResolution_t` to pixels.
pub fn resolution(r: u32) -> Option<(u32, u32)> {
    Some(match r {
        0x0 => (320, 200),
        0x1 => (320, 240),
        0x2 => (400, 256),
        0x3 => (512, 384),
        0x4 => (640, 200),
        0x5 => (640, 350),
        0x6 => (640, 400),
        0x7 => (640, 480),
        0x8 => (800, 600),
        0x9 => (960, 720),
        0xa => (856, 480),
        0xb => (512, 256),
        0xc => (1024, 768),
        0xd => (1280, 1024),
        0xe => (1600, 1200),
        0xf => (400, 300),
        _ => return None,
    })
}

/// `guColorCombineFunction` presets expressed as `grColorCombine` arguments
/// (function, factor, local, other, invert).
pub fn gu_color_combine(f: u32) -> [u32; 5] {
    match f {
        0x0 => [GR_COMBINE_FUNCTION_ZERO, GR_COMBINE_FACTOR_ZERO, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_ITERATED, 0], // ZERO
        0x1 => [GR_COMBINE_FUNCTION_LOCAL, GR_COMBINE_FACTOR_ZERO, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_ITERATED, 0], // CCRGB
        0x2 => [GR_COMBINE_FUNCTION_LOCAL, GR_COMBINE_FACTOR_ZERO, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_ITERATED, 0], // ITRGB
        0x3 => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_LOCAL_ALPHA, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_ITERATED, 0], // ITRGB_DELTA0
        0x4 => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // DECAL_TEXTURE
        0x5 => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_LOCAL, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_TEXTURE, 0], // TEXTURE_TIMES_CCRGB
        0x6 => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_LOCAL, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // TEXTURE_TIMES_ITRGB
        0x7 => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_LOCAL, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // ..._DELTA0
        0x8 => [GR_COMBINE_FUNCTION_SCALE_OTHER_ADD_LOCAL, GR_COMBINE_FACTOR_LOCAL, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // TEXTURE_TIMES_ITRGB_ADD_ALPHA
        0x9 => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_LOCAL_ALPHA, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // TEXTURE_TIMES_ALPHA
        0xa => [GR_COMBINE_FUNCTION_SCALE_OTHER_ADD_LOCAL, GR_COMBINE_FACTOR_LOCAL_ALPHA, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // TEXTURE_TIMES_ALPHA_ADD_ITRGB
        0xb => [GR_COMBINE_FUNCTION_SCALE_OTHER_ADD_LOCAL, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // TEXTURE_ADD_ITRGB
        0xc => [GR_COMBINE_FUNCTION_SCALE_OTHER_MINUS_LOCAL, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // TEXTURE_SUB_ITRGB
        0xd => [GR_COMBINE_FUNCTION_SCALE_OTHER_MINUS_LOCAL_ADD_LOCAL, GR_COMBINE_FACTOR_LOCAL_ALPHA, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_CONSTANT, 0], // CCRGB_BLEND_ITRGB_ON_TEXALPHA
        0xe => [GR_COMBINE_FUNCTION_SCALE_OTHER_MINUS_LOCAL_ADD_LOCAL, GR_COMBINE_FACTOR_TEXTURE_ALPHA, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // DIFF_SPEC_A
        0xf => [GR_COMBINE_FUNCTION_SCALE_OTHER_ADD_LOCAL_ALPHA, GR_COMBINE_FACTOR_LOCAL, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // DIFF_SPEC_B
        _ => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0],
    }
}

/// `guAlphaSource` presets as `grAlphaCombine` arguments.
pub fn gu_alpha_source(mode: u32) -> [u32; 5] {
    match mode {
        0 => [GR_COMBINE_FUNCTION_LOCAL, GR_COMBINE_FACTOR_NONE_, GR_COMBINE_LOCAL_CONSTANT, GR_COMBINE_OTHER_NONE_, 0], // CC_ALPHA
        1 => [GR_COMBINE_FUNCTION_LOCAL, GR_COMBINE_FACTOR_NONE_, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_NONE_, 0], // ITERATED_ALPHA
        2 => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_LOCAL_NONE_, GR_COMBINE_OTHER_TEXTURE, 0], // TEXTURE_ALPHA
        _ => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_LOCAL, GR_COMBINE_LOCAL_ITERATED, GR_COMBINE_OTHER_TEXTURE, 0], // TEX * IT
    }
}
const GR_COMBINE_FACTOR_NONE_: u32 = GR_COMBINE_FACTOR_ZERO;
const GR_COMBINE_OTHER_NONE_: u32 = GR_COMBINE_OTHER_CONSTANT;
const GR_COMBINE_LOCAL_NONE_: u32 = GR_COMBINE_LOCAL_CONSTANT;

/// `grTexCombineFunction` presets as `grTexCombine` arguments
/// (rgb function, rgb factor, alpha function, alpha factor, rgb invert, alpha invert).
pub fn tex_combine_function(f: u32) -> [u32; 6] {
    match f {
        GR_TEXTURECOMBINE_ZERO => [GR_COMBINE_FUNCTION_ZERO, GR_COMBINE_FACTOR_NONE_, GR_COMBINE_FUNCTION_ZERO, GR_COMBINE_FACTOR_NONE_, 0, 0],
        GR_TEXTURECOMBINE_OTHER => [GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, GR_COMBINE_FUNCTION_SCALE_OTHER, GR_COMBINE_FACTOR_ONE, 0, 0],
        GR_TEXTURECOMBINE_ONE => [GR_COMBINE_FUNCTION_ZERO, GR_COMBINE_FACTOR_NONE_, GR_COMBINE_FUNCTION_ZERO, GR_COMBINE_FACTOR_NONE_, 1, 1],
        // DECAL and every multi-TMU mode collapse to "this TMU's texel" with one TMU
        _ => [GR_COMBINE_FUNCTION_LOCAL, GR_COMBINE_FACTOR_NONE_, GR_COMBINE_FUNCTION_LOCAL, GR_COMBINE_FACTOR_NONE_, 0, 0],
    }
}

/// W value of fog table entry `i` (Glide's `guFogTableIndexToW`).
pub fn fog_index_to_w(i: i32) -> f32 {
    2f32.powf(3.0 + (i >> 2) as f32) / (8 - (i & 3)) as f32
}

/// `guFogGenerateExp / Exp2 / Linear`: fill a 64-entry table in guest memory.
pub fn gu_fog_generate(name: &str, table: u32, p1: f32, p2: f32) {
    let max_w = fog_index_to_w(63);
    for i in 0..64 {
        let w = fog_index_to_w(i);
        let f = match name {
            "GUFOGGENERATEEXP" => {
                let scale = 1.0 - (-p1 * max_w).exp();
                (1.0 - (-p1 * w).exp()) / scale
            }
            "GUFOGGENERATEEXP2" => {
                let scale = 1.0 - (-(p1 * max_w) * (p1 * max_w)).exp();
                (1.0 - (-(p1 * w) * (p1 * w)).exp()) / scale
            }
            _ => {
                if w <= p1 {
                    0.0
                } else if w >= p2 {
                    1.0
                } else {
                    (w - p1) / (p2 - p1)
                }
            }
        };
        unsafe { crate::mem::wr8(table + i as u32, (f.clamp(0.0, 1.0) * 255.0) as u8) };
    }
}
