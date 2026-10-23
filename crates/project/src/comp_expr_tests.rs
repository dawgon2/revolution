//! Comp parameter expressions (C4.2): `Comp::param_at`, references, cycles, time variance,
//! serde and sanitize.

use super::*;
use crate::keyframe::Keyframe;
use filmcraft_time::TICKS_PER_SECOND;

fn secs(s: i64) -> Tick {
    Tick(TICKS_PER_SECOND * s)
}

fn with_expr(mut n: CompNode, key: &str, value: ParamValue, expr: &str) -> CompNode {
    let mut p = Param::new(value);
    p.expression = Some(expr.into());
    n.params.insert(key.into(), p);
    n
}

fn labelled(id: NodeId, kind: CompKind, label: &str) -> CompNode {
    CompNode { label: label.into(), ..CompNode::new(id, kind) }
}

/// Out (1), a Merge "Mix" (2), a Solid "Back" (3), a Transform (4).
fn comp() -> Comp {
    let mut c = Comp::new(1920, 1080, FrameRate::FPS_24, secs(10));
    c.nodes.push(labelled(2, CompKind::Merge, "Mix"));
    c.nodes.push(labelled(3, CompKind::Solid, "Back"));
    c.nodes.push(CompNode::new(4, CompKind::Transform));
    c
}

fn set(c: &mut Comp, id: NodeId, key: &str, value: ParamValue, expr: &str) {
    let n = c.node_mut(id).unwrap();
    *n = with_expr(n.clone(), key, value, expr);
}

#[test]
fn no_expression_is_the_keyframed_value() {
    let mut c = comp();
    assert_eq!(c.param_at(2, "opacity", secs(1)), (ParamValue::Float(100.0), None));
    let mut p = Param::new(ParamValue::Float(0.0));
    p.keyframes = vec![Keyframe::new(secs(0), ParamValue::Float(0.0)), Keyframe::new(secs(2), ParamValue::Float(50.0))];
    c.node_mut(2).unwrap().params.insert("opacity".into(), p);
    assert_eq!(c.param_at(2, "opacity", secs(1)), (ParamValue::Float(25.0), None));
    // an empty expression is no expression
    c.node_mut(2).unwrap().params.get_mut("opacity").unwrap().expression = Some("  ".into());
    assert_eq!(c.param_at(2, "opacity", secs(1)), (ParamValue::Float(25.0), None));
    // missing node / parameter: an error
    assert!(c.param_at(99, "opacity", secs(1)).1.is_some());
    assert!(c.param_at(2, "nope", secs(1)).1.is_some());
}

#[test]
fn expressions_drive_each_type() {
    let mut c = comp();
    set(&mut c, 2, "opacity", ParamValue::Float(100.0), "time * 10");
    assert_eq!(c.param_at(2, "opacity", secs(1)), (ParamValue::Float(10.0), None));
    assert_eq!(c.param_at(2, "opacity", secs(3)), (ParamValue::Float(30.0), None));
    // value is the keyframed value
    set(&mut c, 4, "rotation", ParamValue::Float(15.0), "value * 2 + frame");
    assert_eq!(c.param_at(4, "rotation", secs(1)), (ParamValue::Float(54.0), None));
    // a point: "auto" (NaN) reads as the frame centre
    set(&mut c, 4, "position", ParamValue::Vec2(Vec2::new(f64::NAN, f64::NAN)), "value + [time, 0]");
    assert_eq!(c.param_at(4, "position", secs(2)), (ParamValue::Vec2(Vec2::new(962.0, 540.0)), None));
    // a point given back as it was read stays auto
    let (ParamValue::Vec2(p), None) = c.param_at(4, "position", secs(0)) else { panic!("a point") };
    assert!(p.x.is_nan() && p.y.is_nan(), "{p:?}");
    set(&mut c, 4, "position", ParamValue::Vec2(Vec2::new(f64::NAN, 5.0)), "[value.x / 2, value.y]");
    assert_eq!(c.param_at(4, "position", secs(0)).0, ParamValue::Vec2(Vec2::new(480.0, 5.0)));
    // a point takes 2 parts (or one, broadcast), not 3
    set(&mut c, 4, "position", ParamValue::Vec2(Vec2::ZERO), "[1, 2, 3]");
    assert!(c.param_at(4, "position", secs(0)).1.unwrap().message.contains("2 parts"));
    set(&mut c, 4, "position", ParamValue::Vec2(Vec2::ZERO), "7");
    assert_eq!(c.param_at(4, "position", secs(0)).0, ParamValue::Vec2(Vec2::new(7.0, 7.0)));
    set(&mut c, 4, "anchor", ParamValue::Vec2(Vec2::ZERO), "[width, height] / 4");
    assert_eq!(c.param_at(4, "anchor", secs(0)).0, ParamValue::Vec2(Vec2::new(480.0, 270.0)));
    // a colour: a 3-vector keeps alpha
    set(&mut c, 3, "color", ParamValue::Color([0.0, 0.0, 0.0, 0.5]), "[1, 0.5, time / 4]");
    assert_eq!(c.param_at(3, "color", secs(1)), (ParamValue::Color([1.0, 0.5, 0.25, 0.5]), None));
    // a choice: rounded, clamped to the schema's entries
    set(&mut c, 2, "blend", ParamValue::Choice(0), "1000");
    let ParamValue::Choice(b) = c.param_at(2, "blend", secs(0)).0 else { panic!() };
    let n = params(&CompKind::Merge).into_iter().find(|d| d.id == "blend").map(|d| match d.kind {
        crate::effect::ParamKind::Choice(names) => names.len(),
        _ => 0,
    });
    assert_eq!(Some(b as usize + 1), n);
    // a bool
    set(&mut c, 4, "uniform_scale", ParamValue::Bool(true), "time > 5");
    assert_eq!(c.param_at(4, "uniform_scale", secs(1)).0, ParamValue::Bool(false));
    assert_eq!(c.param_at(4, "uniform_scale", secs(6)).0, ParamValue::Bool(true));
}

#[test]
fn references_by_label_id_and_self() {
    let mut c = comp();
    set(&mut c, 2, "opacity", ParamValue::Float(40.0), "value");
    set(&mut c, 4, "rotation", ParamValue::Float(0.0), "node(\"Mix\").opacity + 1");
    assert_eq!(c.param_at(4, "rotation", secs(0)), (ParamValue::Float(41.0), None));
    set(&mut c, 4, "rotation", ParamValue::Float(0.0), "node(\"mix\").opacity * 2");
    assert_eq!(c.param_at(4, "rotation", secs(0)).0, ParamValue::Float(80.0));
    set(&mut c, 4, "rotation", ParamValue::Float(0.0), "node(2).opacity - 1");
    assert_eq!(c.param_at(4, "rotation", secs(0)).0, ParamValue::Float(39.0));
    set(&mut c, 4, "rotation", ParamValue::Float(0.0), "node(\"Back\").color.g");
    assert_eq!(c.param_at(4, "rotation", secs(0)).0, ParamValue::Float(1.0), "the schema default colour");
    set(&mut c, 4, "scale", ParamValue::Float(50.0), "self.rotation + value");
    assert_eq!(c.param_at(4, "scale", secs(0)).0, ParamValue::Float(51.0));
    // a reference to another expression sees its result
    set(&mut c, 2, "opacity", ParamValue::Float(40.0), "time * 10");
    set(&mut c, 4, "rotation", ParamValue::Float(0.0), "node(\"Mix\").opacity");
    assert_eq!(c.param_at(4, "rotation", secs(3)).0, ParamValue::Float(30.0));
    // missing node, missing parameter, text parameter: errors, keyframed value
    for bad in ["node(\"Nope\").opacity", "node(77).opacity", "node(\"Mix\").nope", "self.nope"] {
        set(&mut c, 4, "rotation", ParamValue::Float(7.0), bad);
        let (v, e) = c.param_at(4, "rotation", secs(0));
        assert_eq!(v, ParamValue::Float(7.0), "{bad}");
        assert!(e.is_some(), "{bad}");
    }
}

#[test]
fn broken_expressions_fall_back() {
    let mut c = comp();
    for bad in ["sin(time", "1/0", "sqrt(-1)", "", "🙂", "[1,2"] {
        set(&mut c, 2, "opacity", ParamValue::Float(60.0), bad);
        let (v, e) = c.param_at(2, "opacity", secs(1));
        assert_eq!(v, ParamValue::Float(60.0), "{bad}");
        assert_eq!(e.is_some(), !bad.is_empty(), "{bad}");
    }
    set(&mut c, 2, "opacity", ParamValue::Float(60.0), "sin(time");
    let e = c.param_at(2, "opacity", secs(1)).1.unwrap();
    assert_eq!(e.to_string(), "expected `)` after `sin(time` at column 9");
    // Text / Curve / Path refuse expressions
    assert!(!expression_kind_allowed(&ParamValue::Text("x".into())));
    assert!(!expression_kind_allowed(&ParamValue::Curve(Vec::new())));
    assert!(!expression_kind_allowed(&ParamValue::Path(MaskPath::default())));
    assert!(expression_kind_allowed(&ParamValue::Vec2(Vec2::ZERO)));
    set(&mut c, 2, "label_text", ParamValue::Text("hi".into()), "1");
    let (v, e) = c.param_at(2, "label_text", secs(0));
    assert_eq!(v, ParamValue::Text("hi".into()));
    assert!(e.is_some());
}

#[test]
fn cycles_and_self_reference_are_errors() {
    let mut c = comp();
    // a → b → a
    set(&mut c, 2, "opacity", ParamValue::Float(10.0), "node(4).rotation");
    set(&mut c, 4, "rotation", ParamValue::Float(20.0), "node(\"Mix\").opacity");
    let (v, e) = c.param_at(2, "opacity", secs(0));
    assert_eq!(v, ParamValue::Float(10.0));
    assert!(e.unwrap().message.contains("cycle"));
    let (v, e) = c.param_at(4, "rotation", secs(0));
    assert_eq!(v, ParamValue::Float(20.0));
    assert!(e.is_some());
    // itself
    set(&mut c, 3, "color", ParamValue::Color([1.0; 4]), "self.color");
    let (v, e) = c.param_at(3, "color", secs(0));
    assert_eq!(v, ParamValue::Color([1.0; 4]));
    assert!(e.is_some());
    set(&mut c, 4, "scale", ParamValue::Float(5.0), "node(4).scale + 1");
    assert!(c.param_at(4, "scale", secs(0)).1.is_some());
    // time variance analysis stops on cycles
    assert!(!c.param_is_time_varying(2, "opacity"));
    assert!(!c.param_is_time_varying(3, "color"));
}

#[test]
fn deep_reference_chains_stop_at_the_cap() {
    // node i's opacity = node(i-1).opacity + 1; node 10's is 5
    let chain = |len: u64| {
        let mut c = Comp::new(64, 64, FrameRate::FPS_24, secs(1));
        c.nodes.push(with_expr(CompNode::new(10, CompKind::Merge), "opacity", ParamValue::Float(5.0), "value"));
        for i in 11..(11 + len) {
            let src = format!("node({}).opacity + 1", i - 1);
            c.nodes.push(with_expr(CompNode::new(i, CompKind::Merge), "opacity", ParamValue::Float(0.0), &src));
        }
        (c, 10 + len)
    };
    // The evaluator's stack: a chain of `levels` references, each expression `calls` nested
    // `min(…, 1e9)` around a `len`-long `+1` chain, read on a 2 MiB thread (unoptimised). The
    // heights along a reference chain add up to at most MAX_CHAIN_HEIGHT, so this never overflows:
    // too tall is an error, not a crash.
    let stacked = |levels: u64, calls: usize, len: usize| {
        let mut c = Comp::new(64, 64, FrameRate::FPS_24, secs(1));
        let wrap = |inner: String| (0..calls).fold(inner, |a, _| format!("min({a}, 1e9)"));
        c.nodes.push(with_expr(CompNode::new(10, CompKind::Merge), "opacity", ParamValue::Float(5.0), &wrap("value".to_string() + &"+0".repeat(len))));
        for i in 11..(11 + levels) {
            let src = wrap(format!("node({}).opacity", i - 1) + &"+1".repeat(len));
            assert!(crate::expr::parse(&src).is_ok(), "each expression is within the caps");
            c.nodes.push(with_expr(CompNode::new(i, CompKind::Merge), "opacity", ParamValue::Float(0.0), &src));
        }
        let run = move || c.param_at(10 + levels, "opacity", secs(0));
        std::thread::Builder::new().stack_size(2 << 20).spawn(run).unwrap().join().unwrap()
    };
    // 64 nested calls around a 185-long chain at each of 8 levels: too deep together
    let (v, e) = stacked(8, 64, 185);
    assert_eq!(v, ParamValue::Float(0.0));
    assert!(e.unwrap().message.contains("too deep together"));
    let (v, e) = stacked(8, 0, 250);
    assert_eq!(v, ParamValue::Float(0.0));
    assert!(e.is_some());
    // the tallest single expression evaluates, and so does a deep chain of shorter ones
    assert_eq!(stacked(0, 64, 185), (ParamValue::Float(5.0), None));
    assert_eq!(stacked(2, 64, 185), (ParamValue::Float(5.0 + 2.0 * 185.0), None));
    assert_eq!(stacked(3, 64, 185).1.map(|e| e.message.contains("too deep together")), Some(true));
    assert_eq!(stacked(8, 20, 60), (ParamValue::Float(5.0 + 8.0 * 60.0), None));

    let (c, top) = chain(7);
    assert_eq!(c.param_at(top, "opacity", secs(0)), (ParamValue::Float(12.0), None));
    let (c, top) = chain(30);
    let (v, e) = c.param_at(top, "opacity", secs(0));
    assert_eq!(v, ParamValue::Float(0.0));
    let e = e.unwrap();
    assert!(e.message.contains("nested"));
    // a nested error names where it came from, with one column at most (the top expression's)
    assert!(e.message.starts_with("`node "), "{}", e.message);
    assert!(e.to_string().matches("at column").count() <= 1, "{e}");
    assert!(!c.param_is_time_varying(top, "opacity"));
}

/// Each node reads the previous one many times: without memoising this is exponential.
#[test]
fn reference_fan_out_is_bounded() {
    let mut c = Comp::new(64, 64, FrameRate::FPS_24, secs(1));
    c.nodes.push(with_expr(CompNode::new(10, CompKind::Merge), "opacity", ParamValue::Float(1.0), "time"));
    for i in 11..19u64 {
        let one = format!("node({}).opacity", i - 1);
        let src = vec![one; 40].join(" + ");
        c.nodes.push(with_expr(CompNode::new(i, CompKind::Merge), "opacity", ParamValue::Float(0.0), &src));
    }
    let start = std::time::Instant::now();
    let _ = c.param_at(18, "opacity", secs(0));
    assert!(c.param_is_time_varying(18, "opacity"));
    let _ = c.node_expressions_static(18);
    // one frame shared by every node: each parameter evaluated once
    let all = c.nodes_with_expressions_at(secs(0));
    assert_eq!(all.len(), 9);
    assert_eq!(all.get(&11).and_then(|n| n.value_at("opacity", Tick::ZERO)), Some(ParamValue::Float(0.0)));
    assert_eq!(all.get(&12).and_then(|n| n.value_at("opacity", Tick::ZERO)), Some(ParamValue::Float(0.0)));
    let all = c.nodes_with_expressions_at(Tick(TICKS_PER_SECOND / 4));
    assert_eq!(all.get(&12).and_then(|n| n.value_at("opacity", Tick::ZERO)), Some(ParamValue::Float(0.25 * 40.0 * 40.0)));
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn time_variance() {
    let mut c = comp();
    set(&mut c, 2, "opacity", ParamValue::Float(1.0), "time * 10");
    assert!(c.param_is_time_varying(2, "opacity"));
    for (src, varying) in [
        ("5", false),
        ("frame", true),
        ("wiggle(1, 2)", true),
        ("noise(3)", false),
        ("noise(time)", true),
        ("random(4)", false),
        ("value * 2", false),
        ("sin(time", false),
        ("width / 2", false),
    ] {
        set(&mut c, 4, "rotation", ParamValue::Float(0.0), src);
        assert_eq!(c.param_is_time_varying(4, "rotation"), varying, "{src}");
    }
    // through references
    set(&mut c, 4, "rotation", ParamValue::Float(0.0), "node(\"Mix\").opacity");
    assert!(c.param_is_time_varying(4, "rotation"));
    assert!(!c.node_expressions_static(4), "reads another node");
    set(&mut c, 4, "rotation", ParamValue::Float(0.0), "self.scale");
    assert!(!c.param_is_time_varying(4, "rotation"));
    assert!(c.node_expressions_static(4));
    // value of an animated parameter
    let p = c.node_mut(4).unwrap().params.get_mut("rotation").unwrap();
    p.keyframes = vec![Keyframe::new(secs(0), ParamValue::Float(0.0)), Keyframe::new(secs(1), ParamValue::Float(9.0))];
    p.expression = Some("value".into());
    assert!(c.param_is_time_varying(4, "rotation"));
    assert!(!c.node_expressions_static(4));
    assert!(c.node_expressions_static(3), "no expressions");
    // the comp's rate and size aren't in a node's own state
    for src in ["width / 2", "height", "fps * 2"] {
        set(&mut c, 3, "color", ParamValue::Color([1.0; 4]), src);
        assert!(!c.param_is_time_varying(3, "color"), "{src}");
        assert!(!c.node_expressions_static(3), "{src}");
    }
    assert!(!c.param_is_time_varying(3, "color"));
}

#[test]
fn node_with_expressions_at_bakes_values() {
    let mut c = comp();
    assert!(c.node_with_expressions_at(2, secs(1)).is_none());
    set(&mut c, 2, "opacity", ParamValue::Float(100.0), "time * 10");
    let n = c.node_with_expressions_at(2, secs(2)).unwrap();
    assert_eq!(n.params.get("opacity"), Some(&Param::new(ParamValue::Float(20.0))));
    assert_eq!(n.value_at("opacity", secs(9)), Some(ParamValue::Float(20.0)));
    assert!(c.node_with_expressions_at(99, secs(0)).is_none());
}

#[test]
fn serde_old_files_and_round_trip() {
    // a parameter saved before expressions loads with none
    let p: Param = serde_json::from_str(r#"{"value":{"Float":2.0}}"#).unwrap();
    assert_eq!(p.expression, None);
    // saving without expressions writes exactly what it wrote before (no `expression` key)
    let c = comp();
    let mut plain = c.clone();
    plain.node_mut(2).unwrap().params.insert("opacity".into(), Param::new(ParamValue::Float(50.0)));
    let json = serde_json::to_string(&plain).unwrap();
    assert!(!json.contains("expression"), "{json}");
    assert_eq!(json, serde_json::to_string(&serde_json::from_str::<Comp>(&json).unwrap()).unwrap());
    assert!(json.contains(r#""opacity":{"value":{"Float":50.0}}"#), "{json}");
    // with one: round-trips
    let mut with = c;
    set(&mut with, 2, "opacity", ParamValue::Float(50.0), "time * 10");
    let json = serde_json::to_string(&with).unwrap();
    assert!(json.contains(r#""expression":"time * 10""#), "{json}");
    let back: Comp = serde_json::from_str(&json).unwrap();
    assert_eq!(back, with);
}

#[test]
fn sanitize_drops_over_long_expressions() {
    let mut c = comp();
    set(&mut c, 2, "opacity", ParamValue::Float(50.0), &"1+".repeat(3_000));
    set(&mut c, 4, "rotation", ParamValue::Float(0.0), "time");
    c.sanitize();
    assert_eq!(c.node(2).unwrap().params.get("opacity").unwrap().expression, None);
    assert_eq!(c.node(4).unwrap().params.get("rotation").unwrap().expression.as_deref(), Some("time"));
    // a hostile file with a huge expression loads and is brought within the caps
    let huge = "9".repeat(100_000);
    let json = format!(
        r#"{{"width":4,"height":4,"nodes":[{{"id":2,"kind":{{"type":"merge"}},"params":{{"opacity":{{"value":{{"Float":1.0}},"expression":"{huge}"}}}}}}]}}"#
    );
    let mut c: Comp = serde_json::from_str(&json).unwrap();
    c.sanitize();
    assert_eq!(c.node(2).unwrap().params.get("opacity").unwrap().expression, None);
}
