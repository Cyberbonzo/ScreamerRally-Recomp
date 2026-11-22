//! The backing test (`docs/re/native/visibility.md`): is a static's base (its lowest part)
//! in front of world geometry already drawn, or against the sky, the background or the void?
//!
//! The runtime marks statics in `FrameData::static_backing` with the opaque pass that draws
//! them and the tests their bases take. The renderer draws the opaque statics of pass 0,
//! then runs test 1, draws pass 1, runs test 2, draws pass 2 (then everything else as
//! before). A test draws the bases of the statics marked for it (one draw for all, no colour
//! written) and counts per static, in a buffer, with an atomic add in the pixel shader:
//!
//! * area (test 1 only): every base fragment inside the view (no depth test);
//! * visible: fragments not behind what is drawn (the world's depth test, nothing written);
//! * sky: fragments where nothing was drawn (depth forced to 0, the cleared value: the test
//!   passes only over pixels no surface wrote).
//!
//! The counts are copied to a staging buffer after the view and read back later without
//! stalling ([`BackingTest::results`]), so the runtime uses them one frame late.
//!
//! Moving objects (`FrameData::dynamic_backing`, the floating detector of
//! `docs/re/native/visibility.md`, "Moving objects"): after the opaque statics of every pass
//! and before the opaque moving objects, **test 3** draws the tested instances' surfaces below
//! their object's base top (clipped per vertex in world height) with the same three counts
//! (area, visible, sky) per object, in the counters after the statics'.

use super::prepare::{BaseVertex, StaticGeometry};
use super::renderer::{err, immutable_buffer};
use super::scene::*;
use std::collections::VecDeque;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{PCSTR, s};

/// Counters per tested static: area, visible 1, sky 1, visible 2, sky 2.
const COUNTERS: usize = 5;
/// Staging buffers in flight (results waiting to be read).
const MAX_PENDING: usize = 8;

/// One test draw: counter, backing bits required, depth rule.
#[derive(Clone, Copy)]
enum Rule {
    /// No depth test.
    Area,
    /// The world's depth, tested, not written.
    Visible,
    /// Depth 0 (the cleared value), tested: only pixels no surface wrote.
    Sky,
}

struct Pending {
    tag: u64,
    view: u32,
    statics: Vec<u32>,
    tested: Vec<u8>,
    /// Moving objects tested (their counters follow the statics').
    objects: usize,
    /// The staging buffer and its capacity (tested statics and objects).
    staging: ID3D11Buffer,
    cap: usize,
}

/// One instanced draw of test 3: index range and instance range (`FrameData::dynamics`
/// instances in the renderer's instance buffer).
#[derive(Clone, Copy, Debug)]
pub struct DynDraw {
    pub start: u32,
    pub count: u32,
    pub inst_start: u32,
    pub inst_count: u32,
}

pub struct BackingTest {
    device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    no_depth: ID3D11DepthStencilState,
    test_depth: ID3D11DepthStencilState,
    no_color: ID3D11BlendState,
    no_cull: ID3D11RasterizerState,
    /// Per draw: (counter, bits) as `TestCb`.
    cbs: Vec<ID3D11Buffer>,
    /// The bases of the scene's statics (`prepare::build_bases`), per static its vertices
    /// (first, count), and this frame's index list of the tested statics' bases.
    vb: Option<ID3D11Buffer>,
    ranges: Vec<(u32, u32)>,
    ib: super::renderer::DynBuf,
    indices: Vec<u32>,
    /// GPU time of the tests (`SR_NATIVE_VIS_GPU=1`): per view, logged every 2000 views.
    timer: Option<super::timing::GpuTimer>,
    timed: super::timing::Accum,
    /// Counters of the view being drawn and their UAV (grown as needed).
    counts: Option<(ID3D11Buffer, ID3D11UnorderedAccessView, usize)>,
    free_staging: Vec<(ID3D11Buffer, usize)>,
    pending: VecDeque<Pending>,
    /// This frame: the tested statics in order (their test slot = position) and their bits.
    tests: Vec<u32>,
    bits: Vec<u8>,
    /// Per-static words for the shaders (`backing` buffer, `native.hlsl`).
    pub words: Vec<u32>,
    /// The view's tests ran (counts cleared).
    cleared: bool,
    /// Results of frames without any tested static (nothing to read back).
    ready: Vec<BackingResults>,
    /// Test 3 (moving objects): its shaders (the world's input layout), per draw constants
    /// (counter, static slots before the objects'), the objects tested this frame and their
    /// base tops (frame space y, `dyn_tops` in `native.hlsl`).
    dyn_vs: ID3D11VertexShader,
    dyn_ps: ID3D11PixelShader,
    dyn_cb: ID3D11Buffer,
    objects: usize,
    tops: Vec<f32>,
    tops_buf: super::renderer::DynBuf,
}

impl BackingTest {
    pub fn new(device: &ID3D11Device, vs_code: &[u8], ps_code: &[u8], dyn_vs_code: &[u8], dyn_ps_code: &[u8]) -> Result<BackingTest, String> {
        unsafe {
            let mut vs = None;
            let mut ps = None;
            device.CreateVertexShader(vs_code, None, Some(&mut vs)).map_err(err)?;
            device.CreatePixelShader(ps_code, None, Some(&mut ps)).map_err(err)?;
            let mut dyn_vs = None;
            let mut dyn_ps = None;
            device.CreateVertexShader(dyn_vs_code, None, Some(&mut dyn_vs)).map_err(err)?;
            device.CreatePixelShader(dyn_ps_code, None, Some(&mut dyn_ps)).map_err(err)?;
            let dyn_cb = {
                let desc = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DYNAMIC, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, ..Default::default() };
                let mut b = None;
                device.CreateBuffer(&desc, None, Some(&mut b)).map_err(err)?;
                b.unwrap()
            };
            let el = |name: PCSTR, fmt: DXGI_FORMAT, off: u32| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: fmt,
                InputSlot: 0,
                AlignedByteOffset: off,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            };
            let elements = [el(s!("POSITION"), DXGI_FORMAT_R32G32B32_FLOAT, 0), el(s!("SLOT"), DXGI_FORMAT_R32_UINT, 12)];
            let mut layout = None;
            device.CreateInputLayout(&elements, vs_code, Some(&mut layout)).map_err(err)?;
            let ds = |enable: bool| -> Result<ID3D11DepthStencilState, String> {
                let d = D3D11_DEPTH_STENCIL_DESC {
                    DepthEnable: enable.into(),
                    DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ZERO,
                    DepthFunc: D3D11_COMPARISON_GREATER_EQUAL,
                    ..Default::default()
                };
                let mut s = None;
                device.CreateDepthStencilState(&d, Some(&mut s)).map_err(err)?;
                Ok(s.unwrap())
            };
            let no_color = {
                let mut desc = D3D11_BLEND_DESC::default();
                desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: false.into(),
                    SrcBlend: D3D11_BLEND_ONE,
                    DestBlend: D3D11_BLEND_ZERO,
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ONE,
                    DestBlendAlpha: D3D11_BLEND_ZERO,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: 0,
                };
                let mut b = None;
                device.CreateBlendState(&desc, Some(&mut b)).map_err(err)?;
                b.unwrap()
            };
            let no_cull = {
                let d = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ..Default::default() };
                let mut r = None;
                device.CreateRasterizerState(&d, Some(&mut r)).map_err(err)?;
                r.unwrap()
            };
            let mut cbs = Vec::new();
            for (counter, bits) in [(0u32, (BACKING_TEST1 | BACKING_TEST2) as u32), (1, BACKING_TEST1 as u32), (2, BACKING_TEST1 as u32), (3, BACKING_TEST2 as u32), (4, BACKING_TEST2 as u32)] {
                let v = [counter, bits, 0, 0];
                let desc = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_IMMUTABLE, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
                let init = D3D11_SUBRESOURCE_DATA { pSysMem: v.as_ptr() as *const _, ..Default::default() };
                let mut b = None;
                device.CreateBuffer(&desc, Some(&init), Some(&mut b)).map_err(err)?;
                cbs.push(b.unwrap());
            }
            Ok(BackingTest {
                device: device.clone(),
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                layout: layout.unwrap(),
                no_depth: ds(false)?,
                test_depth: ds(true)?,
                no_color,
                no_cull,
                cbs,
                vb: None,
                ranges: Vec::new(),
                ib: super::renderer::DynBuf::new(D3D11_BIND_INDEX_BUFFER, false),
                indices: Vec::new(),
                timer: if std::env::var("SR_NATIVE_VIS_GPU").is_ok_and(|v| v.trim() == "1") { Some(super::timing::GpuTimer::new(device)?) } else { None },
                timed: Default::default(),
                counts: None,
                free_staging: Vec::new(),
                pending: VecDeque::new(),
                tests: Vec::new(),
                bits: Vec::new(),
                words: Vec::new(),
                cleared: false,
                ready: Vec::new(),
                dyn_vs: dyn_vs.unwrap(),
                dyn_ps: dyn_ps.unwrap(),
                dyn_cb,
                objects: 0,
                tops: Vec::new(),
                tops_buf: super::renderer::DynBuf::new(D3D11_BIND_SHADER_RESOURCE, true),
            })
        }
    }

    /// A new scene: its statics' bases; results still pending belong to the old one.
    pub fn upload(&mut self, scene: &SceneData, g: &StaticGeometry, writes_depth: &dyn Fn(u32) -> bool) -> Result<usize, String> {
        let bases: Vec<BaseVertex> = super::prepare::build_bases(scene, g, writes_depth);
        // the vertices of a static's base are consecutive (build_bases goes static by static)
        self.ranges = vec![(0, 0); scene.statics.len()];
        let mut k = 0;
        while k < bases.len() {
            let si = bases[k].slot as usize;
            let first = k;
            while k < bases.len() && bases[k].slot as usize == si {
                k += 1;
            }
            if let Some(r) = self.ranges.get_mut(si) {
                *r = (first as u32, (k - first) as u32);
            }
        }
        self.vb = if bases.is_empty() { None } else { Some(immutable_buffer(&self.device, super::renderer::as_bytes(&bases), D3D11_BIND_VERTEX_BUFFER, 0)?) };
        for p in self.pending.drain(..) {
            self.free_staging.push((p.staging, p.cap));
        }
        self.ready.clear();
        Ok(bases.len() / 3)
    }

    /// The frame's backing marks: the shaders' words and the tested statics, and the moving
    /// objects tested (`origin_y`: the frame origin's height, for their base tops). False: no
    /// statics' marks given (no passes, no statics' tests).
    pub fn begin_frame(&mut self, device: &ID3D11Device, ctx: &ID3D11DeviceContext, frame: &FrameData, statics: usize, origin_y: f64) -> Result<bool, String> {
        self.tests.clear();
        self.bits.clear();
        self.words.clear();
        self.indices.clear();
        // moving objects: one base top per object
        self.tops.clear();
        for d in frame.dynamic_backing.iter().filter(|d| d.object != DynamicBacking::NONE.object) {
            let o = d.object as usize;
            if o >= self.tops.len() {
                self.tops.resize(o + 1, f32::MIN);
            }
            self.tops[o] = (d.base_top - origin_y) as f32;
        }
        self.objects = self.tops.len();
        if self.objects > 0 {
            let tops = std::mem::take(&mut self.tops);
            let r = self.tops_buf.write(device, ctx, &tops);
            self.tops = tops;
            r?;
        }
        if frame.static_backing.is_empty() {
            return Ok(false);
        }
        self.words.reserve(statics.max(1));
        for i in 0..statics.max(1) {
            let b = frame.static_backing.get(i).copied().unwrap_or(0) & (BACKING_PASS | BACKING_TEST1 | BACKING_TEST2);
            let mut w = b as u32;
            if b & (BACKING_TEST1 | BACKING_TEST2) != 0 && i < statics {
                w |= (self.tests.len() as u32) << 8;
                self.tests.push(i as u32);
                self.bits.push(b);
                let (first, n) = self.ranges.get(i).copied().unwrap_or((0, 0));
                self.indices.extend(first..first + n);
            }
            self.words.push(w);
        }
        if !self.indices.is_empty() {
            let idx = std::mem::take(&mut self.indices);
            let r = self.ib.write(device, ctx, &idx);
            self.indices = idx;
            r?;
        }
        Ok(true)
    }

    /// Statics tested this frame.
    pub fn tested(&self) -> usize {
        self.tests.len()
    }

    /// Moving objects tested this frame (test 3).
    pub fn objects(&self) -> usize {
        self.objects
    }

    /// The counters of this view, cleared at its first test.
    fn ensure_cleared(&mut self, ctx: &ID3D11DeviceContext) -> Result<(), String> {
        if !self.cleared {
            if let Some(t) = self.timer.as_mut() {
                t.begin(ctx);
            }
            self.ensure_counts(self.tests.len() + self.objects)?;
            let (_, uav, _) = self.counts.as_ref().unwrap();
            unsafe { ctx.ClearUnorderedAccessViewUint(uav, &[0; 4]) };
            self.cleared = true;
        }
        Ok(())
    }

    /// Run test 3 now (moving objects against every static drawn): `draws` are instanced
    /// draws of the scene's vertex and index buffers with the renderer's instance buffer,
    /// which the caller has bound (input layout, buffers, the view's constants and resources
    /// as for the world). The caller rebinds its own pipeline afterwards.
    pub fn run_dynamics(&mut self, ctx: &ID3D11DeviceContext, draws: &[DynDraw], rtv: &ID3D11RenderTargetView, dsv: &ID3D11DepthStencilView, vp: D3D11_VIEWPORT) -> Result<(), String> {
        if self.objects == 0 || draws.is_empty() {
            return Ok(());
        }
        self.ensure_cleared(ctx)?;
        let (_, uav, _) = self.counts.as_ref().unwrap();
        let first = self.tests.len() as u32;
        unsafe {
            ctx.OMSetRenderTargetsAndUnorderedAccessViews(Some(&[Some(rtv.clone())]), dsv, 1, 1, Some(&Some(uav.clone())), None);
            ctx.OMSetBlendState(&self.no_color, None, 0xffff_ffff);
            ctx.RSSetState(&self.no_cull);
            ctx.VSSetShader(&self.dyn_vs, None);
            ctx.GSSetShader(None, None);
            ctx.PSSetShader(&self.dyn_ps, None);
            ctx.VSSetShaderResources(8, Some(&[self.tops_buf.srv.clone()]));
            for (counter, rule) in [(0u32, Rule::Area), (1, Rule::Visible), (2, Rule::Sky)] {
                let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(&self.dyn_cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).map_err(err)?;
                std::ptr::copy_nonoverlapping([counter, 0, first, 0].as_ptr() as *const u8, m.pData as *mut u8, 16);
                ctx.Unmap(&self.dyn_cb, 0);
                ctx.VSSetConstantBuffers(3, Some(&[Some(self.dyn_cb.clone())]));
                match rule {
                    Rule::Area => ctx.OMSetDepthStencilState(&self.no_depth, 0),
                    _ => ctx.OMSetDepthStencilState(&self.test_depth, 0),
                }
                let v = match rule {
                    Rule::Sky => D3D11_VIEWPORT { MinDepth: 0.0, MaxDepth: 0.0, ..vp },
                    _ => vp,
                };
                ctx.RSSetViewports(Some(&[v]));
                for d in draws {
                    ctx.DrawIndexedInstanced(d.count, d.inst_count, d.start, 0, d.inst_start);
                }
            }
            ctx.RSSetViewports(Some(&[vp]));
            ctx.VSSetShaderResources(8, Some(&[None]));
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), dsv);
        }
        Ok(())
    }

    /// Run test `level` (1 or 2) now, against the depth the opaque passes have written into
    /// `dsv`. The caller rebinds its own pipeline afterwards.
    pub fn run(&mut self, ctx: &ID3D11DeviceContext, level: u32, rtv: &ID3D11RenderTargetView, dsv: &ID3D11DepthStencilView, vp: D3D11_VIEWPORT) -> Result<(), String> {
        let (Some(vb), n) = (self.vb.clone(), self.tests.len()) else { return Ok(()) };
        if n == 0 || self.indices.is_empty() {
            return Ok(());
        }
        self.ensure_cleared(ctx)?;
        let (_, uav, _) = self.counts.as_ref().unwrap();
        let draws: &[(usize, Rule)] = if level == 1 { &[(0, Rule::Area), (1, Rule::Visible), (2, Rule::Sky)] } else { &[(3, Rule::Visible), (4, Rule::Sky)] };
        unsafe {
            ctx.OMSetRenderTargetsAndUnorderedAccessViews(Some(&[Some(rtv.clone())]), dsv, 1, 1, Some(&Some(uav.clone())), None);
            ctx.OMSetBlendState(&self.no_color, None, 0xffff_ffff);
            ctx.RSSetState(&self.no_cull);
            ctx.IASetInputLayout(&self.layout);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            let stride = std::mem::size_of::<BaseVertex>() as u32;
            let offset = 0u32;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(vb) as *const _), Some(&stride), Some(&offset));
            ctx.IASetIndexBuffer(self.ib.buf.as_ref(), DXGI_FORMAT_R32_UINT, 0);
            ctx.VSSetShader(&self.vs, None);
            ctx.GSSetShader(None, None);
            ctx.PSSetShader(&self.ps, None);
            for &(k, rule) in draws {
                let cb = [Some(self.cbs[k].clone())];
                ctx.VSSetConstantBuffers(3, Some(&cb));
                match rule {
                    Rule::Area => ctx.OMSetDepthStencilState(&self.no_depth, 0),
                    _ => ctx.OMSetDepthStencilState(&self.test_depth, 0),
                }
                let v = match rule {
                    Rule::Sky => D3D11_VIEWPORT { MinDepth: 0.0, MaxDepth: 0.0, ..vp },
                    _ => vp,
                };
                ctx.RSSetViewports(Some(&[v]));
                ctx.DrawIndexed(self.indices.len() as u32, 0, 0);
            }
            ctx.RSSetViewports(Some(&[vp]));
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), dsv);
        }
        Ok(())
    }

    fn ensure_counts(&mut self, n: usize) -> Result<(), String> {
        if self.counts.as_ref().is_some_and(|c| c.2 >= n) {
            return Ok(());
        }
        let cap = n.next_power_of_two().max(256);
        let desc = D3D11_BUFFER_DESC { ByteWidth: (cap * COUNTERS * 4) as u32, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32, ..Default::default() };
        let mut b = None;
        unsafe { self.device.CreateBuffer(&desc, None, Some(&mut b)) }.map_err(err)?;
        let b = b.unwrap();
        let ud = D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_UINT,
            ViewDimension: D3D11_UAV_DIMENSION_BUFFER,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 { Buffer: D3D11_BUFFER_UAV { FirstElement: 0, NumElements: (cap * COUNTERS) as u32, Flags: 0 } },
        };
        let mut u = None;
        unsafe { self.device.CreateUnorderedAccessView(&b, Some(&ud), Some(&mut u)) }.map_err(err)?;
        self.counts = Some((b, u.unwrap(), cap));
        Ok(())
    }

    /// After a view: its counts to a staging buffer, to be read later. `tag` and `view`
    /// identify the results.
    pub fn end_view(&mut self, ctx: &ID3D11DeviceContext, tag: u64, view: u32) -> Result<(), String> {
        let n = self.tests.len() + self.objects;
        let cleared = std::mem::replace(&mut self.cleared, false);
        if cleared && let Some(t) = self.timer.as_mut() {
            t.end(ctx);
        }
        if n == 0 || !cleared {
            // nothing tested (or no bases, no objects in view): results are known now
            if !self.words.is_empty() || self.objects > 0 {
                let dynamics = (0..self.objects as u32).map(|o| (o, BackingCounts { tested: 1, ..Default::default() })).collect();
                self.ready.push(BackingResults { tag, view, statics: Vec::new(), dynamics });
            }
            return Ok(());
        }
        if self.pending.len() >= MAX_PENDING {
            // the oldest is dropped (the GPU is far behind): its staging buffer is reused
            let old = self.pending.pop_front().unwrap();
            self.free_staging.push((old.staging, old.cap));
        }
        let bytes = n * COUNTERS * 4;
        let (staging, cap) = match self.free_staging.iter().position(|(_, cap)| *cap >= n) {
            Some(i) => self.free_staging.swap_remove(i),
            None => {
                let cap = n.next_power_of_two().max(256);
                let desc = D3D11_BUFFER_DESC { ByteWidth: (cap * COUNTERS * 4) as u32, Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default() };
                let mut b = None;
                unsafe { self.device.CreateBuffer(&desc, None, Some(&mut b)) }.map_err(err)?;
                (b.unwrap(), cap)
            }
        };
        let (src, _, _) = self.counts.as_ref().unwrap();
        let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: bytes as u32, bottom: 1, back: 1 };
        unsafe { ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, src, 0, Some(&bx)) };
        self.pending.push_back(Pending { tag, view, statics: self.tests.clone(), tested: self.bits.clone(), objects: self.objects, staging, cap });
        Ok(())
    }

    /// The results finished since the last call, oldest first. `wait`: block until every
    /// pending one is done (deterministic runs: the results of the previous frame always
    /// arrive with the next one).
    pub fn results(&mut self, ctx: &ID3D11DeviceContext, wait: bool) -> Vec<BackingResults> {
        let mut out = std::mem::take(&mut self.ready);
        if let Some(t) = self.timer.as_mut() {
            for ms in t.poll(ctx) {
                self.timed.add(ms);
            }
            if self.timed.n >= 2000 {
                sr_runtime::info!("native backing test: GPU {:.4} ms per view on average, {:.4} at most ({} views)", self.timed.mean(), self.timed.max, self.timed.n);
                self.timed = Default::default();
            }
        }
        while let Some(p) = self.pending.front() {
            let buf = p.staging.clone();
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            let flags = if wait { 0 } else { D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32 };
            let r = unsafe { ctx.Map(&buf, 0, D3D11_MAP_READ, flags, Some(&mut m)) };
            if r.is_err() {
                // not ready yet (DXGI_ERROR_WAS_STILL_DRAWING): later, in order
                break;
            }
            let p = self.pending.pop_front().unwrap();
            let n = p.statics.len();
            let c = unsafe { std::slice::from_raw_parts(m.pData as *const u32, (n + p.objects) * COUNTERS) };
            let dynamics = (0..p.objects)
                .map(|o| {
                    let v = &c[(n + o) * COUNTERS..(n + o + 1) * COUNTERS];
                    (o as u32, BackingCounts { area: v[0], visible: [v[1], 0], sky: [v[2], 0], tested: 1 })
                })
                .collect();
            let statics = p
                .statics
                .iter()
                .enumerate()
                .map(|(k, &si)| {
                    let v = &c[k * COUNTERS..k * COUNTERS + COUNTERS];
                    let bits = p.tested[k];
                    let tested = (bits & BACKING_TEST1 != 0) as u8 | ((bits & BACKING_TEST2 != 0) as u8) << 1;
                    (si, BackingCounts { area: v[0], visible: [v[1], v[3]], sky: [v[2], v[4]], tested })
                })
                .collect();
            unsafe { ctx.Unmap(&buf, 0) };
            self.free_staging.push((buf, p.cap));
            out.push(BackingResults { tag: p.tag, view: p.view, statics, dynamics });
        }
        out
    }
}
