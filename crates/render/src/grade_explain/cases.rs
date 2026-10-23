//! The fixed evaluation set for node explanations (GRADE-PLAN §3 G2.11): 36 known grades on a
//! synthetic frame with their measured [`NodeFacts`] and template sentence. The model crate's eval
//! reads them (`FILMCRAFT_DUMP_EXPLAIN_CASES=<path>` writes JSON lines `{label, facts, template}`)
//! and scores an AI explanation with [`super::check_explanation`].

use filmcraft_color::WorkingSpace;
use filmcraft_project::grade::{Grade, GradeNode};
use filmcraft_project::keyframe::ParamValue;
use filmcraft_project::mask::Mask;
use filmcraft_project::{MaskPath, Param};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

use super::{BranchFacts, NodeFacts, explain_node};
use crate::Image;
use crate::grade::{GradeCtx, compile_fx, qualifier_hsl};

const W: usize = 120;
const H: usize = 60;
const WS: WorkingSpace = WorkingSpace::Rec709;
const SKIN: [f32; 3] = [0.45, 0.24, 0.15];
const SKY: [f32; 3] = [0.03, 0.05, 0.25];

/// One evaluation case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalCase {
    /// A short id, e.g. `warm_gain`.
    pub label: String,
    pub facts: NodeFacts,
    /// The template sentence ([`NodeFacts::sentence`]) for the label "Node 1".
    pub template: String,
}

/// The synthetic frame: a grey ramp on top; below, a skin patch, a sky-blue, a green and a yellow
/// patch.
fn frame() -> Image {
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
                    0 => SKY,
                    1 => [0.05, 0.2, 0.04],
                    _ => [0.3, 0.25, 0.05],
                }
            };
            let i = (y * W + x) * 4;
            if let Some(px) = img.px.get_mut(i..i + 4) {
                px.copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
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

fn color(key: &'static str, c: [f32; 4]) -> (&'static str, ParamValue) {
    (key, ParamValue::Color(c))
}

fn float(key: &'static str, v: f64) -> (&'static str, ParamValue) {
    (key, ParamValue::Float(v))
}

fn window(x0: f64, x1: f64, inverted: bool) -> Mask {
    let mut m = Mask::new("w", MaskPath::rect(x0, 0.0, x1, H as f64));
    m.feather = Param::new(ParamValue::Float(0.0));
    m.inverted = inverted;
    m
}

fn qualified(hue_of: [f32; 3], width: f64, extra: &[(&'static str, ParamValue)]) -> GradeNode {
    let hue = qualifier_hsl(WS, hue_of)[0];
    let mut p =
        vec![float("qual.on", 1.0), float("qual.hue", f64::from(hue)), float("qual.hueWidth", width), float("qual.satLow", 0.15), float("qual.soft", 0.02)];
    p.extend(extra.iter().cloned());
    node(&p)
}

/// The 36 cases, in a fixed order.
pub fn eval_cases() -> Vec<EvalCase> {
    let gain = |v: f32| color("gain", [0.0, 0.0, 0.0, v]);
    let list: Vec<(&str, GradeNode)> = vec![
        ("identity", GradeNode::new(1)),
        ("warm_gain", node(&[color("gain", [0.08, 0.03, 0.0, 0.0])])),
        ("cool_lift", node(&[color("lift", [0.0, 0.02, 0.05, 0.0])])),
        ("plus_third_stop", node(&[gain(0.1)])),
        ("minus_one_stop", node(&[gain(-0.35)])),
        ("contrast_up", node(&[float("contrast", 1.3)])),
        ("contrast_down", node(&[float("contrast", 0.7)])),
        ("saturation_up", node(&[float("saturation", 1.4)])),
        ("saturation_down", node(&[float("saturation", 0.8)])),
        ("saturation_extreme_down", node(&[float("saturation", 0.05)])),
        ("temperature_warm", node(&[float("temperature", 30.0)])),
        ("temperature_cool_tint_green", node(&[float("temperature", -60.0), float("tint", -30.0)])),
        ("tint_magenta", node(&[float("tint", 40.0)])),
        ("qualifier_skin_gain", qualified(SKIN, 0.06, &[color("gain", [0.0, 0.0, 0.0, 0.3])])),
        ("qualifier_sky_cool", qualified(SKY, 0.08, &[float("temperature", -50.0)])),
        ("qualifier_skin_desaturate", qualified(SKIN, 0.06, &[float("saturation", 0.5)])),
        ("window_left_gain", {
            let mut n = node(&[gain(0.3)]);
            n.windows.push(window(0.0, 60.0, false));
            n
        }),
        ("window_inverted_warm", {
            let mut n = node(&[float("temperature", 40.0)]);
            n.windows.push(window(0.0, 60.0, true));
            n
        }),
        ("disabled_gain", {
            let mut n = node(&[gain(0.1)]);
            n.enabled = false;
            n
        }),
        ("showing_matte", qualified(SKIN, 0.06, &[float("qual.show", 1.0), gain(0.3)])),
        ("grain_only", node(&[float("grain.strength", 0.5)])),
        ("noise_reduction_only", node(&[float("nr.luma", 0.5), float("nr.chroma", 0.5)])),
        ("curve_lift_shadows", node(&[("curve.master", ParamValue::Curve(vec![[0.0, 0.0], [0.25, 0.33], [1.0, 1.0]]))])),
        ("curve_compress_highlights", node(&[("curve.master", ParamValue::Curve(vec![[0.0, 0.0], [0.75, 0.62], [1.0, 1.0]]))])),
        ("split_tone", node(&[color("lift", [0.0, 0.02, 0.06, 0.0]), color("gain", [0.1, 0.04, 0.0, 0.0])])),
        ("extreme_brighten", node(&[gain(1.5)])),
        ("extreme_darken", node(&[gain(-0.8)])),
        ("gamma_midtones_up", node(&[color("gamma", [0.0, 0.0, 0.0, 0.3])])),
        ("offset_blue", node(&[color("offset", [0.0, 0.0, 0.03, 0.0])])),
        ("contrast_sat_warm", node(&[float("contrast", 1.2), float("saturation", 0.8), float("temperature", 20.0)])),
        ("qualifier_skin_inverted", qualified(SKIN, 0.06, &[float("qual.invert", 1.0), gain(0.3)])),
        ("lut_node", {
            let mut n = GradeNode::new(1);
            n.lut = "builtin:film".into();
            n
        }),
        ("highlight_rolloff", node(&[float("hl.rolloff", 0.8)])),
        ("qualifier_and_window", {
            let mut n = qualified(SKIN, 0.06, &[gain(0.3)]);
            n.windows.push(window(0.0, 15.0, false));
            n
        }),
        ("curve_s_opposite_signs", node(&[("curve.master", ParamValue::Curve(vec![[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]]))])),
        ("tiny_gain_zero_coverage", node(&[gain(0.002)])),
    ];
    measure_all(&frame(), list.into_iter().map(|(l, n)| (l.to_string(), n)).collect())
}

/// Grade every node on `src` and measure it ([`explain_node`], template for "Node 1").
fn measure_all(src: &Image, list: Vec<(String, GradeNode)>) -> Vec<EvalCase> {
    list.into_iter()
        .map(|(label, n)| {
            let mut out = src.clone();
            let g = Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() };
            for op in compile_fx(&g, Tick(0), WS, GradeCtx::default(), src.w, src.h) {
                op.apply(&mut out);
            }
            let facts = explain_node(&n, Tick(0), src, &out, WS);
            let template = facts.sentence("Node 1");
            EvalCase { label, facts, template }
        })
        .collect()
}

// ---- the second held-out set ------------------------------------------------------------------

/// The seed of [`eval_cases_v2`]'s control values.
pub const V2_SEED: u64 = 2;
const W2: usize = 96;
const H2: usize = 64;
/// Patch colours of the second frame (linear Rec. 709; none is in the training generator's palette).
const SKIN2: [f32; 3] = [0.38, 0.21, 0.14];
const TEAL2: [f32; 3] = [0.03, 0.12, 0.16];
const FOLIAGE2: [f32; 3] = [0.08, 0.16, 0.03];
const RED2: [f32; 3] = [0.42, 0.07, 0.06];

/// SplitMix64 (the training generator's generator, its own seed here).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [a, b).
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * ((self.next() >> 11) as f64 / (1u64 << 53) as f64)
    }
    fn f32(&mut self, a: f64, b: f64) -> f32 {
        self.range(a, b) as f32
    }
}

/// The second frame, laid out unlike the first (and unlike every training frame, which are all
/// 120 × 60 with a horizontal ramp on top): 96 × 64, a vertical grey ramp on the left 40 columns
/// (another curve), and a 2 × 2 grid of patches on the right (skin, teal / foliage, red).
fn frame_v2() -> Image {
    let mut img = Image::new(W2, H2);
    for y in 0..H2 {
        for x in 0..W2 {
            let c = if x < 40 {
                let t = (y as f32 + 0.5) / H2 as f32;
                let v = 0.004 + 0.85 * t.powf(1.6);
                [v, v, v]
            } else {
                match ((x - 40) / 28, y / 32) {
                    (0, 0) => SKIN2,
                    (_, 0) => TEAL2,
                    (0, _) => FOLIAGE2,
                    _ => RED2,
                }
            };
            let i = (y * W2 + x) * 4;
            if let Some(px) = img.px.get_mut(i..i + 4) {
                px.copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
        }
    }
    img
}

fn rect_window(x0: f64, y0: f64, x1: f64, y1: f64, inverted: bool) -> Mask {
    let mut m = Mask::new("w", MaskPath::rect(x0, y0, x1, y1));
    m.feather = Param::new(ParamValue::Float(0.0));
    m.inverted = inverted;
    m
}

/// A second evaluation set (the follow-up of TRAINING.md "Held-out integrity"): the control
/// families of [`eval_cases`] (plus a few neighbours: contrast pivot, a channel curve, a real look
/// LUT, lift master) on another frame ([`frame_v2`]), with control values drawn from seed
/// [`V2_SEED`]. Labels start with `h2_`. Cases whose compact facts equal a training example must
/// still be dropped against the actual training data (`explain_score --set 2 --dump-cases …
/// --dedupe train.jsonl`); this list is the full candidate set, in a fixed order.
pub fn eval_cases_v2() -> Vec<EvalCase> {
    let mut r = Rng(V2_SEED ^ 0x6865_6c64_6f75_7432);
    let gain = |v: f32| color("gain", [0.0, 0.0, 0.0, v]);
    let qual = |hue_of: [f32; 3], width: f64, extra: &[(&'static str, ParamValue)]| {
        let hue = qualifier_hsl(WS, hue_of)[0];
        let mut p =
            vec![float("qual.on", 1.0), float("qual.hue", f64::from(hue)), float("qual.hueWidth", width), float("qual.satLow", 0.12), float("qual.soft", 0.03)];
        p.extend(extra.iter().cloned());
        node(&p)
    };
    let look = crate::luts::builtins().iter().find(|b| b.id.starts_with("look-")).map(|b| format!("builtin:{}", b.id)).unwrap_or_default();
    let w2 = W2 as f64;
    let h2 = H2 as f64;
    // every value is drawn in this order, so the list is fixed for a given seed
    let list: Vec<(&str, GradeNode)> = vec![
        ("identity", GradeNode::new(1)),
        ("warm_gain", node(&[color("gain", [r.f32(0.04, 0.12), r.f32(0.01, 0.05), 0.0, 0.0])])),
        ("cool_lift", node(&[color("lift", [0.0, r.f32(0.0, 0.03), r.f32(0.03, 0.07), 0.0])])),
        ("gain_up", node(&[gain(r.f32(0.12, 0.25))])),
        ("gain_down", node(&[gain(r.f32(-0.5, -0.25))])),
        ("contrast_up", node(&[float("contrast", r.range(1.2, 1.5))])),
        ("contrast_down", node(&[float("contrast", r.range(0.55, 0.8))])),
        ("contrast_pivot", node(&[float("contrast", r.range(1.2, 1.5)), float("pivot", r.range(0.3, 0.6))])),
        ("saturation_up", node(&[float("saturation", r.range(1.3, 1.8))])),
        ("saturation_down", node(&[float("saturation", r.range(0.6, 0.85))])),
        ("saturation_extreme_down", node(&[float("saturation", r.range(0.0, 0.1))])),
        ("temperature_warm", node(&[float("temperature", r.range(20.0, 50.0))])),
        ("temperature_cool", node(&[float("temperature", r.range(-70.0, -30.0))])),
        ("tint_green", node(&[float("tint", r.range(-50.0, -20.0))])),
        ("warm_tint_magenta", node(&[float("temperature", r.range(10.0, 30.0)), float("tint", r.range(20.0, 50.0))])),
        ("qualifier_skin_gain", qual(SKIN2, r.range(0.04, 0.08), &[gain(r.f32(0.2, 0.4))])),
        ("qualifier_teal_warm", qual(TEAL2, r.range(0.05, 0.1), &[float("temperature", r.range(30.0, 60.0))])),
        ("qualifier_foliage_desaturate", qual(FOLIAGE2, r.range(0.05, 0.1), &[float("saturation", r.range(0.3, 0.6))])),
        ("window_top_gain", {
            let mut n = node(&[gain(r.f32(0.2, 0.4))]);
            n.windows.push(rect_window(0.0, 0.0, w2, h2 / 2.0, false));
            n
        }),
        ("window_inverted_cool", {
            let mut n = node(&[float("temperature", r.range(-60.0, -30.0))]);
            n.windows.push(rect_window(0.0, 0.0, 40.0, h2, true));
            n
        }),
        ("disabled_contrast", {
            let mut n = node(&[float("contrast", r.range(1.2, 1.5))]);
            n.enabled = false;
            n
        }),
        ("showing_matte", qual(RED2, r.range(0.04, 0.08), &[float("qual.show", 1.0), gain(r.f32(0.2, 0.4))])),
        ("grain_only", node(&[float("grain.strength", r.range(0.3, 0.8))])),
        ("noise_reduction_only", node(&[float("nr.luma", r.range(0.3, 0.8)), float("nr.chroma", r.range(0.3, 0.8))])),
        ("curve_lift_shadows", {
            let x = r.f32(0.2, 0.3);
            let y = x + r.f32(0.06, 0.1);
            node(&[("curve.master", ParamValue::Curve(vec![[0.0, 0.0], [x, y], [1.0, 1.0]]))])
        }),
        ("curve_compress_highlights", {
            let x = r.f32(0.7, 0.8);
            let y = x - r.f32(0.08, 0.14);
            node(&[("curve.master", ParamValue::Curve(vec![[0.0, 0.0], [x, y], [1.0, 1.0]]))])
        }),
        ("curve_red_channel", node(&[("curve.r", ParamValue::Curve(vec![[0.0, 0.0], [0.5, 0.5 + r.f32(0.05, 0.1)], [1.0, 1.0]]))])),
        (
            "split_tone_warm_shadows",
            node(&[color("lift", [r.f32(0.03, 0.06), r.f32(0.01, 0.03), 0.0, 0.0]), color("gain", [0.0, r.f32(0.01, 0.03), r.f32(0.06, 0.12), 0.0])]),
        ),
        ("extreme_brighten", node(&[gain(r.f32(1.2, 1.5))])),
        ("extreme_darken", node(&[gain(r.f32(-0.8, -0.65))])),
        ("gamma_midtones_down", node(&[color("gamma", [0.0, 0.0, 0.0, r.f32(-0.35, -0.2)])])),
        ("offset_warm", node(&[color("offset", [r.f32(0.02, 0.04), r.f32(0.0, 0.01), 0.0, 0.0])])),
        (
            "contrast_sat_cool",
            node(&[float("contrast", r.range(1.1, 1.3)), float("saturation", r.range(1.1, 1.3)), float("temperature", r.range(-30.0, -15.0))]),
        ),
        ("qualifier_skin_inverted", qual(SKIN2, r.range(0.04, 0.08), &[float("qual.invert", 1.0), float("saturation", r.range(0.5, 0.7))])),
        ("lut_look", {
            let mut n = node(&[float("lut.mix", r.range(0.5, 1.0))]);
            n.lut = look.clone();
            n
        }),
        ("highlight_rolloff", node(&[float("hl.rolloff", r.range(0.5, 1.0)), float("hl.knee", r.range(0.6, 0.8))])),
        ("qualifier_and_window", {
            let mut n = qual(SKIN2, r.range(0.04, 0.08), &[gain(r.f32(0.2, 0.4))]);
            n.windows.push(rect_window(40.0, 0.0, 54.0, h2, false));
            n
        }),
        ("curve_s", {
            let d = r.f32(0.06, 0.12);
            node(&[("curve.master", ParamValue::Curve(vec![[0.0, 0.0], [0.25, 0.25 - d], [0.75, 0.75 + d], [1.0, 1.0]]))])
        }),
        ("tiny_gain", node(&[gain(r.f32(0.001, 0.003))])),
        ("lift_master_down", node(&[color("lift", [0.0, 0.0, 0.0, r.f32(-0.08, -0.04)])])),
    ];
    measure_all(&frame_v2(), list.into_iter().map(|(l, n)| (format!("h2_{l}"), n)).collect())
}

/// A third, small evaluation set for what shipped after the v1 tunes (fork AI1.6): the
/// Temperature / Tint pairs the white-balance picker and Auto balance (G2c) write, alone, with
/// exposure, inside a qualifier or a window, and corrections with a colour or saturation change
/// too small for the checker to report (a tune must leave it out; v1's misses). On the second
/// set's frame; labels start with `h3_`. Fixed values (no seed).
pub fn eval_cases_v3() -> Vec<EvalCase> {
    let gain = |v: f32| color("gain", [0.0, 0.0, 0.0, v]);
    let skin_hue = f64::from(qualifier_hsl(WS, SKIN2)[0]);
    let w2 = W2 as f64;
    let h2 = H2 as f64;
    let list: Vec<(&str, GradeNode)> = vec![
        ("wb_remove_warm_green_cast", node(&[float("temperature", -28.0), float("tint", 14.0)])),
        ("wb_remove_cool_cast", node(&[float("temperature", 36.0), float("tint", -6.0)])),
        ("wb_remove_green_cast", node(&[float("tint", 22.0)])),
        ("wb_auto_balance", node(&[float("temperature", -18.0), float("tint", 6.0)])),
        ("auto_balance_below_threshold", node(&[float("temperature", -9.0), float("tint", 3.0)])),
        ("wb_and_exposure", node(&[float("temperature", -22.0), gain(0.18)])),
        ("wb_inside_skin_qualifier", {
            node(&[
                float("qual.on", 1.0),
                float("qual.hue", skin_hue),
                float("qual.hueWidth", 0.06),
                float("qual.satLow", 0.12),
                float("qual.soft", 0.03),
                float("temperature", 26.0),
            ])
        }),
        ("wb_in_window", {
            let mut n = node(&[float("temperature", -32.0), float("tint", 8.0)]);
            n.windows.push(rect_window(0.0, 0.0, w2 / 2.0, h2, false));
            n
        }),
        ("gain_with_tiny_tint", node(&[gain(0.3), float("tint", 4.0)])),
        ("contrast_with_tiny_saturation", node(&[float("contrast", 1.3), float("saturation", 1.04)])),
        ("gain_with_faint_blue_highlights", node(&[color("gain", [0.0, 0.0, 0.015, 0.22])])),
        ("lift_with_faint_warm_shadows", node(&[color("lift", [0.008, 0.0, 0.0, -0.05])])),
        ("tiny_temperature_only", node(&[float("temperature", 3.0)])),
    ];
    measure_all(&frame_v2(), list.into_iter().map(|(l, n)| (format!("h3_{l}"), n)).collect())
}

/// The sixth evaluation set (fork AI2b): G4's grading tools on the second set's frame. HDR zone
/// wheels (exposure, saturation, balance per zone), the colour warper (hue-sat and chroma-luma
/// moves) and the 3D qualifier (picked clusters around a patch, inverted, with despill), alone and
/// with a correction. Labels start with `h6_`. Fixed values (no seed); never in the training data.
pub fn eval_cases_v6() -> Vec<EvalCase> {
    use filmcraft_project::qual3d::Qual3d;
    use filmcraft_project::warper::{WarpMode, Warper};
    let gain = |v: f32| color("gain", [0.0, 0.0, 0.0, v]);
    let warped = |moves: &[(WarpMode, u32, [f32; 2])], extra: &[(&'static str, ParamValue)]| {
        let mut n = node(extra);
        let mut w = Warper::default();
        for &(mode, index, to) in moves {
            w.mesh_mut(mode).set_point(mode, index, to, None);
        }
        n.warper = Some(w);
        n
    };
    let picked = |patch: [f32; 3], invert: bool, despill: f32, extra: &[(&'static str, ParamValue)]| {
        let mut params = vec![float("qual.on", 1.0)];
        if invert {
            params.push(float("qual.invert", 1.0));
        }
        params.extend_from_slice(extra);
        let mut n = node(&params);
        let c = crate::grade::qual3d_point(WS, patch).unwrap_or_default();
        let mut k = Qual3d::default();
        k.add_samples(&[c, c.map(|v| v + 0.01), c.map(|v| v - 0.01)], false);
        k.despill = despill;
        n.qual3d = Some(k);
        n
    };
    let list: Vec<(&str, GradeNode)> = vec![
        ("hdr_global_exposure_up", node(&[float("hdr.global.exposure", 0.6)])),
        ("hdr_highlights_down", node(&[float("hdr.highlight.exposure", -1.0)])),
        ("hdr_shadows_lifted", node(&[float("hdr.shadow.exposure", 0.8)])),
        ("hdr_dark_balance_blue", node(&[color("hdr.dark.balance", [0.0, 0.0, 0.3, 0.0])])),
        ("hdr_global_desaturated", node(&[float("hdr.global.sat", 0.4)])),
        ("hdr_specular_and_gain", node(&[float("hdr.specular.exposure", -1.2), gain(0.15)])),
        ("warper_red_toward_orange", warped(&[(WarpMode::HueSat, 16, [0.07, 0.75]), (WarpMode::HueSat, 24, [0.07, 1.0])], &[])),
        ("warper_cyan_toward_blue", warped(&[(WarpMode::HueSat, 20, [0.62, 0.75]), (WarpMode::HueSat, 28, [0.62, 1.0])], &[])),
        ("warper_chroma_luma_darker", warped(&[(WarpMode::ChromaLuma, 12, [0.5, 0.3])], &[float("contrast", 1.1)])),
        ("qual3d_skin_brighter", picked(SKIN2, false, 0.0, &[gain(0.3)])),
        ("qual3d_foliage_desaturated", picked(FOLIAGE2, false, 0.0, &[float("saturation", 0.4)])),
        ("qual3d_outside_red_cooler", picked(RED2, true, 0.0, &[float("temperature", -40.0)])),
        ("qual3d_teal_despill", picked(TEAL2, false, 0.8, &[])),
    ];
    measure_all(&frame_v2(), list.into_iter().map(|(l, n)| (format!("h6_{l}"), n)).collect())
}

/// One case of the branch held-out set (fork AI2 set 5): the facts of one branching node of a
/// fixed graph grade and its template note. Never in the training data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEvalCase {
    /// A short id, e.g. `b5_parallel_even_0` (the grade's name and the note's position).
    pub label: String,
    pub facts: BranchFacts,
    /// The template note ([`BranchFacts::sentence`]).
    pub template: String,
}

/// Fixed graph grades with every kind of branching node: even and uneven parallel blends (one
/// near-equal), chained branches, layer mixers with blend modes and opacities (three layers),
/// splitter / combiner (straight and with a channel detour), key mixers (add, take away with an
/// outside selection, overlap of three), outside / inside nodes and a pass-through mixer.
pub fn branch_eval_cases() -> Vec<BranchEvalCase> {
    use filmcraft_project::dag::{Edge, Port};
    use filmcraft_project::grade::{NodeKind, SOURCE};
    fn n(g: &mut Grade, id: u64, kind: NodeKind, label: &str) {
        let mut x = GradeNode::new(id);
        x.kind = kind;
        x.label = label.into();
        g.nodes.push(x);
    }
    fn e(g: &mut Grade, a: u64, ap: u8, b: u64, bp: u8) {
        g.edges.push(Edge::new(Port::new(a, ap), Port::new(b, bp)));
    }
    fn set(g: &mut Grade, id: u64, k: &str, v: f64) {
        if let Some(x) = g.node_mut(id) {
            x.params.insert(k.into(), Param::new(ParamValue::Float(v)));
        }
    }
    use NodeKind::{Combiner, Correction as C, KeyMixer, LayerMixer, ParallelMixer, Splitter};
    let two = |a: &str, b: &str, mixer: NodeKind, label: &str| {
        let mut g = Grade::default();
        n(&mut g, 1, C, a);
        n(&mut g, 2, C, b);
        n(&mut g, 3, mixer, label);
        e(&mut g, SOURCE, 0, 1, 0);
        e(&mut g, SOURCE, 0, 2, 0);
        e(&mut g, 1, 0, 3, 0);
        e(&mut g, 2, 0, 3, 1);
        g
    };
    let mut grades: Vec<(&str, Grade)> = Vec::new();
    grades.push(("parallel_even", two("Skin", "Background", ParallelMixer, "")));
    let mut g = two("", "", ParallelMixer, "Blend");
    n(&mut g, 4, C, "Sky");
    e(&mut g, SOURCE, 0, 4, 0);
    e(&mut g, 4, 0, 3, 2);
    set(&mut g, 3, "mix.gain.0", 2.0);
    grades.push(("parallel_three", g));
    let mut g = two("Exposure", "", ParallelMixer, "");
    g.edges.retain(|x| x.to.node != 2);
    e(&mut g, 1, 0, 2, 0);
    set(&mut g, 3, "mix.gain.1", 0.25);
    grades.push(("parallel_chained", g));
    let mut g = two("", "Warm up", ParallelMixer, "");
    set(&mut g, 3, "mix.gain.0", 1.02);
    set(&mut g, 3, "mix.gain.1", 0.98);
    grades.push(("parallel_near_equal", g));
    grades.push(("layer_normal", two("", "Look", LayerMixer, "")));
    let mut g = two("Fix", "", LayerMixer, "");
    set(&mut g, 3, "layer.opacity.1", 0.35);
    set(&mut g, 3, "layer.mode.1", 13.0);
    grades.push(("layer_soft_light", g));
    let mut g = two("", "", LayerMixer, "Looks");
    n(&mut g, 4, C, "Grade");
    e(&mut g, SOURCE, 0, 4, 0);
    e(&mut g, 4, 0, 3, 2);
    set(&mut g, 3, "layer.opacity.1", 0.5);
    set(&mut g, 3, "layer.mode.1", 4.0);
    set(&mut g, 3, "layer.opacity.2", 0.8);
    set(&mut g, 3, "layer.mode.2", 8.0);
    grades.push(("layer_three", g));
    let mut g = Grade::default();
    n(&mut g, 1, C, "");
    n(&mut g, 2, Splitter, "");
    n(&mut g, 3, Combiner, "");
    e(&mut g, SOURCE, 0, 1, 0);
    e(&mut g, 1, 0, 2, 0);
    for c in 0..3 {
        e(&mut g, 2, c, 3, c);
    }
    grades.push(("split_combine", g));
    let mut g = Grade::default();
    n(&mut g, 1, C, "Contrast");
    n(&mut g, 2, Splitter, "");
    n(&mut g, 3, C, "Shadows");
    n(&mut g, 4, Combiner, "");
    e(&mut g, SOURCE, 0, 1, 0);
    e(&mut g, 1, 0, 2, 0);
    e(&mut g, 2, 0, 4, 0);
    e(&mut g, 2, 1, 3, 0);
    e(&mut g, 3, 0, 4, 1);
    e(&mut g, 2, 2, 4, 2);
    grades.push(("split_detour", g));
    let keys = |k: usize, label: &str| {
        let mut g = Grade::default();
        for b in 0..k as u64 {
            n(&mut g, b + 1, C, if b == 0 { "Skin" } else { "" });
            e(&mut g, SOURCE, 0, b + 1, 0);
        }
        let m = k as u64 + 1;
        n(&mut g, m, KeyMixer, label);
        for b in 0..k as u64 {
            e(&mut g, b + 1, 1, m, b as u8);
        }
        n(&mut g, m + 1, C, "");
        e(&mut g, 1, 0, m + 1, 0);
        e(&mut g, m, 0, m + 1, 1);
        (g, m)
    };
    grades.push(("key_add", keys(2, "").0));
    let (mut g, m) = keys(2, "Mask");
    set(&mut g, m, "kmix.mode.1", 1.0);
    set(&mut g, m, "kmix.invert.1", 1.0);
    set(&mut g, m + 1, "key.invert", 1.0);
    grades.push(("key_subtract_outside", g));
    let (mut g, m) = keys(3, "");
    set(&mut g, m, "kmix.mode.1", 2.0);
    set(&mut g, m, "kmix.mode.2", 2.0);
    grades.push(("key_overlap_three", g));
    let mut g = Grade::default();
    n(&mut g, 1, C, "Sky");
    n(&mut g, 2, C, "");
    e(&mut g, SOURCE, 0, 1, 0);
    e(&mut g, 1, 0, 2, 0);
    e(&mut g, 1, 1, 2, 1);
    set(&mut g, 2, "key.invert", 1.0);
    grades.push(("outside_sky", g));
    let mut g = Grade::default();
    n(&mut g, 1, C, "");
    n(&mut g, 2, C, "Balance");
    e(&mut g, SOURCE, 0, 1, 0);
    e(&mut g, 1, 0, 2, 0);
    e(&mut g, 1, 0, 2, 1);
    grades.push(("inside_picture", g));
    let mut g = Grade::default();
    n(&mut g, 1, C, "");
    n(&mut g, 2, ParallelMixer, "");
    e(&mut g, SOURCE, 0, 1, 0);
    e(&mut g, 1, 0, 2, 0);
    grades.push(("parallel_pass_through", g));
    let mut out = Vec::new();
    for (name, g) in grades {
        for (i, facts) in super::branch_facts(&g, Tick(0)).into_iter().enumerate() {
            out.push(BranchEvalCase { label: format!("b5_{name}_{i}"), template: facts.sentence(), facts });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{RangeFacts, VOCABULARY, check_explanation, check_explanation_labeled, check_grammar, check_vocabulary};
    use super::*;

    /// Per case: explanations that are WRONG for it (each must fail, and in the checker's
    /// vocabulary, so it is the facts that reject them) and right paraphrases (each must pass).
    fn hand_written(label: &str) -> (&'static [&'static str], &'static [&'static str]) {
        match label {
            "identity" => {
                (&["The picture gets warmer.", "It brightens everything by a third of a stop."], &["No visible change.", "Nothing changes in the picture."])
            }
            "warm_gain" => (
                &[
                    "The shadows and highlights get cooler.",
                    "It cools everything and darkens the midtones by half a stop.",
                    "It warms the shadows and the skin.",
                ],
                &[
                    "It affects 78 % of the frame; inside it, it warms the shadows and highlights slightly, brightens everything by about a sixth of a stop and adds a slight magenta tint to the highlights.",
                ],
            ),
            "cool_lift" => (
                &["The shadows get warmer.", "The highlights get much cooler.", "Saturation rises by 40 percent."],
                &["The shadows get less saturated, cooler and a little greener, while the midtones and highlights cool slightly."],
            ),
            "plus_third_stop" => (
                &["Everything gets one stop brighter.", "Everything gets a third of a stop darker."],
                &["Everything gets about a third of a stop brighter.", "It brightens the picture by point three stops."],
            ),
            "minus_one_stop" => (
                &["The picture gets 1⅓ stops brighter.", "It darkens the picture by a third of a stop."],
                &["The picture gets about 1¼ stops darker.", "The shadows get 1⅙ stops darker and the midtones and highlights 1⅓ stops darker."],
            ),
            "contrast_up" => (
                &["It reduces contrast.", "The shadows get brighter and the highlights darker."],
                &["It adds contrast, darkening the shadows and brightening the highlights, and saturates the picture."],
            ),
            "contrast_down" => (
                &["It adds contrast.", "Everything gets more saturated."],
                &["It lowers contrast, brightening the shadows and darkening the highlights, and desaturates the picture."],
            ),
            "saturation_up" => (
                &["Colors become 27 percent less saturated.", "Colors become 60 percent more saturated."],
                &[
                    "It affects 50 % of the frame; inside it, colors become more saturated by about 27 percent.",
                    "It affects 50 % of the frame; inside it, colors get twenty-seven percent more saturated.",
                ],
            ),
            "saturation_down" => (
                &["Colors become more vivid.", "Saturation drops by 45 percent.", "Saturation rises by 17 percent."],
                &["It affects 50 % of the frame; inside it, saturation drops by 17 percent."],
            ),
            "saturation_extreme_down" => (
                &["Colors are slightly muted.", "Saturation rises by 94 percent."],
                &["It affects 50 % of the frame; inside it, saturation falls by about 94 percent."],
            ),
            "temperature_warm" => (
                &["The picture turns cooler.", "The picture gets much warmer."],
                &["It affects 70 % of the frame; inside it, the picture gets slightly warmer."],
            ),
            "temperature_cool_tint_green" => {
                (&["The picture gets warmer.", "It adds a magenta tint."], &["It cools the whole picture slightly and adds a green tint."])
            }
            "tint_magenta" => (
                &["It adds a green tint.", "It brightens the picture by a third of a stop."],
                &["It darkens the picture slightly, cools it slightly and adds a slight magenta tint."],
            ),
            "qualifier_skin_gain" => (
                &["It brightens the whole frame by ⅚ stop.", "It brightens the shadows by ⅚ stop.", "It brightens the sky by ⅚ of a stop."],
                &["The midtones and highlights in the selected skin tones get about ⅚ of a stop brighter, while the shadows stay the same."],
            ),
            "qualifier_sky_cool" => (
                &["It warms the shadows and midtones in the selection.", "It cools the highlights."],
                &["The qualifier selection cools the shadows and midtones, leaving the highlights untouched."],
            ),
            "qualifier_skin_desaturate" => (
                &["It desaturates the skin by 50 percent.", "It warms the highlights."],
                &["In the selected skin tones, the midtones and highlights turn cooler and greener, and the shadows are untouched."],
            ),
            "window_left_gain" => (
                &["It darkens the window by ⅚ stop.", "It brightens the whole frame by ⅚ stop."],
                &["Inside the window, the picture gets about ⅚ stop brighter."],
            ),
            "window_inverted_warm" => (
                &["Inside the window the shadows get warmer.", "The midtones and highlights get cooler."],
                &["Outside the window, the midtones and highlights get slightly warmer and less saturated and the shadows are untouched."],
            ),
            "disabled_gain" => {
                (&["It brightens the picture by ⅓ stop.", "It is disabled, but it warms the picture."], &["This node is disabled, so nothing changes."])
            }
            "showing_matte" => {
                (&["It brightens the skin by ⅚ stop.", "It cools the shadows."], &["The node is showing its qualifier matte rather than grading."])
            }
            "grain_only" => (
                &["It adds a third of a stop of brightness.", "It reduces noise and warms the picture."],
                &["It affects 65 % of the frame; inside it, it adds grain."],
            ),
            "noise_reduction_only" => (&["It reduces noise.", "It removes noise and brightens the picture."], &["No visible change."]),
            "curve_lift_shadows" => (
                &["The curve darkens the shadows.", "The curve adds a stop to the highlights."],
                &[
                    "The curve brightens the shadows by about ⅔ stop, the midtones by half a stop and the highlights by a sixth of a stop, and desaturates the shadows.",
                ],
            ),
            "curve_compress_highlights" => (
                &["The curve brightens the highlights.", "The curve lifts the shadows by ⅚ stop."],
                &["The curve darkens the shadows and midtones by ⅚ stop and the highlights by half a stop."],
            ),
            "split_tone" => (
                &["The shadows go warmer.", "The highlights go greener."],
                &["The shadows go less saturated, cooler and a third of a stop brighter, while the highlights get slightly more magenta."],
            ),
            "extreme_brighten" => (
                &["It brightens the picture slightly.", "It brightens the picture by five stops."],
                &[
                    "It brightens the whole picture dramatically and desaturates it.",
                    "The shadows get about 2⅔ stops brighter, the midtones about 2⅙ stops brighter and the highlights about 1⅙ stops brighter.",
                ],
            ),
            "extreme_darken" => (
                &["It brightens the picture by four stops.", "It darkens the picture by half a stop."],
                &[
                    "It darkens the whole picture strongly and desaturates it.",
                    "It darkens the shadows by about 3⅔ stops, the midtones by 4⅓ stops and the highlights by 4⅔ stops.",
                ],
            ),
            "gamma_midtones_up" => (
                &["It darkens the midtones.", "It adds ⅔ stop to the highlights."],
                &["It brightens the shadows by about ⅔ stop and the midtones by half a stop, and desaturates the shadows."],
            ),
            "offset_blue" => (
                &["It warms the shadows.", "It cools the highlights by 40 percent."],
                &["It cools the shadows, and slightly cools the midtones and highlights."],
            ),
            "contrast_sat_warm" => (
                &["The shadows get brighter.", "It lowers contrast."],
                &[
                    "The shadows get a third of a stop darker and slightly warmer, while the midtones get a sixth of a stop brighter and the highlights a third of a stop brighter.",
                ],
            ),
            "qualifier_skin_inverted" => (
                &["It brightens the shadows by half a stop.", "The skin tones get brighter by ⅚ stop."],
                &["Outside the selection, the shadows and midtones get about ⅚ of a stop brighter, and the highlights get about ⅔ of a stop brighter."],
            ),
            "lut_node" => (&["The lut warms the picture.", "Nothing changes, but the lut brightens it."], &["No visible change."]),
            "highlight_rolloff" => (
                &["The highlights get brighter.", "The shadows change slightly."],
                &["The highlights change slightly, while the shadows and midtones stay the same."],
            ),
            "qualifier_and_window" => (
                &["The shadows get brighter by ⅚ of a stop.", "The whole frame gets brighter by ⅚ of a stop."],
                &["In the selected skin tones inside the window, the midtones and highlights get about ⅚ of a stop brighter, and the shadows stay the same."],
            ),
            "curve_s_opposite_signs" => (
                &["The curve brightens the picture.", "The curve darkens the picture."],
                &[
                    "The curve darkens the shadows by about a stop and brightens the highlights by about a third of a stop, and saturates the shadows and midtones.",
                ],
            ),
            "tiny_gain_zero_coverage" => (&["It brightens the picture slightly.", "The shadows get a sixth of a stop brighter."], &["No visible change."]),
            _ => (&[], &[]),
        }
    }

    /// Texts that were accepted by an earlier, blacklist checker or are otherwise tempting: each
    /// must fail for the given case (data-driven: add a row for every false accept found).
    const ADVERSARIAL: &[(&str, &str)] = &[
        // binding ambiguities of the heuristic checker (review round 3), now outside the grammar
        // or bound to their own clause; the synthetic_* labels are in `synthetic_cases`
        ("synthetic_highlights_only", "It leaves the shadows brighter."),
        ("synthetic_shadows_down_highlights_up", "The shadows get darker, also the highlights."),
        ("synthetic_shadows_down_highlights_up", "The shadows get darker and the highlights."),
        ("synthetic_shadows_darker_warmer", "Lowers shadows to a cool tint."),
        ("synthetic_shadows_darker_warmer", "It lowers the shadows and adds a cool tint to them."),
        ("synthetic_shadows_darker_warmer", "It lowers the shadows and adds a cool tint to the shadows."),
        ("synthetic_highlights_one_shadows_third", "Highlights brighten by ⅓ stop, shadows by ⅓ stop."),
        ("synthetic_highlights_one_shadows_third", "The highlights brighten by ⅓ stop and the shadows by a stop."),
        ("qualifier_skin_inverted", "Inside the selection, the midtones get brighter."),
        ("qualifier_skin_inverted", "In the selection, the shadows and midtones get about ⅚ of a stop brighter."),
        ("window_inverted_warm", "Inside the window, the midtones and highlights get slightly warmer."),
        ("synthetic_all_minus_one_and_half", "The picture gets darker, slightly."),
        ("synthetic_all_minus_one_and_half", "The picture gets a little darker."),
        ("synthetic_all_minus_one_and_half", "The picture gets a bit darker."),
        ("synthetic_all_minus_one_and_half", "It darkens the picture gently."),
        ("window_left_gain", "It brightens the skin."),
        ("window_left_gain", "The faces get about ⅚ stop brighter."),
        ("window_left_gain", "Inside the window, the sky gets brighter."),
        ("contrast_up", "The shadows get darker than the highlights."),
        ("contrast_up", "The highlights get brighter than the shadows."),
        ("synthetic_highlights_only", "Everything gets brighter."),
        ("synthetic_highlights_only", "The whole picture gets brighter."),
        ("synthetic_highlights_only", "It brightens the picture."),
        ("synthetic_mid_brighter_sat20", "Brightens the midtones by 20 % with more color."),
        ("synthetic_mid_brighter_sat20", "Brightens the midtones by 20 %."),
        // review round 3 of the grammar checker
        ("synthetic_shadows_darker_highlights_warmer", "Node 1 'adds a cool teal look to the skin': the highlights get warmer and the shadows darker."),
        ("synthetic_shadows_darker_highlights_warmer", "Node 7: the highlights get warmer and the shadows darker."),
        ("synthetic_shadows_darker_highlights_warmer", "Node 1 'Skin': the highlights get warmer and the shadows darker."),
        ("synthetic_shadows_darker_highlights_warmer", "The highlights get warmer. Node 1: the shadows get darker."),
        ("synthetic_shadows_darker_highlights_warmer", "The highlights get warmer and the 'shadows' darker."),
        ("synthetic_window_partial", "Everything gets brighter."),
        ("synthetic_window_partial", "It brightens the picture by about ¾ stop."),
        ("synthetic_window_partial", "Brightens by ¾ stop."),
        ("synthetic_window_partial", "Inside it, everything gets brighter."),
        ("synthetic_partial_no_matte", "Everything gets brighter."),
        ("synthetic_partial_no_matte", "It brightens the picture by about ¾ stop."),
        ("synthetic_partial_no_matte", "Brightens by ¾ stop."),
        ("synthetic_darker_more_contrast", "More contrast."),
        ("synthetic_brighter_more_contrast", "More contrast."),
        ("synthetic_shadows_up_highlights_down", "The shadows get brighter."),
        ("synthetic_all_plus_three_quarters", "Everything changes slightly."),
        ("synthetic_all_plus_three_quarters", "It brightens everything slightly."),
        ("synthetic_all_plus_three_quarters", "The picture gets a little brighter."),
        ("synthetic_all_plus_042", "The picture gets much brighter."),
        ("synthetic_shadows_minus1_highlights_minus_half", "The shadows get darker by a stop, but the highlights by ½ stop."),
        ("synthetic_shadows_minus1_highlights_minus_half", "The shadows get darker by a stop; the highlights by ½ stop."),
        ("synthetic_shadows_minus1_highlights_minus_half", "It darkens the shadows by a stop; more contrast, the highlights by ½ stop."),
        ("plus_third_stop", "It affects <100 % of the frame; inside it, everything gets about a third of a stop brighter."),
        ("plus_third_stop", "It affects the whole frame; everything gets 0 stops brighter."),
        // hue and temperature words outside "more / toward"
        ("warm_gain", "It adds a blue cast."),
        ("warm_gain", "A cold, blue look."),
        ("warm_gain", "It removes the orange cast from the shadows."),
        ("warm_gain", "It reduces the warm cast."),
        ("warm_gain", "It adds a yellow tint to the shadows and a blue tint to the highlights."),
        ("temperature_cool_tint_green", "It adds a magenta cast."),
        ("temperature_cool_tint_green", "It removes the green tint."),
        // range synonyms are not in the vocabulary
        ("warm_gain", "The dark areas get brighter."),
        ("warm_gain", "The lows get brighter."),
        ("warm_gain", "The blacks get brighter."),
        ("warm_gain", "The lights get warmer."),
        ("warm_gain", "The brightest parts get warmer."),
        ("warm_gain", "The mids get warmer."),
        ("warm_gain", "The highs get warmer."),
        ("warm_gain", "The darks get warmer."),
        ("warm_gain", "The whites get warmer."),
        // no range named, touched ranges disagree
        ("contrast_up", "Everything gets brighter."),
        ("contrast_up", "The picture gets darker."),
        ("curve_s_opposite_signs", "The curve brightens the picture."),
        // coverage complement and percentages next to brightness / warmth words
        ("qualifier_skin_gain", "It affects 87 percent of the frame."),
        ("plus_third_stop", "It brightens the picture by 96 percent."),
        ("plus_third_stop", "It brightens by 50 percent."),
        ("temperature_warm", "The picture gets warmer by 70 percent."),
        ("temperature_warm", "It warms 70 percent."),
        // number words
        ("saturation_up", "Colors get seventy-three percent more saturated."),
        ("saturation_up", "Colors get forty points more saturated."),
        ("saturation_up", "Colors get point nine percent more saturated."),
        ("plus_third_stop", "It brightens the picture by one point five stops."),
        ("plus_third_stop", "It brightens the picture by one."),
        ("plus_third_stop", "It brightens the picture by one stop."),
        ("plus_third_stop", "It brightens the picture by point nine stops."),
        // disabled / matte view / no-op nodes: only the whitelisted phrasings
        ("disabled_gain", "Disabled, but it brightens the picture slightly."),
        ("disabled_gain", "It is disabled and warms the picture."),
        ("disabled_gain", "Nothing changes, but the picture is brighter."),
        ("disabled_gain", "It is disabled and it is a bit warmer."),
        ("showing_matte", "The node is showing its qualifier matte and brightening the skin."),
        ("showing_matte", "It is showing the matte, so it warms the picture."),
        ("identity", "No visible change, but a little warmer."),
        ("identity", "It is not disabled."),
        // lexicon outside the vocabulary
        ("plus_third_stop", "It opens up the shadows."),
        ("plus_third_stop", "It gives deeper shadows."),
        ("plus_third_stop", "It adds punch."),
        ("plus_third_stop", "It adds depth."),
        ("plus_third_stop", "It recovers shadow detail."),
        ("plus_third_stop", "The left side gets brighter."),
        // verbs with a direction
        ("saturation_down", "It saturates the picture."),
        ("saturation_up", "It desaturates the picture."),
        // strength and negation
        ("extreme_brighten", "It hardly brightens the picture."),
        ("extreme_brighten", "It gently brightens the picture."),
        ("plus_third_stop", "It adds more."),
        ("plus_third_stop", "It adds a tint."),
        // an inverted qualifier does not change the skin
        ("qualifier_skin_inverted", "The selected skin tones get brighter by ⅚ stop."),
    ];

    #[test]
    fn thirty_cases_and_their_templates_pass() {
        let cases = eval_cases();
        assert_eq!(cases.len(), 36);
        let labels: std::collections::BTreeSet<_> = cases.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels.len(), 36, "unique labels");
        for c in &cases {
            assert_eq!(check_explanation_labeled(&c.facts, None, Some(1), &c.template), Ok(()), "{}: {}", c.label, c.template);
        }
    }

    #[test]
    fn second_set_is_fixed_distinct_and_its_templates_pass() {
        let cases = eval_cases_v2();
        assert_eq!(cases.len(), 40);
        assert_eq!(cases, eval_cases_v2(), "deterministic");
        let labels: std::collections::BTreeSet<_> = cases.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels.len(), cases.len(), "unique labels");
        let first = eval_cases();
        let mut rejected = Vec::new();
        for c in &cases {
            assert!(c.label.starts_with("h2_"), "{}", c.label);
            assert!(first.iter().all(|f| f.label != c.label), "{}", c.label);
            assert_eq!(check_grammar(&c.template), Ok(()), "{}", c.template);
            if let Err(e) = check_explanation_labeled(&c.facts, None, Some(1), &c.template) {
                rejected.push(format!("{}: {e}: {}", c.label, c.template));
            }
        }
        // Known checker / template disagreement (reported, not fixed here): on this frame noise
        // reduction changes 1 % of the frame (the shadows), and the checker reads the template's
        // "also reduces noise" as a whole-picture claim while the midtones are untouched.
        assert!(rejected.len() == 1 && rejected.iter().all(|r| r.starts_with("h2_noise_reduction_only:")), "{}", rejected.join("\n"));
        // another frame: the visible changes measure differently from the first set's
        let visible: Vec<&EvalCase> = cases.iter().filter(|c| c.facts.enabled && !c.facts.showing_matte && c.facts.visible()).collect();
        assert!(visible.len() >= 30, "{} visible cases", visible.len());
        assert!(visible.iter().all(|c| first.iter().all(|f| f.facts != c.facts)), "a visible case repeats the first set's facts");
        // every kind is represented: changes, a disabled node, a matte view, a no-op
        assert!(cases.iter().any(|c| !c.facts.enabled));
        assert!(cases.iter().any(|c| c.facts.showing_matte));
        assert!(cases.iter().any(|c| c.facts.enabled && !c.facts.showing_matte && !c.facts.visible()));
        // the LUT case uses a real look, so it changes the picture
        assert!(cases.iter().any(|c| c.label == "h2_lut_look" && c.facts.visible()));
    }

    #[test]
    fn third_set_is_fixed_distinct_and_its_templates_pass() {
        let cases = eval_cases_v3();
        assert_eq!(cases.len(), 13);
        assert_eq!(cases, eval_cases_v3(), "deterministic");
        let labels: std::collections::BTreeSet<_> = cases.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels.len(), cases.len(), "unique labels");
        let others: Vec<EvalCase> = eval_cases().into_iter().chain(eval_cases_v2()).collect();
        for c in &cases {
            assert!(c.label.starts_with("h3_"), "{}", c.label);
            assert_eq!(check_grammar(&c.template), Ok(()), "{}", c.template);
            assert_eq!(check_explanation_labeled(&c.facts, None, Some(1), &c.template).map_err(|e| e.to_string()), Ok(()), "{}: {}", c.label, c.template);
            if c.facts.visible() {
                assert!(others.iter().all(|f| f.facts != c.facts), "{} repeats another set's facts", c.label);
            }
        }
        // the white-balance cases change the picture
        let invisible: Vec<&str> = cases.iter().filter(|c| c.label.starts_with("h3_wb_") && !c.facts.visible()).map(|c| c.label.as_str()).collect();
        assert!(invisible.is_empty(), "{invisible:?}");
    }

    #[test]
    fn sixth_set_is_fixed_distinct_and_its_templates_pass() {
        let cases = eval_cases_v6();
        assert_eq!(cases.len(), 13);
        assert_eq!(cases, eval_cases_v6(), "deterministic");
        let labels: std::collections::BTreeSet<_> = cases.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels.len(), cases.len(), "unique labels");
        let others: Vec<EvalCase> = eval_cases().into_iter().chain(eval_cases_v2()).chain(eval_cases_v3()).collect();
        for c in &cases {
            assert!(c.label.starts_with("h6_"), "{}", c.label);
            assert_eq!(check_grammar(&c.template), Ok(()), "{}", c.template);
            assert_eq!(check_explanation_labeled(&c.facts, None, Some(1), &c.template).map_err(|e| e.to_string()), Ok(()), "{}: {}", c.label, c.template);
            assert!(others.iter().all(|f| f.facts != c.facts), "{} repeats another set's facts", c.label);
        }
        // every case changes the picture (each tool is really exercised)
        let invisible: Vec<String> = cases.iter().filter(|c| !c.facts.visible()).map(|c| format!("{}: {}", c.label, c.template)).collect();
        assert!(invisible.is_empty(), "{invisible:#?}");
        // the 3D-qualifier cases carry the 3D facts and act on part of the frame
        for c in cases.iter().filter(|c| c.label.starts_with("h6_qual3d_")) {
            let q = c.facts.controls.qualifier.as_ref().expect("a qualifier");
            assert!(q.three_d, "{}", c.label);
        }
        assert!(cases.iter().filter(|c| c.label.starts_with("h6_hdr_")).all(|c| !c.facts.controls.hdr.is_empty()));
    }

    #[test]
    fn wrong_explanations_fail_and_paraphrases_pass() {
        let mut problems = Vec::new();
        for c in eval_cases() {
            let (wrong, right) = hand_written(&c.label);
            if wrong.len() < 2 || right.is_empty() {
                problems.push(format!("{}: needs hand-written explanations", c.label));
            }
            for w in wrong {
                if let Ok(()) = check_explanation(&c.facts, w) {
                    problems.push(format!("{}: wrong text accepted: {w}", c.label));
                }
                // wrong texts stay inside the vocabulary: the facts reject them, not the word list
                if let Err(e) = check_vocabulary(w) {
                    problems.push(format!("{}: wrong text outside the vocabulary ({e}): {w}", c.label));
                }
            }
            for r in right {
                if let Err(e) = check_grammar(r) {
                    problems.push(format!("{}: right text outside the grammar ({e}): {r}", c.label));
                }
                if let Err(e) = check_explanation(&c.facts, r) {
                    problems.push(format!("{}: right text rejected ({e}): {r}", c.label));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn a_text_for_another_case_is_usually_wrong() {
        // every template is checked against every other case's facts: most must fail (the
        // checker doesn't just accept anything template-shaped)
        let cases = eval_cases();
        let (mut accepted, mut total) = (0, 0);
        for a in &cases {
            for b in &cases {
                if a.label != b.label {
                    total += 1;
                    accepted += usize::from(check_explanation_labeled(&a.facts, None, Some(1), &b.template).is_ok());
                }
            }
        }
        assert!(accepted * 10 < total, "{accepted} of {total} cross-case templates accepted");
    }

    #[test]
    fn truncated_and_mangled_texts_never_panic() {
        for c in eval_cases() {
            let (_, right) = hand_written(&c.label);
            for t in std::iter::once(&c.template.as_str()).chain(right) {
                let cs: Vec<char> = t.chars().collect();
                for i in 0..=cs.len() {
                    let (head, tail): (String, String) = (cs.iter().take(i).collect(), cs.iter().skip(i).collect());
                    let _ = (check_explanation(&c.facts, &head), check_explanation(&c.facts, &tail));
                    let _ = check_explanation(&c.facts, &format!("{head}1e999{tail}"));
                    let _ = check_explanation(&c.facts, &format!("{head} NaN % ½ {tail}"));
                }
            }
        }
    }

    #[test]
    fn facts_round_trip_through_json() {
        for c in eval_cases() {
            let line = serde_json::to_string(&c).unwrap_or_default();
            let back: EvalCase = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{}: {e}", c.label));
            assert_eq!(back, c);
        }
        assert!(serde_json::from_str::<NodeFacts>("{}").is_err());
        let bad = serde_json::to_string(&eval_cases().first().map(|c| c.facts.clone())).unwrap_or_default().replace("shadows", "lows");
        assert!(serde_json::from_str::<Option<NodeFacts>>(&bad).is_err());
        // ranges must come in order, numbers must be finite
        let json = serde_json::to_string(&eval_cases().get(3).map(|c| c.facts.clone())).unwrap_or_default();
        let swapped = json.replacen("\"shadows\"", "\"X\"", 1).replacen("\"midtones\"", "\"shadows\"", 1).replacen("\"X\"", "\"midtones\"", 1);
        assert!(serde_json::from_str::<Option<NodeFacts>>(&json).is_ok_and(|f| f.is_some()));
        assert!(serde_json::from_str::<Option<NodeFacts>>(&swapped).is_err());
        assert!(serde_json::from_str::<Option<NodeFacts>>(&json.replacen("\"coverage\":", "\"coverage\":1e999,\"x\":", 1)).is_err());
    }

    /// `FILMCRAFT_DUMP_EXPLAIN_CASES=<path> cargo test -p filmcraft-render dump_explain_cases`
    /// writes the cases as JSON lines `{label, facts, template}` for the model crate's eval.
    #[test]
    fn dump_explain_cases() {
        let Ok(path) = std::env::var("FILMCRAFT_DUMP_EXPLAIN_CASES") else { return };
        let lines: Vec<String> = eval_cases().iter().filter_map(|c| serde_json::to_string(c).ok()).collect();
        assert_eq!(lines.len(), 36);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap_or_else(|e| panic!("{path}: {e}"));
    }

    fn rf(range: &'static str, stops: f32, warmth: f32, sat: Option<f32>) -> RangeFacts {
        RangeFacts { range, touched: true, share: 0.9, stops, warmth, tint: 0.0, saturation: sat }
    }

    /// Hand-made facts for the adversarial rows that no grade of the eval set reproduces.
    fn synthetic_cases() -> Vec<EvalCase> {
        let u = RangeFacts::untouched;
        let mk = |label: &str, ranges: [RangeFacts; 3]| {
            let facts =
                NodeFacts { enabled: true, showing_matte: false, measured: true, controls: Default::default(), coverage: 0.96, matte_coverage: None, ranges };
            EvalCase { label: label.into(), template: facts.sentence("Node 1"), facts }
        };
        vec![
            mk("synthetic_highlights_only", [u("shadows"), u("midtones"), rf("highlights", 0.5, 0.0, None)]),
            mk("synthetic_shadows_down_highlights_up", [rf("shadows", -0.5, 0.0, None), u("midtones"), rf("highlights", 0.5, 0.0, None)]),
            mk("synthetic_shadows_darker_warmer", [rf("shadows", -0.5, 0.05, None), u("midtones"), u("highlights")]),
            mk("synthetic_highlights_one_shadows_third", [rf("shadows", 0.33, 0.0, None), u("midtones"), rf("highlights", 1.0, 0.0, None)]),
            mk("synthetic_all_minus_one_and_half", [rf("shadows", -1.5, 0.0, None), rf("midtones", -1.5, 0.0, None), rf("highlights", -1.5, 0.0, None)]),
            mk("synthetic_mid_brighter_sat20", [u("shadows"), rf("midtones", 0.33, 0.0, Some(20.0)), u("highlights")]),
            mk("synthetic_shadows_darker_highlights_warmer", [rf("shadows", -0.5, 0.0, None), u("midtones"), rf("highlights", 0.0, 0.05, None)]),
            {
                let mut c =
                    mk("synthetic_window_partial", [rf("shadows", 0.75, 0.0, None), rf("midtones", 0.75, 0.0, None), rf("highlights", 0.75, 0.0, None)]);
                c.facts.controls.windows = 1;
                c.facts.coverage = 0.3;
                c.facts.matte_coverage = Some(0.3);
                c.template = c.facts.sentence("Node 1");
                c
            },
            {
                let mut c =
                    mk("synthetic_partial_no_matte", [rf("shadows", 0.75, 0.0, None), rf("midtones", 0.75, 0.0, None), rf("highlights", 0.75, 0.0, None)]);
                c.facts.coverage = 0.3;
                c.template = c.facts.sentence("Node 1");
                c
            },
            mk("synthetic_darker_more_contrast", [rf("shadows", -1.5, 0.0, None), rf("midtones", -1.25, 0.0, None), rf("highlights", -1.0, 0.0, None)]),
            mk("synthetic_brighter_more_contrast", [rf("shadows", 0.2, 0.0, None), rf("midtones", 0.6, 0.0, None), rf("highlights", 1.0, 0.0, None)]),
            mk("synthetic_shadows_up_highlights_down", [rf("shadows", 1.0, 0.0, None), u("midtones"), rf("highlights", -1.0, 0.0, None)]),
            mk("synthetic_all_plus_three_quarters", [rf("shadows", 0.75, 0.0, None), rf("midtones", 0.75, 0.0, None), rf("highlights", 0.75, 0.0, None)]),
            mk("synthetic_all_plus_042", [rf("shadows", 0.42, 0.0, None), rf("midtones", 0.42, 0.0, None), rf("highlights", 0.42, 0.0, None)]),
            mk("synthetic_shadows_minus1_highlights_minus_half", [rf("shadows", -1.0, 0.0, None), u("midtones"), rf("highlights", -0.5, 0.0, None)]),
        ]
    }

    /// The true counterparts of the adversarial synthetic rows pass (so the rows fail for the
    /// reason they were written for).
    #[test]
    fn synthetic_positive_texts_pass() {
        let cases = synthetic_cases();
        let get = |l: &str| cases.iter().find(|c| c.label == l).map(|c| c.facts.clone());
        let rows: &[(&str, Option<&str>, Option<usize>, &str)] = &[
            ("synthetic_shadows_darker_highlights_warmer", Some("Skin"), Some(1), "Node 1 'Skin': the highlights get warmer and the shadows darker."),
            ("synthetic_shadows_darker_highlights_warmer", Some("Skin"), Some(1), "Node 1: the highlights get warmer and the shadows darker."),
            ("synthetic_shadows_darker_highlights_warmer", None, None, "The highlights get warmer and the shadows darker."),
            ("synthetic_window_partial", None, None, "Inside the window, everything gets about ¾ stop brighter."),
            ("synthetic_partial_no_matte", None, None, "It affects 30 % of the frame; inside it, everything gets about ¾ stop brighter."),
            ("synthetic_darker_more_contrast", None, None, "Everything gets much darker; more contrast."),
            ("synthetic_shadows_up_highlights_down", None, None, "The shadows get a stop brighter and the highlights a stop darker."),
            ("synthetic_all_plus_three_quarters", None, None, "It brightens everything by about ¾ stop."),
            ("synthetic_all_plus_042", None, None, "The picture gets about half a stop brighter."),
            ("synthetic_shadows_minus1_highlights_minus_half", None, None, "The shadows get darker by a stop, and the highlights by ½ stop."),
        ];
        let mut problems = Vec::new();
        for (l, label, index, t) in rows {
            let Some(f) = get(l) else {
                problems.push(format!("unknown {l}"));
                continue;
            };
            if let Err(e) = check_explanation_labeled(&f, *label, *index, t) {
                problems.push(format!("{l}: {t} ({e})"));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
        // a label never passes without the node's index
        let f = get("synthetic_shadows_darker_highlights_warmer").unwrap_or_else(|| eval_cases().remove(1).facts);
        assert!(check_explanation(&f, "Node 1: the highlights get warmer and the shadows darker.").is_err());
    }

    #[test]
    fn synthetic_templates_pass() {
        for c in synthetic_cases() {
            assert_eq!(check_explanation_labeled(&c.facts, None, Some(1), &c.template), Ok(()), "{}: {}", c.label, c.template);
        }
    }

    #[test]
    fn adversarial_texts_fail() {
        let mut cases = eval_cases();
        cases.extend(synthetic_cases());
        let mut problems = Vec::new();
        for (label, text) in ADVERSARIAL {
            let Some(c) = cases.iter().find(|c| c.label == *label) else {
                problems.push(format!("unknown case {label}"));
                continue;
            };
            if let Ok(()) = check_explanation_labeled(&c.facts, None, Some(1), text) {
                problems.push(format!("{label}: accepted: {text}"));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn vocabulary_is_closed_and_documented() {
        let mut v: Vec<&str> = VOCABULARY.to_vec();
        v.sort_unstable();
        let n = v.len();
        v.dedup();
        assert_eq!(v.len(), n, "no duplicate words");
        assert!(v.iter().all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase())));
        eprintln!("vocabulary: {n} words");
        // every word of every template and paraphrase is in it
        for c in eval_cases() {
            assert_eq!(check_vocabulary(&c.template), Ok(()), "{}", c.template);
            assert_eq!(check_grammar(&c.template), Ok(()), "{}", c.template);
        }
        assert!(matches!(check_vocabulary("It makes the picture warmer."), Err(crate::grade_explain::FactMismatch::UnknownWord(_))));
        assert!(check_vocabulary("It warms the picture!").is_ok() && check_vocabulary("It warms the picture @").is_err());
    }

    #[test]
    fn the_branch_set_covers_every_kind_and_its_templates_pass() {
        let cases = branch_eval_cases();
        assert!(cases.len() >= 15, "{}", cases.len());
        let mut labels: Vec<&str> = cases.iter().map(|c| c.label.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), cases.len(), "labels must be unique");
        for kind in ["parallel", "layer", "splitter", "combiner", "key_mixer", "limited"] {
            assert!(cases.iter().any(|c| serde_json::to_string(&c.facts).unwrap_or_default().contains(&format!("\"kind\":\"{kind}\""))), "{kind}");
        }
        for c in &cases {
            assert_eq!(super::super::check_branch_explanation(&c.facts, &c.template), Ok(()), "{}: {}", c.label, c.template);
        }
        let near = cases.iter().find(|c| c.label.starts_with("b5_parallel_near_equal")).map(|c| c.template.clone()).unwrap_or_default();
        assert!(near.ends_with("blended 51 / 49."), "{near}");
    }
}
