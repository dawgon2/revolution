//! Grade comparison and Shot Match (GRADE-PLAN G2.8 / G2.9): the maths behind
//! `grade.compareData`, `grade.compareShots`, `grade.checkScene` and `grade.shotMatch`.
//!
//! * **Same grade?** [`same_grade`] / [`grade_fingerprint`] compare grades by the data that
//!   renders: the enabled nodes (the first [`MAX_NODES`]) in order with their parameters
//!   (keyframes included), LUT and windows. A bypassed grade renders nothing, so it equals no
//!   grade (and every other bypassed grade). Node ids, labels, window names and window tracking
//!   methods are ignored; a parameter at its schema identity (after the schema clamp,
//!   [`GradeNode::float_at`] semantics) counts as absent, a node with no effective parameter is
//!   skipped, and a dependent parameter whose control is off is ignored (pivot without contrast,
//!   grain size / softness without grain strength, qualifier ranges without `qual.on`, NR radius
//!   without NR, the knee without highlight roll-off, `lut.mix` without a LUT). Wheels compare by
//!   their per-channel sums `[r + m, g + m, b + m]` after the per-component clamp (what the
//!   renderer uses), so a master-vs-RGB split of the same push is the same grade; the switches
//!   `qual.on` / `qual.invert` compare as on / off (≥ ½). A node's LUT is its reference string
//!   (when non-empty and `lut.mix` isn't 0 at every keyframe): whether the reference resolves in
//!   the project's LUT library is not checked, so an unknown reference still counts. Floats
//!   compare exactly after the clamp. Both are built on one canonical byte encoding, hashed with
//!   FNV-1a (64 bit, no random state), so `same_grade(a, b)` ⇒ equal fingerprints.
//! * **Do the shots look matched?** [`ShotStats::measure`] samples a frame (box filter to at most
//!   [`SAMPLE_SIDE`] px on its longer side, non-finite pixels skipped) and takes the
//!   [`color_match`] statistics (Oklab; per tonal range weight / L / a / b; chroma; skin-protected
//!   colour means) plus the mean Oklab L and a 16-bin L histogram. [`compare`] reports B against
//!   A (see [`ShotComparison`] for the formulas and the notes' thresholds); [`outliers`] scores
//!   every shot of a scene against the scene's per-statistic median.
//! * **Shot Match** [`match_node`]: solves a grade node's lift / gamma / gain / offset wheels and
//!   saturation so the current frame, graded by that node ([`crate::grade::compile`], the CPU
//!   chain), has the reference's statistics. The solver is Lumetri's Apply Match
//!   Levenberg–Marquardt ([`color_match::levenberg_marquardt`]).

use filmcraft_color::ops::{ColorOp, apply_chain};
use filmcraft_color::spaces::{apply3, gamut_matrix, to_f32};
use filmcraft_color::{Gamut, WorkingSpace};
use filmcraft_project::grade::{Grade, GradeNode, MAX_NODES, MAX_WINDOWS, NODE_PARAMS, NodeParam, NodeParamKind, node_param};
use filmcraft_project::{Interpolation, Param, ParamValue};
use filmcraft_time::Tick;

use crate::Image;
use crate::color_match::{self, Stats, oklab};

// ---------------------------------------------------------------------------------------------
// Same grade?
// ---------------------------------------------------------------------------------------------

/// A deterministic 64-bit hash of what `grade` renders (FNV-1a over [`same_grade`]'s canonical
/// form): equal for grades [`same_grade`] calls equal. Stable within a build (window shapes are
/// encoded through their serde form).
pub fn grade_fingerprint(grade: &Grade) -> u64 {
    canonical(grade).iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// True when `a` and `b` render identically by their data (see the module docs for what is
/// compared and what is ignored).
pub fn same_grade(a: &Grade, b: &Grade) -> bool {
    canonical(a) == canonical(b)
}

#[derive(Default)]
struct Canon(Vec<u8>);

impl Canon {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        // −0 and +0 render alike
        let v = if v == 0.0 { 0.0f32 } else { v };
        self.0.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.0.extend_from_slice(b);
    }
}

/// The parameter a dependent parameter needs to have any effect.
fn controlling_key(key: &str) -> Option<&'static str> {
    match key {
        "pivot" => Some("contrast"),
        "grain.size" | "grain.softness" => Some("grain.strength"),
        "hl.knee" => Some("hl.rolloff"),
        "qual.on" => None,
        k if k.starts_with("qual.") => Some("qual.on"),
        _ => None,
    }
}

fn canonical(grade: &Grade) -> Vec<u8> {
    let mut c = Canon::default();
    c.u8(b'G');
    if grade.bypass {
        // a bypassed grade renders as no grade at all
        return c.0;
    }
    for n in grade.nodes.iter().take(MAX_NODES).filter(|n| n.enabled) {
        let encoded: Vec<(&'static str, Vec<u8>)> = NODE_PARAMS.iter().filter_map(|p| canon_param(n, p).map(|e| (p.key, e))).collect();
        let present = |k: &str| encoded.iter().any(|(key, _)| *key == k);
        let nr_on = present("nr.luma") || present("nr.chroma");
        let lut_on = !n.lut.is_empty() && lut_mix_on(n);
        let mut nc = Canon::default();
        let mut any = false;
        if lut_on {
            any = true;
            nc.bytes(b"lut");
            nc.bytes(n.lut.as_bytes());
        }
        for (key, e) in &encoded {
            let live = match *key {
                "nr.size" => nr_on,
                "lut.mix" => lut_on,
                k => controlling_key(k).is_none_or(present),
            };
            if live {
                any = true;
                nc.bytes(key.as_bytes());
                nc.bytes(e);
            }
        }
        if !any {
            continue;
        }
        c.u8(b'N');
        c.bytes(&nc.0);
        for w in n.windows.iter().take(MAX_WINDOWS) {
            let mut w = w.clone();
            w.name.clear();
            w.track_method = Default::default();
            let enc = serde_json::to_vec(&w).unwrap_or_else(|_| format!("{w:?}").into_bytes());
            c.u8(b'W');
            c.bytes(&enc);
        }
        c.u8(b'E');
    }
    c.0
}

/// Whether a node's `lut.mix` is above 0 anywhere (missing: the identity, 1).
fn lut_mix_on(n: &GradeNode) -> bool {
    let Some(p) = node_param("lut.mix") else { return false };
    let Some(param) = n.params.get("lut.mix") else { return true };
    let on = |v: &ParamValue| canon_value(p.key, p.kind, v)[0] > 0.0;
    if param.keyframes.is_empty() { on(&param.value) } else { param.keyframes.iter().any(|k| on(&k.value)) }
}

/// Switch parameters: the renderer reads them as on when ≥ ½.
const SWITCHES: [&str; 2] = ["qual.on", "qual.invert"];

/// A parameter's canonical value as `[f32; 4]`: a number in the first slot (a switch as 0 / 1),
/// a wheel as its per-channel sums `[r + m, g + m, b + m, 0]` after the per-component clamp.
fn canon_value(key: &str, kind: NodeParamKind, v: &ParamValue) -> [f32; 4] {
    match kind {
        NodeParamKind::Float { identity, min, max } => {
            let x = match v {
                ParamValue::Float(x) if x.is_finite() => (*x as f32).clamp(min, max),
                _ => identity,
            };
            let x = if SWITCHES.contains(&key) { if x >= 0.5 { 1.0 } else { 0.0 } } else { x };
            [x, 0.0, 0.0, 0.0]
        }
        NodeParamKind::Wheel { range } => match v {
            ParamValue::Color(c) => {
                let c = c.map(|v| if v.is_finite() { v.clamp(-range, range) } else { 0.0 });
                [c[0] + c[3], c[1] + c[3], c[2] + c[3], 0.0]
            }
            _ => [0.0; 4],
        },
        NodeParamKind::Curve => [0.0; 4],
    }
}

fn identity_value(key: &str, kind: NodeParamKind) -> [f32; 4] {
    match kind {
        NodeParamKind::Float { identity, .. } => canon_value(key, kind, &ParamValue::Float(identity as f64)),
        _ => [0.0; 4],
    }
}

fn interp_tag(i: Interpolation) -> u8 {
    Interpolation::ALL.iter().position(|x| *x == i).map_or(255, |p| p as u8)
}

fn canon_param(n: &GradeNode, p: &NodeParam) -> Option<Vec<u8>> {
    let param: &Param = n.params.get(p.key)?;
    let mut c = Canon::default();
    if p.kind == NodeParamKind::Curve {
        // curves render from their static value (`GradeNode::curve`)
        let pts = n.curve(p.key)?;
        c.u8(b'C');
        for q in pts {
            c.f32(q[0]);
            c.f32(q[1]);
        }
        return Some(c.0);
    }
    let id = identity_value(p.key, p.kind);
    let put = |c: &mut Canon, v: [f32; 4]| v.iter().for_each(|x| c.f32(*x));
    if param.keyframes.is_empty() {
        let v = canon_value(p.key, p.kind, &param.value);
        if v == id {
            return None;
        }
        c.u8(b'S');
        put(&mut c, v);
        return Some(c.0);
    }
    let vals: Vec<[f32; 4]> = param.keyframes.iter().map(|k| canon_value(p.key, p.kind, &k.value)).collect();
    if vals.iter().all(|v| *v == id) {
        return None;
    }
    let first = vals.first().copied().unwrap_or(id);
    if vals.iter().all(|v| *v == first) {
        // keyframes that all hold one value render like that static value
        c.u8(b'S');
        put(&mut c, first);
        return Some(c.0);
    }
    c.u8(b'K');
    for (k, v) in param.keyframes.iter().zip(&vals) {
        c.u64(k.time.0 as u64);
        c.u8(interp_tag(k.interp));
        c.u64(k.out_influence.to_bits());
        c.u64(k.in_influence.to_bits());
        put(&mut c, *v);
    }
    Some(c.0)
}

// ---------------------------------------------------------------------------------------------
// Shot statistics and comparison
// ---------------------------------------------------------------------------------------------

/// Frames are box-filtered to at most this many pixels on their longer side before measuring.
pub const SAMPLE_SIDE: usize = 128;
/// Bins of the lightness histogram (Oklab L over 0…1; brighter pixels land in the last bin).
pub const HIST_BINS: usize = 16;

/// A tonal range counts as populated in both shots when each has at least this fraction of its
/// opaque pixels in it (the smaller of the two range weights).
pub const RANGE_MIN: f32 = 0.05;
/// Just-noticeable exposure difference (stops): smaller differences are not mentioned.
pub const JND_STOPS: f32 = 0.1;
/// Just-noticeable colour cast (Oklab a / b units): "slightly" from here, plain from 3×, "much"
/// from 7.5×.
pub const JND_CAST: f32 = 0.004;
/// Just-noticeable saturation difference (percent of chroma).
pub const JND_SAT_PCT: f32 = 5.0;
/// Below this tonal overlap the comparison says it is rough.
pub const LOW_OVERLAP: f32 = 0.5;

/// What one frame looks like, for comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct ShotStats {
    /// The Apply Match statistics ([`color_match::stats`], skin-protected colour means).
    pub stats: Stats,
    /// Mean Oklab L of the opaque pixels.
    pub mean_l: f32,
    /// Fraction of sampled pixels that are opaque (> 0.001 alpha).
    pub coverage: f32,
    /// Normalised Oklab L histogram of the opaque pixels (sums to 1).
    pub histogram: [f32; HIST_BINS],
}

impl ShotStats {
    /// Measure a frame in Rec. 709-primaries working linear light (premultiplied RGBA). `None`
    /// for an empty, fully transparent or non-finite frame.
    pub fn measure(img: &Image) -> Option<ShotStats> {
        Self::measure_in(img, WorkingSpace::Rec709)
    }

    /// Measure a frame in `working`'s linear light (`RenderOptions { working_output: true }`);
    /// its primaries are converted to Rec. 709 for Oklab. Working linear light is 1.0 = reference
    /// white in every working space (SDR white; 203 cd/m² in PQ / HLG, `filmcraft_color`'s
    /// transform docs), so SDR, DWG and HDR shots measure on one scale with no extra
    /// normalisation: HDR highlights are L > 1 (the histogram's last bin), and wide-gamut colours
    /// outside Rec. 709 lose chroma (negative components clip in Oklab), so HDR / wide-gamut
    /// saturation reads low against what a viewer sees.
    pub fn measure_in(img: &Image, working: WorkingSpace) -> Option<ShotStats> {
        let s = to_rec709(sample(img, SAMPLE_SIDE)?, working);
        from_sample(&s, true)
    }
}

/// One tonal range of a comparison (B against A).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RangeDiff {
    /// The smaller of the two shots' fractions of opaque pixels in this range.
    pub shared: f32,
    /// Both shots populate the range (`shared ≥ RANGE_MIN`); only then are its numbers used.
    pub populated: bool,
    /// 3 · log2(L_b / L_a) of the range's mean Oklab L (positive: B brighter).
    pub exposure_stops: f32,
    /// Δ Oklab b (positive: B warmer / yellower).
    pub warmth: f32,
    /// Δ Oklab a (positive: B more magenta, negative: greener).
    pub tint: f32,
}

/// B compared with A.
///
/// * `exposure_stops = 3 · log2(mean L_b / mean L_a)` (Oklab L is close to the cube root of
///   luminance, exactly for a uniform gain), clamped to ±10.
/// * `warmth` / `tint`: Δ Oklab b / a, averaged over the populated ranges weighted by `shared`.
/// * `saturation_pct = (r_b / r_a − 1) · 100` with r = mean chroma / mean L (exposure-independent:
///   both scale by ∛gain), mean L floored at 0.01, r_a at 0.01; clamped to ±1000.
/// * `overlap = Σ min(hist_a, hist_b)` over the L histograms (1: the same tonal distribution).
/// * `score = 100 · exp(−0.03 · d²)` with d the difference in just-noticeable units:
///   d² = (stops / [`JND_STOPS`])² + (warmth / [`JND_CAST`])² + (tint / [`JND_CAST`])² +
///   (saturation / [`JND_SAT_PCT`])² + Σ populated ranges `shared` · ((range warmth − warmth)² +
///   (range tint − tint)²) / [`JND_CAST`]². One JND gives ≈ 97, ⅓ stop alone ≈ 72.
/// * `notes`: exposure from [`JND_STOPS`] ("B is about ⅓ stop darker"), cast from [`JND_CAST`]
///   ("B is slightly warmer"; a populated range is named when the whole shot isn't, or when it
///   goes the other way: "B's shadows are slightly cooler"), saturation from [`JND_SAT_PCT`] ("B
///   is 8 % less saturated"), and a caveat when `overlap` < [`LOW_OVERLAP`].
#[derive(Clone, Debug, PartialEq)]
pub struct ShotComparison {
    pub score: f32,
    pub exposure_stops: f32,
    pub warmth: f32,
    pub tint: f32,
    pub saturation_pct: f32,
    /// Shadows, midtones, highlights.
    pub per_range: [RangeDiff; 3],
    pub overlap: f32,
    pub notes: Vec<String>,
}

const RANGE_NAMES: [&str; 3] = ["shadows", "midtones", "highlights"];

fn stops_between(la: f32, lb: f32) -> f32 {
    let (la, lb) = (la.max(1e-4), lb.max(1e-4));
    let s = 3.0 * (lb / la).log2();
    if s.is_finite() { s.clamp(-10.0, 10.0) } else { 0.0 }
}

fn finite_or_zero(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// Compare shot `b` with shot `a` (see [`ShotComparison`]).
pub fn compare(a: &ShotStats, b: &ShotStats) -> ShotComparison {
    let exposure_stops = stops_between(a.mean_l, b.mean_l);
    let frac = |s: &ShotStats, k: usize| s.stats.bands.get(k).map_or(0.0, |b| b[0] / s.coverage.max(1e-6));
    let mut per_range = [RangeDiff::default(); 3];
    for (k, r) in per_range.iter_mut().enumerate() {
        let (Some(ba), Some(bb)) = (a.stats.bands.get(k), b.stats.bands.get(k)) else { continue };
        let shared = finite_or_zero(frac(a, k).min(frac(b, k))).clamp(0.0, 1.0);
        *r = RangeDiff {
            shared,
            populated: shared >= RANGE_MIN,
            exposure_stops: stops_between(ba[1], bb[1]),
            warmth: finite_or_zero(bb[3] - ba[3]),
            tint: finite_or_zero(bb[2] - ba[2]),
        };
    }
    let used: Vec<&RangeDiff> = per_range.iter().filter(|r| r.populated).collect();
    let (wsum, warm, tint) = if used.is_empty() {
        // nothing shared enough: fall back to every range by its shared weight
        per_range.iter().fold((0.0, 0.0, 0.0), |(w, x, y), r| (w + r.shared, x + r.shared * r.warmth, y + r.shared * r.tint))
    } else {
        used.iter().fold((0.0, 0.0, 0.0), |(w, x, y), r| (w + r.shared, x + r.shared * r.warmth, y + r.shared * r.tint))
    };
    let (warmth, tint) = if wsum > 1e-6 { (finite_or_zero(warm / wsum), finite_or_zero(tint / wsum)) } else { (0.0, 0.0) };
    // chroma relative to lightness: Oklab chroma grows with exposure (both scale by ∛gain)
    let rel = |s: &ShotStats| s.stats.chroma / s.mean_l.max(0.01);
    let saturation_pct = finite_or_zero((rel(b) / rel(a).max(0.01) - 1.0) * 100.0).clamp(-1000.0, 1000.0);
    let overlap = a.histogram.iter().zip(&b.histogram).map(|(x, y)| x.min(*y)).sum::<f32>();
    let overlap = finite_or_zero(overlap).clamp(0.0, 1.0);

    let mut d2 = (exposure_stops / JND_STOPS).powi(2) + (warmth / JND_CAST).powi(2) + (tint / JND_CAST).powi(2) + (saturation_pct / JND_SAT_PCT).powi(2);
    for r in &used {
        d2 += r.shared * (((r.warmth - warmth) / JND_CAST).powi(2) + ((r.tint - tint) / JND_CAST).powi(2));
    }
    let score = finite_or_zero(100.0 * (-0.03 * d2).exp()).clamp(0.0, 100.0);

    let mut notes = Vec::new();
    if exposure_stops.abs() >= JND_STOPS {
        notes.push(format!("B is about {} {}", stops_words(exposure_stops), if exposure_stops > 0.0 { "brighter" } else { "darker" }));
    }
    cast_notes(&mut notes, warmth, &per_range, |r| r.warmth, ("warmer", "cooler"));
    cast_notes(&mut notes, tint, &per_range, |r| r.tint, ("more magenta", "greener"));
    if saturation_pct.abs() >= JND_SAT_PCT {
        notes.push(format!("B is {:.0} % {} saturated", saturation_pct.abs(), if saturation_pct > 0.0 { "more" } else { "less" }));
    }
    if overlap < LOW_OVERLAP {
        notes.push(format!("the shots share only {:.0} % of their tonal range, so this is a rough guide", overlap * 100.0));
    }
    ShotComparison { score, exposure_stops, warmth, tint, saturation_pct, per_range, overlap, notes }
}

fn degree(v: f32) -> &'static str {
    let a = v.abs();
    if a >= 7.5 * JND_CAST {
        "much "
    } else if a >= 3.0 * JND_CAST {
        ""
    } else {
        "slightly "
    }
}

fn cast_notes(notes: &mut Vec<String>, global: f32, ranges: &[RangeDiff; 3], get: impl Fn(&RangeDiff) -> f32, words: (&str, &str)) {
    let word = |v: f32| if v > 0.0 { words.0 } else { words.1 };
    let global_noted = global.abs() >= JND_CAST;
    if global_noted {
        notes.push(format!("B is {}{}", degree(global), word(global)));
    }
    for (r, name) in ranges.iter().zip(RANGE_NAMES) {
        let v = get(r);
        if !r.populated || v.abs() < JND_CAST {
            continue;
        }
        if !global_noted {
            notes.push(format!("{name} {}{}", degree(v), word(v)));
        } else if v.signum() != global.signum() {
            notes.push(format!("B's {name} are {}{}", degree(v), word(v)));
        }
    }
}

/// "⅓ stop", "1 stop", "1⅔ stops", rounded to sixths (at least ⅙).
fn stops_words(s: f32) -> String {
    let a = s.abs();
    if !a.is_finite() || a > 4.0 {
        return format!("{:.1} stops", a.min(99.0));
    }
    let n = ((a * 6.0).round() as u32).max(1);
    let (whole, rem) = (n / 6, n % 6);
    let frac = ["", "⅙", "⅓", "½", "⅔", "⅚"].get(rem as usize).copied().unwrap_or("");
    let num = match (whole, frac) {
        (0, f) => f.to_string(),
        (w, "") => w.to_string(),
        (w, f) => format!("{w}{f}"),
    };
    format!("{num} {}", if n <= 6 { "stop" } else { "stops" })
}

/// The most shots a scene check looks at.
pub const MAX_SCENE_SHOTS: usize = 4096;

fn median(mut v: Vec<f32>) -> f32 {
    v.retain(|x| x.is_finite());
    v.sort_by(f32::total_cmp);
    let n = v.len();
    match (v.get(n.saturating_sub(1) / 2), v.get(n / 2)) {
        (Some(a), Some(b)) => (a + b) * 0.5,
        _ => 0.0,
    }
}

/// The scene's consensus: every statistic's median over `stats`.
pub fn consensus(stats: &[ShotStats]) -> Option<ShotStats> {
    let stats = stats.get(..stats.len().min(MAX_SCENE_SHOTS))?;
    if stats.is_empty() {
        return None;
    }
    let med = |f: &dyn Fn(&ShotStats) -> f32| median(stats.iter().map(f).collect());
    let bands = std::array::from_fn(|k| std::array::from_fn(|c| med(&|s| s.stats.bands.get(k).and_then(|b| b.get(c)).copied().unwrap_or(0.0))));
    let skin = std::array::from_fn(|c| med(&|s| s.stats.skin.get(c).copied().unwrap_or(0.0)));
    let mut histogram: [f32; HIST_BINS] = std::array::from_fn(|i| med(&|s| s.histogram.get(i).copied().unwrap_or(0.0)));
    let total: f32 = histogram.iter().sum();
    if total > 1e-9 {
        histogram.iter_mut().for_each(|h| *h /= total);
    }
    Some(ShotStats { stats: Stats { bands, chroma: med(&|s| s.stats.chroma), skin }, mean_l: med(&|s| s.mean_l), coverage: med(&|s| s.coverage), histogram })
}

/// Scene check: every shot's difference from the scene's consensus ([`consensus`], the median
/// shot) as `(index, 100 − compare(consensus, shot).score)`, most different first. Only the first
/// [`MAX_SCENE_SHOTS`] shots are looked at.
pub fn outliers(stats: &[ShotStats]) -> Vec<(usize, f32)> {
    let Some(c) = consensus(stats) else { return Vec::new() };
    let mut out: Vec<(usize, f32)> =
        stats.iter().take(MAX_SCENE_SHOTS).enumerate().map(|(i, s)| (i, (100.0 - compare(&c, s).score).clamp(0.0, 100.0))).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Box-filter `img` to at most `max_side` px on its longer side, skipping non-finite pixels
/// (alpha clamped to 0…1, colour to ±1e6). `None` when the image is empty or its buffer is
/// shorter than `w × h × 4`.
fn sample(img: &Image, max_side: usize) -> Option<Image> {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 {
        return None;
    }
    let px = img.px.get(..w.checked_mul(h)?.checked_mul(4)?)?;
    let side = max_side.max(1);
    let block = w.div_ceil(side).max(h.div_ceil(side)).max(1);
    let (sw, sh) = (w.div_ceil(block), h.div_ceil(block));
    let mut acc = vec![[0f64; 5]; sw * sh];
    for (i, p) in px.as_chunks::<4>().0.iter().enumerate() {
        if !p.iter().all(|v| v.is_finite()) {
            continue;
        }
        let (x, y) = (i % w, i / w);
        if let Some(c) = acc.get_mut((y / block) * sw + x / block) {
            for (a, v) in c.iter_mut().zip(p) {
                *a += *v as f64;
            }
            c[4] += 1.0;
        }
    }
    let mut out = Image::new(sw, sh);
    for (o, c) in out.px.as_chunks_mut::<4>().0.iter_mut().zip(&acc) {
        if c[4] <= 0.0 {
            continue;
        }
        let alpha = (c[3] / c[4]).clamp(0.0, 1.0) as f32;
        for k in 0..3 {
            o[k] = (c[k] / c[4]).clamp(-1e6, 1e6) as f32;
        }
        o[3] = alpha;
    }
    Some(out)
}

fn working_gamut(working: WorkingSpace) -> Gamut {
    match working {
        WorkingSpace::Rec709 => Gamut::Bt709,
        WorkingSpace::Rec2100Pq | WorkingSpace::Rec2100Hlg => Gamut::Bt2020,
        WorkingSpace::DaVinciWide => Gamut::DaVinciWide,
    }
}

/// Convert working linear light to Rec. 709 primaries (premultiplied, so the matrix applies as is).
fn to_rec709(mut img: Image, working: WorkingSpace) -> Image {
    let g = working_gamut(working);
    if g != Gamut::Bt709 {
        let m = to_f32(&gamut_matrix(g, Gamut::Bt709));
        for p in img.px.as_chunks_mut::<4>().0 {
            let c = apply3(&m, [p[0], p[1], p[2]]);
            p[..3].copy_from_slice(&c);
        }
    }
    img
}

/// Statistics of a sampled, sanitised Rec. 709 frame.
fn from_sample(s: &Image, skin_protect: bool) -> Option<ShotStats> {
    let mut n = 0f64;
    let mut opaque = 0f64;
    let mut sum_l = 0f64;
    let mut hist = [0f64; HIST_BINS];
    for p in s.px.as_chunks::<4>().0 {
        n += 1.0;
        if p[3] <= 1e-3 {
            continue;
        }
        let l = oklab([p[0] / p[3], p[1] / p[3], p[2] / p[3]])[0];
        if !l.is_finite() {
            continue;
        }
        opaque += 1.0;
        sum_l += l as f64;
        let bin = ((l.max(0.0) * HIST_BINS as f32) as usize).min(HIST_BINS - 1);
        if let Some(h) = hist.get_mut(bin) {
            *h += 1.0;
        }
    }
    if opaque < 1.0 {
        return None;
    }
    let stats = color_match::stats(s, skin_protect);
    let finite = stats.bands.iter().flatten().chain(&stats.skin).all(|v| v.is_finite()) && stats.chroma.is_finite();
    let out = ShotStats { stats, mean_l: (sum_l / opaque) as f32, coverage: (opaque / n.max(1.0)) as f32, histogram: hist.map(|h| (h / opaque) as f32) };
    (finite && out.mean_l.is_finite()).then_some(out)
}

// ---------------------------------------------------------------------------------------------
// Shot Match
// ---------------------------------------------------------------------------------------------

/// The node parameters Shot Match solved.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeMatch {
    /// `lift`, `gamma`, `gain`, `offset` (`ParamValue::Color([r, g, b, master])`) and
    /// `saturation` (`ParamValue::Float`), each within its schema range ([`node_param`]).
    pub params: Vec<(&'static str, ParamValue)>,
    /// Statistics distance (residual norm) before and after.
    pub before: f32,
    pub after: f32,
}

impl NodeMatch {
    /// A grade node with id `id` holding the solved parameters.
    pub fn node(&self, id: u64) -> GradeNode {
        let mut n = GradeNode::new(id);
        for (k, v) in &self.params {
            n.params.insert((*k).to_string(), Param::new(v.clone()));
        }
        n
    }

    fn identity() -> Self {
        Self::from_x(&[0.0; UNKNOWNS], 0.0, 0.0)
    }

    fn from_x(x: &[f32; UNKNOWNS], before: f32, after: f32) -> Self {
        let mut params: Vec<(&'static str, ParamValue)> = WHEELS
            .iter()
            .enumerate()
            .map(|(w, &(key, scale))| {
                let range = wheel_range(key);
                let rgb: [f32; 3] = std::array::from_fn(|c| finite_or_zero(x.get(w * 3 + c).copied().unwrap_or(0.0) * scale).clamp(-range, range));
                // move the common part to the master ring when that stays in range (renders the same)
                let m = (rgb[0] + rgb[1] + rgb[2]) / 3.0;
                let split = rgb.map(|v| v - m);
                let value = if split.iter().all(|v| v.abs() <= range) { [split[0], split[1], split[2], m] } else { [rgb[0], rgb[1], rgb[2], 0.0] };
                // tiny values are noise of the solver: show a clean wheel
                (key, ParamValue::Color(value.map(|v| if v.abs() < 1e-4 { 0.0 } else { v })))
            })
            .collect();
        let sat = saturation_of(x);
        params.push(("saturation", ParamValue::Float(if (sat - 1.0).abs() < 1e-4 { 1.0 } else { sat as f64 })));
        Self { params, before, after }
    }
}

/// The solved wheels and the scale of their unknowns (unknown × scale = wheel offset).
const WHEELS: [(&str, f32); 4] = [("lift", 0.25), ("gamma", 0.5), ("gain", 0.5), ("offset", 0.1)];
/// Unknowns: 4 wheels × RGB, saturation.
const UNKNOWNS: usize = 13;
/// Saturation = 1 + unknown × this.
const SAT_SCALE: f32 = 0.5;
/// Pixels on the longer side of the frames Shot Match grades per evaluation.
const MATCH_SIDE: usize = 96;

fn wheel_range(key: &str) -> f32 {
    match node_param(key).map(|p| p.kind) {
        Some(NodeParamKind::Wheel { range }) => range,
        _ => 1.0,
    }
}

fn sat_range() -> (f32, f32) {
    match node_param("saturation").map(|p| p.kind) {
        Some(NodeParamKind::Float { min, max, .. }) => (min, max),
        _ => (0.0, 4.0),
    }
}

fn saturation_of(x: &[f32; UNKNOWNS]) -> f32 {
    let (lo, hi) = sat_range();
    finite_or_zero(1.0 + x.get(UNKNOWNS - 1).copied().unwrap_or(0.0) * SAT_SCALE).clamp(lo, hi)
}

fn bounds() -> [(f32, f32); UNKNOWNS] {
    let (slo, shi) = sat_range();
    std::array::from_fn(|i| match WHEELS.get(i / 3) {
        Some(&(key, scale)) => {
            let r = wheel_range(key) / scale;
            (-r, r)
        }
        None => ((slo - 1.0) / SAT_SCALE, (shi - 1.0) / SAT_SCALE),
    })
}

/// `img` (premultiplied) through `ops`, colour unpremultiplied around the chain.
fn apply_ops(img: &Image, ops: &[ColorOp]) -> Image {
    let mut out = img.clone();
    if ops.is_empty() {
        return out;
    }
    for p in out.px.as_chunks_mut::<4>().0 {
        let a = p[3];
        if a <= 1e-3 {
            continue;
        }
        let c = apply_chain(ops, [p[0] / a, p[1] / a, p[2] / a]);
        for k in 0..3 {
            p[k] = if c[k].is_finite() { c[k] * a } else { 0.0 };
        }
    }
    out
}

/// `img` (working linear light) graded by `node` alone, on the CPU ([`crate::grade::compile`]).
pub fn grade_with_node(img: &Image, node: &GradeNode, working: WorkingSpace) -> Image {
    let grade = Grade { nodes: vec![node.clone()], bypass: false, ..Default::default() };
    apply_ops(img, &crate::grade::compile(&grade, Tick(0), working))
}

/// Shot Match: the lift / gamma / gain / offset wheels and saturation of a grade node that make
/// `current` (working linear light, `RenderOptions { working_output: true }`) look like
/// `reference` by the Apply Match statistics. Both frames are box-filtered to ≤ 96 px; the node is
/// evaluated through [`crate::grade::compile`]. Residuals: Apply Match's (per tonal range L, a, b
/// weighted by the range's shared coverage; chroma) plus the mean L (×4, exposure). Solved by
/// [`color_match::levenberg_marquardt`] (Tikhonov λ = 0.01 on the scaled unknowns: lift 0.25,
/// gamma 0.5, gain 0.5, offset 0.1 per unit, saturation 1 + 0.5 · unit), within the schema
/// ranges. `skin` down-weights skin tones in the colour statistics (as Apply Match does). An
/// unusable frame (empty, transparent, non-finite) gives the identity.
pub fn match_node(current: &Image, reference: &Image, working: WorkingSpace, skin: bool) -> NodeMatch {
    let Some(cur) = sample(current, MATCH_SIDE) else { return NodeMatch::identity() };
    let Some(refs) = sample(reference, MATCH_SIDE).and_then(|r| from_sample(&to_rec709(r, working), skin)) else {
        return NodeMatch::identity();
    };
    if from_sample(&to_rec709(cur.clone(), working), skin).is_none() {
        return NodeMatch::identity();
    }
    let residuals = |x: &[f32; UNKNOWNS]| -> Vec<f32> {
        let node = NodeMatch::from_x(x, 0.0, 0.0).node(1);
        let graded = to_rec709(grade_with_node(&cur, &node, working), working);
        match from_sample(&graded, skin) {
            Some(s) => {
                let mut r = color_match::residuals(&s.stats, &refs.stats);
                r.push((s.mean_l - refs.mean_l) * 4.0);
                r.iter_mut().for_each(|v| *v = finite_or_zero(*v).clamp(-1e3, 1e3));
                r
            }
            // a candidate that blacks the frame out is as far as it gets
            None => vec![1e3; 11],
        }
    };
    let (x, before, after) = color_match::levenberg_marquardt([0.0; UNKNOWNS], bounds(), 0.01, residuals);
    NodeMatch::from_x(&x, finite_or_zero(before).sqrt(), finite_or_zero(after).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{Keyframe, Mask, MaskPath};

    // ---- same grade ----

    fn node(id: u64, params: &[(&str, ParamValue)]) -> GradeNode {
        let mut n = GradeNode::new(id);
        for (k, v) in params {
            n.params.insert((*k).into(), Param::new(v.clone()));
        }
        n
    }

    fn g(nodes: Vec<GradeNode>) -> Grade {
        Grade { nodes, ..Default::default() }
    }

    fn assert_same(a: &Grade, b: &Grade, same: bool) {
        assert_eq!(same_grade(a, b), same, "{a:?}\n{b:?}");
        if same {
            assert_eq!(grade_fingerprint(a), grade_fingerprint(b));
        } else {
            assert_ne!(grade_fingerprint(a), grade_fingerprint(b));
        }
    }

    #[test]
    fn ids_labels_and_identity_params_are_ignored() {
        let lift = ("lift", ParamValue::Color([0.1, 0.0, -0.05, 0.0]));
        let a = g(vec![node(1, &[lift.clone(), ("saturation", ParamValue::Float(1.2))])]);
        let mut b = g(vec![node(9, &[lift.clone(), ("saturation", ParamValue::Float(1.2)), ("contrast", ParamValue::Float(1.0))])]);
        b.nodes[0].label = "Primaries".into();
        b.nodes[0].params.insert("gamma".into(), Param::new(ParamValue::Color([0.0; 4])));
        b.nodes[0].params.insert("unknown.key".into(), Param::new(ParamValue::Float(3.0)));
        b.nodes[0].params.insert("pivot".into(), Param::new(ParamValue::Float(0.2))); // no contrast: no effect
        assert_same(&a, &b, true);
        // an empty node and a disabled node change nothing
        let mut c = b.clone();
        c.nodes.insert(0, GradeNode::new(4));
        let mut off = node(5, &[("gain", ParamValue::Color([1.0, 0.0, 0.0, 0.0]))]);
        off.enabled = false;
        c.nodes.push(off);
        assert_same(&a, &c, true);
        assert_same(&Grade::default(), &Grade::with_node(), true);
        // values clamp to the schema first: 1e9 and 4 (the max) are the same contrast
        let x = g(vec![node(1, &[("contrast", ParamValue::Float(1e9))])]);
        let y = g(vec![node(1, &[("contrast", ParamValue::Float(4.0))])]);
        assert_same(&x, &y, true);
        // NaN is the identity
        assert_same(&g(vec![node(1, &[("saturation", ParamValue::Float(f64::NAN))])]), &Grade::default(), true);
    }

    #[test]
    fn real_differences_are_seen() {
        let a = g(vec![node(1, &[("lift", ParamValue::Color([0.1, 0.0, 0.0, 0.0]))])]);
        let b = g(vec![node(1, &[("lift", ParamValue::Color([0.1, 0.0, 0.0, 1e-6]))])]);
        assert_same(&a, &b, false);
        // order matters
        let s = node(2, &[("saturation", ParamValue::Float(0.5))]);
        let ab = g(vec![a.nodes[0].clone(), s.clone()]);
        let ba = g(vec![s, a.nodes[0].clone()]);
        assert_same(&ab, &ba, false);
        // a bypassed grade is no grade
        let mut byp = ab.clone();
        byp.bypass = true;
        assert_same(&ab, &byp, false);
        assert_same(&byp, &Grade::default(), true);
        assert_same(&byp, &Grade { nodes: vec![GradeNode::new(3)], bypass: true, ..Default::default() }, true);
        // windows matter, their names don't
        let mut w1 = ab.clone();
        let mut m = Mask::new("Window 1", MaskPath::default());
        w1.nodes[0].windows.push(m.clone());
        assert_same(&ab, &w1, false);
        let mut w2 = ab.clone();
        m.name = "Face".into();
        w2.nodes[0].windows.push(m.clone());
        w2.nodes[0].id = 77;
        assert_same(&w1, &w2, true);
        m.track_method = filmcraft_project::TrackMethod::Position;
        let mut w4 = ab.clone();
        w4.nodes[0].windows.push(m.clone());
        assert_same(&w1, &w4, true);
        m.inverted = true;
        let mut w3 = ab.clone();
        w3.nodes[0].windows.push(m);
        assert_same(&w1, &w3, false);
    }

    #[test]
    fn luts_roll_off_and_switches_compare_by_what_renders() {
        // a LUT counts while its mix isn't 0 everywhere; the mix only with a LUT
        let mut l = node(1, &[]);
        l.lut = "builtin:look-a".into();
        assert_same(&g(vec![l.clone()]), &Grade::default(), false);
        let mut other = l.clone();
        other.lut = "builtin:look-b".into();
        assert_same(&g(vec![l.clone()]), &g(vec![other]), false);
        let mut half = l.clone();
        half.params.insert("lut.mix".into(), Param::new(ParamValue::Float(0.5)));
        assert_same(&g(vec![l.clone()]), &g(vec![half.clone()]), false);
        let mut off = l.clone();
        off.params.insert("lut.mix".into(), Param::new(ParamValue::Float(0.0)));
        assert_same(&g(vec![off]), &Grade::default(), true);
        let mut anim = l.clone();
        let mut mix = Param::new(ParamValue::Float(0.0));
        mix.put_keyframe(Tick(0), ParamValue::Float(0.0));
        mix.put_keyframe(Tick(100), ParamValue::Float(0.0));
        anim.params.insert("lut.mix".into(), mix.clone());
        assert_same(&g(vec![anim.clone()]), &Grade::default(), true);
        mix.put_keyframe(Tick(200), ParamValue::Float(1.0));
        anim.params.insert("lut.mix".into(), mix);
        assert_same(&g(vec![anim]), &Grade::default(), false);
        let no_lut = node(1, &[("lut.mix", ParamValue::Float(0.5))]);
        assert_same(&g(vec![no_lut]), &Grade::default(), true);
        // the knee only with roll-off
        assert_same(&g(vec![node(1, &[("hl.knee", ParamValue::Float(0.6))])]), &Grade::default(), true);
        let ro = |k: f64| g(vec![node(1, &[("hl.rolloff", ParamValue::Float(0.5)), ("hl.knee", ParamValue::Float(k))])]);
        assert_same(&ro(0.6), &ro(0.8), false);
        // switches are on / off at ½
        let q = |on: f64, inv: f64| {
            g(vec![node(1, &[("qual.on", ParamValue::Float(on)), ("qual.invert", ParamValue::Float(inv)), ("qual.hue", ParamValue::Float(0.3))])])
        };
        assert_same(&q(0.6, 0.0), &q(1.0, 0.4), true);
        assert_same(&q(0.6, 0.0), &q(0.6, 0.5), false);
        assert_same(&q(0.4, 1.0), &Grade::default(), true);
    }

    #[test]
    fn wheels_compare_by_their_channel_sums() {
        let a = g(vec![node(1, &[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.2]))])]);
        let b = g(vec![node(2, &[("gain", ParamValue::Color([0.2, 0.2, 0.2, 0.0]))])]);
        assert_same(&a, &b, true);
        // a Shot Match node (common part on the master ring) and its RGB-only twin
        let reference = frame();
        let current = scaled(&reference, [THIRD * WARM[0], THIRD * WARM[1], THIRD * WARM[2]]);
        let m = match_node(&current, &reference, WorkingSpace::Rec709, false);
        let solved = m.node(1);
        let mut twin = solved.clone();
        twin.id = 8;
        let mut split = false;
        for key in ["lift", "gamma", "gain", "offset"] {
            let c = wheel(&m, key);
            split |= c[3] != 0.0;
            twin.params.insert(key.into(), Param::new(ParamValue::Color([c[0] + c[3], c[1] + c[3], c[2] + c[3], 0.0])));
        }
        assert!(split, "{m:?}");
        assert_same(&g(vec![solved]), &g(vec![twin]), true);
    }

    #[test]
    fn keyframes_compare_by_what_they_render() {
        let mut p = Param::new(ParamValue::Float(1.0));
        p.put_keyframe(Tick(0), ParamValue::Float(1.0));
        p.put_keyframe(Tick(1000), ParamValue::Float(2.0));
        let mut a = GradeNode::new(1);
        a.params.insert("saturation".into(), p.clone());
        let mut b = GradeNode::new(2);
        b.params.insert("saturation".into(), p.clone());
        assert_same(&g(vec![a.clone()]), &g(vec![b.clone()]), true);
        // a different keyframe time or value is a different grade
        p.put_keyframe(Tick(2000), ParamValue::Float(0.5));
        b.params.insert("saturation".into(), p);
        assert_same(&g(vec![a.clone()]), &g(vec![b]), false);
        // an animated parameter that holds one value is that static value
        let mut held = Param::new(ParamValue::Float(0.0));
        held.keyframes = vec![Keyframe::new(Tick(0), ParamValue::Float(1.5)), Keyframe::new(Tick(500), ParamValue::Float(1.5))];
        let mut c = GradeNode::new(3);
        c.params.insert("saturation".into(), held);
        assert_same(&g(vec![c]), &g(vec![node(1, &[("saturation", ParamValue::Float(1.5))])]), true);
        // keyframes all at identity are absent
        let mut idk = Param::new(ParamValue::Color([0.0; 4]));
        idk.put_keyframe(Tick(0), ParamValue::Color([0.0; 4]));
        idk.put_keyframe(Tick(9), ParamValue::Color([-0.0, 0.0, 0.0, 0.0]));
        let mut d = GradeNode::new(4);
        d.params.insert("lift".into(), idk);
        assert_same(&g(vec![d]), &Grade::default(), true);
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let a = g(vec![node(1, &[("gain", ParamValue::Color([0.2, 0.1, 0.0, 0.0])), ("curve.r", ParamValue::Curve(vec![[0.0, 0.1], [1.0, 0.9]]))])]);
        assert_eq!(grade_fingerprint(&a), grade_fingerprint(&a.clone()));
        assert_ne!(grade_fingerprint(&a), grade_fingerprint(&Grade::default()));
    }

    // ---- shot statistics ----

    /// A synthetic frame (linear light): a lightness ramp with colour bands and a skin block.
    fn frame() -> Image {
        let (w, h) = (192, 108);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / w as f32;
                let mut c = [t * t + 0.01, t * t * 0.9 + 0.02, t * 0.8 * t + 0.03];
                if y < h / 3 {
                    c = [c[0] * 0.6, c[1] * 1.1, c[2] * 0.7];
                } else if y > 2 * h / 3 {
                    c = [c[0] * 0.8, c[1] * 0.85, c[2] * 1.2];
                }
                if (60..120).contains(&x) && (40..70).contains(&y) {
                    c = [0.55, 0.32, 0.22];
                }
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
        }
        img
    }

    /// `img` × `gain` per channel (linear light).
    fn scaled(img: &Image, gain: [f32; 3]) -> Image {
        let mut out = img.clone();
        for p in out.px.as_chunks_mut::<4>().0 {
            for k in 0..3 {
                p[k] *= gain[k];
            }
        }
        out
    }

    const THIRD: f32 = 1.259_921; // 2^(1/3)
    const WARM: [f32; 3] = [1.08, 1.0, 0.92];

    #[test]
    fn a_frame_against_itself_is_identical() {
        let s = ShotStats::measure(&frame()).unwrap();
        let c = compare(&s, &s);
        assert!(c.score >= 99.0 && c.notes.is_empty(), "{c:?}");
        assert!((c.overlap - 1.0).abs() < 1e-5 && c.exposure_stops == 0.0);
    }

    #[test]
    fn an_offset_copy_is_described_with_the_right_words() {
        let f = frame();
        let a = ShotStats::measure(&f).unwrap();
        let b = ShotStats::measure(&scaled(&f, [THIRD * WARM[0], THIRD * WARM[1], THIRD * WARM[2]])).unwrap();
        let c = compare(&a, &b);
        assert!((c.exposure_stops - 0.333).abs() < 0.08, "{c:?}");
        assert!(c.warmth > JND_CAST, "{c:?}");
        assert!(c.score < 80.0, "{c:?}");
        let text = c.notes.join("; ");
        assert!(text.contains("B is about ⅓ stop brighter") && text.contains("warmer"), "{text}");
        assert!(!text.contains("darker") && !text.contains("cooler"), "{text}");
        // the other way round
        let c = compare(&b, &a);
        let text = c.notes.join("; ");
        assert!(c.exposure_stops < -0.25 && text.contains("⅓ stop darker") && text.contains("cooler"), "{text}");
        // a pure saturation change
        let grey = |img: &Image, k: f32| {
            let mut o = img.clone();
            for p in o.px.as_chunks_mut::<4>().0 {
                let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                for c in &mut p[..3] {
                    *c = y + (*c - y) * k;
                }
            }
            o
        };
        let c = compare(&a, &ShotStats::measure(&grey(&f, 0.8)).unwrap());
        assert!(c.saturation_pct < -5.0 && c.notes.iter().any(|n| n.contains("less saturated")), "{c:?}");
    }

    #[test]
    fn stops_words_round_to_sixths() {
        assert_eq!(stops_words(0.33), "⅓ stop");
        assert_eq!(stops_words(-0.5), "½ stop");
        assert_eq!(stops_words(0.1), "⅙ stop");
        assert_eq!(stops_words(1.0), "1 stop");
        assert_eq!(stops_words(1.34), "1⅓ stops");
        assert_eq!(stops_words(5.0), "5.0 stops");
        assert_eq!(stops_words(f32::NAN), "99.0 stops");
    }

    #[test]
    fn the_offset_shot_is_the_scene_outlier() {
        let f = frame();
        let shots: Vec<ShotStats> = [[1.0; 3], [1.02, 1.0, 0.99], [0.98, 0.99, 1.0], [1.0, 1.01, 1.02], [THIRD * WARM[0], THIRD * WARM[1], THIRD * WARM[2]]]
            .iter()
            .map(|g| ShotStats::measure(&scaled(&f, *g)).unwrap())
            .collect();
        let o = outliers(&shots);
        assert_eq!(o.len(), 5);
        assert_eq!(o[0].0, 4);
        assert!(o[0].1 > 15.0 && o[1].1 < 5.0, "{o:?}");
        assert!(outliers(&[]).is_empty());
        assert_eq!(outliers(&shots[..1]), vec![(0, 0.0)]);
    }

    fn hostile_images() -> Vec<Image> {
        let mut nan = frame();
        nan.px.iter_mut().step_by(3).for_each(|v| *v = f32::NAN);
        let mut inf = frame();
        inf.px.iter_mut().step_by(5).for_each(|v| *v = f32::INFINITY);
        let mut huge = frame();
        huge.px.iter_mut().for_each(|v| *v *= 1e30);
        let mut neg = frame();
        neg.px.iter_mut().for_each(|v| *v = -*v);
        let transparent = Image { w: 4, h: 4, px: vec![0.0; 64] };
        let short = Image { w: 100, h: 100, px: vec![1.0; 10] };
        let tall = Image { w: 1, h: 3000, px: vec![0.5; 12000] };
        vec![
            Image::new(0, 0),
            Image { w: 1, h: 1, px: vec![0.3, 0.2, 0.1, 1.0] },
            nan,
            inf,
            huge,
            neg,
            transparent,
            short,
            tall,
            Image { w: 3, h: 3, px: vec![f32::NAN; 36] },
        ]
    }

    #[test]
    fn hostile_frames_measure_without_panicking() {
        let good = ShotStats::measure(&frame()).unwrap();
        for img in hostile_images() {
            for ws in WorkingSpace::ALL {
                if let Some(s) = ShotStats::measure_in(&img, ws) {
                    let c = compare(&good, &s);
                    assert!(c.score.is_finite() && (0.0..=100.0).contains(&c.score), "{c:?}");
                    assert!([c.exposure_stops, c.warmth, c.tint, c.saturation_pct, c.overlap].iter().all(|v| v.is_finite()), "{c:?}");
                    let _ = compare(&s, &s);
                    let _ = outliers(&[s.clone(), good.clone(), s]);
                }
            }
        }
        assert!(ShotStats::measure(&Image::new(0, 0)).is_none());
        assert!(ShotStats::measure(&Image { w: 4, h: 4, px: vec![0.0; 64] }).is_none());
        assert!(ShotStats::measure(&Image { w: 100, h: 100, px: vec![1.0; 10] }).is_none());
    }

    // ---- shot match ----

    fn wheel(m: &NodeMatch, key: &str) -> [f32; 4] {
        m.params.iter().find(|(k, _)| *k == key).and_then(|(_, v)| v.as_color()).unwrap()
    }

    fn sat(m: &NodeMatch) -> f64 {
        m.params.iter().find(|(k, _)| *k == "saturation").and_then(|(_, v)| v.as_f64()).unwrap()
    }

    fn in_range(m: &NodeMatch) {
        assert_eq!(m.params.len(), 5);
        for (k, v) in &m.params {
            match (node_param(k).map(|p| p.kind), v) {
                (Some(NodeParamKind::Wheel { range }), ParamValue::Color(c)) => {
                    assert!(c.iter().all(|x| x.is_finite() && x.abs() <= range), "{k} {c:?}")
                }
                (Some(NodeParamKind::Float { min, max, .. }), ParamValue::Float(x)) => {
                    assert!(x.is_finite() && *x >= min as f64 && *x <= max as f64, "{k} {x}")
                }
                other => panic!("unexpected {k}: {other:?}"),
            }
        }
    }

    #[test]
    fn shot_match_brings_an_offset_copy_back() {
        for ws in [WorkingSpace::DaVinciWide, WorkingSpace::Rec709] {
            let reference = frame();
            let current = scaled(&reference, [THIRD * WARM[0], THIRD * WARM[1], THIRD * WARM[2]]);
            let m = match_node(&current, &reference, ws, true);
            in_range(&m);
            assert!(m.after < m.before * 0.2, "{m:?}");
            let fixed = grade_with_node(&current, &m.node(1), ws);
            let c = compare(&ShotStats::measure_in(&reference, ws).unwrap(), &ShotStats::measure_in(&fixed, ws).unwrap());
            assert!(c.score >= 95.0, "{c:?}");
            assert!(c.exposure_stops.abs() <= 0.05, "{c:?}");
            assert!(c.warmth.abs() <= JND_CAST && c.tint.abs() <= JND_CAST, "{c:?}");
        }
    }

    #[test]
    fn shot_match_of_identical_frames_is_the_identity() {
        let f = frame();
        for ws in [WorkingSpace::DaVinciWide, WorkingSpace::Rec709] {
            let m = match_node(&f, &f, ws, false);
            in_range(&m);
            assert!(m.before < 1e-4, "{m:?}");
            for key in ["lift", "gamma", "gain", "offset"] {
                assert!(wheel(&m, key).iter().all(|v| v.abs() < 0.01), "{key}: {m:?}");
            }
            assert!((sat(&m) - 1.0).abs() < 0.01, "{m:?}");
        }
    }

    #[test]
    fn shot_match_survives_hostile_frames() {
        let good = frame();
        for img in hostile_images() {
            for (cur, refs) in [(&img, &good), (&good, &img), (&img, &img)] {
                let m = match_node(cur, refs, WorkingSpace::DaVinciWide, true);
                in_range(&m);
                assert!(m.before.is_finite() && m.after.is_finite(), "{m:?}");
            }
        }
    }
}
