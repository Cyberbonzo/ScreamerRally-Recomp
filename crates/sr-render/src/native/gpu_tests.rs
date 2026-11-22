//! GPU tests of the native renderer: the real shaders on a Direct3D 11 device (WARP, the
//! software rasteriser, so the results do not depend on the machine's GPU; the shared
//! formulas also on the hardware device when there is one), read back and compared with the
//! CPU references in `prepare.rs` and with the Glide path's own vertex shader
//! (`voodoo.hlsl`) for the shared depth buffer.

use super::prepare::{self, GpuVertex};
use super::renderer::{DEPTH_SCALE, NativeRenderer, SHADER, Target, as_bytes, compile_source, immutable_buffer};
use super::scene::*;
use crate::Filter;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::s;

fn device(kind: D3D_DRIVER_TYPE) -> Option<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut ctx = None;
    unsafe {
        D3D11CreateDevice(None, kind, HMODULE::default(), D3D11_CREATE_DEVICE_FLAG(0), Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx)).ok()?;
    }
    Some((device?, ctx?))
}

/// The WARP device (every test needs it; it is part of Windows).
fn warp() -> (ID3D11Device, ID3D11DeviceContext) {
    device(D3D_DRIVER_TYPE_WARP).expect("WARP Direct3D 11 device")
}

/// A colour target (RGBA8) and depth-stencil (D32_S8, as the backend's scene target).
struct Harness {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    w: u32,
    h: u32,
    color: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    depth: ID3D11Texture2D,
    dsv: ID3D11DepthStencilView,
}

impl Harness {
    fn new(w: u32, h: u32) -> Harness {
        Harness::on(warp(), w, h)
    }

    fn on((device, ctx): (ID3D11Device, ID3D11DeviceContext), w: u32, h: u32) -> Harness {
        unsafe {
            let td = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                ..Default::default()
            };
            let mut color = None;
            device.CreateTexture2D(&td, None, Some(&mut color)).unwrap();
            let color = color.unwrap();
            let mut rtv = None;
            device.CreateRenderTargetView(&color, None, Some(&mut rtv)).unwrap();
            let dd = D3D11_TEXTURE2D_DESC { Format: DXGI_FORMAT_D32_FLOAT_S8X24_UINT, BindFlags: D3D11_BIND_DEPTH_STENCIL.0 as u32, ..td };
            let mut depth = None;
            device.CreateTexture2D(&dd, None, Some(&mut depth)).unwrap();
            let depth = depth.unwrap();
            let mut dsv = None;
            device.CreateDepthStencilView(&depth, None, Some(&mut dsv)).unwrap();
            Harness { device, ctx, w, h, color, rtv: rtv.unwrap(), depth, dsv: dsv.unwrap() }
        }
    }

    fn target(&self, ext: f32) -> Target<'_> {
        Target { rtv: &self.rtv, dsv: &self.dsv, size: (self.w, self.h), game: (640, 480), ext, cov: None }
    }

    fn clear(&self, c: [f32; 4]) {
        unsafe {
            self.ctx.ClearRenderTargetView(&self.rtv, &c);
            self.ctx.ClearDepthStencilView(&self.dsv, (D3D11_CLEAR_DEPTH.0 | D3D11_CLEAR_STENCIL.0) as u32, 0.0, 0);
        }
    }

    /// Copy a texture to a staging one and read its rows (`bpp` bytes per texel).
    fn read(&self, tex: &ID3D11Texture2D, bpp: usize) -> Vec<u8> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            let sd = D3D11_TEXTURE2D_DESC { Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0, ..desc };
            let mut st = None;
            self.device.CreateTexture2D(&sd, None, Some(&mut st)).unwrap();
            let st = st.unwrap();
            self.ctx.CopyResource(&st, tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            let mut out = Vec::with_capacity(self.w as usize * self.h as usize * bpp);
            for y in 0..self.h as usize {
                let row = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), self.w as usize * bpp);
                out.extend_from_slice(row);
            }
            self.ctx.Unmap(&st, 0);
            out
        }
    }

    /// Colours as `[r, g, b, a]`.
    fn colors(&self) -> Vec<[u8; 4]> {
        self.read(&self.color, 4).chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
    }

    fn color_at(&self, c: &[[u8; 4]], x: u32, y: u32) -> [u8; 4] {
        c[(y * self.w + x) as usize]
    }

    /// Depth and stencil per pixel.
    fn depths(&self) -> Vec<(f32, u8)> {
        self.read(&self.depth, 8).chunks(8).map(|c| (f32::from_le_bytes([c[0], c[1], c[2], c[3]]), c[4])).collect()
    }

    /// Upload `scene` and draw every view of `frame` (as the backend does).
    fn native(&self, scene: &SceneData, frame: &FrameData, ext: f32) -> NativeRenderer {
        let mut n = NativeRenderer::new(&self.device, &self.ctx).unwrap();
        n.upload(scene).unwrap();
        n.begin_frame(frame).unwrap();
        for v in 0..frame.views.len() {
            n.draw_view(frame, v, &self.target(ext), Filter::Nearest).unwrap();
        }
        n.unbind();
        n
    }
}

/// A camera at the origin looking along world +z, view axes = world axes (x right, y down).
fn camera() -> Camera {
    Camera { pos: [0.0; 3], rot: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], proj: Projection { center: [320.0, 240.0], scale: [320.0, 320.0], near: 16.0, far: 100_000.0 } }
}

fn view() -> View {
    View { camera: camera(), viewport: [0.0, 0.0, 640.0, 480.0], mirror: false, clear: None, fog: None, sky: None }
}

/// A square in the plane z = `z`, centred at (x, y), `size` across, one material.
fn square(material: u32, x: f32, y: f32, z: f32, size: f32, color: [u8; 4]) -> Mesh {
    let h = size / 2.0;
    let v = |px: f32, py: f32, u: f32, vv: f32| Vertex { pos: [px, py, z], uv: [u, vv], color, normal: [0.0, 0.0, -1.0] };
    Mesh {
        vertices: vec![v(x - h, y - h, 0.0, 0.0), v(x + h, y - h, 1.0, 0.0), v(x + h, y + h, 1.0, 1.0), v(x - h, y + h, 0.0, 1.0)],
        indices: vec![0, 1, 2, 0, 2, 3],
        ranges: vec![MeshRange { material, indices: 0..6 }],
    }
}

fn solid(w: u32, h: u32, c: [u8; 4]) -> TextureData {
    TextureData { width: w, height: h, format: TexFormat::Rgba8, pixels: c.iter().copied().cycle().take((w * h * 4) as usize).collect(), mipmaps: Mipmaps::None }
}

// ---- depth: the native world and the Glide path share one depth buffer ----

/// The Glide path's vertex (`sr_runtime::glide::state::Vertex`, the backend's input layout).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GlideVertex {
    x: f32,
    y: f32,
    ooz: f32,
    oow: f32,
    rgba: [f32; 4],
    stw: [f32; 3],
}

/// The backend's `StateCb` (`d3d.rs`), the fields the vertex shader reads.
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
    grow: [f32; 4],
}

/// The Glide path's pipeline for a car's opaque polygons in a race: `voodoo.hlsl`'s own
/// vertex shader with the constants `d3d.rs` sets for forced depth (`tex_range.z =
/// DEPTH_SCALE`, `screen.z = 1`: w-buffer), the `ForcedDepth::Opaque` depth-stencil state
/// (GREATER_EQUAL, depth and stencil written) and `depth_raster(false, true)` (constant bias
/// 64, no depth clip). The pixel shader only paints a colour.
struct GlidePath {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    cb: ID3D11Buffer,
    ds: ID3D11DepthStencilState,
    rs: ID3D11RasterizerState,
}

impl GlidePath {
    /// `bias`: the rasterizer's constant depth bias (the backend's 64 for opaque polygons;
    /// 0 to compare the depth values alone).
    fn new(h: &Harness, bias: i32) -> GlidePath {
        let src = include_str!("../voodoo.hlsl");
        let vs_code = compile_source(src, s!("vs_main"), s!("vs_5_0")).unwrap();
        let ps_code = compile_source("float4 ps(float4 p : SV_Position) : SV_Target { return float4(1, 0, 0, 1); }", s!("ps"), s!("ps_5_0")).unwrap();
        unsafe {
            let (mut vs, mut ps) = (None, None);
            h.device.CreateVertexShader(&vs_code, None, Some(&mut vs)).unwrap();
            h.device.CreatePixelShader(&ps_code, None, Some(&mut ps)).unwrap();
            let el = |name: windows::core::PCSTR, fmt: DXGI_FORMAT, off: u32| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: fmt,
                InputSlot: 0,
                AlignedByteOffset: off,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            };
            // as d3d.rs
            let elements = [
                el(s!("POSITION"), DXGI_FORMAT_R32G32_FLOAT, 0),
                el(s!("OOZ"), DXGI_FORMAT_R32_FLOAT, 8),
                el(s!("OOW"), DXGI_FORMAT_R32_FLOAT, 12),
                el(s!("COLOR"), DXGI_FORMAT_R32G32B32A32_FLOAT, 16),
                el(s!("TEXCOORD"), DXGI_FORMAT_R32G32B32_FLOAT, 32),
            ];
            let mut layout = None;
            h.device.CreateInputLayout(&elements, &vs_code, Some(&mut layout)).unwrap();
            let cbv = StateCb { tex_range: [256.0, 256.0, DEPTH_SCALE, 0.0], screen: [2.0 / 640.0, -2.0 / 480.0, 1.0, 0.0], ..Default::default() };
            let cb = immutable_buffer(&h.device, as_bytes(&[cbv]), D3D11_BIND_CONSTANT_BUFFER, 0).unwrap();
            let op = D3D11_DEPTH_STENCILOP_DESC { StencilFailOp: D3D11_STENCIL_OP_KEEP, StencilDepthFailOp: D3D11_STENCIL_OP_KEEP, StencilPassOp: D3D11_STENCIL_OP_REPLACE, StencilFunc: D3D11_COMPARISON_ALWAYS };
            let dsd = D3D11_DEPTH_STENCIL_DESC {
                DepthEnable: true.into(),
                DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ALL,
                DepthFunc: D3D11_COMPARISON_GREATER_EQUAL,
                StencilEnable: true.into(),
                StencilReadMask: 0xff,
                StencilWriteMask: 0xff,
                FrontFace: op,
                BackFace: op,
            };
            let mut ds = None;
            h.device.CreateDepthStencilState(&dsd, Some(&mut ds)).unwrap();
            let rd = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: false.into(), ScissorEnable: true.into(), DepthBias: bias, ..Default::default() };
            let mut rs = None;
            h.device.CreateRasterizerState(&rd, Some(&mut rs)).unwrap();
            GlidePath { vs: vs.unwrap(), ps: ps.unwrap(), layout: layout.unwrap(), cb, ds: ds.unwrap(), rs: rs.unwrap() }
        }
    }

    /// Draw triangles given in view space (x right, y down, z = the game's w) as the game
    /// hands them to Glide: projected with the view's projection, `oow = 1 / w`; stencil
    /// reference `part` (a car part id).
    fn draw(&self, h: &Harness, proj: &Projection, tris: &[[f32; 3]], part: u32) {
        let verts: Vec<GlideVertex> = tris
            .iter()
            .map(|p| {
                let s = proj.project(*p).unwrap();
                GlideVertex { x: s[0], y: s[1], ooz: 0.0, oow: 1.0 / p[2], rgba: [255.0; 4], stw: [0.0, 0.0, 1.0 / p[2]] }
            })
            .collect();
        let vb = immutable_buffer(&h.device, as_bytes(&verts), D3D11_BIND_VERTEX_BUFFER, 0).unwrap();
        unsafe {
            let ctx = &h.ctx;
            ctx.OMSetRenderTargets(Some(&[Some(h.rtv.clone())]), &h.dsv);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: h.w as f32, Height: h.h as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            ctx.RSSetScissorRects(Some(&[windows::Win32::Foundation::RECT { left: 0, top: 0, right: h.w as i32, bottom: h.h as i32 }]));
            ctx.IASetInputLayout(&self.layout);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            let stride = std::mem::size_of::<GlideVertex>() as u32;
            let offset = 0u32;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(vb)), Some(&stride), Some(&offset));
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.GSSetShader(None, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.OMSetDepthStencilState(&self.ds, part);
            ctx.RSSetState(&self.rs);
            ctx.Draw(verts.len() as u32, 0);
        }
    }
}

/// A rectangle in view space at depth z (two triangles).
fn rect(x0: f32, y0: f32, x1: f32, y1: f32, z: f32) -> Vec<[f32; 3]> {
    vec![[x0, y0, z], [x1, y0, z], [x1, y1, z], [x0, y0, z], [x1, y1, z], [x0, y1, z]]
}

/// A floor (view y = `y`) from depth z0 to z1, x from -x to x.
fn floor(x: f32, y: f32, z0: f32, z1: f32) -> Vec<[f32; 3]> {
    vec![[-x, y, z0], [x, y, z0], [x, y, z1], [-x, y, z0], [x, y, z1], [-x, y, z1]]
}

/// The same triangles as a native static mesh (view space = world space here).
fn native_mesh(tris: &[[f32; 3]], material: u32) -> Mesh {
    Mesh {
        vertices: tris.iter().map(|p| Vertex { pos: *p, color: [0, 0, 255, 255], ..Default::default() }).collect(),
        indices: (0..tris.len() as u32).collect(),
        ranges: vec![MeshRange { material, indices: 0..tris.len() as u32 }],
    }
}

fn ulps(a: f32, b: f32) -> u32 {
    (a.to_bits() as i64 - b.to_bits() as i64).unsigned_abs() as u32
}

#[test]
fn glide_and_native_depth_agree() {
    let h = Harness::new(320, 240);
    let unbiased = GlidePath::new(&h, 0);
    let biased = GlidePath::new(&h, 1 << 6);
    let proj = camera().proj;
    let frame = FrameData { views: vec![view()], ..Default::default() };
    // walls facing the camera at several depths (the whole screen); floors whose depth
    // varies across them: one with its corners on the screen, one reaching far outside it
    // (corners at +-48000 game pixels, clipped by the rasterisers)
    let shapes: Vec<(&str, Vec<[f32; 3]>, u32)> = vec![
        ("wall 300", rect(-600.0, -500.0, 600.0, 500.0, 300.0), 2),
        ("wall 1000", rect(-2000.0, -1600.0, 2000.0, 1600.0, 1000.0), 2),
        ("wall 40000", rect(-80000.0, -60000.0, 80000.0, 60000.0, 40000.0), 2),
        ("floor on screen", floor(1000.0, 150.0, 1000.0, 20000.0), 16),
        ("floor beyond the screen", floor(30000.0, 150.0, 200.0, 20000.0), 16),
    ];
    for (name, tris, tolerance) in &shapes {
        let scene = SceneData { materials: vec![Material::flat()], meshes: vec![native_mesh(tris, 0)], statics: vec![Instance::new(0, Transform::IDENTITY)], ..Default::default() };
        h.clear([0.0; 4]);
        h.native(&scene, &frame, 0.0);
        let native = h.depths();
        h.clear([0.0; 4]);
        unbiased.draw(&h, &proj, tris, 0x81);
        let gl = h.depths();
        h.clear([0.0; 4]);
        biased.draw(&h, &proj, tris, 0x81);
        let gb = h.depths();
        let (mut covered, mut one_only, mut worst, mut worst_rel, mut bias_ulps) = (0, 0, 0u32, 0f64, (u32::MAX, 0u32));
        for i in 0..native.len() {
            let (n, g, b) = (native[i].0, gl[i].0, gb[i].0);
            if n == 0.0 || g == 0.0 {
                // edge pixels may be covered by one rasteriser only
                one_only += (n == 0.0) as usize ^ (g == 0.0) as usize;
                continue;
            }
            covered += 1;
            let d = ulps(n, g);
            worst = worst.max(d);
            worst_rel = worst_rel.max(((n - g) / g).abs() as f64);
            assert!(d <= *tolerance, "{name}: pixel {i}: native {n} glide {g} ({d} ulps)");
            let bu = ulps(b, g);
            bias_ulps = (bias_ulps.0.min(bu), bias_ulps.1.max(bu));
            assert!(b > g, "{name}: the Glide path's bias pulls nearer");
        }
        assert!(covered > 1000, "{name}: {covered} pixels");
        assert!(one_only < h.w as usize * 2, "{name}: {one_only} pixels covered by one path only");
        // the value itself: DEPTH_SCALE / z
        if let Some(z) = name.strip_prefix("wall ").and_then(|z| z.parse::<f32>().ok()) {
            let c = native[(h.h / 2 * h.w + h.w / 2) as usize].0;
            assert!(ulps(c, DEPTH_SCALE / z) <= 1, "{name}: {c} vs {}", DEPTH_SCALE / z);
        }
        eprintln!(
            "depth {name}: {covered} pixels, native vs Glide (no bias) at most {worst} ulps ({worst_rel:.1e} relative); the Glide path's bias adds {}..{} ulps; {one_only} edge pixels covered by one path only",
            bias_ulps.0, bias_ulps.1
        );
    }
}

#[test]
fn glide_cars_are_hidden_behind_native_walls() {
    let h = Harness::new(320, 240);
    let glide = GlidePath::new(&h, 1 << 6);
    let proj = camera().proj;
    let frame = FrameData { views: vec![view()], ..Default::default() };
    let wall = rect(-2000.0, -1600.0, 2000.0, 1600.0, 1000.0);
    let scene = SceneData { materials: vec![Material::flat()], meshes: vec![native_mesh(&wall, 0)], statics: vec![Instance::new(0, Transform::IDENTITY)], ..Default::default() };
    let centre = (h.h / 2 * h.w + h.w / 2) as usize;
    // a "car" (Glide-drawn) just in front of and just behind the native wall
    let mut threshold = None;
    for dz in [-5.0f32, -1.0, -0.1, -0.01, 0.0, 0.01, 0.1, 1.0, 5.0] {
        h.clear([0.0; 4]);
        h.native(&scene, &frame, 0.0);
        glide.draw(&h, &proj, &rect(-300.0, -300.0, 300.0, 300.0, 1000.0 + dz), 0x81);
        let c = h.colors()[centre];
        let (d, stencil) = h.depths()[centre];
        let shown = c == [255, 0, 0, 255];
        assert_eq!(shown, stencil == 0x81);
        if dz < 0.0 {
            assert!(shown, "car {dz} in front of the wall is hidden");
            assert!(ulps(d, DEPTH_SCALE / (1000.0 + dz)) <= 70);
        }
        if dz >= 0.1 {
            assert!(!shown, "car {dz} behind the wall shows");
            assert_eq!(stencil, 0, "wall pixels keep stencil 0 (scenery for the decal tests)");
        }
        if !shown && threshold.is_none() {
            threshold = Some(dz);
        }
    }
    eprintln!("hybrid: a Glide car is hidden from {:?} units behind a native wall at 1000 (the Glide path's depth bias)", threshold);
}

/// Polygons reaching behind the eye (view z <= 0), as world polygons beside the game's
/// camera do (`docs/re/native/near.md`): everything in front of the near plane is drawn,
/// exactly where the projection puts it (a wall to the right, as the frame-2100 wall; a
/// decal floor, as shadows and marks under the camera).
#[test]
fn polygons_through_the_eye_plane_are_clipped_at_the_near_plane() {
    let h = Harness::new(320, 240);
    let near = camera().proj.near;
    // view space = world space (x right, y down, z forward); target pixel = 2 game pixels
    let quad = |p: [[f32; 3]; 4]| vec![p[0], p[1], p[2], p[0], p[2], p[3]];
    // a wall at x = 200 from 500 units behind the eye to 3000 in front, y -1000..1000
    let wall = quad([[200.0, -1000.0, -500.0], [200.0, -1000.0, 3000.0], [200.0, 1000.0, 3000.0], [200.0, 1000.0, -500.0]]);
    let wall_hit = |gx: f32, gy: f32| {
        let z = 320.0 * 200.0 / (gx - 320.0);
        let y = (gy - 240.0) * z / 320.0;
        gx > 320.0 && (near..=3000.0).contains(&z) && y.abs() <= 1000.0
    };
    // a floor at y = 150 from 300 units behind the eye to 3000, x -3000..3000
    let floor = quad([[-3000.0, 150.0, -300.0], [3000.0, 150.0, -300.0], [3000.0, 150.0, 3000.0], [-3000.0, 150.0, 3000.0]]);
    let floor_hit = |gx: f32, gy: f32| {
        let z = 320.0 * 150.0 / (gy - 240.0);
        let x = (gx - 320.0) * z / 320.0;
        gy > 240.0 && (near..=3000.0).contains(&z) && x.abs() <= 3000.0
    };
    // control: the same wall from just beyond the near plane (no vertex behind the eye)
    let wall_front = quad([[200.0, -1000.0, near + 4.0], [200.0, -1000.0, 3000.0], [200.0, 1000.0, 3000.0], [200.0, 1000.0, near + 4.0]]);
    let wall_front_hit = |gx: f32, gy: f32| {
        let z = 320.0 * 200.0 / (gx - 320.0);
        let y = (gy - 240.0) * z / 320.0;
        gx > 320.0 && (near + 4.0..=3000.0).contains(&z) && y.abs() <= 1000.0
    };
    let decal = Material::decal(None, Transparency::Blended(Blend::ALPHA), 120.0);
    let cases: Vec<(&str, Material, &Vec<[f32; 3]>, &dyn Fn(f32, f32) -> bool)> = vec![
        ("wall in front of the near plane", Material::flat(), &wall_front, &wall_front_hit),
        ("wall", Material::flat(), &wall, &wall_hit),
        ("decal floor", decal, &floor, &floor_hit),
    ];
    for (name, material, tris, hit) in cases {
        let scene = SceneData { materials: vec![material], meshes: vec![native_mesh(tris, 0)], ..Default::default() };
        let frame = FrameData { views: vec![view()], dynamics: vec![Instance::new(0, Transform::IDENTITY)], ..Default::default() };
        h.clear([0.0; 4]);
        h.native(&scene, &frame, 0.0);
        let c = h.colors();
        let (mut expected, mut missing, mut extra) = (0, 0, 0);
        for y in 0..h.h {
            for x in 0..h.w {
                let drawn = h.color_at(&c, x, y)[2] == 255;
                // the reference at the pixel's centre; pixels whose neighbourhood straddles
                // an edge are left out
                let at = |dx: f32, dy: f32| hit(2.0 * (x as f32 + 0.5 + dx), 2.0 * (y as f32 + 0.5 + dy));
                let all = [(-0.6, -0.6), (0.6, -0.6), (-0.6, 0.6), (0.6, 0.6)].iter().all(|&(dx, dy)| at(dx, dy));
                let none = [(-0.6, -0.6), (0.6, -0.6), (-0.6, 0.6), (0.6, 0.6)].iter().all(|&(dx, dy)| !at(dx, dy));
                expected += all as u32;
                missing += (all && !drawn) as u32;
                extra += (none && drawn) as u32;
            }
        }
        eprintln!("{name}: {expected} pixels expected, {missing} missing, {extra} drawn outside");
        assert!(expected > 5000, "{name}: {expected}");
        assert_eq!((missing, extra), (0, 0), "{name}: the polygon must be drawn wherever it is in front of the near plane");
    }
}

// ---- the shared formulas, on the GPU, against the CPU references ----

const FORMULA_CS: &str = r#"
struct RCase { float3 e; float3 n; float2 scroll; uint mirror; };
StructuredBuffer<RCase> rin : register(t8);
StructuredBuffer<uint4> ain : register(t9);
RWStructuredBuffer<float2> rout : register(u0);
RWStructuredBuffer<uint> aout : register(u1);
[numthreads(64, 1, 1)]
void cs_reflect(uint3 id : SV_DispatchThreadID)
{
    RCase c = rin[id.x];
    rout[id.x] = reflect_texel(c.e, c.n, c.scroll, c.mirror != 0);
}
[numthreads(64, 1, 1)]
void cs_anim(uint3 id : SV_DispatchThreadID)
{
    uint4 c = ain[id.x];
    aout[id.x] = c.w == 0 ? anim_frame(c.x, c.y, c.z) : (fade_draws(c.x, c.yz) ? 1u : 0u);
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct RCase {
    e: [f32; 3],
    n: [f32; 3],
    scroll: [f32; 2],
    mirror: u32,
}

/// Run a compute shader of `FORMULA_CS` over `input` (SRV at `t_slot`), `out_size` bytes
/// per result (UAV at `u_slot`).
fn run_cs<T: Copy, R: Copy + Default>(dev: &(ID3D11Device, ID3D11DeviceContext), entry: windows::core::PCSTR, input: &[T], t_slot: u32, u_slot: u32) -> Vec<R> {
    let (device, ctx) = dev;
    let src = format!("{SHADER}\n{FORMULA_CS}");
    let code = compile_source(&src, entry, s!("cs_5_0")).unwrap();
    unsafe {
        let mut cs = None;
        device.CreateComputeShader(&code, None, Some(&mut cs)).unwrap();
        let inb = immutable_buffer(device, as_bytes(input), D3D11_BIND_SHADER_RESOURCE, std::mem::size_of::<T>() as u32).unwrap();
        let mut srv = None;
        device.CreateShaderResourceView(&inb, None, Some(&mut srv)).unwrap();
        let rsize = std::mem::size_of::<R>();
        let n = input.len();
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: (n * rsize) as u32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
            MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
            StructureByteStride: rsize as u32,
            ..Default::default()
        };
        let mut out = None;
        device.CreateBuffer(&desc, None, Some(&mut out)).unwrap();
        let out = out.unwrap();
        let mut uav = None;
        device.CreateUnorderedAccessView(&out, None, Some(&mut uav)).unwrap();
        let sdesc = D3D11_BUFFER_DESC { Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0, StructureByteStride: 0, ..desc };
        let mut st = None;
        device.CreateBuffer(&sdesc, None, Some(&mut st)).unwrap();
        let st = st.unwrap();
        ctx.CSSetShader(cs.as_ref(), None);
        ctx.CSSetShaderResources(t_slot, Some(&[srv]));
        ctx.CSSetUnorderedAccessViews(u_slot, 1, Some(&uav), None);
        ctx.Dispatch(n.div_ceil(64) as u32, 1, 1);
        ctx.CopyResource(&st, &out);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
        let mut res = vec![R::default(); n];
        std::ptr::copy_nonoverlapping(m.pData as *const R, res.as_mut_ptr(), n);
        ctx.Unmap(&st, 0);
        res
    }
}

/// Deterministic pseudo-random numbers (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.next() >> 11) as f64 / (1u64 << 53) as f64 * (hi - lo)
    }
}

fn reflection_cases() -> Vec<RCase> {
    let mut rng = Rng(0x1997_5c4e);
    let mut cases = Vec::new();
    for i in 0..60_000 {
        // view positions in front of the camera, near and far
        let z = if i % 3 == 0 { rng.f(17.0, 400.0) } else { rng.f(130.0, 60_000.0) };
        let e = [rng.f(-1.2, 1.2) * z, rng.f(-0.8, 0.8) * z, z];
        // unit normals, a few slightly off unit length (the game's matrices are rounded)
        let (a, b) = (rng.f(0.0, std::f64::consts::TAU), rng.f(-1.0, 1.0));
        let r = (1.0 - b * b).sqrt() * if i % 7 == 0 { 1.0 + rng.f(-1e-4, 1e-4) } else { 1.0 };
        let n = [r * a.cos(), b * if i % 7 == 0 { 1.0 + rng.f(-1e-4, 1e-4) } else { 1.0 }, r * a.sin()];
        cases.push(RCase {
            e: [e[0] as f32, e[1] as f32, e[2] as f32],
            n: [n[0] as f32, n[1] as f32, n[2] as f32],
            scroll: [(rng.next() % 256) as f32, (rng.next() % 256) as f32],
            mirror: (rng.next() % 4 == 0) as u32,
        });
    }
    cases
}

/// GPU results against the f64 reference: equal, or one texel apart where the reference's
/// 127 r is within 1e-3 of a half (f32 on the GPU, the x87's 64-bit mantissa in the game).
fn check_reflection(name: &str, cases: &[RCase], gpu: &[[f32; 2]]) {
    let mut near_half = 0;
    for (c, g) in cases.iter().zip(gpu) {
        let p = [c.e[0] as f64, c.e[1] as f64, c.e[2] as f64];
        let n = [c.n[0] as f64, c.n[1] as f64, c.n[2] as f64];
        let r = prepare::reflection_texel(p, n, c.scroll, c.mirror != 0);
        for k in 0..2 {
            if r[k] == g[k] as f64 {
                continue;
            }
            // the unrounded value 127 r of this component
            let raw = r[k] - 127.0 + c.scroll[k] as f64;
            let unrounded = {
                let g = [if c.mirror != 0 { p[0] } else { -p[0] }, -p[1], prepare::GAME_EYE - p[2]];
                let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
                let e = [g[0] / l, g[1] / l, g[2] / l];
                let nn = [-n[0], -n[1], -n[2]];
                let kk = 2.0 * (e[0] * nn[0] + e[1] * nn[1] + e[2] * nn[2]);
                127.0 * (kk * nn[if k == 0 { 0 } else { 2 }] - e[if k == 0 { 0 } else { 2 }])
            };
            let frac = (unrounded - unrounded.floor() - 0.5).abs();
            assert!(frac < 1e-3 && (r[k] - g[k] as f64).abs() == 1.0, "{name}: case {c:?}: reference {r:?} (raw {raw}, unrounded {unrounded}) gpu {g:?}");
            near_half += 1;
        }
    }
    eprintln!("{name}: {} reflection cases, {near_half} components one texel apart at a half (127 r within 1e-3 of k + 0.5), all others equal", cases.len());
}

impl std::fmt::Debug for RCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "e {:?} n {:?} scroll {:?} mirror {}", self.e, self.n, self.scroll, self.mirror)
    }
}

#[test]
fn reflection_shader_matches_the_reference() {
    let cases = reflection_cases();
    let w = warp();
    let gpu: Vec<[f32; 2]> = run_cs(&w, s!("cs_reflect"), &cases, 8, 0);
    check_reflection("WARP", &cases, &gpu);
    if let Some(hw) = device(D3D_DRIVER_TYPE_HARDWARE) {
        let gpu: Vec<[f32; 2]> = run_cs(&hw, s!("cs_reflect"), &cases, 8, 0);
        check_reflection("hardware", &cases, &gpu);
    }
}

#[test]
fn anim_and_fade_shaders_match_the_references() {
    let mut cases: Vec<[u32; 4]> = Vec::new();
    let mut rng = Rng(0xa11a_f1a6);
    for count in [1u32, 2, 4, 5, 6, 10, 128, 65535] {
        for shift in [14u32, 16] {
            for v in [0u32, 1, 1637, 1638, 1639, 16383, 16384, 16385, 0x7fff, 0x8000, 0xbfff, 0xc000, 0xffff, 0x10000, 0x7fff_ffff, u32::MAX] {
                cases.push([count, v, shift, 0]);
            }
            for _ in 0..500 {
                cases.push([count, rng.next() as u32 >> (rng.next() % 32), shift, 0]);
            }
        }
    }
    let anim_n = cases.len();
    for f in 0..256 {
        for y in 0..4 {
            for x in 0..4 {
                cases.push([f, x, y, 1]);
            }
        }
    }
    for (name, dev) in [("WARP", Some(warp())), ("hardware", device(D3D_DRIVER_TYPE_HARDWARE))] {
        let Some(dev) = dev else { continue };
        let out: Vec<u32> = run_cs(&dev, s!("cs_anim"), &cases, 9, 1);
        for (c, o) in cases.iter().zip(&out) {
            let want = if c[3] == 0 { prepare::anim_frame(c[0], c[1], c[2]) } else { prepare::fade_draws(c[0], c[1], c[2]) as u32 };
            assert_eq!(*o, want, "{name}: case {c:?}");
        }
        eprintln!("{name}: {anim_n} animation frames and 4096 fade pixels equal to the references");
    }
}

// ---- features through the whole pipeline ----

#[test]
fn animation_picks_the_frame_per_instance() {
    let h = Harness::new(128, 96);
    let colors = [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 0, 255]];
    // frame 2 is larger: resampled into the frames' array
    let textures: Vec<TextureData> = colors.iter().enumerate().map(|(k, c)| if k == 2 { solid(8, 8, *c) } else { solid(4, 4, *c) }).collect();
    let phase_mat = Material { anim: Some(TexAnim { first: 0, count: 4, source: AnimSource::Phase }), ..Material::opaque(0) };
    let env_mat = Material { anim: Some(TexAnim { first: 0, count: 4, source: AnimSource::Env }), ..Material::opaque(0) };
    let quad = |m: u32| square(m, 0.0, 0.0, 0.0, 200.0, [255; 4]);
    let scene = SceneData {
        textures,
        texture_frames: vec![0, 1, 2, 3],
        materials: vec![phase_mat, env_mat],
        meshes: vec![quad(0), quad(1)],
        statics: [(-700.0, 0), (-350.0, 5000), (0.0, -1), (350.0, 16383)]
            .iter()
            .map(|&(x, phase)| Instance { phase, ..Instance::new(0, Transform::at([x as f64 + 100.0, -300.0, 1000.0])) })
            .collect(),
    };
    let clock = 9000; // 4 * 9000 >> 14 = 2
    let env_phase = 0xc000; // 4 * 0xc000 >> 16 = 3
    let frame = FrameData {
        views: vec![view()],
        clock,
        env_phase,
        dynamics: vec![
            Instance { phase: 12000, ..Instance::new(0, Transform::at([-600.0, 300.0, 1000.0])) }, // -> 2
            Instance { phase: 100, ..Instance::new(1, Transform::at([-250.0, 300.0, 1000.0])) },   // env -> 3
            Instance { phase: -5, ..Instance::new(0, Transform::at([100.0, 300.0, 1000.0])) },     // clock -> 2
        ],
        meshes: vec![FrameMesh { origin: [450.0, 300.0, 1000.0], mesh: quad(0) }], // clock -> 2
        ..Default::default()
    };
    h.clear([0.0; 4]);
    let n = h.native(&scene, &frame, 0.0);
    assert_eq!(n.scene_stats().unwrap().animated, 2);
    let c = h.colors();
    // screen centre of a world point at z = 1000: 320 + 0.32 x game px; 0.2 target px per game px
    let px = |x: f32, y: f32| ((320.0 + 0.32 * x) * 0.2, (240.0 + 0.32 * y) * 0.2);
    let check = |x: f32, y: f32, frame: usize, what: &str| {
        let (sx, sy) = px(x, y);
        assert_eq!(h.color_at(&c, sx as u32, sy as u32), colors[frame], "{what}");
    };
    check(-600.0, -300.0, 0, "static phase 0");
    check(-250.0, -300.0, 1, "static phase 5000");
    check(100.0, -300.0, 2, "static phase -1: clock");
    check(450.0, -300.0, 3, "static phase 16383");
    check(-600.0, 300.0, 2, "dynamic phase 12000");
    check(-250.0, 300.0, 3, "dynamic, env source");
    check(100.0, 300.0, 2, "dynamic phase -5: clock");
    check(450.0, 300.0, 2, "frame mesh: clock");
}

#[test]
fn static_fade_dithers_and_hides() {
    let h = Harness::new(64, 48);
    let wall = square(0, 0.0, 0.0, 1000.0, 4000.0, [200, 200, 200, 255]);
    let scene = SceneData { materials: vec![Material::flat()], meshes: vec![wall], statics: vec![Instance::new(0, Transform::IDENTITY)], ..Default::default() };
    let drawn = |fades: Vec<u8>| {
        h.clear([0.0, 0.0, 0.0, 1.0]);
        let frame = FrameData { views: vec![view()], static_fade: fades, ..Default::default() };
        let n = h.native(&scene, &frame, 0.0);
        let c = h.colors();
        let mask: Vec<bool> = c.iter().map(|p| p[0] == 200).collect();
        (mask, n.last)
    };
    let all = (h.w * h.h) as usize;
    assert_eq!(drawn(Vec::new()).0.iter().filter(|&&d| d).count(), all, "no fades: all drawn");
    for f in [0u8, 1, 8, 9, 64, 128, 200, 254, 255] {
        let (mask, stats) = drawn(vec![f]);
        for (i, d) in mask.iter().enumerate() {
            let (x, y) = (i as u32 % h.w, i as u32 / h.w);
            assert_eq!(*d, prepare::fade_draws(f as u32, x, y), "fade {f}: pixel {x},{y}");
        }
        if f == 0 {
            assert_eq!(stats.chunks_visible, 0, "a chunk whose statics are all faded out is skipped");
            assert_eq!(stats.draws, 0);
        }
    }
    // two statics in one chunk fade independently; a missing entry is drawn
    let left = square(0, -1000.0, 0.0, 1000.0, 2000.0, [200, 200, 200, 255]);
    let right = square(0, 1000.0, 0.0, 1000.0, 2000.0, [200, 200, 200, 255]);
    let scene2 = SceneData { materials: vec![Material::flat()], meshes: vec![left, right], statics: vec![Instance::new(0, Transform::IDENTITY), Instance::new(1, Transform::IDENTITY)], ..Default::default() };
    for (fades, want_left, want_right) in [(vec![0u8, 255], false, true), (vec![255, 0], true, false), (vec![0], false, true)] {
        h.clear([0.0, 0.0, 0.0, 1.0]);
        let frame = FrameData { views: vec![view()], static_fade: fades.clone(), ..Default::default() };
        h.native(&scene2, &frame, 0.0);
        let c = h.colors();
        assert_eq!(h.color_at(&c, 16, 24)[0] == 200, want_left, "{fades:?} left");
        assert_eq!(h.color_at(&c, 48, 24)[0] == 200, want_right, "{fades:?} right");
    }
}

#[test]
fn sky_and_ground_fill_the_view() {
    let h = Harness::new(160, 120);
    // texel (s, t) = colour (s, t, 0): the drawn colour tells the coordinates
    let mut px = Vec::with_capacity(256 * 256 * 4);
    for t in 0..256u32 {
        for s in 0..256u32 {
            px.extend_from_slice(&[s as u8, t as u8, 0, 255]);
        }
    }
    let sky_tex = TextureData { width: 256, height: 256, format: TexFormat::Rgba8, pixels: px, mipmaps: Mipmaps::None };
    let scene = SceneData { textures: vec![sky_tex], ..Default::default() };
    let sky = Sky { texture: 0, s: [0.0, 128.0], t: [0.0, 240.0], horizon: 240.0, ground: Some(0x204060) };
    let draw = |v: View, ext: f32| {
        h.clear([0.0; 4]);
        h.native(&scene, &FrameData { views: vec![v], ..Default::default() }, ext);
        h.colors()
    };
    // plain view: 640 game px over 160 target px, s from 0 to 128 (0.8 texels per target px)
    let c = draw(View { sky: Some(sky), ..view() }, 0.0);
    let at = |c: &[[u8; 4]], x: u32, y: u32| h.color_at(c, x, y);
    let tl = at(&c, 0, 0);
    assert!(tl[0] == 0 && tl[1] <= 2, "top left: s 0, t 0: {tl:?}");
    assert_eq!(at(&c, 80, 30)[0], 64, "middle: s 64");
    assert!((at(&c, 159, 59)[0] as i32 - 127).abs() <= 1, "right edge: s 128 {:?}", at(&c, 159, 59));
    assert!((at(&c, 80, 59)[1] as i32 - 238).abs() <= 2, "just above the horizon: t 240 {:?}", at(&c, 80, 59));
    assert_eq!(at(&c, 80, 61), [0x20, 0x40, 0x60, 255], "below the horizon: ground");
    assert_eq!(at(&c, 0, 119), [0x20, 0x40, 0x60, 255]);
    // widened by 160 game px on each side (the target covers 960 game px): s extends
    // proportionally, -32 .. 160 (wrapped: 224 at the left edge)
    let c = draw(View { sky: Some(sky), ..view() }, 160.0);
    assert_eq!(at(&c, 0, 10)[0], 224, "wide left edge {:?}", at(&c, 0, 10));
    assert_eq!(at(&c, 80, 10)[0], 64, "wide middle");
    // mirror view: s as given (the game's pair already swapped), not flipped
    let m = View { mirror: true, viewport: [160.0, 0.0, 480.0, 240.0], sky: Some(Sky { s: [128.0, 0.0], horizon: 120.0, ..sky }), ..view() };
    let c = draw(m, 0.0);
    assert!((at(&c, 40, 5)[0] as i32 - 128).abs() <= 1, "mirror left edge {:?}", at(&c, 40, 5));
    assert!(at(&c, 119, 5)[0] <= 1, "mirror right edge {:?}", at(&c, 119, 5));
    assert_eq!(at(&c, 80, 40), [0x20, 0x40, 0x60, 255], "mirror ground");
    assert_eq!(at(&c, 20, 5), [0, 0, 0, 0], "outside the mirror viewport: untouched");
}

#[test]
fn decal_multiply_and_mode_3_blend() {
    let h = Harness::new(64, 48);
    let back = square(0, 0.0, 0.0, 2000.0, 8000.0, [100, 150, 200, 255]);
    let front = |m: u32| square(m, 0.0, 0.0, 1000.0, 4000.0, [64, 128, 255, 255]);
    let multiply = Material::decal(None, Transparency::Blended(Blend { src: BlendFactor::DstColor, dst: BlendFactor::SrcColor }), 10.0);
    let mode3 = Material::blended(None, Blend { src: BlendFactor::One, dst: BlendFactor::SrcColor });
    let scene = SceneData {
        materials: vec![Material::flat(), multiply, mode3],
        meshes: vec![back, front(1), front(2)],
        statics: vec![Instance::new(0, Transform::IDENTITY)],
        ..Default::default()
    };
    let (dst, src) = ([100.0f32, 150.0, 200.0], [64.0f32, 128.0, 255.0]);
    for (mesh, f) in [(1u32, Box::new(|s: f32, d: f32| 2.0 * s * d) as Box<dyn Fn(f32, f32) -> f32>), (2, Box::new(|s: f32, d: f32| s + s * d))] {
        h.clear([0.0; 4]);
        let frame = FrameData { views: vec![view()], dynamics: vec![Instance::new(mesh, Transform::IDENTITY)], ..Default::default() };
        h.native(&scene, &frame, 0.0);
        let c = h.color_at(&h.colors(), 32, 24);
        for k in 0..3 {
            let want = (f(src[k] / 255.0, dst[k] / 255.0).min(1.0) * 255.0).round();
            assert!((c[k] as f32 - want).abs() <= 1.0, "mesh {mesh} channel {k}: {} vs {want}", c[k]);
        }
    }
}

/// Wave 3's depth rules for moving objects (renderer.rs `ST_*`): decals are pulled over
/// statics only; blended surfaces of moving instances (car glass, overlays) lie over the
/// moving instances' reflective surfaces up to `OVERLAY_WINDOW_PULL` in front of them, over
/// anything else up to `OVERLAY_PULL`; their opaque texels then hide what is behind them
/// from the decals.
#[test]
fn moving_objects_depth_rules() {
    let h = Harness::new(64, 48);
    let (grey, red, blue, green, yellow) = ([50, 50, 50, 255], [200, 0, 0, 255], [0, 0, 200, 255], [0, 255, 0, 255], [255, 255, 0, 255]);
    let window = Material { texture: Some(0), texgen: TexGen::Reflection { scale: [1.0, 1.0], offset: [0.0, 0.0] }, ..Material::opaque(0) };
    let scene = SceneData {
        textures: vec![solid(4, 4, blue)],
        materials: vec![
            Material::flat(),
            window,
            Material::decal(None, Transparency::Blended(Blend::ALPHA), 250.0),
            Material::blended(None, Blend::ALPHA),
        ],
        // 0 background, 1 static square, 2 body, 3 window, 4 decal, 5 overlay (each at z 0)
        meshes: vec![
            square(0, 0.0, 0.0, 4000.0, 20000.0, grey),
            square(0, -1600.0, 0.0, 2000.0, 400.0, grey),
            square(0, 0.0, 0.0, 0.0, 400.0, red),
            square(1, 0.0, 0.0, 0.0, 400.0, blue),
            square(2, 0.0, 0.0, 0.0, 300.0, green),
            square(3, 0.0, 0.0, 0.0, 300.0, yellow),
        ],
        statics: vec![Instance::new(0, Transform::IDENTITY), Instance::new(1, Transform::IDENTITY)],
        ..Default::default()
    };
    let at = |mesh: u32, x: f64, z: f64| Instance::new(mesh, Transform::at([x, 0.0, z]));
    let dynamics = vec![
        // A: a decal 100 units behind a static: pulled over it (shown)
        at(4, -1600.0, 2100.0),
        // B: a decal 100 units behind a moving body: hidden
        at(2, -800.0, 2000.0),
        at(4, -800.0, 2100.0),
        // C: an overlay 40 units behind a moving window: shown
        at(3, 0.0, 2000.0),
        at(5, 0.0, 2040.0),
        // D: an overlay 40 units behind a moving body: hidden; 0.5 behind another: shown
        at(2, 800.0, 2000.0),
        at(5, 800.0, 2040.0),
        // E: an overlay with nothing of its object behind it, a decal 100 units behind the
        // overlay: the overlay's opaque texels keep the decal off
        at(5, 1600.0, 2000.0),
        at(4, 1600.0, 2100.0),
    ];
    h.clear([0.0; 4]);
    let frame = FrameData { views: vec![view()], dynamics, ..Default::default() };
    h.native(&scene, &frame, 0.0);
    let c = h.colors();
    // screen x = 320 + 320 x / z (game pixels), the target a tenth of that
    let px = |x: f64| ((320.0 + 320.0 * x / 2000.0) / 10.0) as u32;
    let rgb = |p: [u8; 4]| [p[0], p[1], p[2]];
    assert_eq!(rgb(h.color_at(&c, px(-1600.0), 24)), [0, 255, 0], "A: decal over a static");
    assert_eq!(rgb(h.color_at(&c, px(-800.0), 24)), [200, 0, 0], "B: decal behind a moving body");
    assert_eq!(rgb(h.color_at(&c, px(0.0), 24)), [255, 255, 0], "C: overlay over a moving window");
    assert_eq!(rgb(h.color_at(&c, px(800.0), 24)), [200, 0, 0], "D: overlay behind a moving body");
    assert_eq!(rgb(h.color_at(&c, px(1600.0), 24)), [255, 255, 0], "E: decal behind an overlay");
    // stencil classes: statics 0, moving bodies 80h, their windows C0h, an overlay's opaque
    // texels 80h
    let d = h.depths();
    let st = |x: u32| d[(24 * h.w + x) as usize].1;
    assert_eq!((st(px(-1600.0)), st(px(-800.0)), st(px(0.0)), st(px(1600.0))), (0, 0x80, 0xc0, 0x80));
    // D': 0.5 units behind a body the overlay shows (coplanar overlays)
    let scene2 = scene.clone();
    h.clear([0.0; 4]);
    let frame = FrameData { views: vec![view()], dynamics: vec![at(2, 800.0, 2000.0), at(5, 800.0, 2000.5)], ..Default::default() };
    h.native(&scene2, &frame, 0.0);
    assert_eq!(rgb(h.color_at(&h.colors(), px(800.0), 24)), [255, 255, 0], "D': overlay just behind a moving body");
}

#[test]
fn smoke_follows_the_games_order_over_moving_objects() {
    // FrameData::dynamic_order: a puff covers the moving objects of its first place or
    // farther whatever the depth, never nearer ones (docs/re/native/dynamics.md, "Smoke")
    let h = Harness::new(64, 48);
    let (red, green) = ([200, 0, 0, 255], [0, 255, 0, 255]);
    let scene = SceneData {
        materials: vec![Material::flat(), Material::decal(None, Transparency::Blended(Blend::ALPHA), 250.0)],
        meshes: vec![square(0, 0.0, 0.0, 0.0, 400.0, red), square(1, 0.0, 0.0, 0.0, 300.0, green)],
        ..Default::default()
    };
    let at = |mesh: u32, x: f64, z: f64| Instance::new(mesh, Transform::at([x, 0.0, z]));
    let dynamics = vec![
        // F: a car (place 2) and a puff 300 units behind its surface whose order covers it
        at(0, -800.0, 2000.0),
        // N: a car (place 1) and a puff 50 units in front of its surface that the order puts
        // behind it (the car's position is nearer than the puff's)
        at(0, 800.0, 1800.0),
        at(1, -800.0, 2300.0),
        at(1, 800.0, 1750.0),
    ];
    let px = |x: f64, z: f64| ((320.0 + 320.0 * x / z) / 10.0) as u32;
    let rgb = |p: [u8; 4]| [p[0], p[1], p[2]];
    for (order, want_f, want_n) in [(vec![2u8, 1, 1, 2], [0, 255, 0], [200, 0, 0]), (Vec::new(), [200, 0, 0], [0, 255, 0])] {
        h.clear([0.0; 4]);
        let frame = FrameData { views: vec![view()], dynamics: dynamics.clone(), dynamic_order: order.clone(), ..Default::default() };
        h.native(&scene, &frame, 0.0);
        let c = h.colors();
        assert_eq!(rgb(h.color_at(&c, px(-800.0, 2000.0), 24)), want_f, "F, order {order:?}");
        assert_eq!(rgb(h.color_at(&c, px(800.0, 1800.0), 24)), want_n, "N, order {order:?}");
        if !order.is_empty() {
            // the cars' pixels carry their places
            let d = h.depths();
            let st = |x: u32| d[(24 * h.w + x) as usize].1;
            assert_eq!((st(px(-800.0, 2000.0)), st(px(800.0, 1800.0))), (0x82, 0x81));
        }
    }
}

#[test]
fn smoke_stands_on_the_ground() {
    // Layer::Decal::stand: a camera-facing card (600 across, centred on its origin 20 units
    // over the ground) is not cut by the ground in front of its lower half; a wall in front
    // still hides it. The game's convention: world y up, view y down; the eye at the origin.
    let h = Harness::new(320, 240);
    let mut v = view();
    v.camera.rot = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0];
    let ground = native_mesh(&world_box([-30000.0, 30000.0], [-600.0, -500.0], [600.0, 40000.0]), 0);
    let wall = native_mesh(&world_box([-2000.0, 2000.0], [-2000.0, 2000.0], [1500.0, 1600.0]), 0);
    let card = square(1, 0.0, 0.0, 0.0, 600.0, [0, 255, 0, 255]);
    let decal = |stand: Option<f32>| Material { layer: Layer::Decal { pull: 250.0, stand }, ..Material::blended(None, Blend::ALPHA) };
    // a point of the card's lower half: world (0, -700) at z 4000 (the ray meets the ground
    // at z 2857, 1143 units in front of the card)
    let (x, y) = (160, ((240.0 + 320.0 * 700.0 / 4000.0) * 0.5) as u32);
    for (stand, with_wall, want) in [(Some(20.0), false, [0, 255, 0]), (None, false, [0, 0, 255]), (Some(20.0), true, [0, 0, 255])] {
        let mut statics = vec![Instance::new(0, Transform::IDENTITY)];
        if with_wall {
            statics.push(Instance::new(2, Transform::IDENTITY));
        }
        let scene = SceneData { materials: vec![Material::flat(), decal(stand)], meshes: vec![ground.clone(), card.clone(), wall.clone()], statics, ..Default::default() };
        h.clear([0.0; 4]);
        let frame = FrameData { views: vec![v.clone()], dynamics: vec![Instance::new(1, Transform::at([0.0, -480.0, 4000.0]))], ..Default::default() };
        h.native(&scene, &frame, 0.0);
        let c = h.color_at(&h.colors(), x, y);
        assert_eq!([c[0], c[1], c[2]], want, "stand {stand:?}, wall {with_wall}");
    }
}

#[test]
fn smoke_dissolves_between_steps() {
    // FrameData::dynamic_dissolve: (1 - t) of the earlier step's frame with t of the later
    // one's, alpha weighted; nothing where the object is not there at a step
    let h = Harness::new(128, 96);
    let anim = Some(TexAnim { first: 0, count: 2, source: AnimSource::Phase });
    let scene = SceneData {
        textures: vec![solid(4, 4, [255, 0, 0, 255]), solid(4, 4, [0, 255, 0, 255])],
        texture_frames: vec![0, 1],
        materials: vec![Material { anim, ..Material::decal(Some(0), Transparency::Blended(Blend::ALPHA), 100.0) }],
        meshes: vec![square(0, 0.0, 0.0, 0.0, 300.0, [255; 4])],
        ..Default::default()
    };
    let (f0, f1) = (0, 8192); // frames 0 (red) and 1 (green)
    let cases = [
        (Dissolve { from: Some(f0), to: Some(f1), t: 0.25 }, [191, 64, 0]),
        (Dissolve { from: None, to: Some(f1), t: 0.25 }, [0, 64, 0]),
        (Dissolve { from: Some(f0), to: None, t: 0.25 }, [191, 0, 0]),
        (Dissolve::NONE, [0, 255, 0]),
    ];
    let xs = [-600.0, -200.0, 200.0, 600.0];
    let dynamics = xs.iter().map(|&x| Instance { phase: f1, ..Instance::new(0, Transform::at([x, 0.0, 1000.0])) }).collect();
    h.clear([0.0, 0.0, 0.0, 1.0]);
    let frame = FrameData { views: vec![view()], dynamics, dynamic_dissolve: cases.iter().map(|c| c.0).collect(), ..Default::default() };
    h.native(&scene, &frame, 0.0);
    let c = h.colors();
    for (k, &x) in xs.iter().enumerate() {
        let p = h.color_at(&c, ((320.0 + 0.32 * x) * 0.2) as u32, 48);
        let want = cases[k].1;
        assert!((0..3).all(|i| (p[i] as i32 - want[i]).abs() <= 2), "case {k}: {p:?}, want {want:?}");
    }
}

#[test]
fn reflection_through_the_pipeline() {
    // a camera-facing square far away: every corner's reflection is about straight back
    // (u 127, v 0, minus the scroll); the env map's texel there is drawn
    let h = Harness::new(320, 240);
    let mut px = Vec::with_capacity(256 * 256 * 4);
    for t in 0..256u32 {
        for s in 0..256u32 {
            px.extend_from_slice(&[s as u8, t as u8, 7, 255]);
        }
    }
    let env = TextureData { width: 256, height: 256, format: TexFormat::Rgba8, pixels: px, mipmaps: Mipmaps::None };
    let m = Material { wrap: [Wrap::Repeat; 2], texgen: TexGen::Reflection { scale: [1.0, 1.0], offset: [0.0, 0.0] }, ..Material::opaque(0) };
    // 4000 units across at 50000: u runs from 122 to 132 across it (v stays 0: r.z rounds
    // to -127 at every corner), 0.8 texels per target pixel
    let mut q = square(0, 0.0, 0.0, 0.0, 4000.0, [255; 4]);
    for v in &mut q.vertices {
        v.normal = [0.0, 0.0, -1.0];
    }
    let scene = SceneData { textures: vec![env], materials: vec![m], meshes: vec![q], ..Default::default() };
    for (scroll, want) in [([0.0f32, 0.0], [127u8, 0]), ([7.0, 3.0], [120, 253])] {
        h.clear([0.0; 4]);
        let frame = FrameData { views: vec![view()], dynamics: vec![Instance { scroll, ..Instance::new(0, Transform::at([0.0, 0.0, 50000.0])) }], ..Default::default() };
        h.native(&scene, &frame, 0.0);
        let c = h.color_at(&h.colors(), 160, 120);
        // v exact (scrolled and wrapped), u interpolated across the square: within a texel
        assert!(c[1] == want[1] && c[2] == 7 && (c[0] as i32 - want[0] as i32).abs() <= 1, "scroll {scroll:?}: {c:?}");
    }
}

/// A box in world space (y up): x0..x1, y0..y1 (height), z0..z1, its 12 triangles.
fn world_box(x: [f32; 2], y: [f32; 2], z: [f32; 2]) -> Vec<[f32; 3]> {
    let c = |i: usize| [x[i & 1], y[(i >> 1) & 1], z[i >> 2]];
    let quads = [[0, 1, 3, 2], [4, 6, 7, 5], [0, 2, 6, 4], [1, 5, 7, 3], [0, 4, 5, 1], [2, 3, 7, 6]];
    quads.iter().flat_map(|q| [c(q[0]), c(q[1]), c(q[2]), c(q[0]), c(q[2]), c(q[3])]).collect()
}

#[test]
fn backing_test_counts_bases_against_the_world() {
    // the game's convention: world y up, the camera's view y down (a reflection, like the
    // game's cameras); the eye at the origin looking along +z
    let h = Harness::new(320, 240);
    let mut v = view();
    v.camera.rot = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0];
    let ground = world_box([-30000.0, 30000.0], [-600.0, -500.0], [600.0, 40000.0]);
    let standing = world_box([-300.0, 300.0], [-500.0, 1500.0], [8000.0, 8600.0]);
    let floating = world_box([-300.0, 300.0], [3000.0, 5000.0], [8000.0, 8600.0]);
    let hidden = world_box([3500.0, 6000.0], [-500.0, 1000.0], [6000.0, 6500.0]);
    let occluder = world_box([1000.0, 3000.0], [-500.0, 2000.0], [2000.0, 2100.0]);
    let backdrop = world_box([-3000.0, 3000.0], [2000.0, 20000.0], [20000.0, 20100.0]);
    let meshes: Vec<Mesh> = [&ground, &standing, &floating, &hidden, &occluder, &backdrop].iter().map(|t| native_mesh(t, 0)).collect();
    let statics = (0..meshes.len() as u32).map(|i| Instance::new(i, Transform::IDENTITY)).collect();
    let scene = SceneData { materials: vec![Material::flat()], meshes, statics, ..Default::default() };
    let t12 = BACKING_TEST1 | BACKING_TEST2;
    // ground and occluder: the base (pass 0); the backdrop drawn in pass 1 (tested against
    // pass 0); the three candidates not drawn, tested against both
    let frame = FrameData {
        views: vec![v],
        static_fade: vec![255, 0, 0, 0, 255, 255],
        static_backing: vec![0, 2 | t12, 2 | t12, 2 | t12, 0, 1 | BACKING_TEST1],
        backing_tag: 77,
        ..Default::default()
    };
    h.clear([0.0, 0.0, 0.0, 1.0]);
    let mut n = h.native(&scene, &frame, 0.0);
    let r = n.backing_results(true);
    assert_eq!(r.len(), 1);
    assert_eq!((r[0].tag, r[0].view), (77, 0));
    let c: std::collections::HashMap<u32, BackingCounts> = r[0].statics.iter().copied().collect();
    assert_eq!(c.keys().copied().collect::<std::collections::BTreeSet<u32>>(), [1, 2, 3, 5].into_iter().collect());
    let (st, fl, hi, bd) = (c[&1], c[&2], c[&3], c[&5]);
    eprintln!("standing {st:?}\nfloating {fl:?}\nhidden {hi:?}\nbackdrop {bd:?}");
    // standing on the ground: its base in front of the ground, nothing behind it is sky
    assert!(st.area > 0 && st.visible[0] == st.area && st.sky[0] == 0, "{st:?}");
    assert_eq!(st.tested, 3);
    // floating above the horizon: its whole base against the sky at test 1; at test 2 the
    // backdrop (pass 1) is behind it
    assert!(fl.area > 0 && fl.visible[0] == fl.area && fl.sky[0] == fl.visible[0], "{fl:?}");
    assert!(fl.visible[1] == fl.area && fl.sky[1] == 0, "{fl:?}");
    // behind the occluder: on screen, not visible
    assert!(hi.area > 0 && hi.visible[0] == 0 && hi.sky[0] == 0, "{hi:?}");
    // the backdrop: tested once (drawn in pass 1), its base against the sky
    assert_eq!(bd.tested, 1);
    assert!(bd.area > 0 && bd.sky[0] == bd.visible[0] && bd.visible[1] == 0, "{bd:?}");
    // the passes drew what they should: the ground and the backdrop, not the candidates
    let px = h.colors();
    let blue = |x: u32, y: u32| h.color_at(&px, x, y) == [0, 0, 255, 255];
    // the floating box's centre (y 4000 at z 8000: screen y 120 - 160 * 4000 / 8000 = 40, on
    // the 320 x 240 target) shows the backdrop behind it, drawn in pass 1
    assert!(blue(160, 40), "backdrop drawn");
    // a second frame without the backing marks: no results, every static drawn
    let frame2 = FrameData { views: frame.views.clone(), ..Default::default() };
    n.begin_frame(&frame2).unwrap();
    n.draw_view(&frame2, 0, &h.target(0.0), Filter::Nearest).unwrap();
    assert!(n.backing_results(true).is_empty());
}

#[test]
fn moving_objects_are_tested_and_faded() {
    // the statics' backing test's world (y up, the eye at the origin looking along +z): the
    // ground and an occluder in the base; three moving boxes: standing on the ground,
    // floating above the horizon, behind the occluder
    let h = Harness::new(320, 240);
    let mut v = view();
    v.camera.rot = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0];
    let ground = world_box([-30000.0, 30000.0], [-600.0, -500.0], [600.0, 40000.0]);
    let occluder = world_box([1000.0, 3000.0], [-500.0, 2000.0], [2000.0, 2100.0]);
    let standing = world_box([-300.0, 300.0], [-500.0, 500.0], [8000.0, 8600.0]);
    let floating = world_box([-300.0, 300.0], [3000.0, 5000.0], [8000.0, 8600.0]);
    let hidden = world_box([3500.0, 6000.0], [-500.0, 1000.0], [6000.0, 6500.0]);
    let meshes: Vec<Mesh> = [&ground, &occluder, &standing, &floating, &hidden].iter().map(|t| native_mesh(t, 0)).collect();
    let scene = SceneData { materials: vec![Material::flat()], meshes, statics: vec![Instance::new(0, Transform::IDENTITY), Instance::new(1, Transform::IDENTITY)], ..Default::default() };
    // bases: the lowest 30 % of each box
    let base = |lo: f64, hi: f64, object: u32| DynamicBacking { object, base_top: lo + 0.3 * (hi - lo) };
    let frame = |fade: Vec<u8>| FrameData {
        views: vec![v.clone()],
        static_fade: vec![255, 255],
        static_backing: vec![0, 0],
        backing_tag: 5,
        dynamics: (2..5).map(|m| Instance::new(m, Transform::IDENTITY)).collect(),
        dynamic_fade: fade,
        dynamic_backing: vec![base(-500.0, 500.0, 0), base(3000.0, 5000.0, 1), base(-500.0, 1000.0, 2)],
        ..Default::default()
    };
    h.clear([0.0, 0.0, 0.0, 1.0]);
    let f = frame(vec![255, 255, 255]);
    let mut n = h.native(&scene, &f, 0.0);
    let r = n.backing_results(true);
    assert_eq!(r.len(), 1);
    assert!(r[0].statics.is_empty() && r[0].tag == 5);
    let c: std::collections::HashMap<u32, BackingCounts> = r[0].dynamics.iter().copied().collect();
    let (st, fl, hi) = (c[&0], c[&1], c[&2]);
    eprintln!("standing {st:?}\nfloating {fl:?}\nhidden {hi:?}");
    assert!([st, fl, hi].iter().all(|c| c.tested == 1));
    // tested before the moving objects are drawn: the standing box's base in front of the
    // ground, the floating one's against the sky, the hidden one's behind the occluder
    assert!(st.area > 0 && st.visible[0] == st.area && st.sky[0] == 0, "{st:?}");
    assert!(fl.area > 0 && fl.visible[0] == fl.area && fl.sky[0] == fl.area, "{fl:?}");
    assert!(hi.area > 0 && hi.visible[0] == 0 && hi.sky[0] == 0, "{hi:?}");
    // only the base: the floating box's front and back faces (no culling) would count about
    // 2 x 40 rows x 12 columns, its base (30 % of its height) about a third of that
    let whole = 2.0 * (160.0 * 2000.0 / 8000.0f32) * (160.0 * 600.0 / 8000.0);
    assert!((fl.area as f32) > 0.2 * whole && (fl.area as f32) < 0.45 * whole, "{fl:?}");
    // drawn: the floating box's centre (y 4000: screen y 40) is blue; with a fade of 0 the
    // sky shows there, the other boxes unchanged
    let px = h.colors();
    assert_eq!(h.color_at(&px, 160, 40), [0, 0, 255, 255]);
    h.clear([0.0, 0.0, 0.0, 1.0]);
    let f = frame(vec![255, 0, 255]);
    n.begin_frame(&f).unwrap();
    n.draw_view(&f, 0, &h.target(0.0), Filter::Nearest).unwrap();
    n.unbind();
    let px = h.colors();
    assert_eq!(h.color_at(&px, 160, 40), [0, 0, 0, 255], "faded out");
    // the standing box (y 0 at z 8000: screen y 120) still drawn in front of the ground
    assert_eq!(h.color_at(&px, 160, 118), [0, 0, 255, 255]);
    // its results: the same counts (fades do not change the test)
    let r = n.backing_results(true);
    assert_eq!(r[0].dynamics.iter().find(|d| d.0 == 1).unwrap().1, fl);
    // a frame without the moving objects' marks: no results for them
    let f2 = FrameData { dynamic_backing: Vec::new(), ..f };
    n.begin_frame(&f2).unwrap();
    n.draw_view(&f2, 0, &h.target(0.0), Filter::Nearest).unwrap();
    assert!(n.backing_results(true).iter().all(|r| r.dynamics.is_empty()));
}

/// A full-width view (sky and ground) covers the whole target, its last column and row
/// included, at the window sizes and shapes people use, with the viewport the runtime
/// actually sends (the game's fitted tables: y from 1.9e-6 to 479.99994): the view's
/// rectangle in target pixels comes out a hair short of the edges in f32 (1279.9999 x
/// 719.9999 at 1280 x 720 with the widescreen extension), and Direct3D clips to the
/// viewport's whole pixels, which left the last column and row undrawn.
#[test]
fn full_width_view_reaches_every_edge() {
    let tex = solid(4, 4, [200, 100, 50, 255]);
    let scene = SceneData { textures: vec![tex], ..Default::default() };
    let sky = Sky { texture: 0, s: [0.0, 4.0], t: [0.0, 4.0], horizon: 240.0, ground: Some(0x204060) };
    let dev = warp();
    let hw = device(D3D_DRIVER_TYPE_HARDWARE);
    for (w, h) in [(1280, 720), (1920, 1080), (1366, 768), (2560, 1080), (1024, 768), (960, 720), (1600, 900)] {
        let ext = crate::display::extension((640, 480), crate::display::Aspect::Auto, (w, h));
        for viewport in [[0.0, 1.907_348_6e-6, 640.0, 479.999_94], [0.0, 0.0, 640.0, 480.0]] {
            for (name, d) in std::iter::once(("WARP", dev.clone())).chain(hw.clone().map(|d| ("hardware", d))) {
                let hs = Harness::on(d, w, h);
                hs.clear([0.0; 4]);
                hs.native(&scene, &FrameData { views: vec![View { sky: Some(sky), viewport, ..view() }], ..Default::default() }, ext);
                let c = hs.colors();
                let undrawn: Vec<(u32, u32)> = (0..h).flat_map(|y| [(0, y), (w - 1, y)]).chain((0..w).flat_map(|x| [(x, 0), (x, h - 1)])).filter(|&(x, y)| hs.color_at(&c, x, y)[3] == 0).collect();
                assert!(undrawn.is_empty(), "{name} {w} x {h}, ext {ext}, viewport {viewport:?}: {} edge pixels undrawn, e.g. {:?}", undrawn.len(), &undrawn[..undrawn.len().min(4)]);
            }
        }
    }
}

/// The viewport moves only an end that falls short of a whole pixel, by as little as f32
/// allows; anything else keeps its exact values (the projection of those views is
/// unchanged).
#[test]
fn viewport_ends_reach_whole_pixels() {
    use super::renderer::viewport_in_target;
    let ext = crate::display::extension((640, 480), crate::display::Aspect::Auto, (1280, 720));
    let sx = 1280.0 / (640.0 + 2.0 * ext);
    let vp = prepare::widen([0.0, 1.907_348_6e-6, 640.0, 479.999_94], ext);
    let v = viewport_in_target(vp, ext, (sx, 1.5));
    // before: 1279.9999 and 719.9999 (the start edges as they were)
    assert_eq!((v.TopLeftX, v.TopLeftY), ((vp[0] + ext) * sx, vp[1] * 1.5));
    assert!(v.TopLeftX + v.Width >= 1280.0 && v.TopLeftY + v.Height >= 720.0, "{} {}", v.TopLeftX + v.Width, v.TopLeftY + v.Height);
    assert!(v.Width - (vp[2] - vp[0]) * sx <= 2.5e-4 && v.Height - (vp[3] - vp[1]) * 1.5 <= 2.5e-4);
    // whole ends and the mirror's half-pixel rectangle: untouched
    let exact = viewport_in_target([0.0, 0.0, 640.0, 480.0], 0.0, (1.5, 1.5));
    assert_eq!((exact.TopLeftX, exact.TopLeftY, exact.Width, exact.Height), (0.0, 0.0, 960.0, 720.0));
    let m = [161.0, 33.6, 481.0, 117.6];
    let mv = viewport_in_target(m, ext, (sx, 1.5));
    assert_eq!((mv.TopLeftX, mv.TopLeftY, mv.Width, mv.Height), ((m[0] + ext) * sx, m[1] * 1.5, (m[2] - m[0]) * sx, (m[3] - m[1]) * 1.5));
}

#[test]
fn shader_vertex_is_32_bytes() {
    assert_eq!(std::mem::size_of::<GpuVertex>(), 32);
}

/// What each feature costs (`cargo test --release --offline -p sr-render feature_costs --
/// --ignored --nocapture`): the `SR_NATIVE_TEST` scene at 1920 x 1080 on the hardware
/// device, with every feature on and with each one off (`test_scene::Off`), interleaved in
/// rounds so that drift and other load on the machine spread over all configurations. Per
/// frame: the CPU time to build the frame description, the CPU time to submit it
/// (`begin_frame` and both views), and the GPU time of both views (timestamp queries).
/// No present, no game.
#[test]
#[ignore]
fn feature_costs() {
    use super::test_scene::{Off, TestScene};
    use super::timing::GpuTimer;
    let Some(dev) = device(D3D_DRIVER_TYPE_HARDWARE) else {
        eprintln!("no hardware device");
        return;
    };
    let h = Harness::on(dev, 1920, 1080);
    let all = Off::default();
    let configs: Vec<(&str, Off, bool)> = vec![
        ("all features", all, true),
        ("fade off", Off { fade: true, ..all }, true),
        ("anim off", Off { anim: true, ..all }, true),
        ("reflect off", Off { reflect: true, ..all }, true),
        ("sky off", Off { sky: true, ..all }, true),
        ("multiply off", Off { multiply: true, ..all }, true),
        ("all off", Off { fade: true, anim: true, reflect: true, sky: true, multiply: true }, true),
        ("no flush after view 0", all, false),
    ];
    let ext = (1920.0 / 1080.0 * 480.0 - 640.0) / 2.0;
    let mut runs: Vec<(TestScene, NativeRenderer, GpuTimer, Vec<f64>, Vec<f64>, Vec<f64>)> = configs
        .iter()
        .map(|(_, off, flush)| {
            let scene = TestScene::build_with(*off);
            let mut n = NativeRenderer::new(&h.device, &h.ctx).unwrap();
            n.upload(&scene.scene).unwrap();
            n.flush_after_main = *flush;
            (scene, n, GpuTimer::new(&h.device).unwrap(), Vec::new(), Vec::new(), Vec::new())
        })
        .collect();
    let mut frame = FrameData::default();
    let (rounds, per_round) = (25, 200);
    let count = runs.len();
    for round in 0..rounds {
        // a different configuration first in each round (order effects spread evenly)
        for k in 0..count {
            let (scene, n, timer, build, submit, gpu) = &mut runs[(k + round) % count];
            for f in 0..per_round {
                let t = (round * per_round + f) as f64 * 0.02;
                let b0 = std::time::Instant::now();
                scene.frame(t, &mut frame);
                let b1 = std::time::Instant::now();
                timer.begin(&h.ctx);
                n.begin_frame(&frame).unwrap();
                for v in 0..frame.views.len() {
                    n.draw_view(&frame, v, &h.target(ext), Filter::Nearest).unwrap();
                }
                timer.end(&h.ctx);
                let b2 = std::time::Instant::now();
                unsafe { h.ctx.Flush() };
                if round > 0 {
                    build.push((b1 - b0).as_secs_f64() * 1000.0);
                    submit.push((b2 - b1).as_secs_f64() * 1000.0);
                }
                let g = timer.poll(&h.ctx);
                if round > 0 {
                    gpu.extend(g);
                }
            }
            // drain this configuration's queries before the next one runs
            let (t0, mut idle) = (std::time::Instant::now(), 0);
            while t0.elapsed().as_secs_f64() < 2.0 && idle < 5 {
                let g = timer.poll(&h.ctx);
                if g.is_empty() {
                    idle += 1;
                    std::thread::sleep(std::time::Duration::from_millis(2));
                } else {
                    idle = 0;
                    if round > 0 {
                        gpu.extend(g);
                    }
                }
            }
        }
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        if v.is_empty() { 0.0 } else { v[v.len() / 2] }
    };
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    eprintln!("feature costs at 1920 x 1080, {} rounds x {per_round} frames per configuration (first round dropped), median / mean per frame:", rounds - 1);
    for ((name, _, _), (_, _, _, build, submit, gpu)) in configs.iter().zip(runs.iter_mut()) {
        let (mb, ms, mg) = (mean(build), mean(submit), mean(gpu));
        eprintln!(
            "  {name:24} build {:.4} / {mb:.4} ms, submit {:.4} / {ms:.4} ms, GPU {:.4} / {mg:.4} ms ({} GPU samples)",
            median(build),
            median(submit),
            median(gpu),
            gpu.len()
        );
    }
}
