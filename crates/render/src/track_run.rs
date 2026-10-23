//! Multi-frame tracking (fork C3.1): point, multi-point and planar trackers that run across a
//! whole shot, and the solves built on them (stabilise, match move, corner pin).
//!
//! [`crate::track`] is frame-pair only; this module keeps state from frame to frame. Time is a
//! frame index (the engine maps frames to `Tick`s). The methods (all published, implemented from
//! the papers' equations):
//!
//! 1. **Point tracker** — template matching of a user pattern box inside a search box:
//!    zero-mean normalised cross-correlation (NCC; Lewis, "Fast Normalized Cross-Correlation",
//!    1995) over integer offsets on the coarsest pyramid level that keeps the search within a
//!    fixed cost budget, refined level by level with ±2 px searches, then to sub-pixel by
//!    inverse compositional affine Lucas–Kanade (Lucas & Kanade 1981; Baker & Matthews
//!    "Lucas–Kanade 20 Years On", 2004) with zero-mean windows, so a pattern that rotates, scales
//!    or tilts keeps an unbiased centre (its warp carries to the next frame's search). The template is the reference frame's
//!    pattern (drift-free) until its NCC falls below [`REFRESH_BELOW`]; then it is re-cut from
//!    the current frame (an adaptive template, Matthews, Ishikawa & Baker 2004). The NCC of the
//!    final position is the confidence; below the caller's threshold the point is lost and then
//!    either stops or holds its last position and keeps searching ([`OnLost`]). A constant-
//!    velocity prediction centres the search.
//! 2. **Multi-point solves** — per frame, the similarity (or rigid / translation) mapping the
//!    reference frame's point positions to the frame's, least squares (2D Procrustes) or RANSAC
//!    with four or more points ([`crate::track::fit_robust`]). Match move = that forward map;
//!    stabilise = its inverse, optionally composed with a smoothed camera path (a centred
//!    Gaussian over translation, unwrapped angle and log-scale about the points' centroid) so a
//!    "smooth" stabilise keeps intended pans; corner pin = the 4-point homography
//!    ([`crate::track::fit_homography`], normalised DLT, Hartley & Zisserman §4.1/§4.4).
//! 3. **Planar tracker** — a polygon region tracked as a homography from the reference frame.
//!    Each frame is first predicted by frame-to-frame [`crate::track::track_step_perspective`],
//!    then **re-anchored on the reference**: the reference is warped into the frame through the
//!    prediction, features of the warped picture are tracked into the frame (pyramidal LK with a
//!    forward–backward check, Kalal et al. 2010) and the homography is refitted from reference
//!    coordinates directly (RANSAC + DLT). Errors don't accumulate along the chain, so an insert
//!    corner-pinned to the plane stays locked. Lost when too few inliers survive; the last good
//!    homography keeps predicting so the plane is re-acquired when it reappears.

use crate::track::{
    self, FB_MAX, Gray, Homography, MAX_FEATURES, Prepared, fit_homography, fit_homography_robust, fit_ls, fit_robust, lk_from, track_step_perspective,
};
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::TrackMethod;
use rayon::prelude::*;
use std::sync::Arc;

/// Most points one multi-point tracker (or solve) uses.
pub const MAX_POINTS: usize = 64;
/// Most frames one batch run tracks.
pub const MAX_RUN_FRAMES: usize = 1 << 20;
/// Largest pattern half-size (px); bigger boxes are clamped to it.
pub const MAX_PATTERN_HALF: f64 = 64.0;
/// Smallest pattern half-size (px); smaller boxes are grown to it.
pub const MIN_PATTERN_HALF: f64 = 3.0;
/// Largest search half-size (px per frame); bigger is clamped.
pub const MAX_SEARCH_HALF: f64 = 256.0;
/// Default confidence (NCC) below which a point is lost.
pub const DEFAULT_LOST_BELOW: f64 = 0.6;
/// Template NCC below which the pattern is re-cut from the current frame.
pub const REFRESH_BELOW: f64 = 0.85;
/// Most vertices of a planar region.
pub const MAX_REGION_POINTS: usize = 1024;
/// Largest stabilise smoothing radius (frames).
pub const MAX_SMOOTH_RADIUS: u32 = 1000;
/// Fewest inliers a planar step needs (four points fit any quad exactly).
pub const PLANAR_MIN_INLIERS: usize = 8;
/// Inlier ratio (of the warped reference's features) below which the plane is lost.
pub const PLANAR_LOST_BELOW: f64 = 0.25;

const MIN_FRAME: usize = 16;
const MAX_FRAME_DIM: usize = 16384;
/// NCC multiply-adds allowed for the coarse search of one point in one frame.
const NCC_BUDGET: usize = 3_000_000;
/// The planar warp covers the region's bounding box plus this margin (px, ≥ the LK window).
const WARP_MARGIN: f64 = 24.0;
/// The re-anchoring crop covers the region's bounding box plus this margin (px): room for the
/// coarsest LK window (±7 px at 1/8 scale) beyond the warped area.
const CROP_MARGIN: f64 = 64.0;

/// Whether `p` is a frame the trackers accept: at least 16×16, at most 16384 a side, consistent.
fn usable(p: &Prepared) -> bool {
    let g = p.base();
    g.w >= MIN_FRAME && g.h >= MIN_FRAME && g.w <= MAX_FRAME_DIM && g.h <= MAX_FRAME_DIM && g.w.checked_mul(g.h) == Some(g.px.len())
}

fn finite(p: Vec2) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

fn inside(g: &Gray, p: Vec2) -> bool {
    finite(p) && p.x >= 0.0 && p.y >= 0.0 && p.x <= g.w.saturating_sub(1) as f64 && p.y <= g.h.saturating_sub(1) as f64
}

// ---- point tracker -------------------------------------------------------------------------

/// One tracked position in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackSample {
    /// Pattern centre in frame pixels (pixel centres at integers). Held at the last good
    /// position while lost.
    pub pos: Vec2,
    /// Match quality 0..1 (zero-mean NCC of the pattern at `pos`).
    pub confidence: f64,
    /// Below the lost threshold (or never found): `pos` is not a measurement.
    pub lost: bool,
}

impl TrackSample {
    fn lost_at(pos: Vec2) -> Self {
        Self { pos, confidence: 0.0, lost: true }
    }
    /// A usable measurement (not lost, finite).
    pub fn valid(&self) -> bool {
        !self.lost && finite(self.pos)
    }
}

/// What a point does after it is lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnLost {
    /// Stop: every later frame (in this direction) is reported lost without searching.
    #[default]
    Stop,
    /// Hold the last good position and keep searching around it; resumes when found again.
    Hold,
}

/// A point to track: the pattern box (centre + half-size) and the search range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointSpec {
    /// Pattern centre in the first frame (frame pixels).
    pub at: Vec2,
    /// Pattern box half-size (px), clamped to [`MIN_PATTERN_HALF`]..=[`MAX_PATTERN_HALF`].
    pub pattern: Vec2,
    /// How far (px, each axis) the centre may move per frame beyond the velocity prediction,
    /// clamped to 1..=[`MAX_SEARCH_HALF`] (After Effects' search box minus the pattern box).
    pub search: Vec2,
}

/// Lost handling for point trackers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointOptions {
    /// Confidence below which the point is lost (0..1).
    pub lost_below: f64,
    pub on_lost: OnLost,
}

impl Default for PointOptions {
    fn default() -> Self {
        Self { lost_below: DEFAULT_LOST_BELOW, on_lost: OnLost::Stop }
    }
}

/// A zero-mean pattern sampled on an integer grid around a (sub-pixel) centre.
#[derive(Clone, Debug)]
struct Patch {
    hx: i32,
    hy: i32,
    vals: Vec<f32>,
    norm: f32,
}

impl Patch {
    /// The pattern of `g` around `c` (level pixels); None when (nearly) flat.
    fn cut(g: &Gray, c: Vec2, hx: i32, hy: i32) -> Option<Patch> {
        let mut vals = Vec::with_capacity(((2 * hx + 1) * (2 * hy + 1)) as usize);
        for y in -hy..=hy {
            for x in -hx..=hx {
                vals.push(g.sample(c.x as f32 + x as f32, c.y as f32 + y as f32));
            }
        }
        let n = vals.len().max(1) as f32;
        let mean = vals.iter().sum::<f32>() / n;
        vals.iter_mut().for_each(|v| *v -= mean);
        let norm = vals.iter().map(|v| v * v).sum::<f32>().sqrt();
        // flat: below ~0.2 % rms contrast
        (norm.is_finite() && norm > 2e-3 * n.sqrt()).then_some(Patch { hx, hy, vals, norm })
    }

    /// Zero-mean NCC of this pattern against `g` centred at `c` (level pixels), its grid warped by
    /// the linear map `lin` (translation ignored), -1..1; 0 on flat.
    fn ncc(&self, g: &Gray, c: Vec2, lin: &Affine) -> f32 {
        let (cx, cy) = (c.x as f32, c.y as f32);
        let [la, lb, lc, ld] = [lin.a, lin.b, lin.c, lin.d].map(|v| v as f32);
        let (mut s, mut si, mut sii) = (0.0f32, 0.0f32, 0.0f32);
        let mut k = 0;
        for y in -self.hy..=self.hy {
            for x in -self.hx..=self.hx {
                let (x, y) = (x as f32, y as f32);
                let i = g.sample(cx + la * x + lc * y, cy + lb * x + ld * y);
                s += self.vals.get(k).copied().unwrap_or(0.0) * i;
                si += i;
                sii += i * i;
                k += 1;
            }
        }
        let n = k.max(1) as f32;
        // Σ t'(i − ī) = Σ t' i since Σ t' = 0
        let var = sii - si * si / n;
        if !(var.is_finite() && var > 1e-10 && self.norm > 0.0) {
            return 0.0;
        }
        (s / (self.norm * var.sqrt())).clamp(-1.0, 1.0)
    }
}

/// A pattern at every pyramid level plus, at full resolution, the steepest-descent images of the
/// six affine warp parameters and their Gauss–Newton Hessian (inverse compositional LK, Baker &
/// Matthews 2004 §3) for sub-pixel refinement that follows the pattern's rotation / scale / shear.
#[derive(Clone, Debug)]
struct Template {
    levels: Vec<Patch>,
    sd: Vec<[f64; 6]>,
    hess: [f64; 36],
}

impl Template {
    fn cut(p: &Prepared, at: Vec2, half: Vec2) -> Option<Template> {
        let mut levels = Vec::new();
        for (l, g) in p.levels.iter().enumerate() {
            let s = (1u32 << l.min(16)) as f64;
            let hx = ((half.x / s).round() as i32).max(2);
            let hy = ((half.y / s).round() as i32).max(2);
            // a level whose pattern is bigger than the level itself is no use
            if l > 0 && ((2 * hx + 1) as usize > g.w || (2 * hy + 1) as usize > g.h) {
                break;
            }
            match Patch::cut(g, at * (1.0 / s), hx, hy) {
                Some(patch) => levels.push(patch),
                None if l == 0 => return None,
                None => break,
            }
        }
        let base = levels.first()?;
        let g = p.base();
        let (mut sd, mut hess) = (Vec::with_capacity(base.vals.len()), [0.0f64; 36]);
        let (cx, cy) = (at.x as f32, at.y as f32);
        for y in -base.hy..=base.hy {
            for x in -base.hx..=base.hx {
                let (px, py) = (cx + x as f32, cy + y as f32);
                let ix = ((g.sample(px + 1.0, py) - g.sample(px - 1.0, py)) * 0.5) as f64;
                let iy = ((g.sample(px, py + 1.0) - g.sample(px, py - 1.0)) * 0.5) as f64;
                // ∂W/∂p for W(x; p) = (a x + c y + e, b x + d y + f), p = (a−1, b, c, d−1, e, f)
                let (x, y) = (x as f64, y as f64);
                let row = [ix * x, iy * x, ix * y, iy * y, ix, iy];
                for (i, ri) in row.iter().enumerate() {
                    for (j, rj) in row.iter().enumerate() {
                        if let Some(h) = hess.get_mut(i * 6 + j) {
                            *h += ri * rj;
                        }
                    }
                }
                sd.push(row);
            }
        }
        Some(Template { levels, sd, hess })
    }

    /// Best integer offset (within ±`r`) of the level-`l` pattern around `c` (level pixels).
    fn search(&self, g: &Gray, l: usize, c: Vec2, rx: i32, ry: i32, lin: &Affine) -> Option<(Vec2, f32)> {
        let patch = self.levels.get(l)?;
        let w = (2 * rx + 1) as usize;
        let n = w * (2 * ry + 1) as usize;
        (0..n)
            .into_par_iter()
            .map(|i| {
                let q = c + Vec2::new((i % w) as f64 - rx as f64, (i / w) as f64 - ry as f64);
                (q, patch.ncc(g, q, lin))
            })
            // ties: the lowest index wins (deterministic)
            .reduce_with(|a, b| if b.1 > a.1 { b } else { a })
    }

    /// Inverse compositional affine Lucas–Kanade of the full-resolution pattern into `g`, from
    /// centre `c` and grid warp `lin`, with zero-mean windows: the refined (centre, linear warp).
    /// None when the Hessian is singular or the warp runs off (moves > 4 px, or the linear part
    /// leaves a 4× scale range or flips).
    fn refine(&self, g: &Gray, c: Vec2, lin: &Affine) -> Option<(Vec2, Affine)> {
        let base = self.levels.first()?;
        let n = base.vals.len();
        let mut w = Affine { e: 0.0, f: 0.0, ..*lin };
        let mut win = vec![0.0f32; n];
        for _ in 0..50 {
            let mut k = 0;
            for y in -base.hy..=base.hy {
                for x in -base.hx..=base.hx {
                    let q = c + w.apply(Vec2::new(x as f64, y as f64));
                    if let Some(o) = win.get_mut(k) {
                        *o = g.sample(q.x as f32, q.y as f32);
                    }
                    k += 1;
                }
            }
            let mean = win.iter().map(|v| *v as f64).sum::<f64>() / n.max(1) as f64;
            let mut b = [0.0f64; 6];
            for ((t, i), sd) in base.vals.iter().zip(&win).zip(&self.sd) {
                let e = (*i as f64 - mean) - *t as f64;
                b.iter_mut().zip(sd).for_each(|(bb, s)| *bb += s * e);
            }
            let mut h = self.hess;
            let dp = track::solve(&mut h, &mut b, 6)?;
            let [da, db, dc, dd, de, df] = <[f64; 6]>::try_from(dp.as_slice()).ok()?;
            let step = Affine { a: 1.0 + da, b: db, c: dc, d: 1.0 + dd, e: de, f: df };
            w = w.then_apply(&step.inverse()?);
            let det = w.determinant();
            if ![w.a, w.b, w.c, w.d, w.e, w.f].iter().all(|v| v.is_finite()) || w.e.abs() > 4.0 || w.f.abs() > 4.0 || !(0.25..=4.0).contains(&det) {
                return None;
            }
            // converged: the centre moves < 0.001 px and the pattern's corners < 0.01 px
            let span = base.hx.max(base.hy) as f64;
            if de * de + df * df < 1e-6 && (da.abs() + db.abs() + dc.abs() + dd.abs()) * span < 1e-2 {
                break;
            }
        }
        Some((c + Vec2::new(w.e, w.f), Affine { e: 0.0, f: 0.0, ..w }))
    }
}

/// Tracks one point frame by frame (run it forward or backward: feed frames in either order).
#[derive(Clone, Debug)]
pub struct PointTracker {
    template: Template,
    half: Vec2,
    search: Vec2,
    size: (usize, usize),
    pos: Vec2,
    vel: Vec2,
    /// The template grid's linear warp into the last frame (rotation / scale / shear).
    lin: Affine,
    opts: PointOptions,
    stopped: bool,
}

impl PointTracker {
    /// Start tracking `spec` in `first`. None when the frame is unusable, the centre is outside
    /// it, a number is not finite or the pattern is flat (nothing to lock on to).
    pub fn new(first: &Prepared, spec: PointSpec, opts: PointOptions) -> Option<Self> {
        if !usable(first) || !finite(spec.at) || !finite(spec.pattern) || !finite(spec.search) || !inside(first.base(), spec.at) {
            return None;
        }
        let half = Vec2::new(spec.pattern.x.clamp(MIN_PATTERN_HALF, MAX_PATTERN_HALF), spec.pattern.y.clamp(MIN_PATTERN_HALF, MAX_PATTERN_HALF));
        let search = Vec2::new(spec.search.x.clamp(1.0, MAX_SEARCH_HALF), spec.search.y.clamp(1.0, MAX_SEARCH_HALF));
        let lost_below = if opts.lost_below.is_finite() { opts.lost_below.clamp(0.0, 1.0) } else { DEFAULT_LOST_BELOW };
        let template = Template::cut(first, spec.at, half)?;
        let g = first.base();
        Some(Self {
            template,
            half,
            search,
            size: (g.w, g.h),
            pos: spec.at,
            vel: Vec2::ZERO,
            lin: Affine::IDENTITY,
            opts: PointOptions { lost_below, ..opts },
            stopped: false,
        })
    }

    /// The sample for the first frame (the pattern itself).
    pub fn first_sample(&self) -> TrackSample {
        TrackSample { pos: self.pos, confidence: 1.0, lost: false }
    }

    /// Track into the next frame (forward or backward in time).
    pub fn step(&mut self, next: &Prepared) -> TrackSample {
        if self.stopped {
            return TrackSample::lost_at(self.pos);
        }
        let found = if usable(next) && (next.base().w, next.base().h) == self.size { self.locate(next) } else { None };
        match found {
            Some((p, lin, conf)) if conf >= self.opts.lost_below => {
                self.vel = p - self.pos;
                self.pos = p;
                self.lin = lin;
                if conf < REFRESH_BELOW
                    && let Some(t) = Template::cut(next, p, self.half)
                {
                    self.template = t;
                    self.lin = Affine::IDENTITY;
                }
                TrackSample { pos: p, confidence: conf, lost: false }
            }
            other => {
                self.vel = Vec2::ZERO;
                if self.opts.on_lost == OnLost::Stop {
                    self.stopped = true;
                }
                TrackSample { pos: self.pos, confidence: other.map_or(0.0, |(_, _, c)| c.max(0.0)), lost: true }
            }
        }
    }

    /// Coarse-to-fine NCC search + sub-pixel affine LK; (position, grid warp, confidence).
    fn locate(&self, next: &Prepared) -> Option<(Vec2, Affine, f64)> {
        let nlev = self.template.levels.len().min(next.levels.len());
        if nlev == 0 {
            return None;
        }
        let pred = self.pos + self.vel;
        let g0 = next.base();
        let pred = Vec2::new(pred.x.clamp(0.0, g0.w.saturating_sub(1) as f64), pred.y.clamp(0.0, g0.h.saturating_sub(1) as f64));
        // the coarsest level needed to keep the exhaustive search within budget
        let mut lvl = 0;
        for l in 0..nlev {
            lvl = l;
            let s = (1u32 << l.min(16)) as f64;
            let Some(g) = next.levels.get(l) else { break };
            let (rx, ry) = (((self.search.x / s).ceil() as usize).min(g.w), ((self.search.y / s).ceil() as usize).min(g.h));
            let Some(p) = self.template.levels.get(l) else { break };
            let cost = (2 * rx + 1).saturating_mul(2 * ry + 1).saturating_mul(p.vals.len());
            if cost <= NCC_BUDGET {
                break;
            }
        }
        let s = (1u32 << lvl.min(16)) as f64;
        let g = next.levels.get(lvl)?;
        let (rx, ry) = ((self.search.x / s).ceil().min(g.w as f64) as i32 + 1, (self.search.y / s).ceil().min(g.h as f64) as i32 + 1);
        let lin = &self.lin;
        let (mut c, _) = self.template.search(g, lvl, pred * (1.0 / s), rx, ry, lin)?;
        // offset of the integer grid from the prediction, carried down the pyramid
        for l in (0..lvl).rev() {
            let g = next.levels.get(l)?;
            (c, _) = self.template.search(g, l, c * 2.0, 2, 2, lin)?;
        }
        let base = self.template.levels.first()?;
        let coarse = base.ncc(g0, c, lin) as f64;
        let fine = self.template.refine(g0, c, lin).filter(|(p, _)| inside(g0, *p)).map(|(p, l)| (p, l, base.ncc(g0, p, &l) as f64));
        let best = match fine {
            Some((p, l, n)) if n >= coarse - 0.02 => (p, l, n),
            _ => (c, *lin, coarse),
        };
        inside(g0, best.0).then_some(best)
    }
}

/// Track `spec` (placed in `frames[start]`) forward to the end and backward to frame 0.
/// One sample per frame. None when the start is out of range or the point can't start.
pub fn track_point(frames: &[Prepared], start: usize, spec: PointSpec, opts: PointOptions) -> Option<Vec<TrackSample>> {
    if frames.len() > MAX_RUN_FRAMES {
        return None;
    }
    let first = frames.get(start)?;
    let tracker = PointTracker::new(first, spec, opts)?;
    let mut out = vec![tracker.first_sample(); frames.len()];
    let mut fwd = tracker.clone();
    for (o, f) in out.iter_mut().zip(frames).skip(start + 1) {
        *o = fwd.step(f);
    }
    let mut back = tracker;
    for (o, f) in out.iter_mut().zip(frames).take(start).rev() {
        *o = back.step(f);
    }
    Some(out)
}

/// [`track_point`] for several points (at most [`MAX_POINTS`]), in parallel.
pub fn track_points(frames: &[Prepared], start: usize, specs: &[PointSpec], opts: PointOptions) -> Vec<Option<Vec<TrackSample>>> {
    specs.iter().take(MAX_POINTS).collect::<Vec<_>>().par_iter().map(|s| track_point(frames, start, **s, opts)).collect()
}

/// Several point trackers stepped together (the incremental form of [`track_points`]).
#[derive(Clone, Debug)]
pub struct MultiPointTracker {
    points: Vec<Result<PointTracker, Vec2>>,
}

impl MultiPointTracker {
    /// Start every spec (at most [`MAX_POINTS`]) in `first`; a point that can't start reports
    /// lost on every frame (at its spec position, or the origin when that isn't finite).
    pub fn new(first: &Prepared, specs: &[PointSpec], opts: PointOptions) -> Self {
        let points = specs.iter().take(MAX_POINTS).map(|s| PointTracker::new(first, *s, opts).ok_or(if finite(s.at) { s.at } else { Vec2::ZERO })).collect();
        Self { points }
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The samples of the first frame.
    pub fn first_samples(&self) -> Vec<TrackSample> {
        self.points.iter().map(|p| p.as_ref().map_or_else(|at| TrackSample::lost_at(*at), PointTracker::first_sample)).collect()
    }

    /// Track every point into `next`.
    pub fn step(&mut self, next: &Prepared) -> Vec<TrackSample> {
        self.points
            .par_iter_mut()
            .map(|p| match p {
                Ok(t) => t.step(next),
                Err(at) => TrackSample::lost_at(*at),
            })
            .collect()
    }
}

// ---- multi-point solves --------------------------------------------------------------------

fn frame_count(tracks: &[Vec<TrackSample>]) -> usize {
    tracks.iter().take(MAX_POINTS).map(Vec::len).max().unwrap_or(0).min(MAX_RUN_FRAMES)
}

/// Per frame, the map from the reference frame's pixels to the frame's, fitted to every point
/// valid in both (`tracks[point][frame]`). One point gives a translation whatever `method` asks;
/// four or more are fitted robustly (RANSAC, 1 px). None where no point is valid (and every
/// frame when `reference` is out of range).
pub fn motion_path(tracks: &[Vec<TrackSample>], reference: usize, method: TrackMethod) -> Vec<Option<Affine>> {
    let n = frame_count(tracks);
    let tracks = tracks.get(..tracks.len().min(MAX_POINTS)).unwrap_or(&[]);
    (0..n)
        .map(|t| {
            let (src, dst): (Vec<Vec2>, Vec<Vec2>) = tracks
                .iter()
                .filter_map(|tr| {
                    let (a, b) = (tr.get(reference)?, tr.get(t)?);
                    (a.valid() && b.valid()).then_some((a.pos, b.pos))
                })
                .unzip();
            if src.is_empty() {
                return None;
            }
            if t == reference {
                return Some(Affine::IDENTITY);
            }
            let m = if src.len() == 1 { TrackMethod::Position } else { method };
            let fit = if src.len() >= 4 { fit_robust(m, &src, &dst).map(|r| r.0).or_else(|| fit_ls(m, &src, &dst)) } else { fit_ls(m, &src, &dst) };
            fit.filter(|a| [a.a, a.b, a.c, a.d, a.e, a.f].iter().all(|v| v.is_finite()) && a.determinant().abs() > 1e-9)
        })
        .collect()
}

/// Match move: per frame, the forward map to apply to an overlay placed in the reference frame
/// so it follows the tracked points (= [`motion_path`]).
pub fn match_move(tracks: &[Vec<TrackSample>], reference: usize, method: TrackMethod) -> Vec<Option<Affine>> {
    motion_path(tracks, reference, method)
}

/// Stabilise: per frame, the map to apply to the frame's picture so the tracked points stay
/// where they are in the reference frame. With `smooth_radius` > 0 (frames, capped at
/// [`MAX_SMOOTH_RADIUS`]) only the motion that departs from a smoothed camera path is cancelled
/// (Gaussian-weighted local linear fit, σ = radius / 2, over the valid frames within ±radius), so
/// pans survive. The local line (rather than a plain weighted mean) keeps a pan unbiased where the
/// window is cut short — at the ends of the shot and next to gaps — instead of slowing it there.
pub fn stabilise(tracks: &[Vec<TrackSample>], reference: usize, method: TrackMethod, smooth_radius: u32) -> Vec<Option<Affine>> {
    let path = motion_path(tracks, reference, method);
    let radius = smooth_radius.min(MAX_SMOOTH_RADIUS) as usize;
    if radius == 0 {
        return path.iter().map(|m| m.and_then(|m| m.inverse())).collect();
    }
    // camera path parameters about the reference points' centroid: p ↦ s R(θ)(p − c) + c + T
    let refs: Vec<Vec2> = tracks.iter().take(MAX_POINTS).filter_map(|t| t.get(reference).filter(|s| s.valid()).map(|s| s.pos)).collect();
    if refs.is_empty() {
        return vec![None; path.len()];
    }
    let c = refs.iter().fold(Vec2::ZERO, |a, p| a + *p) * (1.0 / refs.len() as f64);
    let mut params: Vec<Option<[f64; 4]>> = path
        .iter()
        .map(|m| {
            let m = (*m)?;
            let t = m.apply(c) - c;
            let s = (m.a * m.a + m.b * m.b).sqrt();
            (s > 1e-9 && s.is_finite()).then(|| [t.x, t.y, m.b.atan2(m.a), s.ln()])
        })
        .collect();
    // unwrap the angle so the average doesn't jump at ±π
    let mut last: Option<f64> = None;
    for p in params.iter_mut().flatten() {
        if let Some(l) = last {
            p[2] = l + (p[2] - l + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
        }
        last = Some(p[2]);
    }
    let sigma = (radius as f64 / 2.0).max(0.5);
    let weights: Vec<f64> = (0..=radius).map(|d| (-0.5 * (d as f64 / sigma).powi(2)).exp()).collect();
    (0..path.len())
        .map(|t| {
            let m = path.get(t).copied().flatten()?;
            let (lo, hi) = (t.saturating_sub(radius), t.saturating_add(radius).min(path.len().saturating_sub(1)));
            // weighted sums of 1, d, d² and of p, d·p, with d = u − t
            let (mut t0, mut t1, mut s0, mut s1, mut s2) = ([0.0f64; 4], [0.0f64; 4], 0.0, 0.0, 0.0);
            for (u, p) in params.iter().enumerate().take(hi + 1).skip(lo) {
                let (Some(p), Some(w)) = (p, weights.get(u.abs_diff(t))) else { continue };
                let d = u as f64 - t as f64;
                t0.iter_mut().zip(p).for_each(|(a, v)| *a += w * v);
                t1.iter_mut().zip(p).for_each(|(a, v)| *a += w * d * v);
                s0 += w;
                s1 += w * d;
                s2 += w * d * d;
            }
            if s0 <= 0.0 {
                return None;
            }
            // the line's value at d = 0; the plain mean when the samples can't fix a slope
            let den = s0 * s2 - s1 * s1;
            let fit: [f64; 4] = if den > 1e-9 * s0 * s2.max(1.0) {
                std::array::from_fn(|i| (s2 * t0.get(i).copied().unwrap_or(0.0) - s1 * t1.get(i).copied().unwrap_or(0.0)) / den)
            } else {
                t0.map(|a| a / s0)
            };
            if !fit.iter().all(|v| v.is_finite()) {
                return None;
            }
            let [tx, ty, th, ls] = fit;
            let (s, (sn, cs)) = (ls.exp(), th.sin_cos());
            let smooth = Affine::translate(c.x + tx, c.y + ty)
                .then_apply(&Affine { a: s * cs, b: s * sn, c: -s * sn, d: s * cs, e: 0.0, f: 0.0 })
                .then_apply(&Affine::translate(-c.x, -c.y));
            Some(smooth.then_apply(&m.inverse()?))
        })
        .collect()
}

/// Corner pin from exactly four tracks: per frame, the homography mapping the four reference
/// positions to the frame's (exact 4-point DLT). None (the whole result) unless there are four
/// tracks; per frame None where a point is invalid or three are collinear.
pub fn corner_pin(tracks: &[Vec<TrackSample>], reference: usize) -> Option<Vec<Option<Homography>>> {
    let [a, b, c, d] = tracks else { return None };
    let four = [a, b, c, d];
    let n = frame_count(tracks);
    Some(
        (0..n)
            .map(|t| {
                let mut src = [Vec2::ZERO; 4];
                let mut dst = [Vec2::ZERO; 4];
                for ((s, q), tr) in src.iter_mut().zip(dst.iter_mut()).zip(four) {
                    let (r, f) = (tr.get(reference)?, tr.get(t)?);
                    if !(r.valid() && f.valid()) {
                        return None;
                    }
                    (*s, *q) = (r.pos, f.pos);
                }
                fit_homography(&src, &dst)
            })
            .collect(),
    )
}

/// `quad` mapped through `h` (None when a corner goes to infinity).
pub fn map_quad(h: &Homography, quad: &[Vec2; 4]) -> Option<[Vec2; 4]> {
    let [a, b, c, d] = quad.map(|p| h.apply(p));
    Some([a?, b?, c?, d?])
}

fn quad_area(q: &[Vec2; 4]) -> f64 {
    let mut s = 0.0;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        s += a.x * b.y - b.x * a.y;
    }
    s * 0.5
}

// ---- planar tracker ------------------------------------------------------------------------

/// One frame of a planar track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanarSample {
    /// Reference-frame pixels → this frame's pixels. While lost, the last good map (a hold).
    pub homography: Homography,
    /// The user quad (reference-frame pixels) mapped through `homography`: the corner pin.
    pub corners: [Vec2; 4],
    /// Inlier ratio of the reference re-anchoring, 0..1.
    pub confidence: f64,
    pub inliers: usize,
    pub lost: bool,
}

/// Tracks a planar region (a screen, a sign, a wall) as a homography from a reference frame.
/// Frames are shared, not copied (a 6K pyramid is ~140 MB): the tracker keeps the reference and
/// the previous frame by `Arc`.
#[derive(Clone)]
pub struct PlanarTracker {
    reference: Arc<Prepared>,
    region: Vec<Vec2>,
    quad: [Vec2; 4],
    prev: Arc<Prepared>,
    h: Homography,
    size: (usize, usize),
}

impl PlanarTracker {
    /// Track `region` (polygon, reference pixels, 3..=[`MAX_REGION_POINTS`] vertices) of
    /// `reference`; `quad` is the corner-pin quad to report (reference pixels). None when the
    /// frame is unusable, a number is not finite, the region is degenerate / outside the frame
    /// or holds too little detail (fewer than [`PLANAR_MIN_INLIERS`] features).
    pub fn new(reference: &Prepared, region: &[Vec2], quad: [Vec2; 4]) -> Option<Self> {
        Self::new_shared(Arc::new(reference.clone()), region, quad)
    }

    /// [`PlanarTracker::new`] on a shared frame (no copy).
    pub fn new_shared(reference: Arc<Prepared>, region: &[Vec2], quad: [Vec2; 4]) -> Option<Self> {
        if !usable(&reference) || region.len() < 3 || region.len() > MAX_REGION_POINTS || !region.iter().chain(&quad).all(|p| finite(*p)) {
            return None;
        }
        if track::features_in(&reference, region, MAX_FEATURES, 5.0).len() < PLANAR_MIN_INLIERS {
            return None;
        }
        let g = reference.base();
        let size = (g.w, g.h);
        Some(Self { prev: Arc::clone(&reference), reference, region: region.to_vec(), quad, h: Homography::IDENTITY, size })
    }

    /// The reference frame's sample (identity).
    pub fn first_sample(&self) -> PlanarSample {
        PlanarSample { homography: Homography::IDENTITY, corners: self.quad, confidence: 1.0, inliers: 0, lost: false }
    }

    fn lost(&self) -> PlanarSample {
        PlanarSample { homography: self.h, corners: map_quad(&self.h, &self.quad).unwrap_or(self.quad), confidence: 0.0, inliers: 0, lost: true }
    }

    /// Track into the next frame (forward or backward in time). The frame is kept (shared) as
    /// the next step's previous frame.
    pub fn step(&mut self, next: &Arc<Prepared>) -> PlanarSample {
        let next_shared = Arc::clone(next);
        let next: &Prepared = next;
        if !usable(next) || (next.base().w, next.base().h) != self.size {
            return self.lost();
        }
        // predict frame to frame (the region where it was last seen), else hold the last map
        let seen: Vec<Vec2> = self.region.iter().filter_map(|p| self.h.apply(*p)).collect();
        let chained =
            (seen.len() == self.region.len()).then(|| track_step_perspective(&self.prev, next, &seen)).flatten().map(|st| st.transform.after(&self.h));
        self.prev = next_shared;
        let mut est = chained.unwrap_or(self.h);
        // re-anchor on the reference; a second pass when the first moved the quad noticeably
        let mut result = None;
        for _ in 0..2 {
            let Some((h, inl, feats)) = self.reanchor(next, &est) else { break };
            let moved = match (map_quad(&h, &self.quad), map_quad(&est, &self.quad)) {
                (Some(a), Some(b)) => a.iter().zip(&b).map(|(p, q)| (*p - *q).length()).fold(0.0, f64::max),
                _ => f64::INFINITY,
            };
            est = h;
            result = Some((h, inl, feats));
            if moved < 0.05 {
                break;
            }
        }
        let Some((h, inliers, feats)) = result else { return self.lost() };
        let confidence = (inliers as f64 / feats.max(1) as f64).clamp(0.0, 1.0);
        let corners = map_quad(&h, &self.quad).filter(|q| q.iter().all(|p| finite(*p)) && quad_area(q).abs() > 1.0);
        match corners {
            Some(corners) if inliers >= PLANAR_MIN_INLIERS && confidence >= PLANAR_LOST_BELOW => {
                self.h = h;
                PlanarSample { homography: h, corners, confidence, inliers, lost: false }
            }
            _ => PlanarSample { confidence, inliers, ..self.lost() },
        }
    }

    /// Warp the reference into `next` through `est`, track the warped picture's features into
    /// `next` and refit the homography from reference coordinates: (map, inliers, features).
    /// Only a crop around the predicted region (plus [`CROP_MARGIN`]) is warped and pyramided,
    /// so the cost follows the region, not the frame (a 6K frame is ~100 MB of f32 per copy).
    fn reanchor(&self, next: &Prepared, est: &Homography) -> Option<(Homography, usize, usize)> {
        let inv = est.inverse()?;
        let region: Vec<Vec2> = self.region.iter().map(|p| est.apply(*p)).collect::<Option<_>>()?;
        let g = next.base();
        let (w, h) = (g.w, g.h);
        let lo = region.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |a, p| Vec2::new(a.x.min(p.x), a.y.min(p.y)));
        let hi = region.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, p| Vec2::new(a.x.max(p.x), a.y.max(p.y)));
        let span = |lo: f64, hi: f64, margin: f64, n: usize| -> (usize, usize) {
            ((lo - margin).floor().clamp(0.0, n as f64) as usize, (hi + margin).ceil().clamp(0.0, n as f64) as usize)
        };
        let (cx0, cx1) = span(lo.x, hi.x, CROP_MARGIN, w);
        let (cy0, cy1) = span(lo.y, hi.y, CROP_MARGIN, h);
        let (wx0, wx1) = span(lo.x, hi.x, WARP_MARGIN, w);
        let (wy0, wy1) = span(lo.y, hi.y, WARP_MARGIN, h);
        let (cw, ch) = (cx1.saturating_sub(cx0), cy1.saturating_sub(cy0));
        if cw < MIN_FRAME || ch < MIN_FRAME {
            return None;
        }
        let mut here = Vec::with_capacity(cw.saturating_mul(ch));
        for y in cy0..cy1 {
            let start = y.checked_mul(w)?.checked_add(cx0)?;
            here.extend_from_slice(g.px.get(start..start.checked_add(cw)?)?);
        }
        let rb = self.reference.base();
        let (rw, rh) = (rb.w as f64, rb.h as f64);
        let mut px = here.clone();
        px.par_chunks_mut(cw).enumerate().for_each(|(yy, row)| {
            let y = cy0 + yy;
            if y < wy0 || y >= wy1 {
                return;
            }
            for (xx, o) in row.iter_mut().enumerate() {
                let x = cx0 + xx;
                if x < wx0 || x >= wx1 {
                    continue;
                }
                if let Some(s) = inv.apply(Vec2::new(x as f64, y as f64))
                    && s.x >= -1.0
                    && s.y >= -1.0
                    && s.x <= rw
                    && s.y <= rh
                {
                    *o = rb.sample(s.x as f32, s.y as f32);
                }
            }
        });
        let warped = Prepared::new(Gray { w: cw, h: ch, px });
        let here = Prepared::new(Gray { w: cw, h: ch, px: here });
        let off = Vec2::new(cx0 as f64, cy0 as f64);
        let local: Vec<Vec2> = region.iter().map(|p| *p - off).collect();
        let feats = track::features_in(&warped, &local, MAX_FEATURES, 5.0);
        if feats.len() < PLANAR_MIN_INLIERS {
            return None;
        }
        let pairs: Vec<(Vec2, Vec2)> = feats
            .par_iter()
            .filter_map(|&p| {
                let q = lk_from(&warped.levels, &here.levels, p, [0.0, 0.0])?;
                let back = lk_from(&here.levels, &warped.levels, q, [0.0, 0.0])?;
                let fb = ((back[0] - p[0]).powi(2) + (back[1] - p[1]).powi(2)).sqrt();
                if fb >= FB_MAX {
                    return None;
                }
                Some((inv.apply(Vec2::new(p[0] as f64, p[1] as f64) + off)?, Vec2::new(q[0] as f64, q[1] as f64) + off))
            })
            .collect();
        let (src, dst): (Vec<Vec2>, Vec<Vec2>) = pairs.into_iter().unzip();
        let (hm, inl) = fit_homography_robust(&src, &dst)?;
        Some((hm, inl, feats.len()))
    }
}

/// Track `region` of `frames[start]` forward to the end and backward to frame 0; one sample per
/// frame (`quad` is the corner-pin quad, reference pixels). None when the start is out of range
/// or the region can't be tracked.
pub fn track_plane(frames: &[Arc<Prepared>], start: usize, region: &[Vec2], quad: [Vec2; 4]) -> Option<Vec<PlanarSample>> {
    if frames.len() > MAX_RUN_FRAMES {
        return None;
    }
    let tracker = PlanarTracker::new_shared(Arc::clone(frames.get(start)?), region, quad)?;
    let mut out = vec![tracker.first_sample(); frames.len()];
    let mut fwd = tracker.clone();
    for (o, f) in out.iter_mut().zip(frames).skip(start + 1) {
        *o = fwd.step(f);
    }
    let mut back = tracker;
    for (o, f) in out.iter_mut().zip(frames).take(start).rev() {
        *o = back.step(f);
    }
    Some(out)
}

#[cfg(test)]
#[path = "track_run_tests.rs"]
mod tests;
