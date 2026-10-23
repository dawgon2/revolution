//! Mask tracking: frame-to-frame motion of the picture inside a mask.
//!
//! The method (all textbook, implemented from the papers' equations):
//!
//! 1. **Features** — Shi–Tomasi "good features to track" (Shi & Tomasi, CVPR 1994): the smaller
//!    eigenvalue of the 2×2 gradient structure tensor summed over a 7×7 window, kept above 1 % of
//!    the strongest response inside the mask (shrunk by the window radius), with non-maximum
//!    suppression and a minimum spacing; at most [`MAX_FEATURES`].
//! 2. **Tracking** — pyramidal Lucas–Kanade (Bouguet's formulation of Lucas & Kanade 1981):
//!    a 4-level image pyramid (5-tap binomial blur + 2× decimation), a 15×15 window, Gauss–Newton
//!    iterations per level with bilinear sampling, the estimate doubled down the pyramid.
//!    Each feature is tracked forward and then backward; it is kept only when the backward track
//!    returns within 0.5 px of where it started (forward–backward error, Kalal et al. 2010).
//! 3. **Fit** — RANSAC (Fischler & Bolles 1981) over minimal samples (1 point for Position,
//!    2 for Position & Rotation and Position, Scale & Rotation) with a 1 px inlier threshold and
//!    a fixed-seed generator (deterministic), then a least-squares refit on the inliers:
//!    translation = mean displacement, similarity = the closed-form 2D Umeyama / Procrustes
//!    solution, rigid = the same with the scale normalised away.
//!
//! The result is the transform mapping frame A's pixels to frame B's; the engine applies it to
//! the mask path to write the next Mask Path keyframe.

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::TrackMethod;
use rayon::prelude::*;

/// Most features tracked per frame.
pub const MAX_FEATURES: usize = 160;
const WIN: i32 = 7;
const LEVELS: usize = 4;
pub(crate) const FB_MAX: f32 = 0.5;
const RANSAC_THRESHOLD: f64 = 1.0;
const RANSAC_ITERS: usize = 300;

/// A single-channel f32 image (0..1, display-referred luma).
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Gray {
    /// A `w`×`h` image from row-major pixels. None when `px` doesn't hold exactly `w`×`h`
    /// pixels (or the size overflows).
    pub fn new(w: usize, h: usize, px: Vec<f32>) -> Option<Self> {
        (w.checked_mul(h) == Some(px.len())).then_some(Self { w, h, px })
    }
    /// Luma of 8-bit RGBA (Rec. 709 weights on the encoded values).
    pub fn from_rgba8(w: usize, h: usize, rgba: &[u8]) -> Self {
        let px = rgba.as_chunks::<4>().0.iter().map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0).collect();
        Self { w, h, px }
    }
    #[inline]
    fn at(&self, x: i32, y: i32) -> f32 {
        // total for any (pub) field values: an empty or short image reads 0 instead of panicking
        let (Some(mx), Some(my)) = (self.w.checked_sub(1), self.h.checked_sub(1)) else { return 0.0 };
        let x = usize::try_from(x).unwrap_or(0).min(mx);
        let y = usize::try_from(y).unwrap_or(0).min(my);
        y.checked_mul(self.w).and_then(|i| i.checked_add(x)).and_then(|i| self.px.get(i)).copied().unwrap_or(0.0)
    }
    /// Bilinear sample at continuous coordinates (pixel centres at integers), clamped.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        // far-off coordinates read the clamped edge anyway; bounding them first keeps the
        // integer corner arithmetic below from overflowing on a diverged estimate
        let (x, y) = (x.clamp(-2.0, self.w as f32 + 1.0), y.clamp(-2.0, self.h as f32 + 1.0));
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let a = self.at(x0, y0);
        let b = self.at(x0 + 1, y0);
        let c = self.at(x0, y0 + 1);
        let d = self.at(x0 + 1, y0 + 1);
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        top + (bot - top) * fy
    }
    /// 5-tap binomial blur then 2× decimation.
    pub(crate) fn down(&self) -> Gray {
        let k = [1.0, 4.0, 6.0, 4.0, 1.0].map(|v: f32| v / 16.0);
        let (w, h) = (self.w, self.h);
        if w == 0 || h == 0 || w.checked_mul(h) != Some(self.px.len()) {
            return Gray { w: 0, h: 0, px: Vec::new() };
        }
        let mut tmp = vec![0.0f32; w * h];
        tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = (0..5).map(|i| k[i] * self.at(x as i32 + i as i32 - 2, y as i32)).sum();
            }
        });
        let t = Gray { w, h, px: tmp };
        let (nw, nh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
        let mut px = vec![0.0f32; nw * nh];
        px.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = (0..5).map(|i| k[i] * t.at(2 * x as i32, 2 * y as i32 + i as i32 - 2)).sum();
            }
        });
        Gray { w: nw, h: nh, px }
    }
    fn pyramid(&self) -> Vec<Gray> {
        let mut v = vec![self.clone()];
        while v.len() < LEVELS && v.last().is_some_and(|g| g.w >= 32 && g.h >= 32) {
            let Some(n) = v.last().map(Gray::down) else { break };
            v.push(n);
        }
        v
    }
    /// Central-difference gradient.
    #[inline]
    fn grad(&self, x: i32, y: i32) -> (f32, f32) {
        ((self.at(x + 1, y) - self.at(x - 1, y)) * 0.5, (self.at(x, y + 1) - self.at(x, y - 1)) * 0.5)
    }
}

fn point_in(poly: &[Vec2], x: f64, y: f64) -> bool {
    let n = poly.len();
    let mut w = 0;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.y <= y) != (b.y <= y) && a.x + (b.x - a.x) * (y - a.y) / (b.y - a.y) < x {
            w += if b.y > a.y { 1 } else { -1 };
        }
    }
    w != 0
}

fn dist_to_poly(poly: &[Vec2], p: Vec2) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            let d = b - a;
            let l2 = d.x * d.x + d.y * d.y;
            let t = if l2 > 0.0 { (((p - a).x * d.x + (p - a).y * d.y) / l2).clamp(0.0, 1.0) } else { 0.0 };
            (p - (a + d * t)).length()
        })
        .fold(f64::INFINITY, f64::min)
}

/// Shi–Tomasi corners inside `region` (a polygon in `g`'s pixels), strongest first.
pub fn features(g: &Gray, region: &[Vec2], max: usize, min_dist: f32) -> Vec<[f32; 2]> {
    if region.len() < 3 {
        return Vec::new();
    }
    let lo = region.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |a, p| Vec2::new(a.x.min(p.x), a.y.min(p.y)));
    let hi = region.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, p| Vec2::new(a.x.max(p.x), a.y.max(p.y)));
    let margin = WIN + 2;
    let x0 = (lo.x.floor() as i32).max(margin);
    let y0 = (lo.y.floor() as i32).max(margin);
    let x1 = (hi.x.ceil() as i32).min(g.w as i32 - 1 - margin);
    let y1 = (hi.y.ceil() as i32).min(g.h as i32 - 1 - margin);
    if x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    let (bw, bh) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
    // structure tensor response (min eigenvalue) over a 7×7 window, at pixels inside the region
    let r = 3;
    let resp: Vec<f32> = (0..bw * bh)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (x0 + (i % bw) as i32, y0 + (i / bw) as i32);
            let p = Vec2::new(x as f64, y as f64);
            if !point_in(region, p.x, p.y) || dist_to_poly(region, p) < r as f64 {
                return 0.0;
            }
            let (mut a, mut b, mut c) = (0.0f32, 0.0f32, 0.0f32);
            for dy in -r..=r {
                for dx in -r..=r {
                    let (gx, gy) = g.grad(x + dx, y + dy);
                    a += gx * gx;
                    b += gx * gy;
                    c += gy * gy;
                }
            }
            let tr = (a + c) * 0.5;
            tr - ((a - c) * (a - c) * 0.25 + b * b).sqrt()
        })
        .collect();
    let best = resp.iter().copied().fold(0.0f32, f32::max);
    if best <= 1e-9 {
        return Vec::new();
    }
    let thr = best * 0.01;
    // 3×3 non-maximum suppression
    let mut cand: Vec<(f32, i32, i32)> = Vec::new();
    for yy in 1..bh.saturating_sub(1) {
        for xx in 1..bw.saturating_sub(1) {
            let v = resp[yy * bw + xx];
            if v < thr {
                continue;
            }
            let mut is_max = true;
            'n: for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if (dx != 0 || dy != 0) && resp[(yy as i32 + dy) as usize * bw + (xx as i32 + dx) as usize] > v {
                        is_max = false;
                        break 'n;
                    }
                }
            }
            if is_max {
                cand.push((v, x0 + xx as i32, y0 + yy as i32));
            }
        }
    }
    cand.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut out: Vec<[f32; 2]> = Vec::new();
    let md2 = min_dist * min_dist;
    for (_, x, y) in cand {
        let p = [x as f32, y as f32];
        if out.iter().all(|q| (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) >= md2) {
            out.push(p);
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Region bounding-box pixels above which [`features_in`] looks on a coarser pyramid level.
const FEATURE_BUDGET_PX: f64 = (1u64 << 20) as f64;

/// [`features`] of `region` (full-resolution pixels) in the prepared frame `p`, found on the finest
/// pyramid level whose share of the region's bounding box is at most [`FEATURE_BUDGET_PX`]
/// pixels, returned in full-resolution pixels. A large region of a 6K frame then costs about what
/// one of an HD frame does (the structure tensor is 49 gradients per pixel); LK refines from there.
pub fn features_in(p: &Prepared, region: &[Vec2], max: usize, min_dist: f32) -> Vec<[f32; 2]> {
    let g = p.base();
    let lo = region.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |a, q| Vec2::new(a.x.min(q.x), a.y.min(q.y)));
    let hi = region.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, q| Vec2::new(a.x.max(q.x), a.y.max(q.y)));
    let bw = (hi.x.min(g.w as f64) - lo.x.max(0.0)).max(0.0);
    let bh = (hi.y.min(g.h as f64) - lo.y.max(0.0)).max(0.0);
    let mut area = if (bw * bh).is_finite() { bw * bh } else { 0.0 };
    let mut l = 0;
    while l + 1 < p.levels.len() && area > FEATURE_BUDGET_PX {
        l += 1;
        area /= 4.0;
    }
    let Some(lg) = p.levels.get(l) else { return Vec::new() };
    if l == 0 {
        return features(lg, region, max, min_dist);
    }
    let s = (1u32 << l.min(16)) as f32;
    let scaled: Vec<Vec2> = region.iter().map(|q| *q * (1.0 / s as f64)).collect();
    features(lg, &scaled, max, (min_dist / s).max(1.0)).into_iter().map(|f| [f[0] * s, f[1] * s]).collect()
}

/// Pyramidal Lucas–Kanade: where `p` in `a` moved to in `b` (pyramids from [`Gray::pyramid`]).
fn lk(pa: &[Gray], pb: &[Gray], p: [f32; 2]) -> Option<[f32; 2]> {
    lk_from(pa, pb, p, [0.0, 0.0])
}

/// [`lk`] starting from a guessed displacement `init` (full-resolution pixels; Bouguet's initial
/// guess, scaled to the coarsest level). None when the window is flat or the estimate diverges.
pub(crate) fn lk_from(pa: &[Gray], pb: &[Gray], p: [f32; 2], init: [f32; 2]) -> Option<[f32; 2]> {
    let levels = pa.len().min(pb.len());
    if levels == 0 || !(p[0].is_finite() && p[1].is_finite() && init[0].is_finite() && init[1].is_finite()) {
        return None;
    }
    let top = (1u32 << (levels - 1).min(16)) as f32;
    let mut d = [init[0] / top, init[1] / top];
    let (Some(a0), Some(b0)) = (pa.first(), pb.first()) else { return None };
    if (a0.w, a0.h) != (b0.w, b0.h) {
        return None;
    }
    let bound = 4.0 * a0.w.saturating_add(a0.h) as f32;
    for lvl in (0..levels).rev() {
        let s = (1u32 << lvl) as f32;
        let (a, b) = (&pa[lvl], &pb[lvl]);
        let (px, py) = (p[0] / s, p[1] / s);
        // spatial gradient matrix and template over the window (bilinear at sub-pixel p)
        let n = (2 * WIN + 1) as usize;
        let mut tpl = Vec::with_capacity(n * n);
        let mut grads = Vec::with_capacity(n * n);
        let (mut gxx, mut gxy, mut gyy) = (0.0f32, 0.0f32, 0.0f32);
        for wy in -WIN..=WIN {
            for wx in -WIN..=WIN {
                let (x, y) = (px + wx as f32, py + wy as f32);
                let ix = (a.sample(x + 1.0, y) - a.sample(x - 1.0, y)) * 0.5;
                let iy = (a.sample(x, y + 1.0) - a.sample(x, y - 1.0)) * 0.5;
                gxx += ix * ix;
                gxy += ix * iy;
                gyy += iy * iy;
                tpl.push(a.sample(x, y));
                grads.push((ix, iy));
            }
        }
        let det = gxx * gyy - gxy * gxy;
        if det < 1e-7 {
            return None;
        }
        let mut v = [0.0f32; 2];
        for _ in 0..30 {
            let (mut bx, mut by) = (0.0f32, 0.0f32);
            let mut k = 0;
            for wy in -WIN..=WIN {
                for wx in -WIN..=WIN {
                    let (x, y) = (px + wx as f32 + d[0] + v[0], py + wy as f32 + d[1] + v[1]);
                    let diff = tpl[k] - b.sample(x, y);
                    let (ix, iy) = grads[k];
                    bx += diff * ix;
                    by += diff * iy;
                    k += 1;
                }
            }
            let ex = (gyy * bx - gxy * by) / det;
            let ey = (gxx * by - gxy * bx) / det;
            v[0] += ex;
            v[1] += ey;
            if !(v[0].is_finite() && v[1].is_finite()) || v[0].abs() > bound || v[1].abs() > bound {
                return None;
            }
            if ex * ex + ey * ey < 1e-6 {
                break;
            }
        }
        d = [d[0] + v[0], d[1] + v[1]];
        if lvl > 0 {
            d = [d[0] * 2.0, d[1] * 2.0];
        }
    }
    let q = [p[0] + d[0], p[1] + d[1]];
    let (w, h) = (a0.w as f32, a0.h as f32);
    (q[0].is_finite() && q[1].is_finite() && q[0] >= 0.0 && q[1] >= 0.0 && q[0] < w && q[1] < h).then_some(q)
}

/// Least-squares fit of `method` mapping `src` → `dst`.
pub fn fit_ls(method: TrackMethod, src: &[Vec2], dst: &[Vec2]) -> Option<Affine> {
    let n = src.len();
    if n == 0 || n != dst.len() || (method != TrackMethod::Position && n < 2) {
        return None;
    }
    let inv = 1.0 / n as f64;
    let cs = src.iter().fold(Vec2::ZERO, |a, p| a + *p) * inv;
    let cd = dst.iter().fold(Vec2::ZERO, |a, p| a + *p) * inv;
    if method == TrackMethod::Position {
        let t = cd - cs;
        return Some(Affine::translate(t.x, t.y));
    }
    // 2D Procrustes: [a −b; b a] minimising Σ |R s + t − d|²
    let (mut sa, mut sb, mut ss) = (0.0, 0.0, 0.0);
    for (s, d) in src.iter().zip(dst) {
        let (x, y) = (s.x - cs.x, s.y - cs.y);
        let (u, v) = (d.x - cd.x, d.y - cd.y);
        sa += x * u + y * v;
        sb += x * v - y * u;
        ss += x * x + y * y;
    }
    if ss < 1e-12 {
        return None;
    }
    let (mut a, mut b) = (sa / ss, sb / ss);
    if method == TrackMethod::PositionRotation {
        let l = (a * a + b * b).sqrt();
        if l < 1e-12 {
            return None;
        }
        a /= l;
        b /= l;
    }
    let lin = Affine { a, b, c: -b, d: a, e: 0.0, f: 0.0 };
    let t = cd - lin.apply(cs);
    Some(Affine { e: t.x, f: t.y, ..lin })
}

/// Deterministic xorshift for RANSAC sampling.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// RANSAC + least-squares refit. Returns (transform, inlier count).
pub fn fit_robust(method: TrackMethod, src: &[Vec2], dst: &[Vec2]) -> Option<(Affine, usize)> {
    let n = src.len();
    let k = if method == TrackMethod::Position { 1 } else { 2 };
    if n < k || n != dst.len() {
        return None;
    }
    let inliers_of = |m: &Affine| -> Vec<usize> { (0..n).filter(|&i| (m.apply(src[i]) - dst[i]).length() < RANSAC_THRESHOLD).collect() };
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ n as u64);
    let mut best: Vec<usize> = Vec::new();
    for _ in 0..RANSAC_ITERS {
        let i = rng.below(n);
        let mut idx = vec![i];
        if k == 2 {
            let mut j = rng.below(n);
            let mut guard = 0;
            while (j == i || (src[j] - src[i]).length() < 4.0) && guard < 16 {
                j = rng.below(n);
                guard += 1;
            }
            if j == i {
                continue;
            }
            idx.push(j);
        }
        let (s, d): (Vec<Vec2>, Vec<Vec2>) = idx.iter().map(|&q| (src[q], dst[q])).unzip();
        let Some(m) = fit_ls(method, &s, &d) else { continue };
        let inl = inliers_of(&m);
        if inl.len() > best.len() {
            best = inl;
            if best.len() == n {
                break;
            }
        }
    }
    if best.len() < k {
        return None;
    }
    // refit on inliers, then once more on the refit's inliers
    for _ in 0..2 {
        let (s, d): (Vec<Vec2>, Vec<Vec2>) = best.iter().map(|&q| (src[q], dst[q])).unzip();
        let m = fit_ls(method, &s, &d)?;
        let inl = inliers_of(&m);
        if inl.len() < best.len() {
            break;
        }
        best = inl;
    }
    let (s, d): (Vec<Vec2>, Vec<Vec2>) = best.iter().map(|&q| (src[q], dst[q])).unzip();
    fit_ls(method, &s, &d).map(|m| (m, best.len()))
}

/// One tracking step's result.
#[derive(Clone, Debug)]
pub struct Step {
    /// Maps frame A pixels to frame B pixels.
    pub transform: Affine,
    pub features: usize,
    pub tracked: usize,
    pub inliers: usize,
}

/// A frame prepared for tracking (its pyramid).
#[derive(Clone)]
pub struct Prepared {
    pub(crate) levels: Vec<Gray>,
}

impl Prepared {
    /// The pyramid of `g`. A frame whose pixel count doesn't match its size (or that is empty)
    /// becomes an empty 0×0 frame, which every tracker refuses instead of indexing out of bounds.
    pub fn new(g: Gray) -> Self {
        if g.w == 0 || g.h == 0 || g.w.checked_mul(g.h) != Some(g.px.len()) {
            return Self { levels: vec![Gray { w: 0, h: 0, px: Vec::new() }] };
        }
        Self { levels: g.pyramid() }
    }
    pub fn base(&self) -> &Gray {
        static EMPTY: Gray = Gray { w: 0, h: 0, px: Vec::new() };
        self.levels.first().unwrap_or(&EMPTY)
    }
}

/// Track the picture inside `region` (polygon in frame pixels) from `a` to `b`.
pub fn track_step(a: &Prepared, b: &Prepared, region: &[Vec2], method: TrackMethod) -> Option<Step> {
    let feats = features_in(a, region, MAX_FEATURES, 5.0);
    if feats.len() < 3 {
        return None;
    }
    let pairs: Vec<(Vec2, Vec2)> = feats
        .par_iter()
        .filter_map(|&p| {
            let q = lk(&a.levels, &b.levels, p)?;
            let back = lk(&b.levels, &a.levels, q)?;
            let fb = ((back[0] - p[0]).powi(2) + (back[1] - p[1]).powi(2)).sqrt();
            (fb < FB_MAX).then(|| (Vec2::new(p[0] as f64, p[1] as f64), Vec2::new(q[0] as f64, q[1] as f64)))
        })
        .collect();
    let (src, dst): (Vec<Vec2>, Vec<Vec2>) = pairs.into_iter().unzip();
    let (transform, inliers) = fit_robust(method, &src, &dst)?;
    if inliers < 3 {
        return None;
    }
    Some(Step { transform, features: feats.len(), tracked: src.len(), inliers })
}

// ---- perspective (four-point) tracking, fork Y3.3: a plane seen in perspective (a screen, a
// sign) moves by a homography, not an affine map. Direct linear transform (Hartley & Zisserman,
// "Multiple View Geometry", §4.1 with the normalisation of §4.4) solved as least squares with
// h₃₃ = 1, inside the same RANSAC (4-point minimal samples).

/// A 3×3 projective map, row-major, `m[8]` = 1 after fitting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Homography {
    pub m: [f64; 9],
}

impl Homography {
    pub const IDENTITY: Homography = Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] };

    /// `p` mapped; None at (or numerically near) the line sent to infinity.
    pub fn apply(&self, p: Vec2) -> Option<Vec2> {
        let m = &self.m;
        let w = m[6] * p.x + m[7] * p.y + m[8];
        if !w.is_finite() || w.abs() < 1e-12 {
            return None;
        }
        let q = Vec2::new((m[0] * p.x + m[1] * p.y + m[2]) / w, (m[3] * p.x + m[4] * p.y + m[5]) / w);
        (q.x.is_finite() && q.y.is_finite()).then_some(q)
    }

    /// `self` after `first` (maps through `first`, then `self`).
    pub fn after(&self, first: &Homography) -> Homography {
        let (a, b) = (&self.m, &first.m);
        let mut m = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                m[r * 3 + c] = a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c] + a[r * 3 + 2] * b[6 + c];
            }
        }
        Homography { m }
    }

    fn is_finite(&self) -> bool {
        self.m.iter().all(|v| v.is_finite())
    }

    /// The inverse map (adjugate over the determinant, renormalised to `m[8]` = 1 when possible).
    /// None when singular or non-finite.
    pub fn inverse(&self) -> Option<Homography> {
        let m = &self.m;
        let adj = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let det = m[0] * adj[0] + m[1] * adj[3] + m[2] * adj[6];
        // singular relative to the rows' lengths (Hadamard's bound |det| <= |r0| |r1| |r2|): rows
        // mix pixel offsets with unit-free terms, so the largest entry cubed refused real
        // corner pins of large frames (a 6240 px floor plane: det 0.02 against 0.03, C3.8 review);
        // 1e-14 stays two orders above the rounding of a dependent set of rows (~1e-16)
        let row = |r: usize| m.get(r * 3..r * 3 + 3).map_or(f64::NAN, |v| v.iter().map(|x| x * x).sum::<f64>().sqrt());
        let scale = row(0) * row(1) * row(2);
        if !(det.is_finite() && scale.is_finite() && det.abs() > 1e-14 * scale.max(1e-300)) {
            return None;
        }
        let w = if adj[8].abs() > 1e-12 { adj[8] } else { det };
        let out = Homography { m: adj.map(|v| v / w) };
        out.is_finite().then_some(out)
    }
}

/// The similarity moving `pts` to centroid 0 and mean distance √2 (H&Z §4.4), as (scale, cx, cy).
fn normalising(pts: &[Vec2]) -> Option<(f64, f64, f64)> {
    let n = pts.len() as f64;
    let (cx, cy) = (pts.iter().map(|p| p.x).sum::<f64>() / n, pts.iter().map(|p| p.y).sum::<f64>() / n);
    let d = pts.iter().map(|p| ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt()).sum::<f64>() / n;
    let s = std::f64::consts::SQRT_2 / d;
    (d > 1e-9 && s.is_finite() && cx.is_finite() && cy.is_finite()).then_some((s, cx, cy))
}

/// Solve the n×n system `a` x = `b` (row-major) by Gaussian elimination with partial pivoting.
/// None when singular (a pivot below `1e-10` of the largest entry).
pub(crate) fn solve(a: &mut [f64], b: &mut [f64], n: usize) -> Option<Vec<f64>> {
    if a.len() != n * n || b.len() != n {
        return None;
    }
    let scale = a.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| a[i * n + col].abs().total_cmp(&a[j * n + col].abs()))?;
        if a[piv * n + col].abs() < scale * 1e-10 {
            return None;
        }
        if piv != col {
            for k in 0..n {
                a.swap(piv * n + k, col * n + k);
            }
            b.swap(piv, col);
        }
        for r in col + 1..n {
            let f = a[r * n + col] / a[col * n + col];
            for k in col..n {
                a[r * n + k] -= f * a[col * n + k];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let s: f64 = (r + 1..n).map(|k| a[r * n + k] * x[k]).sum();
        x[r] = (b[r] - s) / a[r * n + r];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// Least-squares homography from at least 4 correspondences (normalised DLT, h₃₃ = 1). None for
/// fewer points, mismatched lengths, non-finite input or a degenerate (e.g. collinear) set.
pub fn fit_homography(src: &[Vec2], dst: &[Vec2]) -> Option<Homography> {
    if src.len() < 4 || src.len() != dst.len() || src.iter().chain(dst).any(|p| !(p.x.is_finite() && p.y.is_finite())) {
        return None;
    }
    let (ss, sx, sy) = normalising(src)?;
    let (ds, dx, dy) = normalising(dst)?;
    let mut ata = [0.0f64; 64];
    let mut atb = [0.0f64; 8];
    for (p, q) in src.iter().zip(dst) {
        let (x, y) = ((p.x - sx) * ss, (p.y - sy) * ss);
        let (u, v) = ((q.x - dx) * ds, (q.y - dy) * ds);
        for (row, rhs) in [([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u), ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v)] {
            for i in 0..8 {
                for j in 0..8 {
                    ata[i * 8 + j] += row[i] * row[j];
                }
                atb[i] += row[i] * rhs;
            }
        }
    }
    let h = solve(&mut ata, &mut atb, 8)?;
    let hn = Homography { m: [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0] };
    // undo the normalisations: H = T_dst⁻¹ · Ĥ · T_src
    let t_src = Homography { m: [ss, 0.0, -ss * sx, 0.0, ss, -ss * sy, 0.0, 0.0, 1.0] };
    let t_dst_inv = Homography { m: [1.0 / ds, 0.0, dx, 0.0, 1.0 / ds, dy, 0.0, 0.0, 1.0] };
    let mut out = t_dst_inv.after(&hn.after(&t_src));
    let w = out.m[8];
    if !(w.is_finite() && w.abs() > 1e-12) {
        return None;
    }
    out.m.iter_mut().for_each(|v| *v /= w);
    out.is_finite().then_some(out)
}

/// Whether three of `p` lie (nearly) on one line: such a 4-point sample can't fix a homography.
fn has_collinear_triple(p: &[Vec2; 4]) -> bool {
    let area = |a: Vec2, b: Vec2, c: Vec2| ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs();
    [(0, 1, 2), (0, 1, 3), (0, 2, 3), (1, 2, 3)].iter().any(|&(i, j, k)| area(p[i], p[j], p[k]) < 1.0)
}

/// RANSAC over 4-point samples + least-squares refit on the inliers. Returns (map, inlier count).
pub fn fit_homography_robust(src: &[Vec2], dst: &[Vec2]) -> Option<(Homography, usize)> {
    let n = src.len();
    if n < 4 || n != dst.len() {
        return None;
    }
    let inliers_of = |h: &Homography| -> Vec<usize> { (0..n).filter(|&i| h.apply(src[i]).is_some_and(|q| (q - dst[i]).length() < RANSAC_THRESHOLD)).collect() };
    let mut rng = Rng(0x51ED_2701_AB54_93C1 ^ n as u64);
    let mut best: Vec<usize> = Vec::new();
    for _ in 0..RANSAC_ITERS {
        let mut idx = [0usize; 4];
        for k in 0..4 {
            idx[k] = rng.below(n);
        }
        if (0..4).any(|i| (i + 1..4).any(|j| idx[i] == idx[j])) {
            continue;
        }
        let sample = idx.map(|q| src[q]);
        if has_collinear_triple(&sample) {
            continue;
        }
        let Some(h) = fit_homography(&sample, &idx.map(|q| dst[q])) else { continue };
        let inl = inliers_of(&h);
        if inl.len() > best.len() {
            best = inl;
            if best.len() == n {
                break;
            }
        }
    }
    if best.len() < 4 {
        return None;
    }
    for _ in 0..2 {
        let (s, d): (Vec<Vec2>, Vec<Vec2>) = best.iter().map(|&q| (src[q], dst[q])).unzip();
        let h = fit_homography(&s, &d)?;
        let inl = inliers_of(&h);
        if inl.len() < best.len() {
            break;
        }
        best = inl;
    }
    let (s, d): (Vec<Vec2>, Vec<Vec2>) = best.iter().map(|&q| (src[q], dst[q])).unzip();
    fit_homography(&s, &d).map(|h| (h, best.len()))
}

/// One perspective tracking step's result.
#[derive(Clone, Debug)]
pub struct PerspectiveStep {
    /// Maps frame A pixels to frame B pixels.
    pub transform: Homography,
    pub features: usize,
    pub tracked: usize,
    pub inliers: usize,
}

/// Track the plane inside `region` (polygon in frame pixels) from `a` to `b` as a homography.
/// None when too little detail survives (fewer than 8 inliers: four points fit any quad exactly).
pub fn track_step_perspective(a: &Prepared, b: &Prepared, region: &[Vec2]) -> Option<PerspectiveStep> {
    let feats = features_in(a, region, MAX_FEATURES, 5.0);
    if feats.len() < 8 {
        return None;
    }
    let pairs: Vec<(Vec2, Vec2)> = feats
        .par_iter()
        .filter_map(|&p| {
            let q = lk(&a.levels, &b.levels, p)?;
            let back = lk(&b.levels, &a.levels, q)?;
            let fb = ((back[0] - p[0]).powi(2) + (back[1] - p[1]).powi(2)).sqrt();
            (fb < FB_MAX).then(|| (Vec2::new(p[0] as f64, p[1] as f64), Vec2::new(q[0] as f64, q[1] as f64)))
        })
        .collect();
    let (src, dst): (Vec<Vec2>, Vec<Vec2>) = pairs.into_iter().unzip();
    let (transform, inliers) = fit_homography_robust(&src, &dst)?;
    if inliers < 8 {
        return None;
    }
    Some(PerspectiveStep { transform, features: feats.len(), tracked: src.len(), inliers })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic texture: smooth blobs + fine detail.
    fn texture(u: f64, v: f64) -> f32 {
        let s = (u * 0.21).sin() * (v * 0.17).cos() + 0.5 * ((u + v) * 0.43).sin() + 0.35 * ((u * 0.9 - v * 0.6).sin() * (v * 0.75).cos());
        let h = {
            let (iu, iv) = ((u / 6.0).floor() as i64, (v / 6.0).floor() as i64);
            let mut x = (iu.wrapping_mul(73_856_093) ^ iv.wrapping_mul(19_349_663)) as u64;
            x ^= x >> 13;
            x = x.wrapping_mul(0x5bd1_e995);
            (x >> 40) as f64 / (1u64 << 24) as f64
        };
        (0.5 + 0.22 * s + 0.12 * h).clamp(0.0, 1.0) as f32
    }

    /// Frame with a textured disc (radius 70, object coords centred) under `pose`, over a static
    /// differently-textured background.
    fn frame(w: usize, h: usize, pose: &Affine) -> Gray {
        let inv = pose.inverse().unwrap();
        let px = (0..w * h)
            .into_par_iter()
            .map(|i| {
                // 2×2 supersampling
                let mut acc = 0.0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let p = Vec2::new((i % w) as f64 + ox - 0.5, (i / w) as f64 + oy - 0.5);
                    let o = inv.apply(p);
                    acc += if o.length() < 70.0 { texture(o.x + 200.0, o.y + 300.0) } else { 0.25 + 0.1 * texture(p.x * 0.5 + 900.0, p.y * 0.5) };
                }
                acc / 4.0
            })
            .collect();
        Gray::new(w, h, px).unwrap()
    }

    fn pose(t: f64, rot_per: f64, scale_per: f64) -> Affine {
        Affine::motion(Vec2::new(150.0 + 3.2 * t, 120.0 - 1.7 * t), Vec2::new(1.0 + scale_per * t, 1.0 + scale_per * t), rot_per * t, Vec2::ZERO)
    }

    fn circle(c: Vec2, r: f64) -> Vec<Vec2> {
        (0..48).map(|i| c + Vec2::new((i as f64 / 48.0 * std::f64::consts::TAU).cos() * r, (i as f64 / 48.0 * std::f64::consts::TAU).sin() * r)).collect()
    }

    fn run(method: TrackMethod, rot: f64, scale: f64, frames: usize) -> f64 {
        let (w, h) = (320, 240);
        let mut region = circle(Vec2::new(150.0, 120.0), 50.0);
        let mut prev = Prepared::new(frame(w, h, &pose(0.0, rot, scale)));
        let mut worst = 0.0f64;
        let probe = [Vec2::new(30.0, 0.0), Vec2::new(-20.0, 25.0), Vec2::new(0.0, -40.0)];
        let mut tracked: Vec<Vec2> = probe.iter().map(|o| pose(0.0, rot, scale).apply(*o)).collect();
        for k in 1..=frames {
            let next = Prepared::new(frame(w, h, &pose(k as f64, rot, scale)));
            let st = track_step(&prev, &next, &region, method).expect("tracked");
            region = region.iter().map(|p| st.transform.apply(*p)).collect();
            tracked = tracked.iter().map(|p| st.transform.apply(*p)).collect();
            for (o, t) in probe.iter().zip(&tracked) {
                let truth = pose(k as f64, rot, scale).apply(*o);
                worst = worst.max((truth - *t).length());
            }
            prev = next;
        }
        worst
    }

    #[test]
    fn tracks_translation() {
        let e = run(TrackMethod::Position, 0.0, 0.0, 10);
        assert!(e < 0.5, "max drift {e} px");
    }

    #[test]
    fn tracks_rotation() {
        let e = run(TrackMethod::PositionRotation, 2.0, 0.0, 10);
        assert!(e < 0.75, "max drift {e} px");
    }

    #[test]
    fn tracks_similarity() {
        let e = run(TrackMethod::PositionScaleRotation, -1.5, 0.012, 10);
        assert!(e < 0.75, "max drift {e} px");
    }

    #[test]
    fn ransac_rejects_outliers() {
        let truth = Affine::motion(Vec2::new(5.0, -3.0), Vec2::new(1.1, 1.1), 7.0, Vec2::ZERO);
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for i in 0..40 {
            let p = Vec2::new((i * 37 % 100) as f64, (i * 61 % 90) as f64);
            src.push(p);
            dst.push(if i % 4 == 0 { p + Vec2::new(20.0, -13.0) } else { truth.apply(p) });
        }
        let (m, inl) = fit_robust(TrackMethod::PositionScaleRotation, &src, &dst).unwrap();
        assert_eq!(inl, 30);
        for p in &src {
            assert!((m.apply(*p) - truth.apply(*p)).length() < 1e-6);
        }
    }

    /// A gentle perspective motion of the plane at time `t` (object coords → frame pixels).
    fn persp(t: f64) -> Homography {
        let a = Affine::motion(Vec2::new(160.0 + 2.5 * t, 120.0 - 1.2 * t), Vec2::new(1.0 + 0.01 * t, 1.0), 1.5 * t, Vec2::ZERO);
        Homography { m: [a.a, a.c, a.e, a.b, a.d, a.f, 0.00012 * t, -0.00008 * t, 1.0] }
    }

    fn inverse(h: &Homography) -> Homography {
        let m = &h.m;
        let inv = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        Homography { m: inv }
    }

    /// A textured square plane (side 160, centred) under `h` over a static background.
    fn persp_frame(w: usize, hgt: usize, h: &Homography) -> Gray {
        let inv = inverse(h);
        let px = (0..w * hgt)
            .into_par_iter()
            .map(|i| {
                let mut acc = 0.0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let p = Vec2::new((i % w) as f64 + ox - 0.5, (i / w) as f64 + oy - 0.5);
                    acc += match inv.apply(p) {
                        Some(o) if o.x.abs() < 80.0 && o.y.abs() < 80.0 => texture(o.x + 200.0, o.y + 300.0),
                        _ => 0.25 + 0.1 * texture(p.x * 0.5 + 900.0, p.y * 0.5),
                    };
                }
                acc / 4.0
            })
            .collect();
        Gray::new(w, hgt, px).unwrap()
    }

    #[test]
    fn homography_fit_recovers_a_known_map_and_rejects_outliers() {
        let truth = persp(8.0);
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for i in 0..40 {
            let p = Vec2::new((i * 37 % 100) as f64 - 50.0, (i * 61 % 90) as f64 - 45.0);
            src.push(p);
            let q = truth.apply(p).unwrap();
            dst.push(if i % 5 == 0 { q + Vec2::new(17.0, -11.0) } else { q });
        }
        let exact = fit_homography(&src[1..5], &dst[1..5]).unwrap();
        for p in &src[1..5] {
            assert!((exact.apply(*p).unwrap() - truth.apply(*p).unwrap()).length() < 1e-6);
        }
        let (h, inl) = fit_homography_robust(&src, &dst).unwrap();
        assert_eq!(inl, 32);
        for p in &src {
            assert!((h.apply(*p).unwrap() - truth.apply(*p).unwrap()).length() < 1e-6);
        }
    }

    #[test]
    fn homography_fit_refuses_degenerate_and_hostile_input() {
        let line: Vec<Vec2> = (0..10).map(|i| Vec2::new(i as f64, 2.0 * i as f64 + 1.0)).collect();
        assert!(fit_homography(&line, &line).is_none(), "collinear points fix no homography");
        assert!(fit_homography_robust(&line, &line).is_none());
        let three = [Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)];
        assert!(fit_homography(&three, &three).is_none());
        assert!(fit_homography_robust(&three, &three).is_none());
        assert!(fit_homography_robust(&[], &[]).is_none());
        let mut sq = vec![Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0), Vec2::new(0.0, 10.0)];
        assert!(fit_homography(&sq, &sq[..3]).is_none(), "mismatched lengths");
        sq[2] = Vec2::new(f64::NAN, 3.0);
        assert!(fit_homography(&sq, &sq).is_none());
        let same = vec![Vec2::new(5.0, 5.0); 6];
        assert!(fit_homography(&same, &same).is_none());
        assert!(Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0] }.apply(Vec2::new(0.0, 4.0)).is_none());
    }

    #[test]
    fn tracks_a_plane_in_perspective() {
        let (w, h) = (320, 240);
        let square = [Vec2::new(-60.0, -60.0), Vec2::new(60.0, -60.0), Vec2::new(60.0, 60.0), Vec2::new(-60.0, 60.0)];
        let mut region: Vec<Vec2> = square.iter().filter_map(|c| persp(0.0).apply(*c)).collect();
        let mut prev = Prepared::new(persp_frame(w, h, &persp(0.0)));
        let corners = [Vec2::new(-70.0, -70.0), Vec2::new(70.0, -70.0), Vec2::new(70.0, 70.0), Vec2::new(-70.0, 70.0)];
        let mut tracked: Vec<Vec2> = corners.iter().filter_map(|c| persp(0.0).apply(*c)).collect();
        let mut worst = 0.0f64;
        for k in 1..=10 {
            let next = Prepared::new(persp_frame(w, h, &persp(k as f64)));
            let st = track_step_perspective(&prev, &next, &region).expect("tracked");
            region = region.iter().filter_map(|p| st.transform.apply(*p)).collect();
            tracked = tracked.iter().filter_map(|p| st.transform.apply(*p)).collect();
            assert_eq!(tracked.len(), 4);
            for (c, t) in corners.iter().zip(&tracked) {
                worst = worst.max((persp(k as f64).apply(*c).unwrap() - *t).length());
            }
            prev = next;
        }
        assert!(worst < 0.75, "max corner drift {worst} px");
    }

    #[test]
    fn gray_new_rejects_a_mismatched_buffer_and_samplers_are_total() {
        // Gray::new used to assert_eq! (panic) on a buffer that doesn't match the size
        assert!(Gray::new(4, 4, vec![0.0; 15]).is_none());
        assert!(Gray::new(usize::MAX, 2, vec![0.0; 2]).is_none());
        assert_eq!(Gray::new(2, 2, vec![0.25; 4]).map(|g| (g.w, g.h)), Some((2, 2)));
        // inconsistent public fields read 0 instead of indexing out of bounds
        let empty = Gray { w: 0, h: 0, px: Vec::new() };
        assert_eq!(empty.sample(1.0, 1.0), 0.0);
        assert_eq!(empty.down().w, 0);
        let short = Gray { w: 10, h: 10, px: vec![0.5; 3] };
        assert!(short.sample(5.0, 5.0).is_finite() && short.grad(9, 9).0.is_finite());
        assert_eq!(short.down().w, 0);
        let sq = [Vec2::new(0.0, 0.0), Vec2::new(9.0, 0.0), Vec2::new(9.0, 9.0), Vec2::new(0.0, 9.0)];
        assert!(features(&short, &sq, 10, 1.0).is_empty());
        assert_eq!(Prepared { levels: Vec::new() }.base().w, 0);
        // mismatched correspondences are refused, not indexed
        assert!(fit_ls(TrackMethod::PositionScaleRotation, &sq, &sq[..3]).is_none());
        assert!(fit_robust(TrackMethod::PositionScaleRotation, &sq, &sq[..3]).is_none());
        assert!(lk_from(&[Gray::new(64, 64, vec![0.5; 4096]).unwrap()], &[Gray::new(32, 64, vec![0.5; 2048]).unwrap()], [8.0, 8.0], [0.0, 0.0]).is_none());
    }

    #[test]
    fn features_of_a_large_region_come_from_a_coarser_level() {
        // 1536×1024 > the 1 Mpx budget: found on level 1, reported in full-resolution pixels
        let (w, h) = (1536, 1024);
        let g = frame(w, h, &Affine::translate(768.0, 512.0));
        let p = Prepared::new(g);
        let all = [Vec2::new(0.0, 0.0), Vec2::new(w as f64, 0.0), Vec2::new(w as f64, h as f64), Vec2::new(0.0, h as f64)];
        let f = features_in(&p, &all, MAX_FEATURES, 5.0);
        assert!(f.len() >= 20, "{}", f.len());
        assert!(f.iter().all(|q| q[0] % 2.0 == 0.0 && q[1] % 2.0 == 0.0 && q[0] < w as f32 && q[1] < h as f32));
        assert!(f.iter().any(|q| q[0] > 0.5 * w as f32), "full-resolution coordinates");
        // a small region stays on the full-resolution level (unchanged behaviour)
        let small = circle(Vec2::new(768.0, 512.0), 60.0);
        assert_eq!(features_in(&p, &small, MAX_FEATURES, 5.0), features(p.base(), &small, MAX_FEATURES, 5.0));
    }

    #[test]
    fn perspective_tracking_of_a_flat_region_is_refused() {
        let flat = Prepared::new(Gray::new(64, 48, vec![0.5; 64 * 48]).unwrap());
        let region = [Vec2::new(5.0, 5.0), Vec2::new(60.0, 5.0), Vec2::new(60.0, 40.0), Vec2::new(5.0, 40.0)];
        assert!(track_step_perspective(&flat, &flat, &region).is_none());
        assert!(track_step_perspective(&flat, &flat, &[]).is_none());
    }
}
