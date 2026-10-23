//! Paint strokes (C3.3): vector brush strokes over a picture: colour, clone, reveal and erase.
//! The comp node kind, engine commands, GPU path and UI come later; this is the data model.
//! Pixels are produced by `filmcraft_render::paint`.
//!
//! **Space.** Like [`crate::mask::MaskPath`] and [`crate::roto`], stroke points, brush sizes and
//! clone offsets are in **frame pixels** (the node's full-resolution frame, x right, y down); the
//! renderer scales by its working pixel scale.
//!
//! **Tools.** [`PaintTool::Color`] paints [`PaintStroke::color`] (linear RGBA, straight alpha).
//! [`PaintTool::Clone`] copies the clone source (the node's own input unless the comp wiring
//! supplies another picture) from `clone_offset` pixels away: the painted pixel at `q` shows the
//! source at `q + clone_offset`, taken `clone_time_offset` later (a [`Tick`], so it is exact
//! whatever the frame rate; the comp wiring renders the source at `t + clone_time_offset`).
//! [`PaintTool::Reveal`] shows the node's **second input** through the stroke (the comp wiring
//! supplies it; without one the stroke does nothing). [`PaintTool::Erase`] restores the node's
//! input picture under the stroke, undoing strokes below it.
//!
//! **Time.** A stroke is drawn on one frame ([`StrokeDuration::SingleFrame`]) or held over a range
//! ([`StrokeDuration::Hold`], open-ended when `to` is `None`). Strokes apply in list order.
//!
//! Every number is hostile: [`Paint::sanitize`] caps counts and cleans non-finite or out-of-range
//! values; the renderer cleans what it reads as well.

use filmcraft_geom::Vec2;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::{Deserialize, Serialize};

/// Most strokes one paint node keeps.
pub const MAX_STROKES: usize = 10_000;
/// Most points one stroke keeps.
pub const MAX_STROKE_POINTS: usize = 100_000;
/// Most points all strokes of one node keep together.
pub const MAX_TOTAL_POINTS: usize = 1_000_000;
/// Largest brush diameter, frame pixels.
pub const MAX_BRUSH_SIZE: f64 = 4_096.0;
/// Spacing range (fraction of the brush size between dabs).
pub const MIN_SPACING: f64 = 0.01;
pub const MAX_SPACING: f64 = 10.0;
/// Largest |coordinate| / |clone offset|, frame pixels.
pub const MAX_COORD: f64 = 1e7;
/// Largest |colour channel| (linear light; HDR allowed up to half-float max).
pub const MAX_COLOR: f32 = 65_504.0;

/// What a stroke does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PaintTool {
    #[default]
    Color,
    Clone,
    Reveal,
    Erase,
}

/// A stroke point (frame pixels) with its pen pressure (0..1; 1 for a mouse).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PaintPoint {
    pub x: f64,
    pub y: f64,
    pub pressure: f64,
}

impl Default for PaintPoint {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, pressure: 1.0 }
    }
}

impl PaintPoint {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y, pressure: 1.0 }
    }
    /// `None` for a non-finite position; coordinates clamped to ± [`MAX_COORD`], pressure to
    /// 0..1 (non-finite → 1).
    pub fn cleaned(self) -> Option<Self> {
        if !self.x.is_finite() || !self.y.is_finite() {
            return None;
        }
        let pressure = if self.pressure.is_finite() { self.pressure.clamp(0.0, 1.0) } else { 1.0 };
        Some(Self { x: self.x.clamp(-MAX_COORD, MAX_COORD), y: self.y.clamp(-MAX_COORD, MAX_COORD), pressure })
    }
}

/// A round brush.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Brush {
    /// Diameter at full pressure, frame pixels.
    pub size: f64,
    /// 0 = soft (falls off from the centre), 1 = hard (antialiased edge).
    pub hardness: f64,
    /// 0..1, the most a stroke covers.
    pub opacity: f64,
    /// Distance between dabs as a fraction of the (pressure-scaled) size.
    pub spacing: f64,
}

impl Default for Brush {
    fn default() -> Self {
        Self { size: 20.0, hardness: 0.8, opacity: 1.0, spacing: 0.1 }
    }
}

impl Brush {
    /// Every value finite and within range (non-finite → the default's).
    pub fn cleaned(self) -> Self {
        let d = Self::default();
        let f = |v: f64, def: f64, lo: f64, hi: f64| if v.is_finite() { v.clamp(lo, hi) } else { def };
        Self {
            size: f(self.size, d.size, 0.0, MAX_BRUSH_SIZE),
            hardness: f(self.hardness, d.hardness, 0.0, 1.0),
            opacity: f(self.opacity, d.opacity, 0.0, 1.0),
            spacing: f(self.spacing, d.spacing, MIN_SPACING, MAX_SPACING),
        }
    }
}

/// When a stroke is visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum StrokeDuration {
    /// Only on the frame starting at `at`.
    SingleFrame { at: Tick },
    /// From `from` (inclusive) to `to` (exclusive); for ever when `to` is `None`.
    Hold {
        from: Tick,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Tick>,
    },
}

impl Default for StrokeDuration {
    fn default() -> Self {
        StrokeDuration::Hold { from: Tick::ZERO, to: None }
    }
}

impl StrokeDuration {
    /// Whether the stroke shows at `t`; `frame` is one frame's duration (a single-frame stroke
    /// shows for `at ≤ t < at + frame`; a non-positive `frame` counts as one tick).
    pub fn is_active(&self, t: Tick, frame: Tick) -> bool {
        match *self {
            StrokeDuration::SingleFrame { at } => t >= at && t < at + Tick(frame.0.max(1)),
            StrokeDuration::Hold { from, to } => t >= from && to.is_none_or(|to| t < to),
        }
    }
}

/// Longest |time| a stroke stores (± one day), so adding it to a time cannot overflow.
fn max_time() -> i64 {
    TICKS_PER_SECOND.saturating_mul(86_400)
}

fn clamp_time(t: Tick) -> Tick {
    let d = max_time();
    Tick(t.0.clamp(-d, d))
}

/// One brush stroke.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PaintStroke {
    pub tool: PaintTool,
    pub points: Vec<PaintPoint>,
    pub brush: Brush,
    /// Linear RGBA, straight (not premultiplied) alpha ([`PaintTool::Color`]).
    pub color: [f32; 4],
    /// Clone source offset (frame pixels): the painted pixel at `q` shows the source at
    /// `q + clone_offset` ([`PaintTool::Clone`]).
    pub clone_offset: Vec2,
    /// Clone source time offset ([`PaintTool::Clone`]): the source is taken this much later.
    pub clone_time_offset: Tick,
    pub duration: StrokeDuration,
}

impl Default for PaintStroke {
    fn default() -> Self {
        Self {
            tool: PaintTool::Color,
            points: Vec::new(),
            brush: Brush::default(),
            color: [1.0, 1.0, 1.0, 1.0],
            clone_offset: Vec2::ZERO,
            clone_time_offset: Tick::ZERO,
            duration: StrokeDuration::default(),
        }
    }
}

impl PaintStroke {
    pub fn new(tool: PaintTool, points: Vec<PaintPoint>, brush: Brush) -> Self {
        Self { tool, points, brush, ..Self::default() }
    }
    /// Bring a stroke within the caps: at most `max_points` points (non-finite ones dropped),
    /// brush cleaned, colour finite and clamped (alpha 0..1), offsets finite and bounded, times
    /// within ± a day, a `Hold` ending before it starts emptied (`to = from`).
    pub fn sanitize(&mut self, max_points: usize) {
        let pts = std::mem::take(&mut self.points);
        self.points = pts.into_iter().filter_map(PaintPoint::cleaned).take(max_points.min(MAX_STROKE_POINTS)).collect();
        self.brush = self.brush.cleaned();
        for (i, c) in self.color.iter_mut().enumerate() {
            let (lo, hi) = if i == 3 { (0.0, 1.0) } else { (-MAX_COLOR, MAX_COLOR) };
            *c = if c.is_finite() { c.clamp(lo, hi) } else { 0.0 };
        }
        let o = |v: f64| if v.is_finite() { v.clamp(-MAX_COORD, MAX_COORD) } else { 0.0 };
        self.clone_offset = Vec2::new(o(self.clone_offset.x), o(self.clone_offset.y));
        self.clone_time_offset = clamp_time(self.clone_time_offset);
        self.duration = match self.duration {
            StrokeDuration::SingleFrame { at } => StrokeDuration::SingleFrame { at: clamp_time(at) },
            StrokeDuration::Hold { from, to } => {
                let from = clamp_time(from);
                StrokeDuration::Hold { from, to: to.map(|t| clamp_time(t).max(from)) }
            }
        };
    }
}

/// The strokes of one paint node, applied in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Paint {
    pub strokes: Vec<PaintStroke>,
}

impl Paint {
    /// Brings a paint node from a file within the caps: at most [`MAX_STROKES`] strokes,
    /// [`MAX_STROKE_POINTS`] per stroke and [`MAX_TOTAL_POINTS`] in all (later strokes lose
    /// points first), every stroke cleaned ([`PaintStroke::sanitize`]).
    pub fn sanitize(&mut self) {
        self.strokes.truncate(MAX_STROKES);
        let mut budget = MAX_TOTAL_POINTS;
        for s in &mut self.strokes {
            s.sanitize(budget);
            budget = budget.saturating_sub(s.points.len());
        }
    }
    /// Total points of all strokes.
    pub fn total_points(&self) -> usize {
        self.strokes.iter().fold(0usize, |a, s| a.saturating_add(s.points.len()))
    }
    /// The strokes showing at `t` (`frame` = one frame's duration), in order.
    pub fn active_at(&self, t: Tick, frame: Tick) -> impl Iterator<Item = &PaintStroke> {
        self.strokes.iter().filter(move |s| s.duration.is_active(t, frame))
    }
    /// Insert `stroke` at `i` (≤ len); refused past the end or at [`MAX_STROKES`]. The stroke is
    /// sanitized against the remaining point budget.
    pub fn insert(&mut self, i: usize, mut stroke: PaintStroke) -> Result<(), &'static str> {
        if self.strokes.len() >= MAX_STROKES {
            return Err("a paint node keeps at most 10 000 strokes");
        }
        if i > self.strokes.len() {
            return Err("no such stroke position");
        }
        let budget = MAX_TOTAL_POINTS.saturating_sub(self.total_points());
        stroke.sanitize(budget);
        self.strokes.insert(i, stroke);
        Ok(())
    }
    /// Append `stroke` (see [`Paint::insert`]).
    pub fn push(&mut self, stroke: PaintStroke) -> Result<(), &'static str> {
        self.insert(self.strokes.len(), stroke)
    }
    /// Remove stroke `i`, if there is one.
    pub fn remove(&mut self, i: usize) -> Option<PaintStroke> {
        (i < self.strokes.len()).then(|| self.strokes.remove(i))
    }
    /// Move stroke `from` to position `to`; false (unchanged) when either is out of range.
    pub fn reorder(&mut self, from: usize, to: usize) -> bool {
        let n = self.strokes.len();
        if from >= n || to >= n {
            return false;
        }
        let s = self.strokes.remove(from);
        self.strokes.insert(to, s);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_hold_and_single_frame() {
        let f = Tick(100);
        let one = StrokeDuration::SingleFrame { at: Tick(1000) };
        assert!(!one.is_active(Tick(999), f) && one.is_active(Tick(1000), f) && one.is_active(Tick(1099), f) && !one.is_active(Tick(1100), f));
        // a zero frame still shows the exact tick
        assert!(one.is_active(Tick(1000), Tick(0)) && !one.is_active(Tick(1001), Tick(-5)));
        let hold = StrokeDuration::Hold { from: Tick(500), to: Some(Tick(800)) };
        assert!(!hold.is_active(Tick(499), f) && hold.is_active(Tick(500), f) && hold.is_active(Tick(799), f) && !hold.is_active(Tick(800), f));
        let open = StrokeDuration::Hold { from: Tick(500), to: None };
        assert!(open.is_active(Tick(i64::MAX), f) && !open.is_active(Tick(0), f));
        // extreme times saturate instead of overflowing
        assert!(!StrokeDuration::SingleFrame { at: Tick(i64::MAX) }.is_active(Tick(i64::MAX), Tick(i64::MAX)));
        assert!(StrokeDuration::SingleFrame { at: Tick(i64::MIN) }.is_active(Tick(i64::MIN), Tick(i64::MAX)));
        let p = Paint {
            strokes: vec![
                PaintStroke { duration: one, ..PaintStroke::default() },
                PaintStroke { duration: hold, ..PaintStroke::default() },
                PaintStroke { duration: open, ..PaintStroke::default() },
            ],
        };
        assert_eq!(p.active_at(Tick(1050), f).count(), 2);
        assert_eq!(p.active_at(Tick(600), f).count(), 2);
    }

    #[test]
    fn paint_serde_round_trip_is_camel_case() {
        let mut s = PaintStroke::new(PaintTool::Clone, vec![PaintPoint::new(1.0, 2.0), PaintPoint { x: 3.0, y: 4.0, pressure: 0.5 }], Brush::default());
        s.clone_offset = Vec2::new(-10.0, 5.0);
        s.clone_time_offset = Tick(-4_233_600_000);
        s.duration = StrokeDuration::SingleFrame { at: Tick(42) };
        let p = Paint { strokes: vec![s, PaintStroke::default()] };
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains(r#""cloneOffset""#) && j.contains(r#""cloneTimeOffset""#) && j.contains(r#""singleFrame""#) && j.contains(r#""clone""#), "{j}");
        let back: Paint = serde_json::from_str(&j).unwrap();
        assert_eq!(back, p);
        let d: Paint = serde_json::from_str(r#"{"strokes":[{"points":[{"x":1,"y":2}]}]}"#).unwrap();
        assert_eq!(d.strokes[0].points[0].pressure, 1.0);
        assert_eq!(d.strokes[0].brush, Brush::default());
    }

    #[test]
    fn sanitize_cleans_hostile_strokes() {
        let mut s = PaintStroke::new(
            PaintTool::Color,
            vec![PaintPoint::new(f64::NAN, 1.0), PaintPoint { x: 1e300, y: -1e300, pressure: f64::INFINITY }, PaintPoint { x: 1.0, y: 1.0, pressure: -3.0 }],
            Brush { size: -5.0, hardness: f64::NAN, opacity: 7.0, spacing: 0.0 },
        );
        s.color = [f32::NAN, f32::INFINITY, -1e30, 9.0];
        s.clone_offset = Vec2::new(f64::INFINITY, -1e300);
        s.clone_time_offset = Tick(i64::MIN);
        s.duration = StrokeDuration::Hold { from: Tick(100), to: Some(Tick(-100)) };
        let mut p = Paint { strokes: vec![s; MAX_STROKES + 5] };
        p.sanitize();
        assert_eq!(p.strokes.len(), MAX_STROKES);
        let s = &p.strokes[0];
        assert_eq!(s.points, vec![PaintPoint { x: MAX_COORD, y: -MAX_COORD, pressure: 1.0 }, PaintPoint { x: 1.0, y: 1.0, pressure: 0.0 }]);
        assert_eq!(s.brush, Brush { size: 0.0, hardness: 0.8, opacity: 1.0, spacing: MIN_SPACING });
        assert_eq!(s.color, [0.0, 0.0, -MAX_COLOR, 1.0]);
        assert_eq!(s.clone_offset, Vec2::new(0.0, -MAX_COORD));
        assert_eq!(s.clone_time_offset, Tick(-max_time()));
        assert_eq!(s.duration, StrokeDuration::Hold { from: Tick(100), to: Some(Tick(100)) });
        // the total point budget holds
        let big = PaintStroke::new(PaintTool::Color, vec![PaintPoint::new(0.0, 0.0); MAX_STROKE_POINTS + 10], Brush::default());
        let mut p = Paint { strokes: vec![big; 15] };
        p.sanitize();
        assert!(p.strokes.iter().all(|s| s.points.len() <= MAX_STROKE_POINTS));
        assert_eq!(p.total_points(), MAX_TOTAL_POINTS);
    }

    #[test]
    fn stroke_list_edits_are_bounds_checked() {
        let mut p = Paint::default();
        assert!(p.insert(1, PaintStroke::default()).is_err());
        p.push(PaintStroke { tool: PaintTool::Erase, ..PaintStroke::default() }).unwrap();
        p.insert(0, PaintStroke { tool: PaintTool::Reveal, ..PaintStroke::default() }).unwrap();
        assert_eq!(p.strokes[0].tool, PaintTool::Reveal);
        assert!(p.reorder(0, 1) && p.strokes[1].tool == PaintTool::Reveal);
        assert!(!p.reorder(2, 0) && !p.reorder(0, usize::MAX));
        assert!(p.remove(2).is_none());
        assert_eq!(p.remove(0).map(|s| s.tool), Some(PaintTool::Erase));
        // inserted strokes are cleaned
        p.push(PaintStroke::new(PaintTool::Color, vec![PaintPoint::new(f64::NAN, 0.0)], Brush { size: f64::NAN, ..Brush::default() })).unwrap();
        assert!(p.strokes[1].points.is_empty() && p.strokes[1].brush.size == 20.0);
    }
}
