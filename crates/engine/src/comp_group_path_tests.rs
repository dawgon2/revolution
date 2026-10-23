//! C4.8: the comp commands with a `group` path edit inside groups: one undo step each, the
//! whole tree's bounds hold, bad paths are refused in plain words and change nothing, and an
//! inner edit renders the same as the same edit on the ungrouped nodes.

use super::*;
use filmcraft_project::ItemKind;
use filmcraft_project::comp::{Comp, CompKind, MediaInSource, group};
use filmcraft_render::comp::{CompCtx, NoSources, render_cpu};
use serde_json::{Value, json};

fn session() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
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

fn refused(s: &mut Session, command: &str, params: Value) -> String {
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let e = s.execute(command, params).expect_err(command).to_string();
    assert!(Arc::ptr_eq(&before, &s.project), "{command} left the project alone");
    assert_eq!(s.history.undo.len(), undo, "{command} added no undo step");
    e
}

/// Runs `command` with `params` (plus `comp` and `group`), checks it is one undo step that undo
/// takes back exactly, redoes it and returns its result.
fn step(s: &mut Session, c: u64, path: &[u64], command: &str, params: Value) -> Value {
    let mut p = params;
    p["comp"] = json!(c);
    if !path.is_empty() {
        p["group"] = json!(path);
    }
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let r = s.execute(command, p.clone()).unwrap_or_else(|e| panic!("{command} {p}: {e}"));
    assert_eq!(s.history.undo.len(), undo + 1, "{command}: one undo step");
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before, "{command}: undo takes it back");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *after, "{command}: redo puts it back");
    r
}

fn inside(s: &Session, c: u64, path: &[u64]) -> Comp {
    group::inner(&comp_of(s, c), path).unwrap().clone()
}

/// A 64×36 comp: Gradient → Blur → Transform → Out, Blur and Transform grouped.
/// (comp, gradient, blur, transform, group)
fn grouped(s: &mut Session) -> (u64, u64, u64, u64, u64) {
    let c = s.execute("comp.new", json!({"width": 64, "height": 36})).unwrap()["comp"].as_u64().unwrap();
    let mut add = |k: &str| s.execute("comp.addNode", json!({"comp": c, "kind": k})).unwrap()["node"].as_u64().unwrap();
    let (g, b, t) = (add("gradient"), add("blur"), add("transform"));
    for (f, to) in [(g, b), (b, t), (t, 1)] {
        s.execute("comp.connect", json!({"comp": c, "from": f, "to": to})).unwrap();
    }
    s.execute("comp.setParam", json!({"comp": c, "node": g, "param": "shape", "value": 1})).unwrap();
    s.execute("comp.setParam", json!({"comp": c, "node": t, "param": "rotation", "value": 20.0})).unwrap();
    let gr = s.execute("comp.group", json!({"comp": c, "nodes": [b, t]})).unwrap()["node"].as_u64().unwrap();
    (c, g, b, t, gr)
}

#[test]
fn every_command_family_edits_inside_a_group() {
    let mut s = session();
    let (c, _, b, t, g) = grouped(&mut s);
    let p = [g];
    let top = comp_of(&s, c);
    // add / connect / disconnect / label / move / bypass / remove
    let solid = step(&mut s, c, &p, "comp.addNode", json!({"kind": "solid", "pos": [10, 20]}))["node"].as_u64().unwrap();
    assert!(inside(&s, c, &p).node(solid).is_some());
    assert_eq!(comp_of(&s, c).nodes.len(), top.nodes.len(), "the comp's own nodes are untouched");
    let merge = step(&mut s, c, &p, "comp.addNode", json!({"kind": "merge"}))["node"].as_u64().unwrap();
    step(&mut s, c, &p, "comp.connect", json!({"from": solid, "to": merge, "toPort": 1}));
    assert!(inside(&s, c, &p).edges.iter().any(|e| e.from.node == solid && e.to.node == merge));
    step(&mut s, c, &p, "comp.disconnect", json!({"to": merge, "toPort": 1}));
    step(&mut s, c, &p, "comp.labelNode", json!({"node": b, "label": "Soft"}));
    assert_eq!(inside(&s, c, &p).node(b).unwrap().label, "Soft");
    step(&mut s, c, &p, "comp.moveNode", json!({"node": b, "pos": [5, 6]}));
    assert_eq!(inside(&s, c, &p).node(b).unwrap().pos, [5.0, 6.0]);
    assert_eq!(step(&mut s, c, &p, "comp.bypass", json!({"node": t}))["bypass"], json!(true));
    step(&mut s, c, &p, "comp.bypass", json!({"node": t, "on": false}));
    // parameters, animation, expressions
    step(&mut s, c, &p, "comp.setParam", json!({"node": b, "param": "blurriness", "value": 7.0}));
    assert_eq!(inside(&s, c, &p).node(b).unwrap().params["blurriness"].value, filmcraft_project::ParamValue::Float(7.0));
    assert_eq!(step(&mut s, c, &p, "comp.toggleAnimation", json!({"node": t, "param": "scale", "time": 0}))["animated"], json!(true));
    let r = step(&mut s, c, &p, "comp.setExpression", json!({"node": t, "param": "rotation", "expression": "10 + 5"}));
    assert_eq!(r["stored"], json!(true));
    assert_eq!(r["value"], json!({"Float": 15.0}));
    step(&mut s, c, &p, "comp.clearExpression", json!({"node": t, "param": "rotation"}));
    assert!(refused(&mut s, "comp.clearExpression", json!({"comp": c, "group": p, "node": t, "param": "rotation"})).contains("no expression"));
    // masks, roto, paint, tracker
    let mask = step(&mut s, c, &p, "comp.addNode", json!({"kind": "mask"}))["node"].as_u64().unwrap();
    let m = filmcraft_project::Mask::new("M", filmcraft_project::MaskPath::rect(1.0, 1.0, 9.0, 9.0));
    step(&mut s, c, &p, "comp.setMasks", json!({"node": mask, "masks": [m]}));
    let roto = step(&mut s, c, &p, "comp.addNode", json!({"kind": "roto"}))["node"].as_u64().unwrap();
    step(&mut s, c, &p, "comp.roto.addShape", json!({"node": roto}));
    let paint = step(&mut s, c, &p, "comp.addNode", json!({"kind": "paint"}))["node"].as_u64().unwrap();
    step(&mut s, c, &p, "comp.setPaint", json!({"node": paint, "paint": {"strokes": []}}));
    let tracker = step(&mut s, c, &p, "comp.addNode", json!({"kind": "tracker"}))["node"].as_u64().unwrap();
    step(&mut s, c, &p, "comp.track.setPoints", json!({"node": tracker, "points": [{"at": [10, 10]}]}));
    step(&mut s, c, &p, "comp.roto.linkTracker", json!({"node": roto, "shape": 0, "tracker": tracker}));
    assert!(refused(&mut s, "comp.track.analyse", json!({"comp": c, "group": p, "node": tracker})).contains("own nodes only"));
    assert!(refused(&mut s, "comp.setSize", json!({"comp": c, "group": p, "width": 10})).contains("own nodes only"));
    // the inner Media Out, removing, comp.get of the inside
    step(&mut s, c, &p, "comp.setOutput", json!({"node": b}));
    assert_eq!(inside(&s, c, &p).output, Some(b));
    step(&mut s, c, &p, "comp.removeNode", json!({"nodes": [solid, merge]}));
    assert!(inside(&s, c, &p).node(solid).is_none());
    let got = s.execute("comp.get", json!({"comp": c, "group": p})).unwrap();
    assert!(got["nodes"].as_array().unwrap().iter().any(|n| n["id"] == json!(b)));
    assert_eq!(got["comp"], json!(c));
}

#[test]
fn group_inputs_nested_groups_and_expose_at_depth() {
    let mut s = session();
    let (c, _, b, t, g) = grouped(&mut s);
    // a second Group Input inside: the group gets a second input port
    assert_eq!(comp_of(&s, c).node(g).unwrap().kind.inputs(), 1);
    let gi = step(&mut s, c, &[g], "comp.addNode", json!({"kind": "groupInput"}))["node"].as_u64().unwrap();
    let n = inside(&s, c, &[g]).node(gi).unwrap().clone();
    assert!(matches!(n.kind, CompKind::MediaIn { source: MediaInSource::GroupInput { index: 1 }, .. }));
    assert_eq!(n.label, "Input 2");
    assert_eq!(comp_of(&s, c).node(g).unwrap().kind.inputs(), 2);
    let gi5 = step(&mut s, c, &[g], "comp.addNode", json!({"kind": "groupInput", "index": 5}))["node"].as_u64().unwrap();
    assert_eq!(comp_of(&s, c).node(g).unwrap().kind.inputs(), 6);
    // something feeds input 5; removing that Group Input drops the dangling connection outside
    let solid = s.execute("comp.addNode", json!({"comp": c, "kind": "solid"})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": solid, "to": g, "toPort": 5})).unwrap();
    step(&mut s, c, &[g], "comp.removeNode", json!({"nodes": [gi5]}));
    assert!(!comp_of(&s, c).edges.iter().any(|e| e.to.node == g && e.to.index == 5));
    assert!(refused(&mut s, "comp.addNode", json!({"comp": c, "kind": "groupInput"})).contains("inside a group"));
    assert!(refused(&mut s, "comp.addNode", json!({"comp": c, "group": [g], "kind": "groupInput", "index": 8})).contains("index"));
    // group the inner Transform inside the group: a nested group, edited through a 2-long path
    let inner = step(&mut s, c, &[g], "comp.group", json!({"nodes": [t], "label": "Turn"}))["node"].as_u64().unwrap();
    assert_eq!(group::nesting(&comp_of(&s, c)), 2);
    step(&mut s, c, &[g, inner], "comp.setParam", json!({"node": t, "param": "scale", "value": 50.0}));
    // expose on the nested group, then the outer group exposes that exposed id
    let id = step(&mut s, c, &[g], "comp.group.expose", json!({"node": inner, "inner": t, "param": "rotation"}))["id"].as_str().unwrap().to_string();
    let outer = step(&mut s, c, &[], "comp.group.expose", json!({"node": g, "inner": inner, "param": id}))["id"].as_str().unwrap().to_string();
    let before = picture(&comp_of(&s, c));
    s.execute("comp.setParam", json!({"comp": c, "node": g, "param": outer, "value": 75.0})).unwrap();
    assert_ne!(picture(&comp_of(&s, c)), before, "the outer group's setting turns the nested Transform");
    s.execute("comp.group.unexpose", json!({"comp": c, "node": g, "id": outer})).unwrap();
    step(&mut s, c, &[g], "comp.group.unexpose", json!({"node": inner, "id": id}));
    step(&mut s, c, &[g], "comp.ungroup", json!({"node": inner}));
    assert_eq!(group::nesting(&comp_of(&s, c)), 1);
    assert!(inside(&s, c, &[g]).node(b).is_some());
}

#[test]
fn bad_group_paths_are_refused_and_change_nothing() {
    let mut s = session();
    let (c, gr, b, _, g) = grouped(&mut s);
    let set = |path: Value| json!({"comp": c, "group": path, "node": b, "param": "blurriness", "value": 3.0});
    for (path, says) in [
        (json!([999]), "no node 999 in the comp"),
        (json!([gr]), "not a group"),
        (json!([g, 999]), "no node 999 inside group"),
        (json!([g, g, g, g, g]), "at most 4"),
        (json!([]), "empty"),
        (json!(["x"]), "group node ids"),
        (json!({"a": 1}), "group node ids"),
        (json!([-1]), "group node ids"),
    ] {
        for (cmd, p) in [
            ("comp.setParam", set(path.clone())),
            ("comp.addNode", json!({"comp": c, "group": path.clone(), "kind": "blur"})),
            ("comp.connect", json!({"comp": c, "group": path.clone(), "from": b, "to": 1})),
            ("comp.group", json!({"comp": c, "group": path.clone(), "nodes": [b]})),
            ("comp.group.expose", json!({"comp": c, "group": path.clone(), "node": g, "inner": b, "param": "blurriness"})),
            ("comp.setExpression", json!({"comp": c, "group": path.clone(), "node": b, "param": "blurriness", "expression": "1"})),
        ] {
            let e = refused(&mut s, cmd, p);
            assert!(e.contains(says), "{cmd} {path}: {e}");
        }
        assert!(s.execute("comp.get", json!({"comp": c, "group": path})).is_err());
    }
    // a node of the comp's own level isn't inside the group
    assert!(refused(&mut s, "comp.setParam", json!({"comp": c, "group": [g], "node": gr, "param": "shape", "value": 1})).contains("no node"));
}

#[test]
fn depth_and_node_caps_count_the_whole_tree() {
    let mut s = session();
    let (c, _, b, _, g) = grouped(&mut s);
    // nest b in groups until the limit: each comp.group one level further in
    let mut path = vec![g];
    let mut node = b;
    while path.len() < filmcraft_project::comp::MAX_GROUP_DEPTH {
        node = s.execute("comp.group", json!({"comp": c, "group": path, "nodes": [b]})).unwrap()["node"].as_u64().unwrap();
        path.push(node);
    }
    assert_eq!(group::nesting(&comp_of(&s, c)), 4);
    assert!(inside(&s, c, &path).node(b).is_some(), "the Blur is 4 groups deep ({node})");
    assert!(refused(&mut s, "comp.group", json!({"comp": c, "group": path, "nodes": [b]})).contains("at most 4"));
    // fill the comp to its cap at the top: adding inside the group is refused too
    while comp_of(&s, c).all_nodes().len() < filmcraft_project::dag::MAX_NODES {
        s.execute("comp.addNode", json!({"comp": c, "kind": "solid"})).unwrap();
    }
    assert!(refused(&mut s, "comp.addNode", json!({"comp": c, "group": [g], "kind": "solid"})).contains("at most"));
    assert!(refused(&mut s, "comp.addNode", json!({"comp": c, "group": path, "kind": "solid"})).contains("at most"));
}

#[test]
fn templates_save_from_and_insert_into_a_group() {
    let mut s = session();
    s.prefs_path = Some(crate::media_test_util::tmp_dir("c4-8-group-templates").join("prefs.json"));
    let (c, _, b, _, g) = grouped(&mut s);
    s.execute("comp.template.save", json!({"comp": c, "group": [g], "nodes": [b], "name": "Inner blur", "replace": true})).unwrap();
    let top = comp_of(&s, c).nodes.len();
    let before = inside(&s, c, &[g]).nodes.len();
    let r = step(&mut s, c, &[g], "comp.template.insert", json!({"name": "Inner blur"}));
    assert_eq!(r["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(inside(&s, c, &[g]).nodes.len(), before + 1);
    assert_eq!(comp_of(&s, c).nodes.len(), top, "inserted inside the group, not beside it");
    assert!(refused(&mut s, "comp.template.insert", json!({"comp": c, "group": [999], "name": "Inner blur"})).contains("no node 999"));
}

#[test]
fn group_inputs_from_a_template_are_left_out_at_the_top_level() {
    let mut s = session();
    s.prefs_path = Some(crate::media_test_util::tmp_dir("c4-8-group-input-templates").join("prefs.json"));
    let (c, _, b, _, g) = grouped(&mut s);
    let gi = inside(&s, c, &[g]).nodes.iter().find(|n| matches!(n.kind, CompKind::MediaIn { source: MediaInSource::GroupInput { .. }, .. })).unwrap().id;
    s.execute("comp.template.save", json!({"comp": c, "group": [g], "nodes": [gi, b], "name": "Input and blur", "replace": true})).unwrap();
    s.execute("comp.template.save", json!({"comp": c, "group": [g], "nodes": [gi], "name": "Only input", "replace": true})).unwrap();
    // inside a group both come in whole
    let r = step(&mut s, c, &[g], "comp.template.insert", json!({"name": "Input and blur"}));
    assert_eq!(r["nodes"].as_array().unwrap().len(), 2);
    // at the comp's own level the Group Input is left out, with a warning
    let top = comp_of(&s, c).nodes.len();
    let r = step(&mut s, c, &[], "comp.template.insert", json!({"name": "Input and blur"}));
    assert_eq!(r["nodes"].as_array().unwrap().len(), 1);
    assert!(r["warnings"].to_string().contains("Group Input"), "{r}");
    let now = comp_of(&s, c);
    assert_eq!(now.nodes.len(), top + 1);
    assert!(!now.nodes.iter().any(|n| matches!(n.kind, CompKind::MediaIn { source: MediaInSource::GroupInput { .. }, .. })));
    assert!(refused(&mut s, "comp.template.insert", json!({"comp": c, "name": "Only input"})).contains("only Group Inputs"));
}

#[test]
fn unexposing_a_setting_the_outer_group_re_exposes_is_refused() {
    let mut s = session();
    let (c, _, _, t, g) = grouped(&mut s);
    let inner = s.execute("comp.group", json!({"comp": c, "group": [g], "nodes": [t], "label": "Turn"})).unwrap()["node"].as_u64().unwrap();
    let id = s.execute("comp.group.expose", json!({"comp": c, "group": [g], "node": inner, "inner": t, "param": "rotation"})).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.execute("comp.group.expose", json!({"comp": c, "node": g, "inner": inner, "param": id})).unwrap();
    let e = refused(&mut s, "comp.group.unexpose", json!({"comp": c, "group": [g], "node": inner, "id": id}));
    assert!(e.contains("group around this one") && e.contains(&format!("group {g}")), "{e}");
    // taken off the outer group first, the inner one comes off too
    s.execute("comp.group.unexpose", json!({"comp": c, "node": g, "id": id})).unwrap();
    step(&mut s, c, &[g], "comp.group.unexpose", json!({"node": inner, "id": id}));
}

#[test]
fn merge_keys_keep_levels_apart() {
    let mut s = session();
    let (c, _, b, _, g) = grouped(&mut s);
    // a second level: the Blur in a group inside the group
    let g2 = s.execute("comp.group", json!({"comp": c, "group": [g], "nodes": [b]})).unwrap()["node"].as_u64().unwrap();
    // a node id used at two levels: drags on each are their own undo steps
    let ids = |c: &Comp| c.nodes.iter().map(|n| n.id).collect::<std::collections::BTreeSet<_>>();
    let levels: [Vec<u64>; 3] = [vec![], vec![g], vec![g, g2]];
    for (a, z) in [(0, 1), (1, 2), (0, 2)] {
        let (pa, pz) = (&levels[a], &levels[z]);
        let shared = *ids(&inside_or_top(&s, c, pa)).intersection(&ids(&inside_or_top(&s, c, pz))).next().expect("an id at both levels");
        let undo = s.history.undo.len();
        let mv = |path: &[u64], x: f64| {
            let mut p = json!({"comp": c, "node": shared, "pos": [x, 1.0]});
            if !path.is_empty() {
                p["group"] = json!(path);
            }
            p
        };
        s.execute("comp.moveNode", mv(pa, 10.0)).unwrap();
        s.execute("comp.moveNode", mv(pa, 20.0)).unwrap();
        assert_eq!(s.history.undo.len(), undo + 1, "one drag, one step");
        s.execute("comp.moveNode", mv(pz, 30.0)).unwrap();
        assert_eq!(s.history.undo.len(), undo + 2, "levels {pa:?} and {pz:?}: separate steps");
    }
    // a slider drag (setParam merges per node and parameter) on the same Blur id at two levels
    let undo = s.history.undo.len();
    s.execute("comp.setParam", json!({"comp": c, "group": [g, g2], "node": b, "param": "blurriness", "value": 3.0})).unwrap();
    s.execute("comp.setParam", json!({"comp": c, "group": [g, g2], "node": b, "param": "blurriness", "value": 4.0})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    // a Blur with the same id in both groups: the inner group's ids count up to the outer's
    let id = s.execute("comp.addNode", json!({"comp": c, "group": [g], "kind": "blur"})).unwrap()["node"].as_u64().unwrap();
    for _ in 0..64 {
        let n = s.execute("comp.addNode", json!({"comp": c, "group": [g, g2], "kind": "blur"})).unwrap()["node"].as_u64().unwrap();
        if n >= id {
            assert_eq!(n, id);
            break;
        }
    }
    assert!(inside(&s, c, &[g, g2]).node(id).is_some_and(|n| matches!(n.kind, CompKind::Blur)));
    let undo = s.history.undo.len();
    s.execute("comp.setParam", json!({"comp": c, "group": [g], "node": id, "param": "blurriness", "value": 5.0})).unwrap();
    s.execute("comp.setParam", json!({"comp": c, "group": [g, g2], "node": id, "param": "blurriness", "value": 6.0})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 2, "two groups' Blur {id}: separate steps");
}

fn inside_or_top(s: &Session, c: u64, path: &[u64]) -> Comp {
    if path.is_empty() { comp_of(s, c) } else { inside(s, c, path) }
}

#[test]
fn an_inner_edit_renders_like_the_same_edit_ungrouped() {
    fn chain(s: &mut Session) -> (u64, u64, u64) {
        let c = s.execute("comp.new", json!({"width": 64, "height": 36})).unwrap()["comp"].as_u64().unwrap();
        let mut add = |k: &str| s.execute("comp.addNode", json!({"comp": c, "kind": k})).unwrap()["node"].as_u64().unwrap();
        let (gr, b, t) = (add("gradient"), add("blur"), add("transform"));
        for (f, to) in [(gr, b), (b, t), (t, 1)] {
            s.execute("comp.connect", json!({"comp": c, "from": f, "to": to})).unwrap();
        }
        s.execute("comp.setParam", json!({"comp": c, "node": gr, "param": "shape", "value": 1})).unwrap();
        (c, b, t)
    }
    let mut s = session();
    let (c, b, t) = chain(&mut s);
    let plain = picture(&comp_of(&s, c));
    // the flat edit
    let mut flat = session();
    assert_eq!(chain(&mut flat), (c, b, t));
    flat.execute("comp.setParam", json!({"comp": c, "node": b, "param": "blurriness", "value": 9.0})).unwrap();
    let want = picture(&comp_of(&flat, c));
    assert_ne!(want, plain);
    // the same edit inside a group
    let g = s.execute("comp.group", json!({"comp": c, "nodes": [b, t]})).unwrap()["node"].as_u64().unwrap();
    assert_eq!(picture(&comp_of(&s, c)), plain);
    s.execute("comp.setParam", json!({"comp": c, "group": [g], "node": b, "param": "blurriness", "value": 9.0})).unwrap();
    assert_eq!(picture(&comp_of(&s, c)), want);
    // one level deeper too
    let g2 = s.execute("comp.group", json!({"comp": c, "group": [g], "nodes": [b]})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.setParam", json!({"comp": c, "group": [g, g2], "node": b, "param": "blurriness", "value": 2.0})).unwrap();
    flat.execute("comp.setParam", json!({"comp": c, "node": b, "param": "blurriness", "value": 2.0})).unwrap();
    assert_eq!(picture(&comp_of(&s, c)), picture(&comp_of(&flat, c)));
}
