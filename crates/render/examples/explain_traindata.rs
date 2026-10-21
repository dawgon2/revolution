//! Training data for the fine-tuned node explainer (GRADE-PLAN G2.11b).
//!
//! ```text
//! cargo run --release -p filmcraft-render --example explain_traindata -- \
//!     --out-dir ~/Apps/filmcraft-train/runs/<name> [--grades 2600] [--per-grade 2] [--seed 1] [--val 0.05] [--subtle 0.3]
//! ```
//!
//! Builds many random one-node grades on synthetic frames (every control family of the 36-case
//! evaluation set: wheels, contrast, saturation, temperature / tint, curves, qualifier incl.
//! inverted, windows incl. inverted, LUT, grain, noise reduction, highlight roll-off, disabled,
//! matte view, identity), measures each with [`explain_node`], and writes chat examples:
//! system + user = `filmcraft_llm::explain_prompt(facts, template)` (exactly the prompt the app
//! and `llm_eval` use), assistant = an explanation written by a small rule-based *realizer* from
//! the verified facts (several phrasings per grade). **Every target passes
//! `check_explanation_labeled`**; candidates that don't are dropped (and counted in the stats).
//! Grades whose compact facts equal one of the 36 held-out evaluation cases are dropped too.
//!
//! Output (in `--out-dir`): `train.jsonl`, `val.jsonl` (split by grade, not by example), one
//! object per line `{"messages": [{role, content} x3], "id", "family"}`, and `data-stats.json`.
//! Deterministic for a given seed.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::Write as _;

use filmcraft_color::WorkingSpace;
use filmcraft_project::grade::{Grade, GradeNode};
use filmcraft_project::keyframe::ParamValue;
use filmcraft_project::mask::Mask;
use filmcraft_project::{MaskPath, Param};
use filmcraft_render::Image;
use filmcraft_render::grade::{GradeCtx, compile_fx, qualifier_hsl};
use filmcraft_render::grade_explain::cases::{eval_cases, eval_cases_v2, eval_cases_v3};
use filmcraft_render::grade_explain::{FactMismatch, NodeFacts, check_explanation_labeled, explain_node};
use filmcraft_time::Tick;

const W: usize = 120;
const H: usize = 60;
const WS: WorkingSpace = WorkingSpace::Rec709;
const SKIN: [f32; 3] = [0.45, 0.24, 0.15];
const SKY: [f32; 3] = [0.03, 0.05, 0.25];

// ---- deterministic randomness ---------------------------------------------------------------

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.f()
    }
    fn chance(&mut self, p: f64) -> bool {
        self.f() < p
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> Option<&'a T> {
        xs.get(self.below(xs.len()))
    }
    /// A pick from a list of string choices ("" when empty).
    fn word(&mut self, xs: &[&'static str]) -> &'static str {
        self.pick(xs).copied().unwrap_or("")
    }
    fn shuffle<T>(&mut self, xs: &mut [T]) {
        for i in (1..xs.len()).rev() {
            let j = self.below(i + 1);
            xs.swap(i, j);
        }
    }
}

// ---- grades -----------------------------------------------------------------------------------

/// A synthetic frame: a grey ramp on top, four colour patches below (the evaluation frame when
/// `patches` is the evaluation set).
fn frame(patches: [[f32; 3]; 4], ramp_gamma: f32) -> Image {
    let mut img = Image::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let c = if y < H / 2 {
                let t = (x as f32 + 0.5) / W as f32;
                let v = 0.002 + 0.9 * t.powf(ramp_gamma);
                [v, v, v]
            } else {
                patches.get(x / 30).copied().unwrap_or(SKIN)
            };
            let i = (y * W + x) * 4;
            if let Some(px) = img.px.get_mut(i..i + 4) {
                px.copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
        }
    }
    img
}

const EVAL_PATCHES: [[f32; 3]; 4] = [SKIN, SKY, [0.05, 0.2, 0.04], [0.3, 0.25, 0.05]];

/// Colours patches are drawn from (linear Rec. 709).
const PALETTE: &[[f32; 3]] = &[
    SKIN,
    [0.6, 0.35, 0.25],
    [0.25, 0.12, 0.07],
    [0.35, 0.2, 0.14],
    SKY,
    [0.1, 0.2, 0.5],
    [0.05, 0.12, 0.2],
    [0.05, 0.2, 0.04],
    [0.1, 0.3, 0.08],
    [0.3, 0.25, 0.05],
    [0.5, 0.05, 0.04],
    [0.4, 0.1, 0.3],
    [0.02, 0.15, 0.15],
    [0.18, 0.18, 0.18],
    [0.6, 0.55, 0.45],
    [0.02, 0.02, 0.025],
];

fn set(n: &mut GradeNode, key: &str, v: ParamValue) {
    n.params.insert(key.into(), Param::new(v));
}

fn setf(n: &mut GradeNode, key: &str, v: f64) {
    set(n, key, ParamValue::Float(v));
}

fn window(rng: &mut Rng, inverted: bool) -> Mask {
    let x0 = rng.range(0.0, 0.7) * W as f64;
    let x1 = (x0 + rng.range(0.15, 0.6) * W as f64).min(W as f64);
    let (y0, y1) = if rng.chance(0.6) { (0.0, H as f64) } else { (rng.range(0.0, 0.4) * H as f64, rng.range(0.6, 1.0) * H as f64) };
    let mut m = Mask::new("w", MaskPath::rect(x0, y0, x1, y1));
    m.feather = Param::new(ParamValue::Float(if rng.chance(0.7) { 0.0 } else { rng.range(1.0, 6.0) }));
    m.inverted = inverted;
    m
}

/// A wheel offset: colour ball and/or master.
fn wheel(rng: &mut Rng, key: &str, s: f64) -> [f32; 4] {
    let (col, master): (f64, (f64, f64)) = match key {
        "lift" => (0.06, (-0.12, 0.12)),
        "gamma" => (0.06, (-0.4, 0.4)),
        "gain" => (0.12, (-0.8, 1.5)),
        _ => (0.04, (-0.06, 0.06)),
    };
    let use_col = rng.chance(0.6);
    let use_master = !use_col || rng.chance(0.4);
    let mut c = [0.0f32; 4];
    if use_col {
        // a hue direction: one or two channels pushed
        for v in c.iter_mut().take(3) {
            if rng.chance(0.6) {
                *v = (rng.range(-col, col) * s) as f32;
            }
        }
    }
    if use_master {
        c[3] = (rng.range(master.0, master.1) * s) as f32;
    }
    c
}

fn curve(rng: &mut Rng, s: f64) -> Vec<[f32; 2]> {
    let k = 1 + rng.below(3);
    let mut xs: Vec<f64> = (0..k).map(|_| rng.range(0.12, 0.88)).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() < 0.08);
    let mut pts = vec![[0.0f32, 0.0f32]];
    let mut last_y = 0.0f64;
    for x in xs {
        let y = (x + rng.range(-0.15, 0.15) * s).clamp(last_y + 0.02, 0.98);
        pts.push([x as f32, y as f32]);
        last_y = y;
    }
    pts.push([1.0, 1.0]);
    pts
}

/// `v` moved toward 1 by the subtle factor `s` (exactly `v` when `s` is 1).
fn toward_one(v: f64, s: f64) -> f64 {
    if s == 1.0 { v } else { 1.0 + (v - 1.0) * s }
}

/// One correction family on `n`; returns its name. With probability `subtle` the colour and tone
/// amounts shrink to 10–40 % (smol360-v2, AI1.6: near the checker's thresholds the tunes claimed
/// changes too small to report; these teach leaving them out). `subtle` 0 draws no extra random
/// numbers, so older data sets regenerate unchanged.
fn correction(rng: &mut Rng, n: &mut GradeNode, luts: &[String], subtle: f64) -> &'static str {
    let s = if subtle > 0.0 && rng.chance(subtle) { rng.range(0.1, 0.4) } else { 1.0 };
    match rng.below(14) {
        0..=3 => {
            let key = rng.word(&["lift", "gamma", "gain", "offset"]);
            let v = wheel(rng, key, s);
            set(n, key, ParamValue::Color(v));
            key
        }
        4 => {
            setf(n, "contrast", toward_one(rng.range(0.5, 1.8), s));
            if rng.chance(0.3) {
                setf(n, "pivot", rng.range(0.3, 0.6));
            }
            "contrast"
        }
        5 => {
            setf(n, "saturation", if rng.chance(0.15) { rng.range(0.0, 0.15) } else { toward_one(rng.range(0.4, 2.0), s) });
            "saturation"
        }
        6 | 7 => {
            if rng.chance(0.7) {
                setf(n, "temperature", rng.range(-80.0, 80.0) * s);
            }
            if rng.chance(0.5) || !n.params.contains_key("temperature") {
                setf(n, "tint", rng.range(-60.0, 60.0) * s);
            }
            "temperature_tint"
        }
        8 | 9 => {
            let key = if rng.chance(0.7) { "curve.master" } else { rng.word(&["curve.r", "curve.g", "curve.b"]) };
            set(n, key, ParamValue::Curve(curve(rng, s)));
            "curve"
        }
        10 => {
            if let Some(l) = rng.pick(luts) {
                n.lut = l.clone();
                if rng.chance(0.4) {
                    setf(n, "lut.mix", rng.range(0.2, 1.0));
                }
            }
            "lut"
        }
        11 => {
            setf(n, "grain.strength", rng.range(0.05, 1.0));
            "grain"
        }
        12 => {
            setf(n, "nr.luma", rng.range(0.0, 1.0));
            setf(n, "nr.chroma", rng.range(0.0, 1.0));
            "noise_reduction"
        }
        _ => {
            setf(n, "hl.rolloff", rng.range(0.2, 1.0));
            if rng.chance(0.4) {
                setf(n, "hl.knee", rng.range(0.5, 0.9));
            }
            "rolloff"
        }
    }
}

fn add_qualifier(rng: &mut Rng, n: &mut GradeNode, patches: &[[f32; 3]; 4]) {
    let target = if rng.chance(0.8) { patches.get(rng.below(4)).copied().unwrap_or(SKIN) } else { [rng.f() as f32, rng.f() as f32, rng.f() as f32] };
    let hue = qualifier_hsl(WS, target).first().copied().unwrap_or(0.0);
    setf(n, "qual.on", 1.0);
    setf(n, "qual.hue", f64::from(hue));
    setf(n, "qual.hueWidth", rng.range(0.03, 0.2));
    setf(n, "qual.satLow", rng.range(0.05, 0.25));
    setf(n, "qual.soft", rng.range(0.01, 0.05));
    if rng.chance(0.25) {
        setf(n, "qual.invert", 1.0);
    }
}

/// A random one-node grade and the names of its families.
fn random_grade(rng: &mut Rng, patches: &[[f32; 3]; 4], luts: &[String], subtle: f64) -> (GradeNode, String) {
    let mut n = GradeNode::new(1);
    let roll = rng.f();
    if roll < 0.02 {
        return (n, "identity".into());
    }
    let mut fams = Vec::new();
    let k = if rng.chance(0.55) {
        1
    } else if rng.chance(0.7) {
        2
    } else {
        3
    };
    for _ in 0..k {
        fams.push(correction(rng, &mut n, luts, subtle));
    }
    if roll < 0.06 {
        add_qualifier(rng, &mut n, patches);
        setf(&mut n, "qual.show", if rng.chance(0.5) { 1.0 } else { 2.0 });
        fams.push("matte_view");
    } else {
        if rng.chance(0.2) {
            add_qualifier(rng, &mut n, patches);
            fams.push("qualifier");
        }
        if rng.chance(0.18) {
            let inv = rng.chance(0.3);
            n.windows.push(window(rng, inv));
            fams.push(if inv { "window_inverted" } else { "window" });
        }
    }
    if rng.chance(0.06) {
        n.enabled = false;
        fams.push("disabled");
    }
    fams.sort_unstable();
    fams.dedup();
    (n, fams.join("+"))
}

// ---- the realizer -----------------------------------------------------------------------------

const RANGES: [&str; 3] = ["shadows", "midtones", "highlights"];
const CAST_STEPS: [f32; 3] = [0.01, 0.03, 0.08];

/// One tonal range in the realizer's terms.
#[derive(Clone, Copy, Debug)]
struct Rf {
    touched: bool,
    stops: f32,
    /// Stops rounded to sixths.
    n: i64,
    /// Signed cast levels (0 none, 1 slight, 2 plain, 3 much), as the template words them.
    warm: i64,
    tint: i64,
    /// Saturation change in percent (|s| ≥ 10).
    sat: Option<i64>,
    warm_v: f32,
    tint_v: f32,
}

fn sixths(s: f32) -> i64 {
    if s.is_finite() { (s * 6.0).round().clamp(-600.0, 600.0) as i64 } else { 0 }
}

fn cast(v: f32) -> i64 {
    let l = CAST_STEPS.iter().filter(|s| v.abs() >= **s).count() as i64;
    if v < 0.0 { -l } else { l }
}

fn rf(f: &NodeFacts) -> [Rf; 3] {
    f.ranges.clone().map(|r| Rf {
        touched: r.touched,
        stops: r.stops,
        n: if r.touched { sixths(r.stops) } else { 0 },
        warm: if r.touched { cast(r.warmth) } else { 0 },
        tint: if r.touched { cast(r.tint) } else { 0 },
        sat: if r.touched { r.saturation.map(|s| s.round() as i64).filter(|s| s.abs() >= 10) } else { None },
        warm_v: r.warmth,
        tint_v: r.tint,
    })
}

impl Rf {
    fn has_phrase(&self) -> bool {
        self.n != 0 || self.warm != 0 || self.tint != 0 || self.sat.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dim {
    Bright,
    Warm,
    Tint,
    Sat,
}

/// One claim: a dimension with a direction over some ranges, with an amount (sixths of a stop
/// or percent) or a cast level.
#[derive(Clone, Debug)]
struct Claim {
    dim: Dim,
    sign: i64,
    ranges: Vec<usize>,
    /// Bright: sixths; Sat: percent; Warm / Tint: the level (1..3).
    amount: i64,
    /// For brightness: the largest |stops| of the ranges (for strength words).
    max_stops: f32,
    min_stops: f32,
    /// The checker's strength level of the largest change (stops ÷ ⅚, cast ÷ 0.08, % ÷ 40).
    level: f32,
}

const FRAC_DIGITS: [&str; 6] = ["", "⅙", "⅓", "½", "⅔", "⅚"];

/// "⅓ stop", "1⅓ stops", "a third of a stop", "half a stop", "two stops" … for |n| sixths.
fn stops_amount(rng: &mut Rng, n: i64) -> String {
    let a = n.unsigned_abs();
    let (whole, frac) = (a / 6, (a % 6) as usize);
    let digits = {
        let w = if whole == 0 { String::new() } else { whole.to_string() };
        format!("{w}{} {}", FRAC_DIGITS.get(frac).copied().unwrap_or(""), if a <= 6 { "stop" } else { "stops" })
    };
    let words: Option<&str> = match a {
        1 => Some("a sixth of a stop"),
        2 => Some(*rng.pick(&["a third of a stop", "one third of a stop"]).unwrap_or(&"a third of a stop")),
        3 => Some(*rng.pick(&["half a stop", "a half stop"]).unwrap_or(&"half a stop")),
        4 => Some("two thirds of a stop"),
        5 => Some("five sixths of a stop"),
        6 => Some(*rng.pick(&["a stop", "one stop"]).unwrap_or(&"a stop")),
        12 => Some("two stops"),
        18 => Some("three stops"),
        24 => Some("four stops"),
        _ => None,
    };
    let core = match words {
        Some(w) if rng.chance(0.55) => w.to_string(),
        _ => digits,
    };
    if rng.chance(0.4) { format!("{} {core}", rng.word(&["about", "roughly", "around"])) } else { core }
}

fn pct_amount(rng: &mut Rng, p: i64) -> String {
    let unit = if rng.chance(0.6) { "percent" } else { "%" };
    let core = format!("{} {unit}", p.unsigned_abs());
    if rng.chance(0.35) { format!("{} {core}", rng.word(&["about", "roughly", "around"])) } else { core }
}

const WEAK: &[&str] = &["slightly", "a little", "a bit", "subtly", "gently"];
const STRONG: &[&str] = &["much", "strongly", "a lot"];

/// A strength word allowed for a cast level (None for a plain level).
fn cast_strength(rng: &mut Rng, level: i64) -> Option<&'static str> {
    match level.abs() {
        1 => Some(rng.word(WEAK)),
        3 => Some(rng.word(STRONG)),
        _ => None,
    }
}

fn bright_strength(rng: &mut Rng, c: &Claim) -> Option<&'static str> {
    if c.max_stops < 0.3 {
        Some(rng.word(&["slightly", "a little", "a bit"]))
    } else if c.min_stops > 1.4 {
        Some(rng.word(&["much", "strongly", "dramatically", "a lot"]))
    } else {
        None
    }
}

fn sat_strength(rng: &mut Rng, p: i64) -> Option<&'static str> {
    if p.abs() < 18 {
        Some(rng.word(&["slightly", "a little", "a bit"]))
    } else if p.abs() >= 36 {
        Some(rng.word(&["much", "strongly", "a lot"]))
    } else {
        None
    }
}

/// How a set of ranges is named, and whether the subject is plural.
fn scope(rng: &mut Rng, ranges: &[usize], whole_ok: bool, whole_frame_ok: bool) -> (String, bool) {
    if ranges.len() == 3 && whole_ok && rng.chance(0.6) {
        let mut opts: Vec<(&str, bool)> =
            vec![("everything", false), ("the picture", false), ("all tones", true), ("the image", false), ("all the tones", true)];
        if whole_frame_ok {
            opts.push(("the whole picture", false));
            opts.push(("the entire image", false));
        }
        let (s, pl) = rng.pick(&opts).copied().unwrap_or(("everything", false));
        return (s.to_string(), pl);
    }
    let names: Vec<&str> = ranges.iter().filter_map(|i| RANGES.get(*i).copied()).collect();
    let the = rng.chance(0.85);
    let list = match names.as_slice() {
        [] => String::new(),
        [a] => (*a).to_string(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    (if the { format!("the {list}") } else { list }, true)
}

fn get(plural: bool, rng: &mut Rng) -> &'static str {
    if plural { rng.word(&["get", "become", "turn", "go"]) } else { rng.word(&["gets", "becomes", "turns", "goes"]) }
}

/// The claim as a verb phrase without subject: "brightens the shadows by a third of a stop".
fn verb_phrase(rng: &mut Rng, c: &Claim, sc: &str) -> String {
    match c.dim {
        Dim::Bright => {
            let verb = if c.sign > 0 { rng.word(&["brightens", "brightens", "lifts", "raises"]) } else { rng.word(&["darkens", "darkens", "lowers"]) };
            if rng.chance(0.8) {
                format!("{verb} {sc} by {}", stops_amount(rng, c.amount))
            } else if let Some(s) = bright_strength(rng, c).filter(|_| verb.ends_with("ens")) {
                format!("{verb} {sc} {s}")
            } else {
                format!("{verb} {sc} by {}", stops_amount(rng, c.amount))
            }
        }
        Dim::Warm => {
            if rng.chance(0.3) {
                let hue = if c.sign > 0 { rng.word(&["warm", "orange", "yellow"]) } else { rng.word(&["cool", "blue"]) };
                let st = match c.amount {
                    1 => format!("{} ", rng.word(&["slight", "subtle", "gentle"])),
                    3 => "strong ".to_string(),
                    _ => String::new(),
                };
                format!("adds a {st}{hue} {} to {sc}", rng.word(&["tint", "cast"]))
            } else {
                let verb = if c.sign > 0 { "warms" } else { "cools" };
                match cast_strength(rng, c.amount) {
                    Some(s) => format!("{verb} {sc} {s}"),
                    None => format!("{verb} {sc}"),
                }
            }
        }
        Dim::Tint => {
            let hue = if c.sign > 0 { "magenta" } else { "green" };
            let st = match c.amount {
                1 => format!("{} ", rng.word(&["slight", "subtle", "gentle"])),
                3 => "strong ".to_string(),
                _ => String::new(),
            };
            format!("adds a {st}{hue} {} to {sc}", rng.word(&["tint", "cast"]))
        }
        Dim::Sat => {
            let amt = pct_amount(rng, c.amount);
            match (c.sign > 0, rng.below(3)) {
                (true, 0) => format!("saturates {sc} by {amt}"),
                (true, 1) => format!("{} the saturation in {sc} by {amt}", rng.word(&["boosts", "raises", "increases"])),
                (false, 0) => format!("desaturates {sc} by {amt}"),
                (false, 1) => format!("{} the saturation in {sc} by {amt}", rng.word(&["reduces", "lowers", "cuts"])),
                (true, _) => match sat_strength(rng, c.amount) {
                    Some(s) => format!("saturates {sc} {s}"),
                    None => format!("saturates {sc}"),
                },
                (false, _) => match sat_strength(rng, c.amount) {
                    Some(s) => format!("desaturates {sc} {s}"),
                    None => format!("desaturates {sc}"),
                },
            }
        }
    }
}

/// The claim as a predicate after "get": "a third of a stop brighter", "slightly warmer".
fn predicate(rng: &mut Rng, c: &Claim, signed_style: bool) -> String {
    match c.dim {
        Dim::Bright => {
            let adj = if c.sign > 0 { "brighter" } else { "darker" };
            if signed_style {
                let a = c.amount.unsigned_abs();
                let (whole, frac) = (a / 6, (a % 6) as usize);
                let w = if whole == 0 { String::new() } else { whole.to_string() };
                return format!(
                    "{}{w}{} {}",
                    if c.sign > 0 { "+" } else { "\u{2212}" },
                    FRAC_DIGITS.get(frac).copied().unwrap_or(""),
                    if a <= 6 { "stop" } else { "stops" }
                );
            }
            match rng.below(3) {
                0 => format!("{adj} by {}", stops_amount(rng, c.amount)),
                _ => format!("{} {adj}", stops_amount(rng, c.amount)),
            }
        }
        Dim::Warm => {
            let adj = if c.sign > 0 { rng.word(&["warmer", "warmer", "warmer", "yellower"]) } else { rng.word(&["cooler", "cooler", "cooler", "bluer"]) };
            match cast_strength(rng, c.amount) {
                Some(s) => format!("{s} {adj}"),
                None => adj.to_string(),
            }
        }
        Dim::Tint => {
            let adj = if c.sign > 0 { "more magenta" } else { "greener" };
            match cast_strength(rng, c.amount) {
                Some(s) => format!("{s} {adj}"),
                None => adj.to_string(),
            }
        }
        Dim::Sat => {
            if signed_style {
                return format!("saturation {}{} %", if c.sign > 0 { "+" } else { "\u{2212}" }, c.amount.unsigned_abs());
            }
            let adj = if c.sign > 0 {
                rng.word(&["more saturated", "more saturated", "more vivid", "more colorful"])
            } else {
                rng.word(&["less saturated", "less saturated", "less colorful", "less vivid"])
            };
            match rng.below(3) {
                0 => format!("{} {adj}", pct_amount(rng, c.amount)),
                1 => format!("{adj} by {}", pct_amount(rng, c.amount)),
                _ => match sat_strength(rng, c.amount) {
                    Some(s) => format!("{s} {adj}"),
                    None => adj.to_string(),
                },
            }
        }
    }
}

/// Claims per dimension: ranges with the same value share a claim.
fn dimension_claims(r: &[Rf; 3]) -> Vec<Claim> {
    let mut out = Vec::new();
    for dim in [Dim::Bright, Dim::Warm, Dim::Tint, Dim::Sat] {
        let mut groups: Vec<(i64, Vec<usize>)> = Vec::new();
        for (i, x) in r.iter().enumerate() {
            let v = match dim {
                Dim::Bright => x.n,
                Dim::Warm => x.warm,
                Dim::Tint => x.tint,
                Dim::Sat => x.sat.unwrap_or(0),
            };
            if v == 0 {
                continue;
            }
            match groups.iter_mut().find(|(g, _)| *g == v) {
                Some((_, idx)) => idx.push(i),
                None => groups.push((v, vec![i])),
            }
        }
        for (v, idx) in groups {
            let stops: Vec<f32> = idx.iter().filter_map(|i| r.get(*i)).map(|x| x.stops.abs()).collect();
            let level = idx
                .iter()
                .filter_map(|i| r.get(*i))
                .map(|x| match dim {
                    Dim::Bright => x.stops.abs() / (5.0 / 6.0),
                    Dim::Warm => x.warm_v.abs() / 0.08,
                    Dim::Tint => x.tint_v.abs() / 0.08,
                    Dim::Sat => x.sat.unwrap_or(0).unsigned_abs() as f32 / 40.0,
                })
                .fold(0.0, f32::max);
            out.push(Claim {
                level,
                dim,
                sign: v.signum(),
                amount: v.abs(),
                max_stops: stops.iter().copied().fold(0.0, f32::max),
                min_stops: stops.iter().copied().fold(f32::INFINITY, f32::min),
                ranges: idx,
            });
        }
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn percent_of_frame(rng: &mut Rng, f: f32) -> String {
    let p = (f * 100.0).round() as i64;
    let unit = if rng.chance(0.5) { "%" } else { "percent" };
    if p == 0 && f > 0.0 { format!("<1 {unit}") } else { format!("{p} {unit}") }
}

/// Where the node acts, as a lead before the claims. `None` = no lead.
fn lead(rng: &mut Rng, f: &NodeFacts) -> (Option<String>, bool) {
    let c = &f.controls;
    let qual = c.qualifier.as_ref();
    let whole = f.matte_coverage.is_none() && f.coverage >= 0.9;
    let mut opts: Vec<String> = Vec::new();
    // the share of the frame
    if whole {
        opts.push(format!("{} the whole frame; ", rng.word(&["It affects", "It affects", "This node affects", "Affects"])));
    } else {
        opts.push(format!(
            "{} {} of the frame; inside it, ",
            rng.word(&["It affects", "It affects", "This node affects", "Affects"]),
            percent_of_frame(rng, f.affected())
        ));
    }
    let win_lead = match (c.windows, c.windows_inverted) {
        (0, _) => None,
        (n, 0) if n > 0 => Some("inside the window"),
        (n, m) if n == m => Some("outside the window"),
        _ => None,
    };
    let qual_lead =
        qual.map(|q| if q.invert { "outside the selection" } else { rng.word(&["in the selection", "inside the selection", "in the qualifier selection"]) });
    match (qual_lead, win_lead) {
        (Some(q), None) => opts.push(format!("{}, ", capitalize(q))),
        (None, Some(w)) => opts.push(format!("{}, ", capitalize(w))),
        (Some(q), Some(w)) => opts.push(format!("{} {w}, ", capitalize(q))),
        (None, None) => {}
    }
    if whole {
        opts.push(String::new());
    }
    // no lead at all (named ranges need none)
    opts.push(String::new());
    let pick = rng.pick(&opts).cloned().unwrap_or_default();
    let is_coverage = pick.contains("of the frame") || pick.contains("whole frame");
    ((!pick.is_empty()).then_some(pick), is_coverage)
}

fn join_clauses(rng: &mut Rng, clauses: &[String], allow_semicolon: bool, verbs: bool) -> String {
    let mut s = String::new();
    for (i, c) in clauses.iter().enumerate() {
        if i > 0 {
            let last = i + 1 == clauses.len();
            let j = if last {
                if verbs { rng.word(&[" and ", ", and "]) } else { rng.word(&[" and ", ", and ", " and ", ", while ", ", but "]) }
            } else if allow_semicolon && rng.chance(0.3) {
                "; "
            } else {
                ", "
            };
            s.push_str(j);
        }
        s.push_str(c);
    }
    s
}

/// The agent for a verb clause ("It", "This node", "The curve", …).
fn agent(rng: &mut Rng, f: &NodeFacts) -> &'static str {
    let c = &f.controls;
    let mut opts = vec!["It", "It", "This node", "The node"];
    if !c.curves.is_empty() && c.wheels.is_empty() && c.contrast.is_none() && c.saturation.is_none() && c.temperature.is_none() && c.tint.is_none() {
        opts.push("The curve");
    }
    if c.qualifier.as_ref().is_some_and(|q| !q.invert) {
        opts.push("The selection");
    }
    rng.word(&opts)
}

/// One candidate explanation of a node that changes the picture.
fn realize(rng: &mut Rng, f: &NodeFacts) -> String {
    let r = rf(f);
    let (lead_text, lead_is_coverage) = lead(rng, f);
    let whole_frame_ok = f.matte_coverage.is_none() && f.coverage >= 0.9;
    let whole_ok = lead_text.is_some() || whole_frame_ok;
    let untouched: Vec<usize> = (0..3).filter(|i| r.get(*i).is_some_and(|x| !x.touched)).collect();
    let slight: Vec<usize> = (0..3).filter(|i| r.get(*i).is_some_and(|x| x.touched && !x.has_phrase())).collect();
    let texture = {
        let mut t = Vec::new();
        if f.controls.grain.is_some() {
            t.push("adds grain");
        }
        if f.controls.noise_reduction.is_some() {
            t.push(rng.word(&["reduces noise", "removes noise"]));
        }
        t
    };
    let mut clauses: Vec<String> = Vec::new();
    let style = rng.below(3);
    let mut claims = dimension_claims(&r);
    // now and then leave out the small changes (the checker only needs those ≥ half the largest)
    if rng.chance(0.5) {
        let top = claims.iter().map(|c| c.level).fold(0.0, f32::max);
        claims.retain(|c| c.level >= 0.45 * top);
    }
    let mut verbs = false;
    match style {
        // verbs per dimension: "it brightens the shadows by ⅓ stop and warms the highlights"
        0 if !claims.is_empty() => {
            let mut cl = claims.clone();
            if rng.chance(0.3) {
                rng.shuffle(&mut cl);
            }
            let ag = agent(rng, f);
            for (i, c) in cl.iter().enumerate() {
                let (sc, _) = scope(rng, &c.ranges, whole_ok, whole_frame_ok);
                let vp = verb_phrase(rng, c, &sc);
                clauses.push(if i == 0 && (lead_text.is_none() || rng.chance(0.5)) {
                    format!("{} {vp}", if lead_text.is_some() { lowercase_first(ag) } else { ag.to_string() })
                } else {
                    vp
                });
            }
            verbs = true;
        }
        // subjects per dimension: "the shadows get ⅓ stop brighter, while the highlights get warmer"
        1 if !claims.is_empty() => {
            // claims about the same ranges share one subject
            let mut by_scope: Vec<(Vec<usize>, Vec<String>)> = Vec::new();
            for c in &claims {
                let p = predicate(rng, c, false);
                match by_scope.iter_mut().find(|(r, _)| *r == c.ranges) {
                    Some((_, ps)) => ps.push(p),
                    None => by_scope.push((c.ranges.clone(), vec![p])),
                }
            }
            for (ranges, ps) in &by_scope {
                let (sc, pl) = scope(rng, ranges, whole_ok, whole_frame_ok);
                let plist = match ps.as_slice() {
                    [a] => a.clone(),
                    [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                    [] => continue,
                };
                clauses.push(format!("{sc} {} {plist}", get(pl, rng)));
            }
        }
        // per range group, like the template: "the shadows get ⅓ stop brighter and slightly warmer"
        _ => {
            let mut groups: Vec<(Vec<usize>, Rf)> = Vec::new();
            for (i, x) in r.iter().enumerate() {
                if !x.touched || !x.has_phrase() {
                    continue;
                }
                match groups.iter_mut().find(|(_, g)| g.n == x.n && g.warm == x.warm && g.tint == x.tint && g.sat == x.sat) {
                    Some((idx, _)) => idx.push(i),
                    None => groups.push((vec![i], *x)),
                }
            }
            let signed = rng.chance(0.3);
            for (idx, g) in &groups {
                let one = [*g];
                let mut cs = dimension_claims(&[one[0], Rf { touched: false, ..one[0] }.cleared(), Rf { touched: false, ..one[0] }.cleared()]);
                for c in cs.iter_mut() {
                    c.ranges = idx.clone();
                    let st: Vec<f32> = idx.iter().filter_map(|i| r.get(*i)).map(|x| x.stops.abs()).collect();
                    c.max_stops = st.iter().copied().fold(0.0, f32::max);
                    c.min_stops = st.iter().copied().fold(f32::INFINITY, f32::min);
                }
                let preds: Vec<String> = cs.iter().map(|c| predicate(rng, c, signed)).collect();
                let (sc, pl) = scope(rng, idx, whole_ok, whole_frame_ok);
                let plist = match preds.as_slice() {
                    [] => continue,
                    [a] => a.clone(),
                    [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                };
                clauses.push(if signed { format!("{sc} {plist}") } else { format!("{sc} {} {plist}", get(pl, rng)) });
            }
        }
    }
    let touched: Vec<usize> = (0..3).filter(|i| r.get(*i).is_some_and(|x| x.touched)).collect();
    let mut body = if clauses.is_empty() && !texture.is_empty() {
        if (lead_text.is_some() && rng.chance(0.7)) || touched.is_empty() {
            format!("it {}", texture.join(" and "))
        } else {
            let (sc, _) = scope(rng, &touched, false, false);
            format!("it {} {sc}", texture.join(" and ").replace("adds grain", "adds grain to").replace("noise", "noise in"))
        }
    } else {
        let mut b = join_clauses(rng, &clauses, !verbs, verbs);
        if !texture.is_empty() {
            if (lead_text.is_some() && rng.chance(0.7)) || touched.is_empty() {
                b.push_str(&format!("; also {}", texture.join(" and ")));
            } else {
                let (sc, _) = scope(rng, &touched, false, false);
                b.push_str(&format!(
                    "; also {} {sc}",
                    texture.first().copied().unwrap_or("adds grain").replace("adds grain", "adds grain to").replace("noise", "noise in")
                ));
            }
        }
        b
    };
    if !slight.is_empty() && texture.is_empty() {
        let (sc, pl) = scope(rng, &slight, false, false);
        let sep = if body.is_empty() { "" } else { "; " };
        body.push_str(&format!("{sep}{sc} {} slightly", if pl { "change" } else { "changes" }));
    }
    if !untouched.is_empty() && rng.chance(0.6) {
        let (sc, pl) = scope(rng, &untouched, false, false);
        let tail = match rng.below(3) {
            0 => format!(", leaving {sc} {}", rng.word(&["untouched", "unchanged"])),
            1 => format!("; {sc} {} {}", if pl { rng.word(&["stay", "remain", "are"]) } else { "stays" }, rng.word(&["untouched", "the same", "unchanged"])),
            _ => format!(", while {sc} {} {}", if pl { "stay" } else { "stays" }, rng.word(&["the same", "untouched"])),
        };
        body.push_str(&tail);
    }
    let text = match &lead_text {
        Some(l) => format!("{l}{body}"),
        None => body,
    };
    let _ = lead_is_coverage;
    let mut out = capitalize(text.trim());
    out.push('.');
    out
}

impl Rf {
    fn cleared(self) -> Rf {
        Rf { n: 0, warm: 0, tint: 0, sat: None, ..self }
    }
}

const DISABLED: &[&str] = &[
    "This node is disabled, so nothing changes.",
    "It is disabled; no visible change.",
    "The node is switched off, so the picture is unchanged.",
    "Disabled, no visible change.",
    "This node is turned off, so nothing changes.",
    "The node is bypassed; no change.",
];
const MATTE: &[&str] = &[
    "The node is showing its qualifier matte rather than grading.",
    "It is showing the matte, not a grade.",
    "This node is showing its qualifier selection, not grading.",
    "Showing the qualifier matte rather than grading.",
    "It is in matte view, showing the selection rather than grading.",
];
const NOCHANGE: &[&str] =
    &["No visible change.", "Nothing changes in the picture.", "The picture is unchanged.", "This node is unchanged.", "It is unchanged; no visible change."];

/// Candidate explanations of one node: noop phrasings or many random realizations.
fn candidates(rng: &mut Rng, f: &NodeFacts, tries: usize) -> Vec<String> {
    let list: Option<&[&str]> = if !f.enabled {
        Some(DISABLED)
    } else if f.showing_matte {
        Some(MATTE)
    } else if !f.visible() {
        Some(NOCHANGE)
    } else {
        None
    };
    if let Some(l) = list {
        return l.iter().map(|s| (*s).to_string()).collect();
    }
    (0..tries).map(|_| realize(rng, f)).collect()
}

// ---- main ---------------------------------------------------------------------------------------

struct Args {
    out_dir: std::path::PathBuf,
    grades: usize,
    per_grade: usize,
    seed: u64,
    val: f64,
    tries: usize,
    samples: usize,
    subtle: f64,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut a = Args { out_dir: std::path::PathBuf::new(), grades: 2600, per_grade: 2, seed: 1, val: 0.05, tries: 48, samples: 0, subtle: 0.0 };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut v = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--out-dir" => a.out_dir = v()?.into(),
            "--grades" => a.grades = v()?.parse::<usize>()?.min(1_000_000),
            "--per-grade" => a.per_grade = v()?.parse::<usize>()?.clamp(1, 16),
            "--seed" => a.seed = v()?.parse()?,
            "--val" => a.val = v()?.parse::<f64>()?.clamp(0.0, 0.5),
            "--tries" => a.tries = v()?.parse::<usize>()?.clamp(1, 1000),
            "--samples" => a.samples = v()?.parse::<usize>()?.min(1000),
            "--subtle" => a.subtle = v()?.parse::<f64>()?.clamp(0.0, 1.0),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if a.out_dir.as_os_str().is_empty() {
        return Err("usage: explain_traindata --out-dir <dir> [--grades N] [--per-grade K] [--seed S] [--val F] [--tries T] [--samples N] [--subtle F]".into());
    }
    Ok(a)
}

const NAMES: &[&str] = &["Skin", "Balance", "Sky", "Look", "Exposure", "Warm up", "Contrast", "Fix", "Shadows", "Grade"];

fn reason_name(e: &FactMismatch) -> String {
    format!("{e:?}").split(['(', ' ', '{']).next().unwrap_or("?").to_string()
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    std::fs::create_dir_all(&args.out_dir)?;
    let mut rng = Rng(args.seed ^ 0x6772_6164_6532_6262);
    let held_out: BTreeSet<String> =
        // (every held-out set: sets 2 and 3 judge v2 tunes, AI1.6)
        eval_cases().iter().chain(eval_cases_v2().iter()).chain(eval_cases_v3().iter()).filter_map(|c| serde_json::to_string(&c.facts).ok()).map(|s| filmcraft_llm::compact_facts(&s)).collect();
    let luts: Vec<String> = {
        let all: Vec<String> = filmcraft_render::luts::builtins().iter().map(|b| format!("builtin:{}", b.id)).collect();
        // looks more often than camera conversions
        let looks: Vec<String> = all.iter().filter(|s| s.starts_with("builtin:look")).cloned().collect();
        let mut v = all;
        v.extend(looks.iter().cloned());
        v.extend(looks);
        v
    };
    let mut train = std::io::BufWriter::new(std::fs::File::create(args.out_dir.join("train.jsonl"))?);
    let mut val = std::io::BufWriter::new(std::fs::File::create(args.out_dir.join("val.jsonl"))?);
    let mut stats_fam: BTreeMap<String, usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let (mut tried, mut passed, mut held, mut dup_facts, mut no_target, mut n_train, mut n_val, mut bytes) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut samples = Vec::new();
    for gi in 0..args.grades {
        let patches = if rng.chance(0.5) {
            EVAL_PATCHES
        } else {
            let mut p = [SKIN; 4];
            for x in p.iter_mut() {
                *x = rng.pick(PALETTE).copied().unwrap_or(SKIN);
            }
            p
        };
        let gamma = if rng.chance(0.5) { 2.0 } else { rng.range(1.0, 3.0) as f32 };
        let src = frame(patches, gamma);
        let (node, fam) = random_grade(&mut rng, &patches, &luts, args.subtle);
        let mut out = src.clone();
        let g = Grade { nodes: vec![node.clone()], bypass: false, ..Default::default() };
        for op in compile_fx(&g, Tick(0), WS, GradeCtx::default(), src.w, src.h) {
            op.apply(&mut out);
        }
        let facts = explain_node(&node, Tick(0), &src, &out, WS);
        let facts_json = serde_json::to_string(&facts)?;
        let compact = filmcraft_llm::compact_facts(&facts_json);
        if held_out.contains(&compact) {
            held += 1;
            continue;
        }
        // the same facts twice add nothing but weight (identity, most no-ops)
        if !seen.insert(compact.clone()) {
            dup_facts += 1;
            continue;
        }
        let kind = if !facts.enabled {
            "disabled"
        } else if facts.showing_matte {
            "matte_view"
        } else if !facts.visible() {
            "no_change"
        } else {
            "change"
        };
        // labels: "Node k" (k = 1 most often) with an optional name
        let index = if rng.chance(0.5) { 1 } else { 1 + rng.below(8) };
        let name = rng.chance(0.3).then(|| rng.word(NAMES));
        let label = match name {
            Some(n) => format!("Node {index} '{n}'"),
            None => format!("Node {index}"),
        };
        let template = facts.sentence(&label);
        let mut ok: Vec<String> = Vec::new();
        for c in candidates(&mut rng, &facts, args.tries) {
            tried += 1;
            match check_explanation_labeled(&facts, name, Some(index), &c) {
                Ok(()) => {
                    passed += 1;
                    if !ok.contains(&c) {
                        ok.push(c);
                    }
                }
                Err(e) => {
                    *reasons.entry(reason_name(&e)).or_default() += 1;
                    if samples.len() < args.samples {
                        samples.push(format!("{fam} | {e} | {c} | template: {template}"));
                    }
                }
            }
        }
        if ok.is_empty() {
            no_target += 1;
            continue;
        }
        // shorter answers are easier for a small model to get exactly right: keep the shorter 60 %
        ok.sort_by_key(|t| t.chars().count());
        ok.truncate(ok.len().div_ceil(5) * 3);
        rng.shuffle(&mut ok);
        let is_val = rng.chance(args.val);
        let (system, user) = filmcraft_llm::explain_prompt(&facts_json, &template);
        for target in ok.iter().take(args.per_grade) {
            // now and then the answer carries the node's label, exactly as given
            let target = if rng.chance(0.12) { format!("{label}: {}", lowercase_first(target)) } else { target.clone() };
            if check_explanation_labeled(&facts, name, Some(index), &target).is_err() {
                continue;
            }
            let line = serde_json::json!({
                "id": format!("g{gi}"),
                "family": fam,
                "kind": kind,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user},
                    {"role": "assistant", "content": target},
                ],
            })
            .to_string();
            bytes += line.len() + 1;
            if is_val {
                writeln!(val, "{line}")?;
                n_val += 1;
            } else {
                writeln!(train, "{line}")?;
                n_train += 1;
            }
            *kinds.entry(kind).or_default() += 1;
        }
        for f in fam.split('+') {
            *stats_fam.entry(f.to_string()).or_default() += 1;
        }
    }
    train.flush()?;
    val.flush()?;
    let stats = serde_json::json!({
        "seed": args.seed,
        "grades": args.grades,
        "per_grade": args.per_grade,
        "train_examples": n_train,
        "val_examples": n_val,
        "bytes": bytes,
        "dropped_held_out_facts": held,
        "dropped_duplicate_facts": dup_facts,
        "dropped_no_passing_target": no_target,
        "candidates_tried": tried,
        "candidates_passed": passed,
        "candidate_rejections": reasons,
        "families": stats_fam,
        "subtle": args.subtle,
        "kinds": kinds,
    });
    std::fs::write(args.out_dir.join("data-stats.json"), serde_json::to_string_pretty(&stats)?)?;
    println!("{}", serde_json::to_string_pretty(&stats)?);
    for s in samples {
        eprintln!("REJECT {s}");
    }
    Ok(())
}

fn lowercase_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
