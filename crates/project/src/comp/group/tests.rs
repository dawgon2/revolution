use super::*;
use crate::keyframe::ParamValue;
use filmcraft_time::{FrameRate, Tick};

fn edge(from: NodeId, to: NodeId, port: u8) -> Edge {
    Edge::new(Port::new(from, 0), Port::new(to, port))
}

/// Solid (2) → Blur (3) → Transform (4) → Out (1).
fn chain() -> Comp {
    let mut c = Comp::new(64, 36, FrameRate { num: 24, den: 1 }, Tick(1_000));
    c.nodes.push(CompNode::new(2, CompKind::Solid));
    c.nodes.push(CompNode::new(3, CompKind::Blur));
    c.nodes.push(CompNode::new(4, CompKind::Transform));
    c.edges = vec![edge(2, 3, 0), edge(3, 4, 0), edge(4, 1, 0)];
    c
}

fn sorted(c: &Comp) -> (Vec<CompNode>, Vec<Edge>) {
    let mut n = c.nodes.clone();
    n.sort_by_key(|n| n.id);
    let mut e = c.edges.clone();
    e.sort();
    (n, e)
}

/// A group nested `levels` deep around one Solid.
fn nested(levels: usize) -> Comp {
    let mut inner = Comp::new(64, 36, FrameRate::default(), Tick::ZERO);
    inner.nodes.push(CompNode::new(2, CompKind::Solid));
    inner.edges.push(edge(2, 1, 0));
    for _ in 0..levels {
        let mut c = Comp::new(64, 36, FrameRate::default(), Tick::ZERO);
        c.nodes.push(CompNode::new(2, CompKind::Group { comp: Box::new(inner), exposed: Vec::new() }));
        c.edges.push(edge(2, 1, 0));
        inner = c;
    }
    inner
}

fn depth_of(c: &Comp) -> usize {
    c.nodes.iter().filter_map(|n| if let CompKind::Group { comp, .. } = &n.kind { Some(1 + depth_of(comp)) } else { None }).max().unwrap_or(0)
}

#[test]
fn group_and_ungroup_restore_the_graph() {
    let mut c = chain();
    let before = c.clone();
    let g = group_nodes(&mut c, &[3, 4], "Soft move").unwrap();
    assert_eq!(c.nodes.len(), 3, "{:?}", c.nodes);
    let gn = c.node(g).unwrap();
    assert_eq!(gn.label, "Soft move");
    assert_eq!(gn.kind.inputs(), 1);
    assert!(c.edges.contains(&edge(2, g, 0)) && c.edges.contains(&edge(g, 1, 0)));
    let CompKind::Group { comp: inner, .. } = &gn.kind else { panic!() };
    assert_eq!(inner.validate().unwrap().nodes().count(), 4); // input, blur, transform, out
    assert!(c.validate().is_ok());
    // serde round trip
    let back: Comp = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back, c);

    let ids = ungroup(&mut c, g).unwrap();
    assert_eq!(ids, vec![3, 4]);
    assert_eq!(sorted(&c), sorted(&before));
}

#[test]
fn group_refusals_change_nothing() {
    let mut c = chain();
    let before = c.clone();
    for (ids, want) in [(vec![], "select"), (vec![9], "no node 9"), (vec![1, 4], "Media Out"), (vec![2, 3, 4, 1], "Media Out")] {
        let e = group_nodes(&mut c, &ids, "").unwrap_err();
        assert!(e.contains(want), "{ids:?}: {e}");
        assert_eq!(c, before);
    }
    // a node that feeds nothing
    c.nodes.push(CompNode::new(9, CompKind::Solid));
    let e = group_nodes(&mut c, &[9], "").unwrap_err();
    assert!(e.contains("don't feed anything"), "{e}");
    // a selected node that doesn't lead to the output
    let e = group_nodes(&mut c, &[3, 9], "").unwrap_err();
    assert!(e.contains("doesn't lead"), "{e}");
    // two outputs: Merge of 3 over 2 next to the chain
    let mut m = chain();
    m.nodes.push(CompNode::new(5, CompKind::Merge));
    m.edges = vec![edge(2, 3, 0), edge(3, 5, 0), edge(2, 5, 1), edge(5, 4, 0), edge(4, 1, 0)];
    let e = group_nodes(&mut m, &[2, 3], "").unwrap_err();
    assert!(e.contains("one output"), "{e}");
    // a loop through an outside node: 3 → 5 (outside) → 4, and 3 → 4
    let mut l = chain();
    l.nodes.push(CompNode::new(5, CompKind::Merge));
    l.edges = vec![edge(2, 3, 0), edge(3, 5, 0), edge(5, 4, 1), edge(3, 4, 0)];
    l.nodes.retain(|n| n.id != 4);
    l.nodes.push(CompNode::new(4, CompKind::Merge));
    l.edges.push(edge(4, 1, 0));
    let before = l.clone();
    let e = group_nodes(&mut l, &[3, 4], "").unwrap_err();
    assert!(e.contains("loop") || e.contains("one output"), "{e}");
    assert_eq!(l, before);
}

#[test]
fn inputs_are_one_per_distinct_source() {
    // Solid 2 feeds both inputs of Merge 5; group [5]: one input
    let mut c = Comp::new(64, 36, FrameRate::default(), Tick::ZERO);
    c.nodes.push(CompNode::new(2, CompKind::Solid));
    c.nodes.push(CompNode::new(3, CompKind::Gradient));
    c.nodes.push(CompNode::new(5, CompKind::Merge));
    c.edges = vec![edge(2, 5, 0), edge(2, 5, 1), edge(5, 1, 0)];
    let g = group_nodes(&mut c, &[5], "").unwrap();
    assert_eq!(c.node(g).unwrap().kind.inputs(), 1);
    // two sources: two inputs, in the order of the ports they fed
    let mut c = Comp::new(64, 36, FrameRate::default(), Tick::ZERO);
    c.nodes.push(CompNode::new(2, CompKind::Solid));
    c.nodes.push(CompNode::new(3, CompKind::Gradient));
    c.nodes.push(CompNode::new(5, CompKind::Merge));
    c.edges = vec![edge(3, 5, 1), edge(2, 5, 0), edge(5, 1, 0)];
    let before = c.clone();
    let g = group_nodes(&mut c, &[5], "").unwrap();
    assert_eq!(c.node(g).unwrap().kind.inputs(), 2);
    assert!(c.edges.contains(&edge(2, g, 0)) && c.edges.contains(&edge(3, g, 1)));
    ungroup(&mut c, g).unwrap();
    assert_eq!(sorted(&c), sorted(&before));
}

#[test]
fn expose_unexpose_and_overrides() {
    let mut c = chain();
    let g = group_nodes(&mut c, &[3, 4], "").unwrap();
    let id = expose(&mut c, g, 3, "blurriness", "Softness").unwrap();
    assert_eq!(id, "blurriness");
    assert!(expose(&mut c, g, 3, "blurriness", "").unwrap_err().contains("already"));
    assert!(expose(&mut c, g, 3, "nope", "").unwrap_err().contains("no parameter"));
    assert!(expose(&mut c, g, 77, "scale", "").unwrap_err().contains("no node 77"));
    assert!(expose(&mut c, 2, 3, "scale", "").unwrap_err().contains("not a group"));
    let s = expose(&mut c, g, 4, "scale", "").unwrap();
    let n = c.node(g).unwrap();
    let defs = exposed_params(n);
    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0].0.label, "Softness");
    assert_eq!(param_def(n, &s).map(|d| d.id), Some("scale"));
    // the group's value overrides the inner one at evaluation
    assert!(matches!(inner_with_overrides(n), Some(Cow::Borrowed(_))));
    c.node_mut(g).unwrap().params.insert(id.clone(), Param::new(ParamValue::Float(12.0)));
    let n = c.node(g).unwrap();
    let inner = inner_with_overrides(n).unwrap();
    assert_eq!(inner.node(3).unwrap().params.get("blurriness").map(|p| p.value.clone()), Some(ParamValue::Float(12.0)));
    // a nested group exposes the outer group's exposed id
    let mut outer = Comp::new(64, 36, FrameRate::default(), Tick::ZERO);
    outer.nodes.push(c.node(2).unwrap().clone());
    outer.nodes.push(c.node(g).unwrap().clone());
    outer.edges = vec![edge(2, g, 0), edge(g, 1, 0)];
    let g2 = group_nodes(&mut outer, &[g], "").unwrap();
    let top = expose(&mut outer, g2, g, &id, "").unwrap();
    assert_eq!(param_def(outer.node(g2).unwrap(), &top).map(|d| d.default), Some(ParamValue::Float(12.0)));
    // unexpose bakes the value in
    unexpose(&mut c, g, &id).unwrap();
    let CompKind::Group { comp, exposed } = &c.node(g).unwrap().kind else { panic!() };
    assert_eq!(exposed.len(), 1);
    assert_eq!(comp.node(3).unwrap().params.get("blurriness").map(|p| p.value.clone()), Some(ParamValue::Float(12.0)));
    assert!(unexpose(&mut c, g, &id).is_err());
}

#[test]
fn sanitize_caps_nesting_depth() {
    let mut c = nested(10);
    assert_eq!(depth_of(&c), 10);
    c.sanitize();
    assert_eq!(depth_of(&c), MAX_GROUP_DEPTH + 1); // the deepest kept group is an empty shell
    assert!(c.all_nodes().len() < 40);
    // and it still validates and loads
    let back: Comp = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back, c);
    let mut ok = nested(MAX_GROUP_DEPTH);
    let before = ok.clone();
    ok.sanitize();
    assert_eq!(ok, before);
}

#[test]
fn sanitize_drops_bad_exposed_params_and_clamps_inputs() {
    let mut c = chain();
    let g = group_nodes(&mut c, &[3, 4], "").unwrap();
    expose(&mut c, g, 3, "blurriness", "").unwrap();
    let n = c.node_mut(g).unwrap();
    let CompKind::Group { comp, exposed } = &mut n.kind else { panic!() };
    for (id, node, param) in [("a", 999, "scale"), ("b", 4, "nope"), ("blurriness", 4, "scale"), ("", 4, "scale"), ("c", 1, "scale")] {
        exposed.push(ExposedParam { id: id.into(), label: "x".repeat(500), node, param: param.into() });
    }
    for i in 0..(MAX_EXPOSED * 2) {
        exposed.push(ExposedParam { id: format!("r{i}"), label: String::new(), node: 4, param: "rotation".into() });
    }
    // a Group Input far out of range, and an inner size that disagrees
    comp.nodes.push(CompNode::new(50, CompKind::MediaIn { source: MediaInSource::GroupInput { index: 1_000_000_000 }, offset: Tick::ZERO }));
    comp.width = 1;
    n.params.insert("not_exposed".into(), Param::new(ParamValue::Float(1.0)));
    n.params.insert("blurriness".into(), Param::new(ParamValue::Float(3.0)));
    c.sanitize();
    let n = c.node(g).unwrap();
    let CompKind::Group { comp, exposed } = &n.kind else { panic!() };
    assert_eq!(comp.width, 64);
    assert!(exposed.len() <= MAX_EXPOSED);
    assert_eq!(exposed.first().map(|e| e.id.as_str()), Some("blurriness"));
    assert!(exposed.iter().all(|e| comp.node(e.node).is_some() && e.label.chars().count() <= MAX_LABEL && !e.id.is_empty()));
    assert!(!exposed.iter().any(|e| e.id == "a" || e.id == "b" || e.id == "c"));
    assert_eq!(n.params.keys().collect::<Vec<_>>(), vec!["blurriness"]);
    assert!(matches!(comp.node(50).unwrap().kind, CompKind::MediaIn { source: MediaInSource::GroupInput { index: 7 }, .. }));
    assert_eq!(n.kind.inputs(), 8);
}

#[test]
fn inner_nodes_count_toward_the_node_cap() {
    let mut c = Comp::new(64, 36, FrameRate::default(), Tick::ZERO);
    for g in 0..4u64 {
        let mut inner = Comp::new(64, 36, FrameRate::default(), Tick::ZERO);
        for i in 0..200u64 {
            inner.nodes.push(CompNode::new(10 + i, CompKind::Solid));
        }
        c.nodes.push(CompNode::new(2 + g, CompKind::Group { comp: Box::new(inner), exposed: Vec::new() }));
    }
    c.sanitize();
    assert!(c.all_nodes().len() <= dag::MAX_NODES, "{}", c.all_nodes().len());
    // grouping refuses past the cap
    let mut full = chain();
    for i in 0..(dag::MAX_NODES as u64 - 4) {
        full.nodes.push(CompNode::new(100 + i, CompKind::Solid));
    }
    assert!(group_nodes(&mut full, &[3, 4], "").unwrap_err().contains("at most"));
}

#[test]
fn hostile_group_json_never_panics() {
    for json in [
        r#"{"width":4,"height":4,"output":1,"nodes":[{"id":1,"kind":{"type":"group","comp":{"width":0,"height":0}}}]}"#,
        // a cyclic inside, exposed pointing at itself
        r#"{"width":4,"height":4,"output":2,"nodes":[{"id":2,"kind":{"type":"group","comp":{"width":4,"height":4,"output":1,
            "nodes":[{"id":1,"kind":{"type":"blur"}},{"id":3,"kind":{"type":"blur"}}],
            "edges":[{"from":{"node":1},"to":{"node":3}},{"from":{"node":3},"to":{"node":1}}]},
            "exposed":[{"id":"x","node":2,"param":"x"},{"id":"y","node":1,"param":"blurriness"}]}}]}"#,
        r#"{"width":4,"height":4,"nodes":[{"id":1,"kind":{"type":"mediaIn","source":{"kind":"groupInput","index":4294967295}}}]}"#,
    ] {
        let mut c: Comp = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}"));
        c.sanitize();
        let _ = c.validate();
        for n in c.all_nodes() {
            let _ = n.kind.inputs();
            let _ = exposed_params(n);
            let _ = inner_with_overrides(n);
        }
        let ids: Vec<NodeId> = c.nodes.iter().map(|n| n.id).collect();
        let _ = group_nodes(&mut c.clone(), &ids, "");
        for id in ids {
            let _ = ungroup(&mut c.clone(), id);
        }
    }
    // a group with no `comp` is rejected by serde (required), not a panic
    let r: Result<Comp, _> = serde_json::from_str(r#"{"width":4,"height":4,"nodes":[{"id":1,"kind":{"type":"group","exposed":[]}}]}"#);
    let _ = r;
}

#[test]
fn grouping_refuses_to_nest_past_the_limit() {
    let mut c = nested(MAX_GROUP_DEPTH);
    let before = c.clone();
    let e = group_nodes(&mut c, &[2], "").unwrap_err();
    assert!(e.contains("nest at most"), "{e}");
    assert_eq!(c, before);
    assert_eq!(nesting(&c), MAX_GROUP_DEPTH);
    // one level less still groups
    let mut c = nested(MAX_GROUP_DEPTH - 1);
    let g = group_nodes(&mut c, &[2], "").unwrap();
    assert_eq!(nesting(&c), MAX_GROUP_DEPTH);
    let before = c.clone();
    c.sanitize();
    assert_eq!(c, before, "nothing emptied");
    assert!(c.node(g).is_some());
}

#[test]
fn ungroup_renumbers_inner_ids_the_outer_comp_uses() {
    let mut c = chain();
    let g = group_nodes(&mut c, &[3, 4], "").unwrap();
    // outer nodes now take the inner ids 3 and 4
    c.nodes.push(CompNode::new(3, CompKind::Solid));
    c.nodes.push(CompNode::new(4, CompKind::Gradient));
    let ids = ungroup(&mut c, g).unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().all(|id| *id > g), "{ids:?}");
    let mut seen = BTreeSet::new();
    assert!(c.nodes.iter().all(|n| seen.insert(n.id)), "ids unique");
    assert!(matches!(c.node(3).unwrap().kind, CompKind::Solid));
    assert!(c.validate().is_ok());
    // the chain is wired through the renumbered nodes: 2 → blur → transform → 1
    let blur = c.nodes.iter().find(|n| matches!(n.kind, CompKind::Blur)).unwrap().id;
    let tr = c.nodes.iter().find(|n| matches!(n.kind, CompKind::Transform)).unwrap().id;
    assert!(c.edges.contains(&edge(2, blur, 0)) && c.edges.contains(&edge(blur, tr, 0)) && c.edges.contains(&edge(tr, 1, 0)));
}

#[test]
fn sanitize_drops_group_values_of_the_wrong_type() {
    let mut c = chain();
    let g = group_nodes(&mut c, &[3, 4], "").unwrap();
    let id = expose(&mut c, g, 3, "blurriness", "").unwrap();
    let n = c.node_mut(g).unwrap();
    n.params.insert(id.clone(), Param::new(ParamValue::Text("soft".into())));
    c.sanitize();
    assert!(c.node(g).unwrap().params.is_empty());
    let n = c.node_mut(g).unwrap();
    let mut p = Param::new(ParamValue::Float(2.0));
    p.keyframes.push(crate::keyframe::Keyframe::new(Tick(5), ParamValue::Bool(true)));
    n.params.insert(id.clone(), p);
    c.sanitize();
    assert!(c.node(g).unwrap().params.is_empty());
    c.node_mut(g).unwrap().params.insert(id.clone(), Param::new(ParamValue::Float(2.0)));
    c.sanitize();
    assert_eq!(c.node(g).unwrap().params.len(), 1);
}

#[test]
fn group_paths_resolve_or_say_why_not() {
    let mut c = nested(4);
    // each level's group is node 2; the innermost holds the Solid
    let deep = inner(&c, &[2, 2, 2, 2]).unwrap();
    assert!(matches!(deep.node(2).map(|n| &n.kind), Some(CompKind::Solid)));
    assert!(matches!(inner(&c, &[2]).unwrap().node(2).map(|n| &n.kind), Some(CompKind::Group { .. })));
    inner_mut(&mut c, &[2, 2]).unwrap().nodes.push(CompNode::new(9, CompKind::Blur));
    assert!(inner(&c, &[2, 2]).unwrap().node(9).is_some());
    for (path, says) in [
        (vec![], "empty"),
        (vec![2, 2, 2, 2, 2], "at most 4"),
        (vec![7], "no node 7 in the comp"),
        (vec![2, 7], "no node 7 inside group 2"),
        (vec![2, 2, 9], "is a blur node, not a group"),
        (vec![2, 2, 2, 2, u64::MAX], "at most 4"),
    ] {
        let e = inner(&c, &path).unwrap_err();
        assert!(e.contains(says), "{path:?}: {e}");
        assert!(inner_mut(&mut c, &path).unwrap_err().contains(says));
    }
}

#[test]
fn a_group_input_stays_at_its_level() {
    let mut c = nested(1);
    let g = inner_mut(&mut c, &[2]).unwrap();
    g.nodes.push(CompNode::new(5, CompKind::MediaIn { source: MediaInSource::GroupInput { index: 0 }, offset: Tick::ZERO }));
    g.nodes.push(CompNode::new(6, CompKind::Blur));
    g.edges = vec![edge(5, 6, 0), edge(6, 2, 0), edge(2, 1, 0)];
    g.nodes.retain(|n| n.id != 2);
    g.nodes.push(CompNode::new(2, CompKind::Transform));
    assert!(group_nodes(g, &[5, 6], "").unwrap_err().contains("Group Input"));
    assert!(group_nodes(g, &[6], "").is_ok());
}
