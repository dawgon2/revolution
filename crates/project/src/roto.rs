//! Roto shapes (C3.3): animated Bézier or B-spline outlines with a per-point feather, combined
//! into one matte. The comp node kind, engine commands, GPU path and UI come later; this is the
//! data model. Coverage is computed by `filmcraft_render::roto`.
//!
//! **Space.** Like [`MaskPath`], every coordinate and distance is in **frame pixels** (the comp's
//! or clip's full-resolution pixels, x right, y down); the renderer scales by its working pixel
//! scale.
//!
//! **One keyframe = one whole shape state.** A shape's outline is one [`Param`] holding a plain
//! [`ParamValue::Path`]: one [`MaskVertex`] per point (position, Bézier tangents and the
//! per-point `feather_in` / `feather_out`), with the path's `closed` flag. The existing keyframe
//! machinery (temporal eases, hold, the keyframe lane) animates it, interpolating every vertex
//! value including the feathers when the point counts match and holding otherwise; preset
//! scaling ([`MaskPath::transformed`]) moves the points and scales the feathers.
//!
//! **Kinds.** A [`RotoKind::Bezier`] shape is a cubic Bézier spline through its points (tangents
//! relative to the point, zero = corner). A [`RotoKind::BSpline`] shape is a uniform cubic
//! B-spline whose control points are the stored points (tangents ignored); an open one clamps its
//! ends (end points tripled) so it starts and ends on its first and last point. An open shape is
//! filled as if a straight edge closed it.
//!
//! **Feather.** Each point has an inner and an outer feather distance (≥ 0): coverage is 1 at the
//! inner distance inside the outline and 0 at the outer distance outside, with the widths
//! interpolated along the outline between points. The global *Feather* param adds half of itself
//! to each side (a symmetric falloff like a mask's), *Expansion* grows (> 0) or shrinks the shape,
//! *Opacity* (%) scales it, *Inverted* flips it and [`RotoMode`] combines it with the shapes above.
//!
//! Every number is hostile: [`Roto::sanitize`] caps counts and cleans non-finite or out-of-range
//! values; the renderer also cleans what it reads ([`RotoShape::path_at`]), so an unsanitized
//! shape cannot crash it.

use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

use crate::keyframe::{Param, ParamValue};
use crate::mask::{MAX_POINT_FEATHER, MaskPath};

/// Most shapes one roto keeps.
pub const MAX_SHAPES: usize = 256;
/// Most points one shape keeps.
pub const MAX_POINTS: usize = 4_096;
/// Most keyframes one roto parameter keeps.
pub const MAX_KEYFRAMES: usize = 4_096;
/// Largest feather distance (per point inner / outer, and the global feather), frame pixels.
pub const MAX_FEATHER: f64 = MAX_POINT_FEATHER;
/// Largest |expansion|, frame pixels.
pub const MAX_EXPANSION: f64 = 10_000.0;
/// Longest shape name in characters.
pub const MAX_NAME: usize = 64;
/// Longest tracker id / output name of a [`TrackLink`] in characters.
pub const MAX_LINK_ID: usize = 128;

/// The spline a shape's points describe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RotoKind {
    /// Cubic Bézier through the points, with per-point tangents.
    #[default]
    Bezier,
    /// Uniform cubic B-spline; the points are its control points.
    BSpline,
}

/// How a shape combines with the result of the shapes above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RotoMode {
    /// Union: `a + m − a·m`.
    #[default]
    Add,
    /// `a · (1 − m)`.
    Subtract,
    /// `a · m`.
    Intersect,
    /// `m` (the shapes above are discarded).
    Replace,
}

impl RotoMode {
    pub const ALL: [RotoMode; 4] = [RotoMode::Add, RotoMode::Subtract, RotoMode::Intersect, RotoMode::Replace];
    pub fn label(self) -> &'static str {
        match self {
            RotoMode::Add => "Add",
            RotoMode::Subtract => "Subtract",
            RotoMode::Intersect => "Intersect",
            RotoMode::Replace => "Replace",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        Self::ALL.into_iter().find(|m| m.label().to_ascii_lowercase() == s)
    }
    /// Combine accumulated coverage `a` with this shape's coverage `m`.
    pub fn combine(self, a: f32, m: f32) -> f32 {
        match self {
            RotoMode::Add => a + m - a * m,
            RotoMode::Subtract => a * (1.0 - m),
            RotoMode::Intersect => a * m,
            RotoMode::Replace => m,
        }
    }
    /// Starting value when this is the first enabled shape: Subtract / Intersect start from the
    /// full frame, the others from nothing.
    pub fn start(self) -> f32 {
        match self {
            RotoMode::Subtract | RotoMode::Intersect => 1.0,
            RotoMode::Add | RotoMode::Replace => 0.0,
        }
    }
}

/// A tracker output a shape follows (track-assisted roto, fork C3.7): the renderer draws the
/// shape through that Tracker / Planar Tracker node's measured motion from its reference frame
/// (`filmcraft_render::comp::track::linked_roto`); a link to a node that is gone is ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrackLink {
    /// Id of the tracker node (in the same comp).
    pub tracker: String,
    /// Which of its outputs: "transform" (or empty) = the node's whole solve, else a Tracker
    /// point's name (that point's translation only).
    pub output: String,
}

/// One roto shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RotoShape {
    pub name: String,
    pub kind: RotoKind,
    /// The outline, keyframed as a whole: a [`ParamValue::Path`], one vertex per point (with its
    /// feather); `closed` is the path's.
    pub shape: Param,
    /// Global feather, frame pixels (≥ 0), half added to each side of every point's feather.
    pub feather: Param,
    /// 0..100 %.
    pub opacity: Param,
    /// Frame pixels (negative shrinks).
    pub expansion: Param,
    pub mode: RotoMode,
    pub inverted: bool,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_link: Option<TrackLink>,
}

/// Roto shape parameter ids (keyframe commands address them by these).
pub const ROTO_PARAMS: [&str; 4] = ["shape", "feather", "opacity", "expansion"];

impl Default for RotoShape {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: RotoKind::Bezier,
            shape: Param::new(ParamValue::Path(MaskPath { vertices: Vec::new(), closed: true })),
            feather: Param::new(ParamValue::Float(0.0)),
            opacity: Param::new(ParamValue::Float(100.0)),
            expansion: Param::new(ParamValue::Float(0.0)),
            mode: RotoMode::Add,
            inverted: false,
            enabled: true,
            track_link: None,
        }
    }
}

impl RotoShape {
    /// An enabled shape of `kind` with outline `path` (static, not keyframed).
    pub fn new(name: impl Into<String>, kind: RotoKind, path: MaskPath) -> Self {
        Self { name: name.into(), kind, shape: Param::new(ParamValue::Path(path)), ..Self::default() }
    }
    pub fn param(&self, id: &str) -> Option<&Param> {
        match id {
            "shape" => Some(&self.shape),
            "feather" => Some(&self.feather),
            "opacity" => Some(&self.opacity),
            "expansion" => Some(&self.expansion),
            _ => None,
        }
    }
    pub fn param_mut(&mut self, id: &str) -> Option<&mut Param> {
        match id {
            "shape" => Some(&mut self.shape),
            "feather" => Some(&mut self.feather),
            "opacity" => Some(&mut self.opacity),
            "expansion" => Some(&mut self.expansion),
            _ => None,
        }
    }
    pub fn is_animated(&self) -> bool {
        ROTO_PARAMS.iter().any(|p| self.param(p).is_some_and(Param::is_animated))
    }
    /// The outline at `t` (interpolated between keyframes when their counts match), cleaned and
    /// capped to [`MAX_POINTS`] (an empty path when the value is not a path).
    pub fn path_at(&self, t: Tick) -> MaskPath {
        match self.shape.value_at(t) {
            ParamValue::Path(p) => p.cleaned(MAX_POINTS),
            _ => MaskPath { vertices: Vec::new(), closed: true },
        }
    }
    /// Set the outline at `t` (the static value, or a keyframe when the shape is animated).
    pub fn set_path_at(&mut self, t: Tick, path: MaskPath) {
        self.shape.set_at(t, ParamValue::Path(path));
    }
    /// Add or replace a shape keyframe at `t` (the shape becomes animated).
    pub fn put_path_keyframe(&mut self, t: Tick, path: MaskPath) {
        self.shape.put_keyframe(t, ParamValue::Path(path));
    }
    /// Global feather at `t` (frame pixels, finite, 0 ..= [`MAX_FEATHER`]).
    pub fn feather_at(&self, t: Tick) -> f64 {
        clean_f64(self.feather.f64_at(t), 0.0).clamp(0.0, MAX_FEATHER)
    }
    /// Opacity at `t` as 0..1.
    pub fn opacity_at(&self, t: Tick) -> f64 {
        (clean_f64(self.opacity.f64_at(t), 100.0) / 100.0).clamp(0.0, 1.0)
    }
    /// Expansion at `t` (frame pixels, finite, within ± [`MAX_EXPANSION`]).
    pub fn expansion_at(&self, t: Tick) -> f64 {
        clean_f64(self.expansion.f64_at(t), 0.0).clamp(-MAX_EXPANSION, MAX_EXPANSION)
    }

    /// Bring a shape from a file within the caps (see [`Roto::sanitize`]).
    pub fn sanitize(&mut self) {
        if self.name.chars().count() > MAX_NAME {
            self.name = self.name.chars().take(MAX_NAME).collect();
        }
        sanitize_param(&mut self.shape, |v| match v {
            ParamValue::Path(p) => ParamValue::Path(p.cleaned(MAX_POINTS)),
            _ => ParamValue::Path(MaskPath { vertices: Vec::new(), closed: true }),
        });
        sanitize_float(&mut self.feather, 0.0, 0.0, MAX_FEATHER);
        sanitize_float(&mut self.opacity, 100.0, 0.0, 100.0);
        sanitize_float(&mut self.expansion, 0.0, -MAX_EXPANSION, MAX_EXPANSION);
        if let Some(l) = &mut self.track_link {
            for s in [&mut l.tracker, &mut l.output] {
                if s.chars().count() > MAX_LINK_ID {
                    *s = s.chars().take(MAX_LINK_ID).collect();
                }
            }
        }
    }
}

fn clean_f64(v: f64, default: f64) -> f64 {
    if v.is_finite() { v } else { default }
}

/// Clean a param's keyframe list (sorted, one keyframe per time, capped to the earliest
/// [`MAX_KEYFRAMES`], finite influences) and map every value (static and keyframed) through `f`.
pub(crate) fn sanitize_param(p: &mut Param, f: impl Fn(ParamValue) -> ParamValue) {
    p.keyframes.sort_by_key(|k| k.time);
    p.keyframes.dedup_by_key(|k| k.time);
    p.keyframes.truncate(MAX_KEYFRAMES);
    for k in &mut p.keyframes {
        for i in [&mut k.in_influence, &mut k.out_influence] {
            *i = if i.is_finite() { i.clamp(0.0, 1.0) } else { 1.0 / 3.0 };
        }
    }
    p.map_values(f);
}

/// A scalar param: every value a finite Float in `min ..= max` (anything else → `default`).
pub(crate) fn sanitize_float(p: &mut Param, default: f64, min: f64, max: f64) {
    sanitize_param(p, |v| match v {
        ParamValue::Float(x) if x.is_finite() => ParamValue::Float(x.clamp(min, max)),
        _ => ParamValue::Float(default),
    });
}

/// The shapes of one roto node, top to bottom (the first is combined first).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Roto {
    pub shapes: Vec<RotoShape>,
}

impl Roto {
    /// Brings a roto from a file within the caps: at most [`MAX_SHAPES`] shapes, names shortened,
    /// every keyframe list sorted / deduplicated / capped, every outline at most [`MAX_POINTS`]
    /// vertices with finite coordinates (± [`crate::mask::MAX_VERTEX_COORD`]) and feathers in
    /// 0 ..= [`MAX_FEATHER`], scalar params finite Floats in range (wrong kinds reset).
    pub fn sanitize(&mut self) {
        self.shapes.truncate(MAX_SHAPES);
        for s in &mut self.shapes {
            s.sanitize();
        }
    }
    /// Insert `shape` at `i` (≤ len); refused past the end or at [`MAX_SHAPES`].
    pub fn insert(&mut self, i: usize, shape: RotoShape) -> Result<(), &'static str> {
        if self.shapes.len() >= MAX_SHAPES {
            return Err("a roto keeps at most 256 shapes");
        }
        if i > self.shapes.len() {
            return Err("no such shape position");
        }
        self.shapes.insert(i, shape);
        Ok(())
    }
    /// Remove shape `i`, if there is one.
    pub fn remove(&mut self, i: usize) -> Option<RotoShape> {
        (i < self.shapes.len()).then(|| self.shapes.remove(i))
    }
    /// Move shape `from` to position `to`; false (unchanged) when either is out of range.
    pub fn reorder(&mut self, from: usize, to: usize) -> bool {
        let n = self.shapes.len();
        if from >= n || to >= n {
            return false;
        }
        let s = self.shapes.remove(from);
        self.shapes.insert(to, s);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyframe::Keyframe;
    use crate::mask::{MAX_VERTEX_COORD, MaskVertex};
    use filmcraft_geom::{Affine, Vec2};

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> MaskPath {
        MaskPath::rect(x0, y0, x1, y1)
    }

    #[test]
    fn shape_keyframes_interpolate_points_and_feather_together() {
        let a = square(0.0, 0.0, 10.0, 10.0);
        let mut b = a.transformed(&Affine::translate(100.0, 20.0));
        b.vertices.iter_mut().for_each(|v| *v = v.with_feather(4.0, 8.0));
        let mut s = RotoShape::new("s", RotoKind::BSpline, a.clone());
        s.put_path_keyframe(Tick(0), a);
        s.put_path_keyframe(Tick(100), b);
        let mid = s.path_at(Tick(50));
        assert_eq!(mid.vertices[0].p, Vec2::new(50.0, 10.0));
        assert_eq!((mid.vertices[2].feather_in, mid.vertices[2].feather_out), (2.0, 4.0));
        // a different point count holds the earlier state
        s.put_path_keyframe(Tick(200), MaskPath::polygon(&[Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)]));
        assert_eq!(s.path_at(Tick(150)).len(), 4);
        assert!(s.is_animated());
    }

    /// Effect presets scale every Path param with `transformed(Affine::scale(sx, sy))`
    /// (engine `presets::scale_value`): the points scale per axis, the feathers by the mean scale.
    #[test]
    fn preset_style_scaling_keeps_points_and_feather() {
        let mut p = square(10.0, 10.0, 20.0, 20.0);
        p.vertices[1] = p.vertices[1].with_feather(3.0, 6.0);
        let mut s = RotoShape::new("s", RotoKind::Bezier, p.clone());
        s.put_path_keyframe(Tick(0), p.clone());
        s.put_path_keyframe(Tick(10), p.transformed(&Affine::translate(5.0, 0.0)));
        let scale = Affine::scale(2.0, 8.0);
        s.shape.map_values(|v| match v {
            ParamValue::Path(path) => ParamValue::Path(path.transformed(&scale)),
            other => other,
        });
        for (t, dx) in [(0, 0.0), (10, 5.0)] {
            let q = s.path_at(Tick(t));
            assert_eq!(q.len(), 4);
            assert_eq!(q.vertices[1].p, Vec2::new((20.0 + dx) * 2.0, 80.0));
            assert_eq!((q.vertices[1].feather_in, q.vertices[1].feather_out), (12.0, 24.0));
            assert_eq!((q.vertices[0].feather_in, q.vertices[0].feather_out), (0.0, 0.0));
            assert!(q.closed);
        }
    }

    #[test]
    fn roto_serde_round_trip_is_camel_case() {
        let mut p = square(1.0, 2.0, 3.0, 4.0);
        p.vertices[0] = p.vertices[0].with_feather(1.0, 2.0);
        p.closed = false;
        let mut s = RotoShape::new("s", RotoKind::BSpline, p);
        s.mode = RotoMode::Subtract;
        s.inverted = true;
        s.track_link = Some(TrackLink { tracker: "7".into(), output: "track1".into() });
        s.feather.put_keyframe(Tick(10), ParamValue::Float(4.0));
        let r = Roto { shapes: vec![s, RotoShape::default()] };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains(r#""trackLink""#) && j.contains(r#""bSpline""#) && j.contains(r#""subtract""#) && j.contains(r#""feather_out":2.0"#), "{j}");
        let back: Roto = serde_json::from_str(&j).unwrap();
        assert_eq!(back, r);
        assert!(!back.shapes[0].path_at(Tick(0)).closed);
        // everything defaults
        let d: Roto = serde_json::from_str(r#"{"shapes":[{}]}"#).unwrap();
        assert_eq!(d.shapes, vec![RotoShape::default()]);
        assert!(d.shapes[0].enabled && d.shapes[0].path_at(Tick(0)).closed);
        assert!(RotoMode::from_name("replace") == Some(RotoMode::Replace));
    }

    #[test]
    fn sanitize_cleans_hostile_shapes() {
        let mut bad = square(0.0, 0.0, 10.0, 10.0);
        bad.vertices[0].p.x = f64::NAN;
        bad.vertices[1].t_in.y = f64::INFINITY;
        bad.vertices[2].p.x = 1e300;
        bad.vertices[3] = bad.vertices[3].with_feather(-5.0, f64::NAN);
        let mut s = RotoShape::new("x".repeat(500), RotoKind::Bezier, bad);
        s.shape.keyframes = vec![Keyframe::new(Tick(5), ParamValue::Float(3.0)), Keyframe::new(Tick(5), ParamValue::Bool(true))];
        s.shape.keyframes[0].in_influence = f64::NAN;
        s.feather.value = ParamValue::Float(f64::INFINITY);
        s.opacity.value = ParamValue::Text("loud".into());
        s.expansion.keyframes = (0..10_000).rev().map(|i| Keyframe::new(Tick(i), ParamValue::Float(-1e300))).collect();
        let mut r = Roto { shapes: vec![s; MAX_SHAPES + 3] };
        r.sanitize();
        assert_eq!(r.shapes.len(), MAX_SHAPES);
        let s = &r.shapes[0];
        assert_eq!(s.name.chars().count(), MAX_NAME);
        let p = s.shape.value.as_path().unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(p.vertices[0].p.x, 0.0);
        assert_eq!(p.vertices[1].t_in.y, 0.0);
        assert_eq!(p.vertices[2].p.x, MAX_VERTEX_COORD);
        assert_eq!((p.vertices[3].feather_in, p.vertices[3].feather_out), (0.0, 0.0));
        assert_eq!(s.shape.keyframes.len(), 1);
        assert!(s.shape.keyframes[0].value.as_path().is_some_and(MaskPath::is_empty));
        assert_eq!(s.shape.keyframes[0].in_influence, 1.0 / 3.0);
        assert_eq!(s.feather.value, ParamValue::Float(0.0));
        assert_eq!(s.opacity.value, ParamValue::Float(100.0));
        // sorted first, then the earliest keyframes kept
        assert_eq!(s.expansion.keyframes.len(), MAX_KEYFRAMES);
        assert_eq!(s.expansion.keyframes.first().map(|k| k.time), Some(Tick(0)));
        assert!(s.expansion.keyframes.windows(2).all(|w| w[0].time < w[1].time));
        assert_eq!(s.expansion_at(Tick(3)), -MAX_EXPANSION);
        // accessors are safe even without sanitize
        let mut raw = RotoShape::default();
        raw.opacity.value = ParamValue::Float(f64::NAN);
        raw.shape.value = ParamValue::Bool(false);
        assert_eq!((raw.opacity_at(Tick(0)), raw.path_at(Tick(0)).len()), (1.0, 0));
        // a huge point list is capped
        let many = MaskPath { vertices: (0..MAX_POINTS + 50).map(|i| MaskVertex::corner(Vec2::new(i as f64, 0.0))).collect(), closed: true };
        assert_eq!(RotoShape::new("m", RotoKind::Bezier, many).path_at(Tick(0)).len(), MAX_POINTS);
    }

    #[test]
    fn shape_list_edits_are_bounds_checked() {
        let mut r = Roto::default();
        assert!(r.insert(1, RotoShape::default()).is_err());
        r.insert(0, RotoShape { name: "a".into(), ..RotoShape::default() }).unwrap();
        r.insert(1, RotoShape { name: "b".into(), ..RotoShape::default() }).unwrap();
        assert!(r.reorder(0, 1) && r.shapes[0].name == "b");
        assert!(!r.reorder(0, 2) && !r.reorder(9, 0));
        assert!(r.remove(5).is_none());
        assert_eq!(r.remove(0).map(|s| s.name), Some("b".into()));
    }
}
