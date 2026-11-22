//! GPU time of a stretch of commands (timestamp queries, read back a few frames later
//! without stalling) and running averages for the log.

use windows::Win32::Graphics::Direct3D11::*;
use windows::core::Interface;

struct QuerySet {
    disjoint: ID3D11Query,
    t0: ID3D11Query,
    t1: ID3D11Query,
}

/// Brackets GPU work with `begin` / `end`; `poll` collects finished measurements.
pub struct GpuTimer {
    sets: Vec<QuerySet>,
    /// Sets waiting for their results, oldest first.
    pending: std::collections::VecDeque<usize>,
    free: Vec<usize>,
    open: Option<usize>,
}

/// `GetData` without flushing, telling "not ready" (S_FALSE) from success (the `windows`
/// wrapper maps both to `Ok`).
fn get_data(ctx: &ID3D11DeviceContext, q: &ID3D11Query, out: *mut core::ffi::c_void, size: u32) -> bool {
    let hr = unsafe { (Interface::vtable(ctx).GetData)(ctx.as_raw(), q.as_raw(), out, size, D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32) };
    hr.0 == 0
}

impl GpuTimer {
    pub fn new(device: &ID3D11Device) -> Result<GpuTimer, String> {
        let make = |kind: D3D11_QUERY| -> Result<ID3D11Query, String> {
            let mut q = None;
            unsafe { device.CreateQuery(&D3D11_QUERY_DESC { Query: kind, MiscFlags: 0 }, Some(&mut q)) }.map_err(|e| e.to_string())?;
            q.ok_or_else(|| "no query".to_string())
        };
        let mut sets = Vec::new();
        for _ in 0..8 {
            sets.push(QuerySet { disjoint: make(D3D11_QUERY_TIMESTAMP_DISJOINT)?, t0: make(D3D11_QUERY_TIMESTAMP)?, t1: make(D3D11_QUERY_TIMESTAMP)? });
        }
        Ok(GpuTimer { free: (0..sets.len()).rev().collect(), sets, pending: Default::default(), open: None })
    }

    /// Start measuring (skipped while all query sets wait for results).
    pub fn begin(&mut self, ctx: &ID3D11DeviceContext) {
        if self.open.is_some() {
            return;
        }
        if let Some(i) = self.free.pop() {
            let s = &self.sets[i];
            unsafe {
                ctx.Begin(&s.disjoint);
                ctx.End(&s.t0);
            }
            self.open = Some(i);
        }
    }

    pub fn end(&mut self, ctx: &ID3D11DeviceContext) {
        if let Some(i) = self.open.take() {
            let s = &self.sets[i];
            unsafe {
                ctx.End(&s.t1);
                ctx.End(&s.disjoint);
            }
            self.pending.push_back(i);
        }
    }

    /// Finished measurements since the last call, in milliseconds (disjoint ones dropped).
    pub fn poll(&mut self, ctx: &ID3D11DeviceContext) -> Vec<f64> {
        let mut out = Vec::new();
        while let Some(&i) = self.pending.front() {
            let s = &self.sets[i];
            let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            if !get_data(ctx, &s.disjoint, &mut dj as *mut _ as *mut _, std::mem::size_of_val(&dj) as u32) {
                break;
            }
            let (mut a, mut b) = (0u64, 0u64);
            let ok = get_data(ctx, &s.t0, &mut a as *mut _ as *mut _, 8) && get_data(ctx, &s.t1, &mut b as *mut _ as *mut _, 8);
            if !ok {
                break;
            }
            self.pending.pop_front();
            self.free.push(i);
            if !dj.Disjoint.as_bool() && dj.Frequency > 0 && b >= a {
                out.push((b - a) as f64 * 1000.0 / dj.Frequency as f64);
            }
        }
        out
    }
}

/// Running sums for averages over a reporting interval.
#[derive(Clone, Copy, Debug, Default)]
pub struct Accum {
    pub n: u64,
    pub sum: f64,
    pub max: f64,
}

impl Accum {
    pub fn add(&mut self, v: f64) {
        self.n += 1;
        self.sum += v;
        self.max = self.max.max(v);
    }
    pub fn mean(&self) -> f64 {
        if self.n == 0 { 0.0 } else { self.sum / self.n as f64 }
    }
}
