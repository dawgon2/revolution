//! Magic Mask on a grade node (fork GRADE-PLAN G3): the strokes that tell the segmentation
//! model what to isolate, and how to refine its matte.
//!
//! Only the prompts are stored. The per-frame mattes are a cache the app regenerates from them
//! (so a project stays small and loads without the model). A stroke is pinned to the source time
//! it was drawn on; propagation runs forward and back from each stroke, and a new stroke only
//! changes the frames up to its neighbours.
//!
//! Every number is hostile: loading cleans it ([`MagicMask::cleaned`]): non-finite points are
//! dropped, coordinates clamped to the frame, counts capped, refine values clamped.

use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

/// Most strokes a node keeps.
pub const MAX_STROKES: usize = 256;
/// Most points one stroke keeps.
pub const MAX_STROKE_POINTS: usize = 64;
/// Grow / shrink limit, in pixels of a 1080-line frame (scaled with the frame).
pub const MAX_GROW: f32 = 100.0;
/// Soften limit (blur radius), in pixels of a 1080-line frame.
pub const MAX_SOFTEN: f32 = 100.0;

/// What the model is asked to find.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MagicMaskMode {
    /// Whatever the strokes cover (SAM-style).
    #[default]
    Object,
    /// A whole person (the strokes pick which one).
    Person,
}

/// A point of a stroke, normalised to the frame (0..1, x right, y down).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    /// `true`: part of the subject; `false`: an "exclude" stroke.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub positive: bool,
}

/// One stroke (or click: one point), drawn on the frame at `time` (source time).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaskStroke {
    pub time: Tick,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<StrokePoint>,
    /// A box dragged around the subject, `[x0, y0, x1, y1]` normalised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<[f32; 4]>,
}

/// How the model's matte is refined before it limits the node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskRefine {
    /// Grow (> 0) or shrink (< 0) the matte, pixels of a 1080-line frame.
    #[serde(default)]
    pub grow: f32,
    /// Blur the matte's edge, pixels of a 1080-line frame.
    #[serde(default)]
    pub soften: f32,
    /// Matte values below this (0..1) become 0 ("clean black").
    #[serde(default)]
    pub clean_black: f32,
    /// Matte values above 1 − this (0..1) become 1 ("clean white").
    #[serde(default)]
    pub clean_white: f32,
    /// Use the outside of the subject instead.
    #[serde(default)]
    pub invert: bool,
}

impl MaskRefine {
    /// Every value finite and in range (NaN → 0).
    pub fn cleaned(self) -> Self {
        let c = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { 0.0 };
        Self {
            grow: c(self.grow, -MAX_GROW, MAX_GROW),
            soften: c(self.soften, 0.0, MAX_SOFTEN),
            clean_black: c(self.clean_black, 0.0, 1.0),
            clean_white: c(self.clean_white, 0.0, 1.0),
            invert: self.invert,
        }
    }
}

/// A node's Magic Mask.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MagicMask {
    #[serde(default)]
    pub mode: MagicMaskMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strokes: Vec<MaskStroke>,
    #[serde(default)]
    pub refine: MaskRefine,
}

impl MagicMask {
    /// The mask with hostile values removed: non-finite points dropped, coordinates clamped,
    /// strokes without a point or box dropped, at most [`MAX_STROKES`] strokes of at most
    /// [`MAX_STROKE_POINTS`] points, sorted by time (stable), refine clamped.
    pub fn cleaned(self) -> Self {
        let mut strokes: Vec<MaskStroke> = self
            .strokes
            .into_iter()
            .filter_map(|s| {
                let points: Vec<StrokePoint> = s
                    .points
                    .into_iter()
                    .filter(|p| p.x.is_finite() && p.y.is_finite())
                    .take(MAX_STROKE_POINTS)
                    .map(|p| StrokePoint { x: p.x.clamp(0.0, 1.0), y: p.y.clamp(0.0, 1.0), positive: p.positive })
                    .collect();
                let rect = s.rect.and_then(clean_rect);
                (!points.is_empty() || rect.is_some()).then_some(MaskStroke { time: s.time, points, rect })
            })
            .take(MAX_STROKES)
            .collect();
        strokes.sort_by_key(|s| s.time);
        Self { mode: self.mode, strokes, refine: self.refine.cleaned() }
    }

    /// The strokes drawn on the frame at `time`.
    pub fn strokes_at(&self, time: Tick) -> impl Iterator<Item = &MaskStroke> {
        self.strokes.iter().filter(move |s| s.time == time)
    }
}

/// A box with finite corners, put in order and clamped to the frame.
pub fn clean_rect(r: [f32; 4]) -> Option<[f32; 4]> {
    if r.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let c = |v: f32| v.clamp(0.0, 1.0);
    Some([c(r[0].min(r[2])), c(r[1].min(r[3])), c(r[0].max(r[2])), c(r[1].max(r[3]))])
}

/// Deserialize a node's Magic Mask, cleaned. A malformed one (a stroke without a time, an
/// unknown mode, …) loads as no Magic Mask rather than failing the whole project.
pub(crate) fn de_magic<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<MagicMask>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value::<MagicMask>(v).ok()).map(MagicMask::cleaned))
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::{Grade, GradeNode};

    #[test]
    fn old_nodes_have_no_magic_mask_and_it_round_trips() {
        let g: Grade = serde_json::from_str(r#"{"nodes":[{"id":1}]}"#).unwrap();
        assert_eq!(g.nodes[0].magic_mask, None);
        assert!(!serde_json::to_string(&g).unwrap().contains("magicMask"));
        let mut n = GradeNode::new(2);
        n.magic_mask = Some(MagicMask {
            mode: MagicMaskMode::Person,
            strokes: vec![MaskStroke { time: Tick(48), points: vec![StrokePoint { x: 0.5, y: 0.25, positive: false }], rect: Some([0.1, 0.2, 0.3, 0.4]) }],
            refine: MaskRefine { grow: 3.0, soften: 2.0, clean_black: 0.1, clean_white: 0.2, invert: true },
        });
        let json = serde_json::to_string(&n).unwrap();
        assert!(json.contains("\"magicMask\""), "{json}");
        assert_eq!(serde_json::from_str::<GradeNode>(&json).unwrap(), n);
    }

    #[test]
    fn hostile_magic_masks_load_cleaned() {
        let mut pts = String::from("[");
        for i in 0..200 {
            pts.push_str(&format!("{{\"x\":{},\"y\":0.5}},", i as f32 / 100.0));
        }
        pts.push_str("{\"x\":-5,\"y\":9}]");
        let json = format!(
            r#"{{"id":1,"magicMask":{{"mode":"person","strokes":[
                {{"time":90,"points":{pts}}},
                {{"time":10,"rect":[0.9,0.8,-1,0.1]}},
                {{"time":5}},
                {{"time":7,"rect":[1e40,0,1,1]}}
            ],"refine":{{"grow":1e9,"soften":-3,"cleanBlack":2,"cleanWhite":-1,"invert":true}}}}}}"#
        );
        let n: GradeNode = serde_json::from_str(&json).unwrap();
        let m = n.magic_mask.unwrap();
        assert_eq!(m.mode, MagicMaskMode::Person);
        assert_eq!(m.strokes.len(), 2, "empty and non-finite-only strokes dropped");
        assert_eq!(m.strokes[0].time, Tick(10));
        assert_eq!(m.strokes[0].rect, Some([0.0, 0.1, 0.9, 0.8]));
        assert_eq!(m.strokes[1].points.len(), MAX_STROKE_POINTS);
        assert!(m.strokes[1].points.iter().all(|p| (0.0..=1.0).contains(&p.x) && p.positive));
        assert_eq!(m.refine, MaskRefine { grow: MAX_GROW, soften: 0.0, clean_black: 1.0, clean_white: 0.0, invert: true });
        assert_eq!(m.strokes_at(Tick(10)).count(), 1);

        let many = format!(r#"{{"id":1,"magicMask":{{"strokes":[{}]}}}}"#, vec![r#"{"time":1,"rect":[0,0,1,1]}"#; 1000].join(","));
        let n: GradeNode = serde_json::from_str(&many).unwrap();
        assert_eq!(n.magic_mask.unwrap().strokes.len(), MAX_STROKES);
        assert_eq!(MaskRefine { grow: f32::NAN, ..MaskRefine::default() }.cleaned().grow, 0.0);

        // A malformed Magic Mask drops itself, not the project.
        for bad in [r#"{"strokes":[{"points":[]}]}"#, r#"{"mode":"cat"}"#, r#"{"strokes":[{"time":1,"points":[{"x":"a","y":0}]}]}"#, "7"] {
            let n: GradeNode = serde_json::from_str(&format!(r#"{{"id":3,"magicMask":{bad},"label":"kept"}}"#)).unwrap();
            assert_eq!((n.magic_mask, n.label.as_str()), (None, "kept"), "{bad}");
        }
    }
}
