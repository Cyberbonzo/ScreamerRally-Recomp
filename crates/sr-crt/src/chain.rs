//! The shader chain: a Rust port of ShaderGlass's Direct3D 11 runtime (`ShaderGlass.cpp`
//! `Process`, `ShaderPass.cpp`, `Shader.cpp`, `Preset.cpp`, `Texture.cpp`).
//!
//! ShaderGlass: shader effect overlay. Copyright (C) 2021-2025 mausimus (mausimus.net),
//! https://github.com/mausimus/ShaderGlass, GNU General Public License v3.0. This port is
//! part of a GPL-3.0 program.
//!
//! As in ShaderGlass, the input is first copied into the chain's `Original` texture by a
//! preprocess pass (flips, the vertical orientation's quarter turn; here also the game's
//! gamma), then every pass of the preset draws a full-screen quad: into its own texture,
//! the last into the output (the back buffer) inside the output box. RetroArch semantics:
//! `Source`, `Original`, `OriginalHistoryN`, `PassOutputN`, `PassFeedbackN`, aliases and
//! their feedback, the preset's lookup textures; `MVP`, `SourceSize`, `OutputSize`,
//! `OriginalSize`, `FinalViewportSize`, `<texture>Size`, `PassOutputSizeN`, `FrameCount`
//! (with the pass's modulo), `FrameDirection`; scale types source / viewport / absolute;
//! float and sRGB framebuffers and `#pragma format`; wrap modes; push-constant (b1) and UBO
//! (b0) parameter layouts; the preset's parameter overrides.
//!
//! Differences from ShaderGlass, all towards RetroArch's behaviour: `frame_count_mod` is
//! read (ShaderGlass looks for `framecount_mod`, which presets do not use); `mipmap_input`
//! and the textures' `mipmap` generate mipmaps; `FrameDirection` is 1 (ShaderGlass leaves
//! it 0), and the sizes of history, feedback and lookup textures, `Rotation`,
//! `TotalSubFrames`, `CurrentSubFrame`, `OriginalFPS`, `FrameTimeDelta` and
//! `OriginalAspect` are set too.

use std::collections::HashMap;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{PCSTR, s};

use crate::pack::{ParamDef, PassData, PresetData, ShaderData, UserParam, option};

type Srv = ID3D11ShaderResourceView;
type Rtv = ID3D11RenderTargetView;

fn err(e: windows::core::Error) -> String {
    e.to_string()
}

const PUSH_BUFFER: i32 = -1;

/// The two quads (ShaderPass.cpp `sVertexBuffer`): position (x, y, z, w) and texture
/// coordinates. The first is the preprocess pass's (clip space), the second the shader
/// passes' (0..1, mapped by their MVP).
#[rustfmt::skip]
const VERTICES: [f32; 48] = [
    -1.0, -1.0, 0.0, 1.0, 0.0, 1.0,
    -1.0, 1.0,  0.0, 1.0, 0.0, 0.0,
    1.0,  -1.0, 0.0, 1.0, 1.0, 1.0,
    1.0,  1.0,  0.0, 1.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0, 0.0, 1.0,
    0.0, 1.0, 0.0, 1.0, 0.0, 0.0,
    1.0, 0.0, 0.0, 1.0, 1.0, 1.0,
    1.0, 1.0, 0.0, 1.0, 1.0, 0.0,
];

/// ShaderGlass's MVP for shader passes: 0..1 to clip space.
const PASS_MVP: [f32; 16] = [2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, -1.0, 0.0, 1.0];

/// Shader.cpp `sFormats` (the RGBA8 formats map to BGRA as there).
fn format(name: &str) -> Option<DXGI_FORMAT> {
    Some(match name {
        "R8_UNORM" => DXGI_FORMAT_R8_UNORM,
        "R8_UINT" | "R8_SINT" => DXGI_FORMAT_R8_UINT,
        "R8G8_UNORM" => DXGI_FORMAT_R8G8_UNORM,
        "R8G8_UINT" => DXGI_FORMAT_R8G8_UINT,
        "R8G8_SINT" => DXGI_FORMAT_R8G8_SINT,
        "R8G8B8A8_UNORM" => DXGI_FORMAT_B8G8R8A8_UNORM,
        "R8G8B8A8_UINT" => DXGI_FORMAT_R8G8B8A8_UINT,
        "R8G8B8A8_SINT" => DXGI_FORMAT_R8G8B8A8_SINT,
        "R8G8B8A8_SRGB" => DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        "A2B10G10R10_UNORM_PACK32" => DXGI_FORMAT_R10G10B10A2_UNORM,
        "A2B10G10R10_UINT_PACK32" => DXGI_FORMAT_R10G10B10A2_UINT,
        "R16_UINT" => DXGI_FORMAT_R16_UINT,
        "R16_SINT" => DXGI_FORMAT_R16_SINT,
        "R16_SFLOAT" => DXGI_FORMAT_R16_FLOAT,
        "R16G16_UINT" => DXGI_FORMAT_R16G16_UINT,
        "R16G16_SINT" => DXGI_FORMAT_R16G16_SINT,
        "R16G16_SFLOAT" => DXGI_FORMAT_R16G16_FLOAT,
        "R16G16B16A16_UINT" => DXGI_FORMAT_R16G16B16A16_UINT,
        "R16G16B16A16_SINT" => DXGI_FORMAT_R16G16B16A16_SINT,
        "R16G16B16A16_SFLOAT" => DXGI_FORMAT_R16G16B16A16_FLOAT,
        "R32_UINT" => DXGI_FORMAT_R32_UINT,
        "R32_SINT" => DXGI_FORMAT_R32_SINT,
        "R32_SFLOAT" => DXGI_FORMAT_R32_FLOAT,
        "R32G32_UINT" => DXGI_FORMAT_R32G32_UINT,
        "R32G32_SINT" => DXGI_FORMAT_R32G32_SINT,
        "R32G32_SFLOAT" => DXGI_FORMAT_R32G32_FLOAT,
        "R32G32B32A32_UINT" => DXGI_FORMAT_R32G32B32A32_UINT,
        "R32G32B32A32_SINT" => DXGI_FORMAT_R32G32B32A32_SINT,
        "R32G32B32A32_SFLOAT" => DXGI_FORMAT_R32G32B32A32_FLOAT,
        _ => return None,
    })
}

fn is_true(v: Option<&str>) -> bool {
    matches!(v, Some("true") | Some("1"))
}

/// Port of `Shader`: one pass's shaders, its preset options and its parameter buffers.
struct Shader {
    def: ShaderData,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    alias: String,
    scale: (f32, f32),
    viewport: (bool, bool),
    absolute: (bool, bool),
    format: DXGI_FORMAT,
    filter_linear: bool,
    wrap: D3D11_TEXTURE_ADDRESS_MODE,
    frame_count_mod: u32,
    mipmap_input: bool,
    push: Vec<u8>,
    ubo: Vec<u8>,
}

impl Shader {
    fn new(device: &ID3D11Device, pass: &PassData) -> Result<Shader, String> {
        let def = pass.shader.clone();
        let o = |k: &str| option(&pass.options, k);
        let size = |buf: i32| def.params.iter().filter(|p| p.buffer == buf).map(|p| (p.offset + p.size) as usize).max().unwrap_or(0);
        let mut fmt = DXGI_FORMAT_B8G8R8A8_UNORM;
        if is_true(o("srgb_framebuffer")) {
            fmt = DXGI_FORMAT_B8G8R8A8_UNORM_SRGB;
        }
        if is_true(o("float_framebuffer")) {
            fmt = DXGI_FORMAT_R16G16B16A16_FLOAT;
        }
        if !def.format.is_empty() {
            if let Some(f) = format(&def.format) {
                fmt = f;
            }
        }
        let num = |k: &str| o(k).and_then(|v| v.trim().parse::<f32>().ok());
        let mut scale = (num("scale_x").unwrap_or(1.0), num("scale_y").unwrap_or(1.0));
        if let Some(s) = num("scale") {
            scale = (s, s);
        }
        let mut viewport = (o("scale_type_x") == Some("viewport"), o("scale_type_y") == Some("viewport"));
        let mut absolute = (o("scale_type_x") == Some("absolute"), o("scale_type_y") == Some("absolute"));
        match o("scale_type") {
            Some("viewport") => viewport = (true, true),
            Some("absolute") => absolute = (true, true),
            _ => {}
        }
        let wrap = match o("wrap_mode") {
            Some("clamp_to_edge") => D3D11_TEXTURE_ADDRESS_CLAMP,
            Some("repeat") => D3D11_TEXTURE_ADDRESS_WRAP,
            Some("mirrored_repeat") => D3D11_TEXTURE_ADDRESS_MIRROR,
            _ => D3D11_TEXTURE_ADDRESS_BORDER,
        };
        // RetroArch's key is frame_count_mod (ShaderGlass reads framecount_mod)
        let frame_count_mod = num("frame_count_mod").or_else(|| num("framecount_mod")).unwrap_or(0.0).max(0.0) as u32;
        let (mut vs, mut ps, mut layout) = (None, None, None);
        unsafe {
            device.CreateVertexShader(&def.vs, None, Some(&mut vs)).map_err(|e| format!("{}: vertex shader: {e}", def.name))?;
            device.CreatePixelShader(&def.ps, None, Some(&mut ps)).map_err(|e| format!("{}: pixel shader: {e}", def.name))?;
            let el = [
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: s!("TEXCOORD"),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 0,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: s!("TEXCOORD"),
                    SemanticIndex: 1,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
            ];
            device.CreateInputLayout(&el, &def.vs, Some(&mut layout)).map_err(|e| format!("{}: input layout: {e}", def.name))?;
        }
        let mut s = Shader {
            push: vec![0; size(PUSH_BUFFER)],
            ubo: vec![0; size(0)],
            def,
            vs: vs.unwrap(),
            ps: ps.unwrap(),
            layout: layout.unwrap(),
            alias: o("alias").unwrap_or("").to_string(),
            scale,
            viewport,
            absolute,
            format: fmt,
            filter_linear: is_true(o("filter_linear")),
            wrap,
            frame_count_mod,
            mipmap_input: is_true(o("mipmap_input")),
        };
        // every parameter starts at its default (Shader constructor)
        let defaults: Vec<(String, f32)> = s.def.params.iter().map(|p| (p.name.clone(), p.default)).collect();
        for (n, v) in defaults {
            s.set(&n, &v.to_le_bytes());
        }
        // the bytecode is in the device's objects now
        s.def.vs = Vec::new();
        s.def.ps = Vec::new();
        Ok(s)
    }

    /// `SetParam(name, ...)`: every parameter of that name (it can be in both buffers).
    fn set(&mut self, name: &str, v: &[u8]) {
        for p in &self.def.params {
            if p.name == name {
                let buf = if p.buffer == PUSH_BUFFER { &mut self.push } else { &mut self.ubo };
                let n = (p.size as usize).min(v.len());
                let o = p.offset as usize;
                if o + n <= buf.len() {
                    buf[o..o + n].copy_from_slice(&v[..n]);
                }
            }
        }
    }

    fn has(&self, name: &str) -> bool {
        self.def.params.iter().any(|p| p.name == name)
    }

    fn param(&self, name: &str) -> Option<&ParamDef> {
        self.def.params.iter().find(|p| p.name == name)
    }
}

fn float4(w: u32, h: u32) -> [u8; 16] {
    let v = [w as f32, h as f32, 1.0 / w.max(1) as f32, 1.0 / h.max(1) as f32];
    let mut b = [0u8; 16];
    for (i, x) in v.iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
    }
    b
}

fn mat_bytes(m: &[f32; 16]) -> [u8; 64] {
    let mut b = [0u8; 64];
    for (i, x) in m.iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
    }
    b
}

/// Port of `ShaderPass`: a shader with its samplers and constant buffers.
struct Pass {
    shader: Shader,
    /// Binding, texture name, sampler.
    samplers: Vec<(u32, String, ID3D11SamplerState)>,
    ubo: Option<ID3D11Buffer>,
    push: Option<ID3D11Buffer>,
    dest: (u32, u32),
    /// Draws the clip-space quad (the preprocess pass) instead of the 0..1 one.
    preprocess: bool,
    mvp: [f32; 16],
}

struct Lut {
    name: String,
    srv: Srv,
    size: (u32, u32),
    linear: bool,
    wrap: Option<D3D11_TEXTURE_ADDRESS_MODE>,
}

fn sampler(device: &ID3D11Device, linear: bool, wrap: D3D11_TEXTURE_ADDRESS_MODE) -> Result<ID3D11SamplerState, String> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: if linear { D3D11_FILTER_MIN_MAG_MIP_LINEAR } else { D3D11_FILTER_MIN_MAG_MIP_POINT },
        AddressU: wrap,
        AddressV: wrap,
        AddressW: wrap,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
    };
    let mut s = None;
    unsafe { device.CreateSamplerState(&desc, Some(&mut s)).map_err(err)? };
    Ok(s.unwrap())
}

fn buffer(device: &ID3D11Device, size: usize) -> Result<Option<ID3D11Buffer>, String> {
    if size == 0 {
        return Ok(None);
    }
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: ((size + 15) & !15) as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        ..Default::default()
    };
    let mut b = None;
    unsafe { device.CreateBuffer(&desc, None, Some(&mut b)).map_err(err)? };
    Ok(b)
}

impl Pass {
    fn new(device: &ID3D11Device, shader: Shader, luts: &[Lut], preprocess: bool) -> Result<Pass, String> {
        let mut samplers = Vec::new();
        for (name, binding) in &shader.def.samplers {
            // a lookup texture samples as the preset says, everything else as the pass
            let (linear, wrap) = match luts.iter().find(|l| l.name == *name) {
                Some(l) => (l.linear, l.wrap.unwrap_or(D3D11_TEXTURE_ADDRESS_BORDER)),
                None => (shader.filter_linear, shader.wrap),
            };
            samplers.push((*binding, name.clone(), sampler(device, linear, wrap)?));
        }
        Ok(Pass {
            ubo: buffer(device, shader.ubo.len())?,
            push: buffer(device, shader.push.len())?,
            shader,
            samplers,
            dest: (0, 0),
            preprocess,
            mvp: PASS_MVP,
        })
    }

    fn requires_feedback(&self) -> bool {
        self.shader.def.samplers.iter().any(|(n, _)| n.ends_with("Feedback") || n.starts_with("PassFeedback"))
    }

    fn requires_history(&self) -> u32 {
        self.shader
            .def
            .samplers
            .iter()
            .filter_map(|(n, _)| n.strip_prefix("OriginalHistory").and_then(|h| h.parse::<u32>().ok()))
            .filter(|&h| h > 0 && h < 100)
            .max()
            .unwrap_or(0)
    }

    /// `ShaderPass::Resize`: the sizes this pass's uniforms see.
    fn resize(&mut self, source: (u32, u32), dest: (u32, u32), sizes: &[(String, (u32, u32))], pass_sizes: &[(u32, u32)]) {
        self.dest = dest;
        self.shader.set("SourceSize", &float4(source.0, source.1));
        self.shader.set("OutputSize", &float4(dest.0, dest.1));
        for (name, (w, h)) in sizes {
            let key = format!("{name}Size");
            if self.shader.has(&key) {
                self.shader.set(&key, &float4(*w, *h));
            }
        }
        for (p, &(w, h)) in pass_sizes.iter().enumerate() {
            if w != 0 && h != 0 {
                let key = format!("PassOutputSize{p}");
                if self.shader.has(&key) {
                    self.shader.set(&key, &float4(w, h));
                }
            }
        }
    }

    /// `ShaderPass::Render`.
    fn render(&mut self, ctx: &ID3D11DeviceContext, vb: &ID3D11Buffer, source: &Srv, resources: &HashMap<String, Srv>, frame: u32, target: &Rtv, origin: (i32, i32)) {
        let mut fc = frame;
        if self.shader.frame_count_mod > 0 {
            fc %= self.shader.frame_count_mod;
        }
        self.shader.set("FrameCount", &fc.to_le_bytes());
        let mvp = mat_bytes(&self.mvp);
        self.shader.set("MVP", &mvp);
        unsafe {
            for (cb, data) in [(&self.ubo, &self.shader.ubo), (&self.push, &self.shader.push)] {
                if let Some(cb) = cb {
                    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                    if ctx.Map(cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_ok() {
                        std::ptr::copy_nonoverlapping(data.as_ptr(), m.pData as *mut u8, data.len());
                        ctx.Unmap(cb, 0);
                    }
                }
            }
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: origin.0 as f32,
                TopLeftY: origin.1 as f32,
                Width: self.dest.0 as f32,
                Height: self.dest.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.IASetInputLayout(&self.shader.layout);
            let stride = 24u32;
            let offset = 0u32;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(vb.clone())), Some(&stride), Some(&offset));
            ctx.VSSetShader(&self.shader.vs, None);
            ctx.PSSetShader(&self.shader.ps, None);
            let mut bound = Vec::new();
            for (binding, name, samp) in &self.samplers {
                let view = if name == "Source" {
                    Some(source.clone())
                } else {
                    // OriginalHistory0 and missing history are Original
                    resources.get(name).or_else(|| if name.starts_with("OriginalHistory") { resources.get("Original") } else { None }).cloned()
                };
                if let Some(v) = view {
                    ctx.PSSetShaderResources(*binding, Some(&[Some(v)]));
                    bound.push(*binding);
                }
                ctx.PSSetSamplers(*binding, Some(&[Some(samp.clone())]));
            }
            if let Some(cb) = &self.ubo {
                ctx.VSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
                ctx.PSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
            }
            if let Some(cb) = &self.push {
                ctx.VSSetConstantBuffers(1, Some(&[Some(cb.clone())]));
                ctx.PSSetConstantBuffers(1, Some(&[Some(cb.clone())]));
            }
            ctx.Draw(4, if self.preprocess { 0 } else { 4 });
            for b in bound {
                ctx.PSSetShaderResources(b, Some(&[None]));
            }
            ctx.OMSetRenderTargets(Some(&[None]), None);
        }
    }
}

/// A texture the chain renders into and samples from.
struct Target {
    tex: ID3D11Texture2D,
    rtv: Rtv,
    srv: Srv,
    mips: bool,
}

fn create_target(device: &ID3D11Device, w: u32, h: u32, fmt: DXGI_FORMAT, mips: bool) -> Result<Target, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w.max(1),
        Height: h.max(1),
        MipLevels: if mips { 0 } else { 1 },
        ArraySize: 1,
        Format: fmt,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: if mips { D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32 } else { 0 },
    };
    unsafe {
        let mut t = None;
        device.CreateTexture2D(&desc, None, Some(&mut t)).map_err(|e| format!("texture {w}x{h} {fmt:?}: {e}"))?;
        let t = t.unwrap();
        let (mut rtv, mut srv) = (None, None);
        device.CreateRenderTargetView(&t, None, Some(&mut rtv)).map_err(err)?;
        device.CreateShaderResourceView(&t, None, Some(&mut srv)).map_err(err)?;
        Ok(Target { tex: t, rtv: rtv.unwrap(), srv: srv.unwrap(), mips })
    }
}

fn create_copy(device: &ID3D11Device, w: u32, h: u32, fmt: DXGI_FORMAT) -> Result<(ID3D11Texture2D, Srv), String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w.max(1),
        Height: h.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: fmt,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        ..Default::default()
    };
    unsafe {
        let mut t = None;
        device.CreateTexture2D(&desc, None, Some(&mut t)).map_err(err)?;
        let t = t.unwrap();
        let mut srv = None;
        device.CreateShaderResourceView(&t, None, Some(&mut srv)).map_err(err)?;
        Ok((t, srv.unwrap()))
    }
}

/// Resources that depend on the input and output sizes.
struct Sized {
    key: (u32, u32, u32, u32, i32),
    original: Target,
    /// Outputs of the passes but the last.
    outputs: Vec<Target>,
    /// Feedback copies of every pass (the last one's from the output box).
    feedback: Vec<(ID3D11Texture2D, Srv)>,
    /// OriginalHistory1.. (index 0 = 1).
    history: Vec<(ID3D11Texture2D, Srv)>,
    resources: HashMap<String, Srv>,
}

/// What a chain is built from.
pub struct ChainDesc<'a> {
    pub preset: &'a PresetData,
    /// The vertical orientation (ShaderGlass "Orientation: Vertical").
    pub vertical: bool,
}

/// Compile one of the built-in shaders (builtin.hlsl).
fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    const SRC: &str = include_str!("builtin.hlsl");
    unsafe {
        let (mut code, mut errors) = (None, None);
        let r = D3DCompile(SRC.as_ptr() as *const _, SRC.len(), s!("builtin.hlsl"), None, None, entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut errors));
        if let Err(e) = r {
            let msg = errors
                .map(|b: ID3DBlob| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).to_string())
                .unwrap_or_default();
            return Err(format!("built-in shader: {e} {msg}"));
        }
        let b = code.unwrap();
        Ok(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec())
    }
}

/// A built-in pass; `vertical` is its SGVertical (kept by `Chain::reset_params`, which
/// puts every parameter back to its default).
fn builtin_shader(name: &str, pixel: PCSTR, linear: bool, vertical: f32) -> Result<PassData, String> {
    let pd = |n: &str, o: u32, s: u32, d: f32| ParamDef { name: n.into(), buffer: 0, offset: o, size: s, min: 0.0, max: 0.0, default: d, step: 0.0, description: String::new() };
    Ok(PassData {
        shader: ShaderData {
            name: name.into(),
            format: String::new(),
            vs: compile(s!("vs_main"), s!("vs_5_0"))?,
            ps: compile(pixel, s!("ps_5_0"))?,
            params: vec![pd("MVP", 0, 64, 0.0), pd("SGVertical", 64, 4, vertical), pd("Gamma", 68, 4, 1.0)],
            samplers: vec![("Source".into(), 2)],
        },
        options: if linear { vec![("filter_linear".into(), "true".into())] } else { Vec::new() },
    })
}

/// ShaderGlass's "none" preset (PassthroughPresetDef): the input as it is, scaled to the
/// output with nearest-neighbour sampling.
pub fn passthrough_preset() -> Result<PresetData, String> {
    Ok(PresetData { name: "none".into(), category: "general".into(), passes: vec![builtin_shader("passthrough", s!("ps_copy"), false, 0.0)?], ..Default::default() })
}

/// Decode and create a lookup texture (Texture.cpp; with RetroArch's `mipmap`).
fn create_lut(device: &ID3D11Device, ctx: &ID3D11DeviceContext, t: &crate::pack::TextureData) -> Result<Lut, String> {
    let (w, h, px) = crate::image::decode_rgba(&t.png).map_err(|e| format!("texture {}: {e}", t.name))?;
    let o = |k: &str| option(&t.options, k);
    let mips = o("mipmap") == Some("true");
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: if mips { 0 } else { 1 },
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: if mips { (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32 } else { D3D11_BIND_SHADER_RESOURCE.0 as u32 },
        CPUAccessFlags: 0,
        MiscFlags: if mips { D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32 } else { 0 },
    };
    unsafe {
        let mut tex = None;
        if mips {
            device.CreateTexture2D(&desc, None, Some(&mut tex)).map_err(err)?;
        } else {
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as *const _, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
            device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)).map_err(err)?;
        }
        let tex = tex.unwrap();
        let mut srv = None;
        device.CreateShaderResourceView(&tex, None, Some(&mut srv)).map_err(err)?;
        let srv = srv.unwrap();
        if mips {
            ctx.UpdateSubresource(&tex, 0, None, px.as_ptr() as *const _, w * 4, 0);
            ctx.GenerateMips(&srv);
        }
        let wrap = match o("wrap_mode") {
            Some("repeat") => Some(D3D11_TEXTURE_ADDRESS_WRAP),
            Some("clamp_to_edge") => Some(D3D11_TEXTURE_ADDRESS_CLAMP),
            Some("mirrored_repeat") => Some(D3D11_TEXTURE_ADDRESS_MIRROR),
            _ => None,
        };
        Ok(Lut { name: t.name.clone(), srv, size: (w, h), linear: o("linear") == Some("true"), wrap })
    }
}

/// A preset ready to run.
pub struct Chain {
    pub name: String,
    device: ID3D11Device,
    pre: Pass,
    passes: Vec<Pass>,
    luts: Vec<Lut>,
    vb: ID3D11Buffer,
    raster: ID3D11RasterizerState,
    vertical: bool,
    flip: (bool, bool),
    sized: Option<Sized>,
    start: Option<f64>,
    last_time: Option<f64>,
    requires_feedback: bool,
    requires_history: u32,
    /// The parameters the player can change, and the overrides.
    pub params: Vec<UserParam>,
    overrides: Vec<(String, f32)>,
    /// GPU memory of the lookup textures, and of all textures at the current size
    /// (estimates, bytes).
    pub texture_bytes: u64,
    pub gpu_bytes: u64,
}

fn bpp(f: DXGI_FORMAT) -> u64 {
    match f {
        DXGI_FORMAT_R16G16B16A16_FLOAT | DXGI_FORMAT_R16G16B16A16_UINT | DXGI_FORMAT_R16G16B16A16_SINT | DXGI_FORMAT_R32G32_FLOAT => 8,
        DXGI_FORMAT_R32G32B32A32_FLOAT | DXGI_FORMAT_R32G32B32A32_UINT | DXGI_FORMAT_R32G32B32A32_SINT => 16,
        DXGI_FORMAT_R8_UNORM | DXGI_FORMAT_R8_UINT => 1,
        DXGI_FORMAT_R8G8_UNORM | DXGI_FORMAT_R16_FLOAT | DXGI_FORMAT_R16_UINT | DXGI_FORMAT_R16_SINT => 2,
        _ => 4,
    }
}

impl Chain {
    /// `Preset::Create` + `ShaderGlass::RebuildShaders`.
    pub fn new(device: &ID3D11Device, ctx: &ID3D11DeviceContext, desc: &ChainDesc) -> Result<Chain, String> {
        let preset = desc.preset;
        if preset.passes.is_empty() {
            return Err(format!("{}: no passes", preset.name));
        }
        let mut luts = Vec::new();
        let mut texture_bytes = 0;
        for t in &preset.textures {
            let l = create_lut(device, ctx, t)?;
            texture_bytes += l.size.0 as u64 * l.size.1 as u64 * 4;
            luts.push(l);
        }
        let mut passes = Vec::new();
        for p in &preset.passes {
            passes.push(Pass::new(device, Shader::new(device, p)?, &luts, false)?);
        }
        let turn = if desc.vertical { 1.0 } else { 0.0 };
        let pre_def = builtin_shader("preprocess", s!("ps_main"), false, turn)?;
        let pre = Pass::new(device, Shader::new(device, &pre_def)?, &[], true)?;
        if desc.vertical {
            // the quarter turn back at the end (the preprocess shader as a last pass)
            passes.push(Pass::new(device, Shader::new(device, &pre_def)?, &[], false)?);
        }
        let vb = unsafe {
            let d = D3D11_BUFFER_DESC { ByteWidth: (VERTICES.len() * 4) as u32, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32, ..Default::default() };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: VERTICES.as_ptr() as *const _, ..Default::default() };
            let mut b = None;
            device.CreateBuffer(&d, Some(&init), Some(&mut b)).map_err(err)?;
            b.unwrap()
        };
        let raster = unsafe {
            let d = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: false.into(), ..Default::default() };
            let mut r = None;
            device.CreateRasterizerState(&d, Some(&mut r)).map_err(err)?;
            r.unwrap()
        };
        let requires_feedback = passes.iter().any(|p| p.requires_feedback());
        let requires_history = passes.iter().map(|p| p.requires_history()).max().unwrap_or(0);
        let mut c = Chain {
            name: preset.name.clone(),
            device: device.clone(),
            pre,
            passes,
            luts,
            vb,
            raster,
            vertical: desc.vertical,
            flip: (false, false),
            sized: None,
            start: None,
            last_time: None,
            requires_feedback,
            requires_history,
            params: preset.user_params(),
            overrides: preset.overrides.clone(),
            texture_bytes,
            gpu_bytes: texture_bytes,
        };
        c.reset_params();
        Ok(c)
    }

    /// The number of shader passes (without the built-in ones).
    pub fn pass_count(&self) -> usize {
        self.passes.len() - self.vertical as usize
    }

    /// `ResetParams`: every parameter at the preset's override or the shader's default.
    pub fn reset_params(&mut self) {
        for i in 0..self.passes.len() {
            let names: Vec<(String, f32)> = self.passes[i]
                .shader
                .def
                .params
                .iter()
                .filter(|p| p.size == 4 && p.name != "FrameCount")
                .map(|p| (p.name.clone(), self.overrides.iter().find(|o| o.0 == p.name).map_or(p.default, |o| o.1)))
                .collect();
            for (n, v) in names {
                self.passes[i].shader.set(&n, &v.to_le_bytes());
            }
        }
        // runtime values the shaders read like parameters (see the module documentation)
        self.set_runtime("FrameDirection", 1i32.to_le_bytes());
        self.set_runtime("Rotation", 0u32.to_le_bytes());
        self.set_runtime("TotalSubFrames", 1u32.to_le_bytes());
        self.set_runtime("CurrentSubFrame", 1u32.to_le_bytes());
        self.set_runtime("OriginalFPS", 60.0f32.to_le_bytes());
    }

    fn set_runtime(&mut self, name: &str, v: [u8; 4]) {
        for p in &mut self.passes {
            p.shader.set(name, &v);
        }
    }

    /// Set a parameter (by name, in every pass that has it).
    pub fn set_param(&mut self, name: &str, value: f32) {
        for p in &mut self.passes {
            if p.shader.param(name).is_some_and(|d| d.size == 4) {
                p.shader.set(name, &value.to_le_bytes());
            }
        }
    }

    pub fn set_flip(&mut self, h: bool, v: bool) {
        self.flip = (h, v);
    }

    /// Run the chain: `input` (`input_size` pixels) through every pass, the last into
    /// `output` (a `output_size` render target) inside `rect` (x, y, w, h; may reach beyond
    /// it). `output_tex` is the output's texture (the last pass's feedback is copied from
    /// it). `time` in seconds drives FrameCount (ShaderGlass: 60 per second); `gamma` is
    /// applied to the input (1 = none).
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        ctx: &ID3D11DeviceContext,
        input: &Srv,
        input_size: (u32, u32),
        output: &Rtv,
        output_tex: &ID3D11Texture2D,
        output_size: (u32, u32),
        rect: (i32, i32, u32, u32),
        time: f64,
        gamma: f32,
    ) -> Result<(), String> {
        let (rx, ry, rw, rh) = rect;
        if rw == 0 || rh == 0 || input_size.0 == 0 || input_size.1 == 0 {
            return Ok(());
        }
        let out_format = unsafe {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            output_tex.GetDesc(&mut d);
            d.Format
        };
        let key = (input_size.0, input_size.1, rw, rh, out_format.0);
        if self.sized.as_ref().is_none_or(|s| s.key != key) {
            self.sized = None;
            self.sized = Some(self.build(input_size, (rw, rh), out_format, key)?);
        }
        let start = *self.start.get_or_insert(time);
        // ShaderGlass: shaders run at a logical 60 frames per second
        let frame = (((time - start) * 1000.0) / 16.666_666).round().max(0.0) as u32;
        let delta = self.last_time.map_or(0, |t| ((time - t) * 1e6).max(0.0) as u32);
        self.last_time = Some(time);
        self.set_runtime("FrameTimeDelta", delta.to_le_bytes());
        let sized = self.sized.as_ref().unwrap();
        unsafe {
            ctx.GSSetShader(None, None);
            ctx.RSSetState(&self.raster);
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.OMSetDepthStencilState(None, 0);
            ctx.RSSetScissorRects(Some(&[RECT { left: 0, top: 0, right: 16384, bottom: 16384 }]));
        }
        // preprocess: the input into Original (flipped as asked: ShaderGlass's MVP scale and
        // translation, its small offset to pick the same input pixel every time)
        let (mut sx, mut sy, mut tx, mut ty) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
        if self.flip.0 {
            sx = -sx;
            tx = -tx;
        }
        if self.flip.1 {
            sy = -sy;
            ty = -ty;
        }
        tx += 0.0001;
        ty += 0.0001;
        self.pre.mvp = [sx, 0.0, 0.0, 0.0, 0.0, sy, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, tx, ty, 0.0, 1.0];
        self.pre.shader.set("Gamma", &gamma.to_le_bytes());
        let orig = sized.original.rtv.clone();
        self.pre.render(ctx, &self.vb, input, &sized.resources, frame, &orig, (0, 0));
        if sized.original.mips {
            unsafe { ctx.GenerateMips(&sized.original.srv) };
        }
        let n = self.passes.len();
        for p in 0..n {
            let source = if p == 0 { sized.original.srv.clone() } else { sized.outputs[p - 1].srv.clone() };
            let (target, origin) = if p == n - 1 { (output.clone(), (rx, ry)) } else { (sized.outputs[p].rtv.clone(), (0, 0)) };
            self.passes[p].render(ctx, &self.vb, &source, &sized.resources, frame, &target, origin);
            if p < n - 1 && sized.outputs[p].mips {
                unsafe { ctx.GenerateMips(&sized.outputs[p].srv) };
            }
        }
        unsafe {
            if self.requires_feedback {
                for (q, out) in sized.outputs.iter().enumerate() {
                    ctx.CopyResource(&sized.feedback[q].0, &out.tex);
                }
                // the last pass's output: its box in the output, clipped to it
                let (bw, bh) = (output_size.0 as i32, output_size.1 as i32);
                let (x0, y0) = (rx.max(0), ry.max(0));
                let (x1, y1) = ((rx + rw as i32).min(bw), (ry + rh as i32).min(bh));
                if x1 > x0 && y1 > y0 {
                    let b = D3D11_BOX { left: x0 as u32, top: y0 as u32, front: 0, right: x1 as u32, bottom: y1 as u32, back: 1 };
                    ctx.CopySubresourceRegion(&sized.feedback[n - 1].0, 0, (x0 - rx) as u32, (y0 - ry) as u32, 0, output_tex, 0, Some(&b));
                }
            }
        }
        if self.requires_history > 0 {
            // the oldest history texture takes the current Original; the others move back
            let s = self.sized.as_mut().unwrap();
            let oldest = s.history.pop().unwrap();
            unsafe { ctx.CopyResource(&oldest.0, &s.original.tex) };
            s.history.insert(0, oldest);
            for (h, (_, srv)) in s.history.iter().enumerate() {
                s.resources.insert(format!("OriginalHistory{}", h + 1), srv.clone());
            }
        }
        Ok(())
    }

    /// The sizes of every pass and the textures between them (`Process`: the resize and
    /// `rebuildPasses` parts).
    fn build(&mut self, input: (u32, u32), viewport: (u32, u32), out_format: DXGI_FORMAT, key: (u32, u32, u32, u32, i32)) -> Result<Sized, String> {
        let device = self.device.clone();
        let (mut ow, mut oh) = input;
        let (mut vw, mut vh) = viewport;
        if self.vertical {
            std::mem::swap(&mut ow, &mut oh);
            std::mem::swap(&mut vw, &mut vh);
        }
        let mut sizes: Vec<(String, (u32, u32))> = vec![("Original".into(), (ow, oh)), ("FinalViewport".into(), (vw, vh))];
        for l in &self.luts {
            sizes.push((l.name.clone(), l.size));
        }
        for h in 1..=self.requires_history {
            sizes.push((format!("OriginalHistory{h}"), (ow, oh)));
        }
        let n = self.passes.len();
        let mut pass_sizes: Vec<((u32, u32), (u32, u32))> = Vec::with_capacity(n);
        let (mut sw, mut sh) = (ow, oh);
        let (mut fvw, mut fvh) = (vw, vh);
        for (p, pass) in self.passes.iter().enumerate() {
            if p == n - 1 {
                // the last pass draws the source into the (real) viewport
                if self.vertical {
                    std::mem::swap(&mut fvw, &mut fvh);
                }
                pass_sizes.push(((sw, sh), (fvw, fvh)));
            } else {
                let s = &pass.shader;
                let w = if s.viewport.0 {
                    vw as f32 * s.scale.0
                } else if s.absolute.0 {
                    s.scale.0
                } else {
                    sw as f32 * s.scale.0
                };
                let h = if s.viewport.1 {
                    vh as f32 * s.scale.1
                } else if s.absolute.1 {
                    s.scale.1
                } else {
                    sh as f32 * s.scale.1
                };
                let (w, h) = ((w as u32).clamp(1, 16384), (h as u32).clamp(1, 16384));
                pass_sizes.push(((sw, sh), (w, h)));
                if !s.alias.is_empty() {
                    sizes.push((s.alias.clone(), (w, h)));
                    sizes.push((format!("{}Feedback", s.alias), (w, h)));
                }
                sizes.push((format!("PassFeedback{p}"), (w, h)));
                (sw, sh) = (w, h);
            }
        }
        let outs: Vec<(u32, u32)> = pass_sizes.iter().map(|p| p.1).collect();
        self.pre.resize(input, (ow, oh), &sizes, &outs);
        for (p, pass) in self.passes.iter_mut().enumerate() {
            pass.resize(pass_sizes[p].0, pass_sizes[p].1, &sizes, &outs);
        }
        let aspect = ow as f32 / oh.max(1) as f32;
        self.set_runtime("OriginalAspect", aspect.to_le_bytes());
        self.set_runtime("OriginalAspectRotated", aspect.to_le_bytes());
        // textures: Original (mipmapped when pass 0 asks), each pass's output but the
        // last's (mipmapped when the next pass asks), feedback and history copies
        let mut resources: HashMap<String, Srv> = HashMap::new();
        for l in &self.luts {
            resources.insert(l.name.clone(), l.srv.clone());
        }
        let mut bytes = self.texture_bytes;
        let original = create_target(&device, ow, oh, DXGI_FORMAT_B8G8R8A8_UNORM, self.passes[0].shader.mipmap_input)?;
        bytes += ow as u64 * oh as u64 * 4;
        resources.insert("Original".into(), original.srv.clone());
        let mut outputs = Vec::new();
        let mut feedback = Vec::new();
        for p in 0..n.saturating_sub(1) {
            let s = &self.passes[p].shader;
            let (w, h) = pass_sizes[p].1;
            let t = create_target(&device, w, h, s.format, self.passes[p + 1].shader.mipmap_input)?;
            bytes += w as u64 * h as u64 * bpp(s.format) * (1 + self.requires_feedback as u64);
            resources.insert(format!("PassOutput{p}"), t.srv.clone());
            if !s.alias.is_empty() {
                resources.insert(s.alias.clone(), t.srv.clone());
            }
            if self.requires_feedback {
                let f = create_copy(&device, w, h, s.format)?;
                resources.insert(format!("PassFeedback{p}"), f.1.clone());
                if !s.alias.is_empty() {
                    resources.insert(format!("{}Feedback", s.alias), f.1.clone());
                }
                feedback.push(f);
            }
            outputs.push(t);
        }
        if self.requires_feedback {
            let last = &self.passes[n - 1].shader;
            let (w, h) = pass_sizes[n - 1].1;
            let f = create_copy(&device, w, h, out_format)?;
            bytes += w as u64 * h as u64 * 4;
            resources.insert(format!("PassFeedback{}", n - 1), f.1.clone());
            if !last.alias.is_empty() {
                resources.insert(format!("{}Feedback", last.alias), f.1.clone());
            }
            feedback.push(f);
        }
        let mut history = Vec::new();
        for h in 0..self.requires_history {
            let c = create_copy(&device, ow, oh, DXGI_FORMAT_B8G8R8A8_UNORM)?;
            bytes += ow as u64 * oh as u64 * 4;
            resources.insert(format!("OriginalHistory{}", h + 1), c.1.clone());
            history.push(c);
        }
        self.gpu_bytes = bytes;
        Ok(Sized { key, original, outputs, feedback, history, resources })
    }
}
