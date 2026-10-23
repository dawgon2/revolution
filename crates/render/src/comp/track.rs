//! Tracker and Planar Tracker nodes (fork C3.5a): the transform a node's stored tracks give at a
//! comp time, and the projective draw the CPU runs for it. The solves are the trackers' own
//! ([`crate::track_run`]: match move, stabilise, corner pin; [`crate::track::fit_homography`]
//! for the plane), evaluated from the keyframes analysis wrote, so a render never tracks.
//!
//! Coordinates are comp pixels (continuous, pixel centres at +0.5), the same as Transform's
//! position and the masks'; the trackers' frame pixels (centres at integers) are converted at the
//! analysis boundary (`filmcraft_engine::comp_track`).

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::comp::{CompKind, TrackOperation, TrackerPoint};
use filmcraft_project::roto::Roto;
use filmcraft_project::{MaskPath, MaskVertex, Param, ParamValue, TrackMethod};
use filmcraft_time::{FrameRate, Tick};
use rayon::prelude::*;

use crate::image::Image;
use crate::track::{Homography, fit_homography};
use crate::track_run::{TrackSample, match_move, stabilise};

/// How a tracker node moves a picture at one time (comp pixels, source → destination).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackWarp {
    Affine(Affine),
    Projective(Homography),
}

impl TrackWarp {
    /// The same map for a picture `s` times the comp size.
    pub fn scaled(self, s: f64) -> TrackWarp {
        let (k, ik) = (Affine::scale(s, s), Affine::scale(1.0 / s, 1.0 / s));
        match self {
            TrackWarp::Affine(m) => TrackWarp::Affine(k.then_apply(&m).then_apply(&ik)),
            TrackWarp::Projective(h) => TrackWarp::Projective(homography_of(&k).after(&h).after(&homography_of(&ik))),
        }
    }

    /// An affine map when the projective part is (numerically) zero: the GPU can draw it.
    pub fn simplified(self) -> TrackWarp {
        match self {
            TrackWarp::Projective(h) if h.m[6].abs() < 1e-12 && h.m[7].abs() < 1e-12 && (h.m[8] - 1.0).abs() < 1e-12 => {
                TrackWarp::Affine(Affine { a: h.m[0], b: h.m[3], c: h.m[1], d: h.m[4], e: h.m[2], f: h.m[5] })
            }
            w => w,
        }
    }
}

/// What a tracker node draws at a time: `warp` applied to the foreground (input 1) merged over
/// input 0 when `foreground`, else to input 0 alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackPlan {
    pub warp: TrackWarp,
    pub foreground: bool,
}

/// An affine map as a homography.
pub fn homography_of(m: &Affine) -> Homography {
    Homography { m: [m.a, m.c, m.e, m.b, m.d, m.f, 0.0, 0.0, 1.0] }
}

/// The frame rectangle of a `w`×`h` picture, clockwise from the top left.
pub fn frame_quad(w: f64, h: f64) -> [Vec2; 4] {
    [Vec2::new(0.0, 0.0), Vec2::new(w, 0.0), Vec2::new(w, h), Vec2::new(0.0, h)]
}

fn finite(p: Vec2) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

/// A track's position at `t` when `t` is within its analysed keyframes (first ..= last).
fn tracked_at(p: &Param, t: Tick) -> Option<Vec2> {
    let (first, last) = (p.keyframes.first()?, p.keyframes.last()?);
    (first.time <= t && t <= last.time).then(|| p.value_at(t).as_vec2()).flatten().filter(|v| finite(*v))
}

/// A track's position at `t` (held before the first and after the last keyframe).
fn held_at(p: &Param, t: Tick) -> Option<Vec2> {
    p.value_at(t).as_vec2().filter(|v| finite(*v))
}

fn sample(p: &Param, t: Tick) -> TrackSample {
    match tracked_at(p, t) {
        Some(pos) => TrackSample { pos, confidence: 1.0, lost: false },
        None => TrackSample { pos: Vec2::ZERO, confidence: 0.0, lost: true },
    }
}

/// The sample at the reference: held before / after the analysed time (the reference is where
/// the foreground is placed, wherever the analysis ran).
fn held_sample(p: &Param, t: Tick) -> TrackSample {
    match held_at(p, t) {
        Some(pos) => TrackSample { pos, confidence: 1.0, lost: false },
        None => TrackSample { pos: Vec2::ZERO, confidence: 0.0, lost: true },
    }
}

/// The analysed points (those with keyframes), in order.
fn analysed(points: &[TrackerPoint]) -> Vec<&Param> {
    points.iter().map(|p| &p.track).filter(|p| !p.keyframes.is_empty()).collect()
}

/// `t` moved into the time the analysed tracks cover (their earliest first .. latest last
/// keyframe), so a render outside it holds the nearest solve.
fn clamp_to_tracks(tracks: &[&Param], t: Tick) -> Tick {
    let lo = tracks.iter().filter_map(|p| p.keyframes.first().map(|k| k.time)).min();
    let hi = tracks.iter().filter_map(|p| p.keyframes.last().map(|k| k.time)).max();
    match (lo, hi) {
        (Some(lo), Some(hi)) if lo <= hi => t.clamp(lo, hi),
        _ => t,
    }
}

/// The plan of a Tracker or Planar Tracker node at comp time `t` for a `size` comp (pixels) at
/// `rate`; `has_fg` = its input 1 is connected. None for other kinds; an identity warp when
/// nothing has been analysed (or the solve fails at `t`).
pub fn plan(kind: &CompKind, rate: FrameRate, t: Tick, size: (f64, f64), has_fg: bool) -> Option<TrackPlan> {
    let (warp, op) = match kind {
        CompKind::Tracker { points, operation, smooth_radius, reference, method, corner_points } => {
            (tracker_warp(points, corner_points, *operation, *smooth_radius, *reference, *method, rate, t, size), *operation)
        }
        CompKind::PlanarTracker { region, quad, reference, corners, operation, .. } => {
            let region: Vec<Vec2> = region.vertices.iter().map(|v| v.p).collect();
            let quad = reference_quad(quad, &region, size);
            (planar_warp(&quad, *reference, corners, *operation, t, size), *operation)
        }
        _ => return None,
    };
    let warp = warp.map_or(TrackWarp::Affine(Affine::IDENTITY), TrackWarp::simplified);
    Some(TrackPlan { warp, foreground: has_fg && op != TrackOperation::Stabilise })
}

/// A Planar Tracker's quad at the reference: `quad`, else (all zero) the region's bounding box,
/// else the frame.
pub fn reference_quad(quad: &[Vec2; 4], region: &[Vec2], (w, h): (f64, f64)) -> [Vec2; 4] {
    if quad.iter().any(|p| *p != Vec2::ZERO) && quad.iter().all(|p| finite(*p)) {
        return *quad;
    }
    let pts: Vec<Vec2> = region.iter().copied().filter(|p| finite(*p)).collect();
    if pts.len() >= 3 {
        let lo = pts.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |a, p| Vec2::new(a.x.min(p.x), a.y.min(p.y)));
        let hi = pts.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, p| Vec2::new(a.x.max(p.x), a.y.max(p.y)));
        if hi.x - lo.x > 1.0 && hi.y - lo.y > 1.0 {
            return [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
        }
    }
    frame_quad(w, h)
}

#[allow(clippy::too_many_arguments)]
fn tracker_warp(
    points: &[TrackerPoint],
    corner_points: &[u32],
    op: TrackOperation,
    radius: u32,
    reference: Tick,
    method: TrackMethod,
    rate: FrameRate,
    t: Tick,
    (w, h): (f64, f64),
) -> Option<TrackWarp> {
    let tracks = analysed(points);
    if tracks.is_empty() {
        return None;
    }
    let t = clamp_to_tracks(&tracks, t);
    match op {
        TrackOperation::CornerPin => {
            // the picture's corners onto the four chosen points (LO.5), else points 1-4 (held
            // outside their analysed time); a chosen point that isn't analysed gives no warp
            let dst: Vec<Vec2> = match filmcraft_project::comp::valid_corner_points(corner_points, points.len()) {
                Some(four) => four
                    .iter()
                    .map(|&i| points.get(i).map(|p| &p.track).filter(|p| !p.keyframes.is_empty()).and_then(|p| held_at(p, t)))
                    .collect::<Option<_>>()?,
                None => tracks.iter().take(4).map(|p| held_at(p, t)).collect::<Option<_>>()?,
            };
            (dst.len() == 4).then(|| fit_homography(&frame_quad(w, h), &dst)).flatten().map(TrackWarp::Projective)
        }
        TrackOperation::MatchMove => {
            let set: Vec<Vec<TrackSample>> = tracks.iter().map(|p| vec![held_sample(p, reference), sample(p, t)]).collect();
            match_move(&set, 0, method).get(1).copied().flatten().map(TrackWarp::Affine)
        }
        TrackOperation::Stabilise => {
            // the reference, then the frames t - r ..= t + r: the smoothing window of t (index
            // r + 1) never reaches the reference at index 0 (it is r + 1 away)
            let r = radius.min(filmcraft_project::comp::MAX_SMOOTH_RADIUS);
            let fd = rate.frame_duration().0.max(1);
            let at = |k: i64| Tick(t.0.saturating_add(fd.saturating_mul(k)));
            let window = -i64::from(r)..=i64::from(r);
            let set: Vec<Vec<TrackSample>> =
                tracks.iter().map(|p| std::iter::once(held_sample(p, reference)).chain(window.clone().map(|k| sample(p, at(k)))).collect()).collect();
            let i = usize::try_from(r).ok()?.checked_add(1)?;
            stabilise(&set, 0, method, r).get(i).copied().flatten().map(TrackWarp::Affine)
        }
    }
}

fn planar_warp(quad: &[Vec2; 4], reference: Tick, corners: &[Param], op: TrackOperation, t: Tick, (w, h): (f64, f64)) -> Option<TrackWarp> {
    let [a, b, c, d] = corners else { return None };
    let four = [a, b, c, d];
    if four.iter().any(|p| p.keyframes.is_empty()) {
        return None;
    }
    let at = |t: Tick| -> Option<Vec<Vec2>> { four.iter().map(|p| held_at(p, t)).collect() };
    let now = at(t)?;
    match op {
        TrackOperation::CornerPin => fit_homography(&frame_quad(w, h), &now).map(TrackWarp::Projective),
        TrackOperation::MatchMove | TrackOperation::Stabilise => {
            // reference → now through the quad (the analysed corners at the reference, when
            // there are any, else the quad as drawn)
            let base = at(reference).unwrap_or_else(|| quad.to_vec());
            let hm = fit_homography(&base, &now)?;
            if op == TrackOperation::Stabilise { hm.inverse().map(TrackWarp::Projective) } else { Some(TrackWarp::Projective(hm)) }
        }
    }
}

/// The motion a tracker node measured from its reference frame to `t` (comp pixels): the match
/// move whatever the node's own operation (a Stabilise or Corner Pin tracker still says how the
/// object moved), for track-assisted roto (fork C3.7). `output` picks what is followed: `""` or
/// `"transform"` = the node's solve (all its points with its method, or the plane); a point's name
/// = that point's translation alone. None when the node is not a tracker, has not been analysed,
/// names no such point, or the solve fails at `t` (the shape then stays where it was drawn).
pub fn follow_warp(kind: &CompKind, rate: FrameRate, t: Tick, size: (f64, f64), output: &str) -> Option<TrackWarp> {
    let whole = output.is_empty() || output.eq_ignore_ascii_case("transform");
    let warp = match kind {
        CompKind::Tracker { points, reference, method, .. } if whole => {
            tracker_warp(points, &[], TrackOperation::MatchMove, 0, *reference, *method, rate, t, size)
        }
        CompKind::Tracker { points, reference, .. } => {
            let p = points.iter().find(|p| p.name == output)?;
            if p.track.keyframes.is_empty() {
                return None;
            }
            let t = clamp_to_tracks(&[&p.track], t);
            let d = held_at(&p.track, t)? - held_at(&p.track, *reference)?;
            Some(TrackWarp::Affine(Affine::translate(d.x, d.y)))
        }
        CompKind::PlanarTracker { region, quad, reference, corners, .. } if whole => {
            let region: Vec<Vec2> = region.vertices.iter().map(|v| v.p).collect();
            let quad = reference_quad(quad, &region, size);
            planar_warp(&quad, *reference, corners, TrackOperation::MatchMove, t, size)
        }
        _ => None,
    };
    warp.map(TrackWarp::simplified)
}

/// `path` moved by `warp` (comp pixels): an affine map moves the points and tangents and scales
/// the feathers ([`MaskPath::transformed`]). A projective one maps each point and each tangent
/// handle's end as points (Bézier control points are points: exact for affine maps, and the
/// usual control-point approximation of a projected curve), falling back to the map's Jacobian
/// at the vertex for a handle whose end would leave the finite plane; feathers scale with the
/// local area change. If a vertex itself would leave the finite plane the whole path keeps its
/// place (as when the solve fails), so one vertex never springs back alone.
pub fn path_through(path: &MaskPath, warp: TrackWarp) -> MaskPath {
    match warp {
        TrackWarp::Affine(m) => path.transformed(&m),
        TrackWarp::Projective(h) => {
            let moved: Option<Vec<MaskVertex>> = path
                .vertices
                .iter()
                .map(|v| {
                    let p = h.apply(v.p).filter(|q| finite(*q))?;
                    let jac = jacobian(&h, v.p)?;
                    let handle = |t: Vec2| h.apply(v.p + t).filter(|q| finite(*q)).map(|q| q - p).unwrap_or_else(|| jac.apply(t));
                    let k = jac.determinant().abs().sqrt();
                    let k = if k.is_finite() && k > 0.0 { k } else { 1.0 };
                    Some(MaskVertex { p, t_in: handle(v.t_in), t_out: handle(v.t_out), feather_in: v.feather_in * k, feather_out: v.feather_out * k })
                })
                .collect();
            match moved {
                Some(vertices) if vertices.iter().all(|v| finite(v.t_in) && finite(v.t_out)) => MaskPath { vertices, closed: path.closed },
                _ => path.clone(),
            }
        }
    }
}

/// The linear part of `h` at `p` (its Jacobian, as an affine map without translation); None
/// near the line sent to infinity.
pub(crate) fn jacobian(h: &Homography, p: Vec2) -> Option<Affine> {
    let m = &h.m;
    let w = m[6] * p.x + m[7] * p.y + m[8];
    if !w.is_finite() || w.abs() < 1e-12 {
        return None;
    }
    let (u, v) = (m[0] * p.x + m[1] * p.y + m[2], m[3] * p.x + m[4] * p.y + m[5]);
    let w2 = w * w;
    let j =
        Affine { a: (m[0] * w - u * m[6]) / w2, c: (m[1] * w - u * m[7]) / w2, b: (m[3] * w - v * m[6]) / w2, d: (m[4] * w - v * m[7]) / w2, e: 0.0, f: 0.0 };
    [j.a, j.b, j.c, j.d].iter().all(|x| x.is_finite()).then_some(j)
}

impl TrackWarp {
    /// The inverse map (None when it is singular).
    pub fn inverse(self) -> Option<TrackWarp> {
        match self {
            TrackWarp::Affine(m) => m.inverse().map(TrackWarp::Affine),
            TrackWarp::Projective(h) => h.inverse().map(TrackWarp::Projective),
        }
    }
}

/// Whether a link names a node `kinds` knows as a Tracker or Planar Tracker (the link is
/// dangling otherwise: the shape stays where it was drawn).
pub fn link_target<'a>(tracker: &str, kinds: impl Fn(filmcraft_project::dag::NodeId) -> Option<&'a CompKind>) -> Option<&'a CompKind> {
    let id = tracker.trim().parse::<filmcraft_project::dag::NodeId>().ok()?;
    kinds(id).filter(|k| matches!(k, CompKind::Tracker { .. } | CompKind::PlanarTracker { .. }))
}

/// `roto` at `t` with its linked shapes drawn through their trackers' motion (fork C3.7): a
/// linked shape's outline (its keyframes, drawn relative to the tracker's reference frame) is
/// moved by [`follow_warp`] and held as a static outline for this time. Borrowed unchanged when
/// no shape follows a tracker that is there.
pub fn linked_roto<'a, 'k>(
    roto: &'a Roto,
    kinds: impl Fn(filmcraft_project::dag::NodeId) -> Option<&'k CompKind>,
    rate: FrameRate,
    t: Tick,
    size: (f64, f64),
) -> std::borrow::Cow<'a, Roto> {
    let warps: Vec<Option<TrackWarp>> =
        roto.shapes.iter().map(|sh| sh.track_link.as_ref().and_then(|l| follow_warp(link_target(&l.tracker, &kinds)?, rate, t, size, &l.output))).collect();
    if warps.iter().all(Option::is_none) {
        return std::borrow::Cow::Borrowed(roto);
    }
    let mut out = roto.clone();
    for (sh, w) in out.shapes.iter_mut().zip(warps) {
        if let Some(w) = w {
            let moved = path_through(&sh.path_at(t), w);
            sh.shape = Param::new(ParamValue::Path(moved));
        }
    }
    std::borrow::Cow::Owned(out)
}

/// `src` drawn through `h` (source pixels → destination pixels) onto a transparent `w`×`h`
/// picture, bilinear; minified maps sample a half-size copy first (as [`Image::transformed`]).
pub fn warp_projective(src: &Image, w: usize, h: usize, hm: &Homography) -> Image {
    let mut out = Image::new(w, h);
    let Some(inv) = hm.inverse() else { return out };
    // the map's area scale at the source centre: below 1/4 (half size each way) take a half copy
    let c = Vec2::new(src.w as f64 / 2.0, src.h as f64 / 2.0);
    let area = match (hm.apply(c), hm.apply(c + Vec2::new(1.0, 0.0)), hm.apply(c + Vec2::new(0.0, 1.0))) {
        (Some(p), Some(px), Some(py)) => {
            let (u, v) = (px - p, py - p);
            (u.x * v.y - u.y * v.x).abs()
        }
        _ => 1.0,
    };
    if area.is_finite() && area < 0.25 && src.w >= 4 && src.h >= 4 {
        let half = src.downsample2();
        return warp_projective(&half, w, h, &hm.after(&Homography { m: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0] }));
    }
    let (sw, sh) = (src.w as f64, src.h as f64);
    if w == 0 {
        return out;
    }
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let py = y as f64 + 0.5;
        for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let Some(s) = inv.apply(Vec2::new(x as f64 + 0.5, py)) else { continue };
            if s.x < -1.0 || s.y < -1.0 || s.x > sw + 1.0 || s.y > sh + 1.0 {
                continue;
            }
            px.copy_from_slice(&src.sample_bilinear(s.x as f32, s.y as f32));
        }
    });
    out
}

#[cfg(test)]
#[path = "track_tests.rs"]
mod tests;
