//! GPU-vs-CPU parity of colour-operation chains (`filmcraft_color::ops::ColorOp::apply` is the
//! reference; fork ADR 0003). Each operation alone, then chains, on the working image read back
//! as f32: values must match to [`TOL`] relative (f32 arithmetic and the GPU's `pow` / `log` /
//! `exp` differ in the last bits).

use std::sync::Arc;

use super::blend_tests::device;
use super::fx_tests::full_precision;
use super::fx_tests::{compare, picture};
use super::*;
use filmcraft_color::Range;
use filmcraft_color::log::LogCurve;
use filmcraft_color::ops::{ColorOp, Table, Tf, gamut_op};
use filmcraft_color::spaces::Gamut;
use filmcraft_render::gpufx::FxOp;
use filmcraft_render::plan::LayerFx;

const TOL: f32 = 2e-4;
/// PQ decoding of the test picture's super-whites (signal up to 1.6, beyond PQ's 0…1): `c2 − c3·p`
/// nearly cancels there and magnifies the last bits of `pow` (Intel UHD 750: 2.4e-4).
const TOL_PQ_DECODE: f32 = 1e-3;
/// HDR zone wheels at +16 stops (×65536) with saturation 1.1: `Y₂ + (rgb₂ − Y₂)·S` cancels on
/// near-grey pixels and shows the dot product's last bits (RX 6700 XT 2.2e-4, UHD 750 4.5e-4).
const TOL_ZONE_CLAMP: f32 = 1e-3;

fn lggo() -> ColorOp {
    ColorOp::Lggo { lift: [0.05, -0.02, 0.0], gamma: [1.2, 0.9, 1.0], gain: [1.1, 0.95, 1.3], offset: [0.01, 0.0, -0.02] }
}

fn cases() -> Vec<(&'static str, Vec<ColorOp>)> {
    let mut v: Vec<(&'static str, Vec<ColorOp>)> = vec![
        ("709 → DWG", vec![gamut_op(Gamut::Bt709, Gamut::DaVinciWide)]),
        ("DWG → 709", vec![gamut_op(Gamut::DaVinciWide, Gamut::Bt709)]),
        ("matrix + offset", vec![ColorOp::Matrix { m: [[1.2, 0.1, -0.1], [0.0, 0.9, 0.05], [0.02, -0.3, 1.4]], offset: [0.01, -0.02, 0.03] }]),
        ("F-Log2 decode (full)", vec![ColorOp::decode_log(LogCurve::FLog2, Range::Full)]),
        ("S-Log3 decode (limited)", vec![ColorOp::decode_log(LogCurve::SLog3, Range::Limited)]),
        ("curve table", vec![ColorOp::Table(Table::sample(0.0, 1.0, 33, |x| x * x * (3.0 - 2.0 * x)))]),
        ("lggo", vec![lggo()]),
        ("clamp", vec![ColorOp::Clamp { lo: 0.1, hi: 0.8 }]),
        ("saturation", vec![ColorOp::Saturation { sat: 1.6, vibrance: 0.0, weights: [0.2126, 0.7152, 0.0722] }]),
        ("desaturate", vec![ColorOp::Saturation { sat: 0.0, vibrance: 0.0, weights: [0.2627, 0.678, 0.0593] }]),
    ];
    for (t, enc, dec) in [
        (Tf::Srgb, "sRGB encode", "sRGB decode"),
        (Tf::Gamma24, "γ 2.4 encode", "γ 2.4 decode"),
        (Tf::Pq, "PQ encode", "PQ decode"),
        (Tf::Hlg, "HLG encode", "HLG decode"),
        (Tf::DaVinciIntermediate, "DI encode", "DI decode"),
    ] {
        v.push((enc, vec![ColorOp::Encode(t)]));
        v.push((dec, vec![ColorOp::Decode(t)]));
    }
    v.push(("HLG OOTF", vec![ColorOp::HlgOotf { weights: [0.2627, 0.678, 0.0593], k: 4.926, exponent: 0.2 }]));
    v.push(("tone map 4000 → 203", vec![filmcraft_color::transform::ToneMap::new(4000.0, 203.0).op()]));
    v.push(("gamut compress", vec![ColorOp::Matrix { m: [[1.4, -0.3, -0.1], [-0.2, 1.3, -0.1], [0.0, -0.4, 1.4]], offset: [0.0; 3] }, ColorOp::GamutCompress]));
    v.push((
        "desaturate into gamut",
        vec![
            ColorOp::Matrix { m: [[1.4, -0.3, -0.1], [-0.2, 1.3, -0.1], [0.0, -0.4, 1.4]], offset: [0.0; 3] },
            ColorOp::DesaturateIntoGamut { weights: [0.2126, 0.7152, 0.0722] },
        ],
    ));
    // Lumetri's own operations (4.3a)
    let luma = [0.2126, 0.7152, 0.0722];
    v.push(("tone regions", vec![ColorOp::ToneRegions { shadows: 0.2, highlights: -0.25, weights: luma }]));
    v.push(("contrast +", vec![ColorOp::Contrast { amount: 0.5, keep_above_one: false }]));
    v.push(("contrast − (HDR)", vec![ColorOp::Contrast { amount: -0.4, keep_above_one: true }]));
    v.push(("specular", vec![ColorOp::Specular { gain: 0.6 }]));
    v.push(("split tone", vec![ColorOp::SplitTone { shadows: [-0.06, 0.03, 0.06], highlights: [0.09, 0.015, -0.06], weights: luma }]));
    v.push(("vibrance", vec![ColorOp::Saturation { sat: 1.2, vibrance: 0.6, weights: luma }]));
    v.push(("vignette dark", vec![ColorOp::Vignette { amount: -3.0, midpoint: 0.45, feather: 0.27, sx: 1.3, size: [67.0, 41.0] }]));
    v.push(("vignette light", vec![ColorOp::Vignette { amount: 2.0, midpoint: 0.27, feather: 0.45, sx: 1.0, size: [67.0, 41.0] }]));
    // film grain (G1.6): hard per-pixel, soft 2.5 px, smooth 4 px cells; seeds using all 32 bits
    v.push(("grain hard", vec![ColorOp::Grain { strength: 1.0, inv_size: 1.0, softness: 0.0, seed: 0xdead_beef, weights: luma }]));
    v.push(("grain soft", vec![ColorOp::Grain { strength: 0.7, inv_size: 1.0 / 2.5, softness: 0.6, seed: 7, weights: luma }]));
    v.push(("grain smooth", vec![ColorOp::Grain { strength: 0.4, inv_size: 0.25, softness: 1.0, seed: u32::MAX, weights: luma }]));
    // Curves, Wheels, Looks, LUTs (4.3b)
    let curve = Table::sample(0.0, 1.0, 1024, |x| x + 0.15 * (std::f64::consts::TAU * x).sin() * x * (1.0 - x));
    v.push(("curve (luma)", vec![ColorOp::Curve { table: curve.clone(), channels: [true; 3], hdr: false }]));
    v.push(("curve (red, HDR)", vec![ColorOp::Curve { table: curve.clone(), channels: [true, false, false], hdr: true }]));
    v.push(("wheels", vec![ColorOp::Wheels { shadows: [0.06, -0.03, 0.02], midtones: [-0.02, 0.05, 0.0], highlights: [0.1, -0.05, 0.08], weights: luma }]));
    let hue = |k: f64| Some(Table::sample(0.0, 1.0, 1024, move |x| 0.5 + 0.2 * (std::f64::consts::TAU * (x + k)).sin()));
    for hdr in [false, true] {
        let h = filmcraft_color::ops::HslCurves {
            hue_vs_hue: hue(0.1),
            hue_vs_sat: hue(0.3),
            hue_vs_luma: hue(0.5),
            luma_vs_sat: hue(0.7),
            sat_vs_sat: hue(0.9),
            hdr,
        };
        v.push(("HSL curves", vec![ColorOp::HslCurves(Arc::new(h))]));
    }
    let h = filmcraft_color::ops::HslCurves { hue_vs_hue: None, hue_vs_sat: hue(0.2), hue_vs_luma: None, luma_vs_sat: None, sat_vs_sat: hue(0.4), hdr: false };
    v.push(("HSL curves (two)", vec![ColorOp::HslCurves(Arc::new(h))]));
    for look in 1..=8 {
        v.push(("look", vec![ColorOp::Look { look, intensity: 0.8, hdr: false }]));
        v.push(("look (HDR)", vec![ColorOp::Look { look, intensity: 1.0, hdr: true }]));
    }
    let mut cube = filmcraft_color::Lut3d::from_fn(17, |c| [c[0] * c[0] * 0.9 + 0.05 * c[2], c[1].sqrt() * 0.95, (c[2] + 0.2 * c[0]).min(1.0)]);
    cube.domain_min = [-0.05, 0.0, 0.0];
    cube.domain_max = [1.1, 1.0, 1.2];
    let shaper =
        filmcraft_color::Lut1d { domain_min: [0.0; 3], domain_max: [1.5, 1.5, 1.5], data: (0..64).map(|i| [(i as f32 / 63.0).powf(0.8); 3]).collect() };
    for (name, l, hdr) in [
        ("LUT (cube)", filmcraft_color::Lut::from_cube(cube.clone()), false),
        ("LUT (shaper + cube, HDR)", filmcraft_color::Lut { title: String::new(), shaper: Some(shaper.clone()), cube: Some(cube.clone()) }, true),
        ("LUT (shaper)", filmcraft_color::Lut { title: String::new(), shaper: Some(shaper), cube: None }, false),
    ] {
        v.push((name, vec![ColorOp::Lut { lut: Arc::new(l), intensity: 0.85, hdr }]));
    }
    // colour warper (G4.1): hue-sat (with and without luma kept, a falloff, a pin) and chroma-luma
    for (name, space, preserve, moved, range) in [
        ("warp hue-sat", filmcraft_color::warp::WarpSpace::HueSat, false, vec![(9u32, [0.08f32, 0.2f32]), (20, [-0.1, -0.3]), (31, [0.3, 0.0])], 1.5f32),
        ("warp hue-sat (luma kept)", filmcraft_color::warp::WarpSpace::HueSat, true, vec![(2, [0.05, -0.25]), (13, [-0.2, 0.15])], 0.0),
        ("warp chroma-luma", filmcraft_color::warp::WarpSpace::ChromaLuma, false, vec![(27, [0.0, -0.25]), (14, [0.2, 0.1]), (8, [0.0, 0.2])], 2.0),
    ] {
        let spec = filmcraft_color::warp::MeshSpec {
            space,
            cols: 8,
            rows: 4,
            moved: &moved,
            pinned: &[10],
            range,
            strength: 0.9,
            preserve_luma: preserve,
            weights: luma,
        };
        let w = filmcraft_color::warp::compile(&spec).expect("a warp");
        v.push((name, vec![ColorOp::Warp2D(Arc::new(w))]));
    }
    // 3D qualifier (G4.3)
    v.extend(key3d_cases());
    // whole Lumetri grades in every working space
    for working in [
        filmcraft_color::WorkingSpace::Rec709,
        filmcraft_color::WorkingSpace::DaVinciWide,
        filmcraft_color::WorkingSpace::Rec2100Pq,
        filmcraft_color::WorkingSpace::Rec2100Hlg,
    ] {
        let mut e = filmcraft_project::find_effect("lumetri").expect("lumetri").instance();
        for (k, val) in [
            ("temperature", -20.0),
            ("exposure", -0.4),
            ("contrast", 30.0),
            ("highlights", 25.0),
            ("shadows", -15.0),
            ("whites", 10.0),
            ("blacks", -10.0),
            ("saturation", 120.0),
            ("faded_film", 20.0),
            ("vibrance", -30.0),
            ("hdr_specular", 30.0),
            ("vignette_amount", -1.5),
        ] {
            if let Some(p) = e.params.get_mut(k) {
                p.value = filmcraft_project::ParamValue::Float(val);
            }
        }
        let cx = filmcraft_render::effects::FxCtx {
            t: filmcraft_time::Tick::ZERO,
            px_scale: 1.0,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            working,
        };
        for (k, val) in [
            ("curve_luma", filmcraft_project::ParamValue::Curve(vec![[0.0, 0.03], [0.4, 0.35], [1.0, 0.97]])),
            ("hue_vs_sat", filmcraft_project::ParamValue::Curve(vec![[0.0, 0.6], [0.5, 0.4]])),
            ("wheel_midtones", filmcraft_project::ParamValue::Vec2(filmcraft_geom::Vec2::new(0.1, -0.2))),
            ("look", filmcraft_project::ParamValue::Choice(3)),
        ] {
            if let Some(p) = e.params.get_mut(k) {
                p.value = val;
            }
        }
        let ops = filmcraft_render::effects::lumetri_gpu(&e, &cx, 67, 41).expect("GPU form");
        let Some(FxOp::ColorChain(chain)) = ops.first() else { panic!("{ops:?}") };
        v.push(("Lumetri grade", chain.to_vec()));
    }
    // every input transform into a Rec. 709 sequence (log tone mapped, camera gamuts compressed)
    for cs in filmcraft_color::ColorSpace::ALL {
        v.push(("input transform", filmcraft_color::transform::input_ops(cs, Range::Limited, &filmcraft_color::ColorPipeline::REC709, None)));
    }
    // HDR zone wheels (fork G4.2): every zone and the global wheel set, on linear light (DWG
    // weights, test picture values 0…1.6 span the dark and light zones), and narrow edges
    {
        use filmcraft_color::ops::{Zone, ZoneAdjust, ZoneWheels};
        let w = Gamut::DaVinciWide.luma().map(|v| v as f32);
        let shift = |r: f32, g: f32| {
            let b = -(w[0] * r + w[1] * g) / w[2];
            [r, g, b]
        };
        let zone =
            |edge: f32, falloff: f32, exposure: f32, sat: f32, r: f32, g: f32| Zone { edge, falloff, adjust: ZoneAdjust { exposure, sat, shift: shift(r, g) } };
        let z = ZoneWheels {
            weights: w,
            pivot: 0.18,
            global: ZoneAdjust { exposure: 0.3, sat: 1.1, shift: shift(0.02, -0.01) },
            zones: [
                zone(-4.0, 1.0, 0.5, 0.8, 0.05, 0.0),
                zone(-2.5, 1.5, -0.4, 1.2, 0.0, 0.04),
                zone(-1.0, 0.7, 0.8, 0.9, -0.03, 0.02),
                zone(0.5, 1.0, -0.6, 1.3, 0.04, -0.02),
                zone(1.5, 0.4, 1.2, 0.7, -0.02, 0.0),
                zone(2.5, 0.2, -1.5, 0.5, 0.0, -0.05),
            ],
        };
        v.push(("zone wheels", vec![ColorOp::ZoneWheels(Arc::new(z.clone()))]));
        // hostile but finite: the exposure sum past both clamps (±16 stops)
        for e in [12.0f32, -12.0] {
            let mut h = z.clone();
            h.global.exposure = e;
            for zone in &mut h.zones {
                zone.adjust.exposure = e.signum() * 4.0;
            }
            v.push(("zone wheels (exposure clamp)", vec![ColorOp::ZoneWheels(Arc::new(h))]));
        }
        v.push((
            "zone wheels in a DI grade",
            vec![
                ColorOp::Encode(Tf::DaVinciIntermediate),
                lggo(),
                ColorOp::Decode(Tf::DaVinciIntermediate),
                ColorOp::ZoneWheels(Arc::new(z)),
                ColorOp::Encode(Tf::DaVinciIntermediate),
            ],
        ));
    }
    // compiled node grades with HDR wheels in every working space (PQ / HLG decode, act, encode)
    for working in filmcraft_color::WorkingSpace::ALL {
        let mut n = filmcraft_project::grade::GradeNode::new(1);
        for (k, val) in [
            ("hdr.highlight.exposure", filmcraft_project::ParamValue::Float(-1.0)),
            ("hdr.shadow.exposure", filmcraft_project::ParamValue::Float(0.7)),
            ("hdr.global.sat", filmcraft_project::ParamValue::Float(1.3)),
            ("hdr.light.balance", filmcraft_project::ParamValue::Color([0.1, -0.05, -0.05, 0.0])),
        ] {
            n.params.insert(k.into(), filmcraft_project::Param::new(val));
        }
        let g = filmcraft_project::grade::Grade { nodes: vec![n], ..Default::default() };
        v.push(("zone wheels grade", filmcraft_render::grade::compile(&g, filmcraft_time::Tick::ZERO, working)));
    }
    // an F-Log2 clip graded in DaVinci Wide Gamut / Intermediate and shown in Rec. 709
    v.push((
        "F-Log2 → DWG/DI grade → 709",
        vec![
            ColorOp::decode_log(LogCurve::FLog2, Range::Limited),
            gamut_op(Gamut::Bt2020, Gamut::DaVinciWide),
            ColorOp::Encode(Tf::DaVinciIntermediate),
            lggo(),
            ColorOp::Saturation { sat: 1.3, vibrance: 0.0, weights: [0.2627, 0.678, 0.0593] },
            ColorOp::Decode(Tf::DaVinciIntermediate),
            gamut_op(Gamut::DaVinciWide, Gamut::Bt709),
            ColorOp::Encode(Tf::Gamma24),
        ],
    ));
    v
}

/// Every operation and chain on the GPU against the CPU.
#[test]
fn gpu_color_ops_match_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut failures = Vec::new();
    for (name, ops) in cases() {
        let op = FxOp::ColorChain(Arc::from(ops));
        assert!(op.gpu_ok(), "{name}");
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        op.apply(&mut cpu);
        let fx = LayerFx { size: (w, h), decimation: 1, ops: vec![op] };
        let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("{name}: max rel diff {worst:.2e}, {:.3}% off by > 2 %", flips * 100.0);
        let tol = match name {
            "PQ decode" => TOL_PQ_DECODE,
            "zone wheels (exposure clamp)" => TOL_ZONE_CLAMP,
            _ => TOL,
        };
        if worst > tol || flips > 0.0 {
            failures.push(format!("{name}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "GPU colour ops differ from the CPU:\n{}", failures.join("\n"));
}

/// The colour warper (G4.1) on hostile pixels: near black, wide-gamut negatives, HDR above 1,
/// grey and black, in both planes, with and without luma kept. Prints the max absolute error.
#[test]
fn gpu_warp_matches_cpu_on_hostile_pixels() {
    use filmcraft_color::warp::{MeshSpec, WarpSpace, compile, hsv_to_rgb};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let mut pixels: Vec<[f32; 3]> = vec![
        [0.02, -0.0059, 0.0],
        [0.0011, 0.0, 0.0],
        [1e-5, 0.0, 0.0],
        [3e-4, 1e-4, 0.0],
        [0.002, 0.0005, -0.0004],
        [1.8, 0.9, 0.2],
        [2.5, 1.2, 3.0],
        [6.0, 0.5, 0.1],
        [-0.1, 0.4, 0.6],
        [0.4, -0.05, 0.1],
        [0.5, 0.5, 0.5],
        [0.0, 0.0, 0.0],
    ];
    for i in 0..48 {
        let h = i as f32 / 48.0;
        pixels.push(hsv_to_rgb(h, 0.3 + 0.7 * (i % 4) as f32 / 3.0, 0.2 + 0.25 * (i % 5) as f32));
    }
    let (w, h) = (pixels.len() as u32, 1u32);
    let px: Vec<f32> = pixels.iter().flat_map(|p| [p[0], p[1], p[2], 1.0].map(super::fx_tests::half)).collect();
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let luma = [0.2126, 0.7152, 0.0722];
    let mut failures = Vec::new();
    for space in [WarpSpace::HueSat, WarpSpace::ChromaLuma] {
        for preserve in [false, true] {
            let moved = [(24u32, [0.05f32, -0.3f32]), (9, [0.1, 0.2]), (14, [-0.15, -0.2]), (27, [0.0, -0.25]), (5, [0.0, 0.3])];
            let spec = MeshSpec { space, cols: 8, rows: 4, moved: &moved, pinned: &[], range: 2.0, strength: 1.0, preserve_luma: preserve, weights: luma };
            let warp = compile(&spec).expect("a warp");
            let op = FxOp::ColorChain(Arc::from(vec![ColorOp::Warp2D(Arc::new(warp))]));
            let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
            op.apply(&mut cpu);
            let fx = LayerFx { size: (w, h), decimation: 1, ops: vec![op] };
            let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
            let abs = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            let (worst, flips) = compare(&cpu.px, &gpu);
            eprintln!("warp {space:?} preserve {preserve}: max abs {abs:.2e}, max rel {worst:.2e}, flips {flips}");
            if worst > TOL || flips > 0.0 {
                failures.push(format!("{space:?} {preserve}: {worst:.2e} abs {abs:.2e}, flips {flips:.4}"));
            }
        }
    }
    assert!(failures.is_empty(), "GPU warp differs from the CPU:\n{}", failures.join("\n"));
}

/// Fork G4.2: the HDR zone wheels' largest absolute GPU − CPU difference (the parity report's
/// number; the relative check is in [`gpu_color_ops_match_cpu`]).
#[test]
fn gpu_zone_wheels_absolute_error() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    for (name, ops) in cases().into_iter().filter(|(n, _)| matches!(*n, "zone wheels" | "zone wheels in a DI grade")) {
        let op = FxOp::ColorChain(Arc::from(ops));
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        op.apply(&mut cpu);
        let fx = LayerFx { size: (w, h), decimation: 1, ops: vec![op] };
        let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
        let worst = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        eprintln!("{name}: max abs diff {worst:.2e}");
        assert!(worst < 1e-4, "{name}: {worst}");
    }
}

/// Review fix (G4.2): saturation at its cap (every wheel's saturation absurd, product clamped to
/// 65536) amplifies the last bits of `Y₂` by the cap, so this compares with a looser tolerance:
/// finite, and within 1 % of the larger of the value and 1.
#[test]
fn gpu_zone_wheels_at_the_saturation_cap() {
    use filmcraft_color::ops::{Zone, ZoneAdjust, ZoneWheels};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let w = Gamut::DaVinciWide.luma().map(|v| v as f32);
    let zone = |edge| Zone { edge, falloff: 1.0, adjust: ZoneAdjust { exposure: 0.0, sat: 100.0, shift: [0.0; 3] } };
    let z = ZoneWheels {
        weights: w,
        pivot: 0.18,
        global: ZoneAdjust { exposure: 0.0, sat: 100.0, shift: [0.0; 3] },
        zones: [-4.0, -2.5, -1.0, 1.0, 2.5, 4.5].map(zone),
    };
    let mut c = full_precision(&dev, &q);
    let (wd, h) = (67u32, 41u32);
    let px = picture(wd, h);
    let frame = VideoFrame::rgba_f32(wd, h, px.clone());
    let op = FxOp::ColorChain(Arc::from(vec![ColorOp::ZoneWheels(Arc::new(z))]));
    let mut cpu = filmcraft_render::Image { w: wd as usize, h: h as usize, px: px.clone() };
    op.apply(&mut cpu);
    let fx = LayerFx { size: (wd, h), decimation: 1, ops: vec![op] };
    let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
    let worst = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs() / a.abs().max(1.0)).fold(0.0f32, f32::max);
    eprintln!("zone wheels at the saturation cap: max diff {worst:.2e} (relative above 1)");
    assert!(gpu.iter().all(|v| v.is_finite()), "non-finite GPU output");
    assert!(worst < 1e-2, "{worst}");
}

/// Lumetri HSL Secondary (key, Denoise, Blur, correction, mask views) on the GPU against the CPU.
#[test]
fn gpu_hsl_secondary_matches_cpu() {
    use filmcraft_project::ParamValue;
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut failures = Vec::new();
    for working in [
        filmcraft_color::WorkingSpace::Rec709,
        filmcraft_color::WorkingSpace::DaVinciWide,
        filmcraft_color::WorkingSpace::Rec2100Pq,
        filmcraft_color::WorkingSpace::Rec2100Hlg,
    ] {
        for (denoise, blur, show) in [(0.0, 0.0, 0), (70.0, 0.0, 0), (0.0, 30.0, 0), (100.0, 10.0, 0), (40.0, 5.0, 1), (40.0, 0.0, 2), (40.0, 20.0, 3)] {
            let mut e = filmcraft_project::find_effect("lumetri").expect("lumetri").instance();
            for (k, val) in [
                ("hsl_on", ParamValue::Bool(true)),
                ("hsl_hue", ParamValue::Float(200.0)),
                ("hsl_hue_range", ParamValue::Float(120.0)),
                ("hsl_soft", ParamValue::Float(40.0)),
                ("hsl_sat", ParamValue::Float(150.0)),
                ("hsl_hue_shift", ParamValue::Float(-20.0)),
                ("hsl_temp", ParamValue::Float(30.0)),
                ("hsl_denoise", ParamValue::Float(denoise)),
                ("hsl_blur", ParamValue::Float(blur)),
                ("hsl_show_mask", ParamValue::Choice(show)),
            ] {
                if let Some(p) = e.params.get_mut(k) {
                    p.value = val;
                }
            }
            let cx = filmcraft_render::effects::FxCtx {
                t: filmcraft_time::Tick::ZERO,
                px_scale: 1.0,
                seconds: 0.0,
                timecode: "",
                clip_name: "",
                project: None,
                env: None,
                working,
            };
            let ops = filmcraft_render::effects::lumetri_gpu(&e, &cx, w as usize, h as usize).expect("GPU form");
            assert!(ops.iter().all(FxOp::gpu_ok));
            let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
            for op in &ops {
                op.apply(&mut cpu);
            }
            let fx = LayerFx { size: (w, h), decimation: 1, ops };
            let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
            let (worst, flips) = compare(&cpu.px, &gpu);
            eprintln!("HSL {working:?} denoise {denoise} blur {blur} show {show}: {worst:.2e}, flips {flips:.4}");
            let tol = if working == filmcraft_color::WorkingSpace::Rec2100Pq { 2e-3 } else { TOL };
            if worst > tol || flips > 0.0 {
                failures.push(format!("{working:?} {denoise}/{blur}/{show}: {worst:.2e}, flips {flips:.4}"));
            }
        }
    }
    assert!(failures.is_empty(), "GPU HSL Secondary differs from the CPU:\n{}", failures.join("\n"));
}

/// Masked effects (a whole Lumetri with HSL Secondary and Sharpen inside two masks) and opacity
/// masks on the GPU against the CPU.
#[test]
fn gpu_masked_effects_match_cpu() {
    use filmcraft_project::{MaskMode, ParamValue};
    use filmcraft_render::mask::FlatMask;
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let ellipse: Vec<[f32; 2]> = (0..48)
        .map(|i| {
            let a = i as f32 / 48.0 * std::f32::consts::TAU;
            [33.0 + 22.0 * a.cos(), 20.0 + 13.0 * a.sin()]
        })
        .collect();
    let square = vec![[5.0, 5.0], [25.0, 5.0], [25.0, 30.0], [5.0, 30.0]];
    let mask = |pts: &Vec<[f32; 2]>, mode, feather, inverted| FlatMask { pts: pts.clone(), feather, expansion: 1.5, opacity: 0.9, inverted, mode };
    let masks: Arc<[FlatMask]> = vec![mask(&ellipse, MaskMode::Add, 6.0, false), mask(&square, MaskMode::Subtract, 2.0, false)].into();
    let mut e = filmcraft_project::find_effect("lumetri").expect("lumetri").instance();
    for (k, val) in [
        ("exposure", ParamValue::Float(0.8)),
        ("contrast", ParamValue::Float(40.0)),
        ("sharpen", ParamValue::Float(60.0)),
        ("hsl_on", ParamValue::Bool(true)),
        ("hsl_hue", ParamValue::Float(200.0)),
        ("hsl_hue_range", ParamValue::Float(120.0)),
        ("hsl_blur", ParamValue::Float(10.0)),
        ("hsl_sat", ParamValue::Float(0.0)),
    ] {
        if let Some(p) = e.params.get_mut(k) {
            p.value = val;
        }
    }
    let cx = filmcraft_render::effects::FxCtx {
        t: filmcraft_time::Tick::ZERO,
        px_scale: 1.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project: None,
        env: None,
        working: filmcraft_color::WorkingSpace::Rec709,
    };
    let inner = filmcraft_render::effects::lumetri_gpu(&e, &cx, w as usize, h as usize).expect("GPU form");
    let cases: Vec<(&str, Vec<FxOp>)> = vec![
        ("masked Lumetri", vec![FxOp::Masked { ops: inner.clone(), masks: masks.clone() }]),
        (
            "masked Lumetri + opacity mask",
            vec![FxOp::Masked { ops: inner, masks }, FxOp::OpacityMask { masks: vec![mask(&ellipse, MaskMode::Add, 10.0, true)].into() }],
        ),
        ("no active mask", vec![FxOp::Masked { ops: vec![FxOp::BlackWhite], masks: vec![mask(&square, MaskMode::None, 0.0, false)].into() }]),
    ];
    for (name, ops) in cases {
        assert!(ops.iter().all(FxOp::gpu_ok), "{name}");
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        for op in &ops {
            op.apply(&mut cpu);
        }
        let fx = LayerFx { size: (w, h), decimation: 1, ops };
        let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("{name}: {worst:.2e}, flips {flips:.4}");
        assert!(worst <= 1e-3 && flips == 0.0, "{name}: {worst:.2e}, flips {flips}");
    }
}

/// Non-finite numbers never reach the shader.
#[test]
fn non_finite_chains_stay_on_the_cpu() {
    let bad = FxOp::ColorChain(Arc::from(vec![ColorOp::Saturation { sat: f32::NAN, vibrance: 0.0, weights: [0.3; 3] }]));
    assert!(!bad.gpu_ok());
    let bad = FxOp::ColorChain(Arc::from(vec![ColorOp::Table(Table { lo: 0.0, hi: f32::INFINITY, values: vec![0.0, 1.0].into() })]));
    assert!(!bad.gpu_ok());
}

/// A colour-managed sequence's plan: layers composited in the working space (DaVinci Wide Gamut),
/// the output transform on the accumulator, then a display-referred layer on top — like
/// `execute_cpu`.
#[test]
fn output_stage_converts_the_composite_like_the_cpu() {
    use super::blend_tests::{interior, ramp_layer, stats8, yuv_frame};
    use filmcraft_geom::Affine;
    use filmcraft_render::blend::Blend;
    use filmcraft_render::plan::{FramePlan, OutputStage, PlanLayer, execute_cpu};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let pipe = filmcraft_color::ColorPipeline { working: filmcraft_color::WorkingSpace::DaVinciWide, ..filmcraft_color::ColorPipeline::REC709 };
    let ops = filmcraft_color::OutputTransform::display(&pipe).ops();
    assert!(!ops.is_empty());
    // bright working-space values (scene light up to ~6×) so the tone map has work to do
    let bright = {
        let f = ramp_layer(200, 120, 1);
        let filmcraft_frame::PixelData::RgbaF32(px) = &f.data else { unreachable!() };
        Arc::new(VideoFrame::rgba_f32(200, 120, px.iter().enumerate().map(|(i, v)| if i % 4 == 3 { *v } else { v * 6.0 }).collect()))
    };
    let (w, h) = (320usize, 180usize);
    for at in [2usize, 3] {
        let plan = FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![
                PlanLayer { frame: yuv_frame(640, 360), matrix: Affine::scale(0.5, 0.5), opacity: 1.0, blend: Blend::Normal, fx: None, comp: None },
                PlanLayer { frame: bright.clone(), matrix: Affine::translate(100.0, 40.0), opacity: 0.9, blend: Blend::Screen, fx: None, comp: None },
                // a caption: drawn after the conversion when `at` = 2
                PlanLayer { frame: ramp_layer(80, 30, 4), matrix: Affine::translate(20.0, 140.0), opacity: 1.0, blend: Blend::Normal, fx: None, comp: None },
            ],
            output: Some(OutputStage { ops: ops.clone().into(), at }),
        };
        let cpu = execute_cpu(&plan).over_black_rgba8();
        c.composite(&plan);
        let (_, _, gpu) = c.read_output().expect("readback");
        let (p99, mean, max) = stats8(&cpu, &gpu, &interior(&plan));
        eprintln!("output at {at}: 8-bit p99 {p99} mean {mean:.3} max {max}");
        assert!(p99 <= 3 && mean < 0.75, "at {at}: p99 {p99}, mean {mean}, max {max}");
    }
}

/// Colour grades (fork ADR 0004) compiled to colour operations run on the GPU like on the CPU:
/// three serial nodes (primaries, contrast / saturation / white balance, curves) in a Rec. 709 and
/// a DWG / DI working space; an identity grade compiles to nothing.
#[test]
fn gpu_compiled_grades_match_cpu() {
    use filmcraft_color::WorkingSpace;
    use filmcraft_project::grade::{Grade, GradeNode};
    use filmcraft_project::{Param, ParamValue};
    use filmcraft_time::Tick;
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let node = |id: u64, params: &[(&str, ParamValue)]| {
        let mut n = GradeNode::new(id);
        for (k, v) in params {
            n.params.insert((*k).into(), Param::new(v.clone()));
        }
        n
    };
    let grade = Grade {
        nodes: vec![
            node(
                1,
                &[
                    ("lift", ParamValue::Color([0.04, 0.0, -0.03, 0.01])),
                    ("gamma", ParamValue::Color([0.0, 0.2, -0.1, 0.0])),
                    ("gain", ParamValue::Color([0.1, 0.0, 0.2, -0.05])),
                    ("offset", ParamValue::Color([0.0, 0.0, 0.0, 0.02])),
                ],
            ),
            node(
                2,
                &[
                    ("contrast", ParamValue::Float(1.25)),
                    ("pivot", ParamValue::Float(0.4)),
                    ("saturation", ParamValue::Float(1.3)),
                    ("temperature", ParamValue::Float(-20.0)),
                    ("tint", ParamValue::Float(10.0)),
                    // film grain at this node's place (G1.6)
                    ("grain.strength", ParamValue::Float(0.6)),
                    ("grain.size", ParamValue::Float(1.5)),
                    ("grain.softness", ParamValue::Float(0.4)),
                ],
            ),
            node(
                3,
                &[
                    ("curve.master", ParamValue::Curve(vec![[0.0, 0.03], [0.4, 0.35], [1.0, 0.97]])),
                    ("curve.r", ParamValue::Curve(vec![[0.0, 0.0], [0.6, 0.65], [1.0, 1.0]])),
                ],
            ),
        ],
        bypass: false,
        ..Default::default()
    };
    // qualified nodes (G2.1): one per-pixel and inverted, one with a blurred matte (an HSL
    // secondary step), one showing its highlight
    let qual = |id: u64, extra: &[(&str, ParamValue)]| {
        let mut ps = vec![
            ("gain", ParamValue::Color([0.1, -0.05, 0.0, 0.2])),
            ("saturation", ParamValue::Float(0.6)),
            ("qual.on", ParamValue::Float(1.0)),
            ("qual.hue", ParamValue::Float(0.05)),
            ("qual.hueWidth", ParamValue::Float(0.25)),
            ("qual.satLow", ParamValue::Float(0.1)),
            ("qual.satHigh", ParamValue::Float(0.8)),
            ("qual.lumLow", ParamValue::Float(0.1)),
            ("qual.lumHigh", ParamValue::Float(0.9)),
            ("qual.soft", ParamValue::Float(0.1)),
        ];
        ps.extend(extra.iter().cloned());
        node(id, &ps)
    };
    let mut qualified = grade.clone();
    qualified.nodes.push(qual(4, &[("qual.invert", ParamValue::Float(1.0))]));
    qualified.nodes.push(qual(5, &[("qual.blur", ParamValue::Float(0.2))]));
    // a node limited by a window (an ellipse with feather) and a qualified, windowed node (G2.2)
    let mut windowed = qual(7, &[]);
    let mut win =
        filmcraft_project::Mask::new("w", filmcraft_project::MaskPath::ellipse(filmcraft_geom::Vec2::new(30.0, 20.0), filmcraft_geom::Vec2::new(18.0, 12.0)));
    win.feather = Param::new(ParamValue::Float(6.0));
    windowed.windows.push(win.clone());
    qualified.nodes.push(windowed);
    let mut lifted = node(8, &[("lift", ParamValue::Color([0.0, 0.05, 0.0, 0.02]))]);
    win.inverted = true;
    lifted.windows.push(win);
    // noise reduction at the node's input, inside its (inverted) window (G2.4)
    lifted.params.insert("nr.luma".into(), Param::new(ParamValue::Float(0.6)));
    lifted.params.insert("nr.chroma".into(), Param::new(ParamValue::Float(0.4)));
    qualified.nodes.push(lifted);
    // a node LUT and the highlight soft clip (G2.5 / G2.6)
    let mut finishing = node(9, &[("hl.rolloff", ParamValue::Float(0.8)), ("hl.knee", ParamValue::Float(0.6)), ("lut.mix", ParamValue::Float(0.7))]);
    if let Some(b) = filmcraft_render::luts::builtins().iter().find(|b| b.id.starts_with("look-")) {
        finishing.lut = format!("builtin:{}", b.id);
    }
    qualified.nodes.push(finishing);
    let mut shown = qualified.clone();
    shown.nodes.push(qual(6, &[("qual.show", ParamValue::Float(1.0))]));
    let mut c = full_precision(&dev, &q);
    let (w, h) = (67u32, 41u32);
    let px = picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut failures = Vec::new();
    for ws in [WorkingSpace::Rec709, WorkingSpace::DaVinciWide] {
        for (name, g) in [("qualified", &qualified), ("highlight", &shown)] {
            let ctx = filmcraft_render::grade::GradeCtx { seq_time: Tick(5), ..Default::default() };
            let ops = filmcraft_render::grade::compile_fx(g, Tick(123_456_789), ws, ctx, w as usize, h as usize);
            assert!(ops.iter().any(|o| matches!(o, FxOp::HslSecondary { .. })), "{name}");
            assert!(ops.iter().filter(|o| matches!(o, FxOp::Masked { .. })).count() >= 1, "{name}");
            assert!(ops.iter().all(FxOp::gpu_ok), "{name} {ws:?}");
            let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
            ops.iter().for_each(|o| o.apply(&mut cpu));
            let fx = LayerFx { size: (w, h), decimation: 1, ops };
            let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
            let (worst, flips) = compare(&cpu.px, &gpu);
            eprintln!("{name} grade in {ws:?}: max rel diff {worst:.2e}, {:.3}% off by > 2 %", flips * 100.0);
            if worst > TOL || flips > 0.0 {
                failures.push(format!("{name} {ws:?}: {worst:.2e}, flips {flips:.4}"));
            }
        }
        assert!(filmcraft_render::grade::compile(&Grade::with_node(), Tick(0), ws).is_empty());
        let ops = filmcraft_render::grade::compile(&grade, Tick(123_456_789), ws);
        assert!(ops.iter().any(|o| matches!(o, ColorOp::Grain { .. })));
        let op = FxOp::ColorChain(Arc::from(ops));
        assert!(op.gpu_ok(), "{ws:?}");
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        op.apply(&mut cpu);
        let fx = LayerFx { size: (w, h), decimation: 1, ops: vec![op] };
        let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
        let (worst, flips) = compare(&cpu.px, &gpu);
        eprintln!("grade in {ws:?}: max rel diff {worst:.2e}, {:.3}% off by > 2 %", flips * 100.0);
        if worst > TOL || flips > 0.0 {
            failures.push(format!("{ws:?}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "GPU grades differ from the CPU:\n{}", failures.join("\n"));
}

/// Chains run as shaders specialised to their op codes (fork U0.2.3b) give exactly what the
/// interpreter gives: every case alone, and all of them as one chain.
#[test]
fn specialised_chains_match_the_interpreter() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (67u32, 41u32);
    let frame = VideoFrame::rgba_f32(w, h, picture(w, h));
    let mut chains = cases();
    let all: Vec<ColorOp> = cases().into_iter().flat_map(|(_, ops)| ops).collect();
    // (the longest chain the specialiser takes, and one past it: the interpreter)
    let long: Vec<ColorOp> = all.iter().cycle().take(crate::colorops::MAX_SPECIALISED_OPS).cloned().collect();
    chains.push(("every case in one chain", all));
    chains.push(("longest specialised chain", long.clone()));
    chains.push(("one op too long", long.into_iter().chain([lggo()]).collect()));
    let run = |interpret_only: bool| {
        let mut c = full_precision(&dev, &q);
        c.fx.as_mut().expect("fx stage").set_interpret_only(interpret_only);
        let out: Vec<Vec<f32>> = chains
            .iter()
            .map(|(_, ops)| {
                let fx = LayerFx { size: (w, h), decimation: 1, ops: vec![FxOp::ColorChain(Arc::from(ops.clone()))] };
                c.effect_image(&frame, &fx).expect("effect image").2
            })
            .collect();
        (out, c.fx.as_ref().expect("fx stage").specialised_count())
    };
    let (interpreted, (none, _)) = run(true);
    let (specialised, (built, failed)) = run(false);
    assert_eq!(none, 0, "interpret-only built specialised shaders");
    assert_eq!(failed, 0, "a specialised chain shader failed to build");
    if std::env::var("FILMCRAFT_CHAIN_SPECIALISE").as_deref() != Ok("0") {
        assert!(built > 20, "only {built} specialised chain shaders built");
    }
    let mut failures = Vec::new();
    for (((name, _), a), b) in chains.iter().zip(&interpreted).zip(&specialised) {
        let (worst, flips) = compare(a, b);
        // (the compilers fold and fuse the two shaders differently: last bits, as against the CPU)
        let tol = if name.starts_with("PQ decode") { TOL_PQ_DECODE } else { TOL };
        if worst > tol || flips > 0.0 {
            failures.push(format!("{name}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "specialised chains differ from the interpreter:\n{}", failures.join("\n"));
}

/// Each op's code is the first value of its part of the program, in order.
#[test]
fn program_codes_are_each_ops_first_value() {
    for (name, ops) in cases() {
        let (p, codes) = crate::colorops::program_and_codes(&ops).expect("program");
        assert_eq!(codes.len(), ops.len(), "{name}");
        for (op, code) in ops.iter().zip(&codes) {
            let alone = crate::colorops::program(std::slice::from_ref(op)).expect("program");
            assert_eq!(alone.first().map(|v| *v as u32), Some(*code), "{name}");
        }
        assert_eq!(crate::colorops::program(&ops), Some(p), "{name}");
    }
}

/// The specialised source: the interpreter's loop is gone, one call per op with its literal code;
/// nothing to specialise (no ops, too many, no markers) gives None.
#[test]
fn specialised_source_calls_each_op_in_order() {
    let src = include_str!("colorops.wgsl");
    let s = crate::colorops::specialise(src, &[5, 11, 5, 26]).expect("specialised");
    assert!(!s.contains("chain_op(u32(at(pc))"), "interpreter loop left in");
    let calls: Vec<&str> = s.lines().filter(|l| l.trim_start().starts_with("c = chain_op_")).collect();
    assert_eq!(
        calls,
        [
            "    c = chain_op_5(c, xy, &pc, len);",
            "    c = chain_op_11(c, xy, &pc, len);",
            "    c = chain_op_5(c, xy, &pc, len);",
            "    c = chain_op_26(c, xy, &pc, len);"
        ]
    );
    // one function per distinct code, holding that case's body
    assert_eq!(s.matches("fn chain_op_5(").count(), 1);
    assert_eq!(s.matches("fn chain_op_").count(), 3);
    assert!(s.contains("lggo_channel(c.x"));
    assert_eq!(s.matches("fn run_chain(").count(), 1);
    // an unknown code, or a code without its case, is not specialised
    assert!(crate::colorops::specialise(src, &[5, 999]).is_none());
    assert!(crate::colorops::specialise(&src.replace("case CO_CLAMP: {", "case CO_CLAMPED: {"), &[11]).is_none());
    assert!(crate::colorops::specialise(src, &[]).is_none());
    assert!(crate::colorops::specialise(src, &vec![5; crate::colorops::MAX_SPECIALISED_OPS + 1]).is_none());
    assert!(crate::colorops::specialise("fn run_chain() {}", &[5]).is_none());
    assert!(crate::colorops::specialise("// RUN_CHAIN_END x // RUN_CHAIN_BEGIN", &[5]).is_none());
}

/// Every op code specialises to WGSL that parses and validates (no GPU needed), alone and with
/// all codes in one chain.
#[test]
fn every_specialised_op_validates() {
    use wgpu::naga;
    let src = include_str!("colorops.wgsl");
    // (29: the colour warper, G4.1; 30: the HDR zone wheels, G4.2; 31: the 3D qualifier, G4.3)
    let all: Vec<u32> = (1..=26).chain([29, 30, 31]).collect();
    for codes in all.iter().map(|c| vec![*c]).chain([all.clone()]) {
        let s = crate::colorops::specialise(src, &codes).unwrap_or_else(|| panic!("code {codes:?} not specialised"));
        let module = naga::front::wgsl::parse_str(&s).unwrap_or_else(|e| panic!("{codes:?}: {}", e.emit_to_string(&s)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{codes:?}: {e:?}"));
    }
}

/// 3D qualifier chains (fork G4.3): the key as a matte view and limiting a correction (SDR and
/// HDR scaling, exclude spheres), and the despill.
fn key3d_cases() -> Vec<(&'static str, Vec<ColorOp>)> {
    use filmcraft_color::key3d::{Key3d, despill_dir, opponent};
    let sph = |c: [f32; 3], r: f32| {
        let o = opponent(c);
        [o[0], o[1], o[2], r]
    };
    let inc = [sph([0.8, 0.3, 0.2], 0.05), sph([0.6, 0.5, 0.2], 0.02), sph([0.2, 0.4, 0.9], 0.0)];
    let exc = [sph([0.7, 0.35, 0.25], 0.01)];
    let mut v = Vec::new();
    for (name, hdr, exclude) in [("3D key matte", false, false), ("3D key matte (HDR, exclude)", true, true)] {
        let k = Key3d::new(&inc, if exclude { &exc[..] } else { &[] }, 0.08, hdr);
        v.push((name, vec![ColorOp::Key3d(Arc::new(k)), ColorOp::HslCorrect { hue_shift: 0.0, sat: 1.0, temp: 0.0, tint: 0.0, show_mask: 3, hdr }]));
    }
    let o = opponent([0.8, 0.3, 0.2]);
    let dir = despill_dir([o[1], o[2]]).expect("a chroma");
    v.push(("despill", vec![ColorOp::Despill { dir, amount: 0.8 }]));
    v.push((
        "3D key limiting a correction with despill",
        vec![
            ColorOp::Stash,
            ColorOp::Key3d(Arc::new(Key3d::new(&inc, &exc, 0.05, false))),
            ColorOp::Despill { dir, amount: 1.0 },
            lggo(),
            ColorOp::MixStash { invert: false },
        ],
    ));
    v
}

/// The 3D qualifier (G4.3) on hostile pixels: NaN, ±∞, wide-gamut negatives, HDR above 1, black;
/// the matte view must be 0 where the CPU says 0 and the despill must pass non-finite pixels
/// unchanged. Prints the max absolute error.
#[test]
fn gpu_key3d_matches_cpu_on_hostile_pixels() {
    use filmcraft_color::key3d::{Key3d, despill_dir, opponent};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let mut pixels: Vec<[f32; 3]> = vec![
        [f32::NAN, 0.2, 0.2],
        [0.2, f32::INFINITY, 0.1],
        [f32::NEG_INFINITY, 0.0, 0.0],
        [-0.1, 0.4, 0.6],
        [0.4, -0.05, 0.1],
        [1.8, 0.9, 0.2],
        [6.0, 0.5, 0.1],
        [0.0, 0.0, 0.0],
        [0.5, 0.5, 0.5],
        [0.8, 0.3, 0.2],
    ];
    for i in 0..48 {
        pixels.push(filmcraft_color::warp::hsv_to_rgb(i as f32 / 48.0, 0.3 + 0.7 * (i % 4) as f32 / 3.0, 0.2 + 0.25 * (i % 5) as f32));
    }
    let (w, h) = (pixels.len() as u32, 1u32);
    let px: Vec<f32> = pixels.iter().flat_map(|p| [p[0], p[1], p[2], 1.0].map(super::fx_tests::half)).collect();
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let o = opponent([0.8, 0.3, 0.2]);
    let inc = [[o[0], o[1], o[2], 0.1]];
    let dir = despill_dir([o[1], o[2]]).expect("a chroma");
    let mut failures = Vec::new();
    for hdr in [false, true] {
        for (name, ops) in [
            (
                "matte",
                vec![
                    ColorOp::Key3d(Arc::new(Key3d::new(&inc, &[], 0.2, hdr))),
                    ColorOp::HslCorrect { hue_shift: 0.0, sat: 1.0, temp: 0.0, tint: 0.0, show_mask: 3, hdr },
                ],
            ),
            ("despill", vec![ColorOp::Despill { dir, amount: 0.7 }]),
        ] {
            let op = FxOp::ColorChain(Arc::from(ops));
            assert!(op.gpu_ok(), "{name}");
            let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
            op.apply(&mut cpu);
            let fx = LayerFx { size: (w, h), decimation: 1, ops: vec![op] };
            let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
            let mut abs = 0.0f32;
            let mut bad = 0;
            for ((a, b), input) in cpu.px.chunks(4).zip(gpu.chunks(4)).zip(&pixels) {
                if input.iter().all(|v| v.is_finite()) {
                    for (x, y) in a.iter().zip(b) {
                        abs = abs.max((x - y).abs());
                    }
                } else {
                    // a non-finite pixel: the matte view is 0, the despill passes it unchanged (the
                    // GPU's frame upload already spreads NaN across its channels, so only that it
                    // stays non-finite is compared)
                    let ok = if name == "matte" { a == b && b.iter().take(3).all(|v| *v == 0.0) } else { b.iter().take(3).any(|v| !v.is_finite()) };
                    if !ok {
                        eprintln!("non-finite pixel {input:?}: cpu {a:?} gpu {b:?}");
                        bad += 1;
                    }
                }
            }
            eprintln!("3D qualifier {name} hdr {hdr}: max abs {abs:.2e}, non-finite mismatches {bad}");
            if abs > TOL || bad > 0 {
                failures.push(format!("{name} {hdr}: abs {abs:.2e}, mismatches {bad}"));
            }
        }
    }
    assert!(failures.is_empty(), "GPU 3D qualifier differs from the CPU:\n{}", failures.join("\n"));
}
