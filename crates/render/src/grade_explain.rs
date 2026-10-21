//! The node explainer (GRADE-PLAN §3 G2.10): measured facts first.
//!
//! [`explain_node`] reads a grade node's controls at a time (which wheels moved and where, curve
//! bends, qualifier and windows) and measures what the node really did by comparing its input and
//! output frames with the [`crate::color_match`] statistics: Oklab, the tonal-range weights
//! (shadows / midtones / highlights by the input's lightness) and chroma. The working space's
//! linear RGB goes to CIE XYZ through its primaries and then to Oklab (Ottosson's M1 on XYZ), so a
//! wide-gamut sequence reads the same as Rec. 709. The measurement is restricted to the pixels the
//! node changes by more than a just-noticeable difference ([`JND`]), so a qualified or windowed
//! node is described inside its matte. Large frames are measured on a deterministic grid of at
//! most [`MAX_SAMPLES`] pixels.
//!
//! Per tonal range it reports exposure in stops (mean luminance ratio), warmth and tint (the shift
//! of neutral in Oklab chromaticity a/L, b/L, which an exposure change leaves alone) and
//! saturation (the ratio of the chromaticity spreads about their means, which a cast leaves
//! alone). [`NodeFacts::sentence`] turns the facts into one plain English template sentence;
//! [`NodeFacts::summary`] into a short strip label; [`explain_grade`] joins the nodes' sentences.
//! [`change_overlay`] draws where and how strongly a node acted, and [`node_state_key`] keys a
//! cache of explanations by node state.

use filmcraft_color::spaces::to_f32;
use filmcraft_color::{ColorPipeline, WorkingSpace};
use filmcraft_project::grade::{GradeNode, MAX_WINDOWS, NODE_PARAMS, NodeParamKind};
use filmcraft_project::keyframe::ParamValue;
use filmcraft_project::mask::{MASK_PARAMS, Mask, MaskMode};
use filmcraft_time::Tick;
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize};

mod branches;
#[doc(hidden)]
pub mod cases;
mod check;

pub use branches::explain_branches;
pub use check::{
    FactMismatch, GRAMMAR_DOC, NOOP_PHRASES, VOCABULARY, check_explanation, check_explanation_labeled, check_grammar, check_shape, check_vocabulary,
};

use crate::Image;
use crate::color_match::tonal_weights;
use crate::grade::GradeCtx;

/// A just-noticeable colour difference in Oklab (ΔE_OK ≈ 0.01 ~ ΔE2000 1): pixels the node
/// changes by more than this count as changed.
pub const JND: f32 = 0.01;

/// The Oklab difference [`change_overlay`] draws fully saturated.
pub const STRONG_CHANGE: f32 = 0.12;

/// The most pixels the statistics sample (a regular grid with one stride on both axes).
pub const MAX_SAMPLES: usize = 1_000_000;

/// Any change at all (for the matte coverage of a qualified / windowed node).
const ANY_CHANGE: f32 = 0.001;

/// A tonal range counts as touched when at least this share of its pixels changed …
const RANGE_MIN_SHARE: f32 = 0.005;
/// … and those pixels cover at least this much of the frame.
const RANGE_MIN_FRAME: f32 = 0.001;

/// Chromaticity (a/L, b/L) shifts: "slightly", plain, "much".
const CAST_STEPS: [f32; 3] = [0.01, 0.03, 0.08];

/// Saturation changes smaller than this (percent) aren't reported.
const SAT_MIN_PCT: i64 = 10;

/// Saturation is only measured where chromaticity varies at least this much (standard deviation):
/// on a neutral area a scale of chroma has nothing to act on.
const SAT_MIN_SPREAD: f64 = 0.02;

/// Pixels darker than this Oklab lightness don't enter the chromaticity statistics (a/L is
/// unstable).
const CHROMA_MIN_L: f32 = 0.05;

/// "affects the whole frame" from this coverage on (without a matte).
const WHOLE_FRAME: f32 = 0.9;

const RANGE_NAMES: [&str; 3] = ["shadows", "midtones", "highlights"];

// ---- colour ---------------------------------------------------------------------------------

/// A working space's linear RGB → XYZ → Oklab, and its luminance.
struct Space {
    to_xyz: [[f32; 3]; 3],
}

impl Space {
    fn new(working: WorkingSpace) -> Self {
        let gamut = ColorPipeline { working, wide_gamut: false, auto_tone_map: true }.working_gamut();
        Self { to_xyz: to_f32(&gamut.to_xyz()) }
    }

    fn xyz(&self, c: [f32; 3]) -> [f32; 3] {
        let m = &self.to_xyz;
        m.map(|row| row[0] * c[0] + row[1] * c[1] + row[2] * c[2])
    }

    /// Luminance (the Y row of the primaries).
    fn y(&self, c: [f32; 3]) -> f64 {
        self.xyz(c.map(|v| v.max(0.0)))[1] as f64
    }

    /// Oklab (Björn Ottosson, 2020) of a linear colour, from XYZ (D65, white Y = 1).
    fn lab(&self, c: [f32; 3]) -> [f32; 3] {
        let [x, y, z] = self.xyz(c.map(|v| v.max(0.0)));
        let l = 0.818_933 * x + 0.361_866_74 * y - 0.128_859_71 * z;
        let m = 0.032_984_544 * x + 0.929_311_9 * y + 0.036_145_64 * z;
        let s = 0.048_200_3 * x + 0.264_366_27 * y + 0.633_851_7 * z;
        let (l, m, s) = (l.max(0.0).cbrt(), m.max(0.0).cbrt(), s.max(0.0).cbrt());
        [
            0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        ]
    }
}

// ---- facts ----------------------------------------------------------------------------------

/// A colour wheel that moved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WheelFact {
    /// `lift`, `gamma`, `gain` or `offset`.
    pub wheel: String,
    /// The master ring.
    pub master: f32,
    /// The direction the colour ball moved (a hue name), when it moved.
    pub towards: Option<String>,
    /// How far the colour ball moved (length of the r, g, b offsets about their mean).
    pub color_amount: f32,
}

/// A tone curve that bends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurveFact {
    /// `curve.master`, `curve.r`, …
    pub curve: String,
    /// What it does where, e.g. "lifts shadows", "compresses highlights".
    pub effects: Vec<String>,
}

/// A switched-on qualifier, in numbers and words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualifierFact {
    pub hue: f32,
    pub hue_width: f32,
    pub sat: [f32; 2],
    pub lum: [f32; 2],
    pub invert: bool,
    /// e.g. "hues around orange (±18°), saturation 30–100 %, luminance 0–100 %".
    pub words: String,
}

/// The node's LUT.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LutFact {
    /// The LUT reference (`builtin:<id>`, `lib:<id>`).
    pub reference: String,
    pub mix: f32,
}

/// The controls of a node that differ from identity.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlFacts {
    pub wheels: Vec<WheelFact>,
    /// (contrast, pivot) when contrast isn't 1.
    pub contrast: Option<[f32; 2]>,
    pub saturation: Option<f32>,
    pub temperature: Option<f32>,
    pub tint: Option<f32>,
    pub curves: Vec<CurveFact>,
    /// Highlight roll-off (amount, knee) when on.
    pub highlight_rolloff: Option<[f32; 2]>,
    pub lut: Option<LutFact>,
    pub grain: Option<f32>,
    /// Noise reduction (luma, chroma) when either is on.
    pub noise_reduction: Option<[f32; 2]>,
    pub qualifier: Option<QualifierFact>,
    /// Windows that render (not off, not at opacity 0).
    pub windows: usize,
    /// How many of them are inverted.
    pub windows_inverted: usize,
}

/// The measured change in one tonal range, inside the changed area.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeFacts {
    /// `shadows`, `midtones` or `highlights`.
    pub range: &'static str,
    /// Whether the node visibly changed this range.
    pub touched: bool,
    /// Share of this range's pixels (by tonal weight) the node changed.
    pub share: f32,
    /// Exposure change in stops.
    pub stops: f32,
    /// Shift of neutral towards yellow (+) / blue (−), Oklab b/L.
    pub warmth: f32,
    /// Shift of neutral towards magenta (+) / green (−), Oklab a/L.
    pub tint: f32,
    /// Saturation change in percent, when the range has colour to measure it on.
    pub saturation: Option<f32>,
}

/// The owned form `RangeFacts` is read from (its `range` is a `&'static str`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RangeFactsRaw {
    range: String,
    touched: bool,
    share: f32,
    stops: f32,
    warmth: f32,
    tint: f32,
    saturation: Option<f32>,
}

impl<'de> Deserialize<'de> for RangeFacts {
    /// Reads the range name back as one of [`RANGE_NAMES`] (anything else is an error).
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = RangeFactsRaw::deserialize(d)?;
        let range =
            RANGE_NAMES.iter().copied().find(|n| *n == r.range).ok_or_else(|| serde::de::Error::custom(format!("unknown tonal range '{}'", r.range)))?;
        let facts = Self { range, touched: r.touched, share: r.share, stops: r.stops, warmth: r.warmth, tint: r.tint, saturation: r.saturation };
        if ![facts.share, facts.stops, facts.warmth, facts.tint].iter().chain(facts.saturation.iter()).all(|v| v.is_finite()) {
            return Err(serde::de::Error::custom("non-finite number in the range facts"));
        }
        Ok(facts)
    }
}

/// [`NodeFacts`] as read from JSON, validated by [`NodeFacts`]'s `TryFrom`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeFactsRaw {
    enabled: bool,
    showing_matte: bool,
    measured: bool,
    controls: ControlFacts,
    coverage: f32,
    matte_coverage: Option<f32>,
    ranges: [RangeFacts; 3],
}

impl TryFrom<NodeFactsRaw> for NodeFacts {
    type Error = String;

    /// Ranges must be shadows, midtones, highlights in order and every number finite.
    fn try_from(r: NodeFactsRaw) -> Result<Self, String> {
        if r.ranges.iter().zip(RANGE_NAMES).any(|(f, n)| f.range != n) {
            return Err("ranges must be shadows, midtones, highlights in that order".into());
        }
        let c = &r.controls;
        let nums = [c.contrast.map(|v| v[0]), c.contrast.map(|v| v[1]), c.saturation, c.temperature, c.tint, c.grain, c.lut.as_ref().map(|l| l.mix)]
            .into_iter()
            .flatten()
            .chain(c.highlight_rolloff.iter().flatten().copied())
            .chain(c.noise_reduction.iter().flatten().copied())
            .chain(c.wheels.iter().flat_map(|w| [w.master, w.color_amount]))
            .chain(c.qualifier.iter().flat_map(|q| [q.hue, q.hue_width].into_iter().chain(q.sat).chain(q.lum)))
            .chain([r.coverage])
            .chain(r.matte_coverage);
        if !nums.into_iter().all(f32::is_finite) {
            return Err("non-finite number in the node facts".into());
        }
        Ok(Self {
            enabled: r.enabled,
            showing_matte: r.showing_matte,
            measured: r.measured,
            controls: r.controls,
            coverage: r.coverage,
            matte_coverage: r.matte_coverage,
            ranges: r.ranges,
        })
    }
}

impl RangeFacts {
    fn untouched(range: &'static str) -> Self {
        Self { range, touched: false, share: 0.0, stops: 0.0, warmth: 0.0, tint: 0.0, saturation: None }
    }
}

/// What a node does: its controls and the measured change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "NodeFactsRaw")]
pub struct NodeFacts {
    pub enabled: bool,
    /// The node shows its qualifier selection (Highlight / Matte view) instead of grading: nothing
    /// is measured.
    pub showing_matte: bool,
    /// False when the frames couldn't be measured (empty, mismatched sizes) or the node shows its
    /// matte: nothing is reported.
    pub measured: bool,
    pub controls: ControlFacts,
    /// Fraction of the frame changed by more than [`JND`].
    pub coverage: f32,
    /// With a qualifier or windows: the fraction of the frame the node touches at all (where the
    /// output differs from the input), its matte.
    pub matte_coverage: Option<f32>,
    /// Shadows, midtones, highlights.
    pub ranges: [RangeFacts; 3],
}

/// Read `node`'s controls at `t` and measure what it does: `input` and `output` are the node's
/// input and output frames (premultiplied linear light in `working`, the same size). Hostile
/// frames (empty, mismatched, non-finite) give facts with no visible change.
pub fn explain_node(node: &GradeNode, t: Tick, input: &Image, output: &Image, working: WorkingSpace) -> NodeFacts {
    let controls = read_controls(node, t);
    let has_matte = controls.qualifier.is_some() || controls.windows > 0;
    let showing_matte = node.qualifier_at(t).is_some_and(|q| q.show > 0);
    let mut facts = NodeFacts {
        enabled: node.enabled,
        showing_matte,
        measured: false,
        controls,
        coverage: 0.0,
        matte_coverage: None,
        ranges: RANGE_NAMES.map(RangeFacts::untouched),
    };
    if showing_matte {
        return facts;
    }
    let Some(m) = measure(input, output, &Space::new(working)) else {
        return facts;
    };
    facts.measured = true;
    facts.coverage = m.coverage;
    facts.matte_coverage = has_matte.then_some(m.any_coverage);
    facts.ranges = m.ranges;
    facts
}

/// Per pixel: unpremultiplied colour, or None (transparent, non-finite).
fn unpremultiply(p: &[f32; 4]) -> Option<[f32; 3]> {
    let [r, g, b, a] = *p;
    if !(a.is_finite() && a > 1e-3) {
        return None;
    }
    let c = [r / a, g / a, b / a];
    c.iter().all(|v| v.is_finite()).then_some(c)
}

fn delta_e(x: [f32; 3], y: [f32; 3]) -> f32 {
    let d = ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2)).sqrt();
    if d.is_finite() { d } else { 0.0 }
}

/// The pixel count of a well-formed image (`px` holds `w·h` RGBA pixels), else None.
fn pixel_count(img: &Image) -> Option<usize> {
    let n = img.w.checked_mul(img.h)?;
    (n > 0 && n.checked_mul(4)? == img.px.len()).then_some(n)
}

/// The sampling stride for `n` pixels: ⌈√(n / MAX_SAMPLES)⌉ (at least 1).
fn stride(n: usize) -> usize {
    let mut s = ((n as f64 / MAX_SAMPLES as f64).sqrt().ceil() as usize).max(1);
    // (guard the float estimate: the grid must hold at most MAX_SAMPLES-ish pixels)
    while s < 1 << 20 && n / s.saturating_mul(s) > MAX_SAMPLES {
        s += 1;
    }
    s
}

struct Measured {
    coverage: f32,
    any_coverage: f32,
    ranges: [RangeFacts; 3],
}

/// Accumulators of one tonal range (f64).
#[derive(Default, Clone, Copy)]
struct Acc {
    /// Total tonal weight of the range over all measured pixels.
    total: f64,
    /// Weight of the changed pixels.
    changed: f64,
    y_in: f64,
    y_out: f64,
    /// Chromaticity statistics: weight, Σw·u, Σw·|u|² for input and output.
    cw: f64,
    u_in: [f64; 2],
    u_out: [f64; 2],
    in_sq: f64,
    out_sq: f64,
}

impl Acc {
    fn add(&mut self, o: &Acc) {
        self.total += o.total;
        self.changed += o.changed;
        self.y_in += o.y_in;
        self.y_out += o.y_out;
        self.cw += o.cw;
        for c in 0..2 {
            self.u_in[c] += o.u_in[c];
            self.u_out[c] += o.u_out[c];
        }
        self.in_sq += o.in_sq;
        self.out_sq += o.out_sq;
    }
}

/// One sampled row's sums.
#[derive(Default, Clone, Copy)]
struct RowSums {
    acc: [Acc; 3],
    samples: usize,
    changed: usize,
    any: usize,
}

fn measure_row(inp: &[[f32; 4]], out: &[[f32; 4]], step: usize, sp: &Space) -> RowSums {
    let mut r = RowSums::default();
    for (pi, po) in inp.iter().step_by(step).zip(out.iter().step_by(step)) {
        r.samples += 1;
        let (Some(ci), Some(co)) = (unpremultiply(pi), unpremultiply(po)) else { continue };
        let (li, lo) = (sp.lab(ci), sp.lab(co));
        if !li.iter().chain(&lo).all(|v| v.is_finite()) {
            continue;
        }
        let w = tonal_weights(li[0]);
        for (a, wk) in r.acc.iter_mut().zip(w) {
            a.total += wk as f64;
        }
        let de = delta_e(li, lo);
        if de > ANY_CHANGE {
            r.any += 1;
        }
        if de <= JND {
            continue;
        }
        r.changed += 1;
        let (yi, yo) = (sp.y(ci), sp.y(co));
        let chroma_ok = li[0] >= CHROMA_MIN_L && lo[0] >= CHROMA_MIN_L;
        let ui = [(li[1] / li[0]) as f64, (li[2] / li[0]) as f64];
        let uo = [(lo[1] / lo[0]) as f64, (lo[2] / lo[0]) as f64];
        for (a, wk) in r.acc.iter_mut().zip(w) {
            let wk = wk as f64;
            a.changed += wk;
            a.y_in += wk * yi;
            a.y_out += wk * yo;
            if chroma_ok {
                a.cw += wk;
                for c in 0..2 {
                    a.u_in[c] += wk * ui[c];
                    a.u_out[c] += wk * uo[c];
                }
                a.in_sq += wk * (ui[0] * ui[0] + ui[1] * ui[1]);
                a.out_sq += wk * (uo[0] * uo[0] + uo[1] * uo[1]);
            }
        }
    }
    r
}

fn measure(input: &Image, output: &Image, sp: &Space) -> Option<Measured> {
    let n = pixel_count(input)?;
    if pixel_count(output)? != n || input.w != output.w {
        return None;
    }
    let step = stride(n);
    let (rows_in, rows_out) = (input.px.as_chunks::<4>().0, output.px.as_chunks::<4>().0);
    let w = input.w;
    let ys: Vec<usize> = (0..input.h).step_by(step).collect();
    // rows in parallel, summed in row order: deterministic
    let rows: Vec<RowSums> = ys
        .par_iter()
        .map(|&y| {
            let lo = y.saturating_mul(w);
            let hi = lo.saturating_add(w);
            match (rows_in.get(lo..hi), rows_out.get(lo..hi)) {
                (Some(a), Some(b)) => measure_row(a, b, step, sp),
                _ => RowSums::default(),
            }
        })
        .collect();
    let mut sum = RowSums::default();
    for r in &rows {
        for (a, b) in sum.acc.iter_mut().zip(&r.acc) {
            a.add(b);
        }
        sum.samples += r.samples;
        sum.changed += r.changed;
        sum.any += r.any;
    }
    let frame = sum.samples.max(1) as f64;
    let mut k = 0;
    let ranges = sum.acc.map(|a| {
        let name = RANGE_NAMES.get(k).copied().unwrap_or("");
        k += 1;
        range_facts(name, &a, frame)
    });
    Some(Measured { coverage: (sum.changed as f64 / frame) as f32, any_coverage: (sum.any as f64 / frame) as f32, ranges })
}

fn range_facts(range: &'static str, a: &Acc, frame: f64) -> RangeFacts {
    let share = if a.total > 1e-9 { (a.changed / a.total).clamp(0.0, 1.0) as f32 } else { 0.0 };
    if a.changed <= 0.0 || share < RANGE_MIN_SHARE || ((a.changed / frame) as f32) < RANGE_MIN_FRAME {
        return RangeFacts { share, ..RangeFacts::untouched(range) };
    }
    let stops = if a.y_in > 1e-12 && a.y_out > 1e-12 { (a.y_out / a.y_in).log2() as f32 } else { 0.0 };
    let (mut warmth, mut tint, mut saturation) = (0.0, 0.0, None);
    if a.cw > 1e-9 {
        let mi = a.u_in.map(|v| v / a.cw);
        let mo = a.u_out.map(|v| v / a.cw);
        // spreads about each mean: their ratio is the saturation change (a cast moves the mean,
        // not the spread)
        let var_in = (a.in_sq / a.cw - (mi[0] * mi[0] + mi[1] * mi[1])).max(0.0);
        let var_out = (a.out_sq / a.cw - (mo[0] * mo[0] + mo[1] * mo[1])).max(0.0);
        let mut s = 1.0;
        if var_in > SAT_MIN_SPREAD * SAT_MIN_SPREAD {
            let r = (var_out / var_in).sqrt();
            if r.is_finite() {
                s = r;
                saturation = Some(((r - 1.0) * 100.0) as f32);
            }
        }
        // the cast: where neutral goes
        tint = (mo[0] - s * mi[0]) as f32;
        warmth = (mo[1] - s * mi[1]) as f32;
    }
    let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
    RangeFacts { range, touched: true, share, stops: fin(stops), warmth: fin(warmth), tint: fin(tint), saturation: saturation.filter(|v| v.is_finite()) }
}

// ---- controls -------------------------------------------------------------------------------

/// The windows that render at `t`: the first [`MAX_WINDOWS`], without those switched off (mode
/// None) or at opacity 0.
fn active_windows(node: &GradeNode, t: Tick) -> Vec<&Mask> {
    let windows = node.windows.get(..node.windows.len().min(MAX_WINDOWS)).unwrap_or_default();
    windows.iter().filter(|w| w.mode != MaskMode::None).filter(|w| !matches!(w.opacity.value_at(t), ParamValue::Float(o) if o.is_nan() || o <= 0.0)).collect()
}

/// A hue (0…1, HSL: 0 red, ⅓ green, ⅔ blue) as one of twelve names.
fn hue_name(h: f32) -> &'static str {
    const NAMES: [&str; 12] = ["red", "orange", "yellow", "yellow-green", "green", "spring green", "cyan", "azure", "blue", "violet", "magenta", "rose"];
    let h = if h.is_finite() { h.rem_euclid(1.0) } else { 0.0 };
    let i = ((h * 12.0).round() as usize) % 12;
    NAMES.get(i).copied().unwrap_or("red")
}

/// The hue an RGB offset points to (its HSL hue), None when it's (near) neutral.
fn offset_hue(rgb: [f32; 3]) -> Option<(f32, &'static str)> {
    let mean = (rgb[0] + rgb[1] + rgb[2]) / 3.0;
    let d = rgb.map(|v| v - mean);
    let amount = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if !(amount.is_finite() && amount > 0.005) {
        return None;
    }
    // hue of the chroma vector on the RGB hexagon (as HSL)
    let x = d[0] - 0.5 * (d[1] + d[2]);
    let y = (3f32.sqrt() / 2.0) * (d[1] - d[2]);
    let h = y.atan2(x) / std::f32::consts::TAU;
    Some((amount, hue_name(h)))
}

fn pct(v: f32) -> i64 {
    (v * 100.0).round() as i64
}

fn identity_of(key: &str) -> Option<f32> {
    NODE_PARAMS.iter().find(|p| p.key == key).and_then(|p| match p.kind {
        NodeParamKind::Float { identity, .. } => Some(identity),
        _ => None,
    })
}

fn read_controls(node: &GradeNode, t: Tick) -> ControlFacts {
    let mut c = ControlFacts::default();
    for key in ["lift", "gamma", "gain", "offset"] {
        let w = node.wheel_at(key, t);
        let colour = offset_hue([w[0], w[1], w[2]]);
        if w[3] != 0.0 || colour.is_some() {
            c.wheels.push(WheelFact {
                wheel: key.into(),
                master: w[3],
                towards: colour.map(|(_, n)| n.to_string()),
                color_amount: colour.map_or(0.0, |(a, _)| a),
            });
        }
    }
    let changed = |key: &str| {
        let v = node.float_at(key, t);
        (Some(v) != identity_of(key)).then_some(v)
    };
    c.contrast = changed("contrast").map(|k| [k, node.float_at("pivot", t)]);
    c.saturation = changed("saturation");
    c.temperature = changed("temperature");
    c.tint = changed("tint");
    for key in ["curve.master", "curve.r", "curve.g", "curve.b"] {
        if let Some(points) = node.curve(key) {
            c.curves.push(CurveFact { curve: key.into(), effects: curve_effects(&points) });
        }
    }
    c.highlight_rolloff = changed("hl.rolloff").filter(|v| *v > 0.0).map(|r| [r, node.float_at("hl.knee", t)]);
    let mix = node.float_at("lut.mix", t);
    c.lut = (!node.lut.is_empty() && mix > 0.0).then(|| LutFact { reference: node.lut.clone(), mix });
    c.grain = changed("grain.strength").filter(|v| *v > 0.0);
    let (nl, nc) = (node.float_at("nr.luma", t), node.float_at("nr.chroma", t));
    c.noise_reduction = (nl > 0.0 || nc > 0.0).then_some([nl, nc]);
    c.qualifier = node.qualifier_at(t).map(|q| {
        let hues =
            if q.hue_width >= 1.0 { "all hues".to_string() } else { format!("hues around {} (±{}°)", hue_name(q.hue), (q.hue_width * 180.0).round() as i64) };
        let words = format!(
            "{hues}, saturation {}–{} %, luminance {}–{} %{}",
            pct(q.sat[0]),
            pct(q.sat[1]),
            pct(q.lum[0]),
            pct(q.lum[1]),
            if q.invert { ", inverted" } else { "" }
        );
        QualifierFact { hue: q.hue, hue_width: q.hue_width, sat: q.sat, lum: q.lum, invert: q.invert, words }
    });
    let windows = active_windows(node, t);
    c.windows = windows.len();
    c.windows_inverted = windows.iter().filter(|w| w.inverted).count();
    c
}

/// What a curve does at shadows (¼), midtones (½) and highlights (¾).
fn curve_effects(points: &[[f32; 2]]) -> Vec<String> {
    const N: usize = 1024;
    let lut = crate::effects::curve_lut(points, N);
    let at = |x: f32| {
        let i = ((x * (N - 1) as f32).round() as usize).min(N - 1);
        lut.get(i).map(|y| y - x).filter(|d| d.is_finite()).unwrap_or(0.0)
    };
    let mut out = Vec::new();
    for (x, up, down) in
        [(0.25, "lifts shadows", "lowers shadows"), (0.5, "raises midtones", "lowers midtones"), (0.75, "lifts highlights", "compresses highlights")]
    {
        let d = at(x);
        if d > 0.01 {
            out.push(up.to_string());
        } else if d < -0.01 {
            out.push(down.to_string());
        }
    }
    if out.is_empty() {
        out.push("bends slightly".to_string());
    }
    out
}

// ---- text -----------------------------------------------------------------------------------

const MINUS: char = '\u{2212}';

fn signed(v: i64) -> String {
    if v < 0 { format!("{MINUS}{}", v.unsigned_abs()) } else { format!("+{v}") }
}

/// Stops rounded to the nearest ⅙ as sixths (0: no change).
fn sixths(stops: f32) -> i64 {
    if stops.is_finite() { (stops * 6.0).round().clamp(-600.0, 600.0) as i64 } else { 0 }
}

/// "+⅙", "−1½", "+2" for a count of sixths.
fn sixths_text(n: i64) -> String {
    const FRAC: [&str; 6] = ["", "⅙", "⅓", "½", "⅔", "⅚"];
    let sign = if n < 0 { MINUS.to_string() } else { "+".to_string() };
    let a = n.unsigned_abs();
    let (whole, frac) = (a / 6, (a % 6) as usize);
    let whole = if whole == 0 && frac != 0 { String::new() } else { whole.to_string() };
    format!("{sign}{whole}{}", FRAC.get(frac).copied().unwrap_or(""))
}

fn stop_word(n: i64) -> &'static str {
    if n.unsigned_abs() <= 6 { "stop" } else { "stops" }
}

fn stops_text(n: i64) -> String {
    format!("{} {}", sixths_text(n), stop_word(n))
}

/// 0 none, 1 slightly, 2 plain, 3 much.
fn cast_level(v: f32) -> usize {
    CAST_STEPS.iter().filter(|s| v.abs() >= **s).count()
}

fn cast_text(v: f32, pos: &str, neg: &str) -> Option<String> {
    let word = if v > 0.0 { pos } else { neg };
    match cast_level(v) {
        0 => None,
        1 => Some(format!("slightly {word}")),
        2 => Some(word.to_string()),
        _ => Some(format!("much {word}")),
    }
}

fn sat_pct(r: &RangeFacts) -> Option<i64> {
    r.saturation.map(|s| s.round() as i64).filter(|s| s.abs() >= SAT_MIN_PCT)
}

impl RangeFacts {
    /// The measured changes in words, in order: exposure, warmth, tint, saturation.
    fn phrases(&self) -> Vec<String> {
        let mut out = Vec::new();
        let n = sixths(self.stops);
        if n != 0 {
            out.push(stops_text(n));
        }
        out.extend(cast_text(self.warmth, "warmer", "cooler"));
        out.extend(cast_text(self.tint, "more magenta", "greener"));
        if let Some(s) = sat_pct(self) {
            out.push(format!("saturation {} %", signed(s)));
        }
        out
    }
}

fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [a] => a.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn percent_text(f: f32) -> String {
    let p = (f * 100.0).round() as i64;
    if p == 0 && f > 0.0 { "<1 %".to_string() } else { format!("{p} %") }
}

impl NodeFacts {
    /// The share of the frame the node affects: its matte when it has a qualifier or windows,
    /// otherwise the visibly changed area.
    pub fn affected(&self) -> f32 {
        self.matte_coverage.unwrap_or(self.coverage)
    }

    /// Whether any pixel changed visibly.
    pub fn visible(&self) -> bool {
        self.measured && self.coverage > 0.0
    }

    /// "adds grain", "reduces noise", both, or None.
    fn texture(&self) -> Option<String> {
        let mut t = Vec::new();
        if self.controls.grain.is_some() {
            t.push("adds grain");
        }
        if self.controls.noise_reduction.is_some() {
            t.push("reduces noise");
        }
        (!t.is_empty()).then(|| join_and(&t))
    }

    /// One plain sentence, e.g. "Node 2 'Skin': affects 18 % of the frame; inside it, midtones
    /// +⅙ stop, slightly greener, saturation −16 %; shadows and highlights untouched."
    pub fn sentence(&self, label: &str) -> String {
        if !self.enabled {
            return format!("{label}: disabled, no visible change.");
        }
        if self.showing_matte {
            return format!("{label}: is showing its qualifier selection (Highlight / Matte view), not a grade.");
        }
        if !self.visible() {
            return format!("{label}: no visible change.");
        }
        let texture = self.texture();
        // touched ranges grouped by identical phrases, in range order (true: texture only)
        let mut groups: Vec<(Vec<&str>, String, bool)> = Vec::new();
        let mut untouched = Vec::new();
        for r in &self.ranges {
            if !r.touched {
                untouched.push(r.range);
                continue;
            }
            let p = r.phrases();
            let (p, tex) = match (p.is_empty(), &texture) {
                (false, _) => (p.join(", "), false),
                (true, Some(t)) => (t.clone(), true),
                (true, None) => ("changed slightly".to_string(), false),
            };
            match groups.iter_mut().find(|(_, q, _)| *q == p) {
                Some((names, _, _)) => names.push(r.range),
                None => groups.push((vec![r.range], p, tex)),
            }
        }
        let whole = self.matte_coverage.is_none() && self.coverage >= WHOLE_FRAME;
        let mut s =
            if whole { format!("{label}: affects the whole frame") } else { format!("{label}: affects {} of the frame", percent_text(self.affected())) };
        if !groups.is_empty() {
            let parts: Vec<String> = groups
                .iter()
                .map(|(names, p, tex)| match (names.len(), *tex) {
                    (3, true) => p.clone(),
                    (3, false) => format!("{p} in all tones"),
                    (_, true) => format!("{p} in {}", join_and(names)),
                    _ if p.starts_with("saturation") => format!("{p} in {}", join_and(names)),
                    _ => format!("{} {p}", join_and(names)),
                })
                .collect();
            s.push_str(if whole { "; " } else { "; inside it, " });
            s.push_str(&parts.join("; "));
        }
        if let Some(t) = texture.filter(|_| !groups.iter().any(|g| g.2)) {
            s.push_str(&format!("; also {t}"));
        }
        if !untouched.is_empty() {
            s.push_str(&format!("; {} untouched", join_and(&untouched)));
        }
        s.push('.');
        s
    }

    /// A short label for the node strip (≤ ~40 characters), e.g. "+⅙ stop mids · warmer · 18 %".
    pub fn summary(&self) -> String {
        if !self.enabled {
            return "disabled".to_string();
        }
        if self.showing_matte {
            return "showing matte".to_string();
        }
        if !self.visible() {
            return "no change".to_string();
        }
        let short = |r: &str| match r {
            "shadows" => "shad",
            "midtones" => "mids",
            _ => "highs",
        };
        let mut items = Vec::new();
        // the range with the largest exposure change
        let moved = self.ranges.iter().filter(|r| r.touched && sixths(r.stops) != 0);
        if let Some(r) = moved.max_by(|a, b| a.stops.abs().total_cmp(&b.stops.abs())) {
            let n = sixths(r.stops);
            let all = self.ranges.iter().all(|q| q.touched && sixths(q.stops) == n);
            items.push(if all { stops_text(n) } else { format!("{} {}", stops_text(n), short(r.range)) });
        }
        // the strongest cast
        let touched = || self.ranges.iter().filter(|r| r.touched);
        let warm = touched().map(|r| r.warmth).max_by(|a, b| a.abs().total_cmp(&b.abs())).unwrap_or(0.0);
        let tint = touched().map(|r| r.tint).max_by(|a, b| a.abs().total_cmp(&b.abs())).unwrap_or(0.0);
        let cast = if cast_level(warm) >= cast_level(tint) {
            (cast_level(warm) > 0).then_some(if warm > 0.0 { "warmer" } else { "cooler" })
        } else {
            Some(if tint > 0.0 { "magenta" } else { "greener" })
        };
        items.extend(cast.map(str::to_string));
        if let Some(s) = touched().filter_map(sat_pct).max_by_key(|s| s.unsigned_abs()) {
            items.push(format!("sat {} %", signed(s)));
        }
        if items.is_empty() {
            items.push(match (self.controls.grain.is_some(), self.controls.noise_reduction.is_some()) {
                (true, true) => "grain + NR".to_string(),
                (true, false) => "grain".to_string(),
                (false, true) => "NR".to_string(),
                _ => "subtle".to_string(),
            });
        }
        items.truncate(3);
        items.push(percent_text(self.affected()));
        let mut s = items.join(" · ");
        // keep it short: drop middle items first
        while s.chars().count() > 40 && items.len() > 2 {
            items.remove(items.len() - 2);
            s = items.join(" · ");
        }
        s
    }
}

/// A short paragraph: every node's sentence in order.
pub fn explain_grade(nodes: &[(String, NodeFacts)]) -> String {
    if nodes.is_empty() {
        return "The grade has no nodes.".to_string();
    }
    nodes.iter().map(|(label, f)| f.sentence(label)).collect::<Vec<_>>().join(" ")
}

// ---- overlay --------------------------------------------------------------------------------

/// "What changed": per pixel the Oklab difference between `input` and `output` (linear light in
/// `working`) as a heat colour with alpha (premultiplied), the same size. Below half a [`JND`]
/// it's transparent; a just-noticeable change is a faint red, a [`STRONG_CHANGE`] an opaque
/// yellow. Mismatched or malformed frames give a transparent image (the input's size when it is
/// well formed).
pub fn change_overlay(input: &Image, output: &Image, working: WorkingSpace) -> Image {
    let Some(n) = pixel_count(input) else {
        return Image::new(0, 0);
    };
    let mut out = Image::new(input.w, input.h);
    if pixel_count(output) != Some(n) || output.w != input.w {
        return out;
    }
    let sp = Space::new(working);
    let (pin, pout) = (input.px.as_chunks::<4>().0, output.px.as_chunks::<4>().0);
    out.px.as_chunks_mut::<4>().0.par_iter_mut().zip(pin.par_iter().zip(pout.par_iter())).for_each(|(dst, (pi, po))| {
        let (Some(ci), Some(co)) = (unpremultiply(pi), unpremultiply(po)) else { return };
        let de = delta_e(sp.lab(ci), sp.lab(co));
        if de < 0.5 * JND {
            return;
        }
        let t = ((de - 0.5 * JND) / (STRONG_CHANGE - 0.5 * JND)).clamp(0.0, 1.0);
        let a = 0.15 + 0.85 * t;
        let c = [1.0, 0.85 * t, 0.1 * t * t];
        *dst = [c[0] * a, c[1] * a, c[2] * a, a];
    });
    out
}

// ---- cache key ------------------------------------------------------------------------------

/// FNV-1a, 64 bit (stable across builds and platforms).
struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn f32(&mut self, v: f32) {
        self.bytes(&v.to_bits().to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
}

/// A deterministic hash of everything about the node that decides its explanation at `t`:
/// enabled, every parameter's value at `t` as the grade reads it (clamped, identity for missing;
/// the qualifier's ranges only while it is on), the curves, the LUT reference, the windows that
/// render (shape, mode, invert, values at `t`), the working space and the render scale, and for a
/// node with grain the sequence time and clip (the grain pattern).
///
/// The input frame's identity (the clip, its frame, the nodes before this one) is the caller's
/// part of the cache key.
pub fn node_state_key(node: &GradeNode, t: Tick, ctx: &GradeCtx<'_>, working: WorkingSpace) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    h.bytes(&[node.enabled as u8]);
    h.bytes(working.id().as_bytes());
    h.f32(ctx.px_scale);
    let qual_on = node.qualifier_at(t).is_some();
    for p in NODE_PARAMS {
        if !qual_on && p.key.starts_with("qual.") && p.key != "qual.on" {
            continue;
        }
        h.bytes(p.key.as_bytes());
        match p.kind {
            NodeParamKind::Float { .. } => h.f32(node.float_at(p.key, t)),
            NodeParamKind::Wheel { .. } => node.wheel_at(p.key, t).into_iter().for_each(|v| h.f32(v)),
            NodeParamKind::Curve => {
                let pts = node.curve(p.key).unwrap_or_default();
                h.u64(pts.len() as u64);
                pts.into_iter().flatten().for_each(|v| h.f32(v));
            }
        }
    }
    if node.float_at("grain.strength", t) > 0.0 {
        h.u64(ctx.seq_time.0 as u64);
        h.u64(ctx.clip);
    }
    h.u64(node.lut.len() as u64);
    h.bytes(node.lut.as_bytes());
    let windows = active_windows(node, t);
    h.u64(windows.len() as u64);
    for w in windows {
        let values: Vec<_> = MASK_PARAMS
            .iter()
            .map(|k| match *k {
                "path" => w.path.value_at(t),
                "feather" => w.feather.value_at(t),
                "opacity" => w.opacity.value_at(t),
                _ => w.expansion.value_at(t),
            })
            .collect();
        let s = serde_json::to_string(&(&w.shape, &w.mode, w.inverted, values)).unwrap_or_default();
        h.bytes(s.as_bytes());
        h.bytes(&[0xff]);
    }
    h.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::compile_fx;
    use filmcraft_color::Gamut;
    use filmcraft_color::spaces::gamut_matrix;
    use filmcraft_project::grade::Grade;
    use filmcraft_project::{MaskPath, Param};

    const W: usize = 120;
    const H: usize = 60;
    const R709: WorkingSpace = WorkingSpace::Rec709;
    const DWG: WorkingSpace = WorkingSpace::DaVinciWide;
    const SKIN: [f32; 3] = [0.45, 0.24, 0.15];

    /// A test frame (Rec. 709 linear): a grey ramp (left to right) in the top half; colour patches
    /// in the bottom half (blue, green, yellow) with a skin-tone patch of known size at its left.
    fn frame709() -> Image {
        let mut img = Image::new(W, H);
        for y in 0..H {
            for x in 0..W {
                let c = if y < H / 2 {
                    let t = (x as f32 + 0.5) / W as f32;
                    let v = 0.002 + 0.9 * t * t;
                    [v, v, v]
                } else if x < 30 {
                    SKIN
                } else {
                    match (x - 30) / 30 {
                        0 => [0.03, 0.05, 0.25],
                        1 => [0.05, 0.2, 0.04],
                        _ => [0.3, 0.25, 0.05],
                    }
                };
                let i = (y * W + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
        }
        img
    }

    /// The test frame in `working`'s linear primaries.
    fn frame(working: WorkingSpace) -> Image {
        let mut img = frame709();
        if working == DWG {
            let m = to_f32(&gamut_matrix(Gamut::Bt709, Gamut::DaVinciWide));
            for p in img.px.as_chunks_mut::<4>().0 {
                let c = m.map(|row| row[0] * p[0] + row[1] * p[1] + row[2] * p[2]);
                p[..3].copy_from_slice(&c);
            }
        }
        img
    }

    fn node(params: &[(&str, ParamValue)]) -> GradeNode {
        let mut n = GradeNode::new(1);
        for (k, v) in params {
            n.params.insert((*k).into(), Param::new(v.clone()));
        }
        n
    }

    fn render_at(n: &GradeNode, img: &Image, t: Tick, ws: WorkingSpace) -> Image {
        let mut out = img.clone();
        let g = Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() };
        for op in compile_fx(&g, t, ws, GradeCtx::default(), img.w, img.h) {
            op.apply(&mut out);
        }
        out
    }

    fn explain_in(n: &GradeNode, ws: WorkingSpace) -> NodeFacts {
        let f = frame(ws);
        explain_node(n, Tick(0), &f, &render_at(n, &f, Tick(0), ws), ws)
    }

    fn explain(n: &GradeNode) -> NodeFacts {
        explain_in(n, R709)
    }

    fn show(f: &NodeFacts, label: &str) {
        eprintln!("{f:#?}\n{}\n{}", f.sentence(label), f.summary());
    }

    fn gain(v: f32) -> GradeNode {
        node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, v]))])
    }

    #[test]
    fn identity_node_has_no_visible_change() {
        let f = explain(&GradeNode::new(1));
        assert!(f.measured && f.coverage == 0.0 && f.ranges.iter().all(|r| !r.touched), "{f:?}");
        assert_eq!(f.sentence("Node 1"), "Node 1: no visible change.");
        assert_eq!(f.summary(), "no change");
        assert_eq!(f.controls, ControlFacts::default());
    }

    #[test]
    fn gain_master_reports_stops() {
        let n = gain(0.1);
        let f = explain(&n);
        show(&f, "Node 1 'Gain'");
        assert_eq!(f.controls.wheels, vec![WheelFact { wheel: "gain".into(), master: 0.1, towards: None, color_amount: 0.0 }]);
        // what the node really does to mid grey, in stops, independently of the explainer
        let ops = crate::grade::compile(&Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() }, Tick(0), R709);
        let grey = filmcraft_color::ops::apply_chain(&ops, [0.18; 3]);
        let real = (grey[1] / 0.18).log2();
        assert_eq!(sixths(real), 2, "{real}");
        assert!(f.ranges.iter().all(|r| r.touched && sixths(r.stops) == 2), "{f:?}");
        assert_eq!(f.sentence("Node 1 'Gain'"), "Node 1 'Gain': affects the whole frame; +⅓ stop in all tones.");
        assert_eq!(f.summary(), "+⅓ stop · 96 %");
    }

    #[test]
    fn warm_temperature_reports_no_saturation_change() {
        let f = explain(&node(&[("temperature", ParamValue::Float(30.0))]));
        show(&f, "Node 2 'Warm'");
        assert_eq!(f.controls.temperature, Some(30.0));
        assert!(f.ranges.iter().all(|r| r.warmth > CAST_STEPS[0] && sixths(r.stops) == 0 && sat_pct(r).is_none()), "{f:?}");
        assert_eq!(f.sentence("Node 2 'Warm'"), "Node 2 'Warm': affects 70 % of the frame; inside it, slightly warmer in all tones.");
        assert_eq!(f.summary(), "warmer · 70 %");
    }

    #[test]
    fn cooler_and_greener() {
        let f = explain(&node(&[("temperature", ParamValue::Float(-60.0)), ("tint", ParamValue::Float(-30.0))]));
        show(&f, "Node 1");
        assert!(f.ranges.iter().all(|r| r.warmth < 0.0 && r.tint < 0.0), "{f:?}");
        assert_eq!(
            f.sentence("Node 1"),
            "Node 1: affects the whole frame; shadows +⅙ stop, slightly cooler, slightly greener; midtones slightly cooler, slightly greener; highlights slightly cooler, greener."
        );
        assert_eq!(f.summary(), "+⅙ stop shad · greener · 94 %");
    }

    #[test]
    fn saturation_minus_20() {
        let f = explain(&node(&[("saturation", ParamValue::Float(0.8))]));
        show(&f, "Node 3");
        assert_eq!(f.controls.saturation, Some(0.8));
        // (the control scales chroma on the grading signal: on linear-light Oklab it measures −17 %)
        assert_eq!(f.sentence("Node 3"), "Node 3: affects 50 % of the frame; inside it, saturation −17 % in all tones.");
        assert_eq!(f.summary(), "sat −17 % · 50 %");
    }

    #[test]
    fn dwg_reads_like_rec709() {
        // the same pictures expressed in DWG primaries read the same (the measurement goes
        // through XYZ)
        let m = to_f32(&gamut_matrix(Gamut::Bt709, Gamut::DaVinciWide));
        let to_dwg = |img: &Image| {
            let mut img = img.clone();
            for p in img.px.as_chunks_mut::<4>().0 {
                let c = m.map(|row| row[0] * p[0] + row[1] * p[1] + row[2] * p[2]);
                p[..3].copy_from_slice(&c);
            }
            img
        };
        for n in [gain(0.1), node(&[("temperature", ParamValue::Float(30.0))]), node(&[("saturation", ParamValue::Float(0.8))])] {
            let a = frame(R709);
            let b = render_at(&n, &a, Tick(0), R709);
            let (f709, fdwg) = (explain_node(&n, Tick(0), &a, &b, R709), explain_node(&n, Tick(0), &to_dwg(&a), &to_dwg(&b), DWG));
            assert_eq!(f709.sentence("N"), fdwg.sentence("N"), "{f709:?}\n{fdwg:?}");
            let (o1, o2) = (change_overlay(&a, &b, R709), change_overlay(&to_dwg(&a), &to_dwg(&b), DWG));
            assert!(o1.px.len() == o2.px.len() && o1.px.iter().zip(&o2.px).all(|(x, y)| (x - y).abs() < 0.02));
        }
        // the grade run in DWG (on its own grading signal)
        // a warm node reads warmer, not magenta (its white-balance gains act on DWG's much wider
        // primaries, so it is stronger there and brightens a little)
        let f = explain_in(&node(&[("temperature", ParamValue::Float(30.0))]), DWG);
        show(&f, "DWG");
        assert!(f.ranges.iter().all(|r| r.warmth > 0.0 && r.warmth.abs() > 2.0 * r.tint.abs() && sat_pct(r).is_none()), "{f:?}");
        assert_eq!(
            f.sentence("N"),
            "N: affects the whole frame; shadows +⅙ stop, warmer, slightly more magenta; midtones and highlights warmer, slightly more magenta."
        );
        // gain acts on each space's own grading signal (DaVinci Intermediate is a different
        // curve), so its stops differ; it still brightens every range without a cast
        let f = explain_in(&gain(0.1), DWG);
        show(&f, "DWG");
        assert!(f.ranges.iter().all(|r| r.touched && r.stops > 0.0 && cast_level(r.warmth) == 0 && cast_level(r.tint) == 0), "{f:?}");
        assert_eq!(f.sentence("N"), "N: affects the whole frame; shadows +⅓ stop, saturation +12 %; midtones +½ stop, saturation +10 %; highlights +⅔ stop.");
    }

    #[test]
    fn colour_ball_casts() {
        // offset towards blue: cooler; lift towards orange: warmer shadows
        let blue = node(&[("offset", ParamValue::Color([0.0, 0.0, 0.03, 0.0]))]);
        let f = explain(&blue);
        show(&f, "Offset");
        assert_eq!(f.controls.wheels.first().and_then(|w| w.towards.clone()).as_deref(), Some("blue"));
        assert_eq!(
            f.sentence("Offset"),
            "Offset: affects 75 % of the frame; inside it, shadows cooler, slightly more magenta, saturation −12 %; midtones and highlights slightly cooler."
        );
        assert_eq!(f.summary(), "cooler · sat −12 % · 75 %");
        let warm = node(&[("lift", ParamValue::Color([0.04, 0.02, 0.0, 0.0]))]);
        let f = explain(&warm);
        show(&f, "Lift");
        assert_eq!(f.controls.wheels.first().and_then(|w| w.towards.clone()).as_deref(), Some("orange"));
        assert_eq!(
            f.sentence("Lift"),
            "Lift: affects 52 % of the frame; inside it, shadows +⅙ stop, warmer, slightly more magenta; midtones +⅙ stop, slightly warmer; highlights changed slightly."
        );
    }

    #[test]
    fn contrast_lowers_shadows_and_raises_highlights() {
        let f = explain(&node(&[("contrast", ParamValue::Float(1.3))]));
        show(&f, "Contrast");
        let [s, _, h] = &f.ranges;
        assert!(s.stops < 0.0 && h.stops > 0.0, "{f:?}");
        // (contrast on the grading signal spreads the channels apart: more saturation, measured)
        assert_eq!(
            f.sentence("Contrast"),
            "Contrast: affects the whole frame; shadows −⅓ stop, saturation +29 %; midtones +⅓ stop, saturation +23 %; highlights +⅓ stop, saturation +15 %."
        );
    }

    #[test]
    fn curve_only_node() {
        let n = node(&[("curve.master", ParamValue::Curve(vec![[0.0, 0.0], [0.25, 0.33], [1.0, 1.0]]))]);
        let f = explain(&n);
        show(&f, "Curve");
        let effects = vec!["lifts shadows".to_string(), "raises midtones".into(), "lifts highlights".into()];
        assert_eq!(f.controls.curves, vec![CurveFact { curve: "curve.master".into(), effects }]);
        assert_eq!(
            f.sentence("Curve"),
            "Curve: affects the whole frame; shadows +⅔ stop, saturation −18 %; midtones +½ stop, saturation −16 %; highlights +⅙ stop, saturation −13 %."
        );
        assert_eq!(f.summary(), "+⅔ stop shad · sat −18 % · 97 %");
    }

    #[test]
    fn qualified_skin_node_covers_the_skin_patch() {
        let hue = crate::grade::qualifier_hsl(R709, SKIN)[0];
        let n = node(&[
            ("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.3])),
            ("qual.on", ParamValue::Float(1.0)),
            ("qual.hue", ParamValue::Float(hue as f64)),
            ("qual.hueWidth", ParamValue::Float(0.06)),
            ("qual.satLow", ParamValue::Float(0.15)),
            ("qual.soft", ParamValue::Float(0.02)),
        ]);
        let f = explain(&n);
        show(&f, "Node 2 'Skin'");
        // the skin patch is 30 × 30 of 120 × 60 pixels
        assert_eq!((f.coverage, f.matte_coverage), (0.125, Some(0.125)), "{f:?}");
        assert!(f.controls.qualifier.as_ref().is_some_and(|q| q.words.starts_with("hues around")), "{f:?}");
        assert_eq!(f.sentence("Node 2 'Skin'"), "Node 2 'Skin': affects 13 % of the frame; inside it, midtones and highlights +⅚ stop; shadows untouched.");
        assert_eq!(f.summary(), "+⅚ stop highs · 13 %");
        // the Highlight / Matte view: not a grade, nothing measured
        let mut v = n.clone();
        v.params.insert("qual.show".into(), Param::new(ParamValue::Float(1.0)));
        let f = explain(&v);
        assert!(f.showing_matte && !f.measured);
        assert_eq!(f.sentence("Node 2"), "Node 2: is showing its qualifier selection (Highlight / Matte view), not a grade.");
        assert_eq!(f.summary(), "showing matte");
    }

    fn window(x1: f64) -> Mask {
        let mut m = Mask::new("w", MaskPath::rect(0.0, 0.0, x1, 60.0));
        m.feather = Param::new(ParamValue::Float(0.0));
        m
    }

    #[test]
    fn inverted_window_covers_the_outside() {
        let mut n = gain(0.3);
        let mut m = window(60.0);
        m.inverted = true;
        n.windows.push(m);
        // switched-off windows don't count
        let mut off = window(10.0);
        off.mode = MaskMode::None;
        n.windows.push(off);
        let mut clear = window(10.0);
        clear.opacity = Param::new(ParamValue::Float(0.0));
        n.windows.push(clear);
        let f = explain(&n);
        show(&f, "Node 4");
        assert_eq!((f.controls.windows, f.controls.windows_inverted), (1, 1));
        assert_eq!(f.matte_coverage, Some(0.5), "{f:?}");
        assert_eq!(f.sentence("Node 4"), "Node 4: affects 50 % of the frame; inside it, shadows and midtones +⅚ stop; highlights +⅔ stop.");
        assert_eq!(f.summary(), "+⅚ stop mids · 50 %");
        // the off windows don't change the key either
        let mut fewer = n.clone();
        fewer.windows.truncate(1);
        assert_eq!(node_state_key(&n, Tick(0), &GradeCtx::default(), R709), node_state_key(&fewer, Tick(0), &GradeCtx::default(), R709));
    }

    #[test]
    fn keyframed_window_at_a_later_time() {
        let mut n = gain(0.3);
        let mut m = window(30.0);
        m.path.put_keyframe(Tick(0), ParamValue::Path(MaskPath::rect(0.0, 0.0, 30.0, 60.0)));
        m.path.put_keyframe(Tick(1000), ParamValue::Path(MaskPath::rect(0.0, 0.0, 90.0, 60.0)));
        n.windows.push(m);
        let f = frame(R709);
        let t = Tick(1000);
        let facts = explain_node(&n, t, &f, &render_at(&n, &f, t, R709), R709);
        assert_eq!(facts.matte_coverage, Some(0.75), "{facts:?}");
        let ctx = GradeCtx::default();
        assert_ne!(node_state_key(&n, Tick(0), &ctx, R709), node_state_key(&n, t, &ctx, R709));
    }

    #[test]
    fn grain_only_node() {
        let f = explain(&node(&[("grain.strength", ParamValue::Float(0.5))]));
        show(&f, "Grain");
        assert_eq!(f.sentence("Grain"), "Grain: affects 65 % of the frame; inside it, adds grain.");
        assert_eq!(f.summary(), "grain · 65 %");
    }

    #[test]
    fn controls_in_words() {
        let mut n = node(&[
            ("lift", ParamValue::Color([0.0, 0.0, 0.1, 0.0])),
            ("gamma", ParamValue::Color([0.1, 0.0, 0.0, -0.05])),
            ("offset", ParamValue::Color([0.0, 0.1, 0.0, 0.0])),
            ("contrast", ParamValue::Float(1.2)),
            ("tint", ParamValue::Float(-10.0)),
            ("curve.master", ParamValue::Curve(vec![[0.0, 0.05], [0.25, 0.32], [0.75, 0.7], [1.0, 1.0]])),
            ("grain.strength", ParamValue::Float(0.2)),
            ("nr.luma", ParamValue::Float(0.5)),
            ("hl.rolloff", ParamValue::Float(0.4)),
        ]);
        n.lut = "builtin:x".into();
        let c = read_controls(&n, Tick(0));
        let towards: Vec<_> = c.wheels.iter().map(|w| (w.wheel.as_str(), w.towards.as_deref())).collect();
        assert_eq!(towards, vec![("lift", Some("blue")), ("gamma", Some("red")), ("offset", Some("green"))]);
        assert_eq!(c.wheels.get(1).map(|w| w.master), Some(-0.05));
        assert_eq!(c.contrast, Some([1.2, filmcraft_project::grade::DI_MID_GREY]));
        assert_eq!((c.tint, c.temperature, c.saturation), (Some(-10.0), None, None));
        assert_eq!(c.curves, vec![CurveFact { curve: "curve.master".into(), effects: vec!["lifts shadows".into(), "compresses highlights".into()] }]);
        assert_eq!((c.grain, c.noise_reduction, c.highlight_rolloff), (Some(0.2), Some([0.5, 0.0]), Some([0.4, 0.75])));
        assert_eq!(c.lut, Some(LutFact { reference: "builtin:x".into(), mix: 1.0 }));
        let f = explain(&n);
        assert!(serde_json::to_string(&f).is_ok_and(|s| s.contains("\"matteCoverage\":null") && s.contains("\"showingMatte\":false")));
        assert!(f.sentence("N").contains("adds grain and reduces noise"), "{}", f.sentence("N"));
    }

    #[test]
    fn hostile_frames_give_no_visible_change() {
        let n = gain(0.3);
        let f = frame(R709);
        let mut nan = render_at(&n, &f, Tick(0), R709);
        nan.px.iter_mut().for_each(|v| *v = f32::NAN);
        let bad = Image { w: usize::MAX, h: 2, px: Vec::new() };
        let short = Image { w: W, h: H, px: vec![0.0; 7] };
        let cases = [
            (Image::new(0, 0), Image::new(0, 0)),
            (f.clone(), Image::new(W / 2, H)),
            (f.clone(), nan.clone()),
            (nan, f.clone()),
            (bad.clone(), bad),
            (short.clone(), f.clone()),
        ];
        for (a, b) in &cases {
            for ws in WorkingSpace::ALL {
                let facts = explain_node(&n, Tick(0), a, b, ws);
                assert_eq!(facts.sentence("Node 1"), "Node 1: no visible change.");
                assert_eq!(facts.summary(), "no change");
                let o = change_overlay(a, b, ws);
                assert!(o.px.iter().all(|v| *v == 0.0) && o.px.len() == o.w * o.h * 4);
            }
        }
        // hostile node values read as clamped controls
        let mut h = node(&[("gain", ParamValue::Color([f32::NAN, 1e30, -1e30, f32::INFINITY]))]);
        h.params.insert("saturation".into(), Param::new(ParamValue::Float(f64::NAN)));
        let c = read_controls(&h, Tick(0));
        assert!(c.wheels.iter().all(|w| w.master.is_finite() && w.color_amount.is_finite()) && c.saturation.is_none());
        let ctx = GradeCtx { px_scale: f32::NAN, seq_time: Tick(i64::MIN), clip: u64::MAX, project: None };
        let _ = node_state_key(&h, Tick(i64::MIN), &ctx, R709);
    }

    #[test]
    fn large_frames_are_subsampled() {
        assert_eq!((stride(1), stride(1_000_000), stride(1_000_001), stride(3840 * 2160)), (1, 1, 2, 3));
        assert!(stride(usize::MAX) >= 1);
        // a 2000×1000 frame (2 M pixels) measures on a grid and still reads the gain
        let (w, h) = (2000, 1000);
        let a = Image::filled(w, h, [0.18, 0.18, 0.18, 1.0]);
        let n = gain(0.1);
        let b = render_at(&n, &a, Tick(0), R709);
        let f = explain_node(&n, Tick(0), &a, &b, R709);
        // (mid grey's Oklab lightness, 0.56, weighs into midtones and highlights)
        assert_eq!(f.sentence("N"), "N: affects the whole frame; midtones and highlights +⅓ stop; shadows untouched.");
    }

    #[test]
    fn disabled_nodes_and_whole_grades() {
        let mut n = gain(0.1);
        let on = explain(&n);
        n.enabled = false;
        let f = frame(R709);
        let off = explain_node(&n, Tick(0), &f, &f, R709);
        assert_eq!(off.sentence("Node 2"), "Node 2: disabled, no visible change.");
        let text = explain_grade(&[("Node 1 'Gain'".into(), on.clone()), ("Node 2".into(), off)]);
        assert_eq!(text, format!("{} Node 2: disabled, no visible change.", on.sentence("Node 1 'Gain'")));
        assert_eq!(explain_grade(&[]), "The grade has no nodes.");
    }

    #[test]
    fn overlay_shows_where_and_how_strongly() {
        let mut n = gain(0.3);
        n.windows.push(window(60.0));
        let f = frame(R709);
        let out = render_at(&n, &f, Tick(0), R709);
        let o = change_overlay(&f, &out, R709);
        assert_eq!((o.w, o.h), (W, H));
        assert_eq!(o, change_overlay(&f, &out, R709), "deterministic");
        let alpha = |x: usize, y: usize| o.px.get((y * W + x) * 4 + 3).copied().unwrap_or(-1.0);
        // inside the window (bright grey) visible; outside nothing
        assert!(alpha(50, 10) > 0.15 && alpha(100, 10) == 0.0 && alpha(100, 45) == 0.0, "{} {}", alpha(50, 10), alpha(100, 10));
        assert!(o.px.as_chunks::<4>().0.iter().all(|p| p[3] <= 1.0 && p[0] <= p[3]));
        assert!(change_overlay(&f, &f, R709).px.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn state_key_follows_the_node_state() {
        let ctx = GradeCtx::default();
        let key = |n: &GradeNode| node_state_key(n, Tick(0), &ctx, R709);
        let n = gain(0.1);
        let k = key(&n);
        assert_eq!(k, key(&n.clone()));
        // missing keys and explicit identities read the same
        assert_eq!(key(&GradeNode::new(1)), key(&node(&[("contrast", ParamValue::Float(1.0))])));
        // qualifier ranges don't matter while it's off
        assert_eq!(k, key(&node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.1])), ("qual.hue", ParamValue::Float(0.3))])));
        let mut m = n.clone();
        m.enabled = false;
        assert_ne!(k, key(&m));
        assert_ne!(k, key(&gain(0.11)));
        assert_ne!(k, node_state_key(&n, Tick(0), &ctx, DWG));
        assert_ne!(k, node_state_key(&n, Tick(0), &GradeCtx { px_scale: 0.5, ..ctx }, R709));
        let (mut la, mut lb) = (n.clone(), n.clone());
        la.lut = "builtin:a".into();
        lb.lut = "builtin:b".into();
        assert!(k != key(&la) && key(&la) != key(&lb));
        let mut w = n.clone();
        w.windows.push(window(10.0));
        let kw = key(&w);
        assert_ne!(k, kw);
        if let Some(m) = w.windows.get_mut(0) {
            m.inverted = true;
        }
        assert_ne!(kw, key(&w));
        // grain: sequence time and clip matter only with grain
        let later = GradeCtx { seq_time: Tick(99), clip: 7, ..ctx };
        assert_eq!(k, node_state_key(&n, Tick(0), &later, R709));
        let g = node(&[("grain.strength", ParamValue::Float(0.2))]);
        assert_ne!(key(&g), node_state_key(&g, Tick(0), &later, R709));
        // a keyframed parameter changes the key over time
        let mut kf = Param::new(ParamValue::Float(1.0));
        kf.put_keyframe(Tick(0), ParamValue::Float(1.0));
        kf.put_keyframe(Tick(1000), ParamValue::Float(2.0));
        let mut s = GradeNode::new(1);
        s.params.insert("saturation".into(), kf);
        assert_ne!(key(&s), node_state_key(&s, Tick(1000), &ctx, R709));
    }

    #[test]
    fn stop_and_percent_text() {
        let t: Vec<String> = [1, 2, 3, 4, 5, 6, 8, -1, -9, 12].iter().map(|n| stops_text(*n)).collect();
        assert_eq!(t, ["+⅙ stop", "+⅓ stop", "+½ stop", "+⅔ stop", "+⅚ stop", "+1 stop", "+1⅓ stops", "−⅙ stop", "−1½ stops", "+2 stops"]);
        assert_eq!((sixths(0.08), sixths(0.09), sixths(f32::NAN), sixths(1e30)), (0, 1, 0, 600));
        assert_eq!((percent_text(0.004), percent_text(0.0), percent_text(0.185)), ("<1 %".to_string(), "0 %".to_string(), "19 %".to_string()));
        assert_eq!((hue_name(0.0), hue_name(1.0 / 12.0), hue_name(2.0 / 3.0), hue_name(f32::NAN)), ("red", "orange", "blue", "red"));
        // Oklab through XYZ agrees with the colour-match module's sRGB-primaries form
        let sp = Space::new(R709);
        for c in [[1.0, 1.0, 1.0], [1.0, 0.0, 0.0], SKIN, [0.03, 0.05, 0.25]] {
            let (a, b) = (sp.lab(c), crate::color_match::oklab(c));
            assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 2e-3), "{a:?} vs {b:?}");
        }
    }
}
