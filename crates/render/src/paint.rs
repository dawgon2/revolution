//! Paint strokes (CPU reference for C3.3; the GPU comp kernel of C3.6 lays the same dabs, [`plan`]).
//!
//! Every stroke active at a time ([`filmcraft_project::paint::StrokeDuration::is_active`]) is
//! applied in list order to a linear-light **premultiplied** RGBA f32 picture (the working image
//! format):
//!
//! 1. **Dabs.** Round dabs are laid along the stroke's polyline (working pixels = frame pixels ×
//!    `px_scale`), the first on its first point, then every `spacing × diameter` (diameter scaled
//!    by the pressure interpolated along the segment; at least half a pixel apart). Pressure also
//!    scales the dab's opacity. A dab of radius `r` and hardness `h` covers
//!
//!    ```text
//!    w = r · (1 − h),  c = r − w / 2
//!    u = clamp((c − d) / max(w, 1) + 0.5, 0, 1)
//!    k = lerp(u, smoothstep(u), min(w, 1))
//!    ```
//!
//!    at distance `d` from its centre: a smoothstep falloff from the hard core (`h·r`) to `r`, and
//!    a one-pixel antialiased edge at `r` for a hard brush (the mask feather formula).
//! 2. **Stroke matte.** The dabs of one stroke combine by **maximum** (not by repeated "over"), so
//!    overlapping dabs never build up beyond the brush opacity and the stroke has no spacing
//!    ridges; then the stroke is composited **once** in linear light with its matte `m`:
//!    - Color: `a = m · color.a`, `out = color.rgb · a + out · (1 − a)`, alpha `a + out.a · (1 − a)`
//!      ("over" with the straight-alpha colour);
//!    - Clone / Reveal / Erase: `out = lerp(out, T, m)` with `T` the clone source at
//!      `q + clone_offset` (rounded to whole working pixels; nothing where it falls outside the
//!      frame) and `clone_time_offset` later (the comp renders the node's input at those times,
//!      at most [`MAX_CLONE_TIMES`] distinct offsets per pass, [`clone_times`]; a stroke whose
//!      offset has no picture clones from the input at the same time), the reveal source (the node's second input; no reveal source = the stroke does
//!      nothing), or the node's input picture `src` (Erase undoes the strokes below).
//!      For an opaque `T` this is "over" as well.
//!
//! Bounded work: at most [`MAX_DABS_PER_STROKE`] dabs per stroke and [`MAX_DABS_TOTAL`] in all,
//! and dabs stop once their total clipped area reaches [`MAX_DAB_WORK`] pixel visits. Every value
//! read from the stroke is cleaned (non-finite points dropped, brush clamped). Short or missing
//! buffers count as transparent pixels; an empty or absurd frame gives an empty result.

use filmcraft_project::paint::{Brush, Paint, PaintPoint, PaintStroke, PaintTool};
use filmcraft_time::Tick;
use rayon::prelude::*;

/// Most dabs one stroke lays.
pub const MAX_DABS_PER_STROKE: usize = 100_000;
/// Most dabs all strokes of one [`apply`] lay.
pub const MAX_DABS_TOTAL: usize = 1_000_000;
/// Most pixel visits (sum of the dabs' clipped bounding boxes) of one [`apply`].
pub const MAX_DAB_WORK: u64 = 1 << 31;
/// Most dab positions one stroke walks (on- and off-frame), so a huge off-frame stroke ends.
const MAX_DAB_STEPS: usize = 4 * MAX_DABS_PER_STROKE;
/// Largest frame (pixels) [`apply`] allocates.
pub const MAX_PIXELS: usize = 256 * 1024 * 1024;
/// Largest working pixel scale honoured.
const MAX_PX_SCALE: f64 = 1e4;
/// Rows per band when dabs are bucketed for the parallel stroke matte.
const BAND_ROWS: usize = 32;
/// Most distinct non-zero clone time offsets one Paint pass renders its input at (C3.10); Clone
/// strokes with further offsets clone from the input at the same time.
pub const MAX_CLONE_TIMES: usize = 4;

/// The clone time offsets (C3.10) the Clone strokes of `paint` active at `at` need, in stroke
/// order: the first [`MAX_CLONE_TIMES`] distinct non-zero ones, and how many active Clone strokes
/// ask for an offset beyond those (they clone from the same time).
pub fn clone_times(paint: &Paint, at: Tick, frame: Tick) -> (Vec<Tick>, usize) {
    let mut times: Vec<Tick> = Vec::new();
    let mut over = 0usize;
    for s in paint.active_at(at, frame).take(filmcraft_project::paint::MAX_STROKES) {
        let dt = s.clone_time_offset;
        if s.tool != PaintTool::Clone || dt == Tick::ZERO || times.contains(&dt) {
            continue;
        }
        if times.len() < MAX_CLONE_TIMES {
            times.push(dt);
        } else {
            over = over.saturating_add(1);
        }
    }
    (times, over)
}

/// One dab in working pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub hardness: f32,
    /// Opacity × pressure, 0..1.
    pub weight: f32,
}

impl Dab {
    /// Coverage at distance `d` from the centre (before `weight`).
    #[inline]
    pub fn profile(&self, d: f32) -> f32 {
        let w = self.r * (1.0 - self.hardness);
        let c = self.r - w * 0.5;
        let u = ((c - d) / w.max(1.0) + 0.5).clamp(0.0, 1.0);
        let smooth = u * u * (3.0 - 2.0 * u);
        u + (smooth - u) * w.min(1.0)
    }
    /// Bounding box `[x0, y0, x1, y1)` clipped to a `w × h` frame, or `None` off the frame.
    fn bbox(&self, w: usize, h: usize) -> Option<[usize; 4]> {
        let reach = self.r + 1.0;
        let clip = |v: f32, hi: usize| v.clamp(0.0, hi as f32) as usize;
        let (x0, x1) = (clip((self.x - reach).floor(), w), clip((self.x + reach).ceil(), w));
        let (y0, y1) = (clip((self.y - reach).floor(), h), clip((self.y + reach).ceil(), h));
        (x0 < x1 && y0 < y1).then_some([x0, y0, x1, y1])
    }
}

fn clean_scale(px_scale: f32) -> f64 {
    let s = f64::from(px_scale);
    if s.is_finite() && s > 0.0 { s.min(MAX_PX_SCALE) } else { 0.0 }
}

/// The dabs of `stroke` in working pixels (`px_scale` working px per frame px), at most `cap`.
/// Off-frame dabs (for a `w × h` frame) are skipped.
pub fn dabs(stroke: &PaintStroke, px_scale: f32, w: usize, h: usize, cap: usize) -> Vec<Dab> {
    let s = clean_scale(px_scale);
    let brush: Brush = stroke.brush.cleaned();
    let cap = cap.min(MAX_DABS_PER_STROKE);
    let mut out = Vec::new();
    if s <= 0.0 || brush.size <= 0.0 || cap == 0 {
        return out;
    }
    let pts: Vec<PaintPoint> = stroke.points.iter().filter_map(|p| p.cleaned()).collect();
    let dia = brush.size * s;
    // the frame grown by the largest dab radius: dabs outside it cannot touch a pixel
    let reach = dia * 0.5 + 1.0;
    let rect = [-reach, -reach, w as f64 + reach, h as f64 + reach];
    let step = |pressure: f64| (brush.spacing * dia * pressure).max(0.5);
    let mut steps = 0usize;
    let mut emit = |x: f64, y: f64, pressure: f64, out: &mut Vec<Dab>| {
        steps += 1;
        let d = Dab { x: x as f32, y: y as f32, r: (dia * pressure * 0.5) as f32, hardness: brush.hardness as f32, weight: (brush.opacity * pressure) as f32 };
        if d.r > 0.0 && d.weight > 0.0 && d.x.is_finite() && d.y.is_finite() && d.bbox(w, h).is_some() {
            out.push(d);
        }
        out.len() < cap && steps < MAX_DAB_STEPS
    };
    let Some(first) = pts.first() else { return out };
    if inside(rect, first.x * s, first.y * s) && !emit(first.x * s, first.y * s, first.pressure, &mut out) {
        return out;
    }
    // distance along the current segment to the next dab
    let mut next = step(first.pressure);
    for seg in pts.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let (ax, ay, bx, by) = (a.x * s, a.y * s, b.x * s, b.y * s);
        let len = (bx - ax).hypot(by - ay);
        if !len.is_finite() {
            continue;
        }
        // only the part of the segment within the grown frame lays dabs; the spacing phase
        // advances past the rest in whole steps
        let st = step(a.pressure.max(b.pressure));
        if let Some((t0, t1)) = clip_segment(rect, ax, ay, bx, by) {
            next = skip_to(next, t0 * len, st);
            let end = t1 * len;
            while next <= end {
                let t = if len > 0.0 { next / len } else { 0.0 };
                let pressure = a.pressure + (b.pressure - a.pressure) * t;
                if !emit(ax + (bx - ax) * t, ay + (by - ay) * t, pressure, &mut out) {
                    return out;
                }
                next += step(pressure);
            }
        }
        next = skip_to(next, len, st) - len;
    }
    out
}

fn inside(r: [f64; 4], x: f64, y: f64) -> bool {
    x >= r[0] && y >= r[1] && x <= r[2] && y <= r[3]
}

/// `next` advanced by whole steps of `st` until it is at least `target`.
fn skip_to(next: f64, target: f64, st: f64) -> f64 {
    if next >= target || st <= 0.0 {
        return next;
    }
    let k = ((target - next) / st).ceil();
    let v = next + k * st;
    if v.is_finite() { v.max(target) } else { target }
}

/// The parameter range `[t0, t1]` ⊆ [0, 1] of segment `a`–`b` inside rectangle `r`
/// (Liang–Barsky clipping), `None` when it misses.
fn clip_segment(r: [f64; 4], ax: f64, ay: f64, bx: f64, by: f64) -> Option<(f64, f64)> {
    let (dx, dy) = (bx - ax, by - ay);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, ax - r[0]), (dx, r[2] - ax), (-dy, ay - r[1]), (dy, r[3] - ay)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 <= t1 && t0.is_finite() && t1.is_finite()).then_some((t0, t1))
}

/// Rasterize dabs into the stroke matte `m` by maximum: `m` covers the frame rectangle
/// `area = [x0, y0, x1, y1)` (row-major, `(x1 − x0) × (y1 − y0)`, cleared first), rows in parallel
/// bands. The dabs' boxes lie inside `area`.
fn stroke_matte(dabs: &[(Dab, [usize; 4])], m: &mut [f32], area: [usize; 4]) {
    m.fill(0.0);
    let (aw, ah) = (area[2].saturating_sub(area[0]), area[3].saturating_sub(area[1]));
    if dabs.is_empty() || aw == 0 || ah == 0 {
        return;
    }
    let bands = ah.div_ceil(BAND_ROWS);
    let mut by_band: Vec<Vec<u32>> = vec![Vec::new(); bands];
    for (i, (_, b)) in dabs.iter().enumerate() {
        let Ok(i) = u32::try_from(i) else { break };
        let (ly0, ly1) = (b[1].saturating_sub(area[1]), b[3].saturating_sub(area[1]));
        for band in (ly0 / BAND_ROWS)..ly1.div_ceil(BAND_ROWS).min(bands) {
            if let Some(v) = by_band.get_mut(band) {
                v.push(i);
            }
        }
    }
    m.par_chunks_mut(aw * BAND_ROWS).enumerate().for_each(|(band, rows)| {
        let Some(list) = by_band.get(band) else { return };
        let y_base = area[1] + band * BAND_ROWS;
        for &i in list {
            let Some((d, b)) = dabs.get(i as usize) else { continue };
            let y0 = b[1].max(y_base);
            let y1 = b[3].min(y_base + BAND_ROWS);
            for y in y0..y1 {
                let ly = y - y_base;
                let Some(row) = rows.get_mut(ly * aw..(ly + 1) * aw) else { continue };
                let py = y as f32 + 0.5 - d.y;
                for x in b[0]..b[2] {
                    let px = x as f32 + 0.5 - d.x;
                    let k = d.profile((px * px + py * py).sqrt()) * d.weight;
                    if let Some(o) = row.get_mut(x.saturating_sub(area[0]))
                        && k > *o
                    {
                        *o = k;
                    }
                }
            }
        }
    });
}

/// Apply the strokes of `paint` active at `at` to `src` (premultiplied linear RGBA, `w × h`):
/// see the module docs. `frame` is one frame's duration (for single-frame strokes),
/// `clones` the clone sources by clone time offset (a Clone stroke whose `clone_time_offset` has
/// none clones from `src`), `reveal_src` the node's second input; all must be `w × h` RGBA pictures in the same layout as `src` (pixels beyond a short buffer are left
/// unpainted). Returns the painted picture (`w·h·4` floats; empty for an empty or absurd frame).
pub fn apply(
    paint: &Paint,
    at: Tick,
    frame: Tick,
    px_scale: f32,
    src: &[f32],
    clones: &[(Tick, &[f32])],
    reveal_src: Option<&[f32]>,
    w: usize,
    h: usize,
) -> Vec<f32> {
    let Some(n) = w.checked_mul(h).filter(|n| *n > 0 && *n <= MAX_PIXELS) else { return Vec::new() };
    let mut out = vec![0.0f32; n * 4];
    for (o, s) in out.iter_mut().zip(src) {
        *o = if s.is_finite() { *s } else { 0.0 };
    }
    apply_into(paint, at, frame, px_scale, src, clones, reveal_src, w, h, &mut out);
    out
}

/// [`apply`] into `out`, which holds the picture to paint over (usually a copy of `src`); only
/// the `w × h` pixels that fit in `out` are touched. Each stroke only touches the union of its
/// dabs' boxes.
pub fn apply_into(
    paint: &Paint,
    at: Tick,
    frame: Tick,
    px_scale: f32,
    src: &[f32],
    clones: &[(Tick, &[f32])],
    reveal_src: Option<&[f32]>,
    w: usize,
    h: usize,
    out: &mut [f32],
) {
    if w.checked_mul(h).filter(|n| *n > 0 && *n <= MAX_PIXELS).is_none() {
        return;
    }
    // rows that fit in `out`
    let h = h.min(out.len() / (w * 4));
    if h == 0 {
        return;
    }
    let times: Vec<Tick> = clones.iter().map(|c| c.0).collect();
    let strokes = plan(paint, at, frame, px_scale, w, h, reveal_src.is_some(), &times);
    let mut matte: Vec<f32> = Vec::new();
    for st in &strokes {
        let target = match st.tool {
            PaintTool::Color => None,
            PaintTool::Clone => Some(st.source.checked_sub(1).and_then(|i| clones.get(i as usize)).map_or(src, |c| c.1)),
            PaintTool::Reveal => match reveal_src {
                Some(r) => Some(r),
                None => continue,
            },
            PaintTool::Erase => Some(src),
        };
        let area = st.area;
        let aw = area[2] - area[0];
        let Some(an) = aw.checked_mul(area[3] - area[1]) else { continue };
        matte.resize(an, 0.0);
        let Some(m) = matte.get_mut(..an) else { continue };
        stroke_matte(&st.dabs, m, area);
        let m: &[f32] = m;
        let color = st.color;
        let (ox, oy) = st.offset;
        let is_clone = st.tool == PaintTool::Clone;
        let Some(rows) = out.get_mut(area[1] * w * 4..area[3] * w * 4) else { continue };
        rows.par_chunks_mut(w * 4).zip(m.par_chunks(aw)).enumerate().for_each(|(ly, (row, mrow))| {
            let y = area[1] + ly;
            let Some(span) = row.get_mut(area[0] * 4..area[2] * 4) else { return };
            for (lx, (px, &m)) in span.as_chunks_mut::<4>().0.iter_mut().zip(mrow).enumerate() {
                if m <= 0.0 {
                    continue;
                }
                let x = area[0] + lx;
                match target {
                    None => {
                        let a = m * color[3];
                        for k in 0..3 {
                            px[k] = color[k] * a + px[k] * (1.0 - a);
                        }
                        px[3] = a + px[3] * (1.0 - a);
                    }
                    Some(t) => {
                        let idx = if is_clone { offset_index(x, y, ox, oy, w, h) } else { y.checked_mul(w).and_then(|v| v.checked_add(x)) };
                        let Some(tp) = idx.and_then(|i| t.get(i * 4..i * 4 + 4)) else { continue };
                        for k in 0..4 {
                            let tv = tp.get(k).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
                            px[k] += (tv - px[k]) * m;
                        }
                    }
                }
            }
        });
    }
}

/// One stroke of an [`apply`] ready to composite (C3.6: the CPU and the GPU kernel lay the same
/// dabs): its dabs with their boxes clipped to the frame, the union of those boxes, the cleaned
/// colour (Color), the clone offset in whole working pixels and the clone source.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedStroke {
    pub tool: PaintTool,
    pub color: [f32; 4],
    pub offset: (i64, i64),
    /// Clone source (C3.10): 0 the node's input at the same time, `i` the input at
    /// `clone_times[i − 1]` later (see [`plan`]).
    pub source: u32,
    /// Dabs and their boxes `[x0, y0, x1, y1)` (never empty).
    pub dabs: Vec<(Dab, [usize; 4])>,
    /// Union of the boxes.
    pub area: [usize; 4],
}

/// The strokes of `paint` active at `at` that [`apply`] lays on a `w × h` frame, in order, within
/// the dab and work budgets ([`MAX_DABS_TOTAL`], [`MAX_DAB_WORK`]). Reveal strokes are left out
/// without a reveal source (`has_reveal` false), and strokes without an on-frame dab. A Clone
/// stroke whose `clone_time_offset` is in `clone_times` gets that source (its position + 1), any
/// other stroke source 0.
#[allow(clippy::too_many_arguments)]
pub fn plan(paint: &Paint, at: Tick, frame: Tick, px_scale: f32, w: usize, h: usize, has_reveal: bool, clone_times: &[Tick]) -> Vec<PlannedStroke> {
    let mut planned = Vec::new();
    if w.checked_mul(h).filter(|n| *n > 0 && *n <= MAX_PIXELS).is_none() {
        return planned;
    }
    let s = clean_scale(px_scale);
    let (mut total, mut work) = (0usize, 0u64);
    for stroke in paint.active_at(at, frame).take(filmcraft_project::paint::MAX_STROKES) {
        if total >= MAX_DABS_TOTAL || work >= MAX_DAB_WORK {
            break;
        }
        if stroke.tool == PaintTool::Reveal && !has_reveal {
            continue;
        }
        let mut list = Vec::new();
        let mut area = [usize::MAX, usize::MAX, 0, 0];
        for d in dabs(stroke, px_scale, w, h, MAX_DABS_TOTAL - total) {
            let Some(b) = d.bbox(w, h) else { continue };
            let px = ((b[2] - b[0]) as u64).saturating_mul((b[3] - b[1]) as u64);
            if work.saturating_add(px) > MAX_DAB_WORK {
                work = MAX_DAB_WORK;
                break;
            }
            work += px;
            area = [area[0].min(b[0]), area[1].min(b[1]), area[2].max(b[2]), area[3].max(b[3])];
            list.push((d, b));
        }
        total += list.len();
        if list.is_empty() {
            continue;
        }
        let offset = (round_offset(stroke.clone_offset.x * s), round_offset(stroke.clone_offset.y * s));
        let source = match stroke.tool {
            PaintTool::Clone => {
                clone_times.iter().position(|t| *t == stroke.clone_time_offset).and_then(|i| u32::try_from(i).ok()).map_or(0, |i| i.saturating_add(1))
            }
            _ => 0,
        };
        planned.push(PlannedStroke { tool: stroke.tool, color: clean_color(stroke.color), offset, source, dabs: list, area });
    }
    planned
}

fn clean_color(c: [f32; 4]) -> [f32; 4] {
    let m = filmcraft_project::paint::MAX_COLOR;
    let f = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { 0.0 };
    [f(c[0], -m, m), f(c[1], -m, m), f(c[2], -m, m), f(c[3], 0.0, 1.0)]
}

fn round_offset(v: f64) -> i64 {
    if v.is_finite() { v.round().clamp(-1e12, 1e12) as i64 } else { 0 }
}

/// Pixel index of `(x + ox, y + oy)` in a `w × h` frame, `None` outside it.
fn offset_index(x: usize, y: usize, ox: i64, oy: i64, w: usize, h: usize) -> Option<usize> {
    let sx = i64::try_from(x).ok()?.checked_add(ox)?;
    let sy = i64::try_from(y).ok()?.checked_add(oy)?;
    let (sx, sy) = (usize::try_from(sx).ok()?, usize::try_from(sy).ok()?);
    (sx < w && sy < h).then(|| sy.checked_mul(w)?.checked_add(sx)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_geom::Vec2;
    use filmcraft_project::paint::{PaintStroke, StrokeDuration};

    const F: Tick = Tick(100);

    fn line(tool: PaintTool, a: (f64, f64), b: (f64, f64), size: f64, hardness: f64) -> PaintStroke {
        PaintStroke::new(tool, vec![PaintPoint::new(a.0, a.1), PaintPoint::new(b.0, b.1)], Brush { size, hardness, opacity: 1.0, spacing: 0.05 })
    }

    fn run(p: &Paint, src: &[f32], clone: Option<&[f32]>, reveal: Option<&[f32]>, w: usize, h: usize) -> Vec<f32> {
        let clones: Vec<(Tick, &[f32])> = clone.map(|c| (Tick::ZERO, c)).into_iter().collect();
        apply(p, Tick(0), F, 1.0, src, &clones, reveal, w, h)
    }

    /// A smooth, distinct premultiplied picture.
    fn gradient(w: usize, h: usize) -> Vec<f32> {
        (0..w * h).flat_map(|i| [(i % w) as f32 / w as f32, (i / w) as f32 / h as f32, 0.25, 1.0]).collect()
    }

    fn alpha_sum(img: &[f32]) -> f64 {
        img.as_chunks::<4>().0.iter().map(|p| f64::from(p[3])).sum()
    }

    #[test]
    fn colour_stroke_footprint_is_size_times_length() {
        let (w, h) = (200, 60);
        let src = vec![0.0f32; w * h * 4];
        let p = Paint { strokes: vec![line(PaintTool::Color, (40.0, 30.0), (160.0, 30.0), 20.0, 1.0)] };
        let out = run(&p, &src, None, None, w, h);
        let expect = 20.0 * 120.0 + std::f64::consts::PI * 100.0;
        let got = alpha_sum(&out);
        assert!((got - expect).abs() / expect < 0.01, "{got} vs {expect}");
        // white, premultiplied: rgb = alpha
        let i = (30 * w + 100) * 4;
        assert_eq!(&out[i..i + 4], &[1.0, 1.0, 1.0, 1.0]);
        // a half-opacity brush never builds up beyond its opacity
        let mut half = p.clone();
        half.strokes[0].brush.opacity = 0.5;
        let out = run(&half, &src, None, None, w, h);
        assert!(out.as_chunks::<4>().0.iter().all(|px| px[3] <= 0.5 + 1e-6));
        assert!((out[i + 3] - 0.5).abs() < 1e-6);
        // pressure scales the size
        let mut light = p.clone();
        light.strokes[0].points.iter_mut().for_each(|q| q.pressure = 0.5);
        let got = alpha_sum(&run(&light, &src, None, None, w, h));
        let expect = 0.5 * (10.0 * 120.0 + std::f64::consts::PI * 25.0);
        assert!((got - expect).abs() / expect < 0.03, "{got} vs {expect}");
    }

    #[test]
    fn hardness_shapes_the_dab() {
        let (w, h) = (40, 40);
        let src = vec![0.0f32; w * h * 4];
        let dot = |hardness: f64| {
            let s = PaintStroke::new(PaintTool::Color, vec![PaintPoint::new(20.0, 20.0)], Brush { size: 20.0, hardness, opacity: 1.0, spacing: 0.1 });
            run(&Paint { strokes: vec![s] }, &src, None, None, w, h)
        };
        let (hard, soft) = (dot(1.0), dot(0.0));
        let a = |img: &[f32], x: usize, y: usize| img[(y * w + x) * 4 + 3];
        // centre pixel (19.5..20.5 holds the dab centre at 20.0, 20.0: pixel 19 or 20 is ~0.7 px away)
        assert!(a(&hard, 20, 20) == 1.0 && a(&soft, 20, 20) > 0.95);
        // 7.5 px out: hard is full, soft has fallen off
        assert_eq!(a(&hard, 27, 20), 1.0);
        assert!(a(&soft, 27, 20) < 0.25, "{}", a(&soft, 27, 20));
        // outside the radius both are empty; the hard one's area is the disc's
        assert_eq!((a(&hard, 31, 20), a(&soft, 31, 20)), (0.0, 0.0));
        assert!((alpha_sum(&hard) - std::f64::consts::PI * 100.0).abs() < 1.5, "{}", alpha_sum(&hard));
        assert!(alpha_sum(&soft) < alpha_sum(&hard) * 0.5);
    }

    #[test]
    fn erase_undoes_colour() {
        let (w, h) = (64, 40);
        let src = gradient(w, h);
        let mut red = line(PaintTool::Color, (10.0, 20.0), (50.0, 20.0), 12.0, 0.5);
        red.color = [1.0, 0.0, 0.0, 0.8];
        let painted = run(&Paint { strokes: vec![red.clone()] }, &src, None, None, w, h);
        assert_ne!(painted, src);
        let erase = line(PaintTool::Erase, (10.0, 20.0), (50.0, 20.0), 30.0, 1.0);
        let out = run(&Paint { strokes: vec![red, erase] }, &src, None, None, w, h);
        for (a, b) in out.iter().zip(&src) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn clone_copies_the_offset_region() {
        let (w, h) = (100, 80);
        let src = gradient(w, h);
        let mut c = line(PaintTool::Clone, (40.0, 40.0), (50.0, 40.0), 10.0, 1.0);
        c.clone_offset = Vec2::new(20.0, -10.0);
        let out = run(&Paint { strokes: vec![c.clone()] }, &src, None, None, w, h);
        let px = |img: &[f32], x: usize, y: usize| img[(y * w + x) * 4..(y * w + x) * 4 + 4].to_vec();
        assert_eq!(px(&out, 45, 40), px(&src, 65, 30));
        assert_eq!(px(&out, 41, 38), px(&src, 61, 28));
        assert_eq!(px(&out, 10, 10), px(&src, 10, 10), "outside the stroke");
        // another clone source; an offset that falls outside the frame leaves the pixel alone
        let other = vec![0.5f32; w * h * 4];
        let out = run(&Paint { strokes: vec![c.clone()] }, &src, Some(&other), None, w, h);
        assert_eq!(px(&out, 45, 40), vec![0.5; 4]);
        c.clone_offset = Vec2::new(1e9, 0.0);
        assert_eq!(run(&Paint { strokes: vec![c] }, &src, None, None, w, h), src);
    }

    #[test]
    fn reveal_shows_the_second_input() {
        let (w, h) = (50, 30);
        let src = gradient(w, h);
        let red: Vec<f32> = (0..w * h).flat_map(|_| [1.0, 0.0, 0.0, 1.0]).collect();
        let s = line(PaintTool::Reveal, (10.0, 15.0), (40.0, 15.0), 8.0, 1.0);
        let out = run(&Paint { strokes: vec![s.clone()] }, &src, None, Some(&red), w, h);
        let i = (15 * w + 25) * 4;
        assert_eq!(&out[i..i + 4], &[1.0, 0.0, 0.0, 1.0]);
        assert_eq!(&out[..4], &src[..4]);
        // no second input: nothing happens
        assert_eq!(run(&Paint { strokes: vec![s] }, &src, None, None, w, h), src);
    }

    #[test]
    fn hold_and_single_frame_strokes_show_in_their_range() {
        let (w, h) = (20, 20);
        let src = vec![0.0f32; w * h * 4];
        let mut one = PaintStroke::new(PaintTool::Color, vec![PaintPoint::new(5.0, 5.0)], Brush { size: 6.0, hardness: 1.0, ..Brush::default() });
        one.duration = StrokeDuration::SingleFrame { at: Tick(200) };
        let mut hold = PaintStroke::new(PaintTool::Color, vec![PaintPoint::new(15.0, 15.0)], Brush { size: 6.0, hardness: 1.0, ..Brush::default() });
        hold.duration = StrokeDuration::Hold { from: Tick(100), to: Some(Tick(400)) };
        let p = Paint { strokes: vec![one, hold] };
        let shown = |t: i64| {
            let out = apply(&p, Tick(t), F, 1.0, &src, &[], None, w, h);
            (out[(5 * w + 5) * 4 + 3] > 0.5, out[(15 * w + 15) * 4 + 3] > 0.5)
        };
        assert_eq!(shown(0), (false, false));
        assert_eq!(shown(100), (false, true));
        assert_eq!(shown(250), (true, true));
        assert_eq!(shown(300), (false, true));
        assert_eq!(shown(400), (false, false));
    }

    #[test]
    fn off_frame_parts_do_not_use_up_the_dab_budget() {
        // starts ten million pixels off the frame with a tiny spacing: the on-frame part still paints
        let (w, h) = (120, 40);
        let src = vec![0.0f32; w * h * 4];
        let mut s = line(PaintTool::Color, (-1e7, 20.0), (100.0, 20.0), 6.0, 1.0);
        s.points.push(PaintPoint::new(100.0, 1e7));
        s.brush.spacing = 0.01;
        let out = run(&Paint { strokes: vec![s.clone()] }, &src, None, None, w, h);
        for x in [1, 50, 99] {
            assert_eq!(out[(20 * w + x) * 4 + 3], 1.0, "x = {x}");
        }
        assert_eq!(out[(30 * w + 99) * 4 + 3], 1.0, "the off-frame end comes back into the frame");
        assert!(dabs(&s, 1.0, w, h, usize::MAX).len() < 2_000);
        // a stroke entirely off the frame lays no dabs
        let off = line(PaintTool::Color, (-500.0, -500.0), (-500.0, 5e6), 6.0, 1.0);
        assert!(dabs(&off, 1.0, w, h, usize::MAX).is_empty());
    }

    #[test]
    fn many_small_strokes_stay_fast() {
        let (w, h) = (1920, 1080);
        let src = vec![0.25f32; w * h * 4];
        let strokes = (0..500)
            .map(|i| {
                let (x, y) = (f64::from(i % 50) * 38.0 + 10.0, f64::from(i / 50) * 100.0 + 20.0);
                line(if i % 3 == 0 { PaintTool::Erase } else { PaintTool::Color }, (x, y), (x + 20.0, y + 10.0), 8.0, 0.7)
            })
            .collect();
        let p = Paint { strokes };
        let t0 = std::time::Instant::now();
        let out = run(&p, &src, None, None, w, h);
        let dt = t0.elapsed();
        assert_eq!(out.len(), w * h * 4);
        assert!(dt.as_secs_f64() < 3.0, "{dt:?}");
    }

    #[test]
    fn hostile_strokes_and_frames_never_panic() {
        let nan = PaintStroke {
            tool: PaintTool::Color,
            points: vec![
                PaintPoint { x: f64::NAN, y: 1.0, pressure: f64::NAN },
                PaintPoint { x: 1e300, y: -1e300, pressure: 1e300 },
                PaintPoint { x: 2.0, y: 2.0, pressure: -1.0 },
            ],
            brush: Brush { size: f64::INFINITY, hardness: f64::NAN, opacity: -3.0, spacing: f64::NAN },
            color: [f32::NAN, f32::INFINITY, -1e38, f32::NAN],
            clone_offset: Vec2::new(f64::NAN, f64::INFINITY),
            ..PaintStroke::default()
        };
        let neg = PaintStroke::new(PaintTool::Color, vec![PaintPoint::new(1.0, 1.0); 3], Brush { size: -10.0, ..Brush::default() });
        // a very long stroke with a tiny spacing: bounded dabs
        let long = PaintStroke::new(
            PaintTool::Erase,
            vec![PaintPoint::new(-1e7, 3.0), PaintPoint::new(1e7, 3.0), PaintPoint::new(-1e7, 2.0)],
            Brush { size: 1.0, hardness: 1.0, opacity: 1.0, spacing: 0.0 },
        );
        assert!(dabs(&long, 1.0, 8, 8, usize::MAX).len() <= MAX_DABS_PER_STROKE);
        assert!(dabs(&long, 1e30, 8, 8, usize::MAX).len() <= MAX_DABS_PER_STROKE);
        let mut tools = vec![nan, neg, long];
        for t in [PaintTool::Clone, PaintTool::Reveal, PaintTool::Erase] {
            let mut s = line(t, (0.0, 0.0), (5.0, 5.0), 3.0, 0.5);
            s.clone_offset = Vec2::new(-1e300, 1e300);
            tools.push(s);
        }
        let p = Paint { strokes: tools };
        for (w, h) in [(0, 0), (1, 1), (0, 4), (4, 0), (7, 5), (usize::MAX, 2)] {
            for px in [1.0f32, 0.0, -1.0, f32::NAN, f32::INFINITY, 1e30] {
                let n = w.checked_mul(h).filter(|n| *n <= 64).unwrap_or(0);
                let src = vec![f32::NAN; n * 4];
                let short = vec![0.5f32; n];
                let out = apply(&p, Tick(0), Tick(0), px, &src, &[(Tick::ZERO, &short), (Tick(i64::MIN), &short)], Some(&short), w, h);
                assert_eq!(out.len(), n * 4, "{w}×{h}");
                assert!(out.iter().all(|v| v.is_finite()), "{w}×{h} {px}: {out:?}");
                // a short output buffer is painted as far as it goes
                let mut o = vec![0.0f32; n * 2];
                apply_into(&p, Tick(0), Tick(0), px, &[], &[], None, w, h, &mut o);
                assert!(o.iter().all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    fn clone_strokes_pick_their_time_source() {
        let (w, h) = (60, 40);
        let src = gradient(w, h);
        let before = vec![0.25f32; w * h * 4];
        let after = vec![0.75f32; w * h * 4];
        let stroke = |dt: i64, y: f64| {
            let mut c = line(PaintTool::Clone, (10.0, y), (50.0, y), 6.0, 1.0);
            c.clone_time_offset = Tick(dt);
            c
        };
        let p = Paint { strokes: vec![stroke(-F.0, 10.0), stroke(F.0, 20.0), stroke(0, 30.0), stroke(7 * F.0, 35.0)] };
        let out = apply(&p, Tick(0), F, 1.0, &src, &[(Tick(-F.0), &before), (F, &after)], None, w, h);
        let px = |img: &[f32], x: usize, y: usize| img[(y * w + x) * 4..(y * w + x) * 4 + 4].to_vec();
        assert_eq!(px(&out, 30, 10), vec![0.25; 4], "the frame before");
        assert_eq!(px(&out, 30, 20), vec![0.75; 4], "the frame after");
        assert_eq!(px(&out, 30, 30), px(&src, 30, 30), "offset 0: the same frame");
        assert_eq!(px(&out, 30, 35), px(&src, 30, 35), "an offset without a picture: the same frame");
        // the times a pass needs: distinct, non-zero, in order, capped
        let many = Paint { strokes: (0..10).map(|i| stroke(i % 7 - 3, 5.0)).chain([line(PaintTool::Color, (1.0, 1.0), (2.0, 2.0), 2.0, 1.0)]).collect() };
        let (times, over) = clone_times(&many, Tick(0), F);
        assert_eq!(times, vec![Tick(-3), Tick(-2), Tick(-1), Tick(1)]);
        assert_eq!(over, 2, "offsets 2 and 3 fall back");
        let planned = plan(&many, Tick(0), F, 1.0, w, h, false, &times);
        let sources: Vec<u32> = planned.iter().map(|s| s.source).collect();
        assert_eq!(sources, vec![1, 2, 3, 0, 4, 0, 0, 1, 2, 3, 0]);
        // hostile offsets: never panic
        let hostile = Paint { strokes: vec![stroke(i64::MIN, 10.0), stroke(i64::MAX, 20.0)] };
        let (times, _) = clone_times(&hostile, Tick(0), F);
        let out = apply(&hostile, Tick(0), F, 1.0, &src, &times.iter().map(|t| (*t, before.as_slice())).collect::<Vec<_>>(), None, w, h);
        assert_eq!(px(&out, 30, 10), vec![0.25; 4]);
    }
}
