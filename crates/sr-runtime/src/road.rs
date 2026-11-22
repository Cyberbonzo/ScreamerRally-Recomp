//! The road's line (STRT3FX): the line the computer's cars follow round the track, read from
//! the game, and stretches of it along the road (the visibility editor's road extension,
//! `crate::visedit::road_extension`).
//!
//! The track loader (`37dd9`) loads `<map>opp.spl` from `data\<map>.dat` (`3ab00`, the
//! "GAMESPLINE" file) into the table at `[139700]`: entries of 24h bytes (+0 the nodes, +1Ch
//! their count), each a closed cubic Bezier spline in the ground plane. A node is 38h bytes:
//! +4 / +8 the point (x, z, integers), +10h the next node's index (the last one: 0), +14h /
//! +18h the handle the curve arrives on, +20h / +24h the handle it leaves on, +34h the
//! distance along the line, which the loader measures (`37e21`: 100-unit steps of `363d4`);
//! the cars' progress (car +C8h the node, +BCh the distance) runs along it. Every track has
//! one line (sampled here every ~1000 units: Rally China 1 228 000 units, the game's own
//! measure 1 237 000). The count at `[139704]` reads 0 on Rally Italy during the race,
//! although its line is there, so the table's first entry is read and checked instead.
//!
//! The other splines are no road line: `<map>dir.spl` (`[1396fc]`, 21 to 35 straight
//! points per track, the pace notes' arrows) is up to 47 000 units off the road, and
//! `<map>ost.spl` (`[136a08]`) is empty on these tracks.

use crate::Machine;
use crate::mem;

/// The spline table and the size of its entries and nodes.
const SPLINES: u32 = 0x139700;
const NODE_SIZE: u32 = 0x38;
/// The longest step between two samples of the line (world units).
const STEP: f64 = 1000.0;

/// The road's line as the game holds it now (x, z points of a closed polyline), or `None`
/// when the table holds no plausible line.
pub fn read(m: &Machine) -> Option<Vec<[f32; 2]>> {
    let rd = |a: u32| unsafe { mem::rd32(a) };
    let head = rd(m.img.link(SPLINES));
    if !mem::readable(head, 0x24) {
        return None;
    }
    let (nodes, n) = (rd(head), rd(head + 0x1c));
    if !(3..=4096).contains(&n) || !mem::readable(nodes, n * NODE_SIZE) {
        return None;
    }
    let node = |k: u32| -> [i32; 14] { std::array::from_fn(|i| rd(nodes + k * NODE_SIZE + 4 * i as u32) as i32) };
    let mut points = Vec::new();
    for k in 0..n {
        let a = node(k);
        // a closed chain: each node leads to the next one, the last back to the first
        let next = if k + 1 == n { 0 } else { k + 1 };
        if a[4] as u32 != next {
            return None;
        }
        let b = node(next);
        let p = |x: i32, z: i32| [x as f64, z as f64];
        let curve = [p(a[1], a[2]), p(a[8], a[9]), p(b[5], b[6]), p(b[1], b[2])];
        // the control polygon's length bounds the curve's
        let reach: f64 = curve.windows(2).map(|w| dist(w[0], w[1])).sum();
        let steps = (reach / STEP).ceil().clamp(1.0, 256.0) as usize;
        for i in 0..steps {
            let t = i as f64 / steps as f64;
            let q = bezier(&curve, t);
            points.push([q[0] as f32, q[1] as f32]);
        }
    }
    Some(points)
}

/// Whether `line` belongs to the world in `bounds` (x0, z0, x1, z1, the statics' extent): all
/// but a few of its points within two cells of it (a stale line of another track fails).
pub fn fits(line: &[[f32; 2]], bounds: [f32; 4]) -> bool {
    let m = 2.0 * crate::visedit::CELL_SIZE as f32;
    let inside = line.iter().filter(|p| p[0] >= bounds[0] - m && p[0] <= bounds[2] + m && p[1] >= bounds[1] - m && p[1] <= bounds[3] + m).count();
    !line.is_empty() && inside * 100 >= line.len() * 98
}

fn bezier(c: &[[f64; 2]; 4], t: f64) -> [f64; 2] {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    [0, 1].map(|i| (0..4).map(|k| w[k] * c[k][i]).sum())
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Distance from `p` to the segment `a`-`b`.
fn to_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0) } else { 0.0 };
    dist(p, [a[0] + t * dx, a[1] + t * dz])
}

/// A closed road line with the distance along it at each point.
#[derive(Clone, Debug)]
pub struct Road {
    pts: Vec<[f64; 2]>,
    /// Distance along the line at each point, and the whole length last (`pts.len() + 1`).
    at: Vec<f64>,
}

/// A piece of the line (consecutive segments) with its bounding box, for quick distances.
#[derive(Clone, Debug)]
pub struct Stretch {
    /// Segment `i` runs from point `i` to point `i + 1` (the last one back to the first).
    pub segments: Vec<usize>,
    /// Runs of up to `RUN` segments (first position in `segments`, count) and their boxes
    /// (x0, z0, x1, z1).
    runs: Vec<(usize, usize, [f64; 4])>,
}

const RUN: usize = 16;

impl Road {
    /// The line through `points` (closed: the last point leads back to the first).
    pub fn new(points: &[[f32; 2]]) -> Option<Road> {
        if points.len() < 3 {
            return None;
        }
        let pts: Vec<[f64; 2]> = points.iter().map(|p| [p[0] as f64, p[1] as f64]).collect();
        let mut at = Vec::with_capacity(pts.len() + 1);
        let mut s = 0.0;
        for i in 0..pts.len() {
            at.push(s);
            s += dist(pts[i], pts[(i + 1) % pts.len()]);
        }
        at.push(s);
        Some(Road { pts, at })
    }

    pub fn length(&self) -> f64 {
        *self.at.last().unwrap()
    }

    pub fn points(&self) -> &[[f64; 2]] {
        &self.pts
    }

    pub fn segment(&self, i: usize) -> ([f64; 2], [f64; 2]) {
        (self.pts[i], self.pts[(i + 1) % self.pts.len()])
    }

    /// Where the road passes through the rectangle `r` (x0, z0, x1, z1): the distance along
    /// the line of each point inside it. A rectangle the line misses takes the line's
    /// nearest point when it is within `reach` of the rectangle's centre (a camera beside
    /// the road), else nothing.
    pub fn passes(&self, r: [f64; 4], reach: f64) -> Vec<f64> {
        let inside: Vec<f64> = self.pts.iter().zip(&self.at).filter(|(p, _)| p[0] >= r[0] && p[0] < r[2] && p[1] >= r[1] && p[1] < r[3]).map(|(_, &s)| s).collect();
        if !inside.is_empty() {
            return inside;
        }
        let c = [(r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5];
        let mut best = (f64::MAX, 0.0);
        for i in 0..self.pts.len() {
            let (a, b) = self.segment(i);
            let d = to_segment(c, a, b);
            if d < best.0 {
                // the nearest point's distance along the line (projected on the segment)
                let len = self.at[i + 1] - self.at[i];
                let t = if len > 0.0 { (((c[0] - a[0]) * (b[0] - a[0]) + (c[1] - a[1]) * (b[1] - a[1])) / (len * len)).clamp(0.0, 1.0) } else { 0.0 };
                best = (d, self.at[i] + t * len);
            }
        }
        if best.0 <= reach { vec![best.1] } else { Vec::new() }
    }

    /// How far the road goes from `from` (a distance along the line), forward or back, before
    /// its first point where `keep` is false (at most half the lap).
    pub fn walk(&self, from: f64, forward: bool, keep: impl Fn([f64; 2]) -> bool) -> f64 {
        let (l, n) = (self.length(), self.pts.len());
        let from = from.rem_euclid(l);
        // the first point at or after `from` (forward), at or before it (back)
        let first = self.at[..n].partition_point(|&s| s < from);
        let mut i = if forward { first % n } else if first < n && self.at[first] == from { first } else { (first + n - 1) % n };
        for _ in 0..n {
            let d = if forward { self.at[i] - from } else { from - self.at[i] }.rem_euclid(l);
            if d > l * 0.5 {
                return l * 0.5;
            }
            if !keep(self.pts[i]) {
                return d;
            }
            i = if forward { (i + 1) % n } else { (i + n - 1) % n };
        }
        l * 0.5
    }

    /// The segments within `along` (distance along the road, both ways, round the lap) of
    /// any of the places `from` (distances along the line).
    pub fn stretch(&self, from: &[f64], along: f64) -> Stretch {
        let ranges: Vec<(f64, f64)> = from.iter().map(|&s| (s - along, s + along)).collect();
        self.stretch_ranges(&ranges)
    }

    /// The segments that touch any of the `ranges` (distances along the line, `a` to `b`,
    /// round the lap: they may start below 0 or end past the length).
    pub fn stretch_ranges(&self, ranges: &[(f64, f64)]) -> Stretch {
        let l = self.length();
        let mut segments = Vec::new();
        for i in 0..self.pts.len() {
            let (s0, s1) = (self.at[i], self.at[i + 1]);
            let (mid, half) = ((s0 + s1) * 0.5, (s1 - s0) * 0.5);
            let near = ranges.iter().any(|&(a, b)| {
                let (c, r) = ((a + b) * 0.5, (b - a).abs() * 0.5);
                let d = (mid - c).rem_euclid(l);
                d.min(l - d) <= half + r
            });
            if near {
                segments.push(i);
            }
        }
        let mut runs = Vec::new();
        for (k, chunk) in segments.chunks(RUN).enumerate() {
            let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for &i in chunk {
                let (p, q) = self.segment(i);
                for v in [p, q] {
                    b = [b[0].min(v[0]), b[1].min(v[1]), b[2].max(v[0]), b[3].max(v[1])];
                }
            }
            runs.push((k * RUN, chunk.len(), b));
        }
        Stretch { segments, runs }
    }

    /// Distance from `p` to the stretch `s`, or `None` when it is farther than `limit`.
    pub fn distance(&self, s: &Stretch, p: [f64; 2], limit: f64) -> Option<f64> {
        let mut best = f64::MAX;
        for &(first, count, b) in &s.runs {
            let dx = (b[0] - p[0]).max(p[0] - b[2]).max(0.0);
            let dz = (b[1] - p[1]).max(p[1] - b[3]).max(0.0);
            if dx.hypot(dz) > limit.min(best) {
                continue;
            }
            for &i in &s.segments[first..first + count] {
                let (a, q) = self.segment(i);
                best = best.min(to_segment(p, a, q));
            }
        }
        (best <= limit).then_some(best)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square loop 10 000 units a side, a point every 1000 units.
    fn square() -> Road {
        let mut pts = Vec::new();
        for i in 0..10 {
            pts.push([i as f32 * 1000.0, 0.0]);
        }
        for i in 0..10 {
            pts.push([10000.0, i as f32 * 1000.0]);
        }
        for i in 0..10 {
            pts.push([10000.0 - i as f32 * 1000.0, 10000.0]);
        }
        for i in 0..10 {
            pts.push([0.0, 10000.0 - i as f32 * 1000.0]);
        }
        Road::new(&pts).unwrap()
    }

    #[test]
    fn length_and_passes() {
        let r = square();
        assert!((r.length() - 40000.0).abs() < 1e-6);
        // the bottom edge between x 2500 and 4500: the points at 3000 and 4000
        assert_eq!(r.passes([2500.0, -100.0, 4500.0, 100.0], 0.0), vec![3000.0, 4000.0]);
        // a rectangle beside the road (centre 2000 units off): the nearest point, if in reach
        assert_eq!(r.passes([4900.0, -2100.0, 5100.0, -1900.0], 2500.0), vec![5000.0]);
        assert!(r.passes([4900.0, -2100.0, 5100.0, -1900.0], 1500.0).is_empty());
    }

    #[test]
    fn stretch_follows_the_road_and_wraps() {
        let r = square();
        // from 1500 along the line, 3000 each way: segments covering -1500..4500 (round the
        // start: the last two of the left edge too)
        let s = r.stretch(&[1500.0], 3000.0);
        assert_eq!(s.segments, vec![0, 1, 2, 3, 4, 38, 39]);
        // a place beside the far side of the loop is not near the stretch; one 1500 units off
        // its bottom edge is
        assert!(r.distance(&s, [5000.0, 9000.0], 2000.0).is_none());
        assert!((r.distance(&s, [2000.0, 1500.0], 2000.0).unwrap() - 1500.0).abs() < 1e-9);
        // the whole lap when `along` reaches half of it
        assert_eq!(r.stretch(&[0.0], 20000.0).segments.len(), 40);
        // ranges: 5000 to 7000 and, round the start, -1000 to 0 (segments touching an end
        // count)
        assert_eq!(r.stretch_ranges(&[(5000.0, 7000.0), (-1000.0, 0.0)]).segments, vec![0, 4, 5, 6, 7, 38, 39]);
    }

    #[test]
    fn walk_stops_at_the_first_point_left_out() {
        let r = square();
        // forward from 1500: points 2000, 3000 ... kept while x < 4500: stops at 5000
        let keep = |p: [f64; 2]| p[1] != 0.0 || p[0] < 4500.0;
        assert_eq!(r.walk(1500.0, true, keep), 3500.0);
        // back from 1500 round the start: 1000, 0 (kept), then the left edge (x 0, kept)...
        // until half the lap
        assert_eq!(r.walk(1500.0, false, keep), 20000.0);
        // back, stopping at the left edge's points
        assert_eq!(r.walk(1500.0, false, |p| p[0] != 0.0 || p[1] == 0.0), 2500.0);
        // from a point itself: it counts
        assert_eq!(r.walk(3000.0, true, |p| p[0] != 3000.0), 0.0);
    }
}
