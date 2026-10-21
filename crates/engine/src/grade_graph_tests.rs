//! Grade graph commands (fork C2): branching, connections, output, undo / redo, refusals and
//! hostile parameters.

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_project::dag::{Edge, Port};
use filmcraft_project::grade::{Grade, MAX_NODES, NodeKind, SOURCE};

fn demo() -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let id = q.video_tracks[0].item_at(s.playhead()).unwrap().id;
    s.state.selection = vec![id];
    (s, id)
}

fn grade(s: &Session, c: ClipId) -> Grade {
    s.active_sequence().unwrap().find_item(c).unwrap().1.grade.clone().unwrap_or_default()
}

fn add(s: &mut Session, p: Value) -> u64 {
    s.execute("grade.addNode", p).unwrap()["node"].as_u64().unwrap()
}

fn e(from: u64, fi: u8, to: u64, ti: u8) -> Edge {
    Edge::new(Port::new(from, fi), Port::new(to, ti))
}

fn sorted(mut v: Vec<Edge>) -> Vec<Edge> {
    v.sort_unstable();
    v
}

/// Two serial nodes 1 → 2 on the demo clip.
fn two() -> (Session, ClipId, u64, u64) {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    let b = add(&mut s, json!({"clip": c.0}));
    (s, c, a, b)
}

#[test]
fn add_parallel_writes_the_chain_out_and_mixes_beside_the_node() {
    let (mut s, c, a, b) = two();
    let serial = grade(&s, c);
    let r = s.execute("grade.addParallel", json!({"clip": c.0, "node": b})).unwrap();
    let y = r["node"].as_u64().unwrap();
    let m = r["nodes"][1].as_u64().unwrap();
    let g = grade(&s, c);
    assert_eq!(g.node(m).unwrap().kind, NodeKind::ParallelMixer);
    assert_eq!(sorted(g.edges.clone()), sorted(vec![e(SOURCE, 0, a, 0), e(a, 0, b, 0), e(a, 0, y, 0), e(b, 0, m, 0), e(y, 0, m, 1)]));
    assert_eq!(g.output_node(), Ok(m));
    assert_eq!(g.output, None, "the output pin is dropped when the mixer is the only sink");
    assert_eq!(g.graph_order().unwrap().nodes().last(), Some(m));
    assert_eq!(s.state.grade_node.map(|n| n.node), Some(y), "the new node is current");
    // another parallel node on the same branch joins the same mixer
    let y2 = s.execute("grade.addParallel", json!({"clip": c.0, "node": y})).unwrap()["node"].as_u64().unwrap();
    let g = grade(&s, c);
    assert!(g.edges.contains(&e(y2, 0, m, 2)) && g.edges.contains(&e(a, 0, y2, 0)));
    assert_eq!(g.nodes.iter().filter(|n| n.kind == NodeKind::ParallelMixer).count(), 1);
    // undo takes both steps back to the plain serial grade, redo brings them again
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c), serial);
    s.execute("edit.redo", json!({})).unwrap();
    assert!(grade(&s, c).node(m).is_some() && grade(&s, c).node(y2).is_none());
}

#[test]
fn layer_outside_splitter_and_key_mixer_build_valid_graphs() {
    let (mut s, c, a, b) = two();
    let r = s.execute("grade.addLayer", json!({"clip": c.0, "node": a})).unwrap();
    let lm = r["nodes"][1].as_u64().unwrap();
    let g = grade(&s, c);
    assert_eq!(g.node(lm).unwrap().kind, NodeKind::LayerMixer);
    assert!(g.edges.contains(&e(lm, 0, b, 0)), "what the node fed is fed from the mixer");

    let o = s.execute("grade.addOutside", json!({"clip": c.0, "node": b})).unwrap()["node"].as_u64().unwrap();
    let g = grade(&s, c);
    assert!(g.edges.contains(&e(b, 0, o, 0)) && g.edges.contains(&e(b, 1, o, 1)));
    assert_eq!(g.node(o).unwrap().param("key.invert").map(|p| p.value.clone()), Some(filmcraft_project::ParamValue::Float(1.0)));
    assert_eq!(g.output_node(), Ok(o));

    let r = s.execute("grade.addSplitter", json!({"clip": c.0, "node": o})).unwrap();
    let ids: Vec<u64> = r["nodes"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
    assert_eq!(ids.len(), 5);
    let g = grade(&s, c);
    assert_eq!(g.node(ids[0]).unwrap().kind, NodeKind::Splitter);
    assert_eq!(g.node(ids[4]).unwrap().kind, NodeKind::Combiner);
    assert!(g.edges.contains(&e(ids[0], 2, ids[3], 0)) && g.edges.contains(&e(ids[3], 0, ids[4], 2)));
    assert_eq!(g.output_node(), Ok(ids[4]));

    let k = s.execute("grade.addKeyMixer", json!({"clip": c.0, "node": a})).unwrap()["node"].as_u64().unwrap();
    let g = grade(&s, c);
    assert!(g.edges.contains(&e(a, 1, k, 0)));
    assert_eq!(g.output_node(), Ok(ids[4]), "a key mixer is never the picture's output");
    assert!(g.graph_order().is_ok());
    // a key mixer can't be branched from, nor a splitter be the output
    assert!(s.execute("grade.addParallel", json!({"clip": c.0, "node": k})).is_err());
    assert!(s.execute("grade.setOutput", json!({"clip": c.0, "node": ids[0]})).is_err());
}

#[test]
fn connect_disconnect_and_set_output_with_refusals() {
    let (mut s, c, a, b) = two();
    let r = s.execute("grade.addParallel", json!({"clip": c.0, "node": b})).unwrap();
    let (y, m) = (r["nodes"][0].as_u64().unwrap(), r["nodes"][1].as_u64().unwrap());
    let before = grade(&s, c);
    // a cycle is refused and changes nothing
    let err = s.execute("grade.connect", json!({"clip": c.0, "from": m, "to": a})).unwrap_err().to_string();
    assert!(err.contains("cycle"), "{err}");
    assert_eq!(grade(&s, c), before);
    // feed y from the grade's input instead of a
    s.execute("grade.connect", json!({"clip": c.0, "from": 0, "to": y})).unwrap();
    assert!(grade(&s, c).edges.contains(&e(SOURCE, 0, y, 0)) && !grade(&s, c).edges.contains(&e(a, 0, y, 0)));
    // disconnecting y from the mixer leaves y unconnected; the output stays the mixer
    s.execute("grade.disconnect", json!({"clip": c.0, "to": m, "toPort": 1})).unwrap();
    let g = grade(&s, c);
    assert_eq!(g.output_node(), Ok(m));
    assert!(g.graph_order().is_ok());
    assert!(s.execute("grade.disconnect", json!({"clip": c.0, "to": m, "toPort": 1})).is_err(), "nothing connected there");
    s.execute("grade.setOutput", json!({"clip": c.0, "node": b})).unwrap();
    assert_eq!(grade(&s, c).output, Some(b));
    // one undo step each
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c).output_node(), Ok(m));
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c), before);
    s.execute("edit.redo", json!({})).unwrap();
    assert!(grade(&s, c).edges.contains(&e(SOURCE, 0, y, 0)));
}

#[test]
fn connect_on_a_serial_grade_branches_it_and_set_output_to_the_last_node_does_not() {
    let (mut s, c, a, b) = two();
    let serial = grade(&s, c);
    s.execute("grade.setOutput", json!({"clip": c.0, "node": b})).unwrap();
    assert_eq!(grade(&s, c), serial, "the last node is the serial output already");
    // a's key into b's key input: a real branch
    s.execute("grade.connect", json!({"clip": c.0, "from": a, "fromPort": 1, "to": b, "toPort": 1})).unwrap();
    assert!(grade(&s, c).is_graph());
    // and disconnecting it again gives the plain chain back (the edges are dropped)
    s.execute("grade.disconnect", json!({"clip": c.0, "to": b, "toPort": 1})).unwrap();
    assert_eq!(grade(&s, c), serial);
}

#[test]
fn deleting_nodes_rewires_and_unbranches() {
    let (mut s, c, a, b) = two();
    let serial = grade(&s, c);
    let r = s.execute("grade.addParallel", json!({"clip": c.0, "node": b})).unwrap();
    let (y, m) = (r["nodes"][0].as_u64().unwrap(), r["nodes"][1].as_u64().unwrap());
    // deleting the mixer: what it fed (the output) moves to its first input; y is left over
    s.execute("grade.removeNode", json!({"clip": c.0, "node": m})).unwrap();
    let g = grade(&s, c);
    assert!(g.graph_order().is_ok());
    assert_eq!(g.output_node(), Ok(b));
    assert!(g.edges.iter().all(|e| e.from.node != m && e.to.node != m));
    // deleting the left-over node too gives the plain chain back
    s.execute("grade.removeNode", json!({"clip": c.0, "node": y})).unwrap();
    assert_eq!(grade(&s, c), serial);
    // serial commands on a branched grade: add / insert wire in, move is refused
    s.execute("grade.addOutside", json!({"clip": c.0, "node": a})).unwrap();
    let n = add(&mut s, json!({"clip": c.0, "after": a}));
    let g = grade(&s, c);
    assert!(g.edges.contains(&e(a, 0, n, 0)) && g.graph_order().is_ok());
    let i = s.execute("grade.insertNode", json!({"clip": c.0, "before": a})).unwrap()["node"].as_u64().unwrap();
    let g = grade(&s, c);
    assert!(g.edges.contains(&e(SOURCE, 0, i, 0)) && g.edges.contains(&e(i, 0, a, 0)));
    assert!(s.execute("grade.moveNode", json!({"clip": c.0, "node": a, "to": 0})).is_err());
}

#[test]
fn append_and_paste_keep_branches_valid() {
    let (mut s, c, _a, b) = two();
    s.execute("grade.addParallel", json!({"clip": c.0, "node": b})).unwrap();
    let src = grade(&s, c);
    s.execute("grade.copy", json!({"clip": c.0})).unwrap();
    s.execute("grade.append", json!({"clips": [c.0]})).unwrap();
    let g = grade(&s, c);
    assert_eq!(g.nodes.len(), src.nodes.len() * 2);
    assert!(g.graph_order().is_ok());
    assert_eq!(g.graph_order().unwrap().steps.len(), g.nodes.len() + 1, "every node renders");
}

#[test]
fn hostile_params_and_disabled_without_a_grade() {
    let (mut s, c) = demo();
    // no grade yet: the commands are disabled
    for id in [
        "grade.addParallel",
        "grade.addLayer",
        "grade.addOutside",
        "grade.addSplitter",
        "grade.addKeyMixer",
        "grade.connect",
        "grade.disconnect",
        "grade.setOutput",
    ] {
        assert!(!s.is_enabled(id), "{id}");
        assert!(s.execute(id, json!({"clip": c.0, "node": 1, "from": 0, "to": 1})).is_err(), "{id}");
    }
    let a = add(&mut s, json!({"clip": c.0}));
    assert!(s.is_enabled("grade.addParallel"));
    let before = grade(&s, c);
    let bad = [
        ("grade.addParallel", json!({"clip": c.0, "node": u64::MAX})),
        ("grade.addParallel", json!({"clip": c.0, "node": -1})),
        ("grade.addLayer", json!({"clip": c.0, "node": "x"})),
        ("grade.connect", json!({"clip": c.0, "from": a, "to": a})),
        ("grade.connect", json!({"clip": c.0, "from": 0, "to": 0})),
        ("grade.connect", json!({"clip": c.0, "from": 0, "fromPort": 1, "to": a})),
        ("grade.connect", json!({"clip": c.0, "from": 0, "to": a, "toPort": 255})),
        ("grade.connect", json!({"clip": c.0, "from": 0, "to": a, "toPort": 256})),
        ("grade.connect", json!({"clip": c.0, "from": 0, "to": a, "toPort": 1e300})),
        ("grade.connect", json!({"clip": c.0, "from": u64::MAX, "to": a})),
        ("grade.connect", json!({"clip": c.0, "to": a})),
        ("grade.disconnect", json!({"clip": c.0, "to": u64::MAX, "toPort": 7})),
        ("grade.setOutput", json!({"clip": c.0, "node": u64::MAX})),
        ("grade.setOutput", json!({"clip": c.0})),
        ("grade.addParallel", json!({"clip": u64::MAX, "node": a})),
    ];
    for (id, p) in bad {
        assert!(s.execute(id, p.clone()).is_err(), "{id} {p}");
        assert_eq!(grade(&s, c), before, "{id} {p} changed the grade");
    }
    // the only connection can't go (no edges would read as the serial chain)
    assert!(s.execute("grade.disconnect", json!({"clip": c.0, "to": a})).is_err());
    assert_eq!(grade(&s, c), before);
    // a parallel node needs the node's input: a disconnected node refuses
    add(&mut s, json!({"clip": c.0}));
    s.execute("grade.disconnect", json!({"clip": c.0, "to": a})).unwrap();
    assert!(s.execute("grade.addParallel", json!({"clip": c.0, "node": a})).is_err());
}

#[test]
fn the_node_cap_holds() {
    let (mut s, c) = demo();
    let first = add(&mut s, json!({"clip": c.0}));
    for _ in 1..MAX_NODES - 1 {
        add(&mut s, json!({"clip": c.0}));
    }
    // 63 nodes: a parallel node needs two more
    assert!(s.execute("grade.addParallel", json!({"clip": c.0, "node": first})).is_err());
    assert!(s.execute("grade.addOutside", json!({"clip": c.0, "node": first})).is_ok());
    assert!(s.execute("grade.addOutside", json!({"clip": c.0, "node": first})).is_err());
    assert!(grade(&s, c).graph_order().is_ok());
}

// ---------- review fixes (C2.4) ----------

/// The demo's second video clip (no grade).
fn second(s: &Session, c: ClipId) -> ClipId {
    let q = s.active_sequence().unwrap();
    q.video_tracks.iter().flat_map(|t| t.items.iter()).map(|it| it.id).find(|id| *id != c).unwrap()
}

/// Two serial nodes a → b, then a parallel node y beside b into mixer m.
fn branched() -> (Session, ClipId, u64, u64, u64, u64) {
    let (mut s, c, a, b) = two();
    let r = s.execute("grade.addParallel", json!({"clip": c.0, "node": b})).unwrap();
    let (y, m) = (r["nodes"][0].as_u64().unwrap(), r["nodes"][1].as_u64().unwrap());
    (s, c, a, b, y, m)
}

#[test]
fn outside_uses_the_key_invert_graph_param_and_params_lists_graph_params() {
    let (mut s, c, a, _) = two();
    let o = s.execute("grade.addOutside", json!({"clip": c.0, "node": a})).unwrap()["node"].as_u64().unwrap();
    assert_eq!(grade(&s, c).node(o).unwrap().float_at("key.invert", filmcraft_time::Tick(0)), 1.0);
    let list = s.execute("grade.params", json!({})).unwrap();
    let keys: Vec<&str> = list["params"].as_array().unwrap().iter().filter_map(|p| p["key"].as_str()).collect();
    for k in ["lift", "key.invert", "key.gain", "mix.gain.0", "layer.mode.15", "kmix.invert.3"] {
        assert!(keys.contains(&k), "{k}");
    }
}

#[test]
fn pick_qualifier_on_a_graph_samples_the_nodes_real_input() {
    let (mut s, c, a, _b, y, _m) = branched();
    // a lifts everything a lot; y is fed from a, so y's input is bright
    s.execute("grade.setParam", json!({"clip": c.0, "node": a, "param": "offset", "value": [0.5, 0.5, 0.5, 0.0]})).unwrap();
    s.execute("grade.pickQualifier", json!({"clip": c.0, "node": a, "x": 0.5, "y": 0.5})).unwrap();
    s.execute("grade.pickQualifier", json!({"clip": c.0, "node": y, "x": 0.5, "y": 0.5})).unwrap();
    let g = grade(&s, c);
    let lum = |n: u64| g.node(n).unwrap().float_at("qual.lumLow", filmcraft_time::Tick(0));
    assert!(lum(y) > lum(a) + 0.05, "y samples a's lifted output: a {} y {}", lum(a), lum(y));
    assert!(g.graph_order().is_ok());
}

#[test]
fn removing_the_output_after_set_output_moves_it_upstream() {
    let (mut s, c, a, b, _y, _m) = branched();
    s.execute("grade.setOutput", json!({"clip": c.0, "node": b})).unwrap();
    let before = grade(&s, c);
    s.execute("grade.removeNode", json!({"clip": c.0, "node": b})).unwrap();
    let g = grade(&s, c);
    assert_eq!(g.output_node(), Ok(a));
    assert!(g.graph_order().is_ok() && g.node(b).is_none());
    // undo / redo after a graph delete
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c), before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(grade(&s, c), g);
}

#[test]
fn removing_a_mixer_with_input_0_free_keeps_its_other_input() {
    let (mut s, c, _a, _b, y, m) = branched();
    s.execute("grade.disconnect", json!({"clip": c.0, "to": m, "toPort": 0})).unwrap();
    s.execute("grade.removeNode", json!({"clip": c.0, "node": m})).unwrap();
    let g = grade(&s, c);
    assert_eq!(g.output_node(), Ok(y), "the mixer's first connected input takes over");
    assert!(g.graph_order().is_ok());
}

#[test]
fn removing_a_key_mixer_splitter_or_combiner_leaves_a_clean_grade() {
    let (mut s, c, a, b) = two();
    let serial = grade(&s, c);
    let k = s.execute("grade.addKeyMixer", json!({"clip": c.0, "node": a})).unwrap()["node"].as_u64().unwrap();
    // a key mixer's output is a key: no serial node after or before it
    assert!(s.execute("grade.addNode", json!({"clip": c.0, "after": k})).is_err());
    assert!(s.execute("grade.insertNode", json!({"clip": c.0, "before": k})).is_err());
    s.execute("grade.removeNode", json!({"clip": c.0, "node": k})).unwrap();
    assert_eq!(grade(&s, c), serial);
    // the splitter takes its combiner and the channel nodes with it, and the other way round
    for which in [0usize, 4] {
        let r = s.execute("grade.addSplitter", json!({"clip": c.0, "node": b})).unwrap();
        let ids: Vec<u64> = r["nodes"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
        let removed = s.execute("grade.removeNode", json!({"clip": c.0, "node": ids[which]})).unwrap();
        assert_eq!(removed["removed"].as_array().map(Vec::len), Some(5), "{removed}");
        assert_eq!(grade(&s, c), serial, "removing node {which} of the pair");
    }
    // a channel node alone goes alone (its channel is then unconnected)
    let r = s.execute("grade.addSplitter", json!({"clip": c.0, "node": b})).unwrap();
    let ids: Vec<u64> = r["nodes"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
    s.execute("grade.removeNode", json!({"clip": c.0, "node": ids[2]})).unwrap();
    let g = grade(&s, c);
    assert!(g.node(ids[0]).is_some() && g.node(ids[4]).is_some() && g.graph_order().is_ok());
}

#[test]
fn connect_refuses_picture_to_key_mismatches() {
    let (mut s, c, a, b, y, m) = branched();
    let before = grade(&s, c);
    for (from, fp, to, tp) in [(a, 1, b, 0), (a, 0, y, 1), (0, 0, y, 1), (y, 1, m, 2)] {
        let e = s.execute("grade.connect", json!({"clip": c.0, "from": from, "fromPort": fp, "to": to, "toPort": tp})).unwrap_err().to_string();
        assert!(e.contains("key") && e.contains("picture"), "{e}");
    }
    assert_eq!(grade(&s, c), before);
    let k = s.execute("grade.addKeyMixer", json!({"clip": c.0, "node": a})).unwrap()["node"].as_u64().unwrap();
    s.execute("grade.connect", json!({"clip": c.0, "from": y, "fromPort": 1, "to": k, "toPort": 1})).unwrap();
    s.execute("grade.connect", json!({"clip": c.0, "from": k, "to": b, "toPort": 1})).unwrap();
    assert!(s.execute("grade.connect", json!({"clip": c.0, "from": k, "to": b, "toPort": 0})).is_err());
}

#[test]
fn insert_before_a_mixer_and_several_nodes_in_one_step() {
    let (mut s, c, _a, b, y, m) = branched();
    let i = s.execute("grade.insertNode", json!({"clip": c.0, "before": m})).unwrap()["node"].as_u64().unwrap();
    let g = grade(&s, c);
    assert!(g.edges.contains(&e(b, 0, i, 0)) && g.edges.contains(&e(i, 0, m, 0)) && g.edges.contains(&e(y, 0, m, 1)));
    assert!(g.graph_order().is_ok());
    let undo = s.history.undo.len();
    s.execute("grade.toggleNode", json!({"clip": c.0, "nodes": [i, y]})).unwrap();
    assert!(!grade(&s, c).node(i).unwrap().enabled && !grade(&s, c).node(y).unwrap().enabled);
    s.execute("grade.removeNode", json!({"clip": c.0, "nodes": [i, y]})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 2, "one undo step each");
    let g = grade(&s, c);
    assert!(g.node(i).is_none() && g.node(y).is_none() && g.graph_order().is_ok());
    for bad in [json!("x"), json!([1, "x"]), json!([]), json!([u64::MAX])] {
        assert!(s.execute("grade.removeNode", json!({"clip": c.0, "nodes": bad})).is_err());
        assert!(s.execute("grade.toggleNode", json!({"clip": c.0, "nodes": bad})).is_err());
    }
    assert_eq!(grade(&s, c), g);
}

#[test]
fn append_and_paste_between_serial_graph_and_empty_grades() {
    let (mut s, c, _a, _b, _y, m) = branched();
    let other = second(&s, c);
    let graph = grade(&s, c);
    // graph → empty: the input's connections map to the destination's (empty) end
    s.execute("grade.copy", json!({"clip": c.0})).unwrap();
    s.execute("grade.append", json!({"clips": [other.0]})).unwrap();
    let g = grade(&s, other);
    assert_eq!(g.nodes.len(), graph.nodes.len());
    assert_eq!(g.edges.iter().filter(|e| e.from.node == SOURCE).count(), 1);
    assert!(g.graph_order().is_ok() && g.output_node().is_ok_and(|o| g.node(o).is_some_and(|n| n.kind == NodeKind::ParallelMixer)));
    // paste of a graph is the same graph
    s.execute("grade.paste", json!({"clips": [other.0]})).unwrap();
    assert_eq!(grade(&s, other), graph);
    // serial → graph: the serial nodes follow the mixer
    s.execute("grade.clear", json!({"clip": other.0})).unwrap();
    add(&mut s, json!({"clip": other.0}));
    add(&mut s, json!({"clip": other.0}));
    s.execute("grade.copy", json!({"clip": other.0})).unwrap();
    s.execute("grade.append", json!({"clips": [c.0]})).unwrap();
    let g = grade(&s, c);
    assert_eq!(g.nodes.len(), graph.nodes.len() + 2);
    let first_new = g.edges.iter().find(|e| e.from == Port::new(m, 0)).map(|e| e.to.node).unwrap();
    assert!(graph.node(first_new).is_none(), "a new node follows the mixer");
    assert_eq!(g.graph_order().unwrap().steps.len(), g.nodes.len() + 1, "every node renders");
    // into a nearly full graph: the output is the last copied node on the picture's path
    for _ in g.nodes.len()..MAX_NODES - 1 {
        add(&mut s, json!({"clip": c.0}));
    }
    s.execute("grade.append", json!({"clips": [c.0]})).unwrap();
    let g = grade(&s, c);
    assert_eq!(g.nodes.len(), MAX_NODES);
    assert_eq!(g.output_node(), Ok(g.nodes.last().unwrap().id));
    assert!(g.graph_order().is_ok());
}

#[test]
fn explain_grade_describes_the_branches() {
    let (mut s, c, _a, b) = two();
    // serial: no branch notes
    let r = s.execute("grade.explainGrade", json!({"clip": c.0})).unwrap();
    assert_eq!(r["branches"].as_array().map(Vec::len), Some(0), "{r}");
    s.execute("grade.addParallel", json!({"clip": c.0, "node": b})).unwrap();
    let r = s.execute("grade.explainGrade", json!({"clip": c.0})).unwrap();
    // the mixer is not a graded node; it is told in words
    assert_eq!(r["nodes"].as_array().map(Vec::len), Some(3), "{r}");
    let text = r["text"].as_str().unwrap();
    assert!(text.contains("their results are blended 50 / 50"), "{text}");
    s.execute("grade.addOutside", json!({"clip": c.0, "node": b})).unwrap();
    let r = s.execute("grade.explainGrade", json!({"clip": c.0})).unwrap();
    assert!(r["text"].as_str().unwrap().contains("works on everything outside"), "{r}");
}

// ---------- node positions (fork C2b) ----------

fn pos(s: &Session, c: ClipId, id: u64) -> Option<[f32; 2]> {
    grade(s, c).stored_pos(id)
}

fn undo_len(s: &Session) -> usize {
    s.history.undo.len()
}

/// A drag (several `grade.moveNodes` calls with one gesture) is one undo step; undo / redo bring
/// the positions back and forth; a second drag is a second step.
#[test]
fn move_nodes_is_one_undo_step_per_drag_with_undo_and_redo() {
    let (mut s, c, a, b) = two();
    let plain = grade(&s, c);
    let u0 = undo_len(&s);
    for (i, x) in [10.0, 20.0, 30.0].into_iter().enumerate() {
        let nodes = if i == 0 {
            json!([{"node": a, "x": x, "y": 5}, {"node": b, "x": 400, "y": 0}, {"node": 0, "x": -200, "y": 0}])
        } else {
            json!([{"node": a, "x": x, "y": 5}])
        };
        s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": nodes, "gesture": 7})).unwrap();
    }
    assert_eq!(undo_len(&s), u0 + 1, "one undo step for the drag");
    assert_eq!((pos(&s, c, a), pos(&s, c, b), pos(&s, c, SOURCE)), (Some([30.0, 5.0]), Some([400.0, 0.0]), Some([-200.0, 0.0])));
    // the layout keeps them
    let l = grade(&s, c).layout();
    assert!(l.contains(&(a, [30.0, 5.0])) && l.contains(&(b, [400.0, 0.0])));
    // another edit leaves them alone
    s.execute("grade.setParam", json!({"clip": c.0, "node": a, "param": "saturation", "value": 0.5})).unwrap();
    assert_eq!(pos(&s, c, a), Some([30.0, 5.0]));
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c), plain);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(pos(&s, c, a), Some([30.0, 5.0]));
    // a new drag (another gesture) is its own step
    let u1 = undo_len(&s);
    s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": [{"node": a, "x": 0, "y": 0}], "gesture": 8})).unwrap();
    assert_eq!(undo_len(&s), u1 + 1);
}

/// Without a `gesture`, every `grade.moveNodes` call is its own undo step (even for the same
/// node), so a scripted move can be undone one call at a time.
#[test]
fn move_nodes_without_gesture_is_one_undo_step_per_call() {
    let (mut s, c, a, _) = two();
    let u0 = undo_len(&s);
    for x in [10.0, 20.0, 30.0] {
        s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": [{"node": a, "x": x, "y": 0}]})).unwrap();
    }
    assert_eq!(undo_len(&s), u0 + 3);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(pos(&s, c, a), Some([20.0, 0.0]));
}

/// Reset Layout clears every position in one undo step and is refused when nothing was placed;
/// a grade never dragged saves exactly as before (no position keys).
#[test]
fn reset_layout_clears_positions_and_undragged_grades_save_as_before() {
    let (mut s, c, a, _) = two();
    let plain = grade(&s, c);
    let json_before = serde_json::to_string(&plain).unwrap();
    assert!(!json_before.contains("pos"), "{json_before}");
    assert!(s.execute("grade.resetLayout", json!({"clip": c.0})).is_err());
    s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": [{"node": a, "x": 50, "y": 60}]})).unwrap();
    assert!(serde_json::to_string(&grade(&s, c)).unwrap().contains(r#""pos":[50.0,60.0]"#));
    s.execute("grade.resetLayout", json!({"clip": c.0})).unwrap();
    assert_eq!(serde_json::to_string(&grade(&s, c)).unwrap(), json_before);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(pos(&s, c, a), Some([50.0, 60.0]));
    // the saved project: no position keys for an undragged grade
    s.execute("grade.resetLayout", json!({"clip": c.0})).unwrap();
    let dir = std::env::temp_dir().join(format!("fc-grade-pos-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.fcproj");
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("\"pos\"") && !text.contains("input_pos"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hostile parameters: non-finite or missing coordinates, unknown nodes, bad lists are refused
/// with nothing changed; huge values are clamped.
#[test]
fn move_nodes_refuses_hostile_values_and_clamps_huge_ones() {
    let (mut s, c, a, _) = two();
    let before = grade(&s, c);
    for nodes in [
        json!([{"node": a, "x": f64::NAN, "y": 0}]),
        json!([{"node": a, "x": "1", "y": 0}]),
        json!([{"node": a, "x": 1}]),
        json!([{"node": 999, "x": 1, "y": 1}]),
        json!([{"node": -1, "x": 1, "y": 1}]),
        json!([]),
        json!("x"),
        Value::Array(vec![json!({"node": a, "x": 1, "y": 1}); 1000]),
    ] {
        assert!(s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": nodes})).is_err(), "{nodes}");
    }
    assert!(s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": [{"node": a, "x": 1, "y": 1}], "gesture": -3})).is_err());
    assert_eq!(grade(&s, c), before);
    s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": [{"node": a, "x": 1e300, "y": -1e300}]})).unwrap();
    let max = filmcraft_project::grade::MAX_POS;
    assert_eq!(pos(&s, c, a), Some([max, -max]));
    // a node added next to a node at the edge stays in range
    let n = add(&mut s, json!({"clip": c.0, "after": a}));
    let p = pos(&s, c, n).unwrap();
    assert!(p.iter().all(|v| v.is_finite() && v.abs() <= max), "{p:?}");
}

/// Once a grade has positions, new nodes (serial or a branch) get one next to their anchor; an
/// undragged grade stays automatic. Copy / paste carry positions; append offsets them to the
/// right of the receiving grade.
#[test]
fn new_nodes_are_placed_near_their_anchor_and_copies_carry_positions() {
    let (mut s, c, a, b) = two();
    add(&mut s, json!({"clip": c.0, "after": b}));
    assert!(!grade(&s, c).has_positions(), "an undragged grade stays automatic");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": [{"node": a, "x": 0, "y": 300}, {"node": b, "x": 200, "y": 300}]})).unwrap();
    let n2 = add(&mut s, json!({"clip": c.0, "after": b}));
    let p = pos(&s, c, n2).unwrap();
    assert!((p[0] - (200.0 + filmcraft_project::grade::LAYOUT_COLUMN)).abs() < 1e-3 && (p[1] - 300.0).abs() < 1e-3, "{p:?}");
    let r = s.execute("grade.addParallel", json!({"clip": c.0, "node": b})).unwrap();
    let (y, m) = (r["nodes"][0].as_u64().unwrap(), r["nodes"][1].as_u64().unwrap());
    let (py, pm) = (pos(&s, c, y).unwrap(), pos(&s, c, m).unwrap());
    assert!((py[0] - 200.0).abs() < 1e-3 && py[1] > 300.0, "the parallel node sits below its twin: {py:?}");
    assert!(pm[0] > 200.0, "the mixer sits right of the node: {pm:?}");
    // every node has a distinct spot
    let l = grade(&s, c).layout();
    for (i, (_, p)) in l.iter().enumerate() {
        for (_, q) in l.iter().skip(i + 1) {
            assert!(p != q, "{l:?}");
        }
    }
    // copy / paste carry positions
    let g = grade(&s, c);
    let other = second(&s, c);
    s.execute("grade.copy", json!({"clip": c.0})).unwrap();
    s.execute("grade.paste", json!({"clips": [other.0]})).unwrap();
    assert_eq!(grade(&s, other), g);
    // append: the appended nodes go to the right of what is there
    s.execute("grade.append", json!({"clips": [other.0]})).unwrap();
    let ga = grade(&s, other);
    let right = g.layout().iter().map(|(_, p)| p[0]).fold(f32::MIN, f32::max);
    let new: Vec<_> = ga.nodes.iter().filter(|n| g.node(n.id).is_none()).collect();
    assert_eq!(new.len(), g.nodes.len());
    assert!(new.iter().all(|n| n.pos.is_some_and(|p| p[0] > right)), "{:?}", new.iter().map(|n| n.pos).collect::<Vec<_>>());
}
