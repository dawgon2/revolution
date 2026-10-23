//! `comp.setExpression` / `comp.clearExpression` (fork C4.5): storing, clearing, undo, refusals,
//! and `comp.explain` showing post-expression values and errors.

use super::*;
use filmcraft_project::comp::CompKind;
use filmcraft_project::{ItemKind, ParamValue};
use filmcraft_time::TICKS_PER_SECOND;
use serde_json::json;

fn setup() -> (Session, u64, u64, u64) {
    let mut s = Session::default();
    let c = s.execute("comp.new", json!({"name": "E", "fps": 24})).unwrap()["comp"].as_u64().unwrap();
    let merge = s.execute("comp.addNode", json!({"comp": c, "kind": "merge", "label": "Mix"})).unwrap()["node"].as_u64().unwrap();
    let text = s.execute("comp.addNode", json!({"comp": c, "kind": "text"})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": merge, "to": 1})).unwrap();
    (s, c, merge, text)
}

fn comp(s: &Session, c: u64) -> Arc<filmcraft_project::comp::Comp> {
    match &s.project.item(filmcraft_project::ItemId(c)).unwrap().kind {
        ItemKind::Comp(x) => Arc::clone(x),
        _ => panic!("not a comp"),
    }
}

fn refused(s: &mut Session, params: serde_json::Value) -> String {
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let e = s.execute("comp.setExpression", params).expect_err("refused").to_string();
    assert!(Arc::ptr_eq(&before, &s.project));
    assert_eq!(s.history.undo.len(), undo);
    e
}

#[test]
fn set_evaluate_clear_and_undo() {
    let (mut s, c, merge, _) = setup();
    let undo = s.history.undo.len();
    let r = s
        .execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": "time * 10", "time": 2 * TICKS_PER_SECOND}))
        .unwrap();
    assert_eq!(r["stored"], true);
    assert_eq!(r["value"], json!({"Float": 20.0}));
    assert_eq!(r["error"], serde_json::Value::Null);
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    let cc = comp(&s, c);
    assert_eq!(cc.node(merge).unwrap().params["opacity"].expression.as_deref(), Some("time * 10"));
    assert_eq!(cc.param_at(merge, "opacity", Tick(TICKS_PER_SECOND)).0, ParamValue::Float(10.0));
    // comp.explain shows the value after the expression
    let e = s.execute("comp.explain", json!({"comp": c, "time": 3 * TICKS_PER_SECOND})).unwrap();
    assert_eq!(e["expressions"][0]["param"], "opacity");
    assert_eq!(e["expressions"][0]["value"], json!({"Float": 30.0}));
    let line = e["lines"].as_array().unwrap().iter().find(|l| l["node"] == "Mix" || l["name"] == "Mix").cloned().unwrap_or_default();
    let all = e.to_string();
    assert!(all.contains("Opacity 30.0% from an expression"), "{all} {line}");
    // undo removes it; redo brings it back
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(comp(&s, c).node(merge).unwrap().params.get("opacity").and_then(|p| p.expression.clone()), None);
    s.execute("edit.redo", json!({})).unwrap();
    assert!(comp(&s, c).node(merge).unwrap().params["opacity"].expression.is_some());
    // empty clears (one step); clearing again is a no-op
    s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": ""})).unwrap();
    assert_eq!(comp(&s, c).node(merge).unwrap().params["opacity"].expression, None);
    let undo = s.history.undo.len();
    s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": "  "})).unwrap();
    assert_eq!(s.history.undo.len(), undo);
    // clearExpression
    s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "blend", "expression": "1"})).unwrap();
    s.execute("comp.clearExpression", json!({"comp": c, "node": merge, "param": "blend"})).unwrap();
    assert_eq!(comp(&s, c).node(merge).unwrap().params["blend"].expression, None);
    assert!(s.execute("comp.clearExpression", json!({"comp": c, "node": merge, "param": "blend"})).is_err());
}

#[test]
fn parse_errors_are_returned_and_not_stored_unless_allowed() {
    let (mut s, c, merge, _) = setup();
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": "sin("})).unwrap();
    assert_eq!(r["stored"], false);
    assert!(r["error"]["message"].as_str().unwrap().contains("ends too early"), "{r}");
    assert!(r["error"]["column"].as_u64().is_some());
    assert!(Arc::ptr_eq(&before, &s.project));
    assert_eq!(s.history.undo.len(), undo);
    // allowError stores it; the parameter renders as its keyframed value and explain says why
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": "sin(", "allowError": true})).unwrap();
    assert_eq!(r["stored"], true);
    assert_eq!(r["value"], json!({"Float": 100.0}));
    assert!(r["error"].is_object());
    let e = s.execute("comp.explain", json!({"comp": c})).unwrap();
    assert!(e["expressions"][0]["error"]["text"].as_str().unwrap().contains("column"));
    // an evaluation error is stored (it parses) and reported
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": "1/0"})).unwrap();
    assert_eq!(r["stored"], true);
    assert!(r["error"]["message"].as_str().unwrap().contains("division by zero"));
}

#[test]
fn hostile_and_unsupported_params_are_refused() {
    let (mut s, c, merge, text) = setup();
    assert!(refused(&mut s, json!({"comp": c, "node": 99, "param": "opacity", "expression": "1"})).contains("no node"));
    assert!(refused(&mut s, json!({"comp": c, "node": merge, "param": "nope", "expression": "1"})).contains("no parameter"));
    assert!(refused(&mut s, json!({"comp": c, "node": merge, "param": "opacity"})).contains("expression"));
    assert!(refused(&mut s, json!({"comp": c, "node": merge, "param": "opacity", "expression": 5})).contains("expression"));
    assert!(refused(&mut s, json!({"comp": c, "node": merge, "param": "opacity", "expression": "1+".repeat(3000)})).contains("longer"));
    assert!(refused(&mut s, json!({"comp": c, "node": text, "param": "text", "expression": "1"})).contains("don't take expressions"));
    assert!(refused(&mut s, json!({"comp": c, "node": merge, "param": "opacity", "expression": "1", "time": "soon"})).contains("ticks"));
    assert!(refused(&mut s, json!({"comp": 12345, "node": merge, "param": "opacity", "expression": "1"})).contains("no project item"));
    for junk in ["\u{0}", "🙂🙂", "node(\"Mix\").opacity", "self.opacity"] {
        // parse errors or cycles: never a panic
        let _ = s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": junk, "allowError": true}));
    }
    let _ = s.execute("comp.explain", json!({"comp": c}));
}

/// A comp: Solid (reddish) → Color → Media Out. (session, comp, solid, color)
fn graded() -> (Session, u64, u64, u64) {
    let mut s = Session::default();
    let c = s.execute("comp.new", json!({"name": "G", "fps": 24, "width": 16, "height": 8})).unwrap()["comp"].as_u64().unwrap();
    let solid = s.execute("comp.addNode", json!({"comp": c, "kind": "solid"})).unwrap()["node"].as_u64().unwrap();
    let color = s.execute("comp.addNode", json!({"comp": c, "kind": "color", "label": "Grade"})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.setParam", json!({"comp": c, "node": solid, "param": "color", "value": [0.8, 0.2, 0.2, 1.0]})).unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": solid, "to": color})).unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": color, "to": 1})).unwrap();
    (s, c, solid, color)
}

fn pixels(c: &filmcraft_project::comp::Comp, t: Tick) -> Vec<f32> {
    let ctx = filmcraft_render::comp::CompCtx { t, size: (16, 8), project: None, high_quality: false };
    filmcraft_render::comp::render_cpu(c, ctx, &filmcraft_render::comp::NoSources).px
}

/// LO.4: a Color node's grade numbers and wheels take expressions: the value at a time, the
/// render follows it (the same pixels as the value set by hand), errors refused unless
/// allowError, curves refused, comp.explain lists it, undo / clear work.
#[test]
fn color_node_grade_settings_take_expressions() {
    let (mut s, c, _, color) = graded();
    let two = 2 * TICKS_PER_SECOND;
    let undo = s.history.undo.len();
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": color, "param": "saturation", "expression": "time < 1 ? 1 : 0", "time": two})).unwrap();
    assert_eq!(r["stored"], true);
    assert_eq!(r["value"], json!({"Float": 0.0}));
    assert_eq!(s.history.undo.len(), undo + 1);
    let Some(CompKind::Color { node: g }) = comp(&s, c).node(color).map(|n| n.kind.clone()) else { panic!("color") };
    assert_eq!(g.params["saturation"].expression.as_deref(), Some("time < 1 ? 1 : 0"));
    assert!(comp(&s, c).node(color).unwrap().params.is_empty(), "stored with the grade, not the node");
    // the render follows the expression: coloured at 0 s, grey at 2 s, as if set by hand
    let early = pixels(&comp(&s, c), Tick(0));
    let late = pixels(&comp(&s, c), Tick(two));
    assert!(early[0] > early[1] + 0.1, "{:?}", &early[..4]);
    assert!((late[0] - late[1]).abs() < 1e-3 && (late[1] - late[2]).abs() < 1e-3, "{:?}", &late[..4]);
    let mut by_hand = (*comp(&s, c)).clone();
    if let Some(CompKind::Color { node: g }) = by_hand.node_mut(color).map(|n| &mut n.kind) {
        g.params.insert("saturation".into(), filmcraft_project::Param::new(ParamValue::Float(0.0)));
    }
    assert_eq!(late, pixels(&by_hand, Tick(two)), "the same pixels as the value set by hand");
    // a wheel; another node reads a grade setting by name
    let r = s
        .execute("comp.setExpression", json!({"comp": c, "node": color, "param": "gain", "expression": "[0.5, 0, 0, time]", "time": TICKS_PER_SECOND}))
        .unwrap();
    assert_eq!(r["value"], json!({"Color": [0.5, 0.0, 0.0, 1.0]}));
    // errors: refused unless allowError (then the keyframed value renders)
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": color, "param": "contrast", "expression": "sin("})).unwrap();
    assert_eq!(r["stored"], false);
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": color, "param": "contrast", "expression": "sin(", "allowError": true})).unwrap();
    assert_eq!((r["stored"].clone(), r["value"].clone()), (json!(true), json!({"Float": 1.0})));
    assert!(r["error"].is_object());
    assert!(refused(&mut s, json!({"comp": c, "node": color, "param": "curve.master", "expression": "1"})).contains("curve"));
    assert!(refused(&mut s, json!({"comp": c, "node": color, "param": "tnr.amount", "expression": "1"})).contains("no parameter"));
    // comp.explain lists it with its value now
    let e = s.execute("comp.explain", json!({"comp": c, "time": two})).unwrap();
    let line = e["expressions"].as_array().unwrap().iter().find(|x| x["param"] == "saturation").cloned().unwrap();
    assert_eq!(line["value"], json!({"Float": 0.0}));
    // clear: the grade setting keeps its value, the expression is gone
    s.execute("comp.clearExpression", json!({"comp": c, "node": color, "param": "saturation"})).unwrap();
    assert_eq!(comp(&s, c).node(color).unwrap().expression("saturation"), None);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(comp(&s, c).node(color).unwrap().expression("saturation").is_some());
}

/// LO.4: a Text node's layer settings take expressions (numbers, menus kept within their
/// choices); the source text doesn't; a time expression makes the node non-static (the static
/// cache can't keep its picture), a constant one doesn't.
#[test]
fn text_layer_settings_take_expressions() {
    let (mut s, c, _, text) = setup();
    let r =
        s.execute("comp.setExpression", json!({"comp": c, "node": text, "param": "size", "expression": "50 + time * 10", "time": TICKS_PER_SECOND})).unwrap();
    assert_eq!(r["value"], json!({"Float": 60.0}));
    let Some(CompKind::Text { layer }) = comp(&s, c).node(text).map(|n| n.kind.clone()) else { panic!("text") };
    assert_eq!(layer.params["size"].expression.as_deref(), Some("50 + time * 10"));
    assert!(!comp(&s, c).node_expressions_static(text), "a time expression changes the picture over time");
    assert_eq!(
        comp(&s, c).node_with_expressions_at(text, Tick(2 * TICKS_PER_SECOND)).map(|n| n.value_at("size", Tick(0))),
        Some(Some(ParamValue::Float(70.0)))
    );
    s.execute("comp.setExpression", json!({"comp": c, "node": text, "param": "size", "expression": "72"})).unwrap();
    assert!(comp(&s, c).node_expressions_static(text), "a constant expression stays cacheable");
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": text, "param": "align", "expression": "99"})).unwrap();
    assert!(matches!(serde_json::from_value::<ParamValue>(r["value"].clone()), Ok(ParamValue::Choice(n)) if n < 10), "{r}");
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": text, "param": "tracking", "expression": "nope("})).unwrap();
    assert_eq!(r["stored"], false);
    assert!(refused(&mut s, json!({"comp": c, "node": text, "param": "font", "expression": "1"})).contains("don't take expressions"));
    // a reference from another node to a text setting
    let (_, merge) = (0, s.execute("comp.addNode", json!({"comp": c, "kind": "merge"})).unwrap()["node"].as_u64().unwrap());
    s.execute("comp.labelNode", json!({"comp": c, "node": text, "label": "Title"})).unwrap();
    let r = s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": "node(\"Title\").size"})).unwrap();
    assert_eq!(r["value"], json!({"Float": 72.0}));
}

/// LO.4: Tracker settings are not parameters an expression can drive (the analysed tracks are
/// read at several times a frame): refused in plain words; old comps without expressions load
/// and save unchanged.
#[test]
fn trackers_refuse_and_old_comps_round_trip() {
    let (mut s, c, _, color) = graded();
    let tracker = s.execute("comp.addNode", json!({"comp": c, "kind": "tracker"})).unwrap()["node"].as_u64().unwrap();
    for key in ["operation", "smoothRadius", "track", "point0"] {
        assert!(refused(&mut s, json!({"comp": c, "node": tracker, "param": key, "expression": "1"})).contains("no parameter"), "{key}");
    }
    s.execute("comp.setParam", json!({"comp": c, "node": color, "param": "saturation", "value": 0.5})).unwrap();
    let before = comp(&s, c);
    let text = serde_json::to_string(&*before).unwrap();
    assert!(!text.contains("expression"), "{text}");
    let back: filmcraft_project::comp::Comp = serde_json::from_str(&text).unwrap();
    assert_eq!(back, *before);
}
