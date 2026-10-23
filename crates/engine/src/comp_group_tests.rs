//! `comp.group` / `comp.ungroup` / `comp.group.expose` / `comp.group.unexpose` (fork C4.3):
//! one undo step each, refusals change nothing, exposed parameters set through `comp.setParam`,
//! and a macro survives save / load with the same picture.

use super::*;
use filmcraft_project::ItemKind;
use filmcraft_project::comp::{Comp, CompKind};
use filmcraft_render::comp::{CompCtx, NoSources, render_cpu};
use serde_json::json;

fn session() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn add(s: &mut Session, comp: u64, params: serde_json::Value) -> u64 {
    let mut p = params;
    p["comp"] = json!(comp);
    s.execute("comp.addNode", p).unwrap()["node"].as_u64().unwrap()
}

fn connect(s: &mut Session, comp: u64, from: u64, to: u64, port: u8) {
    s.execute("comp.connect", json!({"comp": comp, "from": from, "to": to, "toPort": port})).unwrap();
}

fn comp_of(s: &Session, c: u64) -> Comp {
    match &s.project.item(ItemId(c)).unwrap().kind {
        ItemKind::Comp(c) => (**c).clone(),
        _ => panic!("not a comp"),
    }
}

fn picture(c: &Comp) -> Vec<f32> {
    render_cpu(c, CompCtx { t: filmcraft_time::Tick(0), size: (64, 36), project: None, high_quality: false }, &NoSources).px
}

/// The command fails and changes nothing (project and undo history).
fn refused(s: &mut Session, command: &str, params: serde_json::Value) -> String {
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let e = s.execute(command, params).expect_err(command).to_string();
    assert!(Arc::ptr_eq(&before, &s.project), "{command} left the project alone");
    assert_eq!(s.history.undo.len(), undo, "{command} added no undo step");
    e
}

/// A 64×36 comp: Gradient → Blur → Transform → Out. (comp, [gradient, blur, transform])
fn chain(s: &mut Session) -> (u64, [u64; 3]) {
    let c = s.execute("comp.new", json!({"name": "G", "width": 64, "height": 36})).unwrap()["comp"].as_u64().unwrap();
    let g = add(s, c, json!({"kind": "gradient"}));
    let b = add(s, c, json!({"kind": "blur"}));
    let t = add(s, c, json!({"kind": "transform"}));
    connect(s, c, g, b, 0);
    connect(s, c, b, t, 0);
    connect(s, c, t, 1, 0);
    s.execute("comp.setParam", json!({"comp": c, "node": g, "param": "shape", "value": 1})).unwrap();
    s.execute("comp.setParam", json!({"comp": c, "node": b, "param": "blurriness", "value": 5.0})).unwrap();
    s.execute("comp.setParam", json!({"comp": c, "node": t, "param": "rotation", "value": 20.0})).unwrap();
    (c, [g, b, t])
}

#[test]
fn group_and_ungroup_are_one_undo_step_each_and_keep_the_picture() {
    let mut s = session();
    let (c, [_, b, t]) = chain(&mut s);
    let flat = comp_of(&s, c);
    let want = picture(&flat);
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let g = s.execute("comp.group", json!({"comp": c, "nodes": [b, t], "label": "Soft turn"})).unwrap()["node"].as_u64().unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    let grouped = comp_of(&s, c);
    assert!(matches!(grouped.node(g).map(|n| &n.kind), Some(CompKind::Group { .. })));
    assert_eq!(grouped.node(g).unwrap().label, "Soft turn");
    assert_eq!(picture(&grouped), want);
    // comp.get shows the group with its inside
    let got = s.execute("comp.get", json!({"comp": c})).unwrap();
    assert_eq!(got["error"], serde_json::Value::Null);
    assert!(got.to_string().contains("\"type\":\"group\""));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    s.execute("edit.redo", json!({})).unwrap();
    let ids = s.execute("comp.ungroup", json!({"comp": c, "node": g})).unwrap()["nodes"].clone();
    assert_eq!(ids, json!([b, t]));
    let back = comp_of(&s, c);
    assert_eq!(picture(&back), want);
    let mut a = back.nodes.clone();
    a.sort_by_key(|n| n.id);
    let mut e = flat.nodes.clone();
    e.sort_by_key(|n| n.id);
    assert_eq!(a, e);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(matches!(comp_of(&s, c).node(g).map(|n| &n.kind), Some(CompKind::Group { .. })));
}

#[test]
fn group_refusals_change_nothing() {
    let mut s = session();
    let (c, [g, b, t]) = chain(&mut s);
    assert!(refused(&mut s, "comp.group", json!({"comp": c, "nodes": []})).contains("select"));
    assert!(refused(&mut s, "comp.group", json!({"comp": c})).contains("need `nodes`"));
    assert!(refused(&mut s, "comp.group", json!({"comp": c, "nodes": ["x"]})).contains("node ids"));
    assert!(refused(&mut s, "comp.group", json!({"comp": c, "nodes": [t, 1]})).contains("Media Out"));
    assert!(refused(&mut s, "comp.group", json!({"comp": c, "nodes": [999]})).contains("no node 999"));
    let lone = add(&mut s, c, json!({"kind": "solid"}));
    assert!(refused(&mut s, "comp.group", json!({"comp": c, "nodes": [lone]})).contains("don't feed anything"));
    assert!(refused(&mut s, "comp.group", json!({"comp": c, "nodes": [g, lone, b]})).contains("doesn't lead"));
    assert!(refused(&mut s, "comp.ungroup", json!({"comp": c, "node": b})).contains("not a group"));
    assert!(refused(&mut s, "comp.ungroup", json!({"comp": c})).contains("need `node`"));
    assert!(refused(&mut s, "comp.group.expose", json!({"comp": c, "node": b, "inner": g, "param": "x"})).contains("not a group"));
    let big: Vec<u64> = (0..10_000).collect();
    assert!(refused(&mut s, "comp.group", json!({"comp": c, "nodes": big})).contains("at most"));
}

#[test]
fn exposed_parameters_are_set_with_set_param_and_survive_save_and_load() {
    let mut s = session();
    let (c, [_, b, t]) = chain(&mut s);
    let g = s.execute("comp.group", json!({"comp": c, "nodes": [b, t]})).unwrap()["node"].as_u64().unwrap();
    let undo = s.history.undo.len();
    let id = s.execute("comp.group.expose", json!({"comp": c, "node": g, "inner": b, "param": "blurriness", "label": "Softness"})).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(s.history.undo.len(), undo + 1);
    assert!(refused(&mut s, "comp.group.expose", json!({"comp": c, "node": g, "inner": b, "param": "blurriness"})).contains("already"));
    assert!(refused(&mut s, "comp.group.expose", json!({"comp": c, "node": g, "inner": b, "param": "nope"})).contains("no parameter"));
    assert!(refused(&mut s, "comp.group.expose", json!({"comp": c, "node": g, "inner": 9999, "param": "scale"})).contains("no node 9999"));
    let rot = s.execute("comp.group.expose", json!({"comp": c, "node": g, "inner": t, "param": "rotation"})).unwrap()["id"].as_str().unwrap().to_string();
    let base = picture(&comp_of(&s, c));
    // the group's own parameter, typed and clamped like the inner one
    s.execute("comp.setParam", json!({"comp": c, "node": g, "param": id, "value": 20.0})).unwrap();
    assert!(refused(&mut s, "comp.setParam", json!({"comp": c, "node": g, "param": id, "value": "soft"})).contains("wants"));
    assert!(refused(&mut s, "comp.setParam", json!({"comp": c, "node": g, "param": "blur_dimensions", "value": 1})).contains("no parameter"));
    s.execute("comp.toggleAnimation", json!({"comp": c, "node": g, "param": rot, "time": 0})).unwrap();
    let soft = comp_of(&s, c);
    let soft_pic = picture(&soft);
    assert_ne!(soft_pic, base, "the exposed parameter changed the picture");
    // explain describes the group and its controls
    let ex = s.execute("comp.explain", json!({"comp": c})).unwrap();
    let line = ex["lines"].as_array().unwrap().iter().find(|l| l["node"] == json!(g)).unwrap()["text"].as_str().unwrap().to_string();
    assert!(line.contains("group of 2 node(s)") && line.contains("Softness"), "{line}");

    // the macro round-trips through a project file: same comp, same picture
    let back = filmcraft_format::decode(&filmcraft_format::encode(&s.project, true)).unwrap().project;
    let loaded = match &back.item(ItemId(c)).unwrap().kind {
        ItemKind::Comp(x) => (**x).clone(),
        _ => panic!("not a comp"),
    };
    assert_eq!(loaded, soft);
    assert_eq!(picture(&loaded), soft_pic);

    // unexpose keeps the picture (the value moves into the inner node), undo brings it back
    s.execute("comp.group.unexpose", json!({"comp": c, "node": g, "id": id})).unwrap();
    assert_eq!(picture(&comp_of(&s, c)), soft_pic);
    assert!(refused(&mut s, "comp.group.unexpose", json!({"comp": c, "node": g, "id": id})).contains("no exposed parameter"));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(comp_of(&s, c), soft);
    // comp.get lists the group's parameters
    let got = s.execute("comp.get", json!({"comp": c})).unwrap();
    let list = got["groupParams"][g.to_string()].as_array().unwrap().clone();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["exposed"]["label"], json!("Softness"));
    assert_eq!(list[0]["def"]["id"], json!("blurriness"));
}

#[test]
fn expose_and_unexpose_undo_and_redo() {
    let mut s = session();
    let (c, [_, b, t]) = chain(&mut s);
    let g = s.execute("comp.group", json!({"comp": c, "nodes": [b, t]})).unwrap()["node"].as_u64().unwrap();
    let plain = comp_of(&s, c);
    let id = s.execute("comp.group.expose", json!({"comp": c, "node": g, "inner": b, "param": "blurriness"})).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("comp.setParam", json!({"comp": c, "node": g, "param": id, "value": 9.0})).unwrap();
    let exposed = comp_of(&s, c);
    s.execute("comp.group.unexpose", json!({"comp": c, "node": g, "id": id})).unwrap();
    let after = comp_of(&s, c);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(comp_of(&s, c), exposed);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(comp_of(&s, c), after);
    for _ in 0..3 {
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(comp_of(&s, c), plain);
    s.execute("edit.redo", json!({})).unwrap();
    let now = comp_of(&s, c);
    let CompKind::Group { exposed: list, .. } = &now.node(g).unwrap().kind else { panic!() };
    assert_eq!(list.len(), 1);
}

#[test]
fn shared_sources_and_several_consumers() {
    let mut s = session();
    // a Solid feeding both inputs of a Merge and a Blur; both selected: one group input
    let c = s.execute("comp.new", json!({"width": 64, "height": 36})).unwrap()["comp"].as_u64().unwrap();
    let so = add(&mut s, c, json!({"kind": "solid"}));
    let bl = add(&mut s, c, json!({"kind": "blur"}));
    let mg = add(&mut s, c, json!({"kind": "merge"}));
    connect(&mut s, c, so, bl, 0);
    connect(&mut s, c, so, mg, 0);
    connect(&mut s, c, bl, mg, 1);
    // the Merge feeds Media Out and a Transform that is unused, so two outside consumers
    let tr = add(&mut s, c, json!({"kind": "transform"}));
    connect(&mut s, c, mg, tr, 0);
    connect(&mut s, c, mg, 1, 0);
    let before = picture(&comp_of(&s, c));
    let g = s.execute("comp.group", json!({"comp": c, "nodes": [bl, mg]})).unwrap()["node"].as_u64().unwrap();
    let after = comp_of(&s, c);
    assert_eq!(after.node(g).unwrap().kind.inputs(), 1);
    let outs: Vec<_> = after.edges.iter().filter(|e| e.from.node == g).map(|e| e.to.node).collect();
    assert_eq!(outs.len(), 2);
    assert!(outs.contains(&tr) && outs.contains(&1));
    assert_eq!(picture(&after), before);
}

#[test]
fn add_node_counts_group_insides() {
    let mut s = session();
    let (c, [_, b, t]) = chain(&mut s);
    // fill the comp to the cap with solids, then group: the inside counts
    let have = comp_of(&s, c).all_nodes().len();
    for _ in have..(filmcraft_project::dag::MAX_NODES - 3) {
        add(&mut s, c, json!({"kind": "solid"}));
    }
    let g = s.execute("comp.group", json!({"comp": c, "nodes": [b, t]})).unwrap()["node"].as_u64().unwrap();
    let full = comp_of(&s, c);
    assert_eq!(full.all_nodes().len(), filmcraft_project::dag::MAX_NODES);
    assert!(full.nodes.len() < filmcraft_project::dag::MAX_NODES);
    assert!(refused(&mut s, "comp.addNode", json!({"comp": c, "kind": "solid"})).contains("at most"));
    assert_eq!(comp_of(&s, c).node(g), full.node(g));
}
