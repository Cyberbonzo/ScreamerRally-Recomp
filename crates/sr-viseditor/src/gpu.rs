//! The editor window's own Direct3D 11 device and swap chain (never the game's): the map
//! (one static vertex buffer per race, a texture per texture variant), the shapes over it,
//! and the panel's pictures.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{PCSTR, s};

use sr_runtime::visedit::{Cell, GRID, TrackMap};

use crate::canvas::Canvas;
use crate::editor::Layout;
use crate::mesh::{self, Batch, MapVertex};
use crate::shapes::ShapeVertex;
use crate::view::MapView;

const SHADER: &str = include_str!("map.hlsl");
/// Window background (outside the track's surfaces).
const BACKGROUND: [f32; 4] = [0.055, 0.063, 0.082, 1.0];

fn err(e: windows::core::Error) -> String {
    e.to_string()
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let r = D3DCompile(SHADER.as_ptr() as *const _, SHADER.len(), s!("map.hlsl"), None, None, entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut errors));
        if let Err(e) = r {
            let msg = errors.map(|b: ID3DBlob| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).to_string()).unwrap_or_default();
            return Err(format!("shader compile failed: {e} {msg}"));
        }
        let b = code.unwrap();
        Ok(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec())
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MapCb {
    xf: [f32; 4],
    dep: [f32; 4],
    info: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ScreenCb {
    screen: [f32; 4],
    dst: [f32; 4],
}

/// The race's map on the GPU.
struct MapGpu {
    vb: ID3D11Buffer,
    batches: Vec<Batch>,
    textures: Vec<ID3D11ShaderResourceView>,
    origin: [f64; 3],
    y_range: [f32; 2],
}

/// A picture (canvas) as a texture.
struct Picture {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    w: i32,
    h: i32,
}

/// What a frame shows.
pub struct Frame<'a> {
    pub layout: Layout,
    pub view: MapView,
    pub tinted: bool,
    pub hover: Option<Cell>,
    pub textures: bool,
    pub shapes: &'a [ShapeVertex],
    pub panel: &'a Canvas,
    pub status: &'a Canvas,
    pub message: Option<&'a Canvas>,
}

pub struct Renderer {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    dsv: Option<ID3D11DepthStencilView>,
    size: (u32, u32),
    vs_map: ID3D11VertexShader,
    ps_map: ID3D11PixelShader,
    il_map: ID3D11InputLayout,
    vs_shape: ID3D11VertexShader,
    ps_shape: ID3D11PixelShader,
    il_shape: ID3D11InputLayout,
    vs_pic: ID3D11VertexShader,
    ps_pic: ID3D11PixelShader,
    cb_map: ID3D11Buffer,
    cb_screen: ID3D11Buffer,
    samp_map: ID3D11SamplerState,
    samp_pic: ID3D11SamplerState,
    blend_alpha: ID3D11BlendState,
    blend_premul: ID3D11BlendState,
    depth_write: ID3D11DepthStencilState,
    depth_test: ID3D11DepthStencilState,
    depth_off: ID3D11DepthStencilState,
    raster: ID3D11RasterizerState,
    states_tex: ID3D11Texture2D,
    states_srv: ID3D11ShaderResourceView,
    map: Option<MapGpu>,
    shape_vb: Option<(ID3D11Buffer, usize)>,
    pictures: [Option<Picture>; 3],
}

impl Renderer {
    pub fn new(hwnd: isize, size: (u32, u32)) -> Result<Renderer, String> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_FLAG(0), Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx)).map_err(err)?;
            let device: ID3D11Device = device.unwrap();
            let ctx: ID3D11DeviceContext = ctx.unwrap();
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(err)?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0.max(1),
                Height: size.1.max(1),
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let hwnd = HWND(hwnd as *mut _);
            let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None).map_err(err)?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);

            let vs_map_code = compile(s!("vs_map"), s!("vs_5_0"))?;
            let vs_shape_code = compile(s!("vs_shape"), s!("vs_5_0"))?;
            let mut vs_map = None;
            let mut ps_map = None;
            let mut vs_shape = None;
            let mut ps_shape = None;
            let mut vs_pic = None;
            let mut ps_pic = None;
            device.CreateVertexShader(&vs_map_code, None, Some(&mut vs_map)).map_err(err)?;
            device.CreatePixelShader(&compile(s!("ps_map"), s!("ps_5_0"))?, None, Some(&mut ps_map)).map_err(err)?;
            device.CreateVertexShader(&vs_shape_code, None, Some(&mut vs_shape)).map_err(err)?;
            device.CreatePixelShader(&compile(s!("ps_shape"), s!("ps_5_0"))?, None, Some(&mut ps_shape)).map_err(err)?;
            device.CreateVertexShader(&compile(s!("vs_picture"), s!("vs_5_0"))?, None, Some(&mut vs_pic)).map_err(err)?;
            device.CreatePixelShader(&compile(s!("ps_picture"), s!("ps_5_0"))?, None, Some(&mut ps_pic)).map_err(err)?;

            let el = |name: PCSTR, fmt: DXGI_FORMAT, off: u32| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: fmt,
                InputSlot: 0,
                AlignedByteOffset: off,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            };
            let map_el = [
                el(s!("POSITION"), DXGI_FORMAT_R32G32B32_FLOAT, 0),
                el(s!("TEXCOORD"), DXGI_FORMAT_R32G32_FLOAT, 12),
                el(s!("COLOR"), DXGI_FORMAT_R8G8B8A8_UNORM, 20),
                el(s!("CELL"), DXGI_FORMAT_R32_UINT, 24),
            ];
            let mut il_map = None;
            device.CreateInputLayout(&map_el, &vs_map_code, Some(&mut il_map)).map_err(err)?;
            let shape_el = [el(s!("POSITION"), DXGI_FORMAT_R32G32_FLOAT, 0), el(s!("COLOR"), DXGI_FORMAT_R8G8B8A8_UNORM, 8)];
            let mut il_shape = None;
            device.CreateInputLayout(&shape_el, &vs_shape_code, Some(&mut il_shape)).map_err(err)?;

            let cbuf = |size: usize| -> Result<ID3D11Buffer, String> {
                let desc = D3D11_BUFFER_DESC {
                    ByteWidth: size.div_ceil(16) as u32 * 16,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                };
                let mut b = None;
                device.CreateBuffer(&desc, None, Some(&mut b)).map_err(err)?;
                Ok(b.unwrap())
            };
            let cb_map = cbuf(std::mem::size_of::<MapCb>())?;
            let cb_screen = cbuf(std::mem::size_of::<ScreenCb>())?;

            let sampler = |filter: D3D11_FILTER, addr: D3D11_TEXTURE_ADDRESS_MODE| -> Result<ID3D11SamplerState, String> {
                let desc = D3D11_SAMPLER_DESC { Filter: filter, AddressU: addr, AddressV: addr, AddressW: addr, MaxAnisotropy: 1, ComparisonFunc: D3D11_COMPARISON_NEVER, MinLOD: 0.0, MaxLOD: f32::MAX, ..Default::default() };
                let mut s = None;
                device.CreateSamplerState(&desc, Some(&mut s)).map_err(err)?;
                Ok(s.unwrap())
            };
            // texels sharp up close, averaged far away
            let samp_map = sampler(D3D11_FILTER_MIN_LINEAR_MAG_POINT_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_WRAP)?;
            let samp_pic = sampler(D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_TEXTURE_ADDRESS_CLAMP)?;

            let blend = |src: D3D11_BLEND| -> Result<ID3D11BlendState, String> {
                let mut bd = D3D11_BLEND_DESC::default();
                bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: true.into(),
                    SrcBlend: src,
                    DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ONE,
                    DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
                };
                let mut b = None;
                device.CreateBlendState(&bd, Some(&mut b)).map_err(err)?;
                Ok(b.unwrap())
            };
            let blend_alpha = blend(D3D11_BLEND_SRC_ALPHA)?;
            let blend_premul = blend(D3D11_BLEND_ONE)?;

            let depth = |enable: bool, write: bool| -> Result<ID3D11DepthStencilState, String> {
                let desc = D3D11_DEPTH_STENCIL_DESC {
                    DepthEnable: enable.into(),
                    DepthWriteMask: if write { D3D11_DEPTH_WRITE_MASK_ALL } else { D3D11_DEPTH_WRITE_MASK_ZERO },
                    DepthFunc: D3D11_COMPARISON_GREATER_EQUAL,
                    ..Default::default()
                };
                let mut d = None;
                device.CreateDepthStencilState(&desc, Some(&mut d)).map_err(err)?;
                Ok(d.unwrap())
            };
            let depth_write = depth(true, true)?;
            let depth_test = depth(true, false)?;
            let depth_off = depth(false, false)?;

            let rd = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ScissorEnable: true.into(), ..Default::default() };
            let mut raster = None;
            device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(err)?;

            let td = D3D11_TEXTURE2D_DESC {
                Width: GRID as u32,
                Height: GRID as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8_UINT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            let mut states_tex = None;
            let mut states_srv = None;
            device.CreateTexture2D(&td, None, Some(&mut states_tex)).map_err(err)?;
            let states_tex = states_tex.unwrap();
            device.CreateShaderResourceView(&states_tex, None, Some(&mut states_srv)).map_err(err)?;

            let mut r = Renderer {
                device,
                ctx,
                swap,
                rtv: None,
                dsv: None,
                size: (0, 0),
                vs_map: vs_map.unwrap(),
                ps_map: ps_map.unwrap(),
                il_map: il_map.unwrap(),
                vs_shape: vs_shape.unwrap(),
                ps_shape: ps_shape.unwrap(),
                il_shape: il_shape.unwrap(),
                vs_pic: vs_pic.unwrap(),
                ps_pic: ps_pic.unwrap(),
                cb_map,
                cb_screen,
                samp_map,
                samp_pic,
                blend_alpha,
                blend_premul,
                depth_write,
                depth_test,
                depth_off,
                raster: raster.unwrap(),
                states_tex,
                states_srv: states_srv.unwrap(),
                map: None,
                shape_vb: None,
                pictures: [None, None, None],
            };
            r.upload_states(&vec![1u8; GRID * GRID]);
            r.resize(size.0, size.1)?;
            Ok(r)
        }
    }

    /// Follow the window's client size.
    pub fn resize(&mut self, w: u32, h: u32) -> Result<(), String> {
        let (w, h) = (w.max(1), h.max(1));
        if self.size == (w, h) && self.rtv.is_some() {
            return Ok(());
        }
        unsafe {
            self.ctx.OMSetRenderTargets(None, None);
            self.rtv = None;
            self.dsv = None;
            self.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0)).map_err(err)?;
            let bb: ID3D11Texture2D = self.swap.GetBuffer(0).map_err(err)?;
            let mut rtv = None;
            self.device.CreateRenderTargetView(&bb, None, Some(&mut rtv)).map_err(err)?;
            let dd = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_D32_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_DEPTH_STENCIL.0 as u32,
                ..Default::default()
            };
            let mut depth = None;
            self.device.CreateTexture2D(&dd, None, Some(&mut depth)).map_err(err)?;
            let mut dsv = None;
            self.device.CreateDepthStencilView(&depth.unwrap(), None, Some(&mut dsv)).map_err(err)?;
            self.rtv = rtv;
            self.dsv = dsv;
        }
        self.size = (w, h);
        Ok(())
    }

    /// The race's map: a vertex buffer and its textures (`None`: no map).
    pub fn set_map(&mut self, map: Option<&TrackMap>) -> Result<(), String> {
        self.map = None;
        let Some(map) = map else { return Ok(()) };
        let t0 = std::time::Instant::now();
        let m = mesh::build(map);
        if m.vertices.is_empty() {
            return Ok(());
        }
        let mut textures = Vec::with_capacity(m.textures.len());
        for &(t, variant) in &m.textures {
            let levels = mesh::texture_levels(&map.textures[t as usize], variant);
            textures.push(self.texture(&levels)?);
        }
        let vb = unsafe {
            let desc = D3D11_BUFFER_DESC { ByteWidth: (m.vertices.len() * std::mem::size_of::<MapVertex>()) as u32, Usage: D3D11_USAGE_IMMUTABLE, BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32, ..Default::default() };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: m.vertices.as_ptr() as *const _, ..Default::default() };
            let mut b = None;
            self.device.CreateBuffer(&desc, Some(&init), Some(&mut b)).map_err(err)?;
            b.unwrap()
        };
        sr_runtime::info!(
            "visibility editor: map on the GPU in {:.0} ms: {} vertices, {} batches, {} textures",
            t0.elapsed().as_secs_f64() * 1000.0,
            m.vertices.len(),
            m.batches.len(),
            textures.len()
        );
        self.map = Some(MapGpu { vb, batches: m.batches, textures, origin: m.origin, y_range: m.y_range });
        Ok(())
    }

    fn texture(&self, levels: &[mesh::Level]) -> Result<ID3D11ShaderResourceView, String> {
        let td = D3D11_TEXTURE2D_DESC {
            Width: levels[0].0,
            Height: levels[0].1,
            MipLevels: levels.len() as u32,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let init: Vec<D3D11_SUBRESOURCE_DATA> = levels.iter().map(|(w, _, px)| D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as *const _, SysMemPitch: w * 4, SysMemSlicePitch: 0 }).collect();
        unsafe {
            let mut t = None;
            let mut srv = None;
            self.device.CreateTexture2D(&td, Some(init.as_ptr()), Some(&mut t)).map_err(err)?;
            self.device.CreateShaderResourceView(&t.unwrap(), None, Some(&mut srv)).map_err(err)?;
            Ok(srv.unwrap())
        }
    }

    /// The sector states (`row * 256 + column`).
    pub fn upload_states(&mut self, states: &[u8]) {
        if states.len() != GRID * GRID {
            return;
        }
        unsafe { self.ctx.UpdateSubresource(&self.states_tex, 0, None, states.as_ptr() as *const _, GRID as u32, 0) };
    }

    fn write<T: Copy>(&self, buf: &ID3D11Buffer, v: &[T]) {
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if self.ctx.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_ok() {
                std::ptr::copy_nonoverlapping(v.as_ptr() as *const u8, m.pData as *mut u8, std::mem::size_of_val(v));
                self.ctx.Unmap(buf, 0);
            }
        }
    }

    /// The canvas in picture slot `slot` (0 panel, 1 status line, 2 message).
    fn picture(&mut self, slot: usize, c: &Canvas) -> Option<ID3D11ShaderResourceView> {
        let fits = self.pictures[slot].as_ref().is_some_and(|p| p.w == c.w && p.h == c.h);
        if !fits {
            let td = D3D11_TEXTURE2D_DESC {
                Width: c.w as u32,
                Height: c.h as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            };
            unsafe {
                let mut t = None;
                let mut srv = None;
                self.device.CreateTexture2D(&td, None, Some(&mut t)).ok()?;
                let t = t.unwrap();
                self.device.CreateShaderResourceView(&t, None, Some(&mut srv)).ok()?;
                self.pictures[slot] = Some(Picture { tex: t, srv: srv.unwrap(), w: c.w, h: c.h });
            }
        }
        let p = self.pictures[slot].as_ref()?;
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&p.tex, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).ok()?;
            for y in 0..c.h as usize {
                let src = &c.px[y * c.w as usize..(y + 1) * c.w as usize];
                std::ptr::copy_nonoverlapping(src.as_ptr() as *const u8, (m.pData as *mut u8).add(y * m.RowPitch as usize), c.w as usize * 4);
            }
            self.ctx.Unmap(&p.tex, 0);
        }
        Some(p.srv.clone())
    }

    fn viewport(&self, r: [i32; 4]) {
        unsafe {
            self.ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: r[0] as f32, TopLeftY: r[1] as f32, Width: r[2].max(1) as f32, Height: r[3].max(1) as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            self.ctx.RSSetScissorRects(Some(&[RECT { left: r[0], top: r[1], right: r[0] + r[2], bottom: r[1] + r[3] }]));
        }
    }

    /// Draw a frame into the back buffer (not presented yet).
    pub fn draw(&mut self, f: &Frame) {
        let (Some(rtv), Some(dsv)) = (self.rtv.clone(), self.dsv.clone()) else { return };
        let (w, h) = self.size;
        unsafe {
            self.ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), &dsv);
            self.ctx.ClearRenderTargetView(&rtv, &BACKGROUND);
            self.ctx.ClearDepthStencilView(&dsv, D3D11_CLEAR_DEPTH.0 as u32, 0.0, 0);
            self.ctx.RSSetState(&self.raster);
        }
        // the map
        if let Some(m) = &self.map {
            let r = f.layout.map;
            self.viewport(r);
            let v = &f.view;
            let (sx, sz) = (2.0 * v.scale / r[2].max(1) as f64, 2.0 * v.scale / r[3].max(1) as f64);
            let cb = MapCb {
                xf: [sx as f32, ((m.origin[0] - v.center[0]) * sx) as f32, sz as f32, ((m.origin[2] - v.center[1]) * sz) as f32],
                dep: [m.y_range[0], 1.0 / (m.y_range[1] - m.y_range[0]).max(1.0), (!f.textures) as u32 as f32, 0.0],
                info: [f.tinted as u32, f.hover.map_or(0, |c| c.0 as u32 * GRID as u32 + c.1 as u32 + 1), 0, 0],
            };
            unsafe {
                self.ctx.IASetInputLayout(&self.il_map);
                self.ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                let stride = std::mem::size_of::<MapVertex>() as u32;
                self.ctx.IASetVertexBuffers(0, 1, Some(&Some(m.vb.clone())), Some(&stride), Some(&0));
                self.ctx.VSSetShader(&self.vs_map, None);
                self.ctx.PSSetShader(&self.ps_map, None);
                self.ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb_map.clone())]));
                self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb_map.clone())]));
                self.ctx.PSSetSamplers(0, Some(&[Some(self.samp_map.clone())]));
                self.ctx.PSSetShaderResources(1, Some(&[Some(self.states_srv.clone())]));
            }
            let mut blended_now = None;
            for b in &m.batches {
                if blended_now != Some(b.blended) {
                    blended_now = Some(b.blended);
                    unsafe {
                        if b.blended {
                            self.ctx.OMSetBlendState(&self.blend_alpha, None, 0xffff_ffff);
                            self.ctx.OMSetDepthStencilState(&self.depth_test, 0);
                        } else {
                            self.ctx.OMSetBlendState(None, None, 0xffff_ffff);
                            self.ctx.OMSetDepthStencilState(&self.depth_write, 0);
                        }
                    }
                }
                let mut cb = cb;
                cb.dep[3] = b.keyed as u32 as f32;
                cb.info[2] = b.texture.is_some() as u32;
                self.write(&self.cb_map, &[cb]);
                unsafe {
                    let srv = b.texture.map(|t| m.textures[t].clone());
                    self.ctx.PSSetShaderResources(0, Some(&[srv]));
                    self.ctx.Draw(b.count, b.start);
                }
            }
            unsafe { self.ctx.PSSetShaderResources(0, Some(&[None, None])) };
        }
        // shapes over the map
        let screen = ScreenCb { screen: [2.0 / w as f32, 2.0 / h as f32, 0.0, 0.0], dst: [0.0; 4] };
        unsafe {
            self.ctx.OMSetBlendState(&self.blend_premul, None, 0xffff_ffff);
            self.ctx.OMSetDepthStencilState(&self.depth_off, 0);
        }
        if !f.shapes.is_empty() {
            self.viewport([0, 0, w as i32, h as i32]);
            let m = f.layout.map;
            unsafe { self.ctx.RSSetScissorRects(Some(&[RECT { left: m[0], top: m[1], right: m[0] + m[2], bottom: m[1] + m[3] }])) };
            let bytes = std::mem::size_of_val(f.shapes);
            if self.shape_vb.as_ref().is_none_or(|b| b.1 < bytes) {
                let cap = bytes.next_power_of_two().max(1 << 16);
                let desc = D3D11_BUFFER_DESC { ByteWidth: cap as u32, Usage: D3D11_USAGE_DYNAMIC, BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32, CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, ..Default::default() };
                let mut b = None;
                if unsafe { self.device.CreateBuffer(&desc, None, Some(&mut b)) }.is_ok() {
                    self.shape_vb = b.map(|b| (b, cap));
                }
            }
            if let Some((vb, _)) = self.shape_vb.clone() {
                self.write(&vb, f.shapes);
                self.write(&self.cb_screen, &[screen]);
                unsafe {
                    self.ctx.IASetInputLayout(&self.il_shape);
                    self.ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                    let stride = std::mem::size_of::<ShapeVertex>() as u32;
                    self.ctx.IASetVertexBuffers(0, 1, Some(&Some(vb)), Some(&stride), Some(&0));
                    self.ctx.VSSetShader(&self.vs_shape, None);
                    self.ctx.PSSetShader(&self.ps_shape, None);
                    self.ctx.VSSetConstantBuffers(1, Some(&[Some(self.cb_screen.clone())]));
                    self.ctx.Draw(f.shapes.len() as u32, 0);
                }
            }
        }
        // the panel, the status line, a message over the map
        let s = f.layout.scale;
        let mut pics: Vec<(usize, &Canvas, [i32; 2])> = vec![(0, f.panel, [f.layout.panel[0], f.layout.panel[1]]), (1, f.status, [f.layout.status[0], f.layout.status[1]])];
        if let Some(msg) = f.message {
            let ms = s * 2;
            let m = f.layout.map;
            pics.push((2, msg, [m[0] + (m[2] - msg.w * ms) / 2, m[1] + (m[3] - msg.h * ms) / 2]));
        }
        self.viewport([0, 0, w as i32, h as i32]);
        for (slot, c, at) in pics {
            let Some(srv) = self.picture(slot, c) else { continue };
            let k = if slot == 2 { s * 2 } else { s };
            let dst = [at[0] as f32, at[1] as f32, (at[0] + c.w * k) as f32, (at[1] + c.h * k) as f32];
            self.write(&self.cb_screen, &[ScreenCb { screen: screen.screen, dst }]);
            unsafe {
                self.ctx.IASetInputLayout(None);
                self.ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
                self.ctx.VSSetShader(&self.vs_pic, None);
                self.ctx.PSSetShader(&self.ps_pic, None);
                self.ctx.VSSetConstantBuffers(1, Some(&[Some(self.cb_screen.clone())]));
                self.ctx.PSSetShaderResources(2, Some(&[Some(srv)]));
                self.ctx.PSSetSamplers(1, Some(&[Some(self.samp_pic.clone())]));
                self.ctx.Draw(4, 0);
                self.ctx.PSSetShaderResources(2, Some(&[None]));
            }
        }
    }

    /// The back buffer as RGBA (`r | g << 8 | b << 16 | a << 24`), before presenting it.
    pub fn capture(&self) -> Result<(u32, u32, Vec<u32>), String> {
        unsafe {
            let bb: ID3D11Texture2D = self.swap.GetBuffer(0).map_err(err)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            bb.GetDesc(&mut desc);
            let sd = D3D11_TEXTURE2D_DESC { Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0, ..desc };
            let mut staging = None;
            self.device.CreateTexture2D(&sd, None, Some(&mut staging)).map_err(err)?;
            let staging = staging.unwrap();
            self.ctx.CopyResource(&staging, &bb);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(err)?;
            let mut px = Vec::with_capacity((desc.Width * desc.Height) as usize);
            for y in 0..desc.Height {
                let row = (m.pData as *const u8).add((y * m.RowPitch) as usize) as *const u32;
                px.extend_from_slice(std::slice::from_raw_parts(row, desc.Width as usize));
            }
            self.ctx.Unmap(&staging, 0);
            Ok((desc.Width, desc.Height, px))
        }
    }

    pub fn present(&self) {
        unsafe {
            let _ = self.swap.Present(1, DXGI_PRESENT(0));
        }
    }

    /// The device works (not removed).
    pub fn ok(&self) -> bool {
        unsafe { self.device.GetDeviceRemovedReason().is_ok() }
    }
}
