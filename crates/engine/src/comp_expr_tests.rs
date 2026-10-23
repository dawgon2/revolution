//! `comp.setExpression` / `comp.clearExpression` (fork C4.5): storing, clearing, undo, refusals,
//! and `comp.explain` showing post-expression values and errors.

use super::*;
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
    assert!(refused(&mut s, json!({"comp": c, "node": text, "param": "text", "expression": "1"})).contains("own parameters"));
    assert!(refused(&mut s, json!({"comp": c, "node": merge, "param": "opacity", "expression": "1", "time": "soon"})).contains("ticks"));
    assert!(refused(&mut s, json!({"comp": 12345, "node": merge, "param": "opacity", "expression": "1"})).contains("no project item"));
    for junk in ["\u{0}", "🙂🙂", "node(\"Mix\").opacity", "self.opacity"] {
        // parse errors or cycles: never a panic
        let _ = s.execute("comp.setExpression", json!({"comp": c, "node": merge, "param": "opacity", "expression": junk, "allowError": true}));
    }
    let _ = s.execute("comp.explain", json!({"comp": c}));
}
