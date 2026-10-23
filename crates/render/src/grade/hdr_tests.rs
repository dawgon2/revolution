//! HDR zone wheels (fork G4.2) in a compiled grade: identity emits nothing, the op sits after the
//! primaries on working linear light, a hand-computed pixel, hostile values stay finite.

use super::*;
use filmcraft_color::ops::{ZoneWheels, apply_chain};
use filmcraft_project::{Param, ParamValue};

fn node(params: &[(&str, ParamValue)]) -> GradeNode {
    let mut n = GradeNode::new(1);
    for (k, v) in params {
        n.params.insert((*k).into(), Param::new(v.clone()));
    }
    n
}

fn grade(n: GradeNode) -> Grade {
    Grade { nodes: vec![n], ..Default::default() }
}

fn zone_op(ops: &[ColorOp]) -> Option<&ZoneWheels> {
    ops.iter().find_map(|o| if let ColorOp::ZoneWheels(z) = o { Some(&**z) } else { None })
}

#[test]
fn identity_zone_wheels_compile_to_nothing() {
    let n = node(&[
        ("hdr.global.exposure", ParamValue::Float(0.0)),
        ("hdr.shadow.sat", ParamValue::Float(1.0)),
        ("hdr.light.range", ParamValue::Float(3.0)),
        ("hdr.black.falloff", ParamValue::Float(5.0)),
        ("hdr.dark.balance", ParamValue::Color([0.3, 0.3, 0.3, 1.0])),
    ]);
    for w in WorkingSpace::ALL {
        assert!(compile(&grade(n.clone()), Tick(0), w).is_empty(), "{w:?}");
        assert!(!node_changes(&n, Tick(0), None));
    }
}

#[test]
fn zone_wheels_follow_the_primaries_on_linear_light() {
    let n = node(&[
        ("saturation", ParamValue::Float(0.8)),
        ("curve.master", ParamValue::Curve(vec![[0.0, 0.1], [1.0, 1.0]])),
        ("hdr.highlight.exposure", ParamValue::Float(-1.0)),
    ]);
    assert!(node_changes(&n, Tick(0), None));
    let ops = compile(&grade(n), Tick(0), WorkingSpace::DaVinciWide);
    let i = ops.iter().position(|o| matches!(o, ColorOp::ZoneWheels(_))).unwrap();
    assert!(matches!(ops[i - 2], ColorOp::Saturation { .. }), "{ops:?}");
    assert_eq!(ops[i - 1], ColorOp::Decode(Tf::DaVinciIntermediate));
    assert_eq!(ops[i + 1], ColorOp::Encode(Tf::DaVinciIntermediate));
    assert!(matches!(ops[i + 2], ColorOp::Curve { .. }), "{ops:?}");
    let z = zone_op(&ops).unwrap();
    assert!((z.weights.iter().sum::<f32>() - 1.0).abs() < 1e-5, "{:?}", z.weights);
    assert_eq!((z.pivot, z.zones[4].edge, z.zones[4].adjust.exposure), (HDR_ZONE_PIVOT, 2.5, -1.0));
}

/// DWG / DI, default edges (Light 1, Highlight 2.5; falloff 1). A grey 3 stops over mid grey
/// (linear 1.44) is fully in Light and Highlight and, with Specular's edge moved to 3, half in
/// Specular: −1 stop from Highlight and +2 × ½ from Specular → 0 stops overall. Mid grey is in no
/// zone; a grey 2 stops under is fully in Shadow (edge −1). Balance keeps the luminance.
#[test]
fn compiled_zone_wheels_match_a_hand_computed_pixel() {
    let n = node(&[
        ("hdr.highlight.exposure", ParamValue::Float(-1.0)),
        ("hdr.specular.exposure", ParamValue::Float(2.0)),
        ("hdr.specular.range", ParamValue::Float(3.0)),
        ("hdr.shadow.exposure", ParamValue::Float(3.0)),
    ]);
    let ops = compile(&grade(n), Tick(0), WorkingSpace::DaVinciWide);
    let out = apply_chain(&ops, [1.44; 3]);
    assert!(out.iter().all(|v| (v - 1.44).abs() < 2e-4), "{out:?}");
    let out = apply_chain(&ops, [0.18; 3]);
    assert!(out.iter().all(|v| (v - 0.18).abs() < 1e-5), "{out:?}");
    let out = apply_chain(&ops, [0.045; 3]);
    assert!(out.iter().all(|v| (v - 0.36).abs() < 1e-4), "{out:?}");
    let tint = node(&[("hdr.global.balance", ParamValue::Color([0.3, -0.1, -0.2, 0.0]))]);
    let ops = compile(&grade(tint), Tick(0), WorkingSpace::DaVinciWide);
    let w = filmcraft_color::Gamut::DaVinciWide.luma().map(|v| v as f32);
    let c = [0.3f32, 0.25, 0.2];
    let o = apply_chain(&ops, c);
    let y = |p: [f32; 3]| w[0] * p[0] + w[1] * p[1] + w[2] * p[2];
    assert!((y(o) - y(c)).abs() < 1e-4 && o[0] > c[0], "{c:?} → {o:?}");
}

#[test]
fn hostile_zone_wheels_compile_finite() {
    let n = node(&[
        ("hdr.global.exposure", ParamValue::Float(f64::MAX)),
        ("hdr.global.sat", ParamValue::Float(-1e300)),
        ("hdr.black.range", ParamValue::Float(f64::NAN)),
        ("hdr.black.falloff", ParamValue::Float(-5.0)),
        ("hdr.light.balance", ParamValue::Color([f32::MAX, f32::NAN, -f32::MAX, f32::INFINITY])),
        ("hdr.specular.sat", ParamValue::Float(1e300)),
        ("hdr.dark.exposure", ParamValue::Text("loud".into())),
    ]);
    for w in WorkingSpace::ALL {
        let ops = compile(&grade(n.clone()), Tick(0), w);
        assert!(!ops.is_empty() && ops.iter().all(ColorOp::is_finite), "{w:?}");
        for c in [[0.0f32; 3], [0.5, 0.2, 0.1], [100.0; 3], [-0.1, 0.0, 2.0]] {
            let o = apply_chain(&ops, c);
            assert!(o.iter().all(|v| v.is_finite()), "{w:?} {c:?}: {o:?}");
        }
    }
}

/// Review fix: in SDR an HDR wheel doesn't clip the primaries' overshoot (gain ×2 pushes a grey
/// past white); a later node bringing it back down still sees it, as without the wheel.
#[test]
fn sdr_zone_wheels_keep_the_primaries_overshoot() {
    let a = node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 1.0])), ("hdr.global.sat", ParamValue::Float(0.9))]);
    let mut b = node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, -0.5]))]);
    b.id = 2;
    let with = compile(&Grade { nodes: vec![a.clone(), b.clone()], ..Default::default() }, Tick(0), WorkingSpace::Rec709);
    assert!(zone_op(&with).is_some());
    let mut plain = a;
    plain.params.remove("hdr.global.sat");
    let without = compile(&Grade { nodes: vec![plain, b], ..Default::default() }, Tick(0), WorkingSpace::Rec709);
    let (o, p) = (apply_chain(&with, [0.6; 3]), apply_chain(&without, [0.6; 3]));
    assert!(o.iter().zip(p).all(|(x, y)| (x - y).abs() < 1e-4) && (o[0] - 0.6).abs() < 1e-3, "{o:?} vs {p:?}");
}

/// Review fix: the wheels in PQ and HLG grades decode, act and encode back finitely; a highlight
/// exposure cut lowers a bright grey and leaves mid grey.
#[test]
fn zone_wheels_in_pq_and_hlg_grades() {
    let n = node(&[("hdr.highlight.exposure", ParamValue::Float(-1.0))]);
    for w in [WorkingSpace::Rec2100Pq, WorkingSpace::Rec2100Hlg] {
        let ops = compile(&grade(n.clone()), Tick(0), w);
        assert!(zone_op(&ops).is_some(), "{w:?}");
        let bright = apply_chain(&ops, [2.0; 3]);
        assert!(bright.iter().all(|v| v.is_finite() && (v - 1.0).abs() < 0.02), "{w:?}: {bright:?}");
        let grey = apply_chain(&ops, [0.18; 3]);
        assert!(grey.iter().all(|v| (v - 0.18).abs() < 2e-3), "{w:?}: {grey:?}");
    }
}
