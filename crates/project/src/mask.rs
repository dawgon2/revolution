//! Effect masks (Premiere's per-effect masks: ellipse, 4-point polygon or rounded rectangle,
//! free-draw Bézier) and the linear (gradient) window of a graduated filter.
//!
//! Every mask is a closed cubic Bézier path in **clip pixel space** (full-resolution source
//! pixels, the space of effect point parameters). An ellipse is four smooth vertices with circular
//! tangents, a polygon is vertices with zero-length tangents and a rounded corner is a pair of
//! vertices joined by a circular-arc cubic ([`MaskPath::with_rounded_corners`]), so all the tools
//! produce the same [`MaskPath`] and the same rasterizer, keyframe interpolation and tracker apply
//! to each.
//!
//! A mask has a keyframable *Mask Path* (vertex-wise interpolation when the vertex counts match,
//! hold otherwise), *Feather* (falloff width in pixels, centred on the edge), *Opacity* (%),
//! *Expansion* (pixels, grows/shrinks the shape by signed distance), *Inverted*, and a combine
//! [`MaskMode`] with the masks above it. Coverage is computed by `filmcraft_render::mask`.
//!
//! A [`MaskShape::Linear`] mask (a graduated filter) reuses the same keyframable *Mask Path*: its
//! first and last vertices are the gradient's *start* and *end* points ([`MaskPath::linear`]: two
//! corner vertices, open). Coverage is 1 on the start side, 0 beyond the end, with a smoothstep
//! ramp between them, so the distance between the points is the softness. Feather widens that
//! ramp (total width = distance + feather, centred on the midpoint), Expansion moves the transition
//! towards the end (positive) or the start (negative); Opacity, Inverted and the mode work as for
//! every mask. Because it is an ordinary two-vertex path, keyframes, tracking and the vertex
//! commands apply unchanged.

use filmcraft_geom::{Affine, Vec2};
use serde::{Deserialize, Serialize};

use crate::effect::{ParamDef, ParamKind};
use crate::keyframe::{Param, ParamValue};

/// Circle tangent length for a 4-segment cubic approximation of a quarter circle.
pub const KAPPA: f64 = 0.552_284_749_830_793_4;

/// One path vertex with its incoming/outgoing Bézier tangents (relative to `p`; zero = corner).
///
/// `feather_in` / `feather_out` are a per-point feather (path pixels, >= 0: the distance inside
/// the outline where coverage reaches 1 / outside where it reaches 0) used by roto shapes
/// ([`crate::roto`]); effect masks ignore them. Unset (0) they are not serialized, so mask JSON
/// is unchanged. They interpolate with the vertex, and [`MaskPath::transformed`] scales them by
/// the transform's mean scale (never translates them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MaskVertex {
    pub p: Vec2,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub t_in: Vec2,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub t_out: Vec2,
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub feather_in: f64,
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub feather_out: f64,
}

fn is_zero(v: &Vec2) -> bool {
    v.x == 0.0 && v.y == 0.0
}

fn is_zero_f(v: &f64) -> bool {
    *v == 0.0
}

/// Largest per-point feather a cleaned vertex keeps (path pixels).
pub const MAX_POINT_FEATHER: f64 = 10_000.0;
/// Largest |coordinate| a cleaned vertex keeps (path pixels).
pub const MAX_VERTEX_COORD: f64 = 1e7;

impl MaskVertex {
    pub fn corner(p: Vec2) -> Self {
        Self { p, ..Self::default() }
    }
    /// A smooth vertex with mirrored tangents (`t_out = t`, `t_in = -t`).
    pub fn smooth(p: Vec2, t: Vec2) -> Self {
        Self { p, t_in: t * -1.0, t_out: t, ..Self::default() }
    }
    /// The vertex with per-point feather distances `inner` / `outer`.
    pub fn with_feather(self, inner: f64, outer: f64) -> Self {
        Self { feather_in: inner, feather_out: outer, ..self }
    }
    /// The vertex with every value finite and within the caps (non-finite -> 0; coordinates within
    /// ± [`MAX_VERTEX_COORD`], feathers 0 ..= [`MAX_POINT_FEATHER`]).
    pub fn cleaned(self) -> Self {
        let c = |v: f64| if v.is_finite() { v.clamp(-MAX_VERTEX_COORD, MAX_VERTEX_COORD) } else { 0.0 };
        let f = |v: f64| if v.is_finite() { v.clamp(0.0, MAX_POINT_FEATHER) } else { 0.0 };
        let v = |q: Vec2| Vec2::new(c(q.x), c(q.y));
        Self { p: v(self.p), t_in: v(self.t_in), t_out: v(self.t_out), feather_in: f(self.feather_in), feather_out: f(self.feather_out) }
    }
    pub fn is_corner(&self) -> bool {
        is_zero(&self.t_in) && is_zero(&self.t_out)
    }
    /// Set the in (`out` false) or out tangent to `tan`; unless `broken`, the other tangent
    /// mirrors it (a smooth vertex stays smooth).
    pub fn set_tangent(&mut self, out: bool, tan: Vec2, broken: bool) {
        let (this, other) = if out { (&mut self.t_out, &mut self.t_in) } else { (&mut self.t_in, &mut self.t_out) };
        *this = tan;
        if !broken {
            *other = tan * -1.0;
        }
    }
}

/// A closed cubic Bézier path (an open path is drawn but still filled as if closed).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MaskPath {
    pub vertices: Vec<MaskVertex>,
    #[serde(default = "yes")]
    pub closed: bool,
}

fn yes() -> bool {
    true
}

impl MaskPath {
    /// An axis-aligned ellipse (four smooth vertices: top, right, bottom, left).
    pub fn ellipse(center: Vec2, radius: Vec2) -> Self {
        let (rx, ry) = (radius.x, radius.y);
        let k = KAPPA;
        let v = vec![
            MaskVertex::smooth(center + Vec2::new(0.0, -ry), Vec2::new(rx * k, 0.0)),
            MaskVertex::smooth(center + Vec2::new(rx, 0.0), Vec2::new(0.0, ry * k)),
            MaskVertex::smooth(center + Vec2::new(0.0, ry), Vec2::new(-rx * k, 0.0)),
            MaskVertex::smooth(center + Vec2::new(-rx, 0.0), Vec2::new(0.0, -ry * k)),
        ];
        Self { vertices: v, closed: true }
    }
    /// Vertex `i` turned from a corner into a smooth vertex (tangent along its neighbours'
    /// direction, a sixth of the way between them) or from smooth into a corner. False (path
    /// unchanged) when there is no vertex `i`.
    pub fn toggle_smooth(&mut self, i: usize) -> bool {
        let n = self.vertices.len();
        if i >= n {
            return false;
        }
        let (Some(prev), Some(next)) = (self.vertices.get((i + n - 1) % n).map(|v| v.p), self.vertices.get((i + 1) % n).map(|v| v.p)) else {
            return false;
        };
        let Some(v) = self.vertices.get_mut(i) else { return false };
        let (fi, fo) = (v.feather_in, v.feather_out);
        *v = if v.is_corner() { MaskVertex::smooth(v.p, (next - prev) * (1.0 / 6.0)) } else { MaskVertex::corner(v.p) }.with_feather(fi, fo);
        true
    }
    /// A polygon through `points` (corner vertices).
    pub fn polygon(points: &[Vec2]) -> Self {
        Self { vertices: points.iter().map(|p| MaskVertex::corner(*p)).collect(), closed: true }
    }
    /// The two points of a linear (gradient) mask: `start` (full coverage) and `end` (none), as an
    /// open path of two corner vertices.
    pub fn linear(start: Vec2, end: Vec2) -> Self {
        Self { vertices: vec![MaskVertex::corner(start), MaskVertex::corner(end)], closed: false }
    }
    /// An axis-aligned rectangle (Premiere's 4-point polygon mask default).
    pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self::polygon(&[Vec2::new(x0, y0), Vec2::new(x1, y0), Vec2::new(x1, y1), Vec2::new(x0, y1)])
    }

    /// An axis-aligned rectangle with corners rounded by `radius` (Premiere 26's rounded
    /// rectangle mask). `radius` ≤ 0 (or not finite) gives the plain 4-point [`MaskPath::rect`].
    pub fn rounded_rect(x0: f64, y0: f64, x1: f64, y1: f64, radius: f64) -> Self {
        let r = Self::rect(x0, y0, x1, y1);
        if radius.is_finite() && radius > 0.0 { r.with_rounded_corners(radius, &[0, 1, 2, 3]) } else { r }
    }

    /// Indices of the sharp corners [`MaskPath::with_rounded_corners`] can round: corner vertices
    /// of a closed path whose two segments are straight lines.
    pub fn roundable_corners(&self) -> Vec<usize> {
        let n = self.vertices.len();
        if !self.closed || n < 3 {
            return Vec::new();
        }
        (0..n)
            .filter(|&i| {
                let (Some(prev), Some(v), Some(next)) = (self.vertices.get((i + n - 1) % n), self.vertices.get(i), self.vertices.get((i + 1) % n)) else {
                    return false;
                };
                v.is_corner() && is_zero(&prev.t_out) && is_zero(&next.t_in)
            })
            .collect()
    }

    /// The path with each listed corner replaced by a circular fillet of `radius`: two vertices on
    /// the adjacent edges joined by a cubic arc (handles `4/3·tan(φ/4)·r` for a turn of φ, so a
    /// right angle gets the [`KAPPA`] quarter circle). The fillet is shrunk so it never takes more
    /// than half of either edge. Every listed roundable corner ([`MaskPath::roundable_corners`])
    /// becomes two vertices, coincident when its geometry is degenerate (a straight or reversing
    /// corner, a zero-length edge), so applying the same list to each keyframe of an animated path
    /// keeps the vertex counts equal. Other indices (out of range, curved, repeated) are left alone.
    pub fn with_rounded_corners(&self, radius: f64, corners: &[usize]) -> Self {
        let n = self.vertices.len();
        let mut round = vec![false; n];
        for &i in corners {
            if let Some(r) = round.get_mut(i) {
                *r = true;
            }
        }
        let roundable = self.roundable_corners();
        round.iter_mut().enumerate().for_each(|(i, r)| *r &= roundable.binary_search(&i).is_ok());
        let radius = if radius.is_finite() { radius.clamp(0.0, 1e7) } else { 0.0 };
        let mut vertices = Vec::with_capacity(n.saturating_mul(2));
        for (i, v) in self.vertices.iter().enumerate() {
            if !round.get(i).copied().unwrap_or(false) {
                vertices.push(*v);
                continue;
            }
            let (Some(prev), Some(next)) = (self.vertices.get((i + n - 1) % n), self.vertices.get((i + 1) % n)) else {
                vertices.push(*v);
                continue;
            };
            let (a, b) = (v.p - prev.p, next.p - v.p);
            let (la, lb) = (a.length(), b.length());
            let fillet = (la > 1e-9 && lb > 1e-9 && radius > 0.0).then(|| (a * (1.0 / la), b * (1.0 / lb))).and_then(|(u, w)| {
                // turn angle φ between the incoming and outgoing directions
                let phi = (u.x * w.x + u.y * w.y).clamp(-1.0, 1.0).acos();
                if !(1e-6..std::f64::consts::PI - 1e-6).contains(&phi) {
                    return None;
                }
                let tan_half = (phi / 2.0).tan();
                let d = (radius * tan_half).min(la / 2.0).min(lb / 2.0);
                let r = d / tan_half;
                let h = 4.0 / 3.0 * (phi / 4.0).tan() * r;
                Some((u, w, d, h))
            });
            match fillet {
                Some((u, w, d, h)) => {
                    vertices.push(MaskVertex { p: v.p - u * d, t_in: Vec2::ZERO, t_out: u * h, ..*v });
                    vertices.push(MaskVertex { p: v.p + w * d, t_in: w * -h, t_out: Vec2::ZERO, ..*v });
                }
                None => {
                    vertices.push(MaskVertex { t_in: Vec2::ZERO, t_out: Vec2::ZERO, ..*v });
                    vertices.push(MaskVertex { t_in: Vec2::ZERO, t_out: Vec2::ZERO, ..*v });
                }
            }
        }
        Self { vertices, closed: self.closed }
    }

    pub fn len(&self) -> usize {
        self.vertices.len()
    }
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// The path with every point and tangent mapped by `m` (tangents by its linear part) and
    /// per-point feathers scaled by its mean scale `sqrt|det|` (distances never translate).
    pub fn transformed(&self, m: &Affine) -> Self {
        let lin = Affine { e: 0.0, f: 0.0, ..*m };
        let k = m.determinant().abs().sqrt();
        let k = if k.is_finite() { k } else { 1.0 };
        Self {
            vertices: self
                .vertices
                .iter()
                .map(|v| MaskVertex {
                    p: m.apply(v.p),
                    t_in: lin.apply(v.t_in),
                    t_out: lin.apply(v.t_out),
                    feather_in: v.feather_in * k,
                    feather_out: v.feather_out * k,
                })
                .collect(),
            closed: self.closed,
        }
    }

    /// The path with every vertex cleaned ([`MaskVertex::cleaned`]) and at most `max` vertices.
    pub fn cleaned(&self, max: usize) -> Self {
        Self { vertices: self.vertices.iter().take(max).map(|v| v.cleaned()).collect(), closed: self.closed }
    }

    /// Mean of the vertex positions.
    pub fn centroid(&self) -> Vec2 {
        if self.vertices.is_empty() {
            return Vec2::ZERO;
        }
        let s = self.vertices.iter().fold(Vec2::ZERO, |a, v| a + v.p);
        s * (1.0 / self.vertices.len() as f64)
    }

    /// Bounding box of the control polygon (contains the curve): (min, max).
    pub fn bounds(&self) -> (Vec2, Vec2) {
        let mut lo = Vec2::new(f64::INFINITY, f64::INFINITY);
        let mut hi = Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        for v in &self.vertices {
            for q in [v.p, v.p + v.t_in, v.p + v.t_out] {
                lo = Vec2::new(lo.x.min(q.x), lo.y.min(q.y));
                hi = Vec2::new(hi.x.max(q.x), hi.y.max(q.y));
            }
        }
        (lo, hi)
    }

    /// Cubic segments `(p0, c1, c2, p3)`; a closed path includes the last→first segment.
    pub fn segments(&self) -> Vec<[Vec2; 4]> {
        let n = self.vertices.len();
        if n < 2 {
            return Vec::new();
        }
        let count = if self.closed { n } else { n - 1 };
        (0..count)
            .map(|i| {
                let a = &self.vertices[i];
                let b = &self.vertices[(i + 1) % n];
                [a.p, a.p + a.t_out, b.p + b.t_in, b.p]
            })
            .collect()
    }

    /// Flatten to a polygon (closed implicitly) whose chords deviate from the curve by at most
    /// `tol` (same units as the path). Corner-only segments emit just their end point.
    pub fn flatten(&self, tol: f64) -> Vec<Vec2> {
        let mut out = Vec::new();
        let n = self.vertices.len();
        if n == 0 {
            return out;
        }
        out.push(self.vertices[0].p);
        // an open path is filled as if closed: flatten its closing segment too
        let segs = if self.closed || n < 2 {
            self.segments()
        } else {
            let mut s = self.segments();
            let (a, b) = (&self.vertices[n - 1], &self.vertices[0]);
            s.push([a.p, a.p + a.t_out, b.p + b.t_in, b.p]);
            s
        };
        let tol = tol.max(1e-4);
        for (i, [p0, c1, c2, p3]) in segs.iter().enumerate() {
            let last = i + 1 == segs.len();
            let straight = (*c1 - *p0).length() < 1e-12 && (*c2 - *p3).length() < 1e-12;
            if !straight {
                // Wang's bound on the number of chords for a cubic.
                let d1 = (*p0 - *c1 * 2.0 + *c2).length();
                let d2 = (*c1 - *c2 * 2.0 + *p3).length();
                let m = d1.max(d2);
                let steps = ((0.75 * m / tol).sqrt().ceil() as usize).clamp(1, 256);
                for k in 1..steps {
                    let t = k as f64 / steps as f64;
                    out.push(cubic(*p0, *c1, *c2, *p3, t));
                }
            }
            if !last {
                out.push(*p3);
            }
        }
        out
    }

    /// Flattened components for keyframe interpolation:
    /// `[x, y, in.x, in.y, out.x, out.y, feather_in, feather_out]` per vertex.
    pub fn components(&self) -> Vec<f64> {
        self.vertices.iter().flat_map(|v| [v.p.x, v.p.y, v.t_in.x, v.t_in.y, v.t_out.x, v.t_out.y, v.feather_in, v.feather_out]).collect()
    }
    /// Inverse of [`MaskPath::components`] (`closed` is taken from `self`).
    pub fn with_components(&self, c: &[f64]) -> Self {
        Self {
            vertices: c
                .as_chunks::<8>()
                .0
                .iter()
                .map(|k| MaskVertex {
                    p: Vec2::new(k[0], k[1]),
                    t_in: Vec2::new(k[2], k[3]),
                    t_out: Vec2::new(k[4], k[5]),
                    feather_in: k[6],
                    feather_out: k[7],
                })
                .collect(),
            closed: self.closed,
        }
    }
}

/// Point on a cubic Bézier at `t`.
pub fn cubic(p0: Vec2, c1: Vec2, c2: Vec2, p3: Vec2, t: f64) -> Vec2 {
    let u = 1.0 - t;
    p0 * (u * u * u) + c1 * (3.0 * u * u * t) + c2 * (3.0 * u * t * t) + p3 * (t * t * t)
}

/// How a mask combines with the result of the masks above it (After Effects' mask modes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskMode {
    /// Ignored (kept for the user to switch back on).
    None,
    /// Union: `a + m − a·m`.
    #[default]
    Add,
    /// `a · (1 − m)`.
    Subtract,
    /// `a · m`.
    Intersect,
    /// `max(a, m)`.
    Lighten,
    /// `min(a, m)`.
    Darken,
    /// `a + m − 2·a·m`.
    Difference,
}

impl MaskMode {
    pub const ALL: [MaskMode; 7] =
        [MaskMode::None, MaskMode::Add, MaskMode::Subtract, MaskMode::Intersect, MaskMode::Lighten, MaskMode::Darken, MaskMode::Difference];
    pub fn label(self) -> &'static str {
        match self {
            MaskMode::None => "None",
            MaskMode::Add => "Add",
            MaskMode::Subtract => "Subtract",
            MaskMode::Intersect => "Intersect",
            MaskMode::Lighten => "Lighten",
            MaskMode::Darken => "Darken",
            MaskMode::Difference => "Difference",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        Self::ALL.into_iter().find(|m| m.label().to_ascii_lowercase() == s)
    }
    /// Shader / wire index.
    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|m| *m == self).unwrap_or(1) as u32
    }
    /// Combine accumulated coverage `a` with this mask's coverage `m`.
    pub fn combine(self, a: f32, m: f32) -> f32 {
        match self {
            MaskMode::None => a,
            MaskMode::Add => a + m - a * m,
            MaskMode::Subtract => a * (1.0 - m),
            MaskMode::Intersect => a * m,
            MaskMode::Lighten => a.max(m),
            MaskMode::Darken => a.min(m),
            MaskMode::Difference => a + m - 2.0 * a * m,
        }
    }
    /// Starting value when this is the first active mask: additive modes start from nothing,
    /// subtractive ones from the full frame.
    pub fn start(self) -> f32 {
        match self {
            MaskMode::Subtract | MaskMode::Intersect | MaskMode::Darken => 1.0,
            _ => 0.0,
        }
    }
}

/// Mask tracking method (Premiere's wrench menu next to the tracking buttons).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackMethod {
    Position,
    PositionRotation,
    #[default]
    PositionScaleRotation,
}

impl TrackMethod {
    pub const ALL: [TrackMethod; 3] = [TrackMethod::Position, TrackMethod::PositionRotation, TrackMethod::PositionScaleRotation];
    pub fn label(self) -> &'static str {
        match self {
            TrackMethod::Position => "Position",
            TrackMethod::PositionRotation => "Position & Rotation",
            TrackMethod::PositionScaleRotation => "Position, Scale & Rotation",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace([' ', '_', '-', '&', ','], "").as_str() {
            "position" => Some(TrackMethod::Position),
            "positionrotation" => Some(TrackMethod::PositionRotation),
            "positionscalerotation" => Some(TrackMethod::PositionScaleRotation),
            _ => None,
        }
    }
}

/// What a mask's path describes. Projects saved before linear masks have no `shape` and load as
/// [`MaskShape::Path`]; path masks keep serializing without the field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaskShape {
    /// The filled (closed) Bézier path: ellipse, polygon, rounded rectangle, free-draw.
    #[default]
    Path,
    /// A linear gradient window from the path's first vertex (coverage 1) to its last (coverage 0).
    Linear,
}

impl MaskShape {
    pub fn is_path(&self) -> bool {
        *self == MaskShape::Path
    }
}

/// A mask on an effect instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mask {
    pub name: String,
    /// Path or linear gradient (absent in older projects: a path).
    #[serde(default, skip_serializing_if = "MaskShape::is_path")]
    pub shape: MaskShape,
    /// `ParamValue::Path` (for a linear mask: start = first vertex, end = last vertex).
    pub path: Param,
    /// Feather width in clip pixels (≥ 0).
    pub feather: Param,
    /// 0..100 %.
    pub opacity: Param,
    /// Clip pixels (negative shrinks).
    pub expansion: Param,
    #[serde(default)]
    pub inverted: bool,
    #[serde(default)]
    pub mode: MaskMode,
    #[serde(default)]
    pub track_method: TrackMethod,
    /// A planar track the mask follows (fork G4.4): the path (keyframes included) is stored in
    /// the track's reference-frame pixels and drawn through the track's homography at each time
    /// ([`PlanarTrack::homography_at`]). Absent in older projects and in masks never tracked
    /// planar, so their saves are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planar: Option<PlanarTrack>,
}

/// Mask parameter ids (keyframe commands address them with `"mask": n`).
pub const MASK_PARAMS: [&str; 4] = ["path", "feather", "opacity", "expansion"];

impl Mask {
    pub fn new(name: impl Into<String>, path: MaskPath) -> Self {
        Self {
            name: name.into(),
            shape: MaskShape::Path,
            path: Param::new(ParamValue::Path(path)),
            feather: Param::new(ParamValue::Float(10.0)),
            opacity: Param::new(ParamValue::Float(100.0)),
            expansion: Param::new(ParamValue::Float(0.0)),
            inverted: false,
            mode: MaskMode::Add,
            track_method: TrackMethod::default(),
            planar: None,
        }
    }
    /// A linear (gradient) mask from `start` (coverage 1) to `end` (coverage 0). Its feather starts
    /// at 0: the distance between the points is the softness.
    pub fn linear(name: impl Into<String>, start: Vec2, end: Vec2) -> Self {
        let mut m = Self::new(name, MaskPath::linear(start, end));
        m.shape = MaskShape::Linear;
        m.feather = Param::new(ParamValue::Float(0.0));
        m
    }
    /// The start and end points of a linear mask at `t` (first and last path vertex; `None` for a
    /// path mask or a path with fewer than two vertices).
    pub fn linear_points_at(&self, t: filmcraft_time::Tick) -> Option<(Vec2, Vec2)> {
        if self.shape != MaskShape::Linear {
            return None;
        }
        let path = self.path_at(t);
        if path.vertices.len() < 2 {
            return None;
        }
        Some((path.vertices.first()?.p, path.vertices.last()?.p))
    }
    pub fn param(&self, id: &str) -> Option<&Param> {
        match id {
            "path" => Some(&self.path),
            "feather" => Some(&self.feather),
            "opacity" => Some(&self.opacity),
            "expansion" => Some(&self.expansion),
            _ => None,
        }
    }
    pub fn param_mut(&mut self, id: &str) -> Option<&mut Param> {
        match id {
            "path" => Some(&mut self.path),
            "feather" => Some(&mut self.feather),
            "opacity" => Some(&mut self.opacity),
            "expansion" => Some(&mut self.expansion),
            _ => None,
        }
    }
    pub fn path_at(&self, t: filmcraft_time::Tick) -> MaskPath {
        match self.path.value_at(t) {
            ParamValue::Path(p) => p,
            _ => MaskPath::default(),
        }
    }
    /// Add a corner vertex at `at` after vertex `after` of the path at `t` (clamped to the end).
    /// An animated path gets the vertex in every keyframe (halfway along that keyframe's edge),
    /// so the keyframes stay interpolable; the one at `t` is then placed at `at`. A linear mask
    /// is refused: it has just its start and end points.
    pub fn add_vertex(&mut self, after: usize, at: Vec2, t: filmcraft_time::Tick) -> Result<(), &'static str> {
        if self.shape == MaskShape::Linear {
            return Err("a linear mask has just its start and end points");
        }
        let n = self.path_at(t).vertices.len();
        // clamp against the length before any insertion, so the index stays valid in the grown path
        let i = after.saturating_add(1).min(n);
        let keyed = self.path.keyframes.len() > 1;
        if keyed {
            if n == 0 {
                return Err("the animated mask path has no vertices to insert after");
            }
            for k in &mut self.path.keyframes {
                if let ParamValue::Path(path) = &mut k.value {
                    let len = path.vertices.len();
                    let at_i = i.min(len);
                    let (Some(a), Some(b)) = (path.vertices.get(after.min(len.saturating_sub(1))), path.vertices.get(at_i % len.max(1))) else { continue };
                    let mid = a.p.lerp(b.p, 0.5);
                    path.vertices.insert(at_i, MaskVertex::corner(mid));
                }
            }
        }
        let mut path = self.path_at(t);
        if keyed {
            if let Some(v) = path.vertices.get_mut(i) {
                *v = MaskVertex::corner(at);
            }
        } else {
            path.vertices.insert(i.min(path.vertices.len()), MaskVertex::corner(at));
        }
        self.path.set_at(t, ParamValue::Path(path));
        Ok(())
    }
    /// Remove vertex `vi` from the path and every keyframe; a mask keeps at least 3 vertices.
    pub fn remove_vertex(&mut self, vi: usize, t: filmcraft_time::Tick) -> Result<(), &'static str> {
        if self.path_at(t).len() <= 3 {
            return Err("a mask keeps at least 3 vertices");
        }
        let values = std::iter::once(&mut self.path.value).chain(self.path.keyframes.iter_mut().map(|k| &mut k.value));
        for v in values {
            if let ParamValue::Path(path) = v
                && vi < path.vertices.len()
            {
                path.vertices.remove(vi);
            }
        }
        Ok(())
    }
    pub fn is_animated(&self) -> bool {
        MASK_PARAMS.iter().any(|p| self.param(p).is_some_and(Param::is_animated))
    }
    /// Keyframe times of every mask parameter (for the keyframe lane / presets).
    pub fn params_mut(&mut self) -> [&mut Param; 4] {
        [&mut self.path, &mut self.feather, &mut self.opacity, &mut self.expansion]
    }
}

/// Parameter schemas of the mask rows in Effect Controls (and the MCP schema).
pub fn mask_param_defs() -> &'static [ParamDef] {
    use std::sync::OnceLock;
    static DEFS: OnceLock<Vec<ParamDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        let fl = |id, label, def, (min, max): (f64, f64), (smin, smax): (f64, f64), unit| ParamDef {
            id,
            label,
            kind: ParamKind::Float { min, max, soft_min: smin, soft_max: smax, unit, decimals: 1 },
            default: ParamValue::Float(def),
            animatable: true,
            group: None,
        };
        vec![
            ParamDef { id: "path", label: "Mask Path", kind: ParamKind::Path, default: ParamValue::Path(MaskPath::default()), animatable: true, group: None },
            fl("feather", "Mask Feather", 10.0, (0.0, 10_000.0), (0.0, 500.0), ""),
            fl("opacity", "Mask Opacity", 100.0, (0.0, 100.0), (0.0, 100.0), " %"),
            fl("expansion", "Mask Expansion", 0.0, (-10_000.0, 10_000.0), (-500.0, 500.0), ""),
        ]
    })
}

// ---- planar track (fork G4.4) ----------------------------------------------------------------

/// Most keys one [`PlanarTrack`] keeps (≈ 2.3 h at 24 fps, 16 MB in memory).
pub const MAX_PLANAR_KEYS: usize = 200_000;
/// Largest |entry| of a stored homography (after normalising `h[8]` to 1): pixel offsets up to a
/// few 10⁴ and perspective terms far below 1 stay well inside it.
const MAX_PLANAR_ENTRY: f64 = 1e9;

/// One frame of a planar track: the homography (row-major 3×3, `h[8]` = 1) mapping the track's
/// reference-frame pixels to the clip's pixels at `time` (clip media time, like mask keyframes).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PlanarKey {
    pub time: filmcraft_time::Tick,
    pub h: [f64; 9],
}

/// The per-frame plane motion a mask follows (fork G4.4, `masks.trackPlanar`).
///
/// **Design.** The planar tracker (`filmcraft_render::track_run::PlanarTracker`, fork C3) gives a
/// homography per frame. Rather than baking it into Mask Path keyframes (a projected Bézier is
/// not a Bézier: a curved window on a tilting sign would lose its shape), the mask keeps the
/// motion itself and the renderer maps the *flattened* outline through it, which is exact for any
/// curve, on the CPU and the GPU alike (both consume the same flattened polygon,
/// `filmcraft_render::mask::prepare`). The stored path and its keyframes stay in the reference
/// frame's pixels, so reshaping the window later never fights the track (the on-monitor handles
/// are drawn and dragged where the window is seen; the engine maps the drag back). Between keys
/// the homography is interpolated entry by entry (keys are one frame apart); before the first and
/// after the last key it holds.
///
/// Every number is hostile: loading runs [`PlanarTrack::cleaned`] (drops keys that are not
/// finite, singular or absurd, sorts and dedups them by time, caps their count), and
/// [`PlanarTrack::homography_at`] only returns maps that pass the same check.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "RawPlanarTrack")]
pub struct PlanarTrack {
    /// The frame (clip media time) the stored path was drawn on: the motion is identity there.
    pub reference: filmcraft_time::Tick,
    pub keys: Vec<PlanarKey>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawPlanarTrack {
    reference: filmcraft_time::Tick,
    keys: Vec<RawPlanarKey>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPlanarKey {
    time: filmcraft_time::Tick,
    /// (`null` entries: NaN / ∞ as a damaged file writes them; the key is dropped)
    h: Vec<Option<f64>>,
}

impl From<RawPlanarTrack> for PlanarTrack {
    fn from(r: RawPlanarTrack) -> Self {
        let keys = r
            .keys
            .into_iter()
            .filter_map(|k| {
                let v: Option<Vec<f64>> = k.h.into_iter().collect();
                let h: [f64; 9] = v?.try_into().ok()?;
                Some(PlanarKey { time: k.time, h })
            })
            .collect();
        PlanarTrack { reference: r.reference, keys }.cleaned()
    }
}

/// `h` normalised to `h[8]` = 1, or None when it is not a usable map: non-finite, absurdly large,
/// singular (relative to its rows' lengths) or with `h[8]` ≈ 0 (the origin sent to infinity).
pub fn clean_homography(h: [f64; 9]) -> Option<[f64; 9]> {
    if !h.iter().all(|v| v.is_finite()) || h[8].abs() < 1e-12 {
        return None;
    }
    let n = h.map(|v| v / h[8]);
    if !n.iter().all(|v| v.is_finite() && v.abs() <= MAX_PLANAR_ENTRY) {
        return None;
    }
    let det = n[0] * (n[4] * n[8] - n[5] * n[7]) - n[1] * (n[3] * n[8] - n[5] * n[6]) + n[2] * (n[3] * n[7] - n[4] * n[6]);
    let row = |r: usize| n.get(r * 3..r * 3 + 3).map_or(f64::NAN, |v| v.iter().map(|x| x * x).sum::<f64>().sqrt());
    let scale = row(0) * row(1) * row(2);
    (det.is_finite() && scale.is_finite() && det.abs() > 1e-10 * scale.max(1e-300)).then_some(n)
}

/// `p` mapped through the (row-major) homography `h`; None at or near the line sent to infinity.
pub fn apply_homography(h: &[f64; 9], p: Vec2) -> Option<Vec2> {
    let w = h[6] * p.x + h[7] * p.y + h[8];
    if !w.is_finite() || w.abs() < 1e-12 {
        return None;
    }
    let q = Vec2::new((h[0] * p.x + h[1] * p.y + h[2]) / w, (h[3] * p.x + h[4] * p.y + h[5]) / w);
    (q.x.is_finite() && q.y.is_finite()).then_some(q)
}

impl PlanarTrack {
    /// Within the caps: usable maps only (normalised), sorted by time, one per time (the last
    /// written wins), at most [`MAX_PLANAR_KEYS`] (the earliest).
    pub fn cleaned(mut self) -> Self {
        self.keys = self.keys.into_iter().filter_map(|k| clean_homography(k.h).map(|h| PlanarKey { time: k.time, h })).collect();
        // (stable: of equal times the last written comes last)
        self.keys.sort_by_key(|k| k.time);
        let mut out: Vec<PlanarKey> = Vec::with_capacity(self.keys.len().min(MAX_PLANAR_KEYS));
        for k in self.keys {
            let full = out.len() >= MAX_PLANAR_KEYS;
            match out.last_mut() {
                Some(l) if l.time == k.time => *l = k,
                _ if full => break,
                _ => out.push(k),
            }
        }
        self.keys = out;
        self
    }

    /// The map from reference pixels to clip pixels at `t` (`h[8]` = 1): interpolated between
    /// the two keys around `t`, held before the first and after the last. None without keys.
    pub fn homography_at(&self, t: filmcraft_time::Tick) -> Option<[f64; 9]> {
        let i = self.keys.partition_point(|k| k.time <= t);
        let (a, b) = match (i.checked_sub(1).and_then(|j| self.keys.get(j)), self.keys.get(i)) {
            (Some(a), Some(b)) => (a, b),
            (Some(k), None) | (None, Some(k)) => return clean_homography(k.h),
            (None, None) => return None,
        };
        let span = (b.time.0 as f64) - (a.time.0 as f64);
        let u = if span > 0.0 { ((t.0 as f64 - a.time.0 as f64) / span).clamp(0.0, 1.0) } else { 0.0 };
        let mut m = [0.0; 9];
        for ((o, x), y) in m.iter_mut().zip(&a.h).zip(&b.h) {
            *o = x + (y - x) * u;
        }
        clean_homography(m).or_else(|| clean_homography(if u < 0.5 { a.h } else { b.h }))
    }
}

#[cfg(test)]
mod planar_tests {
    use super::*;
    use filmcraft_time::Tick;

    const ID: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

    #[test]
    fn old_masks_load_and_save_unchanged() {
        let m = Mask::new("w", MaskPath::rect(0.0, 0.0, 10.0, 10.0));
        let j = serde_json::to_string(&m).unwrap();
        assert!(!j.contains("planar"), "{j}");
        let back: Mask = serde_json::from_str(&j).unwrap();
        assert_eq!(back, m);
        assert_eq!(serde_json::to_string(&back).unwrap(), j, "byte-identical");
    }

    #[test]
    fn planar_track_round_trips_and_interpolates() {
        let mut m = Mask::new("w", MaskPath::rect(0.0, 0.0, 10.0, 10.0));
        let shift = |dx: f64| [1.0, 0.0, dx, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        m.planar = Some(PlanarTrack { reference: Tick(0), keys: vec![PlanarKey { time: Tick(0), h: ID }, PlanarKey { time: Tick(100), h: shift(10.0) }] });
        let j = serde_json::to_string(&m).unwrap();
        let back: Mask = serde_json::from_str(&j).unwrap();
        assert_eq!(back, m);
        let p = back.planar.unwrap();
        let at = |t: i64| apply_homography(&p.homography_at(Tick(t)).unwrap(), Vec2::new(1.0, 2.0)).unwrap();
        assert_eq!(at(-50), Vec2::new(1.0, 2.0), "held before");
        assert!((at(50).x - 6.0).abs() < 1e-12);
        assert!((at(500).x - 11.0).abs() < 1e-12, "held after");
        assert_eq!(PlanarTrack::default().homography_at(Tick(0)), None);
    }

    #[test]
    fn hostile_planar_tracks_are_cleaned_never_panic() {
        // NaN (written as null), wrong lengths, singular, h8 = 0, huge, unsorted duplicates
        let j = r#"{"reference": 5, "keys": [
            {"time": 30, "h": [1,0,0, 0,1,0, 0,0,1]},
            {"time": 10, "h": [null,0,0, 0,1,0, 0,0,1]},
            {"time": 11, "h": [1,0,0]},
            {"time": 12, "h": [1,2,3, 2,4,6, 0,0,1]},
            {"time": 13, "h": [1,0,0, 0,1,0, 0,0,0]},
            {"time": 14, "h": [1e300,0,0, 0,1,0, 0,0,1]},
            {"time": 20, "h": [2,0,0, 0,2,0, 0,0,2]},
            {"time": 20, "h": [1,0,4, 0,1,0, 0,0,1]},
            {"time": 7},
            {}
        ]}"#;
        let p: PlanarTrack = serde_json::from_str(j).unwrap();
        assert_eq!(p.reference, Tick(5));
        assert_eq!(p.keys.iter().map(|k| k.time.0).collect::<Vec<_>>(), vec![20, 30], "{p:?}");
        assert_eq!(p.keys[0].h[2], 4.0, "the last written of equal times wins");
        // a hostile mid-way blend falls back to a key; absurd times never panic
        let flip = PlanarTrack {
            reference: Tick(0),
            keys: vec![PlanarKey { time: Tick(0), h: ID }, PlanarKey { time: Tick(10), h: [-1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0] }],
        };
        assert!(flip.homography_at(Tick(5)).is_some());
        for t in [i64::MIN, -1, 0, 5, 10, i64::MAX] {
            let _ = flip.homography_at(Tick(t));
        }
        // the key cap
        let many = PlanarTrack { reference: Tick(0), keys: (0..MAX_PLANAR_KEYS as i64 + 10).map(|t| PlanarKey { time: Tick(t), h: ID }).collect() }.cleaned();
        assert_eq!(many.keys.len(), MAX_PLANAR_KEYS);
        assert_eq!(apply_homography(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], Vec2::ZERO), None);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tangents_mirror_unless_broken_and_vertices_toggle_smooth() {
        let mut v = MaskVertex::smooth(Vec2::new(10.0, 10.0), Vec2::new(3.0, 0.0));
        v.set_tangent(true, Vec2::new(0.0, 4.0), false);
        assert_eq!((v.t_out, v.t_in), (Vec2::new(0.0, 4.0), Vec2::new(0.0, -4.0)));
        v.set_tangent(false, Vec2::new(-1.0, 0.0), true);
        assert_eq!((v.t_out, v.t_in), (Vec2::new(0.0, 4.0), Vec2::new(-1.0, 0.0)));
        let mut r = MaskPath::rect(0.0, 0.0, 60.0, 30.0);
        assert!(r.toggle_smooth(1));
        assert_eq!(r.vertices[1].t_out, Vec2::new(10.0, 5.0));
        assert_eq!(r.vertices[1].t_in, Vec2::new(-10.0, -5.0));
        assert!(r.toggle_smooth(1) && r.vertices[1].is_corner());
        assert!(!r.toggle_smooth(4) && !MaskPath::default().toggle_smooth(0));
    }

    use super::*;
    use filmcraft_time::Tick;

    #[test]
    fn ellipse_flattens_onto_the_ellipse() {
        let p = MaskPath::ellipse(Vec2::new(100.0, 50.0), Vec2::new(40.0, 20.0));
        let pts = p.flatten(0.05);
        assert!(pts.len() > 16, "{}", pts.len());
        for q in &pts {
            let e = ((q.x - 100.0) / 40.0).powi(2) + ((q.y - 50.0) / 20.0).powi(2);
            assert!((e - 1.0).abs() < 0.01, "{q:?} off the ellipse: {e}");
        }
    }

    #[test]
    fn polygon_flattens_to_its_corners() {
        let p = MaskPath::rect(0.0, 0.0, 10.0, 5.0);
        assert_eq!(p.flatten(0.1), vec![Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 5.0), Vec2::new(0.0, 5.0)]);
    }

    #[test]
    fn path_keyframes_interpolate_vertexwise() {
        let a = MaskPath::rect(0.0, 0.0, 10.0, 10.0);
        let b = a.transformed(&Affine::translate(100.0, 20.0));
        let mut m = Mask::new("Mask (1)", a.clone());
        m.path.put_keyframe(Tick(0), ParamValue::Path(a));
        m.path.put_keyframe(Tick(100), ParamValue::Path(b));
        let mid = m.path_at(Tick(50));
        assert_eq!(mid.vertices[0].p, Vec2::new(50.0, 10.0));
        assert_eq!(mid.vertices[2].p, Vec2::new(60.0, 20.0));
        // different vertex counts hold the earlier path
        m.path.put_keyframe(Tick(200), ParamValue::Path(MaskPath::ellipse(Vec2::ZERO, Vec2::new(1.0, 1.0))));
        m.path.put_keyframe(Tick(300), ParamValue::Path(MaskPath::polygon(&[Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)])));
        assert_eq!(m.path_at(Tick(250)).len(), 4);
    }

    #[test]
    fn modes_combine() {
        assert_eq!(MaskMode::Add.combine(0.0, 0.5), 0.5);
        assert_eq!(MaskMode::Subtract.combine(1.0, 0.25), 0.75);
        assert_eq!(MaskMode::Intersect.combine(0.5, 0.5), 0.25);
        assert_eq!(MaskMode::Difference.combine(1.0, 1.0), 0.0);
        assert_eq!(MaskMode::from_name("intersect"), Some(MaskMode::Intersect));
        assert_eq!(TrackMethod::from_name("Position & Rotation"), Some(TrackMethod::PositionRotation));
    }

    #[test]
    fn mask_serde_round_trip() {
        let mut m = Mask::new("Mask (1)", MaskPath::ellipse(Vec2::new(5.0, 5.0), Vec2::new(3.0, 2.0)));
        m.inverted = true;
        m.mode = MaskMode::Subtract;
        m.feather.put_keyframe(Tick(10), ParamValue::Float(4.0));
        let s = serde_json::to_string(&m).unwrap();
        let back: Mask = serde_json::from_str(&s).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn old_mask_json_loads_as_a_path_and_linear_masks_round_trip() {
        // path masks serialize exactly as before (no `shape`)
        let old = Mask::new("Mask (1)", MaskPath::rect(0.0, 0.0, 10.0, 5.0));
        let v = serde_json::to_value(&old).unwrap();
        assert!(v.get("shape").is_none(), "{v}");
        // a mask as saved before linear masks existed
        let legacy = r#"{"name":"Mask (2)","path":{"value":{"Path":{"vertices":[{"p":{"x":1.0,"y":2.0}},{"p":{"x":5.0,"y":2.0}},{"p":{"x":3.0,"y":6.0}}]}}},
            "feather":{"value":{"Float":4.0}},"opacity":{"value":{"Float":100.0}},"expansion":{"value":{"Float":0.0}},"inverted":true,"mode":"Subtract"}"#;
        let m: Mask = serde_json::from_str(legacy).unwrap();
        assert_eq!((m.shape, m.inverted, m.mode, m.path_at(Tick(0)).len()), (MaskShape::Path, true, MaskMode::Subtract, 3));
        assert_eq!(m.linear_points_at(Tick(0)), None);
        // a keyframed linear mask round-trips and interpolates its points
        let mut l = Mask::linear("Mask (3)", Vec2::new(0.0, 0.0), Vec2::new(0.0, 100.0));
        assert_eq!(l.feather.f64_at(Tick(0)), 0.0);
        l.path.put_keyframe(Tick(0), ParamValue::Path(MaskPath::linear(Vec2::new(0.0, 0.0), Vec2::new(0.0, 100.0))));
        l.path.put_keyframe(Tick(100), ParamValue::Path(MaskPath::linear(Vec2::new(100.0, 0.0), Vec2::new(100.0, 200.0))));
        let s = serde_json::to_string(&l).unwrap();
        assert!(s.contains(r#""shape":"linear""#), "{s}");
        let back: Mask = serde_json::from_str(&s).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.linear_points_at(Tick(50)), Some((Vec2::new(50.0, 0.0), Vec2::new(50.0, 150.0))));
        // an emptied path has no points
        let mut e = l.clone();
        e.path = Param::new(ParamValue::Path(MaskPath::default()));
        assert_eq!(e.linear_points_at(Tick(0)), None);
    }

    #[test]
    fn per_point_feather_is_optional_interpolates_and_scales() {
        // mask JSON without feathers is byte-identical after a round trip
        let json = r#"{"vertices":[{"p":{"x":1.0,"y":2.0}},{"p":{"x":5.0,"y":2.0},"t_in":{"x":-1.0,"y":0.0},"t_out":{"x":1.0,"y":0.0}},{"p":{"x":3.0,"y":6.0}}],"closed":true}"#;
        let p: MaskPath = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&p).unwrap(), json);
        let old = Mask::new("Mask (1)", p);
        let s = serde_json::to_string(&old).unwrap();
        assert!(!s.contains("feather_"), "{s}");
        assert_eq!(serde_json::from_str::<Mask>(&s).unwrap(), old);
        // feathers interpolate with the vertex
        let a = MaskPath::rect(0.0, 0.0, 10.0, 10.0);
        let mut b = a.clone();
        b.vertices[1] = b.vertices[1].with_feather(4.0, 8.0);
        let mut m = Mask::new("m", a.clone());
        m.path.put_keyframe(Tick(0), ParamValue::Path(a));
        m.path.put_keyframe(Tick(100), ParamValue::Path(b.clone()));
        let mid = m.path_at(Tick(50));
        assert_eq!((mid.vertices[1].feather_in, mid.vertices[1].feather_out), (2.0, 4.0));
        // a transform scales feathers by its mean scale and never translates them
        let t = b.transformed(&Affine { a: 2.0, b: 0.0, c: 0.0, d: 8.0, e: 100.0, f: 50.0 });
        assert_eq!((t.vertices[1].feather_in, t.vertices[1].feather_out), (16.0, 32.0));
        let t = b.transformed(&Affine::translate(100.0, 50.0));
        assert_eq!((t.vertices[1].feather_in, t.vertices[1].feather_out), (4.0, 8.0));
        // smooth toggling and rounding keep them
        let mut c = b.clone();
        assert!(c.toggle_smooth(1) && c.vertices[1].feather_out == 8.0);
        let r = b.with_rounded_corners(2.0, &[1]);
        assert_eq!((r.vertices[1].feather_out, r.vertices[2].feather_out), (8.0, 8.0));
        // cleaning
        let v = MaskVertex::corner(Vec2::new(f64::NAN, 1e300)).with_feather(-1.0, f64::INFINITY).cleaned();
        assert_eq!(v, MaskVertex::corner(Vec2::new(0.0, MAX_VERTEX_COORD)));
    }

    #[test]
    fn rounded_rect_corners_are_quarter_circles() {
        let p = MaskPath::rounded_rect(0.0, 0.0, 100.0, 60.0, 20.0);
        assert_eq!(p.len(), 8);
        // top-left corner: a quarter circle of radius 20 about (20, 20); the edges stay straight
        let pts = p.flatten(0.01);
        let mut on_arc = 0;
        for q in &pts {
            let (cx, cy) = (q.x.clamp(20.0, 80.0), q.y.clamp(20.0, 40.0));
            let d = ((q.x - cx).powi(2) + (q.y - cy).powi(2)).sqrt();
            if (q.x - cx).abs() > 1e-9 && (q.y - cy).abs() > 1e-9 {
                on_arc += 1;
                assert!((d - 20.0).abs() < 0.03, "{q:?} is {d} from its corner centre");
            }
            assert!(q.x >= -1e-9 && q.x <= 100.0 + 1e-9 && q.y >= -1e-9 && q.y <= 60.0 + 1e-9, "{q:?} outside the rectangle");
        }
        assert!(on_arc >= 8, "the corners are curved: {on_arc} arc points");
        // the sharp corner itself is cut away
        assert!(pts.iter().all(|q| q.x + q.y > 5.0), "the corner at (0, 0) is rounded off");
        // radius 0 is the plain rectangle
        assert_eq!(MaskPath::rounded_rect(0.0, 0.0, 10.0, 5.0, 0.0), MaskPath::rect(0.0, 0.0, 10.0, 5.0));
    }

    #[test]
    fn rounding_clamps_to_half_an_edge_and_ignores_curves_and_hostile_input() {
        // a radius larger than the shape gives a stadium: fillets meet at the edges' midpoints
        let p = MaskPath::rounded_rect(0.0, 0.0, 100.0, 40.0, 1e9);
        assert_eq!(p.len(), 8);
        for v in &p.vertices {
            assert!(v.p.x.is_finite() && v.p.y.is_finite());
        }
        assert!(p.vertices.iter().any(|v| (v.p - Vec2::new(0.0, 20.0)).length() < 1e-9), "{:?}", p.vertices);
        // a triangle: the 60° turns get fillets tangent to both edges, within the triangle
        let tri = MaskPath::polygon(&[Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0), Vec2::new(50.0, 86.6)]);
        let r = tri.with_rounded_corners(10.0, &tri.roundable_corners());
        assert_eq!(r.len(), 6);
        assert!(r.vertices.iter().all(|v| v.p.y >= -1e-9));
        // an ellipse has no sharp corners; NaN / negative radii and bad indices change nothing harmful
        let e = MaskPath::ellipse(Vec2::new(50.0, 50.0), Vec2::new(30.0, 20.0));
        assert!(e.roundable_corners().is_empty());
        assert_eq!(e.with_rounded_corners(10.0, &[0, 1, 2, 3]), e);
        let sq = MaskPath::rect(0.0, 0.0, 10.0, 10.0);
        assert_eq!(sq.with_rounded_corners(5.0, &[usize::MAX, 99]), sq);
        for bad in [f64::NAN, f64::INFINITY, -3.0, 0.0] {
            let q = sq.with_rounded_corners(bad, &[0]);
            assert_eq!(q.len(), 5, "a listed corner always splits in two");
            assert!(q.vertices.iter().all(|v| v.is_corner() && v.p.x.is_finite()));
        }
        // a collinear vertex splits into two coincident corners, keeping vertex counts stable
        let line = MaskPath::polygon(&[Vec2::new(0.0, 0.0), Vec2::new(5.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(5.0, 5.0)]);
        let q = line.with_rounded_corners(2.0, &[1]);
        assert_eq!(q.len(), 5);
        assert_eq!(q.vertices[1].p, q.vertices[2].p);
    }
}
