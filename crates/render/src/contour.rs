//! Soft matte → editable Bézier roto shapes (groundwork for track-assisted roto: "draw once, it
//! follows the object").
//!
//! Three stages, each a published method (no code from any implementation was used):
//!
//! 1. **Iso-contours** ([`contours`]): marching squares on the sample grid (the 2D analogue of
//!    Lorensen & Cline's marching cubes, 1987) with linear interpolation along cell edges, so a soft
//!    matte gives sub-pixel boundaries. Ambiguous saddle cells are resolved by the cell-centre mean
//!    (inside corners connected when the mean is at or above the level), the same rule in every
//!    cell, so neighbouring cells always agree. The matte is padded with an outside border so every
//!    contour closes (a matte touching the frame edge gives a contour along the edge).
//! 2. **Curve fit** ([`fit_path`]): two binomial smoothing passes remove a hard matte's
//!    staircase; corners are found by a turn-angle threshold with non-maximum suppression and moved
//!    onto the intersection of total-least-squares lines fitted to the runs either side (restoring
//!    the corner marching squares cuts); each run between corners is thinned with Ramer–Douglas–Peucker (Ramer 1972,
//!    Douglas & Peucker 1973) and fitted with Schneider's least-squares cubic Bézier fit with Newton
//!    reparameterisation ("An Algorithm for Automatically Fitting Digitized Curves", Graphics Gems,
//!    1990), splitting at the worst point until every sample is within the tolerance.
//! 3. **Stable vertex count** ([`fit_path_with_count`], [`matte_to_shape`]): for keyframes, exactly
//!    `n` vertices placed by arc length from the contour point nearest the previous keyframe's first
//!    vertex, in the previous keyframe's orientation, snapping vertices onto nearby corners, so
//!    consecutive keyframes correspond vertex to vertex and interpolation does not twist.
//!
//! Coordinates are matte pixels in the [`MaskPath`] convention (clip pixel space): pixel `(x, y)`
//! covers `[x, x+1) × [y, y+1)` and its sample sits at its centre `(x + 0.5, y + 0.5)`, matching the
//! mask rasterizer. Tangents are relative to their vertex. A matte at another resolution than the
//! clip is scaled by the caller ([`MaskPath::transformed`]).
//!
//! Orientation: outer boundaries have positive shoelace area in these y-down coordinates
//! (clockwise on screen), holes negative ([`Contour::is_hole`]).

use std::collections::HashMap;

use filmcraft_geom::Vec2;
use filmcraft_project::{MaskPath, MaskVertex};

/// The default iso level (a matte's 50 % coverage).
pub const DEFAULT_LEVEL: f32 = 0.5;
/// Contours enclosing less than this many square pixels are dropped by [`contours`].
pub const DEFAULT_MIN_AREA: f64 = 2.0;
/// At most this many contours are returned (the largest).
pub const MAX_CONTOURS: usize = 1024;
/// Edge crossings a matte may produce; more (noise) gives no contours rather than a huge allocation.
pub const MAX_CROSSINGS: usize = 1 << 21;
/// Points a contour may have to be fitted.
pub const MAX_FIT_POINTS: usize = 1 << 18;
/// Vertices a fitted path may have.
pub const MAX_VERTICES: usize = 1024;
/// Vertices a closed fitted shape has at least (fewer is awkward to edit).
pub const MIN_CLOSED_VERTICES: usize = 4;
/// A contour point turning by more than this many degrees is a corner.
pub const CORNER_ANGLE_DEG: f64 = 60.0;

/// Steps a neighbour walk may take (bounds work on pathologically dense contours).
const REACH_STEPS: usize = 256;
/// Schneider recursion depth.
const MAX_DEPTH: u32 = 24;
/// Newton reparameterisation passes per fit.
const NEWTON_PASSES: usize = 4;
/// Smoothing passes before fitting.
const SMOOTH_PASSES: usize = 2;

/// One closed iso-contour: sub-pixel points (closed implicitly, last → first) and signed area.
#[derive(Clone, Debug, PartialEq)]
pub struct Contour {
    pub points: Vec<[f64; 2]>,
    /// Shoelace area in square pixels: positive for an outer boundary, negative for a hole.
    pub area: f64,
}

impl Contour {
    pub fn is_hole(&self) -> bool {
        self.area < 0.0
    }
}

/// Closed sub-pixel iso-contours of `matte` (`w × h`, row-major) at `level`, largest first, with
/// contours under [`DEFAULT_MIN_AREA`] dropped. Values ≥ `level` are inside; non-finite values are
/// outside. A buffer shorter than `w × h`, a zero size or a non-finite level give no contours.
pub fn contours(matte: &[f32], w: usize, h: usize, level: f32) -> Vec<Contour> {
    contours_with_min_area(matte, w, h, level, DEFAULT_MIN_AREA)
}

/// [`contours`] with an explicit minimum |area| (square pixels; non-finite → the default).
pub fn contours_with_min_area(matte: &[f32], w: usize, h: usize, level: f32, min_area: f64) -> Vec<Contour> {
    if !level.is_finite() || w == 0 || h == 0 {
        return Vec::new();
    }
    let Some(n) = w.checked_mul(h) else { return Vec::new() };
    if matte.len() < n {
        return Vec::new();
    }
    let (Some(gw), Some(gh)) = (w.checked_add(2), h.checked_add(2)) else { return Vec::new() };
    // crossing ids are `2 · (j · gw + i) + kind`: they must fit in a u64
    if (gw as u128).saturating_mul(gh as u128).saturating_mul(2) >= u64::MAX as u128 {
        return Vec::new();
    }
    let min_area = if min_area.is_finite() { min_area.max(0.0) } else { DEFAULT_MIN_AREA };
    let lv = level as f64;
    // the virtual border outside the frame: 0 (no coverage), or below the level when it is ≤ 0
    let pad = if lv > 0.0 { 0.0 } else { lv - 1.0 };
    let sample = |gi: usize, gj: usize| -> f64 {
        if gi == 0 || gj == 0 || gi > w || gj > h {
            return pad;
        }
        let v = matte.get((gj - 1) * w + (gi - 1)).copied().unwrap_or(f32::NAN) as f64;
        if v.is_finite() { v } else { pad }
    };
    let gw64 = gw as u64;
    let hid = |i: usize, j: usize| (j as u64 * gw64 + i as u64) << 1;
    let vid = |i: usize, j: usize| hid(i, j) | 1;

    // next[x] = the crossing that follows crossing x along its contour
    let mut next: HashMap<u64, u64> = HashMap::new();
    let mut starts: Vec<u64> = Vec::new();
    for cj in 0..gh - 1 {
        for ci in 0..gw - 1 {
            // corners in walk order TL, TR, BR, BL
            let s = [sample(ci, cj), sample(ci + 1, cj), sample(ci + 1, cj + 1), sample(ci, cj + 1)];
            let ins = s.map(|x| x >= lv);
            if ins.iter().all(|&b| b) || ins.iter().all(|&b| !b) {
                continue;
            }
            let edges = [hid(ci, cj), vid(ci + 1, cj), hid(ci, cj + 1), vid(ci, cj)];
            // edge k runs from corner k to corner k+1: +1 exit (in → out), −1 entry (out → in)
            let kind = |k: usize| -> i8 {
                match (ins[k % 4], ins[(k + 1) % 4]) {
                    (true, false) => 1,
                    (false, true) => -1,
                    _ => 0,
                }
            };
            let saddle = ins[0] == ins[2] && ins[1] == ins[3];
            let centre_in = (s[0] + s[1] + s[2] + s[3]) / 4.0 >= lv;
            for k in 0..4 {
                if kind(k) != 1 {
                    continue;
                }
                // each exit joins the next entry around the cell (the inside piece stays on the
                // walk's side), or in a disconnected saddle the entry just before it
                let e = if saddle && !centre_in { (k + 3) % 4 } else { (1..4).map(|d| (k + d) % 4).find(|&m| kind(m) == -1).unwrap_or(k) };
                if next.len() >= MAX_CROSSINGS {
                    return Vec::new();
                }
                next.insert(edges[k % 4], edges[e % 4]);
                starts.push(edges[k % 4]);
            }
        }
    }

    let point = |id: u64| -> [f64; 2] {
        let cell = id >> 1;
        let (gi, gj) = ((cell % gw64) as usize, (cell / gw64) as usize);
        let (i2, j2) = if id & 1 == 0 { (gi + 1, gj) } else { (gi, gj + 1) };
        let (a, b) = (sample(gi, gj), sample(i2, j2));
        let t = if b != a { ((lv - a) / (b - a)).clamp(0.0, 1.0) } else { 0.5 };
        let t = if t.is_finite() { t } else { 0.5 };
        let (x0, y0) = (gi as f64 - 0.5, gj as f64 - 0.5);
        let (x1, y1) = (i2 as f64 - 0.5, j2 as f64 - 0.5);
        [x0 + t * (x1 - x0), y0 + t * (y1 - y0)]
    };

    let mut out = Vec::new();
    for &st in &starts {
        if !next.contains_key(&st) {
            continue;
        }
        let mut pts = Vec::new();
        let mut cur = st;
        let mut closed = false;
        for _ in 0..=MAX_CROSSINGS {
            let Some(nx) = next.remove(&cur) else { break };
            pts.push(point(cur));
            if nx == st {
                closed = true;
                break;
            }
            cur = nx;
        }
        if !closed || pts.len() < 3 {
            continue;
        }
        let area = shoelace(&pts);
        if area.abs() < min_area || !area.is_finite() {
            continue;
        }
        out.push(Contour { points: pts, area });
    }
    out.sort_by(|a, b| b.area.abs().total_cmp(&a.area.abs()));
    out.truncate(MAX_CONTOURS);
    out
}

/// Fit cubic Bézier segments to `contour` (pixels), every contour point within `tolerance_px` of the
/// curve (non-finite or ≤ 0 → 1 px). Sharp turns become corner vertices (zero tangents where both
/// neighbouring runs are straight, broken tangents otherwise). A closed path has at least
/// [`MIN_CLOSED_VERTICES`] vertices. An unusable contour (too few finite points, more than
/// [`MAX_FIT_POINTS`]) gives an empty path.
pub fn fit_path(contour: &[[f64; 2]], tolerance_px: f64, closed: bool) -> MaskPath {
    let empty = MaskPath { vertices: Vec::new(), closed };
    let tol = sanitize_tol(tolerance_px);
    let Some(p) = sanitize(contour, closed) else { return empty };
    let n = p.len();
    if n < 2 || (closed && n < 3) {
        return empty;
    }
    let mut p = smooth(&p, closed, SMOOTH_PASSES);
    let r = (2.0 * tol).clamp(3.0, 50.0);
    let corners = find_corners(&p, closed, r, CORNER_ANGLE_DEG.to_radians());
    refine_corners(&mut p, closed, &corners);
    // breakpoints: (index, is_corner)
    let mut bps: Vec<(usize, bool)> = Vec::new();
    if closed {
        match corners.as_slice() {
            // at least four breakpoints on a smooth or one-corner contour (quarters by index)
            [] => bps.extend((0..4).map(|q| (q * n / 4, false))),
            [c] => bps.extend((0..4).map(|q| ((*c + q * n / 4) % n, q == 0))),
            cs => bps.extend(cs.iter().map(|&c| (c, true))),
        }
        bps.dedup_by_key(|b| b.0);
    } else {
        bps.push((0, true));
        bps.extend(corners.iter().map(|&c| (c, true)));
        bps.push((n - 1, true));
    }
    let mut fit = Fitter { segs: Vec::new(), tol2: tol * tol };
    let pieces = if closed { bps.len() } else { bps.len() - 1 };
    let thin = (tol * 0.2).min(0.1);
    for k in 0..pieces {
        let (Some(&(a, ca)), Some(&(b, cb))) = (bps.get(k), bps.get((k + 1) % bps.len())) else { continue };
        let piece = cyclic_piece(&p, a, b);
        if piece.len() < 2 {
            continue;
        }
        let (p0, p3) = (at(&piece, 0), at(&piece, piece.len() - 1));
        if ca && cb && max_chord_dist(&piece) <= tol {
            fit.push([p0, p0, p3, p3]);
            continue;
        }
        let dd = r.min(arc_len(&piece) / 3.0);
        let t1 = if ca { unit(reach(&piece, false, 0, dd, true) - p0) } else { centred_tangent(&p, closed, a, dd) };
        let t2 = if cb { unit(reach(&piece, false, piece.len() - 1, dd, false) - p3) } else { centred_tangent(&p, closed, b, dd) * -1.0 };
        let thinned = rdp(&piece, thin);
        fit.fit_cubic(&thinned, t1, t2, 0);
    }
    if closed {
        split_to_min(&mut fit.segs, MIN_CLOSED_VERTICES);
    }
    assemble(&fit.segs, closed)
}

/// Fit a closed path of exactly `n` vertices (clamped to 3..=[`MAX_VERTICES`]) to `contour`, placed
/// by equal arc length starting at the contour point nearest `start_hint` (else the topmost-left
/// point) and following the contour's own order; vertices near a corner snap onto it. Use the
/// previous keyframe's first vertex as the hint (and the same orientation, see [`matte_to_shape`])
/// so keyframes correspond vertex to vertex. Unusable input gives an empty path.
pub fn fit_path_with_count(contour: &[[f64; 2]], n: usize, start_hint: Option<[f64; 2]>) -> MaskPath {
    let Some(p) = sanitize(contour, true) else { return MaskPath { vertices: Vec::new(), closed: true } };
    fit_count(&p, n, start_hint.map(v)).map(|(path, _)| path).unwrap_or(MaskPath { vertices: Vec::new(), closed: true })
}

/// The largest outer contour of `matte` at level 0.5 as a roto shape within `tolerance` px. With
/// `prev` (the previous keyframe, ≥ 3 vertices) the shape keeps its vertex count, its start
/// vertex and its orientation, so the two keyframes interpolate without twisting; without it the
/// vertex count is the smallest arc-length placement that meets the tolerance (searched upwards
/// from the adaptive [`fit_path`] count, at least [`MIN_CLOSED_VERTICES`]). A `prev` with
/// non-finite values or more than [`MAX_VERTICES`] vertices is ignored. `None` when the matte has
/// no usable outer contour.
pub fn matte_to_shape(matte: &[f32], w: usize, h: usize, prev: Option<&MaskPath>, tolerance: f64) -> Option<MaskPath> {
    let tol = sanitize_tol(tolerance);
    let cs = contours(matte, w, h, DEFAULT_LEVEL);
    let outer = cs.iter().find(|c| !c.is_hole())?;
    let mut pts = sanitize(&outer.points, true)?;
    if pts.len() < 3 {
        return None;
    }
    // a hostile previous path (non-finite, or so many vertices that flattening it is huge) is ignored
    let finite = |q: Vec2| q.x.is_finite() && q.y.is_finite();
    let prev = prev.filter(|p| (3..=MAX_VERTICES).contains(&p.vertices.len()) && p.vertices.iter().all(|v| finite(v.p) && finite(v.t_in) && finite(v.t_out)));
    if let Some(prev) = prev {
        let prev_pts: Vec<[f64; 2]> = prev.flatten(0.25).iter().map(|q| [q.x, q.y]).collect();
        if (shoelace(&prev_pts) < 0.0) != (outer.area < 0.0) {
            pts.reverse();
        }
        // follow the object: shift the previous start by the motion of the shape's centroid
        let raw: Vec<[f64; 2]> = pts.iter().map(|q| [q.x, q.y]).collect();
        let shift = area_centroid(&raw).zip(area_centroid(&prev_pts)).map(|(a, b)| a - b).unwrap_or(Vec2::ZERO);
        let start = prev.vertices.first().map(|v| v.p + shift);
        return fit_count(&pts, prev.vertices.len(), start).map(|(path, _)| path);
    }
    let raw: Vec<[f64; 2]> = pts.iter().map(|q| [q.x, q.y]).collect();
    let mut count = fit_path(&raw, tol, true).len().clamp(4, MAX_VERTICES);
    let mut best: Option<(MaskPath, f64)> = None;
    for _ in 0..10 {
        let Some((path, err)) = fit_count(&pts, count, None) else { break };
        let done = err <= tol;
        if best.as_ref().is_none_or(|(_, e)| err < *e) || done {
            best = Some((path, err));
        }
        if done || count >= MAX_VERTICES {
            break;
        }
        count = (count + count / 4 + 1).min(MAX_VERTICES);
    }
    best.map(|(path, _)| path)
}

// ---------------------------------------------------------------------------------------------
// helpers

fn v(p: [f64; 2]) -> Vec2 {
    Vec2::new(p[0], p[1])
}
fn dot(a: Vec2, b: Vec2) -> f64 {
    a.x * b.x + a.y * b.y
}
fn unit(a: Vec2) -> Vec2 {
    let l = a.length();
    if l > 1e-12 && l.is_finite() { a * (1.0 / l) } else { Vec2::ZERO }
}
fn at(p: &[Vec2], i: usize) -> Vec2 {
    p.get(i).copied().unwrap_or(Vec2::ZERO)
}
fn sanitize_tol(t: f64) -> f64 {
    if t.is_finite() && t > 0.0 { t.clamp(0.05, 100.0) } else { 1.0 }
}

/// Shoelace area (closed implicitly).
fn shoelace(p: &[[f64; 2]]) -> f64 {
    let n = p.len();
    let mut s = 0.0;
    for (i, a) in p.iter().enumerate() {
        let Some(b) = p.get((i + 1) % n.max(1)) else { continue };
        s += a[0] * b[1] - b[0] * a[1];
    }
    s * 0.5
}

/// Finite points with consecutive duplicates (and, closed, a repeated first point) removed; `None`
/// when there are too many.
fn sanitize(contour: &[[f64; 2]], closed: bool) -> Option<Vec<Vec2>> {
    if contour.len() > MAX_FIT_POINTS {
        return None;
    }
    let mut out: Vec<Vec2> = Vec::with_capacity(contour.len());
    for q in contour.iter().map(|p| v(*p)) {
        if !(q.x.is_finite() && q.y.is_finite()) || q.x.abs() > 1e9 || q.y.abs() > 1e9 {
            continue;
        }
        if out.last().is_some_and(|l| (q - *l).length() < 1e-9) {
            continue;
        }
        out.push(q);
    }
    if closed {
        while out.len() > 1 && out.first().zip(out.last()).is_some_and(|(a, b)| (*a - *b).length() < 1e-9) {
            out.pop();
        }
    }
    Some(out)
}

/// Points `a..=b` of `p`, wrapping past the end (a == b → just that point).
fn cyclic_piece(p: &[Vec2], a: usize, b: usize) -> Vec<Vec2> {
    let n = p.len();
    if n == 0 || a >= n || b >= n {
        return Vec::new();
    }
    let mut out = vec![at(p, a)];
    let mut i = a;
    for _ in 0..n {
        if i == b {
            break;
        }
        i = (i + 1) % n;
        out.push(at(p, i));
    }
    out
}

fn arc_len(p: &[Vec2]) -> f64 {
    p.windows(2).map(|w| (at(w, 1) - at(w, 0)).length()).sum()
}

fn seg_dist(q: Vec2, a: Vec2, b: Vec2) -> f64 {
    let ab = b - a;
    let l2 = dot(ab, ab);
    if l2 <= 1e-24 {
        return (q - a).length();
    }
    let t = (dot(q - a, ab) / l2).clamp(0.0, 1.0);
    (q - (a + ab * t)).length()
}

fn max_chord_dist(p: &[Vec2]) -> f64 {
    let (a, b) = (at(p, 0), at(p, p.len().saturating_sub(1)));
    p.iter().map(|q| seg_dist(*q, a, b)).fold(0.0, f64::max)
}

/// The point reached by walking `r` (arc length) from `i`, forward or back (bounded).
fn reach(p: &[Vec2], closed: bool, i: usize, r: f64, fwd: bool) -> Vec2 {
    let n = p.len();
    let mut prev = at(p, i);
    let mut j = i;
    let mut acc = 0.0;
    for _ in 0..n.min(REACH_STEPS) {
        let nj = if fwd {
            if j + 1 < n {
                j + 1
            } else if closed {
                0
            } else {
                break;
            }
        } else if j > 0 {
            j - 1
        } else if closed {
            n - 1
        } else {
            break;
        };
        if nj == i {
            break;
        }
        let q = at(p, nj);
        acc += (q - prev).length();
        prev = q;
        j = nj;
        if acc >= r {
            break;
        }
    }
    prev
}

fn centred_tangent(p: &[Vec2], closed: bool, i: usize, d: f64) -> Vec2 {
    let t = unit(reach(p, closed, i, d, true) - reach(p, closed, i, d, false));
    if t == Vec2::ZERO { unit(reach(p, closed, i, d, true) - at(p, i)) } else { t }
}

/// Turn angle (radians) at each point, measured to the points `r` away along the contour.
fn turn_angles(p: &[Vec2], closed: bool, r: f64) -> Vec<f64> {
    let n = p.len();
    (0..n)
        .map(|i| {
            if !closed && (i == 0 || i + 1 == n) {
                return 0.0;
            }
            let c = at(p, i);
            let a = unit(c - reach(p, closed, i, r, false));
            let b = unit(reach(p, closed, i, r, true) - c);
            if a == Vec2::ZERO || b == Vec2::ZERO { 0.0 } else { dot(a, b).clamp(-1.0, 1.0).acos() }
        })
        .collect()
}

/// Indices (ascending) of points turning by more than `thresh`, keeping the sharpest within `r`.
fn find_corners(p: &[Vec2], closed: bool, r: f64, thresh: f64) -> Vec<usize> {
    let n = p.len();
    let turns = turn_angles(p, closed, r);
    let mut cand: Vec<usize> = (0..n).filter(|&i| turns.get(i).is_some_and(|&t| t > thresh)).collect();
    cand.sort_by(|&a, &b| turns.get(b).unwrap_or(&0.0).total_cmp(turns.get(a).unwrap_or(&0.0)));
    let mut suppressed = vec![false; n];
    let mut out = Vec::new();
    for c in cand {
        if suppressed.get(c).copied().unwrap_or(true) {
            continue;
        }
        out.push(c);
        if out.len() >= MAX_VERTICES {
            break;
        }
        for fwd in [true, false] {
            let mut j = c;
            let mut acc = 0.0;
            for _ in 0..n.min(REACH_STEPS) {
                let nj = if fwd {
                    if j + 1 < n {
                        j + 1
                    } else if closed {
                        0
                    } else {
                        break;
                    }
                } else if j > 0 {
                    j - 1
                } else if closed {
                    n - 1
                } else {
                    break;
                };
                acc += (at(p, nj) - at(p, j)).length();
                if acc > r || nj == c {
                    break;
                }
                if let Some(s) = suppressed.get_mut(nj) {
                    *s = true;
                }
                j = nj;
            }
        }
        if let Some(s) = suppressed.get_mut(c) {
            *s = true;
        }
    }
    out.sort_unstable();
    out
}

/// Binomial `[1, 2, 1] / 4` smoothing passes (open ends fixed): removes the staircase of a hard
/// (binary) matte before fitting; on a ~1 px point spacing a curve of radius R shrinks by about
/// `passes / (4R)` px, negligible, and corners are restored by [`refine_corners`].
fn smooth(p: &[Vec2], closed: bool, passes: usize) -> Vec<Vec2> {
    let n = p.len();
    let mut cur = p.to_vec();
    if n < 3 {
        return cur;
    }
    for _ in 0..passes {
        cur = (0..n)
            .map(|i| {
                let c = at(&cur, i);
                if !closed && (i == 0 || i + 1 == n) {
                    return c;
                }
                let a = at(&cur, (i + n - 1) % n);
                let b = at(&cur, (i + 1) % n);
                (a + c * 2.0 + b) * 0.25
            })
            .collect();
    }
    cur
}

/// Points at arc distance `[lo, hi]` from `i` (one direction, bounded walk).
fn arc_window(p: &[Vec2], closed: bool, i: usize, lo: f64, hi: f64, fwd: bool) -> Vec<Vec2> {
    let n = p.len();
    let mut out = Vec::new();
    let mut j = i;
    let mut acc = 0.0;
    for _ in 0..n.min(REACH_STEPS) {
        let nj = if fwd {
            if j + 1 < n {
                j + 1
            } else if closed {
                0
            } else {
                break;
            }
        } else if j > 0 {
            j - 1
        } else if closed {
            n - 1
        } else {
            break;
        };
        if nj == i {
            break;
        }
        acc += (at(p, nj) - at(p, j)).length();
        j = nj;
        if acc > hi {
            break;
        }
        if acc >= lo {
            out.push(at(p, nj));
        }
    }
    out
}

/// Total-least-squares line through `q`: (mean, unit direction).
fn fit_line(q: &[Vec2]) -> Option<(Vec2, Vec2)> {
    if q.len() < 2 {
        return None;
    }
    let k = 1.0 / q.len() as f64;
    let mean = q.iter().fold(Vec2::ZERO, |a, b| a + *b) * k;
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for d in q.iter().map(|x| *x - mean) {
        sxx += d.x * d.x;
        sxy += d.x * d.y;
        syy += d.y * d.y;
    }
    let ang = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let dir = Vec2::new(ang.cos(), ang.sin());
    (ang.is_finite() && sxx + syy > 1e-12).then_some((mean, dir))
}

/// Move each corner onto the intersection of straight-line fits to the runs either side of it
/// (marching squares and smoothing cut a sharp corner; this restores it), when that intersection
/// is close to the detected point and the runs are not near-parallel.
fn refine_corners(p: &mut [Vec2], closed: bool, corners: &[usize]) {
    let snapshot = p.to_vec();
    for &c in corners {
        let back = arc_window(&snapshot, closed, c, 2.5, 8.0, false);
        let fwd = arc_window(&snapshot, closed, c, 2.5, 8.0, true);
        let (Some((m1, d1)), Some((m2, d2))) = (fit_line(&back), fit_line(&fwd)) else { continue };
        let cross = d1.x * d2.y - d1.y * d2.x;
        if cross.abs() < 0.35 {
            continue;
        }
        let w = m2 - m1;
        let s = (w.x * d2.y - w.y * d2.x) / cross;
        let q = m1 + d1 * s;
        let orig = at(&snapshot, c);
        if q.x.is_finite()
            && q.y.is_finite()
            && (q - orig).length() < 3.0
            && let Some(x) = p.get_mut(c)
        {
            *x = q;
        }
    }
}

/// Area centroid of a closed polygon (`None` when its area is ~0).
fn area_centroid(p: &[[f64; 2]]) -> Option<Vec2> {
    let n = p.len();
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for (i, u) in p.iter().enumerate() {
        let Some(w) = p.get((i + 1) % n.max(1)) else { continue };
        let k = u[0] * w[1] - w[0] * u[1];
        a += k;
        cx += (u[0] + w[0]) * k;
        cy += (u[1] + w[1]) * k;
    }
    (a.abs() > 1e-9 && a.is_finite()).then(|| Vec2::new(cx / (3.0 * a), cy / (3.0 * a)))
}

/// Ramer–Douglas–Peucker simplification (iterative; keeps both ends).
fn rdp(p: &[Vec2], eps: f64) -> Vec<Vec2> {
    let n = p.len();
    if n < 3 {
        return p.to_vec();
    }
    let mut keep = vec![false; n];
    if let Some(k) = keep.first_mut() {
        *k = true;
    }
    if let Some(k) = keep.last_mut() {
        *k = true;
    }
    let mut stack = vec![(0usize, n - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (pa, pb) = (at(p, a), at(p, b));
        let mut best = (0.0, a);
        for i in a + 1..b {
            let d = seg_dist(at(p, i), pa, pb);
            if d > best.0 {
                best = (d, i);
            }
        }
        if best.0 > eps {
            if let Some(k) = keep.get_mut(best.1) {
                *k = true;
            }
            stack.push((a, best.1));
            stack.push((best.1, b));
        }
    }
    p.iter().zip(keep).filter_map(|(q, k)| k.then_some(*q)).collect()
}

// --- Bézier evaluation and Schneider's fit -----------------------------------------------------

type Bez = [Vec2; 4];

fn bez(b: &Bez, t: f64) -> Vec2 {
    let u = 1.0 - t;
    b[0] * (u * u * u) + b[1] * (3.0 * u * u * t) + b[2] * (3.0 * u * t * t) + b[3] * (t * t * t)
}
fn bez_d1(b: &Bez, t: f64) -> Vec2 {
    let u = 1.0 - t;
    (b[1] - b[0]) * (3.0 * u * u) + (b[2] - b[1]) * (6.0 * u * t) + (b[3] - b[2]) * (3.0 * t * t)
}
fn bez_d2(b: &Bez, t: f64) -> Vec2 {
    let u = 1.0 - t;
    (b[2] - b[1] * 2.0 + b[0]) * (6.0 * u) + (b[3] - b[2] * 2.0 + b[1]) * (6.0 * t)
}

/// Chord-length parameters in [0, 1].
fn chord_params(d: &[Vec2]) -> Vec<f64> {
    let mut u = Vec::with_capacity(d.len());
    let mut acc = 0.0;
    u.push(0.0);
    for w in d.windows(2) {
        acc += (at(w, 1) - at(w, 0)).length();
        u.push(acc);
    }
    if acc > 0.0 {
        u.iter_mut().for_each(|x| *x /= acc);
    }
    u
}

/// Least-squares handle lengths along the fixed tangent directions `t1` (start, forward) and `t2`
/// (end, backward) (Schneider's GenerateBezier); the Wu–Barsky chord/3 heuristic when degenerate.
fn generate(d: &[Vec2], u: &[f64], t1: Vec2, t2: Vec2) -> Bez {
    let (p0, p3) = (at(d, 0), at(d, d.len().saturating_sub(1)));
    let seg = (p3 - p0).length();
    let (mut c00, mut c01, mut c11, mut x0, mut x1) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (p, &t) in d.iter().zip(u) {
        let s = 1.0 - t;
        let (b0, b1, b2, b3) = (s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t);
        let a0 = t1 * b1;
        let a1 = t2 * b2;
        c00 += dot(a0, a0);
        c01 += dot(a0, a1);
        c11 += dot(a1, a1);
        let tmp = *p - (p0 * (b0 + b1) + p3 * (b2 + b3));
        x0 += dot(a0, tmp);
        x1 += dot(a1, tmp);
    }
    let det = c00 * c11 - c01 * c01;
    let (mut al, mut ar) = if det.abs() > 1e-12 { ((x0 * c11 - x1 * c01) / det, (c00 * x1 - c01 * x0) / det) } else { (0.0, 0.0) };
    let eps = 1e-6 * seg;
    let limit = seg.max(1e-9) * 3.0;
    if !(al.is_finite() && ar.is_finite()) || al < eps || ar < eps || al > limit || ar > limit || t1 == Vec2::ZERO || t2 == Vec2::ZERO {
        al = seg / 3.0;
        ar = seg / 3.0;
    }
    [p0, p0 + t1 * al, p3 + t2 * ar, p3]
}

/// Largest squared distance from a sample to the curve at its parameter, and where.
fn max_err(d: &[Vec2], b: &Bez, u: &[f64]) -> (f64, usize) {
    let mut best = (0.0, d.len() / 2);
    for (i, (p, &t)) in d.iter().zip(u).enumerate() {
        let q = bez(b, t) - *p;
        let e = dot(q, q);
        if e > best.0 {
            best = (e, i);
        }
    }
    best
}

/// One Newton–Raphson step per sample towards the closest curve point.
fn reparam(d: &[Vec2], u: &[f64], b: &Bez) -> Vec<f64> {
    d.iter()
        .zip(u)
        .map(|(p, &t)| {
            let q = bez(b, t) - *p;
            let q1 = bez_d1(b, t);
            let q2 = bez_d2(b, t);
            let num = dot(q, q1);
            let den = dot(q1, q1) + dot(q, q2);
            if den.abs() < 1e-12 {
                return t;
            }
            let nt = t - num / den;
            if nt.is_finite() { nt.clamp(0.0, 1.0) } else { t }
        })
        .collect()
}

/// Fit with fixed end tangents, polished by Newton reparameterisation: (curve, max squared error).
fn fit_single(d: &[Vec2], t1: Vec2, t2: Vec2) -> (Bez, f64, usize) {
    let mut u = chord_params(d);
    let mut b = generate(d, &u, t1, t2);
    let (mut err, mut split) = max_err(d, &b, &u);
    for _ in 0..NEWTON_PASSES {
        let nu = reparam(d, &u, &b);
        let nb = generate(d, &nu, t1, t2);
        let (ne, ns) = max_err(d, &nb, &nu);
        if ne < err {
            (u, b, err, split) = (nu, nb, ne, ns);
        } else {
            break;
        }
    }
    (b, err, split)
}

struct Fitter {
    segs: Vec<Bez>,
    tol2: f64,
}

impl Fitter {
    fn push(&mut self, b: Bez) {
        if self.segs.len() < MAX_VERTICES {
            self.segs.push(b);
        }
    }

    /// Schneider's FitCubic: fit, else split at the worst sample and recurse.
    fn fit_cubic(&mut self, d: &[Vec2], t1: Vec2, t2: Vec2, depth: u32) {
        let n = d.len();
        if n < 2 {
            return;
        }
        let (p0, p3) = (at(d, 0), at(d, n - 1));
        if n == 2 {
            let l = (p3 - p0).length() / 3.0;
            self.push([p0, p0 + t1 * l, p3 + t2 * l, p3]);
            return;
        }
        let (b, err, split) = fit_single(d, t1, t2);
        if err <= self.tol2 || depth >= MAX_DEPTH || self.segs.len() + 2 > MAX_VERTICES {
            self.push(b);
            return;
        }
        let split = split.clamp(1, n - 2);
        let mut tc = unit(at(d, split - 1) - at(d, split + 1));
        if tc == Vec2::ZERO {
            tc = unit(at(d, split - 1) - at(d, split));
        }
        let (Some(left), Some(right)) = (d.get(..=split), d.get(split..)) else {
            self.push(b);
            return;
        };
        self.fit_cubic(left, t1, tc, depth + 1);
        self.fit_cubic(right, tc * -1.0, t2, depth + 1);
    }
}

/// Split the longest segment (control-polygon length) in half (de Casteljau, shape unchanged)
/// until there are at least `min` segments; degenerate (single-point) segments stay as they are.
fn split_to_min(segs: &mut Vec<Bez>, min: usize) {
    let poly = |b: &Bez| (b[1] - b[0]).length() + (b[2] - b[1]).length() + (b[3] - b[2]).length();
    while segs.len() < min {
        let Some((i, l)) = segs.iter().map(poly).enumerate().max_by(|a, b| a.1.total_cmp(&b.1)) else { return };
        let Some(b) = segs.get(i).copied() else { return };
        if !(l > 1e-9 && l.is_finite()) {
            return;
        }
        let m = |a: Vec2, c: Vec2| (a + c) * 0.5;
        let (p01, p12, p23) = (m(b[0], b[1]), m(b[1], b[2]), m(b[2], b[3]));
        let (p012, p123) = (m(p01, p12), m(p12, p23));
        let mid = m(p012, p123);
        if let Some(x) = segs.get_mut(i) {
            *x = [b[0], p01, p012, mid];
        }
        segs.insert(i + 1, [mid, p123, p23, b[3]]);
    }
}

fn tidy(t: Vec2) -> Vec2 {
    if t.length() < 1e-9 { Vec2::ZERO } else { t }
}

/// Bézier segments (in order, each starting where the previous ends) → mask vertices.
fn assemble(segs: &[Bez], closed: bool) -> MaskPath {
    let m = segs.len();
    let mut verts = Vec::with_capacity(m + 1);
    for (k, s) in segs.iter().enumerate() {
        let prev = if k > 0 {
            segs.get(k - 1)
        } else if closed {
            segs.last()
        } else {
            None
        };
        let t_in = prev.map(|q| tidy(q[2] - q[3])).unwrap_or(Vec2::ZERO);
        verts.push(MaskVertex { p: s[0], t_in, t_out: tidy(s[1] - s[0]), ..MaskVertex::default() });
    }
    if !closed && let Some(s) = segs.last() {
        verts.push(MaskVertex { p: s[3], t_in: tidy(s[2] - s[3]), t_out: Vec2::ZERO, ..MaskVertex::default() });
    }
    MaskPath { vertices: verts, closed }
}

// --- fixed vertex count ------------------------------------------------------------------------

/// Point on the closed polyline at arc length `s` (wrapped into [0, L)); `cum` has `len + 1`
/// entries, the last being the total length.
fn point_at(p: &[Vec2], cum: &[f64], s: f64) -> Vec2 {
    let total = cum.last().copied().unwrap_or(0.0);
    if total <= 0.0 {
        return at(p, 0);
    }
    let s = s.rem_euclid(total);
    let j = cum.partition_point(|&c| c <= s).saturating_sub(1).min(p.len().saturating_sub(1));
    let (a, b) = (at(p, j), at(p, (j + 1) % p.len().max(1)));
    let (c0, c1) = (cum.get(j).copied().unwrap_or(0.0), cum.get(j + 1).copied().unwrap_or(total));
    let t = if c1 > c0 { ((s - c0) / (c1 - c0)).clamp(0.0, 1.0) } else { 0.0 };
    a.lerp(b, t)
}

/// `n` vertices by arc length from the point nearest `start` (closed contour `pts`): the path and
/// its max error (px).
fn fit_count(pts: &[Vec2], n: usize, start: Option<Vec2>) -> Option<(MaskPath, f64)> {
    let mut pts = smooth(pts, true, SMOOTH_PASSES);
    let m = pts.len();
    if m < 3 {
        return None;
    }
    let n = n.clamp(3, MAX_VERTICES);
    let corner_angle = CORNER_ANGLE_DEG.to_radians();
    let cs0 = find_corners(&pts, true, 3.0, corner_angle);
    refine_corners(&mut pts, true, &cs0);
    let pts = &pts;
    // rotate so the start point comes first: the point nearest the hint (a corner when one is
    // within 3 px, so a keyframe started on a corner stays on it), else the top-left corner, else
    // the topmost-left point
    let start = start.filter(|s| s.x.is_finite() && s.y.is_finite());
    let rotate = |i: usize| -> Vec<Vec2> { (0..m).map(|k| at(pts, (i + k) % m)).collect() };
    let p: Vec<Vec2> = match start {
        Some(s) => {
            let mut best = (f64::INFINITY, 0usize, at(pts, 0));
            for i in 0..m {
                let (a, b) = (at(pts, i), at(pts, (i + 1) % m));
                let ab = b - a;
                let l2 = dot(ab, ab);
                let t = if l2 > 1e-24 { (dot(s - a, ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
                let q = a + ab * t;
                let d = (q - s).length();
                if d < best.0 {
                    best = (d, i, q);
                }
            }
            let (_, i, q) = best;
            let near_corner = cs0.iter().map(|&c| (c, (at(pts, c) - q).length())).filter(|&(_, d)| d <= 3.0).min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((c, _)) = near_corner {
                rotate(c)
            } else {
                let mut out = Vec::with_capacity(m + 1);
                out.push(q);
                for k in 1..=m {
                    let r = at(pts, (i + k) % m);
                    if (r - q).length() > 1e-9 && out.last().is_none_or(|l| (r - *l).length() > 1e-9) {
                        out.push(r);
                    }
                }
                out
            }
        }
        None => {
            let top_left = |a: &usize, b: &usize| {
                let (pa, pb) = (at(pts, *a), at(pts, *b));
                pa.y.total_cmp(&pb.y).then(pa.x.total_cmp(&pb.x))
            };
            let i = cs0.iter().copied().min_by(top_left).or_else(|| (0..m).min_by(top_left))?;
            rotate(i)
        }
    };
    let m = p.len();
    if m < 3 {
        return None;
    }
    let cs = find_corners(&p, true, 3.0, corner_angle);
    let mut cum = Vec::with_capacity(m + 1);
    let mut acc = 0.0;
    cum.push(0.0);
    for i in 0..m {
        acc += (at(&p, (i + 1) % m) - at(&p, i)).length();
        cum.push(acc);
    }
    let total = acc;
    if !(total > 1e-6 && total.is_finite()) {
        return None;
    }
    let h = total / n as f64;
    let mut sv: Vec<f64> = (0..n).map(|k| k as f64 * h).collect();
    let mut corner = vec![false; n];
    let r = 3.0_f64.min(0.45 * h).max(1e-3);
    for &c in &cs {
        let sc = cum.get(c).copied().unwrap_or(0.0);
        if sc.min(total - sc) <= r {
            if let Some(f) = corner.first_mut() {
                *f = true;
            }
            continue;
        }
        let k = ((sc / h).round() as usize).min(n);
        if k == 0 || k >= n || corner.get(k).copied().unwrap_or(true) || (sc - k as f64 * h).abs() > 0.5 * h {
            continue;
        }
        let lo = sv.get(k - 1).copied().unwrap_or(0.0);
        let hi = sv.get(k + 1).copied().unwrap_or(total);
        if sc > lo + 1e-6 && sc < hi - 1e-6 {
            if let Some(x) = sv.get_mut(k) {
                *x = sc;
            }
            if let Some(x) = corner.get_mut(k) {
                *x = true;
            }
        }
    }
    let verts: Vec<Vec2> = sv.iter().map(|&s| point_at(&p, &cum, s)).collect();
    // tangent directions: out (forward) and in (backward)
    let dirs: Vec<(Vec2, Vec2)> = sv
        .iter()
        .zip(&verts)
        .zip(&corner)
        .map(|((&s, &vp), &c)| {
            let fwd = point_at(&p, &cum, s + r);
            let back = point_at(&p, &cum, s - r);
            if c {
                (unit(fwd - vp), unit(back - vp))
            } else {
                let t = unit(fwd - back);
                (t, t * -1.0)
            }
        })
        .collect();
    let mut segs: Vec<Bez> = Vec::with_capacity(n);
    let mut worst: f64 = 0.0;
    let mut j = 1usize; // dense index cursor (p[0] is vertex 0)
    for k in 0..n {
        let a = sv.get(k).copied().unwrap_or(0.0);
        let b = sv.get(k + 1).copied().unwrap_or(total);
        let mut d = vec![at(&verts, k)];
        while j < m {
            let s = cum.get(j).copied().unwrap_or(total);
            if s >= b - 1e-9 {
                break;
            }
            if s > a + 1e-9 {
                d.push(at(&p, j));
            }
            j += 1;
        }
        d.push(at(&verts, (k + 1) % n));
        let t1 = dirs.get(k).map(|x| x.0).unwrap_or(Vec2::ZERO);
        let t2 = dirs.get((k + 1) % n).map(|x| x.1).unwrap_or(Vec2::ZERO);
        let (bz, err, _) = if d.len() == 2 {
            let (p0, p3) = (at(&d, 0), at(&d, 1));
            let l = (p3 - p0).length() / 3.0;
            ([p0, p0 + t1 * l, p3 + t2 * l, p3], 0.0, 0)
        } else {
            fit_single(&d, t1, t2)
        };
        worst = worst.max(err);
        segs.push(bz);
    }
    Some((assemble(&segs, true), worst.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Coverage of `inside` per pixel, 8×8 supersampled (an antialiased matte).
    fn coverage(w: usize, h: usize, inside: impl Fn(f64, f64) -> bool) -> Vec<f32> {
        let s = 8;
        let mut m = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut c = 0;
                for j in 0..s {
                    for i in 0..s {
                        if inside(x as f64 + (i as f64 + 0.5) / s as f64, y as f64 + (j as f64 + 0.5) / s as f64) {
                            c += 1;
                        }
                    }
                }
                m[y * w + x] = c as f32 / (s * s) as f32;
            }
        }
        m
    }
    fn disc(w: usize, h: usize, cx: f64, cy: f64, r: f64) -> Vec<f32> {
        coverage(w, h, |x, y| (x - cx).hypot(y - cy) <= r)
    }
    fn flat_area(p: &MaskPath) -> f64 {
        let f: Vec<[f64; 2]> = p.flatten(0.02).iter().map(|q| [q.x, q.y]).collect();
        shoelace(&f)
    }
    fn radial_err(p: &MaskPath, cx: f64, cy: f64, r: f64) -> f64 {
        p.flatten(0.02).iter().map(|q| ((q.x - cx).hypot(q.y - cy) - r).abs()).fold(0.0, f64::max)
    }

    #[test]
    fn contour_disc_fit_within_half_pixel() {
        let (cx, cy, r) = (64.3, 60.7, 40.0);
        let m = disc(128, 128, cx, cy, r);
        let cs = contours(&m, 128, 128, 0.5);
        assert_eq!(cs.len(), 1);
        assert!(!cs[0].is_hole());
        let raw = cs[0].points.iter().map(|q| ((q[0] - cx).hypot(q[1] - cy) - r).abs()).fold(0.0, f64::max);
        let path = fit_path(&cs[0].points, 0.5, true);
        let err = radial_err(&path, cx, cy, r);
        let area = flat_area(&path);
        let rel = (area - PI * r * r).abs() / (PI * r * r);
        eprintln!(
            "disc: contour pts {} raw err {raw:.3} px, fit {} vertices, radial err {err:.3} px, area err {:.3} %",
            cs[0].points.len(),
            path.len(),
            rel * 100.0
        );
        assert!(err < 0.5, "{err}");
        assert!(rel < 0.01, "{rel}");
        // the stable-count shape meets the same bar
        let shape = matte_to_shape(&m, 128, 128, None, 0.5).unwrap();
        let e2 = radial_err(&shape, cx, cy, r);
        eprintln!("disc matte_to_shape: {} vertices, radial err {e2:.3} px", shape.len());
        assert!(e2 < 0.5, "{e2}");
        // binary (non-antialiased) disc: staircase input, still close
        let b: Vec<f32> = m.iter().map(|&x| if x >= 0.5 { 1.0 } else { 0.0 }).collect();
        let pb = fit_path(&contours(&b, 128, 128, 0.5)[0].points, 0.5, true);
        let eb = radial_err(&pb, cx, cy, r);
        eprintln!("binary disc: {} vertices, radial err {eb:.3} px", pb.len());
        assert!(eb < 0.9, "{eb}");
    }

    #[test]
    fn contour_square_four_corners() {
        let m = coverage(100, 100, |x, y| (20.0..80.0).contains(&x) && (30.0..90.0).contains(&y));
        let cs = contours(&m, 100, 100, 0.5);
        assert_eq!(cs.len(), 1);
        let path = fit_path(&cs[0].points, 0.5, true);
        eprintln!("square: {:?}", path.vertices.iter().map(|v| (v.p.x, v.p.y)).collect::<Vec<_>>());
        assert_eq!(path.len(), 4);
        for want in [(20.0, 30.0), (80.0, 30.0), (80.0, 90.0), (20.0, 90.0)] {
            let d = path.vertices.iter().map(|v| (v.p.x - want.0).hypot(v.p.y - want.1)).fold(f64::INFINITY, f64::min);
            eprintln!("square corner {want:?}: {d:.3} px");
            assert!(d < 1.0, "{want:?} {d}");
        }
        assert!(path.vertices.iter().all(|v| v.is_corner()));
        assert!((flat_area(&path) - 3600.0).abs() / 3600.0 < 0.01);
    }

    #[test]
    fn contour_l_shape() {
        let inside = |x: f64, y: f64| ((10.0..30.0).contains(&x) && (10.0..70.0).contains(&y)) || ((10.0..60.0).contains(&x) && (50.0..70.0).contains(&y));
        let m = coverage(80, 80, inside);
        let cs = contours(&m, 80, 80, 0.5);
        assert_eq!(cs.len(), 1);
        let path = fit_path(&cs[0].points, 0.5, true);
        eprintln!("L: {:?}", path.vertices.iter().map(|v| (v.p.x, v.p.y)).collect::<Vec<_>>());
        assert_eq!(path.len(), 6);
        for want in [(10.0, 10.0), (30.0, 10.0), (30.0, 50.0), (60.0, 50.0), (60.0, 70.0), (10.0, 70.0)] {
            let d = path.vertices.iter().map(|v| (v.p.x - want.0).hypot(v.p.y - want.1)).fold(f64::INFINITY, f64::min);
            assert!(d < 1.0, "{want:?} {d}");
        }
        let area = 20.0 * 60.0 + 30.0 * 20.0;
        let a = flat_area(&path);
        eprintln!("L area {a:.2} vs {area}");
        assert!((a - area).abs() / area < 0.01);
    }

    #[test]
    fn contour_feathered_disc() {
        let (cx, cy, r, f) = (50.0, 48.5, 30.0, 6.0);
        let mut m = vec![0.0f32; 100 * 100];
        for y in 0..100 {
            for x in 0..100 {
                let d = (x as f64 + 0.5 - cx).hypot(y as f64 + 0.5 - cy);
                let t = ((r + f - d) / (2.0 * f)).clamp(0.0, 1.0);
                m[y * 100 + x] = (t * t * (3.0 - 2.0 * t)) as f32;
            }
        }
        let cs = contours(&m, 100, 100, 0.5);
        assert_eq!(cs.len(), 1);
        let path = fit_path(&cs[0].points, 0.5, true);
        let err = radial_err(&path, cx, cy, r);
        eprintln!("feathered disc: {} vertices, radial err {err:.3} px", path.len());
        assert!(err < 0.5, "{err}");
        // a smooth closed shape still gets enough vertices to edit
        assert!(path.len() >= MIN_CLOSED_VERTICES, "{}", path.len());
        let s = matte_to_shape(&m, 100, 100, None, 0.5).unwrap();
        assert!(s.len() >= MIN_CLOSED_VERTICES, "{}", s.len());
        assert!(radial_err(&s, cx, cy, r) < 0.5);
        // a loose tolerance too
        let loose = fit_path(&cs[0].points, 100.0, true);
        assert!(loose.len() >= MIN_CLOSED_VERTICES, "{}", loose.len());
    }

    #[test]
    fn contour_closed_min_vertices() {
        // three corners (a triangle) → one edge split, shape unchanged
        let tri = coverage(100, 100, |x, y| (20.0..=80.0).contains(&y) && (x - 50.0).abs() <= (y - 20.0) * 0.6);
        let cs = contours(&tri, 100, 100, 0.5);
        let path = fit_path(&cs[0].points, 0.5, true);
        eprintln!("triangle: {} vertices, area {:.1}", path.len(), flat_area(&path));
        assert!(path.len() >= MIN_CLOSED_VERTICES);
        assert!((flat_area(&path) - shoelace(&cs[0].points)).abs() / shoelace(&cs[0].points) < 0.02);
        // de Casteljau splitting keeps the curve and the chain
        let b: Bez = [Vec2::new(0.0, 0.0), Vec2::new(10.0, 20.0), Vec2::new(30.0, 20.0), Vec2::new(40.0, 0.0)];
        let mut segs = vec![b];
        split_to_min(&mut segs, 4);
        assert_eq!(segs.len(), 4);
        for w in segs.windows(2) {
            assert!((w[0][3] - w[1][0]).length() < 1e-12);
        }
        let mid = bez(&b, 0.5);
        assert!((segs[1][3] - mid).length() < 1e-9 || (segs[2][0] - mid).length() < 1e-9 || (segs[0][3] - mid).length() < 1e-9);
        // degenerate segments are left alone (no endless loop)
        let mut dots = vec![[Vec2::ZERO; 4]];
        split_to_min(&mut dots, 4);
        assert_eq!(dots.len(), 1);
        split_to_min(&mut Vec::new(), 4);
    }

    #[test]
    fn contour_ring_reports_hole() {
        let m = coverage(120, 120, |x, y| {
            let d = (x - 60.0).hypot(y - 60.0);
            (20.0..=40.0).contains(&d)
        });
        let cs = contours(&m, 120, 120, 0.5);
        assert_eq!(cs.len(), 2);
        assert!(!cs[0].is_hole() && cs[1].is_hole());
        eprintln!("ring areas {:.1} {:.1}", cs[0].area, cs[1].area);
        assert!((cs[0].area - PI * 1600.0).abs() / (PI * 1600.0) < 0.01);
        assert!((cs[1].area + PI * 400.0).abs() / (PI * 400.0) < 0.01);
        // the shape is the outer boundary
        let s = matte_to_shape(&m, 120, 120, None, 0.5).unwrap();
        assert!(radial_err(&s, 60.0, 60.0, 40.0) < 0.5);
    }

    #[test]
    fn contour_two_blobs_sorted_by_area() {
        let m = coverage(160, 100, |x, y| (x - 40.0).hypot(y - 50.0) <= 15.0 || (x - 110.0).hypot(y - 50.0) <= 30.0);
        let cs = contours(&m, 160, 100, 0.5);
        assert_eq!(cs.len(), 2);
        assert!(cs[0].area > cs[1].area && cs[1].area > 0.0);
        let cx0 = cs[0].points.iter().map(|p| p[0]).sum::<f64>() / cs[0].points.len() as f64;
        assert!((cx0 - 110.0).abs() < 1.0, "{cx0}");
    }

    #[test]
    fn contour_count_fit_follows_translation() {
        let m0 = disc(160, 140, 60.0, 60.0, 35.0);
        let p0 = matte_to_shape(&m0, 160, 140, None, 0.5).unwrap();
        let (dx, dy) = (7.3, 4.6);
        let m1 = disc(160, 140, 60.0 + dx, 60.0 + dy, 35.0);
        let p1 = matte_to_shape(&m1, 160, 140, Some(&p0), 0.5).unwrap();
        assert_eq!(p0.len(), p1.len());
        let mut worst: f64 = 0.0;
        for (a, b) in p0.vertices.iter().zip(&p1.vertices) {
            worst = worst.max((b.p.x - a.p.x - dx).hypot(b.p.y - a.p.y - dy));
        }
        let e1 = radial_err(&p1, 60.0 + dx, 60.0 + dy, 35.0);
        eprintln!("translated disc: {} vertices, worst vertex correspondence {worst:.3} px, radial err {e1:.3} px", p1.len());
        assert!(worst < 1.5, "{worst}");
        assert!(e1 < 0.5);
        // reversed previous path: orientation follows it
        let rev = MaskPath { vertices: p0.vertices.iter().rev().map(|v| MaskVertex { p: v.p, t_in: v.t_out, t_out: v.t_in, ..*v }).collect(), closed: true };
        let p2 = matte_to_shape(&m1, 160, 140, Some(&rev), 0.5).unwrap();
        assert!(flat_area(&p2) < 0.0);
        assert!((p2.vertices[1].p.x - rev.vertices[1].p.x - dx).hypot(p2.vertices[1].p.y - rev.vertices[1].p.y - dy) < 1.5);
        // explicit count and start hint
        let c = contours(&m1, 160, 140, 0.5);
        let hint = [60.0 + dx + 35.0, 60.0 + dy];
        let q = fit_path_with_count(&c[0].points, 12, Some(hint));
        assert_eq!(q.len(), 12);
        assert!((q.vertices[0].p.x - hint[0]).hypot(q.vertices[0].p.y - hint[1]) < 0.5);
    }

    #[test]
    fn contour_square_count_fit_snaps_corners() {
        let m = coverage(100, 100, |x, y| (20.0..80.0).contains(&x) && (30.0..90.0).contains(&y));
        let s = matte_to_shape(&m, 100, 100, None, 0.5).unwrap();
        eprintln!("square shape: {} vertices, area {:.1}", s.len(), flat_area(&s));
        assert_eq!(s.len(), 4);
        assert!((flat_area(&s) - 3600.0).abs() / 3600.0 < 0.01);
    }

    #[test]
    fn contour_hostile_inputs() {
        assert!(contours(&[1.0; 10], 4, 4, 0.5).is_empty());
        assert!(contours(&[], 0, 0, 0.5).is_empty());
        assert!(contours(&[1.0], 1, 1, 0.5).is_empty()); // 0.5 px² diamond < min area
        assert_eq!(contours_with_min_area(&[1.0], 1, 1, 0.5, 0.0).len(), 1);
        assert!(contours(&[1.0; 4], usize::MAX, 2, 0.5).is_empty());
        assert!(contours(&[1.0; 4], 2, 2, f32::NAN).is_empty());
        assert!(contours(&[f32::NAN; 64], 8, 8, 0.5).is_empty());
        assert!(contours(&[f32::INFINITY; 64], 8, 8, 0.5).is_empty());
        let _ = contours(&[0.7; 64], 8, 8, -3.0);
        assert!(matte_to_shape(&[f32::NAN; 64], 8, 8, None, 0.5).is_none());
        assert!(matte_to_shape(&[], 0, 0, None, f64::NAN).is_none());
        // all ones touching the borders: a contour along the frame edge
        let cs = contours(&[1.0; 64 * 48], 64, 48, 0.5);
        assert_eq!(cs.len(), 1);
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for p in &cs[0].points {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
        assert_eq!((lo, hi), ([0.0, 0.0], [64.0, 48.0]));
        let s = matte_to_shape(&[1.0; 64 * 48], 64, 48, None, 0.5).unwrap();
        eprintln!("frame shape: {} vertices, area {:.1}", s.len(), flat_area(&s));
        assert!((flat_area(&s) - 64.0 * 48.0).abs() < 10.0);
        // checkerboard noise: bounded output, quick
        let t = std::time::Instant::now();
        let cb: Vec<f32> = (0..512 * 512).map(|i| ((i % 512 + i / 512) % 2) as f32).collect();
        let cs = contours(&cb, 512, 512, 0.5);
        let cs2 = contours_with_min_area(&cb, 512, 512, 0.51, 0.0);
        let shape = matte_to_shape(&cb, 512, 512, None, 0.5);
        eprintln!("checkerboard: {} / {} contours, shape {:?} vertices, {:?}", cs.len(), cs2.len(), shape.as_ref().map(|s| s.len()), t.elapsed());
        assert!(cs.len() <= MAX_CONTOURS && cs2.len() <= MAX_CONTOURS);
        assert!(t.elapsed().as_secs() < 20);
        // fitting hostile point lists
        assert!(fit_path(&[], 0.5, true).is_empty());
        assert!(fit_path(&[[1.0, 1.0]], 0.5, false).is_empty());
        assert!(fit_path(&[[f64::NAN, 1.0], [2.0, f64::INFINITY]], 0.5, true).is_empty());
        assert!(fit_path(&[[0.0, 0.0]; 10], 0.5, true).is_empty());
        assert_eq!(fit_path(&[[0.0, 0.0], [10.0, 0.0]], f64::NAN, false).len(), 2);
        let tri = [[0.0, 0.0], [10.0, 0.0], [5.0, 8.0]];
        assert!(fit_path_with_count(&tri, 0, Some([f64::NAN, 0.0])).len() == 3);
        assert_eq!(fit_path_with_count(&tri, usize::MAX, None).len(), MAX_VERTICES);
        assert!(fit_path_with_count(&[[1.0, 1.0]; 5], 8, None).is_empty());
        let degenerate = MaskPath { vertices: vec![MaskVertex::corner(Vec2::new(f64::NAN, 0.0)); 4], closed: true };
        let m = disc(64, 64, 32.0, 32.0, 20.0);
        assert!(matte_to_shape(&m, 64, 64, Some(&degenerate), 0.5).is_some());
        // a previous path too large to flatten cheaply is ignored rather than flattened
        let huge = MaskPath { vertices: vec![MaskVertex::corner(Vec2::new(1.0, 1.0)); MAX_VERTICES + 1], closed: true };
        assert!(matte_to_shape(&m, 64, 64, Some(&huge), 0.5).is_some_and(|s| s.len() >= MIN_CLOSED_VERTICES));
        let nan_tan = MaskPath {
            vertices: vec![MaskVertex { p: Vec2::new(1.0, 1.0), t_in: Vec2::new(f64::NAN, 0.0), t_out: Vec2::ZERO, ..MaskVertex::default() }; 4],
            closed: true,
        };
        assert!(matte_to_shape(&m, 64, 64, Some(&nan_tan), 0.5).is_some());
    }
}
