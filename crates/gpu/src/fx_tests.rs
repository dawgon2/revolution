//! GPU-vs-CPU parity of the effect stage (`filmcraft_render::gpufx::FxOp::apply` is the
//! reference).
//!
//! Tolerances:
//! - Exact checks ([`gpu_effects_match_cpu_exactly`]): the working image read back as f32 from a
//!   half-float-representable source (so the source draw is lossless) must match the CPU op to
//!   [`EXACT`] per value (f32 arithmetic and the GPU's `pow` / `sqrt` differ in the last bits).
//!   Step functions (Posterize, Color Pass, Leave Color and Change to Color keys, Extract) may
//!   flip a pixel that lands on a threshold; at most [`FLIPS`] of the values may differ by more.
//! - Composited plans ([`gpu_effect_layers_match_cpu_plan`]): the compositor's criterion (p99 of
//!   the per-pixel max 8-bit channel difference ≤ 6, mean < 1.5) away from layer outlines, as for
//!   the blend modes.

use super::blend_tests::{device, interior, ramp_layer, stats8, yuv_frame};
use super::*;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::{EffectInstance, ParamValue, find_effect};
use filmcraft_render::effects::FxCtx;
use filmcraft_render::gpufx::FxOp;
use filmcraft_render::plan::{LayerFx, execute_cpu};
use filmcraft_time::Tick;

/// A compositor with full-precision (`Rgba32Float`) working textures on every adapter: the exact
/// parity checks are about that stage; integrated GPUs' default half-float stage has its own
/// tests (`half_float_*`).
pub(super) fn full_precision(dev: &wgpu::Device, q: &wgpu::Queue) -> GpuCompositor {
    GpuCompositor::with_fx_format(dev, q, fx::FX_FORMAT)
}

const EXACT: f32 = 1e-4;
const HALF_EXACT: f32 = 2e-3;
const FLIPS: f64 = 0.01;

fn cx(t: Tick, px_scale: f32) -> FxCtx<'static> {
    FxCtx { t, px_scale, seconds: 0.0, timecode: "", clip_name: "", project: None, env: None, working: filmcraft_color::WorkingSpace::Rec709 }
}

fn effect(id: &str, params: &[(&str, ParamValue)]) -> EffectInstance {
    let mut e = find_effect(id).unwrap_or_else(|| panic!("{id}")).instance();
    for (k, v) in params {
        e.params.get_mut(*k).unwrap_or_else(|| panic!("{id}.{k}")).value = v.clone();
    }
    e
}

fn fl(v: f64) -> ParamValue {
    ParamValue::Float(v)
}
fn col(r: f32, g: f32, b: f32) -> ParamValue {
    ParamValue::Color([r, g, b, 1.0])
}
fn ch(c: u32) -> ParamValue {
    ParamValue::Choice(c)
}
fn pt(x: f64, y: f64) -> ParamValue {
    ParamValue::Vec2(Vec2::new(x, y))
}

pub(super) fn half(v: f32) -> f32 {
    super::blend_tests::f16_to_f32(f32_to_f16(v))
}

/// Linear premultiplied test picture, every value representable in half floats: hue sweep along
/// x, lightness along y, a grey ramp, black and white columns, super-whites (up to 1.6), and
/// alpha 1 / partial / 0 bands.
pub(super) fn picture(w: u32, h: u32) -> Vec<f32> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / (w - 1) as f32;
            let fy = y as f32 / (h - 1) as f32;
            let c = match x % 11 {
                0 => [0.0, 0.0, 0.0],
                1 => [1.0, 1.0, 1.0],
                2 => [fy, fy, fy],
                3 => [1.6 * fy, 1.2 * fy, 0.3],
                _ => {
                    let rgb = filmcraft_color::hsl_to_rgb(fx, 0.25 + 0.75 * ((y % 5) as f32 / 4.0), 0.1 + 0.8 * fy);
                    rgb.map(filmcraft_color::srgb_to_linear)
                }
            };
            let a = match y % 7 {
                0 => 0.0,
                1 | 2 => 0.5 + 0.4 * fx,
                3 => 0.25,
                _ => 1.0,
            };
            px.extend_from_slice(&[c[0] * a, c[1] * a, c[2] * a, a].map(half));
        }
    }
    px
}

/// Max |cpu − gpu| (finite values required) and the share of values off by more than 0.02.
pub(super) fn compare(cpu: &[f32], gpu: &[f32]) -> (f32, f64) {
    assert_eq!(cpu.len(), gpu.len());
    let mut worst = 0f32;
    let mut flips = 0usize;
    for (a, b) in cpu.iter().zip(gpu) {
        assert!(b.is_finite(), "non-finite GPU value");
        let d = (a - b).abs() / (1.0 + a.abs());
        if d > 0.02 {
            flips += 1;
        } else {
            worst = worst.max(d);
        }
    }
    (worst, flips as f64 / cpu.len() as f64)
}

/// Effect cases: (effect id, parameters, whether a threshold may flip pixels).
fn cases() -> Vec<(&'static str, Vec<(&'static str, ParamValue)>, bool)> {
    vec![
        ("brightness_contrast", vec![("brightness", fl(30.0)), ("contrast", fl(40.0))], false),
        ("brightness_contrast", vec![("brightness", fl(-100.0)), ("contrast", fl(-100.0))], false),
        ("brightness_contrast", vec![("brightness", fl(100.0)), ("contrast", fl(100.0))], false),
        ("brightness_contrast", vec![("brightness", fl(-20.0)), ("contrast", fl(900.0))], false),
        ("proc_amp", vec![("brightness", fl(10.0)), ("contrast", fl(130.0)), ("hue", fl(200.0)), ("saturation", fl(150.0))], false),
        ("proc_amp", vec![("hue", fl(-400.0)), ("saturation", fl(0.0))], false),
        ("tint", vec![], false),
        ("tint", vec![("black", col(0.2, 0.1, 0.5)), ("white", col(1.0, 0.9, 0.3)), ("amount", fl(60.0))], false),
        ("black_white", vec![], false),
        ("color_balance", vec![("shadow_r", fl(50.0)), ("mid_g", fl(-40.0)), ("hi_b", fl(80.0)), ("shadow_b", fl(-100.0)), ("hi_r", fl(100.0))], false),
        ("color_balance", vec![("mid_r", fl(70.0)), ("hi_g", fl(-60.0)), ("preserve", ParamValue::Bool(true))], false),
        ("leave_color", vec![("amount", fl(80.0)), ("tolerance", fl(20.0)), ("softness", fl(30.0))], true),
        ("leave_color", vec![("amount", fl(100.0)), ("color", col(0.1, 0.8, 0.2)), ("tolerance", fl(0.0)), ("softness", fl(0.0))], true),
        ("change_to_color", vec![], true),
        ("change_to_color", vec![("hue_tol", fl(30.0)), ("softness", fl(10.0)), ("to", col(0.9, 0.9, 0.1))], true),
        ("color_pass", vec![("similarity", fl(30.0))], true),
        ("color_pass", vec![("similarity", fl(45.0)), ("reverse", ParamValue::Bool(true)), ("color", col(0.2, 0.4, 0.9))], true),
        ("gamma_correction", vec![("gamma", fl(5.0))], false),
        ("gamma_correction", vec![("gamma", fl(28.0))], false),
        ("gamma_correction", vec![("gamma", fl(0.0))], false),
        ("gamma_correction", vec![("gamma", fl(-3.0))], false),
        ("levels", vec![("in_black", fl(20.0)), ("in_white", fl(230.0)), ("out_black", fl(10.0)), ("out_white", fl(240.0)), ("gamma", fl(150.0))], false),
        ("levels", vec![("in_black", fl(200.0)), ("in_white", fl(0.0)), ("gamma", fl(5.0)), ("out_white", fl(-50.0))], true),
        ("extract", vec![("black", fl(40.0)), ("white", fl(200.0)), ("softness", fl(30.0))], true),
        ("extract", vec![("black", fl(90.0)), ("white", fl(160.0)), ("invert", ParamValue::Bool(true))], true),
        ("invert", vec![("blend", fl(30.0))], false),
        ("invert", vec![("channel", ParamValue::Choice(1))], false),
        ("invert", vec![("channel", ParamValue::Choice(2)), ("blend", fl(70.0))], false),
        ("invert", vec![("channel", ParamValue::Choice(3))], false),
        ("invert", vec![("channel", ParamValue::Choice(4)), ("blend", fl(20.0))], false),
        ("invert", vec![("channel", ParamValue::Choice(9))], false),
        ("posterize", vec![("levels", fl(2.0))], true),
        ("posterize", vec![("levels", fl(7.0))], true),
        ("posterize", vec![("levels", fl(255.0))], true),
        ("gaussian_blur", vec![("blurriness", fl(0.0))], false),
        ("gaussian_blur", vec![("blurriness", fl(5.0))], false),
        ("gaussian_blur", vec![("blurriness", fl(5.0)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("gaussian_blur", vec![("blurriness", fl(80.0))], false),
        ("gaussian_blur", vec![("blurriness", fl(80.0)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("gaussian_blur", vec![("blurriness", fl(12.0)), ("dimensions", ParamValue::Choice(1))], false),
        ("gaussian_blur", vec![("blurriness", fl(12.0)), ("dimensions", ParamValue::Choice(2)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("gaussian_blur", vec![("blurriness", fl(3000.0)), ("repeat_edge", ParamValue::Bool(true))], false),
        ("gaussian_blur", vec![("blurriness", fl(1e9))], false),
        ("gaussian_blur", vec![("blurriness", fl(-50.0))], false),
        ("camera_blur", vec![("percent", fl(20.0))], false),
        ("directional_blur", vec![("length", fl(10.0)), ("direction", fl(30.0))], false),
        ("directional_blur", vec![("length", fl(0.2))], false),
        ("directional_blur", vec![("length", fl(1000.0)), ("direction", fl(-100.0))], false),
        ("sharpen", vec![("amount", fl(80.0))], false),
        ("sharpen", vec![("amount", fl(4000.0))], false),
        ("unsharp_mask", vec![("amount", fl(150.0)), ("radius", fl(3.0)), ("threshold", fl(10.0))], true),
        ("unsharp_mask", vec![("radius", fl(0.1))], false),
        ("unsharp_mask", vec![("amount", fl(300.0)), ("radius", fl(250.0))], false),
        ("crop", vec![("left", fl(10.0)), ("top", fl(5.0)), ("right", fl(20.0)), ("bottom", fl(15.0))], false),
        ("crop", vec![("left", fl(10.0)), ("top", fl(5.0)), ("right", fl(20.0)), ("bottom", fl(15.0)), ("feather", fl(12.0))], false),
        ("crop", vec![("left", fl(12.0)), ("top", fl(5.0)), ("right", fl(20.0)), ("zoom", ParamValue::Bool(true))], false),
        ("crop", vec![("left", fl(70.0)), ("right", fl(70.0)), ("feather", fl(5000.0))], false),
        ("edge_feather", vec![("amount", fl(30.0))], false),
        ("transform", vec![("scale_height", fl(150.0)), ("rotation", fl(30.0)), ("position", pt(30.0, 20.0)), ("opacity", fl(60.0))], false),
        (
            "transform",
            vec![
                ("uniform_scale", ParamValue::Bool(false)),
                ("scale_width", fl(80.0)),
                ("scale_height", fl(120.0)),
                ("skew", fl(20.0)),
                ("skew_axis", fl(45.0)),
                ("anchor", pt(10.0, 30.0)),
            ],
            false,
        ),
        ("horizontal_flip", vec![], false),
        ("vertical_flip", vec![], false),
        ("mirror", vec![("center", pt(20.0, 10.0)), ("angle", fl(30.0))], false),
        ("mirror", vec![("angle", fl(200.0))], false),
        ("offset", vec![("shift", pt(10.0, 30.0)), ("blend", fl(25.0))], false),
        ("offset", vec![("shift", pt(-1e5, 7.25))], false),
        ("asc_cdl", vec![("r_slope", fl(1.4)), ("g_offset", fl(-0.1)), ("b_power", fl(2.2)), ("saturation", fl(1.6))], false),
        ("asc_cdl", vec![("r_power", fl(0.0)), ("g_slope", fl(0.0)), ("saturation", fl(0.0))], false),
        ("channel_mix", vec![("rr", fl(50.0)), ("rg", fl(60.0)), ("gb", fl(-80.0)), ("bc", fl(30.0))], false),
        ("channel_mix", vec![("rb", fl(120.0)), ("monochrome", ParamValue::Bool(true))], false),
        ("color_replace", vec![("similarity", fl(40.0))], true),
        ("color_replace", vec![("similarity", fl(25.0)), ("solid", ParamValue::Bool(true)), ("target", col(0.3, 0.6, 0.2))], true),
        ("color_replace", vec![("similarity", fl(0.0))], true),
        ("alpha_adjust", vec![("opacity", fl(60.0))], false),
        ("alpha_adjust", vec![("ignore", ParamValue::Bool(true)), ("invert", ParamValue::Bool(true))], false),
        ("alpha_adjust", vec![("invert", ParamValue::Bool(true)), ("mask_only", ParamValue::Bool(true)), ("opacity", fl(150.0))], false),
        ("luma_key", vec![("threshold", fl(60.0)), ("cutoff", fl(20.0))], true),
        ("luma_key", vec![("threshold", fl(10.0)), ("cutoff", fl(50.0))], true),
        ("luma_key", vec![], true),
        ("color_key", vec![("color", col(0.1, 0.8, 0.2)), ("tolerance", fl(60.0)), ("feather", fl(10.0))], true),
        ("color_key", vec![("color", col(0.9, 0.2, 0.6)), ("tolerance", fl(200.0))], true),
        ("color_key", vec![], true),
        ("ultra_key", vec![], true),
        ("ultra_key", vec![("output", ch(1))], true),
        ("ultra_key", vec![("output", ch(2)), ("key_color", col(0.1, 0.2, 0.9))], true),
        ("ultra_key", vec![("setting", ch(2))], true),
        ("ultra_key", vec![("setting", ch(3)), ("choke", fl(40.0))], true),
        ("ultra_key", vec![("setting", ch(3)), ("soften", fl(30.0)), ("contrast", fl(50.0)), ("mid_point", fl(40.0))], true),
        (
            "ultra_key",
            vec![
                ("setting", ch(3)),
                ("choke", fl(20.0)),
                ("soften", fl(10.0)),
                ("spill", fl(100.0)),
                ("key_color", col(0.8, 0.1, 0.1)),
                ("cc_saturation", fl(150.0)),
                ("cc_hue", fl(30.0)),
                ("cc_luminance", fl(90.0)),
            ],
            true,
        ),
        ("vignette", vec![], false),
        ("vignette", vec![("amount", fl(-100.0)), ("midpoint", fl(50.0)), ("roundness", fl(0.0)), ("feather", fl(50.0))], false),
        (
            "vignette",
            vec![("amount", fl(-30.0)), ("midpoint", fl(20.0)), ("roundness", fl(-100.0)), ("feather", fl(0.0)), ("color", col(0.1, 0.4, 0.7))],
            false,
        ),
        ("vignette", vec![("amount", fl(40.0)), ("midpoint", fl(90.0)), ("roundness", fl(100.0)), ("feather", fl(100.0))], false),
        ("video_limiter", vec![], false),
        ("video_limiter", vec![("axis", ch(0)), ("clip_level", ch(0)), ("compression", ch(0))], false),
        ("video_limiter", vec![("axis", ch(1)), ("clip_level", ch(9)), ("compression", ch(4))], false),
        ("video_limiter", vec![("axis", ch(2)), ("clip_level", ch(5)), ("compression", ch(2))], false),
        ("video_limiter", vec![("axis", ch(3)), ("clip_level", ch(0)), ("compression", ch(1))], false),
        (
            "video_limiter",
            vec![
                ("axis", ch(2)),
                ("clip_level", ch(0)),
                ("compression", ch(0)),
                ("gamut_warning", ParamValue::Bool(true)),
                ("warning_color", col(0.0, 1.0, 0.0)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![
                ("temperature", fl(20.0)),
                ("tint", fl(-10.0)),
                ("exposure", fl(0.5)),
                ("contrast", fl(15.0)),
                ("highlights", fl(-20.0)),
                ("shadows", fl(25.0)),
                ("whites", fl(10.0)),
                ("blacks", fl(-15.0)),
                ("saturation", fl(110.0)),
                ("creative_on", ParamValue::Bool(false)),
                ("vignette_on", ParamValue::Bool(false)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![
                ("basic_on", ParamValue::Bool(false)),
                ("creative_sat", fl(120.0)),
                ("vibrance", fl(30.0)),
                ("faded_film", fl(25.0)),
                ("shadow_tint", col(0.4, 0.45, 0.6)),
                ("highlight_tint", col(0.6, 0.55, 0.4)),
                ("vignette_on", ParamValue::Bool(false)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![
                ("basic_on", ParamValue::Bool(false)),
                ("creative_on", ParamValue::Bool(false)),
                ("vignette_amount", fl(-3.0)),
                ("vignette_midpoint", fl(45.0)),
                ("vignette_roundness", fl(-30.0)),
                ("vignette_feather", fl(60.0)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![
                ("temperature", fl(-15.0)),
                ("tint", fl(10.0)),
                ("exposure", fl(0.3)),
                ("contrast", fl(20.0)),
                ("highlights", fl(-10.0)),
                ("shadows", fl(15.0)),
                ("whites", fl(-5.0)),
                ("blacks", fl(5.0)),
                ("saturation", fl(105.0)),
                ("creative_sat", fl(110.0)),
                ("vibrance", fl(20.0)),
                ("faded_film", fl(15.0)),
                ("shadow_tint", col(0.48, 0.5, 0.55)),
                ("highlight_tint", col(0.52, 0.5, 0.45)),
                ("vignette_amount", fl(2.0)),
                ("vignette_midpoint", fl(50.0)),
                ("vignette_roundness", fl(20.0)),
                ("vignette_feather", fl(50.0)),
            ],
            false,
        ),
        (
            "lumetri",
            vec![("exposure", fl(4.0)), ("contrast", fl(100.0)), ("temperature", fl(100.0)), ("tint", fl(100.0)), ("whites", fl(100.0)), ("blacks", fl(100.0))],
            false,
        ),
        (
            "lumetri",
            vec![
                ("exposure", fl(-4.0)),
                ("contrast", fl(-100.0)),
                ("temperature", fl(-100.0)),
                ("tint", fl(-100.0)),
                ("whites", fl(-100.0)),
                ("blacks", fl(-100.0)),
            ],
            false,
        ),
    ]
}

/// Every effect case on the GPU against its CPU reference, on the working image read back as f32.
#[test]
fn gpu_effects_match_cpu_exactly() {
    effects_match_cpu(None, EXACT);
}

/// The same through the half-float working textures of integrated GPUs (fork U0.2.3b): every
/// pass rounds to 11 significant bits, so the bound is [`HALF_EXACT`] (about two 10-bit code
/// values at 1.0) instead of [`EXACT`].
#[test]
fn half_float_effects_match_cpu() {
    effects_match_cpu(Some(fx::FX_FORMAT_HALF), HALF_EXACT);
}

fn effects_match_cpu(format: Option<wgpu::TextureFormat>, exact: f32) {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = match format {
        Some(f) => GpuCompositor::with_fx_format(&dev, &q, f),
        None => full_precision(&dev, &q),
    };
    let (w, h) = (67u32, 41u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut failures = Vec::new();
    for (id, params, steps) in cases() {
        let e = effect(id, &params);
        // (a chained effect — Lumetri — is several ops: its colour chain, HSL Secondary, Sharpen)
        let ops = FxOp::eval_all(&e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap_or_else(|| panic!("{id} has no GPU op"));
        assert!(ops.iter().all(FxOp::gpu_ok), "{id} {params:?}: not GPU-capable");
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        for op in &ops {
            op.apply(&mut cpu);
        }
        let fx = LayerFx { size: (w, h), decimation: 1, ops };
        let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("{id} {params:?}: max rel diff {worst:.2e}, {:.3}% flipped", flips * 100.0);
        // half floats: a 40× sharpen amplifies the blur's rounding 40×, and Unsharp's threshold
        // is a step function on that difference; both get the step functions' allowance
        let amplified = format.is_some()
            && (matches!(params.first(), Some((_, ParamValue::Float(a)))if id == "sharpen" && *a > 1000.0)
                || (id == "unsharp_mask" && params.iter().any(|(k, _)| *k == "threshold")));
        let (exact, steps) = if amplified { (0.02, true) } else { (exact, steps) };
        if worst > exact || flips > if steps { FLIPS } else { 0.0 } {
            failures.push(format!("{id} {params:?}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "GPU effects differ from the CPU:\n{}", failures.join("\n"));
}

/// Chains run in order (and Unsharp keeps its original while blurring, between other blurs).
#[test]
fn gpu_effect_chains_match_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (96u32, 54u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let chains: Vec<Vec<EffectInstance>> = vec![
        vec![
            effect("brightness_contrast", &[("brightness", fl(20.0)), ("contrast", fl(30.0))]),
            effect("gaussian_blur", &[("blurriness", fl(8.0))]),
            effect("tint", &[("amount", fl(50.0))]),
        ],
        vec![
            effect("gaussian_blur", &[("blurriness", fl(90.0)), ("repeat_edge", ParamValue::Bool(true))]),
            effect("unsharp_mask", &[("amount", fl(200.0)), ("radius", fl(4.0))]),
            effect("gaussian_blur", &[("blurriness", fl(3.0)), ("dimensions", ParamValue::Choice(1))]),
            effect("sharpen", &[("amount", fl(50.0))]),
        ],
        vec![
            effect("crop", &[("left", fl(10.0)), ("bottom", fl(20.0)), ("feather", fl(4.0))]),
            effect("transform", &[("rotation", fl(-15.0)), ("scale_height", fl(110.0))]),
            effect("horizontal_flip", &[]),
            effect("levels", &[("in_black", fl(15.0)), ("gamma", fl(80.0))]),
            effect("offset", &[("shift", pt(70.0, 10.0))]),
            effect("invert", &[("channel", ParamValue::Choice(4))]),
            effect("directional_blur", &[("length", fl(6.0)), ("direction", fl(90.0))]),
        ],
    ];
    for chain in chains {
        let ops: Vec<FxOp> = chain.iter().filter_map(|e| FxOp::eval(e, &cx(Tick::ZERO, 1.0), w as usize, h as usize)).collect();
        assert!(ops.iter().all(FxOp::gpu_ok));
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        for op in &ops {
            op.apply(&mut cpu);
        }
        let names: Vec<&str> = chain.iter().map(|e| e.effect.as_str()).collect();
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops }).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("{names:?}: max rel diff {worst:.2e}, {:.3}% flipped", flips * 100.0);
        assert!(worst < EXACT * 2.0 && flips == 0.0, "{names:?}: {worst}, {flips}");
    }
}

/// Keyframed parameters are evaluated at the layer's time on the CPU: the GPU result follows them.
#[test]
fn keyframed_effects_follow_time() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (48u32, 32u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut blur = effect("gaussian_blur", &[]);
    let mut bc = effect("brightness_contrast", &[]);
    let p = blur.params.get_mut("blurriness").expect("blurriness");
    p.put_keyframe(Tick::ZERO, fl(0.0));
    p.put_keyframe(Tick::from_seconds_f64(1.0), fl(30.0));
    let p = bc.params.get_mut("brightness").expect("brightness");
    p.put_keyframe(Tick::ZERO, fl(-50.0));
    p.put_keyframe(Tick::from_seconds_f64(1.0), fl(60.0));
    let mut last: Option<Vec<f32>> = None;
    for s in [0.0, 0.3, 0.75, 1.0] {
        let cx = cx(Tick::from_seconds_f64(s), 1.0);
        let ops: Vec<FxOp> = [&bc, &blur].iter().filter_map(|e| FxOp::eval(e, &cx, w as usize, h as usize)).collect();
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        for op in &ops {
            op.apply(&mut cpu);
        }
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops }).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        assert!(worst < EXACT && flips == 0.0, "t = {s}: {worst}, {flips}");
        assert_ne!(last.as_ref(), Some(&gpu), "t = {s}: the keyframes change the picture");
        last = Some(gpu);
    }
}

/// Hostile values: non-finite parameters stay on the CPU (`gpu_ok` is false), huge radii and
/// sizes are capped as on the CPU and run in bounded time on the GPU.
#[test]
fn hostile_parameters_are_bounded() {
    let (w, h) = (40usize, 24usize);
    let nan = [
        effect("brightness_contrast", &[("brightness", fl(f64::NAN))]),
        effect("gaussian_blur", &[("blurriness", fl(f64::INFINITY))]),
        effect("directional_blur", &[("length", fl(f64::NAN))]),
        effect("transform", &[("rotation", fl(f64::NAN))]),
        effect("crop", &[("left", fl(f64::NAN))]),
        effect("offset", &[("shift", pt(f64::INFINITY, 0.0))]),
        effect("levels", &[("in_black", fl(f64::NEG_INFINITY))]),
        effect("transform", &[("scale_height", fl(1e300))]),
        // minifying resamples (the CPU's mip path) stay on the CPU
        effect("transform", &[("scale_height", fl(20.0))]),
        effect("crop", &[("left", fl(-300.0)), ("zoom", ParamValue::Bool(true))]),
        effect("vignette", &[("amount", fl(f64::NAN))]),
        effect("vignette", &[("midpoint", fl(f64::INFINITY))]),
        effect("vignette", &[("roundness", fl(f64::NAN))]),
        effect("vignette", &[("feather", fl(f64::NAN))]),
        effect("video_limiter", &[("clip_level", fl(f64::NAN))]),
        effect("video_limiter", &[("compression", fl(f64::INFINITY))]),
        effect("video_limiter", &[("axis", fl(f64::NAN))]),
        effect("video_limiter", &[("warning_color", ParamValue::Color([f32::NAN, 0.0, 0.0, 1.0]))]),
    ];
    // Lumetri is a colour chain (fork ADR 0003): a non-finite slider either leaves its operation
    // out (the CPU and the GPU agree) or makes a non-finite op the GPU refuses; the CPU copes
    for e in [
        effect("lumetri", &[("exposure", fl(f64::NAN))]),
        effect("lumetri", &[("temperature", fl(f64::INFINITY))]),
        effect("lumetri", &[("contrast", fl(f64::NAN))]),
        effect("lumetri", &[("vignette_amount", fl(f64::NAN))]),
    ] {
        let ops = FxOp::eval_all(&e, &cx(Tick::ZERO, 1.0), w, h).expect("lumetri has a GPU form");
        let mut img = filmcraft_render::Image::filled(w, h, [0.3, 0.2, 0.1, 1.0]);
        for op in &ops {
            op.apply(&mut img);
        }
        // whatever the GPU would run is finite arithmetic on finite pixels
        let gpu_ops: Vec<&FxOp> = ops.iter().filter(|o| o.gpu_ok()).collect();
        let mut finite = filmcraft_render::Image::filled(w, h, [0.3, 0.2, 0.1, 1.0]);
        for op in gpu_ops {
            op.apply(&mut finite);
        }
        assert!(finite.px.iter().all(|v| v.is_finite()), "{}: a GPU-capable Lumetri op produced non-finite pixels", e.effect);
    }
    for e in &nan {
        let op = FxOp::eval(e, &cx(Tick::ZERO, 1.0), w, h);
        // gaussian with an infinite radius is capped (no blur), which the GPU runs
        if e.effect == "gaussian_blur" {
            assert!(op.as_ref().is_some_and(FxOp::gpu_ok));
            continue;
        }
        assert!(op.as_ref().is_some_and(|o| !o.gpu_ok()), "{}: {op:?}", e.effect);
        // and the CPU reference copes
        let mut img = filmcraft_render::Image::filled(w, h, [0.3, 0.2, 0.1, 1.0]);
        if let Some(op) = op {
            op.apply(&mut img);
        }
    }
    // huge radii: box radii capped by the image size
    let op = FxOp::eval(&effect("gaussian_blur", &[("blurriness", fl(1e30))]), &cx(Tick::ZERO, 1.0), w, h);
    let Some(FxOp::Gaussian { rx, ry, .. }) = op else { panic!("{op:?}") };
    assert!(rx.iter().chain(&ry).all(|r| *r as usize <= 4 * w.max(h).max(8)), "{rx:?} {ry:?}");
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let frame = VideoFrame::rgba_f32(w as u32, h as u32, picture(w as u32, h as u32));
    let ops = vec![
        FxOp::eval(&effect("gaussian_blur", &[("blurriness", fl(1e30))]), &cx(Tick::ZERO, 1.0), w, h).expect("op"),
        FxOp::eval(&effect("unsharp_mask", &[("radius", fl(1e30)), ("amount", fl(1e30))]), &cx(Tick::ZERO, 1.0), w, h).expect("op"),
        FxOp::eval(&effect("directional_blur", &[("length", fl(1e30))]), &cx(Tick::ZERO, 1.0), w, h).expect("op"),
    ];
    let t0 = std::time::Instant::now();
    let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w as u32, h as u32), decimation: 1, ops }).expect("effect image");
    assert!(t0.elapsed().as_secs() < 30);
    assert_eq!(gpu.len(), w * h * 4);
    // a working image larger than any texture is refused, not attempted
    assert!(c.effect_image(&frame, &LayerFx { size: (1 << 20, 4), decimation: 1, ops: vec![FxOp::BlackWhite] }).is_none());
}

/// Every Lumetri section has a GPU form in the fork (ADR 0003: one colour chain, HSL Secondary as
/// a keyed chain, Sharpen as an unsharp mask), in SDR and HDR working spaces alike. (Upstream's
/// single-op Lumetri fell back to the CPU for these; the chain does not.)
#[test]
fn every_lumetri_section_has_a_gpu_form() {
    let (w, h) = (40usize, 24usize);
    let cases: Vec<(&str, EffectInstance)> = vec![
        ("sharpen + input LUT", effect("lumetri", &[("sharpen", fl(10.0)), ("input_lut", ParamValue::Text("custom.cube".into()))])),
        ("sharpen", effect("lumetri", &[("sharpen", fl(20.0))])),
        ("input LUT", effect("lumetri", &[("input_lut", ParamValue::Text("test.cube".into()))])),
        ("look LUT", effect("lumetri", &[("look_lut", ParamValue::Text("look.cube".into()))])),
        ("look", effect("lumetri", &[("look", ch(1))])),
        ("luma curve", effect("lumetri", &[("curve_luma", ParamValue::Curve(vec![[0.0, 0.1], [1.0, 0.9]]))])),
        ("hue vs sat", effect("lumetri", &[("hue_vs_sat", ParamValue::Curve(vec![[0.5, 0.5]]))])),
        ("wheel offset", effect("lumetri", &[("wheel_shadows", pt(0.5, 0.0))])),
        ("wheel lightness", effect("lumetri", &[("wheel_midtones_l", fl(20.0))])),
        ("HSL Secondary", effect("lumetri", &[("hsl_on", ParamValue::Bool(true))])),
    ];
    for (name, e) in &cases {
        let ops = FxOp::eval_all(e, &cx(Tick::ZERO, 1.0), w, h).unwrap_or_else(|| panic!("{name}: no GPU form"));
        assert!(ops.iter().all(FxOp::gpu_ok), "{name}: {ops:?}");
    }
    // an HDR working space grades the PQ signal: the same chain, with its HDR encode / decode
    let mut cx_hdr = cx(Tick::ZERO, 1.0);
    cx_hdr.working = filmcraft_color::WorkingSpace::Rec2100Pq;
    let ops = FxOp::eval_all(&effect("lumetri", &[("exposure", fl(0.5))]), &cx_hdr, w, h).expect("HDR Lumetri has a GPU form");
    assert!(ops.iter().all(FxOp::gpu_ok), "{ops:?}");
}

/// Layers with effect chains in a composited plan — YUV and RGBA sources, a decimated working
/// image, rotated / scaled placement, partial opacity and alpha, blend modes — match the CPU plan
/// executor.
#[test]
fn gpu_effect_layers_match_cpu_plan() {
    effect_layers_match_cpu_plan(None);
}

/// Export compositors keep full precision on every adapter, preview ones follow the device rule
/// (FORK-PLAN §1a.64).
#[test]
fn export_compositors_never_use_half_floats() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    assert!(!GpuCompositor::for_export(&dev, &q).effects_half_precision());
    let preview = GpuCompositor::new(&dev, &q);
    let want = fx::fx_format_for(dev.adapter_info().device_type) == fx::FX_FORMAT_HALF;
    assert_eq!(preview.effects_half_precision(), want);
}

/// The same plans through the half-float working textures integrated GPUs use (fork U0.2.3b), on
/// whatever adapter the tests get: the same criterion holds.
#[test]
fn half_float_effect_layers_match_cpu_plan() {
    effect_layers_match_cpu_plan(Some(fx::FX_FORMAT_HALF));
}

fn effect_layers_match_cpu_plan(format: Option<wgpu::TextureFormat>) {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = match format {
        Some(f) => GpuCompositor::with_fx_format(&dev, &q, f),
        None => full_precision(&dev, &q),
    };
    let (w, h) = (320usize, 180usize);
    let ops = |list: &[EffectInstance], lw: u32, lh: u32, px_scale: f32| -> Vec<FxOp> {
        list.iter().filter_map(|e| FxOp::eval(e, &cx(Tick::ZERO, px_scale), lw as usize, lh as usize)).collect()
    };
    let chain_a = [
        effect("brightness_contrast", &[("brightness", fl(15.0)), ("contrast", fl(25.0))]),
        effect("gaussian_blur", &[("blurriness", fl(6.0))]),
        effect("tint", &[("amount", fl(40.0))]),
    ];
    let chain_b = [
        effect("crop", &[("left", fl(8.0)), ("right", fl(12.0)), ("feather", fl(6.0))]),
        effect("unsharp_mask", &[("amount", fl(120.0)), ("radius", fl(2.0))]),
        effect("color_balance", &[("hi_b", fl(60.0)), ("shadow_r", fl(40.0))]),
    ];
    let chain_c = [effect("mirror", &[("angle", fl(90.0))]), effect("gaussian_blur", &[("blurriness", fl(50.0)), ("dimensions", ParamValue::Choice(1))])];
    let ramp = ramp_layer(160, 120, 0);
    for mode in [Blend::Normal, Blend::Screen, Blend::Multiply, Blend::Overlay] {
        let plan = FramePlan::Layers {
            output: None,
            width: w,
            height: h,
            layers: vec![
                // YUV source decoded at half size (decimation 2) with effects, filling the output
                PlanLayer {
                    frame: yuv_frame(640, 360),
                    matrix: Affine::IDENTITY,
                    opacity: 1.0,
                    blend: Blend::Normal,
                    fx: Some(Arc::new(LayerFx { size: (320, 180), decimation: 2, ops: ops(&chain_c, 320, 180, 0.5) })),
                    comp: None,
                },
                PlanLayer {
                    frame: ramp.clone(),
                    matrix: Affine::motion(Vec2::new(170.0, 95.0), Vec2::new(1.1, 1.1), 17.0, Vec2::new(80.0, 60.0)),
                    opacity: 0.8,
                    blend: mode,
                    fx: Some(Arc::new(LayerFx { size: (160, 120), decimation: 1, ops: ops(&chain_a, 160, 120, 1.0) })),
                    comp: None,
                },
                PlanLayer::new(ramp.clone(), Affine::translate(10.0, 20.0), 0.5, Blend::Normal),
                PlanLayer {
                    frame: yuv_frame(320, 180),
                    matrix: Affine::motion(Vec2::new(250.0, 70.0), Vec2::new(0.45, 0.45), -9.0, Vec2::new(160.0, 90.0)),
                    opacity: 0.7,
                    blend: mode,
                    fx: Some(Arc::new(LayerFx { size: (320, 180), decimation: 1, ops: ops(&chain_b, 320, 180, 1.0) })),
                    comp: None,
                },
            ],
        };
        let cpu = execute_cpu(&plan).over_black_rgba8();
        c.composite(&plan);
        let (_, _, gpu) = c.read_output().expect("readback");
        let keep = interior(&plan);
        let (p99, mean, max) = stats8(&cpu, &gpu, &keep);
        let (ap99, amean, _) = stats8(&cpu, &gpu, &vec![true; keep.len()]);
        eprintln!("{mode:?}: interior 8-bit p99 {p99} mean {mean:.3} max {max}; with edges p99 {ap99} mean {amean:.3}");
        assert!(p99 <= 6 && mean < 1.5, "{mode:?}: p99 {p99}, mean {mean}");
    }
}

/// Plans without effects never create working textures.
#[test]
fn plain_layers_skip_the_effect_stage() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let plan = FramePlan::Layers { output: None, width: 64, height: 36, layers: vec![PlanLayer::new(yuv_frame(64, 36), Affine::IDENTITY, 1.0, Blend::Normal)] };
    c.composite(&plan);
    assert!(c.fx.as_ref().is_some_and(fx::FxStage::is_idle));
    let fx = LayerFx { size: (64, 36), decimation: 1, ops: vec![FxOp::BlackWhite] };
    let with = FramePlan::Layers {
        output: None,
        width: 64,
        height: 36,
        layers: vec![PlanLayer { fx: Some(Arc::new(fx)), ..PlanLayer::new(yuv_frame(64, 36), Affine::IDENTITY, 1.0, Blend::Normal) }],
    };
    c.composite(&with);
    assert!(!c.fx.as_ref().is_some_and(fx::FxStage::is_idle));
    // and they are released once no layer needs them
    c.composite(&plan);
    assert!(c.fx.as_ref().is_some_and(fx::FxStage::is_idle));
}

/// Without an effect stage (devices without compute shaders) the compositor renders a layer's
/// effects on the CPU itself: the picture is the same.
#[test]
fn layers_with_effects_fall_back_to_the_cpu_without_a_stage() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let ops: Vec<FxOp> = [effect("tint", &[("amount", fl(70.0))]), effect("gaussian_blur", &[("blurriness", fl(10.0))])]
        .iter()
        .filter_map(|e| FxOp::eval(e, &cx(Tick::ZERO, 1.0), 160, 120))
        .collect();
    let plan = FramePlan::Layers {
        output: None,
        width: 200,
        height: 150,
        layers: vec![
            PlanLayer::new(yuv_frame(200, 150), Affine::IDENTITY, 1.0, Blend::Normal),
            PlanLayer {
                fx: Some(Arc::new(LayerFx { size: (160, 120), decimation: 1, ops })),
                ..PlanLayer::new(
                    ramp_layer(160, 120, 2),
                    Affine::motion(Vec2::new(100.0, 75.0), Vec2::new(0.9, 0.9), 8.0, Vec2::new(80.0, 60.0)),
                    0.8,
                    Blend::Screen,
                )
            },
        ],
    };
    let cpu = execute_cpu(&plan).over_black_rgba8();
    for stage in [true, false] {
        let mut c = full_precision(&dev, &q);
        if !stage {
            c.fx = None;
        }
        c.composite(&plan);
        let (_, _, gpu) = c.read_output().expect("readback");
        let (p99, mean, _) = stats8(&cpu, &gpu, &interior(&plan));
        eprintln!("effect stage {stage}: interior p99 {p99}, mean {mean:.3}");
        assert!(p99 <= 6 && mean < 1.5, "stage {stage}: p99 {p99}, mean {mean}");
    }
}

/// A working image smaller than the frame (fork Phase 5.3: converted media graded at the size
/// drawn): the GPU resamples the frame like the CPU reference before the effects run.
#[test]
fn working_image_smaller_than_the_frame_matches_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (160u32, 96u32);
    // smooth content: the GPU supersamples where the CPU filters bilinearly, which differ only
    // on detail finer than the working image
    let px: Vec<f32> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            [0.1 + 0.8 * x, 0.2 + 0.6 * y, 0.5 * (x + y), 1.0]
        })
        .collect();
    let frame = VideoFrame::rgba_f32(w, h, px);
    for size in [(99u32, 59u32), (80, 48), (157, 95)] {
        let ops: Vec<FxOp> = [effect("brightness_contrast", &[("brightness", fl(20.0))])]
            .iter()
            .filter_map(|e| FxOp::eval(e, &cx(Tick::ZERO, size.0 as f32 / w as f32), size.0 as usize, size.1 as usize))
            .collect();
        let fx = LayerFx { size, decimation: 1, ops };
        let cpu = filmcraft_render::plan::effect_image(&frame, &fx);
        assert_eq!((cpu.w, cpu.h), (size.0 as usize, size.1 as usize));
        let (gw, gh, gpu) = c.effect_image(&frame, &fx).expect("effect image");
        assert_eq!((gw, gh), size);
        let mean = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).sum::<f32>() / gpu.len() as f32;
        eprintln!("{size:?}: mean abs diff {mean:.2e}");
        assert!(mean < 2e-3, "{size:?}: mean abs diff {mean}");
    }
}

/// The half-float upload is lossless for this input, so a 1:1 effect source draw must be too.
/// Odd dimensions expose interpolation drift that can invent opacity before any effect runs.
#[test]
fn effect_source_identity_preserves_odd_sized_half_float_texels() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    for (w, h) in [(67u32, 41u32), (96, 54), (64, 32)] {
        let px = picture(w, h);
        let frame = VideoFrame::rgba_f32(w, h, px.clone());
        for n in [0, 1] {
            let (ow, oh, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: n, ops: Vec::new() }).expect("source image");
            assert_eq!((ow, oh, gpu.len()), (w, h, px.len()));
            for (i, (expected, actual)) in px.iter().zip(&gpu).enumerate() {
                assert_eq!(actual.to_bits(), expected.to_bits(), "{w}x{h} n{n} source component{i}: {actual} != {expected}");
            }
        }
    }
}

#[test]
fn alpha_invert_does_not_amplify_invented_opacity_at_odd_source_pixels() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (67, 41);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let op = FxOp::Invert { channel: 4, blend: 0.2 };
    let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
    op.apply(&mut cpu);
    let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).expect("alpha invert image");
    assert_eq!(gpu.len(), px.len());
    let (worst, flips) = compare(&cpu.px, &gpu);
    assert!(worst <= EXACT && flips == 0.0, "alpha invert: {worst}, {flips}");
    let mut transparent = 0;
    for (source, result) in px.as_chunks::<4>().0.iter().zip(gpu.as_chunks::<4>().0.iter()) {
        if source[3] == 0.0 {
            transparent += 1;
            assert_eq!(&result[..3], &[0.0; 3], "a transparent source must not acquire neighboring RGB");
            assert!((result[3] - 0.8).abs() <= f32::EPSILON);
        }
    }
    assert_eq!(transparent, 402);
}

/// The exact source grid keeps the existing box-minification behavior at non-unit factors.
#[test]
fn effect_source_integer_decimation_preserves_cpu_box_oracle() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    for (w, h) in [(67u32, 41u32), (96, 54)] {
        let frame = VideoFrame::rgba_f32(w, h, picture(w, h));
        for n in [2, 4, 8] {
            let (ow, oh, cpu) = frame.to_linear_f32_decimated(n as usize);
            let (gw, gh, gpu) =
                c.effect_image(&frame, &LayerFx { size: (ow as u32, oh as u32), decimation: n, ops: Vec::new() }).expect("decimated source image");
            assert_eq!((gw as usize, gh as usize, gpu.len()), (ow, oh, cpu.len()));
            let (worst, flips) = compare(&cpu, &gpu);
            assert!(worst <= EXACT && flips == 0.0, "{w}x{h} n{n} source box: {worst}, {flips}");
        }
    }
}

/// Noise reduction (`FxOp::Denoise`, G2.4a) on the GPU against `denoise::apply`: a noisy test
/// picture (alpha bands, super-whites, transparent rows) with an inf, a NaN and a NaN-alpha pixel,
/// luma / chroma / both, spatial sigmas 0.25…3.5 (radius 1…7) and hostile values. The CPU runs
/// on the GPU's own working image (the source draw decides what a non-finite texel becomes);
/// non-finite values must be the same on both, finite ones within 1e-3.
#[test]
fn gpu_denoise_matches_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    if let Ok(a) = pollster::block_on(wgpu::Instance::default().request_adapter(&wgpu::RequestAdapterOptions::default())) {
        eprintln!("adapter: {}", a.get_info().name);
    }
    let mut c = full_precision(&dev, &q);
    let (w, h) = (67u32, 41u32);
    // the test picture plus deterministic, half-float-representable noise
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut px: Vec<f32> = picture(w, h)
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let mut n = [0.0f32; 3];
            for v in &mut n {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *v = ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.04 * p[3];
            }
            [half((p[0] + n[0]).max(0.0)), half((p[1] + n[1]).max(0.0)), half((p[2] + n[2]).max(0.0)), p[3]]
        })
        .collect();
    // opaque rows (y % 7 ≥ 4): an inf, a NaN and a NaN alpha
    for (x, y, k, v) in [(20usize, 5usize, 0usize, f32::INFINITY), (40, 12, 1, f32::NAN), (10, 19, 3, f32::NAN)] {
        if let Some(t) = px.get_mut((y * w as usize + x) * 4 + k) {
            *t = v;
        }
    }
    let frame = VideoFrame::rgba_f32(w, h, px);
    let (_, _, base) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![] }).expect("working image");
    let base_bad = base.iter().filter(|v| !v.is_finite()).count();
    eprintln!("non-finite values in the working image: {base_bad}");
    let mut worst_all = 0f32;
    let cases = [
        (0.5, 0.5, 1.5),
        (1.0, 0.0, 0.5),
        (0.0, 1.0, 2.5),
        (0.3, 0.8, 3.5),
        (1.0, 1.0, 3.5),
        (0.05, 0.02, 1.0),
        (0.0, 0.0, 1.5),
        (f32::NAN, 0.7, f32::NAN),
        (f32::INFINITY, -1.0, 1e30),
    ];
    for (luma, chroma, sigma) in cases {
        let op = FxOp::Denoise { luma, chroma, sigma };
        assert!(op.gpu_ok());
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: base.clone() };
        op.apply(&mut cpu);
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).expect("effect image");
        assert_eq!(cpu.px.len(), gpu.len());
        let mut worst = 0f32;
        let mut mismatched = 0usize;
        for (a, b) in cpu.px.iter().zip(&gpu) {
            if !a.is_finite() || !b.is_finite() {
                if !(a.is_nan() && b.is_nan() || a == b) {
                    mismatched += 1;
                }
            } else {
                worst = worst.max((a - b).abs() / (1.0 + a.abs()));
            }
        }
        let bad = gpu.iter().filter(|v| !v.is_finite()).count();
        eprintln!("denoise {luma} {chroma} σ{sigma}: max rel diff {worst:.2e}, {bad} non-finite (source {base_bad}), {mismatched} mismatched");
        assert!(worst < 1e-3 && mismatched == 0 && bad <= base_bad, "denoise {luma} {chroma} σ{sigma}: {worst:.2e}, {mismatched}, {bad}");
        worst_all = worst_all.max(worst);
    }
    eprintln!("denoise GPU parity: max rel diff {worst_all:.2e}");
}

/// GPU cost of Denoise on a 1920×1080 working image (upload and read-back subtracted by timing
/// the same image without the op). `cargo test -p filmcraft-gpu --lib gpu_denoise_timing --
/// --ignored --nocapture`.
#[test]
#[ignore = "timing, run by hand"]
fn gpu_denoise_timing() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (1920u32, 1080u32);
    let px: Vec<f32> = (0..w * h).flat_map(|i| [(i % 251) as f32 / 251.0, (i % 241) as f32 / 241.0, (i % 239) as f32 / 239.0, 1.0]).collect();
    let frame = VideoFrame::rgba_f32(w, h, px);
    let mut time = |ops: Vec<FxOp>| {
        let fx = LayerFx { size: (w, h), decimation: 1, ops };
        let _ = c.effect_image(&frame, &fx);
        let t = std::time::Instant::now();
        for _ in 0..20 {
            let _ = c.effect_image(&frame, &fx);
        }
        t.elapsed().as_secs_f64() * 1000.0 / 20.0
    };
    let base = time(vec![]);
    for (l, ch, s) in [(1.0, 1.0, 3.5), (1.0, 0.0, 3.5), (0.5, 0.5, 1.5)] {
        let ms = time(vec![FxOp::Denoise { luma: l, chroma: ch, sigma: s }]);
        eprintln!("denoise {l} {ch} σ{s} 1080p: {:.2} ms (frame with read-back {ms:.2} ms, without the op {base:.2} ms)", ms - base);
    }
}

/// Temporal noise reduction (`FxOp::TemporalDenoise`, fork 6.7) on the GPU against the CPU
/// reference (`gpufx::apply_ops`): the design's synthetic sequence (half-float frames, so the
/// source draws are lossless), flows estimated once on the CPU and uploaded as they are, radius
/// 1 and 2, luma / chroma / both, an op before it (replayed on the neighbours) and one after,
/// hostile params and flows, identity cases. Within 1e-3 relative; a threshold flip (a sample
/// landing exactly on the frame edge) may change at most 1e-4 of the values.
#[test]
fn temporal_denoise() {
    use filmcraft_render::flow::{FlowField, chain, estimate_pair};
    use filmcraft_render::gpufx::{NeighbourPicture, TemporalInputs, TemporalNeighbour, apply_ops};
    use filmcraft_render::tnr::fixture::{FRAMES, H, MID, NOISE, W, frames};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    if let Ok(a) = pollster::block_on(wgpu::Instance::default().request_adapter(&wgpu::RequestAdapterOptions::default())) {
        eprintln!("adapter: {}", a.get_info().name);
    }
    let mut c = full_precision(&dev, &q);
    let imgs: Vec<filmcraft_render::Image> = frames(NOISE, 0x7e57)
        .into_iter()
        .map(|mut im| {
            im.px.iter_mut().for_each(|v| *v = half(*v));
            im
        })
        .collect();
    assert_eq!(imgs.len(), FRAMES);
    let vf: Vec<Arc<VideoFrame>> = imgs.iter().map(|im| Arc::new(VideoFrame::rgba_f32(W as u32, H as u32, im.px.clone()))).collect();
    let pair = |a: usize, b: usize| estimate_pair(&imgs[a], &imgs[b]).expect("flow");
    let (m1, m2, p1, p2) = (pair(MID, MID - 1), pair(MID - 1, MID - 2), pair(MID, MID + 1), pair(MID + 1, MID + 2));
    let fl: Vec<(i8, FlowField, FlowField)> = vec![
        (-1, m1.0.clone(), m1.1.clone()),
        (1, p1.0.clone(), p1.1.clone()),
        (-2, chain(&m1.0, &m2.0).expect("chain"), chain(&m2.1, &m1.1).expect("chain")),
        (2, chain(&p1.0, &p2.0).expect("chain"), chain(&p2.1, &p1.1).expect("chain")),
    ];
    let inputs = |fl: &[(i8, FlowField, FlowField)]| {
        Arc::new(TemporalInputs {
            source_size: (W as u32, H as u32),
            neighbours: fl
                .iter()
                .map(|(o, f, b)| TemporalNeighbour {
                    offset: *o,
                    picture: NeighbourPicture::Frame { frame: vf[(MID as i64 + *o as i64) as usize].clone(), decimation: 1 },
                    flow: Arc::new(f.clone()),
                    back: Arc::new(b.clone()),
                })
                .collect(),
        })
    };
    let full = inputs(&fl);
    // hostile flows: NaN / infinite vectors on every third grid pixel of the ±1 pair
    let mut nan = fl[..2].to_vec();
    for (_, f, b) in &mut nan {
        for (i, v) in f.uv.iter_mut().chain(b.uv.iter_mut()).enumerate() {
            if i % 3 == 0 {
                *v = [f32::NAN, f32::NEG_INFINITY];
            }
        }
    }
    let nan = inputs(&nan);
    let tnr = |luma: f32, chroma: f32, radius: u8, motion: f32, inp: &Arc<TemporalInputs>| FxOp::TemporalDenoise {
        luma,
        chroma,
        radius,
        motion,
        inputs: Some(inp.clone()),
    };
    let cases: Vec<(&str, Vec<FxOp>)> = vec![
        ("r1 luma+chroma", vec![tnr(1.0, 1.0, 1, 0.5, &full)]),
        ("r2 luma+chroma", vec![tnr(1.0, 1.0, 2, 0.5, &full)]),
        ("r2 luma only, strict motion", vec![tnr(0.6, 0.0, 2, 0.0, &full)]),
        ("r2 chroma only, loose motion", vec![tnr(0.0, 0.8, 2, 1.0, &full)]),
        ("r9 hostile params", vec![tnr(f32::INFINITY, f32::NAN, 9, f32::NAN, &full)]),
        ("NaN flows", vec![tnr(1.0, 1.0, 1, 0.5, &nan)]),
        (
            "op before (replayed) and after",
            vec![FxOp::BrightnessContrast { br: 0.04, co: 1.1 }, tnr(1.0, 0.7, 2, 0.5, &full), FxOp::Denoise { luma: 0.5, chroma: 0.5, sigma: 1.0 }],
        ),
        ("no inputs", vec![FxOp::TemporalDenoise { luma: 1.0, chroma: 1.0, radius: 2, motion: 0.5, inputs: None }]),
        ("empty inputs", vec![tnr(1.0, 1.0, 2, 0.5, &Arc::new(TemporalInputs { source_size: (W as u32, H as u32), neighbours: vec![] }))]),
        ("zero strength", vec![tnr(0.0, 0.0, 2, 0.5, &full)]),
        // two temporal ops in one layer (they share the spare textures; the second replays the
        // op between them on its neighbours)
        ("two temporal ops", vec![tnr(1.0, 0.0, 1, 0.5, &full), FxOp::BrightnessContrast { br: -0.03, co: 1.05 }, tnr(0.0, 1.0, 2, 0.8, &full)]),
    ];
    let mut worst_all = 0f32;
    let mut check = |name: &str, gpu_ops: Vec<FxOp>, cpu_ops: &[FxOp], c: &mut GpuCompositor| {
        let mut cpu = imgs[MID].clone();
        apply_ops(cpu_ops, &mut cpu);
        let (_, _, gpu) = c.effect_image(&vf[MID], &LayerFx { size: (W as u32, H as u32), decimation: 1, ops: gpu_ops }).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("temporal_denoise {name}: max rel diff {worst:.2e}, flips {flips:.2e}");
        assert!(worst < 1e-3 && flips <= 1e-4, "{name}: {worst:.2e} {flips:.2e}");
        worst_all = worst_all.max(worst);
    };
    for (name, ops) in cases {
        assert!(ops.iter().all(FxOp::gpu_ok), "{name}");
        check(name, ops.clone(), &ops, &mut c);
    }
    // ready images among the neighbours: the GPU has no bind group for them (None) and skips
    // them, shifting the slots of the source frames after them; the result is the CPU's with the
    // source frames alone
    let mixed = Arc::new(TemporalInputs {
        source_size: (W as u32, H as u32),
        neighbours: fl
            .iter()
            .map(|(o, f, b)| {
                let i = (MID as i64 + *o as i64) as usize;
                let picture = if o.abs() == 1 {
                    NeighbourPicture::Image(Arc::new(imgs[i].clone()))
                } else {
                    NeighbourPicture::Frame { frame: vf[i].clone(), decimation: 1 }
                };
                TemporalNeighbour { offset: *o, picture, flow: Arc::new(f.clone()), back: Arc::new(b.clone()) }
            })
            .collect(),
    });
    assert!(!tnr(1.0, 1.0, 2, 0.5, &mixed).gpu_ok());
    check("ready images skipped (slot shift)", vec![tnr(1.0, 1.0, 2, 0.5, &mixed)], &[tnr(1.0, 1.0, 2, 0.5, &inputs(&fl[2..]))], &mut c);
    // the working-texture budget: ±2 needs 8 textures, ±1 6, none 2
    let px = u64::from(W as u32) * u64::from(H as u32) * 16;
    c.set_fx_vram_budget(7 * px);
    check("over budget → ±1", vec![tnr(1.0, 1.0, 2, 0.5, &full)], &[tnr(1.0, 1.0, 1, 0.5, &full)], &mut c);
    c.set_fx_vram_budget(1);
    check("over budget → skipped", vec![tnr(1.0, 1.0, 2, 0.5, &full)], &[], &mut c);
    check(
        "over budget, other ops still run",
        vec![FxOp::Denoise { luma: 0.5, chroma: 0.5, sigma: 1.0 }, tnr(1.0, 1.0, 2, 0.5, &full)],
        &[FxOp::Denoise { luma: 0.5, chroma: 0.5, sigma: 1.0 }],
        &mut c,
    );
    c.set_fx_vram_budget(crate::fx::FX_VRAM_BUDGET);
    check("budget restored", vec![tnr(1.0, 1.0, 2, 0.5, &full)], &[tnr(1.0, 1.0, 2, 0.5, &full)], &mut c);
    eprintln!("temporal_denoise GPU parity: max rel diff {worst_all:.2e}");
}

/// Regression: reading back a 6.2K working image (6224×4152 Rgba32Float, 413 MB) asked for one
/// buffer above the device's largest (256 MiB by default) and panicked in wgpu validation; it is
/// now read in row bands.
#[test]
fn effect_image_reads_back_a_6k_working_image() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (6224u32, 4152u32);
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % w % 256) as u8, (i / w % 256) as u8, 128, 255]).collect();
    let frame = VideoFrame::rgba8(w, h, px);
    let (rw, rh, out) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![] }).expect("6.2K working image");
    assert_eq!((rw, rh, out.len()), (w, h, (w * h * 4) as usize));
    // a pixel in the last band: x = 200, y = 4100 → sRGB codes (200, 4100 % 256, 128), opaque
    let i = ((4100 * w + 200) * 4) as usize;
    let want = [200.0f32, (4100 % 256) as f32, 128.0].map(|v| filmcraft_color::srgb_to_linear(v / 255.0));
    for k in 0..3 {
        assert!((out[i + k] - want[k]).abs() < 2e-3, "channel {k}: {} vs {}", out[i + k], want[k]);
    }
    assert!((out[i + 3] - 1.0).abs() < 1e-6);
}

/// GPU cost of TemporalDenoise at 6.2K (6224×4152, 8-bit synthetic frames, uniform motion,
/// radius 1 and 2): the effect stage with the op minus without it (source draw and read-back
/// cancel), i.e. neighbour draws + YCC conversions + the blend. One measurement after a warm-up.
/// `cargo test -p filmcraft-gpu --lib temporal_denoise_timing_6k -- --ignored --nocapture`.
#[test]
#[ignore = "timing, run by hand"]
fn temporal_denoise_timing_6k() {
    use filmcraft_render::flow::FlowField;
    use filmcraft_render::gpufx::{NeighbourPicture, TemporalInputs, TemporalNeighbour};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    if let Ok(a) = pollster::block_on(wgpu::Instance::default().request_adapter(&wgpu::RequestAdapterOptions::default())) {
        eprintln!("adapter: {}", a.get_info().name);
    }
    let mut c = full_precision(&dev, &q);
    let (w, h) = (6224u32, 4152u32);
    let frame = |k: i32| {
        let px: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = ((i % w) as i32 - 3 * k, (i / w) as i32 + 2 * k);
                let v = (96 + (x * 7 + y * 3).rem_euclid(61) + (x / 9 + y / 7).rem_euclid(5) * 9) as u8;
                [v, v.saturating_add(4), v.saturating_sub(6), 255]
            })
            .collect();
        Arc::new(VideoFrame::rgba8(w, h, px))
    };
    let cur = frame(0);
    let s = w.div_ceil(640) as usize;
    let (gw, gh) = ((w as usize).div_ceil(s), (h as usize).div_ceil(s));
    let flow = |k: i32| {
        let mut f = FlowField::zero(gw, gh, s as f32);
        f.uv.iter_mut().for_each(|v| *v = [3.0 * k as f32 / s as f32, -2.0 * k as f32 / s as f32]);
        Arc::new(f)
    };
    let nbrs: Vec<TemporalNeighbour> = [-1i32, 1, -2, 2]
        .iter()
        .map(|&k| TemporalNeighbour { offset: k as i8, picture: NeighbourPicture::Frame { frame: frame(k), decimation: 1 }, flow: flow(k), back: flow(-k) })
        .collect();
    let inputs = Arc::new(TemporalInputs { source_size: (w, h), neighbours: nbrs });
    // (no read-back: a 6.2K f32 image exceeds the largest buffer `effect_image` may map)
    let mut time = |ops: Vec<FxOp>| {
        let fx = LayerFx { size: (w, h), decimation: 1, ops };
        let _ = stage_ms(&mut c, &cur, &fx);
        stage_ms(&mut c, &cur, &fx)
    };
    let base = time(vec![]).expect("plain stage");
    for r in [1u8, 2] {
        let ms = time(vec![FxOp::TemporalDenoise { luma: 1.0, chroma: 1.0, radius: r, motion: 0.5, inputs: Some(inputs.clone()) }]).expect("temporal stage");
        eprintln!("temporal_denoise radius {r} at 6224×4152 (synthetic): {:.1} ms GPU (stage {ms:.1} ms, without the op {base:.1} ms)", ms - base);
    }
}

/// Run the effect stage of `fx` on `frame` and wait for the GPU: milliseconds from submit to idle.
fn stage_ms(c: &mut GpuCompositor, frame: &VideoFrame, fx: &LayerFx) -> Option<f64> {
    let sbg = c.fx_source_bg(frame, fx)?;
    let nbrs = c.fx_neighbour_sources(fx);
    let stage = c.fx.as_mut()?;
    stage.begin_frame();
    let job = stage.job(&c.device, &c.queue, fx, sbg, &nbrs, &c.dummy)?;
    let mut enc = c.device.create_command_encoder(&Default::default());
    stage.record(&mut enc, &job);
    let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
    let t = std::time::Instant::now();
    c.queue.submit([enc.finish()]);
    let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    stage.end_frame();
    Some(ms)
}

/// The xm5 bench's grade (Lumetri: temperature, exposure, contrast, saturation, a luma curve, HSL
/// Secondary, vignette; then Sharpen) as ops on a `w`×`h` image.
fn bench_grade_ops(w: u32, h: u32) -> Vec<FxOp> {
    let lumetri = effect(
        "lumetri",
        &[
            ("temperature", fl(12.0)),
            ("exposure", fl(0.3)),
            ("contrast", fl(20.0)),
            ("saturation", fl(115.0)),
            ("curve_luma", ParamValue::Curve(vec![[0.0, 0.0], [0.25, 0.2], [0.75, 0.82], [1.0, 1.0]])),
            ("hsl_on", ParamValue::Bool(true)),
            ("hsl_hue", fl(30.0)),
            ("hsl_sat", fl(130.0)),
            ("vignette_amount", fl(-1.5)),
        ],
    );
    let sharpen = effect("sharpen", &[("amount", fl(40.0))]);
    let ctx = cx(Tick::ZERO, 1.0);
    let mut ops = FxOp::eval_all(&lumetri, &ctx, w as usize, h as usize).unwrap_or_default();
    ops.extend(FxOp::eval_all(&sharpen, &ctx, w as usize, h as usize).unwrap_or_default());
    ops
}

/// Half-float preview stays within one 10-bit code value of the f32 path (FORK-PLAN §1a.62 /
/// §1a.64), compared after the sRGB-style display encode, on a typical grade and on single
/// colour / blur ops.
#[test]
fn half_float_preview_is_within_one_10_bit_code_of_f32() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (160u32, 90u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px);
    let mut full = full_precision(&dev, &q);
    let mut half = GpuCompositor::with_fx_format(&dev, &q, fx::FX_FORMAT_HALF);
    // the grade's colour ops (Sharpen is checked below)
    let colour: Vec<FxOp> = bench_grade_ops(w, h).into_iter().filter(|o| !matches!(o, FxOp::Unsharp { .. })).collect();
    let lists = [
        colour,
        ops_of(&effect("gaussian_blur", &[("blurriness", fl(12.0))]), w, h),
        ops_of(&effect("brightness_contrast", &[("brightness", fl(15.0)), ("contrast", fl(25.0))]), w, h),
    ];
    let mut worst_of = |ops: Vec<FxOp>| {
        let fx = LayerFx { size: (w, h), decimation: 1, ops };
        let (_, _, a) = full.effect_image(&frame, &fx).expect("f32 image");
        let (_, _, b) = half.effect_image(&frame, &fx).expect("f16 image");
        assert_eq!(a.len(), b.len());
        let code = |v: f32| filmcraft_color::linear_to_srgb(v.clamp(0.0, 1.0)) * 1023.0;
        let worst = a.chunks(4).zip(b.chunks(4)).flat_map(|(x, y)| (0..3).map(move |i| (code(x[i]) - code(y[i])).abs())).fold(0f32, f32::max);
        eprintln!("{:?}…: worst {worst:.3} 10-bit codes", fx.ops.first().map(|o| format!("{o:?}").chars().take(30).collect::<String>()));
        worst
    };
    for ops in lists {
        let worst = worst_of(ops);
        assert!(worst < 1.0, "half-float preview {worst:.3} 10-bit codes from f32");
    }
    // Known gap (owner question 18): Sharpen / Unsharp round six box passes to half floats and
    // land up to ~4 codes away, which is why half floats are opt-in only (`FILMCRAFT_FX_FORMAT=f16`).
    // When this drops below one code, `HALF_ON_INTEGRATED` may be turned on.
    let sharpen = worst_of(bench_grade_ops(w, h).into_iter().filter(|o| matches!(o, FxOp::Unsharp { .. })).collect());
    assert!(sharpen < 8.0, "half-float Sharpen {sharpen:.3} codes (was ~4)");
}

fn ops_of(e: &EffectInstance, w: u32, h: u32) -> Vec<FxOp> {
    FxOp::eval_all(e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap_or_default()
}

/// Time per op of the xm5 bench's graded export (Lumetri + Sharpen) on a 6.2K working image, in
/// both working formats (fork U0.2.3b: the Intel lane renders graded exports at ~2 fps); GPU time
/// per op with the source draw subtracted. Run with
/// `--ignored --nocapture`; pick the GPU with `MESA_VK_DEVICE_SELECT`.
#[test]
#[ignore = "timing, not a check"]
fn graded_export_ops_timing_6k() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    eprintln!("adapter: {:?}", dev.adapter_info().name);
    let (w, h) = (6240u32, 4160u32);
    let px: Vec<f32> = (0..w * h).flat_map(|i| [(i % 251) as f32 / 251.0, (i % 241) as f32 / 241.0, (i % 239) as f32 / 239.0, 1.0]).collect();
    let frame = VideoFrame::rgba_f32(w, h, px);
    let ops = bench_grade_ops(w, h);
    for format in [fx::FX_FORMAT, fx::FX_FORMAT_HALF] {
        let mut c = GpuCompositor::with_fx_format(&dev, &q, format);
        // GPU time from submit to idle (no upload or read-back: those dominate at 6.2K and made
        // the per-op differences noise), median of 5 after a warm-up that uploads and compiles
        let mut time = |ops: Vec<FxOp>| {
            let fx = LayerFx { size: (w, h), decimation: 1, ops };
            let _ = c.effect_image(&frame, &fx);
            let mut v: Vec<f64> = (0..5).filter_map(|_| stage_ms(&mut c, &frame, &fx)).collect();
            v.sort_by(f64::total_cmp);
            v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
        };
        let base = time(vec![]);
        eprintln!("{format:?}: source draw {base:.1} ms");
        for op in &ops {
            let name = format!("{op:?}").chars().take(if std::env::var_os("FX_TIMING_FULL").is_some() { 4000 } else { 60 }).collect::<String>();
            eprintln!("{format:?}: {:.1} ms  {name}", time(vec![op.clone()]) - base);
        }
        eprintln!("{format:?}: all {:.1} ms", time(ops.clone()) - base);
        // the chain interpreter's fixed cost: one cheap op
        let one = FxOp::ColorChain(std::sync::Arc::from(vec![filmcraft_color::ops::ColorOp::Clamp { lo: 0.0, hi: 1.0 }]));
        eprintln!("{format:?}: one-op chain (Clamp) {:.1} ms", time(vec![one]) - base);
    }
}

/// Passes run on pipelines built for their one op (fork U0.2.3b, `PX_OP`) give what the generic
/// pass gives, for every effect case; every per-op pipeline builds.
#[test]
fn per_op_pipelines_match_the_generic_pass() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (67u32, 41u32);
    let frame = VideoFrame::rgba_f32(w, h, picture(w, h));
    let run = |generic: bool| {
        let mut c = full_precision(&dev, &q);
        c.fx.as_mut().expect("fx stage").set_interpret_only(generic);
        let out: Vec<(String, bool, Vec<f32>)> = cases()
            .into_iter()
            .map(|(id, params, steps)| {
                let e = effect(id, &params);
                let ops = FxOp::eval_all(&e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap_or_default();
                let fx = LayerFx { size: (w, h), decimation: 1, ops };
                (format!("{id} {params:?}"), steps, c.effect_image(&frame, &fx).expect("effect image").2)
            })
            .collect();
        (out, c.fx.as_ref().expect("fx stage").px_specialised_count())
    };
    let (generic, (none, _)) = run(true);
    let (special, (built, failed)) = run(false);
    assert_eq!(none, 0, "generic run built per-op pipelines");
    assert_eq!(failed, 0, "a per-op pipeline failed to build");
    if std::env::var("FILMCRAFT_CHAIN_SPECIALISE").as_deref() != Ok("0") {
        assert!(built > 10, "only {built} per-op pipelines built");
    }
    let mut failures = Vec::new();
    for ((name, steps, a), (_, _, b)) in generic.iter().zip(&special) {
        let (worst, flips) = compare(a, b);
        if worst > 2e-4 || flips > if *steps { FLIPS } else { 0.0 } {
            failures.push(format!("{name}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "per-op pipelines differ from the generic pass:\n{}", failures.join("\n"));
}

/// Fused passes (fork X422 2b: box cascades in `fx_box3`, one-pass Sharpen-sized Unsharp) give what
/// the passes one by one give, for every effect case, at sizes around the 256-pixel tile (three-pixel lines, a partial last tile,
/// lines shorter than the apron) and both working formats.
#[test]
fn fused_passes_match_the_unfused_passes() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    for format in [fx::FX_FORMAT, fx::FX_FORMAT_HALF] {
        for (w, h) in [(300u32, 37u32), (3, 260), (257, 3), (513, 70), (40, 520)] {
            let frame = VideoFrame::rgba_f32(w, h, picture(w, h));
            let run = |fuse: bool| {
                let mut c = GpuCompositor::with_fx_format(&dev, &q, format);
                c.fx.as_mut().expect("fx stage").set_fuse(fuse);
                cases()
                    .into_iter()
                    .map(|(id, params, steps)| {
                        let e = effect(id, &params);
                        let ops = FxOp::eval_all(&e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap_or_default();
                        let fx = LayerFx { size: (w, h), decimation: 1, ops };
                        (format!("{id} {params:?}"), steps, c.effect_image(&frame, &fx).expect("effect image").2)
                    })
                    .collect::<Vec<_>>()
            };
            let (plain, fused) = (run(false), run(true));
            let mut failures = Vec::new();
            for ((name, steps, a), (_, _, b)) in plain.iter().zip(&fused) {
                let bad = |v: &[f32]| v.iter().position(|x| !x.is_finite());
                if bad(a).is_some() || bad(b).is_some() {
                    failures.push(format!("{format:?} {w}×{h} {name}: non-finite at {:?} (unfused) / {:?} (fused)", bad(a), bad(b)));
                    continue;
                }
                let (worst, flips) = compare(a, b);
                // (f32: the one-pass Unsharp may contract its sums differently, one ulp of blur that
                // Sharpen 4000 amplifies to ~4e-6; half floats: the cascade keeps f32 between its boxes instead of rounding to f16, so a
                // threshold may flip; `half_float_effects_match_cpu` holds the fused result to the CPU)
                let (tol, max_flips) = if format == fx::FX_FORMAT { (1e-5, if *steps { FLIPS } else { 0.0 }) } else { (2e-2, FLIPS) };
                if worst > tol || flips > max_flips {
                    failures.push(format!("{format:?} {w}×{h} {name}: {worst:.2e}, flips {flips:.4}"));
                }
            }
            assert!(failures.is_empty(), "fused passes differ from the unfused passes:\n{}", failures.join("\n"));
        }
    }
}

/// The xm5 bench grade (input chain + Lumetri with HSL Secondary + Sharpen) and HSL variants with
/// Blur / Denoise give the same fused (one chain + HSL pass, one Unsharp pass) as pass by pass
/// (fork X422 2b.2), in full precision.
#[test]
fn fused_grade_matches_the_unfused_passes() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (300u32, 200u32);
    let frame = VideoFrame::rgba_f32(w, h, picture(w, h));
    let hsl = |extra: &[(&str, ParamValue)]| {
        let mut p = vec![("hsl_on", ParamValue::Bool(true)), ("hsl_hue", fl(30.0)), ("hsl_sat", fl(130.0)), ("exposure", fl(0.3))];
        p.extend(extra.iter().cloned());
        ops_of(&effect("lumetri", &p), w, h)
    };
    let grades: Vec<(&str, Vec<FxOp>)> = vec![
        ("bench grade", bench_grade_ops(w, h)),
        ("hsl", hsl(&[])),
        ("hsl blur", hsl(&[("hsl_blur", fl(40.0))])),
        ("hsl denoise", hsl(&[("hsl_denoise", fl(60.0))])),
        ("hsl show mask", hsl(&[("hsl_show_mask", ParamValue::Choice(3))])),
    ];
    let run = |fuse: bool| {
        let mut c = full_precision(&dev, &q);
        c.fx.as_mut().expect("fx stage").set_fuse(fuse);
        grades
            .iter()
            .map(|(_, ops)| c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: ops.clone() }).expect("effect image").2)
            .collect::<Vec<_>>()
    };
    let (plain, fused) = (run(false), run(true));
    for (((name, _), a), b) in grades.iter().zip(&plain).zip(&fused) {
        let (worst, flips) = compare(a, b);
        assert!(worst <= 1e-5 && flips == 0.0, "{name}: fused differs from the passes by {worst:.2e}, flips {flips}");
    }
}

/// GPU cost of the keyers (fork Y3.1d) on a 3840×2160 green-screen-like working image (upload and
/// read-back subtracted by timing the same image without the op), on the compositor's default
/// working format for this adapter. `cargo test -p filmcraft-gpu --lib gpu_key_timing_4k --
/// --ignored --nocapture`.
#[test]
#[ignore = "timing, run by hand"]
fn gpu_key_timing_4k() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (3840u32, 2160u32);
    // a green backdrop with a textured subject in the middle third
    let px: Vec<f32> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            if x > w / 3 && x < 2 * w / 3 && y > h / 4 {
                [(i % 251) as f32 / 251.0, (i % 241) as f32 / 300.0, (i % 239) as f32 / 239.0, 1.0]
            } else {
                [0.05 + (y % 7) as f32 * 0.002, 0.55 + (x % 13) as f32 * 0.003, 0.08, 1.0]
            }
        })
        .collect();
    let frame = VideoFrame::rgba_f32(w, h, px);
    let mut time = |ops: Vec<FxOp>| {
        let fx = LayerFx { size: (w, h), decimation: 1, ops };
        let _ = c.effect_image(&frame, &fx);
        let t = std::time::Instant::now();
        for _ in 0..10 {
            let _ = c.effect_image(&frame, &fx);
        }
        t.elapsed().as_secs_f64() * 1000.0 / 10.0
    };
    let base = time(vec![]);
    let cases: Vec<(&str, Vec<(&str, ParamValue)>)> = vec![
        ("ultra_key", vec![("key_color", col(0.05, 0.55, 0.08))]),
        ("ultra_key", vec![("key_color", col(0.05, 0.55, 0.08)), ("setting", ch(3)), ("choke", fl(20.0)), ("soften", fl(20.0))]),
        ("color_key", vec![("color", col(0.05, 0.55, 0.08)), ("tolerance", fl(60.0))]),
        ("luma_key", vec![("threshold", fl(40.0)), ("cutoff", fl(10.0))]),
    ];
    for (id, params) in cases {
        let e = effect(id, &params);
        let ops = FxOp::eval_all(&e, &cx(Tick::ZERO, 1.0), w as usize, h as usize).unwrap_or_default();
        let ms = time(ops);
        eprintln!("{id} {params:?} 4K: {:.2} ms (frame with read-back {ms:.2} ms, without the op {base:.2} ms)", ms - base);
    }
}
