//! Mask coverage (CPU reference; `filmcraft_gpu::GpuMask` runs the same math in WGSL).
//!
//! Each [`Mask`] is flattened to a polygon in working-image pixels (chord error ≤ 0.05 px). For
//! a pixel centre `p` the signed distance `sd` to the polygon (positive inside, nonzero winding)
//! gives the coverage
//!
//! ```text
//! s  = sd + expansion
//! w  = max(feather, 1)
//! u  = clamp(s / w + 0.5, 0, 1)
//! c  = lerp(u, smoothstep(u), min(feather, 1))
//! m  = opacity · (inverted ? 1 − c : c)
//! ```
//!
//! so a hard mask (feather 0) is the analytic box-filtered coverage of the edge (a one-pixel
//! linear ramp across the boundary: exact for straight edges, antialiased everywhere), and a
//! feathered mask falls off smoothly over `feather` pixels centred on the (expanded) edge — a true
//! distance falloff, so corners round off and the width is the same along the whole contour.
//! A linear (gradient) mask is flattened by [`linear_polygon`] to the half-plane on its start
//! side with feather `|end − start| + feather`, so the same formula gives its smoothstep ramp
//! (1 at the start point, 0 at the end) on the CPU, in `GpuMask` and in the fused GPU effect
//! chain without a shape-specific shader path.
//! Masks combine top to bottom with their [`MaskMode`]. All of this runs in linear light on the
//! premultiplied working image: a masked effect is `lerp(original, effected, coverage)` per
//! channel, an opacity mask scales the layer's premultiplied pixels.

use filmcraft_geom::Vec2;
use filmcraft_project::{EffectInstance, Mask, MaskMode, MaskPath, MaskShape};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::comp::track::{TrackWarp, jacobian, path_through};
use crate::image::Image;
use crate::track::Homography;

/// Flattening tolerance in working pixels.
pub const FLATTEN_TOLERANCE: f64 = 0.05;

/// One mask ready for rasterizing, in working-image pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct FlatMask {
    /// Closed polygon (last vertex connects to the first).
    pub pts: Vec<[f32; 2]>,
    pub feather: f32,
    pub expansion: f32,
    /// 0..1.
    pub opacity: f32,
    pub inverted: bool,
    pub mode: MaskMode,
}

/// Flatten the active masks (mode ≠ None) at clip time `t`. `px_scale` = working pixels per clip pixel.
pub fn prepare(masks: &[Mask], t: Tick, px_scale: f32) -> Vec<FlatMask> {
    let s = px_scale as f64;
    masks
        .iter()
        .filter(|m| m.mode != MaskMode::None)
        .map(|m| {
            // a planar-tracked mask (fork G4.4) is drawn through its plane's motion at `t`; its
            // feather and expansion follow the plane's local scale at the shape's centre
            let path = m.path_at(t);
            let warp = planar_warp(m, t);
            let k = warp.as_ref().map_or(1.0, |h| local_scale(h, centroid(&path)));
            // hostile values (damaged project, absurd px_scale) never reach the coverage math:
            // feather / expansion are finite and capped, non-finite points are dropped
            let feather = finite_or_zero(m.feather.f64_at(t) * s * k).clamp(0.0, MAX_FEATHER);
            let expansion = finite_or_zero(m.expansion.f64_at(t) * s * k).clamp(-MAX_EXPANSION, MAX_EXPANSION) as f32;
            let (pts, feather) = if m.shape == MaskShape::Linear {
                let (a, b) = m.linear_points_at(t).unwrap_or_default();
                let (a, b) = match warp.as_ref().map(|h| (ahead(h, a), ahead(h, b))) {
                    Some((Some(a), Some(b))) => (a, b),
                    _ => (a, b),
                };
                linear_polygon([a.x * s, a.y * s], [b.x * s, b.y * s], feather, expansion)
            } else {
                (flatten_path(&path, s, warp.as_ref().map(|h| (h, k))), feather as f32)
            };
            let pts: Vec<[f32; 2]> = pts.into_iter().filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
            let opacity = finite_or_zero(m.opacity.f64_at(t) / 100.0).clamp(0.0, 1.0) as f32;
            FlatMask { pts, feather, expansion, opacity, inverted: m.inverted, mode: m.mode }
        })
        .collect()
}

/// Largest feather (working px) a mask gets; also the largest linear ramp.
const MAX_FEATHER: f64 = 1e5;
/// Largest |expansion| (working px).
const MAX_EXPANSION: f64 = 1e5;
/// Most flattened points per mask (a hostile path cannot allocate without bound).
pub const MAX_MASK_POINTS: usize = 100_000;
/// Most path vertices flattened (each cubic yields at most 256 chords).
const MAX_PATH_VERTICES: usize = 10_000;

fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// A path flattened to working pixels, at most [`MAX_MASK_POINTS`] points (coarser chords when a
/// huge path would exceed that, truncated as a last resort). Paths with more than
/// [`MAX_PATH_VERTICES`] vertices are treated as empty.
fn flatten_path(path: &filmcraft_project::MaskPath, s: f64, warp: Option<(&Homography, f64)>) -> Vec<[f32; 2]> {
    if path.len() > MAX_PATH_VERTICES || !s.is_finite() {
        return Vec::new();
    }
    // (a warp that magnifies by k needs k times finer chords before it)
    let k = warp.map_or(1.0, |w| w.1);
    let mut tol = finite_or_zero(FLATTEN_TOLERANCE / (s * k).max(1e-6)).max(1e-4);
    let mut flat = path.flatten(tol);
    if flat.len() > MAX_MASK_POINTS {
        // chord count scales with 1/√tol
        let r = flat.len() as f64 / MAX_MASK_POINTS as f64;
        tol *= r * r * 1.1;
        flat = path.flatten(tol);
    }
    flat.truncate(MAX_MASK_POINTS);
    // the flattened outline through the plane's map: exact for every curve. A point sent to
    // infinity (the plane seen edge-on) keeps the whole outline where it was drawn.
    if let Some((h, _)) = warp
        && let Some(moved) = flat.iter().map(|p| ahead(h, *p)).collect::<Option<Vec<_>>>()
    {
        flat = moved;
    }
    flat.into_iter().map(|p| [(p.x * s) as f32, (p.y * s) as f32]).collect()
}

/// `p` through `h` when it stays in front of the plane's horizon (`w` > 0, as everywhere on
/// the reference frame, where the map is the identity); None where the plane would fold over.
pub fn ahead(h: &Homography, p: Vec2) -> Option<Vec2> {
    let w = h.m[6] * p.x + h.m[7] * p.y + h.m[8];
    if w > 1e-6 { h.apply(p) } else { None }
}

/// The planar track's map from the mask's reference frame to clip pixels at `t` (fork G4.4);
/// None when the mask follows no planar track (or it has no usable key).
pub fn planar_warp(m: &Mask, t: Tick) -> Option<Homography> {
    m.planar.as_ref()?.homography_at(t).map(|m| Homography { m })
}

/// The mask's path at `t` where it is seen: through its planar track (Bézier control points
/// mapped as points, [`path_through`]: for handles and outlines on the monitor; coverage maps
/// the flattened outline instead, exactly). Where the coverage keeps the drawn outline (some of
/// it folds past the plane's horizon, see [`flatten_path`]) so does this, so the monitor's
/// handles and the render agree.
pub fn shown_path(m: &Mask, t: Tick) -> MaskPath {
    let path = m.path_at(t);
    let Some(h) = planar_warp(m, t) else { return path };
    if path.len() > MAX_PATH_VERTICES {
        return path;
    }
    let tol = (FLATTEN_TOLERANCE * 10.0 / local_scale(&h, centroid(&path))).max(1e-3);
    if path.flatten(tol).iter().all(|p| ahead(&h, *p).is_some()) { path_through(&path, TrackWarp::Projective(h)) } else { path }
}

/// √|det J| of `h` at `p`: how much it scales lengths there (1 where it can't be measured;
/// within 10⁻³ ..= 10³).
pub fn local_scale(h: &Homography, p: Vec2) -> f64 {
    let k = jacobian(h, p).map_or(1.0, |j| j.determinant().abs().sqrt());
    if k.is_finite() && k > 0.0 { k.clamp(1e-3, 1e3) } else { 1.0 }
}

/// Mean of a path's vertex positions (finite ones; the origin for none).
fn centroid(path: &MaskPath) -> Vec2 {
    let pts: Vec<Vec2> = path.vertices.iter().map(|v| v.p).filter(|p| p.x.is_finite() && p.y.is_finite()).collect();
    if pts.is_empty() {
        return Vec2::ZERO;
    }
    let sum = pts.iter().fold(Vec2::ZERO, |a, p| a + *p);
    sum * (1.0 / pts.len() as f64)
}

/// Largest ramp width (working px) of a linear mask; hostile point distances are capped here.
const LINEAR_MAX_RAMP: f64 = MAX_FEATHER;
/// Largest distance (working px) of a linear mask's midpoint from the origin (hostile points).
const LINEAR_MAX_CENTRE: f64 = 1e6;
/// Distance beyond which the image is assumed not to reach (a 32K frame is ~46 000 px across).
const LINEAR_REACH: f64 = 65_536.0;

/// A linear (gradient) mask from `a` (coverage 1) to `b` (coverage 0) as a polygon + feather the
/// shared coverage math turns into the gradient: the start half-plane, bounded by the
/// perpendicular bisector of `a`–`b`, as a quad reaching far beyond the image, with feather
/// `|b − a| + feather`. The signed distance to the bisector `sd` then gives
/// `smoothstep(sd / |b − a| + 0.5)`: exactly 1 at `a`, 0.5 at the midpoint, 0 at `b`. The
/// bisector is split at geometrically spaced points around the midpoint so the f32 distance to
/// its segments stays accurate near the image while its ends reach past the feather band. Equal
/// or non-finite points give no polygon (empty coverage, like a degenerate path).
pub fn linear_polygon(a: [f64; 2], b: [f64; 2], feather: f64, expansion: f32) -> (Vec<[f32; 2]>, f32) {
    let feather = if feather.is_finite() { feather.clamp(0.0, LINEAR_MAX_RAMP) } else { 0.0 };
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt();
    if !len.is_finite() || len < 1e-9 || !a.iter().chain(&b).all(|v| v.is_finite()) {
        return (Vec::new(), feather as f32);
    }
    let width = (len.min(LINEAR_MAX_RAMP) + feather).min(LINEAR_MAX_RAMP);
    // unit normal towards the end point, tangent along the bisector
    let (nx, ny) = (dx / len, dy / len);
    let (tx, ty) = (-ny, nx);
    // clamp the midpoint along the normal (keeps the side of the frame) and the tangent (slides
    // along the bisector), never per axis, which would turn the line
    let (cx, cy) = ((a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5);
    let cn = (cx * nx + cy * ny).clamp(-LINEAR_MAX_CENTRE, LINEAR_MAX_CENTRE);
    let ct = (cx * tx + cy * ty).clamp(-LINEAR_MAX_CENTRE, LINEAR_MAX_CENTRE);
    let (mx, my) = (nx * cn + tx * ct, ny * cn + ty * ct);
    let exp = if expansion.is_finite() { f64::from(expansion).abs().min(1e7) } else { 0.0 };
    // farther than the coverage band from every image pixel
    let reach = (mx * mx + my * my).sqrt() + LINEAR_REACH + exp + width + 2.0;
    let at = |o: f64, depth: f64| [(mx + tx * o - nx * depth) as f32, (my + ty * o - ny * depth) as f32];
    const STEPS: [f64; 5] = [256.0, 1024.0, 4096.0, 16_384.0, LINEAR_REACH];
    let mut pts = Vec::with_capacity(2 * STEPS.len() + 5);
    pts.push(at(-reach, 0.0));
    pts.extend(STEPS.iter().rev().map(|o| at(-o, 0.0)));
    pts.push(at(0.0, 0.0));
    pts.extend(STEPS.iter().map(|o| at(*o, 0.0)));
    pts.push(at(reach, 0.0));
    // back across the start side
    pts.push(at(reach, reach));
    pts.push(at(-reach, reach));
    (pts, width as f32)
}

/// The falloff of a signed (expanded) distance for a feather width.
#[inline]
pub fn falloff(s: f32, feather: f32) -> f32 {
    let w = feather.max(1.0);
    let u = (s / w + 0.5).clamp(0.0, 1.0);
    let smooth = u * u * (3.0 - 2.0 * u);
    u + (smooth - u) * feather.min(1.0)
}

/// Distance band beyond which coverage is saturated (0 or 1).
fn band(m: &FlatMask) -> f32 {
    m.expansion.abs() + m.feather.max(1.0) * 0.5 + 1.0
}

#[inline]
fn seg_dist2(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (qx, qy) = (px - a[0], py - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { ((qx * dx + qy * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (qx - dx * t, qy - dy * t);
    ex * ex + ey * ey
}

/// Coverage of one mask for row `y` into `out` (length = width). Reference per-row algorithm:
/// winding from the row's sorted edge crossings, distance only against edges within the band.
fn mask_row(m: &FlatMask, y: usize, out: &mut [f32]) {
    let n = m.pts.len();
    let py = y as f32 + 0.5;
    let b = band(m);
    let value = |sd: f32| {
        let c = falloff(sd + m.expansion, m.feather);
        m.opacity * if m.inverted { 1.0 - c } else { c }
    };
    if n < 3 {
        out.fill(value(-b));
        return;
    }
    // crossings of the scanline with the polygon edges (x, direction)
    let mut cross: Vec<(f32, i32)> = Vec::new();
    // edges within the band of this row: (a, b, min x, max x)
    let mut near: Vec<([f32; 2], [f32; 2], f32, f32)> = Vec::new();
    for i in 0..n {
        let a = m.pts[i];
        let c = m.pts[(i + 1) % n];
        if (a[1] <= py) != (c[1] <= py) {
            let t = (py - a[1]) / (c[1] - a[1]);
            cross.push((a[0] + (c[0] - a[0]) * t, if c[1] > a[1] { 1 } else { -1 }));
        }
        if py >= a[1].min(c[1]) - b && py <= a[1].max(c[1]) + b {
            near.push((a, c, a[0].min(c[0]) - b, a[0].max(c[0]) + b));
        }
    }
    cross.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut ci = 0;
    let mut wind = 0;
    let b2 = b * b;
    for (x, o) in out.iter_mut().enumerate() {
        let px = x as f32 + 0.5;
        while ci < cross.len() && cross[ci].0 < px {
            wind += cross[ci].1;
            ci += 1;
        }
        let inside = wind != 0;
        let mut d2 = b2;
        for (a, c, x0, x1) in &near {
            if px >= *x0 && px <= *x1 {
                d2 = d2.min(seg_dist2(px, py, *a, *c));
            }
        }
        let d = d2.sqrt();
        *o = value(if inside { d } else { -d });
    }
}

/// Combined coverage of `masks` over a `w`×`h` image, or `None` when there is no active mask
/// (the effect applies everywhere).
pub fn coverage(masks: &[FlatMask], w: usize, h: usize) -> Option<Vec<f32>> {
    let first = masks.iter().find(|m| m.mode != MaskMode::None)?;
    let start = first.mode.start();
    let mut acc = vec![start; w * h];
    if w == 0 {
        return Some(acc);
    }
    acc.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let mut tmp = vec![0.0f32; w];
        for m in masks.iter().filter(|m| m.mode != MaskMode::None) {
            mask_row(m, y, &mut tmp);
            for (a, v) in row.iter_mut().zip(&tmp) {
                *a = m.mode.combine(*a, *v).clamp(0.0, 1.0);
            }
        }
    });
    Some(acc)
}

/// Coverage of an effect's masks on a working image (`px_scale` working px per clip px).
pub fn effect_coverage(masks: &[Mask], t: Tick, px_scale: f32, w: usize, h: usize) -> Option<Vec<f32>> {
    if masks.is_empty() {
        return None;
    }
    coverage(&prepare(masks, t, px_scale), w, h)
}

/// `img = lerp(original, img, cov)` on premultiplied pixels.
pub fn mix(img: &mut Image, original: &Image, cov: &[f32]) {
    img.px.par_chunks_mut(4).zip(original.px.par_chunks(4)).zip(cov.par_iter()).for_each(|((p, o), &c)| {
        for k in 0..4 {
            p[k] = o[k] + (p[k] - o[k]) * c;
        }
    });
}

/// Scale premultiplied pixels by coverage (opacity masks).
pub fn scale_by(img: &mut Image, cov: &[f32]) {
    img.px.par_chunks_mut(4).zip(cov.par_iter()).for_each(|(p, &c)| {
        for v in p {
            *v *= c;
        }
    });
}

/// Apply an effect limited to its masks (Premiere: a masked effect only applies inside the mask).
pub fn apply_effect(img: &mut Image, e: &EffectInstance, cx: &crate::effects::FxCtx) {
    if !e.enabled || e.masks.is_empty() {
        crate::effects::apply(img, e, cx);
        return;
    }
    let Some(cov) = effect_coverage(&e.masks, cx.t, cx.px_scale, img.w, img.h) else {
        crate::effects::apply(img, e, cx);
        return;
    };
    let original = img.clone();
    crate::effects::apply(img, e, cx);
    mix(img, &original, &cov);
}

/// Apply the masks of an item's Opacity effect to its layer (before Motion), if any.
pub fn apply_opacity_masks(img: &mut Image, item: &filmcraft_project::TrackItem, t: Tick, px_scale: f32) {
    let Some(op) = item.effect("opacity").filter(|e| e.enabled && !e.masks.is_empty()) else { return };
    if let Some(cov) = effect_coverage(&op.masks, t, px_scale, img.w, img.h) {
        scale_by(img, &cov);
    }
}

/// Brute-force coverage of one pixel (every edge; the definition the row algorithm and the WGSL
/// shader implement). For tests and documentation.
pub fn coverage_at(masks: &[FlatMask], x: f32, y: f32) -> Option<f32> {
    let first = masks.iter().find(|m| m.mode != MaskMode::None)?;
    let mut acc = first.mode.start();
    for m in masks.iter().filter(|m| m.mode != MaskMode::None) {
        let n = m.pts.len();
        let mut wind = 0;
        let mut d2 = f32::INFINITY;
        for i in 0..n {
            let a = m.pts[i];
            let c = m.pts[(i + 1) % n];
            if (a[1] <= y) != (c[1] <= y) {
                let t = (y - a[1]) / (c[1] - a[1]);
                if a[0] + (c[0] - a[0]) * t < x {
                    wind += if c[1] > a[1] { 1 } else { -1 };
                }
            }
            d2 = d2.min(seg_dist2(x, y, a, c));
        }
        let d = if n < 3 { band(m) } else { d2.sqrt().min(band(m)) };
        let sd = if wind != 0 && n >= 3 { d } else { -d };
        let c = falloff(sd + m.expansion, m.feather);
        let v = m.opacity * if m.inverted { 1.0 - c } else { c };
        acc = m.mode.combine(acc, v).clamp(0.0, 1.0);
    }
    Some(acc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_geom::Vec2;
    use filmcraft_project::MaskPath;

    fn mask(path: MaskPath, feather: f64) -> Mask {
        let mut m = Mask::new("m", path);
        m.feather.value = filmcraft_project::ParamValue::Float(feather);
        m
    }

    #[test]
    fn hard_rect_is_exact_box_coverage() {
        // rect edges on pixel boundaries → exactly 1 inside, 0 outside; half-pixel edge → 0.5
        let m = mask(MaskPath::rect(2.0, 2.0, 6.5, 6.0), 0.0);
        let c = effect_coverage(&[m], Tick(0), 1.0, 10, 10).unwrap();
        assert_eq!(c[3 * 10 + 3], 1.0);
        assert_eq!(c[3 * 10 + 1], 0.0);
        assert!((c[3 * 10 + 6] - 0.5).abs() < 1e-6, "{}", c[3 * 10 + 6]);
        assert_eq!(c[8 * 10 + 3], 0.0);
    }

    #[test]
    fn ellipse_area_matches_analytic() {
        let (rx, ry) = (37.3, 21.7);
        let m = mask(MaskPath::ellipse(Vec2::new(50.2, 40.1), Vec2::new(rx, ry)), 0.0);
        let c = effect_coverage(&[m], Tick(0), 1.0, 100, 80).unwrap();
        let area: f64 = c.iter().map(|v| *v as f64).sum();
        let exact = std::f64::consts::PI * rx * ry;
        assert!((area - exact).abs() / exact < 0.002, "{area} vs {exact}");
    }

    #[test]
    fn feather_is_a_symmetric_distance_falloff() {
        let m = mask(MaskPath::rect(20.0, -100.0, 200.0, 200.0), 10.0);
        let c = effect_coverage(&[m], Tick(0), 1.0, 60, 4).unwrap();
        // edge at x = 20: 0.5 there, saturated 5 px away, symmetric
        let at = |x: usize| c[x];
        assert!((at(19) + at(20) - 1.0).abs() < 1e-5, "{} {}", at(19), at(20));
        assert!(at(13) < 1e-6 && at(26) > 1.0 - 1e-6, "{} {}", at(13), at(26));
        for x in 14..26 {
            assert!(at(x + 1) >= at(x), "monotonic");
        }
    }

    #[test]
    fn expansion_inverted_opacity_modes() {
        let mut m = mask(MaskPath::rect(10.0, 10.0, 20.0, 20.0), 0.0);
        m.expansion.value = filmcraft_project::ParamValue::Float(5.0);
        let c = effect_coverage(std::slice::from_ref(&m), Tick(0), 1.0, 30, 30).unwrap();
        assert_eq!(c[15 * 30 + 6], 1.0, "expanded by 5");
        assert_eq!(c[15 * 30 + 4], 0.0);
        m.inverted = true;
        m.opacity.value = filmcraft_project::ParamValue::Float(50.0);
        let c = effect_coverage(&[m.clone()], Tick(0), 1.0, 30, 30).unwrap();
        assert_eq!(c[15 * 30 + 15], 0.0);
        assert_eq!(c[2], 0.5);
        // subtract a hole from a big rect
        let mut big = mask(MaskPath::rect(0.0, 0.0, 30.0, 30.0), 0.0);
        big.mode = MaskMode::Add;
        let mut hole = mask(MaskPath::rect(10.0, 10.0, 20.0, 20.0), 0.0);
        hole.mode = MaskMode::Subtract;
        let c = effect_coverage(&[big, hole], Tick(0), 1.0, 30, 30).unwrap();
        assert_eq!(c[15 * 30 + 15], 0.0);
        assert_eq!(c[5 * 30 + 5], 1.0);
    }

    #[test]
    fn row_algorithm_matches_brute_force() {
        let mut a = mask(MaskPath::ellipse(Vec2::new(30.0, 25.0), Vec2::new(20.0, 12.0)), 6.0);
        a.expansion.value = filmcraft_project::ParamValue::Float(-2.0);
        let mut b = mask(MaskPath::polygon(&[Vec2::new(5.0, 5.0), Vec2::new(50.0, 10.0), Vec2::new(20.0, 45.0)]), 0.0);
        b.mode = MaskMode::Difference;
        b.inverted = true;
        let flat = prepare(&[a, b], Tick(0), 0.8);
        let (w, h) = (48, 40);
        let c = coverage(&flat, w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let r = coverage_at(&flat, x as f32 + 0.5, y as f32 + 0.5).unwrap();
                assert!((r - c[y * w + x]).abs() < 1e-5, "({x},{y}) {r} vs {}", c[y * w + x]);
            }
        }
    }

    fn smoothstep(u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        u * u * (3.0 - 2.0 * u)
    }

    /// Coverage of a lone linear mask over `w`×`h` against `expect(px, py)`.
    fn check_linear(m: &Mask, w: usize, h: usize, expect: impl Fn(f64, f64) -> f64) {
        let c = effect_coverage(std::slice::from_ref(m), Tick(0), 1.0, w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let e = expect(x as f64 + 0.5, y as f64 + 0.5);
                let v = c[y * w + x] as f64;
                assert!((v - e).abs() < 2e-4, "({x},{y}) {v} vs {e}");
            }
        }
    }

    #[test]
    fn linear_mask_is_a_smoothstep_ramp_from_start_to_end() {
        // vertical graduated filter: full at y = 10, none at y = 30
        let m = Mask::linear("m", Vec2::new(50.0, 10.0), Vec2::new(50.0, 30.0));
        check_linear(&m, 100, 40, |_, py| smoothstep((20.0 - py) / 20.0 + 0.5));
        let c = effect_coverage(std::slice::from_ref(&m), Tick(0), 1.0, 100, 40).unwrap();
        assert_eq!(c[5 * 100 + 3], 1.0, "start side");
        assert_eq!(c[35 * 100 + 97], 0.0, "far side");
        // a diagonal ramp, start bottom-right, at a non-unit pixel scale
        let d = Mask::linear("d", Vec2::new(60.0, 50.0), Vec2::new(20.0, 20.0));
        let flat = prepare(std::slice::from_ref(&d), Tick(0), 0.5);
        let c = coverage(&flat, 50, 40).unwrap();
        let (ax, ay, bx, by) = (30.0f64, 25.0, 10.0, 10.0);
        let len = ((ax - bx) * (ax - bx) + (ay - by) * (ay - by)).sqrt();
        for y in 0..40 {
            for x in 0..50 {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                // signed distance from the bisector towards the start
                let sd = ((px - (ax + bx) / 2.0) * (ax - bx) + (py - (ay + by) / 2.0) * (ay - by)) / len;
                let e = smoothstep(sd / len + 0.5);
                assert!((c[y * 50 + x] as f64 - e).abs() < 2e-4, "({x},{y}) {} vs {e}", c[y * 50 + x]);
            }
        }
    }

    #[test]
    fn linear_mask_feather_expansion_invert_opacity() {
        let mut m = Mask::linear("m", Vec2::new(5.0, 10.0), Vec2::new(5.0, 30.0));
        // feather widens the ramp around the midpoint: width 20 + 10
        m.feather.value = filmcraft_project::ParamValue::Float(10.0);
        check_linear(&m, 10, 40, |_, py| smoothstep((20.0 - py) / 30.0 + 0.5));
        // expansion moves the transition towards the end point
        m.feather.value = filmcraft_project::ParamValue::Float(0.0);
        m.expansion.value = filmcraft_project::ParamValue::Float(5.0);
        check_linear(&m, 10, 40, |_, py| smoothstep((25.0 - py) / 20.0 + 0.5));
        // inverted, half opacity
        m.expansion.value = filmcraft_project::ParamValue::Float(0.0);
        m.inverted = true;
        m.opacity.value = filmcraft_project::ParamValue::Float(50.0);
        check_linear(&m, 10, 40, |_, py| 0.5 * (1.0 - smoothstep((20.0 - py) / 20.0 + 0.5)));
        // a short ramp (< 1 px) is the antialiased hard edge of the bisector, like a hard path mask
        let h = Mask::linear("h", Vec2::new(5.0, 20.0), Vec2::new(5.0, 20.0 + 1e-6));
        let c = effect_coverage(&[h], Tick(0), 1.0, 10, 40).unwrap();
        assert_eq!((c[19 * 10], c[20 * 10]), (1.0, 0.0));
    }

    #[test]
    fn linear_mask_keyframes_and_combines_with_path_masks() {
        let mut m = Mask::linear("m", Vec2::new(0.0, 0.0), Vec2::new(0.0, 20.0));
        m.path.put_keyframe(Tick(0), filmcraft_project::ParamValue::Path(MaskPath::linear(Vec2::new(0.0, 0.0), Vec2::new(0.0, 20.0))));
        m.path.put_keyframe(Tick(100), filmcraft_project::ParamValue::Path(MaskPath::linear(Vec2::new(0.0, 20.0), Vec2::new(0.0, 40.0))));
        let flat = prepare(std::slice::from_ref(&m), Tick(50), 1.0);
        // midpoint at y = 20 halfway through
        let v = coverage_at(&flat, 3.0, 20.0).unwrap();
        assert!((v - 0.5).abs() < 1e-4, "{v}");
        // row algorithm == brute force with a linear mask between path masks
        let a = mask(MaskPath::ellipse(Vec2::new(30.0, 25.0), Vec2::new(20.0, 12.0)), 6.0);
        let mut l = Mask::linear("l", Vec2::new(40.0, 5.0), Vec2::new(10.0, 35.0));
        l.mode = MaskMode::Intersect;
        l.feather.value = filmcraft_project::ParamValue::Float(4.0);
        l.expansion.value = filmcraft_project::ParamValue::Float(-3.0);
        let mut b = mask(MaskPath::polygon(&[Vec2::new(5.0, 5.0), Vec2::new(50.0, 10.0), Vec2::new(20.0, 45.0)]), 0.0);
        b.mode = MaskMode::Difference;
        let flat = prepare(&[a, l, b], Tick(0), 0.8);
        let (w, h) = (48, 40);
        let c = coverage(&flat, w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let r = coverage_at(&flat, x as f32 + 0.5, y as f32 + 0.5).unwrap();
                assert!((r - c[y * w + x]).abs() < 1e-5, "({x},{y}) {r} vs {}", c[y * w + x]);
            }
        }
    }

    /// Regression: non-finite geometry / params used to pass through `prepare` and turn the CPU
    /// coverage (and a masked effect's pixels) NaN.
    #[test]
    fn prepare_keeps_every_mask_finite() {
        use filmcraft_project::ParamValue;
        let bad_poly =
            MaskPath::polygon(&[Vec2::new(1.0, 1.0), Vec2::new(f64::NAN, 2.0), Vec2::new(1e300, 5.0), Vec2::new(6.0, f64::INFINITY), Vec2::new(2.0, 7.0)]);
        let all_bad = MaskPath::polygon(&[Vec2::new(f64::NAN, 1.0), Vec2::new(f64::INFINITY, 2.0), Vec2::new(1e300, f64::NEG_INFINITY)]);
        let mut shapes = vec![
            mask(bad_poly, 0.0),
            mask(all_bad, 0.0),
            mask(MaskPath::rect(1.0, 1.0, 6.0, 6.0), 0.0),
            Mask::linear("l", Vec2::new(f64::NAN, 1.0), Vec2::new(3.0, 1e300)),
            Mask::linear("l2", Vec2::new(1.0, 1.0), Vec2::new(5.0, 7.0)),
        ];
        // a hostile huge path: bounded points
        shapes.push(mask(MaskPath::polygon(&(0..20_000).map(|i| Vec2::new(i as f64, (i % 7) as f64)).collect::<Vec<_>>()), 0.0));
        for (feather, expansion, opacity) in
            [(f64::INFINITY, -1e300, f64::NAN), (f64::NAN, f64::INFINITY, f64::INFINITY), (1e300, 1e300, -1e300), (2.0, 1.0, 50.0)]
        {
            for px_scale in [1.0f32, 1e30, 1e-30, f32::INFINITY, f32::NAN, 0.0] {
                for m in &mut shapes {
                    m.feather.value = ParamValue::Float(feather);
                    m.expansion.value = ParamValue::Float(expansion);
                    m.opacity.value = ParamValue::Float(opacity);
                    m.inverted = !m.inverted;
                }
                let flat = prepare(&shapes, Tick(0), px_scale);
                for f in &flat {
                    assert!(f.pts.len() <= MAX_MASK_POINTS);
                    assert!([f.feather, f.expansion, f.opacity].iter().chain(f.pts.iter().flatten()).all(|v| v.is_finite()), "{f:?}");
                }
                let c = coverage(&flat, 8, 8).unwrap();
                assert!(c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{feather} {expansion} {px_scale}: {c:?}");
                let mut img = Image { w: 8, h: 8, px: vec![0.5; 256] };
                let orig = Image { w: 8, h: 8, px: vec![0.25; 256] };
                mix(&mut img, &orig, &c);
                assert!(img.px.iter().all(|v| v.is_finite()));
            }
        }
    }

    /// A far-away diagonal ramp keeps the frame on its side (the midpoint is clamped along the
    /// normal and the tangent, not per axis, which would swing the line through the frame).
    #[test]
    fn far_linear_mask_keeps_its_side() {
        let a = Vec2::new(3e6, -2e6);
        for (start, end, want) in [(a, a + Vec2::new(10.0, 10.0), 1.0), (a + Vec2::new(10.0, 10.0), a, 0.0)] {
            let m = Mask::linear("far", start, end);
            let c = effect_coverage(&[m], Tick(0), 1.0, 64, 64).unwrap();
            assert!(c.iter().all(|v| *v == want), "{want}: {:?}", &c[..8]);
        }
    }

    #[test]
    fn linear_mask_survives_hostile_values() {
        let pts = [
            (Vec2::new(5.0, 5.0), Vec2::new(5.0, 5.0)),
            (Vec2::new(f64::NAN, 5.0), Vec2::new(5.0, 20.0)),
            (Vec2::new(5.0, 5.0), Vec2::new(f64::INFINITY, 20.0)),
            (Vec2::new(-1e300, 5.0), Vec2::new(1e300, 20.0)),
            (Vec2::new(1e12, 1e12), Vec2::new(1e12 + 10.0, 1e12)),
            (Vec2::new(-1e9, 3.0), Vec2::new(1e9, 7.0)),
        ];
        for (i, (a, b)) in pts.into_iter().enumerate() {
            for (feather, expansion) in [(0.0, 0.0), (f64::NAN, f64::NAN), (f64::INFINITY, f64::NEG_INFINITY), (1e30, 1e30), (-5.0, -1e9)] {
                for inverted in [false, true] {
                    let mut m = Mask::linear("m", a, b);
                    m.feather.value = filmcraft_project::ParamValue::Float(feather);
                    m.expansion.value = filmcraft_project::ParamValue::Float(expansion);
                    m.inverted = inverted;
                    let flat = prepare(std::slice::from_ref(&m), Tick(0), 1.0);
                    let c = coverage(&flat, 12, 12).unwrap();
                    assert!(c.iter().all(|v| (0.0..=1.0).contains(v)), "{i} {feather} {expansion}: {c:?}");
                    let r = coverage_at(&flat, 6.5, 6.5).unwrap();
                    assert!((0.0..=1.0).contains(&r), "{i}: {r}");
                }
            }
        }
        // equal points: empty coverage (the whole frame when inverted)
        let mut m = Mask::linear("m", Vec2::new(5.0, 5.0), Vec2::new(5.0, 5.0));
        assert!(effect_coverage(std::slice::from_ref(&m), Tick(0), 1.0, 8, 8).unwrap().iter().all(|v| *v == 0.0));
        m.inverted = true;
        assert!(effect_coverage(std::slice::from_ref(&m), Tick(0), 1.0, 8, 8).unwrap().iter().all(|v| *v == 1.0));
        // a linear mask whose path lost its vertices is empty, too
        m.path.value = filmcraft_project::ParamValue::Path(MaskPath::default());
        m.inverted = false;
        assert!(effect_coverage(&[m], Tick(0), 1.0, 8, 8).unwrap().iter().all(|v| *v == 0.0));
    }

    /// Fork G4.4: a planar-tracked mask is flattened in its reference frame and every outline
    /// point is mapped through the plane's homography (exact for curves); hostile tracks fall
    /// back to the drawn outline.
    #[test]
    fn planar_tracked_mask_maps_its_flattened_outline() {
        use filmcraft_project::{PlanarKey, PlanarTrack};
        let h = [1.1, 0.05, 12.0, -0.03, 0.95, 7.0, 0.0008, -0.0005, 1.0];
        let path = MaskPath::ellipse(Vec2::new(50.0, 40.0), Vec2::new(20.0, 12.0));
        let mut m = mask(path.clone(), 4.0);
        let plain = prepare(std::slice::from_ref(&m), Tick(0), 2.0);
        m.planar = Some(PlanarTrack { reference: Tick(0), keys: vec![PlanarKey { time: Tick(0), h }] });
        let warped = prepare(std::slice::from_ref(&m), Tick(0), 2.0);
        let hm = Homography { m: h };
        // every warped point lies on the true projected ellipse: un-project, check the implicit form
        let inv = hm.inverse().unwrap();
        let mut worst = 0.0f64;
        for p in &warped[0].pts {
            let o = inv.apply(Vec2::new(p[0] as f64 / 2.0, p[1] as f64 / 2.0)).unwrap();
            let r = (((o.x - 50.0) / 20.0).powi(2) + ((o.y - 40.0) / 12.0).powi(2)).sqrt();
            worst = worst.max((r - 1.0).abs() * 20.0);
        }
        assert!(worst < 0.05, "{worst} px off the projected ellipse");
        assert!(warped[0].pts.len() >= plain[0].pts.len());
        let k = local_scale(&hm, Vec2::new(50.0, 40.0)) as f32;
        assert!((warped[0].feather - plain[0].feather * k).abs() < 1e-4 && (k - 1.0).abs() > 0.005, "{k}");
        // the shown path's vertices are the vertices mapped
        let shown = shown_path(&m, Tick(0));
        for (a, b) in shown.vertices.iter().zip(&path.vertices) {
            assert!((a.p - hm.apply(b.p).unwrap()).length() < 1e-9);
        }
        // a plane sent to infinity at the outline keeps the drawn outline; never panics
        m.planar = Some(PlanarTrack { reference: Tick(0), keys: vec![PlanarKey { time: Tick(0), h: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, -0.02, 0.0, 1.0] }] });
        let edge = prepare(std::slice::from_ref(&m), Tick(0), 2.0);
        assert_eq!(edge[0].pts, plain[0].pts);
        // …and so do its handles on the monitor (the overlay agrees with the render)
        assert_eq!(shown_path(&m, Tick(0)), path);
        // a linear window's two points move with the plane
        let mut lin = Mask::linear("g", Vec2::new(10.0, 10.0), Vec2::new(10.0, 50.0));
        // (a vertical gradient moved down by 5 rows)
        lin.planar = Some(PlanarTrack { reference: Tick(0), keys: vec![PlanarKey { time: Tick(0), h: [1.0, 0.0, 0.0, 0.0, 1.0, 5.0, 0.0, 0.0, 1.0] }] });
        let a = effect_coverage(std::slice::from_ref(&lin), Tick(0), 1.0, 64, 64).unwrap();
        lin.planar = None;
        let b = effect_coverage(std::slice::from_ref(&lin), Tick(0), 1.0, 64, 64).unwrap();
        for y in 0..56 {
            assert!((a[(y + 5) * 64 + 3] - b[y * 64 + 3]).abs() < 1e-4, "row {y}");
        }
        assert!(a.iter().zip(&b).any(|(p, q)| (p - q).abs() > 0.05));
    }
}
