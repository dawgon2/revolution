//! Roto shape coverage (CPU, C3.3). The GPU comp measures the same flattened edges in a kernel
//! (LO.5, `filmcraft_gpu`'s `comp_roto`) and falls back to uploading this f32 coverage (C3.6).
//!
//! [`prepare`] evaluates every enabled [`RotoShape`] of a [`Roto`] at a time and flattens it to a
//! closed polygon in working pixels whose vertices carry the inner / outer feather distance
//! (per-point feather plus half the global feather, scaled to working pixels):
//!
//! - a Bézier shape's segments are the cubics between consecutive points (`p`, `p + t_out`,
//!   `q + t_in`, `q`);
//! - a B-spline shape's segments are uniform cubic B-spline spans, converted to Bézier form with
//!   the standard basis change (`b0 = (P0 + 4P1 + P2)/6`, `b1 = (2P1 + P2)/3`,
//!   `b2 = (P1 + 2P2)/3`, `b3 = (P1 + 4P2 + P3)/6`); an open one triples its end points so the
//!   curve starts and ends on them. A span lies between control points `P1` and `P2`, so its
//!   feather is interpolated from `P1`'s to `P2`'s;
//! - an open shape is closed by a straight edge from its end back to its start.
//!
//! Each cubic is flattened by recursive de Casteljau halving (iterative, at most [`MAX_DEPTH`]
//! levels, i.e. ≤ 1024 chords per segment) until both inner control points are within
//! [`FLATTEN_TOLERANCE`] of the chord *segment* (so a straight segment, e.g. between two corner
//! points, is one chord); each emitted vertex gets the feather interpolated by its curve
//! parameter. A shape keeps at most [`MAX_SHAPE_POINTS`] vertices (a coarser tolerance first,
//! truncation as a last resort) and all shapes together [`MAX_TOTAL_POINTS`].
//!
//! **Coverage with a per-point feather.** For a pixel centre `p`, let `inside` be the nonzero
//! winding of the polygon. For every edge, `d` is the distance from `p` to the edge and `t` the
//! parameter of the closest point on it; the edge's inner / outer widths `wi`, `wo` are its end
//! widths interpolated at `t`. The edge's signed value `s = ±d + expansion` (+ inside) maps to
//!
//! ```text
//! w = wi + wo,  c0 = (wi − wo) / 2
//! u = falloff(s − c0, w)        // crate::mask::falloff: 1 at s = wi, 0 at s = −wo
//! ```
//!
//! (a 1-pixel antialiasing ramp centred on the edge when `w` is 0, as for masks). Inside the
//! shape the coverage is the **minimum** of the edges' values, outside the **maximum**: the union
//! of the outer ramps and the intersection of the inner ones, so a wide feather at one point
//! widens the ramp around it and narrows smoothly towards its neighbours. An edge farther than
//! its own band (`max(wi, wo) + |expansion| + 1.5`) saturates (1 inside, 0 outside) and drops
//! out. With equal widths everywhere this is exactly the signed-distance falloff of a mask.
//! Where a wide and a narrow feather meet at a corner, the inside and outside values can differ
//! slightly right on the outline (min vs max of different edges).
//!
//! The shape's value is `opacity · c` (`1 − c` when inverted); shapes combine top to bottom with
//! their [`RotoMode`], starting from the first enabled shape's [`RotoMode::start`]. No enabled
//! shape gives 0 everywhere. A shape with fewer than three points has no area (`c = 0`).
//!
//! The row rasterizer works in bands of [`BAND_ROWS`] rows: an edge table sorted by the top of
//! each edge's band gives the edges a band can see (the rest of the shape cannot change it), and
//! within a row only edges whose band covers the row and the pixel are measured.

use filmcraft_geom::Vec2;
use filmcraft_project::MaskPath;
use filmcraft_project::roto::{Roto, RotoKind, RotoMode, RotoShape};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::mask::falloff;

/// Flattening tolerance in working pixels.
pub const FLATTEN_TOLERANCE: f64 = 0.05;
/// Deepest de Casteljau subdivision (2^10 chords per cubic at most).
pub const MAX_DEPTH: u32 = 10;
/// Most flattened vertices per shape.
pub const MAX_SHAPE_POINTS: usize = 100_000;
/// Most flattened vertices of all shapes together.
pub const MAX_TOTAL_POINTS: usize = 1_000_000;
/// Largest feather / |expansion| in working pixels.
const MAX_WORK_FEATHER: f64 = 1e5;
/// Largest working pixel scale honoured.
const MAX_PX_SCALE: f64 = 1e4;
/// Largest frame a whole-buffer [`rasterize`] allocates (pixels).
pub const MAX_PIXELS: usize = 256 * 1024 * 1024;
/// Rows per band of the row rasterizer.
pub const BAND_ROWS: usize = 16;

/// A flattened outline vertex in working pixels with its feather distances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatVertex {
    pub p: [f32; 2],
    /// Distance inside the outline where coverage reaches 1.
    pub inner: f32,
    /// Distance outside the outline where coverage reaches 0.
    pub outer: f32,
}

/// One polygon edge with its reach (public for the GPU rasteriser, LO.5, which measures the
/// same edges).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub a: FlatVertex,
    pub b: FlatVertex,
    /// Distance beyond which this edge no longer changes the coverage.
    pub band: f32,
    /// Its band's extent: `[x0, y0, x1, y1]`.
    pub reach: [f32; 4],
}

/// One enabled shape ready for rasterizing.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedShape {
    /// Closed polygon (the last vertex connects to the first); empty when the shape has no area.
    pub pts: Vec<FlatVertex>,
    pub expansion: f32,
    /// 0..1.
    pub opacity: f32,
    pub inverted: bool,
    pub mode: RotoMode,
    /// The edges sorted by the top of their band (`reach[1]`).
    edges: Vec<Edge>,
}

/// A roto evaluated at one time, in working pixels of a `w × h` frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreparedRoto {
    pub shapes: Vec<PreparedShape>,
    pub w: usize,
    pub h: usize,
}

fn finite_or(v: f64, d: f64) -> f64 {
    if v.is_finite() { v } else { d }
}

/// Evaluate and flatten the enabled shapes of `roto` at `at` for a `w × h` working frame;
/// `px_scale` = working pixels per frame pixel (non-finite or ≤ 0: every shape is empty).
pub fn prepare(roto: &Roto, at: Tick, w: usize, h: usize, px_scale: f32) -> PreparedRoto {
    let s = f64::from(px_scale);
    let s = if s.is_finite() && s > 0.0 { s.min(MAX_PX_SCALE) } else { 0.0 };
    let mut budget = MAX_TOTAL_POINTS;
    let shapes = roto
        .shapes
        .iter()
        .take(filmcraft_project::roto::MAX_SHAPES)
        .filter(|sh| sh.enabled)
        .map(|sh| {
            let p = prepare_shape(sh, at, s, budget);
            budget = budget.saturating_sub(p.pts.len());
            p
        })
        .collect();
    PreparedRoto { shapes, w, h }
}

fn prepare_shape(sh: &RotoShape, at: Tick, s: f64, budget: usize) -> PreparedShape {
    let half = finite_or(sh.feather_at(at) * s * 0.5, 0.0).clamp(0.0, MAX_WORK_FEATHER);
    let expansion = finite_or(sh.expansion_at(at) * s, 0.0).clamp(-MAX_WORK_FEATHER, MAX_WORK_FEATHER) as f32;
    let opacity = finite_or(sh.opacity_at(at), 0.0).clamp(0.0, 1.0) as f32;
    let pts = if s > 0.0 { flatten(sh.kind, &sh.path_at(at), s, half, budget.min(MAX_SHAPE_POINTS)) } else { Vec::new() };
    let n = pts.len();
    let mut edges: Vec<Edge> = if n < 3 {
        Vec::new()
    } else {
        (0..n)
            .filter_map(|i| {
                let (a, b) = (*pts.get(i)?, *pts.get((i + 1) % n)?);
                let band = expansion.abs() + a.inner.max(a.outer).max(b.inner).max(b.outer) + 1.5;
                let reach = [a.p[0].min(b.p[0]) - band, a.p[1].min(b.p[1]) - band, a.p[0].max(b.p[0]) + band, a.p[1].max(b.p[1]) + band];
                Some(Edge { a, b, band, reach })
            })
            .collect()
    };
    edges.sort_by(|p, q| p.reach[1].total_cmp(&q.reach[1]));
    PreparedShape { pts, expansion, opacity, inverted: sh.inverted, mode: sh.mode, edges }
}

/// A cubic in Bézier form with the feather (inner, outer) at its two ends.
type Seg = ([Vec2; 4], [f64; 2], [f64; 2]);

/// The cubic segments of a shape in working pixels, closed (see the module docs).
fn segments(kind: RotoKind, path: &MaskPath, s: f64, half: f64) -> Vec<Seg> {
    let pts = &path.vertices;
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let closed = path.closed;
    let fe = |q: &filmcraft_project::MaskVertex| {
        let f = |v: f64| finite_or(v * s + half, 0.0).clamp(0.0, MAX_WORK_FEATHER);
        [f(q.feather_in), f(q.feather_out)]
    };
    let sc = |v: Vec2| v * s;
    let get = |i: usize| pts.get(i % n);
    let mut out = Vec::with_capacity(n + 2);
    match kind {
        RotoKind::Bezier => {
            let count = if closed { n } else { n - 1 };
            for i in 0..count {
                let (Some(a), Some(b)) = (get(i), get(i + 1)) else { continue };
                out.push(([sc(a.p), sc(a.p + a.t_out), sc(b.p + b.t_in), sc(b.p)], fe(a), fe(b)));
            }
        }
        RotoKind::BSpline => {
            // control index sequence: wrapped for closed, ends tripled for open
            let idx: Vec<usize> =
                if closed { (0..n + 3).map(|i| (i + n - 1) % n).collect() } else { [0, 0].into_iter().chain(0..n).chain([n - 1, n - 1]).collect() };
            for w in idx.windows(4) {
                let (Some(p0), Some(p1), Some(p2), Some(p3)) = (get(w[0]), get(w[1]), get(w[2]), get(w[3])) else { continue };
                let (a, b, c, d) = (sc(p0.p), sc(p1.p), sc(p2.p), sc(p3.p));
                let b0 = (a + b * 4.0 + c) * (1.0 / 6.0);
                let b1 = (b * 2.0 + c) * (1.0 / 3.0);
                let b2 = (b + c * 2.0) * (1.0 / 3.0);
                let b3 = (b + c * 4.0 + d) * (1.0 / 6.0);
                out.push(([b0, b1, b2, b3], fe(p1), fe(p2)));
            }
        }
    }
    if !closed && let (Some(first), Some(last)) = (out.first(), out.last()) {
        // straight closing edge from the end back to the start
        let (e, s0) = (last.0[3], first.0[0]);
        out.push(([e, e, s0, s0], last.2, first.1));
    }
    out
}

fn flatten(kind: RotoKind, path: &MaskPath, s: f64, half: f64, cap: usize) -> Vec<FlatVertex> {
    let segs = segments(kind, path, s, half);
    let mut tol = FLATTEN_TOLERANCE;
    for _ in 0..2 {
        let (v, full) = flatten_segs(&segs, tol, cap);
        if !full {
            return v;
        }
        tol *= 64.0;
    }
    flatten_segs(&segs, tol, cap).0
}

/// Flatten closed segments (only segment end points are emitted: the loop closes on itself).
/// The flag is true when `cap` cut the outline short.
fn flatten_segs(segs: &[Seg], tol: f64, cap: usize) -> (Vec<FlatVertex>, bool) {
    let mut out = Vec::new();
    let mut stack: Vec<([Vec2; 4], f64, f64, u32)> = Vec::with_capacity(MAX_DEPTH as usize + 2);
    for (c, fa, fb) in segs {
        stack.clear();
        stack.push((*c, 0.0, 1.0, 0));
        while let Some((q, t0, t1, depth)) = stack.pop() {
            if depth >= MAX_DEPTH || flat_enough(&q, tol) {
                if out.len() >= cap {
                    return (out, true);
                }
                let f = |i: usize| (fa[i] + (fb[i] - fa[i]) * t1) as f32;
                let p = q[3];
                let v = FlatVertex { p: [p.x as f32, p.y as f32], inner: f(0), outer: f(1) };
                if v.p.iter().all(|c| c.is_finite()) {
                    out.push(v);
                }
                continue;
            }
            // de Casteljau halving
            let [p0, p1, p2, p3] = q;
            let m01 = p0.lerp(p1, 0.5);
            let m12 = p1.lerp(p2, 0.5);
            let m23 = p2.lerp(p3, 0.5);
            let a = m01.lerp(m12, 0.5);
            let b = m12.lerp(m23, 0.5);
            let m = a.lerp(b, 0.5);
            let tm = (t0 + t1) * 0.5;
            // right half pushed first so the left half is emitted first
            stack.push(([m, b, m23, p3], tm, t1, depth + 1));
            stack.push(([p0, m01, a, m], t0, tm, depth + 1));
        }
    }
    (out, false)
}

/// Distance from `p` to segment `a`–`b`.
fn point_seg_dist(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let ab = b - a;
    let l2 = ab.x * ab.x + ab.y * ab.y;
    let t = if l2 > 0.0 { (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + ab * t)).length()
}

/// Both inner control points within `tol` of the chord segment: the curve lies in their convex
/// hull, so it is within `tol` of the chord (a straight segment is flat at once). Non-finite
/// geometry counts as flat so it stops subdividing.
fn flat_enough(q: &[Vec2; 4], tol: f64) -> bool {
    let [p0, p1, p2, p3] = *q;
    let d = point_seg_dist(p1, p0, p3).max(point_seg_dist(p2, p0, p3));
    !d.is_finite() || d <= tol
}

/// Squared distance from `(px, py)` to segment `a`–`b` and the closest point's parameter.
#[inline]
fn seg_dist2(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> (f32, f32) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (qx, qy) = (px - a[0], py - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { ((qx * dx + qy * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (qx - dx * t, qy - dy * t);
    (ex * ex + ey * ey, t)
}

impl Edge {
    /// The edge's coverage value at `(px, py)` for the winding `inside`, `None` beyond its band.
    #[inline]
    fn value(&self, px: f32, py: f32, inside: bool, expansion: f32) -> Option<f32> {
        let (d2, t) = seg_dist2(px, py, self.a.p, self.b.p);
        if d2 > self.band * self.band {
            return None;
        }
        let d = d2.sqrt();
        let s = if inside { d } else { -d } + expansion;
        let wi = self.a.inner + (self.b.inner - self.a.inner) * t;
        let wo = self.a.outer + (self.b.outer - self.a.outer) * t;
        Some(falloff(s - (wi - wo) * 0.5, wi + wo))
    }
    /// Whether the edge crosses the horizontal line at `y` (half-open rule): the crossing's `x`
    /// and winding direction.
    #[inline]
    pub fn crosses(&self, y: f32) -> Option<(f32, i32)> {
        let (a, b) = (self.a.p, self.b.p);
        if (a[1] <= y) == (b[1] <= y) {
            return None;
        }
        let t = (y - a[1]) / (b[1] - a[1]);
        Some((a[0] + (b[0] - a[0]) * t, if b[1] > a[1] { 1 } else { -1 }))
    }
}

impl PreparedShape {
    /// The outline's edges, sorted by the top of their band (empty when the shape has no area).
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    #[inline]
    fn value(&self, c: f32) -> f32 {
        self.opacity * if self.inverted { 1.0 - c } else { c }
    }

    /// Brute-force coverage of the shape at `(x, y)` (every edge; the definition the row
    /// algorithm implements).
    pub fn coverage_at(&self, x: f32, y: f32) -> f32 {
        if self.edges.is_empty() {
            return self.value(0.0);
        }
        let wind: i32 = self.edges.iter().filter_map(|e| e.crosses(y)).filter(|(cx, _)| *cx < x).map(|(_, d)| d).sum();
        let inside = wind != 0;
        let mut acc: f32 = if inside { 1.0 } else { 0.0 };
        for e in &self.edges {
            if let Some(v) = e.value(x, y, inside, self.expansion) {
                acc = if inside { acc.min(v) } else { acc.max(v) };
            }
        }
        self.value(acc)
    }

    /// The edges whose band overlaps rows `y0 .. y1` (pixel centres `y0 + 0.5 ..= y1 − 0.5`).
    fn band_edges(&self, y0: usize, y1: usize) -> Vec<&Edge> {
        let (lo, hi) = (y0 as f32 + 0.5, y1 as f32 - 0.5);
        let end = self.edges.partition_point(|e| e.reach[1] <= hi);
        self.edges.get(..end).unwrap_or_default().iter().filter(|e| e.reach[3] >= lo).collect()
    }

    /// Coverage of row `y` into `out` (length = frame width) from the band's candidate edges:
    /// winding from the row's sorted crossings, distances only against edges whose band reaches
    /// the pixel.
    fn row(&self, y: usize, cands: &[&Edge], out: &mut [f32]) {
        let outside = self.value(0.0);
        if self.edges.is_empty() {
            out.fill(outside);
            return;
        }
        let py = y as f32 + 0.5;
        let mut cross: Vec<(f32, i32)> = Vec::new();
        let mut near: Vec<&Edge> = Vec::new();
        for e in cands {
            if let Some(c) = e.crosses(py) {
                cross.push(c);
            }
            if py >= e.reach[1] && py <= e.reach[3] {
                near.push(e);
            }
        }
        if near.is_empty() {
            // no edge reaches this row: every crossing (if any) is far away, winding alone decides
            out.fill(outside);
            if cross.is_empty() {
                return;
            }
        }
        cross.sort_by(|p, q| p.0.total_cmp(&q.0));
        let (x_lo, x_hi) = near.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), e| (lo.min(e.reach[0]), hi.max(e.reach[2])));
        let mut ci = 0;
        let mut wind = 0;
        for (x, o) in out.iter_mut().enumerate() {
            let px = x as f32 + 0.5;
            while let Some(&(cx, dir)) = cross.get(ci)
                && cx < px
            {
                wind += dir;
                ci += 1;
            }
            let inside = wind != 0;
            if px < x_lo || px > x_hi {
                *o = self.value(if inside { 1.0 } else { 0.0 });
                continue;
            }
            let mut acc: f32 = if inside { 1.0 } else { 0.0 };
            for e in &near {
                if px < e.reach[0] || px > e.reach[2] {
                    continue;
                }
                if let Some(v) = e.value(px, py, inside, self.expansion) {
                    acc = if inside { acc.min(v) } else { acc.max(v) };
                }
            }
            *o = self.value(acc);
        }
    }
}

/// Combined coverage of the prepared roto at working pixel `(x, y)` (brute force; for tests,
/// picking and documentation). 0 when no shape is enabled.
pub fn coverage(roto: &PreparedRoto, x: f32, y: f32) -> f32 {
    let Some(first) = roto.shapes.first() else { return 0.0 };
    let mut acc = first.mode.start();
    for s in &roto.shapes {
        acc = s.mode.combine(acc, s.coverage_at(x, y)).clamp(0.0, 1.0);
    }
    acc
}

/// Rasterize the combined coverage into `out` (row-major `w × h`, one f32 per pixel; bands of
/// rows in parallel). Only the rows that fit in `out` are written; an empty frame writes nothing.
pub fn rasterize_into(roto: &PreparedRoto, out: &mut [f32], w: usize, h: usize) {
    if w == 0 || h == 0 {
        return;
    }
    let n = w.saturating_mul(h).min(out.len());
    let Some(region) = out.get_mut(..n) else { return };
    let Some(first) = roto.shapes.first() else {
        region.fill(0.0);
        return;
    };
    let start = first.mode.start();
    region.par_chunks_mut(w.saturating_mul(BAND_ROWS)).enumerate().for_each(|(band, rows)| {
        let y0 = band * BAND_ROWS;
        let y1 = y0 + rows.len().div_ceil(w);
        rows.fill(start);
        let mut tmp = vec![0.0f32; w];
        for s in &roto.shapes {
            let cands = s.band_edges(y0, y1);
            for (i, row) in rows.chunks_mut(w).enumerate() {
                let Some(t) = tmp.get_mut(..row.len()) else { continue };
                s.row(y0 + i, &cands, t);
                for (a, v) in row.iter_mut().zip(t.iter()) {
                    *a = s.mode.combine(*a, *v).clamp(0.0, 1.0);
                }
            }
        }
    });
}

/// The combined coverage as a new `w × h` buffer (empty when the frame is empty or larger than
/// [`MAX_PIXELS`]).
pub fn rasterize(roto: &PreparedRoto, w: usize, h: usize) -> Vec<f32> {
    let Some(n) = w.checked_mul(h).filter(|n| *n <= MAX_PIXELS) else { return Vec::new() };
    let mut out = vec![0.0f32; n];
    rasterize_into(roto, &mut out, w, h);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::roto::RotoShape;
    use filmcraft_project::{MaskVertex, ParamValue};

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> MaskPath {
        MaskPath::rect(x0, y0, x1, y1)
    }

    fn poly(pts: &[(f64, f64)]) -> MaskPath {
        MaskPath::polygon(&pts.iter().map(|&(x, y)| Vec2::new(x, y)).collect::<Vec<_>>())
    }

    fn roto(shapes: Vec<RotoShape>) -> Roto {
        Roto { shapes }
    }

    fn raster(r: &Roto, t: Tick, w: usize, h: usize) -> Vec<f32> {
        rasterize(&prepare(r, t, w, h, 1.0), w, h)
    }

    #[test]
    fn hard_square_covers_its_area() {
        let (x0, y0, x1, y1) = (10.3, 12.2, 50.7, 40.9);
        let r = roto(vec![RotoShape::new("s", RotoKind::Bezier, square(x0, y0, x1, y1))]);
        let c = raster(&r, Tick(0), 64, 64);
        let sum: f64 = c.iter().map(|v| f64::from(*v)).sum();
        let (area, perim) = ((x1 - x0) * (y1 - y0), 2.0 * ((x1 - x0) + (y1 - y0)));
        assert!((sum - area).abs() <= perim * 0.5, "{sum} vs {area}");
        assert!((sum - area).abs() < 0.5, "axis-aligned edges are box-filtered exactly: {sum} vs {area}");
        // inside 1, outside 0, a half-covered pixel 0.5
        assert_eq!(c[20 * 64 + 20], 1.0);
        assert_eq!(c[5 * 64 + 5], 0.0);
        let r = roto(vec![RotoShape::new("s", RotoKind::Bezier, square(10.0, 10.0, 20.5, 20.0))]);
        let c = raster(&r, Tick(0), 32, 32);
        assert!((c[15 * 32 + 20] - 0.5).abs() < 1e-5, "{}", c[15 * 32 + 20]);
    }

    #[test]
    fn straight_edges_flatten_to_their_corners() {
        // corner points: one chord per edge, not a subdivided line
        let r = roto(vec![RotoShape::new("s", RotoKind::Bezier, square(0.0, 0.0, 1000.0, 600.0))]);
        assert_eq!(prepare(&r, Tick(0), 8, 8, 1.0).shapes[0].pts.len(), 4);
        // a curved segment still subdivides
        let e = roto(vec![RotoShape::new("e", RotoKind::Bezier, MaskPath::ellipse(Vec2::new(50.0, 50.0), Vec2::new(40.0, 30.0)))]);
        assert!(prepare(&e, Tick(0), 8, 8, 1.0).shapes[0].pts.len() > 32);
    }

    #[test]
    fn per_point_feather_widens_the_ramp_near_its_point() {
        // point 0 (top-left) has a 20 px outer feather, the others none
        let mut pts = square(20.0, 30.0, 80.0, 70.0);
        pts.vertices[0] = pts.vertices[0].with_feather(0.0, 20.0);
        let r = roto(vec![RotoShape::new("s", RotoKind::Bezier, pts)]);
        let (w, h) = (100, 100);
        let c = raster(&r, Tick(0), w, h);
        let at = |x: usize, y: usize| c[y * w + x];
        // 6 px above the top edge: covered near point 0, not near point 1
        assert!(at(22, 24) > 0.3, "{}", at(22, 24));
        assert!(at(77, 24) == 0.0, "{}", at(77, 24));
        // the ramp near point 0 is monotonic and reaches 0 by 20 px out
        for y in 11..29 {
            assert!(at(21, y + 1) >= at(21, y), "monotonic at {y}");
        }
        assert_eq!(at(21, 8), 0.0);
        // still full inside, and the narrow side has the hard antialiased edge
        assert_eq!(at(50, 50), 1.0);
        assert_eq!((at(79, 60), at(80, 60)), (1.0, 0.0));
        // an inner feather ramps inside the outline instead
        let mut pts = square(20.0, 30.0, 80.0, 70.0);
        pts.vertices.iter_mut().for_each(|p| *p = p.with_feather(10.0, 0.0));
        let c = raster(&roto(vec![RotoShape::new("s", RotoKind::Bezier, pts)]), Tick(0), w, h);
        assert_eq!(c[50 * w + 15], 0.0);
        assert!(c[50 * w + 25] > 0.0 && c[50 * w + 25] < 1.0);
        assert_eq!(c[50 * w + 31], 1.0);
        // the global feather is symmetric around the edge
        let mut s = RotoShape::new("s", RotoKind::Bezier, square(20.0, -100.0, 200.0, 200.0));
        s.feather.value = ParamValue::Float(10.0);
        let c = raster(&roto(vec![s]), Tick(0), 60, 4);
        assert!((c[19] + c[20] - 1.0).abs() < 1e-5, "{} {}", c[19], c[20]);
        assert!(c[13] < 1e-6 && c[26] > 1.0 - 1e-6);
    }

    #[test]
    fn bspline_stays_inside_its_control_hull() {
        for closed in [true, false] {
            let mut p = square(10.0, 10.0, 50.0, 50.0);
            p.closed = closed;
            let s = RotoShape::new("b", RotoKind::BSpline, p);
            let p = prepare(&roto(vec![s]), Tick(0), 60, 60, 1.0);
            let pts = &p.shapes[0].pts;
            assert!(pts.len() > 16, "{}", pts.len());
            for v in pts {
                assert!((10.0..=50.0).contains(&v.p[0]) && (10.0..=50.0).contains(&v.p[1]), "{v:?}");
            }
            assert_eq!(coverage(&p, 30.0, 30.0), 1.0);
            if closed {
                assert_eq!(coverage(&p, 11.0, 11.0), 0.0, "corners are cut");
                // the middle of the top span: (P3 + 23·P0 + 23·P1 + P2) / 48
                assert!(pts.iter().any(|v| (v.p[0] - 30.0).abs() < 1e-3 && (v.p[1] - 560.0 / 48.0).abs() < 1e-3), "{pts:?}");
            } else {
                // an open one starts and ends on its end points
                assert!(pts.iter().any(|v| v.p == [10.0, 10.0]) && pts.iter().any(|v| v.p == [10.0, 50.0]));
            }
        }
    }

    #[test]
    fn keyframed_shapes_interpolate() {
        let a = square(0.0, 0.0, 10.0, 10.0);
        let b = a.transformed(&filmcraft_geom::Affine::translate(40.0, 0.0));
        let mut s = RotoShape::new("s", RotoKind::Bezier, a.clone());
        s.put_path_keyframe(Tick(0), a);
        s.put_path_keyframe(Tick(100), b);
        let r = roto(vec![s]);
        let at = |t: i64, x: f32| coverage(&prepare(&r, Tick(t), 60, 20, 1.0), x, 5.0);
        assert_eq!((at(0, 5.0), at(0, 25.0)), (1.0, 0.0));
        assert_eq!((at(50, 5.0), at(50, 25.0)), (0.0, 1.0));
        assert_eq!((at(100, 25.0), at(100, 45.0)), (0.0, 1.0));
    }

    #[test]
    fn modes_and_inversion_combine() {
        let big = RotoShape::new("big", RotoKind::Bezier, square(0.0, 0.0, 30.0, 30.0));
        let mut hole = RotoShape::new("hole", RotoKind::Bezier, square(10.0, 10.0, 20.0, 20.0));
        hole.mode = RotoMode::Subtract;
        let c = raster(&roto(vec![big.clone(), hole.clone()]), Tick(0), 30, 30);
        assert_eq!((c[15 * 30 + 15], c[5 * 30 + 5]), (0.0, 1.0));
        // intersect keeps only the overlap; replace discards what was above
        let mut i = RotoShape::new("i", RotoKind::Bezier, square(10.0, 0.0, 40.0, 30.0));
        i.mode = RotoMode::Intersect;
        let c = raster(&roto(vec![big.clone(), i]), Tick(0), 40, 30);
        assert_eq!((c[15 * 40 + 5], c[15 * 40 + 15], c[15 * 40 + 35]), (0.0, 1.0, 0.0));
        let mut rep = hole.clone();
        rep.mode = RotoMode::Replace;
        let c = raster(&roto(vec![big.clone(), rep]), Tick(0), 30, 30);
        assert_eq!((c[15 * 30 + 15], c[5 * 30 + 5]), (1.0, 0.0));
        // inverted at half opacity; a disabled shape is skipped; a first Subtract starts full
        let mut inv = big.clone();
        inv.inverted = true;
        inv.opacity.value = ParamValue::Float(50.0);
        let c = raster(&roto(vec![inv]), Tick(0), 40, 40);
        assert_eq!((c[15 * 40 + 15], c[35 * 40 + 35]), (0.0, 0.5));
        let mut off = big.clone();
        off.enabled = false;
        assert!(raster(&roto(vec![off]), Tick(0), 8, 8).iter().all(|v| *v == 0.0));
        let c = raster(&roto(vec![hole]), Tick(0), 30, 30);
        assert_eq!((c[15 * 30 + 15], c[2]), (0.0, 1.0));
    }

    #[test]
    fn row_algorithm_matches_brute_force() {
        let mut a = RotoShape::new("a", RotoKind::BSpline, poly(&[(5.0, 5.0), (40.0, 8.0), (44.0, 30.0), (20.0, 36.0), (8.0, 22.0)]));
        a.feather.value = ParamValue::Float(4.0);
        a.expansion.value = ParamValue::Float(-1.5);
        let mut pts = square(15.0, 12.0, 35.0, 28.0);
        pts.vertices[1] = pts.vertices[1].with_feather(3.0, 9.0);
        pts.vertices[1].t_out = Vec2::new(4.0, 6.0);
        pts.vertices[3] = pts.vertices[3].with_feather(1.0, 0.0);
        pts.closed = false;
        let mut b = RotoShape::new("b", RotoKind::Bezier, pts);
        b.mode = RotoMode::Subtract;
        let mut c = RotoShape::new("c", RotoKind::Bezier, square(30.0, 2.0, 46.0, 18.0));
        c.mode = RotoMode::Add;
        c.inverted = true;
        c.opacity.value = ParamValue::Float(70.0);
        // a thin, far shape that only some bands see
        let d = RotoShape::new("d", RotoKind::Bezier, square(2.0, 37.0, 9.0, 39.5));
        let (w, h) = (48, 40);
        let p = prepare(&roto(vec![a, b, c, d]), Tick(0), w, h, 0.9);
        let r = rasterize(&p, w, h);
        for y in 0..h {
            for x in 0..w {
                let v = coverage(&p, x as f32 + 0.5, y as f32 + 0.5);
                assert!((v - r[y * w + x]).abs() < 1e-5, "({x},{y}) {v} vs {}", r[y * w + x]);
            }
        }
    }

    #[test]
    fn hostile_shapes_and_frames_never_panic() {
        let mut bad = square(1.0, 1.0, 6.0, 6.0);
        bad.vertices[0].p.x = f64::NAN;
        bad.vertices[1].t_out = Vec2::new(f64::INFINITY, 1e300);
        bad.vertices[2] = bad.vertices[2].with_feather(f64::NAN, -4.0);
        let mut shapes = vec![
            RotoShape::new("bad", RotoKind::Bezier, bad.clone()),
            RotoShape::new("bad-b", RotoKind::BSpline, bad),
            RotoShape::new("two", RotoKind::Bezier, poly(&[(1.0, 1.0), (6.0, 1.0)])),
            RotoShape::new("none", RotoKind::BSpline, MaskPath::default()),
            RotoShape { shape: filmcraft_project::Param::new(ParamValue::Text("x".into())), ..RotoShape::default() },
        ];
        // huge tangents on many points: bounded flattening
        let many = MaskPath {
            vertices: (0..filmcraft_project::roto::MAX_POINTS)
                .map(|i| MaskVertex {
                    p: Vec2::new((i % 64) as f64, (i / 64) as f64),
                    t_in: Vec2::new(-1e6, 1e6),
                    t_out: Vec2::new(1e6, -1e6),
                    ..MaskVertex::default()
                })
                .collect(),
            closed: true,
        };
        shapes.push(RotoShape::new("many", RotoKind::Bezier, many));
        for (feather, expansion, opacity) in [(f64::NAN, f64::INFINITY, f64::NAN), (1e300, -1e300, 1e300), (-5.0, 3.0, -50.0), (2.0, 0.0, 100.0)] {
            for sh in &mut shapes {
                sh.feather.value = ParamValue::Float(feather);
                sh.expansion.value = ParamValue::Float(expansion);
                sh.opacity.value = ParamValue::Float(opacity);
                sh.inverted = !sh.inverted;
            }
            let r = roto(shapes.clone());
            for px in [1.0f32, 1e30, 1e-30, f32::NAN, f32::INFINITY, 0.0, -1.0] {
                for (w, h) in [(0, 0), (1, 1), (0, 5), (5, 0), (7, 3)] {
                    let p = prepare(&r, Tick(i64::MIN), w, h, px);
                    for s in &p.shapes {
                        assert!(s.pts.len() <= MAX_SHAPE_POINTS);
                        assert!(s.pts.iter().all(|v| v.p.iter().all(|c| c.is_finite()) && v.inner.is_finite() && v.outer.is_finite()));
                        assert!(s.edges.iter().all(|e| e.band.is_finite()) && s.expansion.is_finite() && (0.0..=1.0).contains(&s.opacity));
                    }
                    let c = rasterize(&p, w, h);
                    assert_eq!(c.len(), w * h);
                    assert!(c.iter().all(|v| (0.0..=1.0).contains(v)), "{c:?}");
                    // a short buffer is filled as far as it goes
                    let mut short = vec![9.0f32; (w * h) / 2];
                    rasterize_into(&p, &mut short, w, h);
                    assert!(short.iter().all(|v| (0.0..=1.0).contains(v)));
                    assert!((0.0..=1.0).contains(&coverage(&p, 0.5, 0.5)));
                }
            }
        }
        // fewer than three points: no area (the whole frame when inverted)
        let mut two = RotoShape::new("two", RotoKind::Bezier, poly(&[(0.0, 0.0), (8.0, 0.0)]));
        assert!(raster(&roto(vec![two.clone()]), Tick(0), 4, 4).iter().all(|v| *v == 0.0));
        two.inverted = true;
        assert!(raster(&roto(vec![two]), Tick(0), 4, 4).iter().all(|v| *v == 1.0));
        // an absurd frame is not allocated
        assert!(rasterize(&PreparedRoto::default(), usize::MAX, 2).is_empty());
        assert!(raster(&Roto::default(), Tick(0), 3, 3).iter().all(|v| *v == 0.0));
    }
}
