//! `comp.*` commands (fork C1): building a comp, graph rules, parameters, undo, hostile params.

use super::*;
use filmcraft_project::comp::{CompKind, MAX_LABEL};
use filmcraft_project::{ItemKind, ParamValue};
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn media(s: &Session) -> u64 {
    s.project.items.values().find(|i| matches!(i.kind, ItemKind::Media(_)) && i.has_video()).unwrap().id.0
}

fn add(s: &mut Session, comp: u64, params: serde_json::Value) -> u64 {
    let mut p = params;
    p["comp"] = json!(comp);
    s.execute("comp.addNode", p).unwrap()["node"].as_u64().unwrap()
}

fn connect(s: &mut Session, comp: u64, from: u64, to: u64, port: u8) {
    s.execute("comp.connect", json!({"comp": comp, "from": from, "to": to, "toPort": port})).unwrap();
}

fn get(s: &mut Session, comp: u64) -> serde_json::Value {
    s.execute("comp.get", json!({"comp": comp})).unwrap()
}

/// The command fails and changes nothing (project and undo history).
fn refused(s: &mut Session, command: &str, params: serde_json::Value) -> String {
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let e = s.execute(command, params).expect_err(command).to_string();
    assert!(Arc::ptr_eq(&before, &s.project), "{command} left the project alone");
    assert_eq!(s.history.undo.len(), undo, "{command} added no undo step");
    e
}

/// Acceptance comp A's shape: Media In → Key → Merge over a Solid → Color → Out.
fn comp_a(s: &mut Session) -> (u64, [u64; 5]) {
    let item = media(s);
    let c = s.execute("comp.new", json!({"name": "A"})).unwrap()["comp"].as_u64().unwrap();
    let m = add(s, c, json!({"kind": "mediaIn", "item": item}));
    let k = add(s, c, json!({"kind": "key", "effect": "luma_key"}));
    let so = add(s, c, json!({"kind": "solid"}));
    let mg = add(s, c, json!({"kind": "merge"}));
    let col = add(s, c, json!({"kind": "color"}));
    connect(s, c, m, k, 0);
    connect(s, c, so, mg, 0);
    connect(s, c, k, mg, 1);
    connect(s, c, mg, col, 0);
    connect(s, c, col, 1, 0);
    (c, [m, k, so, mg, col])
}

#[test]
fn comp_a_builds_and_validates() {
    let mut s = demo();
    let (c, [m, k, so, mg, col]) = comp_a(&mut s);
    let g = get(&mut s, c);
    assert_eq!(g["error"], serde_json::Value::Null);
    assert_eq!(g["order"], json!([m, k, so, mg, col, 1]));
    assert_eq!(g["width"], json!(s.active_sequence().unwrap().settings.width));
    // a comp item is a project item with a picture
    let it = s.project.item(ItemId(c)).unwrap();
    assert!(it.has_video() && it.type_label() == "Comp");
}

#[test]
fn add_after_splices_and_remove_bridges() {
    let mut s = demo();
    let (c, [m, k, ..]) = comp_a(&mut s);
    let b = add(&mut s, c, json!({"kind": "blur", "after": m}));
    let g = get(&mut s, c);
    let edges = g["edges"].as_array().unwrap();
    assert!(edges.contains(&json!({"from": {"node": m, "index": 0}, "to": {"node": b, "index": 0}})));
    assert!(edges.contains(&json!({"from": {"node": b, "index": 0}, "to": {"node": k, "index": 0}})));
    s.execute("comp.removeNode", json!({"comp": c, "nodes": [b]})).unwrap();
    let g = get(&mut s, c);
    assert!(g["edges"].as_array().unwrap().contains(&json!({"from": {"node": m, "index": 0}, "to": {"node": k, "index": 0}})));
    assert_eq!(g["error"], serde_json::Value::Null);
}

#[test]
fn cycles_bad_ports_and_unknown_nodes_are_refused() {
    let mut s = demo();
    let (c, [m, k, _, mg, _]) = comp_a(&mut s);
    let e = refused(&mut s, "comp.connect", json!({"comp": c, "from": mg, "to": k}));
    assert!(e.contains("cycle"), "{e}");
    refused(&mut s, "comp.connect", json!({"comp": c, "from": m, "to": mg, "toPort": 2}));
    refused(&mut s, "comp.connect", json!({"comp": c, "from": 999, "to": mg}));
    refused(&mut s, "comp.addNode", json!({"comp": c, "kind": "warp"}));
    refused(&mut s, "comp.addNode", json!({"comp": c, "kind": "key", "effect": "nope"}));
    refused(&mut s, "comp.addNode", json!({"comp": c, "kind": "blur", "pos": [1e300, 0]}));
    refused(&mut s, "comp.removeNode", json!({"comp": c, "nodes": [999]}));
    refused(&mut s, "comp.disconnect", json!({"comp": c, "to": m}));
    refused(&mut s, "comp.setOutput", json!({"comp": c, "node": 999}));
    refused(&mut s, "comp.setSize", json!({"comp": c, "width": 0}));
    let m = media(&s);
    refused(&mut s, "comp.get", json!({"comp": m}));
    refused(&mut s, "comp.new", json!({"width": 100000}));
}

#[test]
fn a_comp_cannot_read_itself() {
    let mut s = demo();
    let (c, _) = comp_a(&mut s);
    let e = refused(&mut s, "comp.addNode", json!({"comp": c, "kind": "mediaIn", "item": c}));
    assert!(e.contains("nested inside itself"), "{e}");
}

#[test]
fn parameters_are_typed_clamped_and_one_undo_step_per_drag() {
    let mut s = demo();
    let (c, [_, k, so, mg, col]) = comp_a(&mut s);
    let undo = s.history.undo.len();
    s.execute("comp.setParam", json!({"comp": c, "node": mg, "param": "opacity", "value": 30.0})).unwrap();
    s.execute("comp.setParam", json!({"comp": c, "node": mg, "param": "opacity", "value": 250.0})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1, "a drag is one undo step");
    assert_eq!(
        get(&mut s, c)["nodes"].as_array().unwrap().iter().find(|n| n["id"] == json!(mg)).unwrap()["params"]["opacity"]["value"],
        json!({"Float": 100.0})
    );
    // wrong types, out-of-range choices, unknown keys
    refused(&mut s, "comp.setParam", json!({"comp": c, "node": mg, "param": "opacity", "value": "x"}));
    refused(&mut s, "comp.setParam", json!({"comp": c, "node": mg, "param": "blend", "value": 9999}));
    refused(&mut s, "comp.setParam", json!({"comp": c, "node": mg, "param": "nope", "value": 1}));
    refused(&mut s, "comp.setParam", json!({"comp": c, "node": col, "param": "tnr.luma", "value": 1}));
    // a Color node takes the grade's keys, a Key node its effect's, a Solid a colour
    s.execute("comp.setParam", json!({"comp": c, "node": col, "param": "saturation", "value": 0.5})).unwrap();
    s.execute("comp.setParam", json!({"comp": c, "node": k, "param": "threshold", "value": 40.0})).unwrap();
    s.execute("comp.setParam", json!({"comp": c, "node": so, "param": "color", "value": "#ff8000"})).unwrap();
    let Some(ItemKind::Comp(cc)) = s.project.item(ItemId(c)).map(|i| &i.kind) else { panic!("comp") };
    let CompKind::Color { node } = &cc.node(col).unwrap().kind else { panic!("color") };
    assert_eq!(node.params["saturation"].value, ParamValue::Float(0.5));
    // reset removes the stored value
    s.execute("comp.setParam", json!({"comp": c, "node": mg, "param": "opacity", "reset": true})).unwrap();
    assert!(get(&mut s, c)["nodes"].as_array().unwrap().iter().find(|n| n["id"] == json!(mg)).unwrap()["params"].get("opacity").is_none());
}

#[test]
fn keyframes_text_and_animation() {
    let mut s = demo();
    let c = s.execute("comp.new", json!({})).unwrap()["comp"].as_u64().unwrap();
    let t = add(&mut s, c, json!({"kind": "text"}));
    let tr = add(&mut s, c, json!({"kind": "transform", "after": t}));
    s.execute("comp.setParam", json!({"comp": c, "node": t, "param": "text", "value": "Title"})).unwrap();
    s.execute(
        "comp.setParam",
        json!({"comp": c, "node": tr, "param": "position", "keyframes": [{"time": 1000, "value": [50, 20]}, {"time": 0, "value": [10, 20]}]}),
    )
    .unwrap();
    let Some(ItemKind::Comp(cc)) = s.project.item(ItemId(c)).map(|i| &i.kind) else { panic!("comp") };
    let keys = &cc.node(tr).unwrap().params["position"].keyframes;
    assert_eq!(keys.iter().map(|k| k.time.0).collect::<Vec<_>>(), vec![0, 1000], "sorted");
    let CompKind::Text { layer } = &cc.node(t).unwrap().kind else { panic!("text") };
    assert_eq!(layer.params["text"].value, ParamValue::Text("Title".into()));
    let r = s.execute("comp.toggleAnimation", json!({"comp": c, "node": tr, "param": "rotation", "time": 0})).unwrap();
    assert_eq!(r["animated"], json!(true));
    refused(&mut s, "comp.setParam", json!({"comp": c, "node": tr, "param": "position", "keyframes": [{"time": 0.5, "value": [1, 1]}]}));
}

#[test]
fn undo_redo_and_labels() {
    let mut s = demo();
    let (c, [m, ..]) = comp_a(&mut s);
    let label = "x".repeat(500);
    s.execute("comp.labelNode", json!({"comp": c, "node": m, "label": label})).unwrap();
    let Some(ItemKind::Comp(cc)) = s.project.item(ItemId(c)).map(|i| &i.kind) else { panic!("comp") };
    assert_eq!(cc.node(m).unwrap().label.chars().count(), MAX_LABEL);
    let before = (*s.project).clone();
    s.execute("comp.bypass", json!({"comp": c, "node": m})).unwrap();
    s.execute("comp.moveNode", json!({"comp": c, "node": m, "pos": [10, 20]})).unwrap();
    s.execute("comp.moveNode", json!({"comp": c, "node": m, "pos": [30, 40]})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, before, "bypass + one merged move undone");
    s.execute("edit.redo", json!({})).unwrap();
    let Some(ItemKind::Comp(cc)) = s.project.item(ItemId(c)).map(|i| &i.kind) else { panic!("comp") };
    assert!(cc.node(m).unwrap().bypass);
}

#[test]
fn new_clip_places_a_comp_in_one_undo_step() {
    let mut s = demo();
    let before = (*s.project).clone();
    let undo = s.history.undo.len();
    let r = s.execute("comp.newClip", json!({"track": "V2", "time": 0})).unwrap();
    let comp = ItemId(r["comp"].as_u64().unwrap());
    assert_eq!(s.history.undo.len(), undo + 1);
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[1].items.iter().any(|i| i.item == comp && i.start == Tick::ZERO));
    // the selected comp clip is the default `comp`
    let clip = q.video_tracks[1].items.iter().find(|i| i.item == comp).unwrap().id;
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    assert_eq!(s.execute("comp.get", json!({})).unwrap()["comp"], json!(comp.0));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, before);
    // a bad track: nothing made, nothing to undo or redo
    let undo = s.history.undo.len();
    let items = s.project.items.len();
    assert!(s.execute("comp.newClip", json!({"track": "V99"})).is_err());
    assert_eq!((s.history.undo.len(), s.project.items.len()), (undo, items));
}

#[test]
fn mask_nodes_take_masks() {
    let mut s = demo();
    let c = s.execute("comp.new", json!({})).unwrap()["comp"].as_u64().unwrap();
    let m = add(&mut s, c, json!({"kind": "mask"}));
    let mask = filmcraft_project::Mask::linear("m", filmcraft_geom::Vec2::new(0.0, 0.0), filmcraft_geom::Vec2::new(10.0, 10.0));
    s.execute("comp.setMasks", json!({"comp": c, "node": m, "masks": [serde_json::to_value(&mask).unwrap()]})).unwrap();
    let Some(ItemKind::Comp(cc)) = s.project.item(ItemId(c)).map(|i| &i.kind) else { panic!("comp") };
    assert!(matches!(&cc.node(m).unwrap().kind, CompKind::Mask { masks } if masks.len() == 1));
    refused(&mut s, "comp.setMasks", json!({"comp": c, "node": m, "masks": [{"bogus": 1}]}));
    refused(&mut s, "comp.setMasks", json!({"comp": c, "node": 1, "masks": []}));
    let many: Vec<_> = (0..17).map(|_| serde_json::to_value(&mask).unwrap()).collect();
    refused(&mut s, "comp.setMasks", json!({"comp": c, "node": m, "masks": many}));
}

/// A 1280x720 sequence with the demo footage (1920x1080 or so: fitted into the comp) on V1 and V2
/// over the same second range, picture only. Returns the two clips (V1, V2).
fn stacked(s: &mut Session) -> [u64; 2] {
    let item = media(s);
    let q = s.execute("file.newSequence", json!({"name": "Stack", "width": 1280, "height": 720})).unwrap()["sequence"].as_u64().unwrap();
    s.execute("sequence.open", json!({"item": q})).unwrap();
    let put = |s: &mut Session, track: &str| {
        s.execute("timeline.place", json!({"item": item, "track": track, "seconds": 0.0, "audio": false})).unwrap()["clips"][0].as_u64().unwrap()
    };
    [put(s, "V1"), put(s, "V2")]
}

fn frame(s: &Session, t: Tick) -> filmcraft_render::Image {
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, ..Default::default() };
    filmcraft_render::render_sequence(&s.project, s.state.active_sequence.unwrap(), t, opts, &provider)
}

#[test]
fn convert_to_comp_clip_keeps_the_picture_and_is_one_undo_step() {
    let mut s = demo();
    let [v1, v2] = stacked(&mut s);
    for (param, value) in [("scale", json!(40.0)), ("position", json!([300.0, 200.0])), ("rotation", json!(15.0))] {
        s.execute("effects.setParam", json!({"clip": v2, "effect": "motion", "param": param, "value": value})).unwrap();
    }
    s.execute("effects.setParam", json!({"clip": v2, "effect": "opacity", "param": "opacity", "value": 60.0})).unwrap();
    let t = Tick(filmcraft_time::TICKS_PER_SECOND / 2);
    let before_img = frame(&s, t);
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let r = s.execute("comp.convertToCompClip", json!({"clips": [v2, v1], "name": "Stack comp"})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    let comp = r["comp"].as_u64().unwrap();
    let clip = filmcraft_project::ClipId(r["clip"].as_u64().unwrap());
    assert_eq!(s.state.selection, vec![clip]);
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items.len(), 1);
    assert!(q.video_tracks[1].items.is_empty());
    assert_eq!(q.video_tracks[0].items[0].id, clip);
    let g = get(&mut s, comp);
    let kinds: Vec<&str> = g["nodes"].as_array().unwrap().iter().filter_map(|n| n["kind"]["type"].as_str()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "mediaIn").count(), 2, "{kinds:?}");
    assert_eq!(kinds.iter().filter(|k| **k == "transform").count(), 2, "{kinds:?}");
    assert_eq!(kinds.iter().filter(|k| **k == "merge").count(), 1, "{kinds:?}");
    // the same frame as the stack (resampled through the comp's fitted Media In)
    let after_img = frame(&s, t);
    assert_eq!((before_img.w, before_img.h), (after_img.w, after_img.h));
    let (mut sum, mut n) = (0.0f64, 0usize);
    for y in (2..after_img.h.saturating_sub(2)).step_by(3) {
        for x in (2..after_img.w.saturating_sub(2)).step_by(3) {
            let (a, b) = (before_img.get(x, y), after_img.get(x, y));
            sum += a.iter().zip(&b).map(|(p, q)| f64::from((p - q).abs())).sum::<f64>();
            n += 4;
        }
    }
    let mean = sum / n.max(1) as f64;
    let lit = (0..after_img.h).step_by(7).flat_map(|y| (0..after_img.w).step_by(7).map(move |x| (x, y))).filter(|&(x, y)| after_img.get(x, y)[3] > 0.5).count();
    assert!(lit > 20, "the frame shows the footage");
    assert!(mean < 0.02, "mean difference {mean}");
    // one undo step brings the stack back
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
}

#[test]
fn convert_to_comp_clip_refuses_what_it_would_lose() {
    let mut s = demo();
    let [v1, v2] = stacked(&mut s);
    assert!(refused(&mut s, "comp.convertToCompClip", json!({"clips": []})).contains("select"));
    assert!(refused(&mut s, "comp.convertToCompClip", json!({"clips": [987654321u64]})).contains("no clip"));
    // an effect on one of them
    s.execute("effects.apply", json!({"clips": [v2], "effect": "gaussian_blur"})).unwrap();
    assert!(refused(&mut s, "comp.convertToCompClip", json!({"clips": [v1, v2]})).contains("effect"));
    s.execute("edit.undo", json!({})).unwrap();
    // animated Motion
    s.execute("effects.toggleAnimation", json!({"clip": v2, "effect": "motion", "param": "scale"})).unwrap();
    assert!(s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(v2)).unwrap().1.effect("motion").unwrap().param("scale").unwrap().is_animated());
    assert!(refused(&mut s, "comp.convertToCompClip", json!({"clips": [v1, v2]})).contains("animated"));
    s.execute("edit.undo", json!({})).unwrap();
    // different times
    let item = media(&s);
    let late = s.execute("timeline.place", json!({"item": item, "track": "V3", "seconds": 60.0, "audio": false})).unwrap()["clips"][0].as_u64().unwrap();
    assert!(refused(&mut s, "comp.convertToCompClip", json!({"clips": [v1, late]})).contains("same time"));
    // two on one track
    let next = s.execute("timeline.place", json!({"item": item, "track": "V1", "seconds": 60.0, "audio": false})).unwrap()["clips"][0].as_u64().unwrap();
    assert!(refused(&mut s, "comp.convertToCompClip", json!({"clips": [v1, next]})).contains("stacked"));
    // linked sound
    let linked = s.execute("timeline.place", json!({"item": item, "track": "V2", "seconds": 600.0})).unwrap()["clips"].clone();
    let ids: Vec<u64> = linked.as_array().unwrap().iter().filter_map(serde_json::Value::as_u64).collect();
    let q = s.active_sequence().unwrap();
    let pic = ids.iter().copied().find(|c| q.video_tracks.iter().any(|t| t.items.iter().any(|i| i.id.0 == *c))).unwrap();
    let snd = ids.iter().copied().find(|c| *c != pic).unwrap();
    assert!(refused(&mut s, "comp.convertToCompClip", json!({"clips": [pic]})).contains("linked"));
    assert!(refused(&mut s, "comp.convertToCompClip", json!({"clips": [snd]})).contains("sound clip"));
}

/// `tools/comp-accept.json` (the `command_batch` that builds acceptance comps A and B over the
/// footage on V1, DESIGN-C1 §5) runs step by step on the demo, as the MCP server runs a batch.
#[test]
fn the_acceptance_batch_builds_comps_a_and_b() {
    let mut s = demo();
    let batch: serde_json::Value = serde_json::from_str(include_str!("../../../tools/comp-accept.json")).unwrap();
    for step in batch["steps"].as_array().unwrap() {
        let id = step["id"].as_str().unwrap();
        s.execute(id, step.get("params").cloned().unwrap_or(json!({}))).unwrap_or_else(|e| panic!("{id} {}: {e}", step["params"]));
    }
    let q = s.active_sequence().unwrap();
    let comps: Vec<(usize, ItemId)> = q
        .video_tracks
        .iter()
        .enumerate()
        .flat_map(|(n, t)| t.items.iter().map(move |i| (n, i.item)))
        .filter(|(_, i)| matches!(s.project.item(*i).map(|it| &it.kind), Some(ItemKind::Comp(_))))
        .collect();
    assert_eq!(comps.iter().map(|c| c.0).collect::<Vec<_>>(), vec![1, 2], "comp A on V2, comp B on V3");
    for ((_, item), want) in
        comps.iter().zip([vec!["mediaOut", "mediaIn", "key", "solid", "merge", "color"], vec!["mediaOut", "text", "transform", "mediaIn", "merge"]])
    {
        let g = get(&mut s, item.0);
        let kinds: Vec<&str> = g["nodes"].as_array().unwrap().iter().filter_map(|n| n["kind"]["type"].as_str()).collect();
        assert_eq!(kinds, want);
        let Some(ItemKind::Comp(c)) = s.project.item(*item).map(|i| &i.kind) else { panic!("comp") };
        assert!(filmcraft_project::dag::validate(&c.nodes, &c.edges, 1).is_ok(), "{:?}", c.edges);
        assert_eq!(c.edges.len(), want.len() - 1, "every node feeds Media Out (a tree)");
    }
    // both render: the comps' pictures are in the frame
    let img = frame(&s, Tick(filmcraft_time::TICKS_PER_SECOND));
    assert!(img.get(img.w / 2, img.h / 2)[3] > 0.5);
}

/// C1 item 7: `comp.explain` describes comps A and B node by node in flow order, names only nodes
/// that exist, lists changed parameters by their real labels, and reports unconnected nodes and
/// broken graphs instead of failing.
#[test]
fn comp_explain_describes_the_acceptance_comps() {
    let mut s = demo();
    let batch: serde_json::Value = serde_json::from_str(include_str!("../../../tools/comp-accept.json")).unwrap();
    for step in batch["steps"].as_array().unwrap() {
        let id = step["id"].as_str().unwrap();
        s.execute(id, step.get("params").cloned().unwrap_or(json!({}))).unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    let q = s.active_sequence().unwrap();
    let comps: Vec<ItemId> = q
        .video_tracks
        .iter()
        .flat_map(|t| t.items.iter().map(|i| i.item))
        .filter(|i| matches!(s.project.item(*i).map(|it| &it.kind), Some(ItemKind::Comp(_))))
        .collect();
    assert_eq!(comps.len(), 2);
    for item in &comps {
        let e = s.execute("comp.explain", json!({"comp": item.0})).unwrap();
        let lines = e["lines"].as_array().unwrap();
        let Some(ItemKind::Comp(c)) = s.project.item(*item).map(|i| &i.kind) else { panic!("comp") };
        assert_eq!(lines.len(), c.nodes.len(), "{e}");
        // flow order: sources first, Media Out last
        assert!(lines.last().unwrap()["text"].as_str().unwrap().starts_with("is the comp's result"), "{e}");
        let names: Vec<String> = c.nodes.iter().map(crate::comp_explain::node_name).collect();
        for l in lines {
            assert!(names.contains(&l["name"].as_str().unwrap().to_string()), "{l}");
        }
        assert!(e["unused"].as_array().unwrap().is_empty());
        assert!(e["summary"].as_str().unwrap().contains("Media Out"), "{e}");
    }
    // comp A: the Merge names its two inputs as foreground and background
    let a = s.execute("comp.explain", json!({"comp": comps[0].0})).unwrap();
    assert!(a["lines"].as_array().unwrap().iter().any(|l| l["text"].as_str().unwrap().contains("(foreground) over")), "{a}");
    // a node that doesn't reach Media Out is listed as unused; a changed Transform names its labels
    let id = comps[1].0;
    let n = s.execute("comp.addNode", json!({"comp": id, "kind": "blur"})).unwrap();
    let blur = n["node"].as_u64().unwrap_or_else(|| n.as_u64().unwrap());
    let e = s.execute("comp.explain", json!({"comp": id})).unwrap();
    assert_eq!(e["unused"][0]["node"], json!(blur), "{e}");
    assert!(e["unused"][0]["text"].as_str().unwrap().contains("isn't connected"));
    // a cycle: the summary says why, no lines
    let Some(ItemKind::Comp(c)) = s.project.item(comps[1]).map(|i| &i.kind) else { panic!("comp") };
    let mut broken = (**c).clone();
    let first = broken.nodes.iter().find(|n| n.kind.inputs() > 0).map(|n| n.id).unwrap();
    broken.edges.push(filmcraft_project::dag::Edge::new(filmcraft_project::dag::Port::new(first, 0), filmcraft_project::dag::Port::new(first, 0)));
    let e = crate::comp_explain::explain(&broken, &s.project, Tick::ZERO);
    assert!(e.lines.is_empty() && e.summary.contains("can't be shown"), "{e:?}");
    // hostile: an unknown comp id is an error, not a panic
    assert!(s.execute("comp.explain", json!({"comp": 999_999})).is_err());
}

/// A new comp's Media Out sits on the right, and nodes added without a position don't land on
/// top of each other (seen on screen 2026-10-14: Media Out hidden under the first node).
#[test]
fn new_nodes_do_not_stack_on_each_other() {
    let mut s = demo();
    let id = s.execute("comp.new", json!({})).unwrap()["comp"].as_u64().unwrap();
    for kind in ["solid", "blur", "merge"] {
        s.execute("comp.addNode", json!({"comp": id, "kind": kind})).unwrap();
    }
    let Some(ItemKind::Comp(c)) = s.project.item(ItemId(id)).map(|i| &i.kind) else { panic!("comp") };
    let pos: Vec<[f32; 2]> = c.nodes.iter().map(|n| n.pos).collect();
    assert_eq!(pos[0], [filmcraft_project::comp::MEDIA_OUT_X, 0.0]);
    for (i, a) in pos.iter().enumerate() {
        assert!(pos.iter().skip(i + 1).all(|b| a != b), "{pos:?}");
    }
}

/// `comp.setMasks` with the same `gesture` merges into one undo step (a drag on the monitor).
#[test]
fn set_masks_merges_one_gesture_into_one_undo_step() {
    let mut s = demo();
    let id = s.execute("comp.new", json!({})).unwrap()["comp"].as_u64().unwrap();
    let node = s.execute("comp.addNode", json!({"comp": id, "kind": "mask"})).unwrap()["node"].as_u64().unwrap();
    let mask = |x: f64| json!([filmcraft_project::Mask::new("m".to_string(), filmcraft_project::MaskPath::rect(x, 0.0, x + 10.0, 10.0))]);
    let depth = s.history.undo.len();
    for x in [1.0, 2.0, 3.0] {
        s.execute("comp.setMasks", json!({"comp": id, "node": node, "masks": mask(x), "gesture": 7})).unwrap();
    }
    assert_eq!(s.history.undo.len(), depth + 1);
    s.execute("comp.setMasks", json!({"comp": id, "node": node, "masks": mask(4.0), "gesture": 8})).unwrap();
    s.execute("comp.setMasks", json!({"comp": id, "node": node, "masks": mask(5.0)})).unwrap();
    assert_eq!(s.history.undo.len(), depth + 3);
}

// -- C3.5b: Roto and Paint nodes ---------------------------------------------------------------

fn comp_node(s: &Session, comp: u64, node: u64) -> filmcraft_project::comp::CompNode {
    let Some(ItemKind::Comp(c)) = s.project.item(ItemId(comp)).map(|i| &i.kind) else { panic!("comp") };
    c.node(node).unwrap().clone()
}

fn roto(s: &Session, comp: u64, node: u64) -> filmcraft_project::roto::Roto {
    match comp_node(s, comp, node).kind {
        CompKind::Roto { roto, .. } => roto,
        k => panic!("not roto: {k:?}"),
    }
}

fn paint(s: &Session, comp: u64, node: u64) -> filmcraft_project::paint::Paint {
    match comp_node(s, comp, node).kind {
        CompKind::Paint { paint } => paint,
        k => panic!("not paint: {k:?}"),
    }
}

/// Each command one undo step; per-point feather through the path JSON; hostile params refused.
#[test]
fn roto_commands_edit_shapes_as_undo_steps_and_refuse_hostile_params() {
    let mut s = demo();
    let c = s.execute("comp.new", json!({"width": 640, "height": 360})).unwrap()["comp"].as_u64().unwrap();
    let r = add(&mut s, c, json!({"kind": "roto"}));
    let depth = s.history.undo.len();
    let i = s.execute("comp.roto.addShape", json!({"comp": c, "node": r})).unwrap()["shape"].as_u64().unwrap();
    assert_eq!(i, 0);
    assert_eq!(roto(&s, c, r).shapes[0].name, "Shape 1");
    assert_eq!(roto(&s, c, r).shapes[0].path_at(Tick(0)).len(), 4, "the default ellipse");
    let path = json!([
        {"p": [10, 10], "feather_in": 2, "feather_out": 6},
        {"p": [100, 10], "featherIn": 1},
        {"p": [100, 100]},
        [10, 100]
    ]);
    let j = s
        .execute("comp.roto.addShape", json!({"comp": c, "node": r, "path": path, "kind": "bSpline", "mode": "subtract", "feather": 3.0, "time": 0}))
        .unwrap()["shape"]
        .as_u64()
        .unwrap();
    assert_eq!(j, 1);
    let sh = roto(&s, c, r).shapes[1].clone();
    assert_eq!(sh.kind, filmcraft_project::roto::RotoKind::BSpline);
    assert_eq!(sh.mode, filmcraft_project::roto::RotoMode::Subtract);
    assert_eq!(sh.feather_at(Tick(0)), 3.0);
    let v = sh.path_at(Tick(0)).vertices;
    assert_eq!((v[0].feather_in, v[0].feather_out, v[1].feather_in), (2.0, 6.0, 1.0));
    assert!(sh.is_animated());
    s.execute("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 1, "time": 1000, "path": [[0, 0], [50, 0], [50, 50], [0, 50]]})).unwrap();
    let sh = roto(&s, c, r).shapes[1].clone();
    assert_eq!(sh.shape.keyframes.len(), 2);
    assert_eq!(sh.path_at(Tick(1000)).vertices[1].p, filmcraft_geom::Vec2::new(50.0, 0.0));
    // the whole roto and the output
    let mut whole = roto(&s, c, r);
    whole.shapes[0].inverted = true;
    s.execute("comp.setRoto", json!({"comp": c, "node": r, "roto": whole, "output": "matte"})).unwrap();
    assert!(matches!(comp_node(&s, c, r).kind, CompKind::Roto { output: filmcraft_project::comp::RotoOutput::Matte, ref roto } if roto.shapes[0].inverted));
    s.execute("comp.roto.removeShape", json!({"comp": c, "node": r, "shape": 0})).unwrap();
    assert_eq!(roto(&s, c, r).shapes.len(), 1);
    assert_eq!(s.history.undo.len(), depth + 5, "one undo step each");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(roto(&s, c, r).shapes.len(), 2);

    let solid = add(&mut s, c, json!({"kind": "solid"}));
    for (cmd, p) in [
        ("comp.roto.addShape", json!({"comp": c, "node": solid})),
        ("comp.roto.addShape", json!({"comp": c, "node": r, "kind": "nurbs"})),
        ("comp.roto.addShape", json!({"comp": c, "node": r, "mode": 3})),
        ("comp.roto.addShape", json!({"comp": c, "node": r, "feather": -1.0})),
        ("comp.roto.addShape", json!({"comp": c, "node": r, "at": 99})),
        ("comp.roto.addShape", json!({"comp": c, "node": r, "at": -1})),
        ("comp.roto.addShape", json!({"comp": c, "node": r, "path": "x"})),
        ("comp.roto.addShape", json!({"comp": c, "node": r, "path": vec![[0, 0]; filmcraft_project::roto::MAX_POINTS + 1]})),
        ("comp.roto.addShape", json!({"comp": c, "node": r, "shape": {"shape": 5}})),
        ("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 9, "time": 0, "path": [[0, 0], [1, 0], [0, 1]]})),
        ("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 0, "time": 0.5, "path": [[0, 0], [1, 0], [0, 1]]})),
        ("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 0, "path": [[0, 0], [1, 0], [0, 1]]})),
        ("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 0, "time": 0})),
        ("comp.roto.removeShape", json!({"comp": c, "node": r, "shape": 5})),
        ("comp.roto.removeShape", json!({"comp": c, "node": r, "shape": 1.5})),
        ("comp.setRoto", json!({"comp": c, "node": r})),
        ("comp.setRoto", json!({"comp": c, "node": r, "output": "luma"})),
        ("comp.setRoto", json!({"comp": c, "node": r, "roto": {"shapes": 3}})),
        ("comp.setRoto", json!({"comp": c, "node": solid, "output": "alpha"})),
        ("comp.setRoto", json!({"comp": c, "node": r, "roto": {"shapes": vec![json!({}); filmcraft_project::roto::MAX_SHAPES + 1]}})),
    ] {
        refused(&mut s, cmd, p);
    }
    // hostile numbers inside an accepted shape are cleaned
    let hostile = json!({"name": "x".repeat(500), "feather": {"value": {"Float": 1e300}}, "opacity": {"value": {"Float": -5.0}}});
    s.execute("comp.setRoto", json!({"comp": c, "node": r, "roto": {"shapes": [hostile]}})).unwrap();
    let sh = roto(&s, c, r).shapes[0].clone();
    assert!(sh.name.chars().count() <= filmcraft_project::roto::MAX_NAME);
    assert!(sh.feather_at(Tick(0)) <= filmcraft_project::roto::MAX_FEATHER && sh.opacity_at(Tick(0)) == 0.0);
}

#[test]
fn paint_commands_add_remove_reorder_strokes_and_refuse_hostile_params() {
    use filmcraft_project::paint::{MAX_STROKE_POINTS, MAX_STROKES, PaintTool};
    let mut s = demo();
    let c = s.execute("comp.new", json!({})).unwrap()["comp"].as_u64().unwrap();
    let n = add(&mut s, c, json!({"kind": "paint"}));
    let depth = s.history.undo.len();
    let stroke =
        |tool: &str, x: f64| json!({"tool": tool, "points": [{"x": x, "y": 5.0}, {"x": x + 50.0, "y": 5.0, "pressure": 0.5}], "brush": {"size": 12.0}});
    assert_eq!(s.execute("comp.paint.addStroke", json!({"comp": c, "node": n, "stroke": stroke("color", 0.0)})).unwrap()["stroke"], 0);
    assert_eq!(s.execute("comp.paint.addStroke", json!({"comp": c, "node": n, "stroke": stroke("erase", 10.0)})).unwrap()["stroke"], 1);
    assert_eq!(s.execute("comp.paint.addStroke", json!({"comp": c, "node": n, "stroke": stroke("clone", 20.0), "at": 0})).unwrap()["stroke"], 0);
    let tools = |s: &Session| paint(s, c, n).strokes.iter().map(|x| x.tool).collect::<Vec<_>>();
    assert_eq!(tools(&s), vec![PaintTool::Clone, PaintTool::Color, PaintTool::Erase]);
    s.execute("comp.paint.reorder", json!({"comp": c, "node": n, "from": 0, "to": 2})).unwrap();
    assert_eq!(tools(&s), vec![PaintTool::Color, PaintTool::Erase, PaintTool::Clone]);
    s.execute("comp.paint.removeStroke", json!({"comp": c, "node": n, "stroke": 1})).unwrap();
    assert_eq!(tools(&s), vec![PaintTool::Color, PaintTool::Clone]);
    s.execute("comp.setPaint", json!({"comp": c, "node": n, "paint": {"strokes": [stroke("reveal", 1.0)]}})).unwrap();
    assert_eq!(tools(&s), vec![PaintTool::Reveal]);
    assert_eq!(s.history.undo.len(), depth + 6, "one undo step each");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(tools(&s), vec![PaintTool::Color, PaintTool::Clone]);

    let solid = add(&mut s, c, json!({"kind": "solid"}));
    let long = json!({"points": vec![json!({"x": 0.0, "y": 0.0}); MAX_STROKE_POINTS + 1]});
    for (cmd, p) in [
        ("comp.paint.addStroke", json!({"comp": c, "node": solid, "stroke": stroke("color", 0.0)})),
        ("comp.paint.addStroke", json!({"comp": c, "node": n})),
        ("comp.paint.addStroke", json!({"comp": c, "node": n, "stroke": {"tool": "smudge"}})),
        ("comp.paint.addStroke", json!({"comp": c, "node": n, "stroke": stroke("color", 0.0), "at": 50})),
        ("comp.paint.addStroke", json!({"comp": c, "node": n, "stroke": long})),
        ("comp.paint.removeStroke", json!({"comp": c, "node": n, "stroke": 7})),
        ("comp.paint.removeStroke", json!({"comp": c, "node": n})),
        ("comp.paint.reorder", json!({"comp": c, "node": n, "from": 0, "to": 9})),
        ("comp.paint.reorder", json!({"comp": c, "node": n, "from": -1, "to": 0})),
        ("comp.setPaint", json!({"comp": c, "node": n})),
        ("comp.setPaint", json!({"comp": c, "node": n, "paint": {"strokes": [{"points": 1}]}})),
        ("comp.setPaint", json!({"comp": c, "node": n, "paint": {"strokes": vec![json!({}); MAX_STROKES + 1]}})),
    ] {
        refused(&mut s, cmd, p);
    }
    // hostile numbers inside an accepted stroke are cleaned
    let bad = json!({"points": [{"x": 1e300, "y": 0.0}, {"x": 1.0, "y": 1.0, "pressure": -4.0}], "brush": {"size": 1e9, "spacing": 0.0}, "color": [1e9, 0.0, 0.0, 7.0], "cloneTimeOffset": i64::MAX});
    s.execute("comp.paint.addStroke", json!({"comp": c, "node": n, "stroke": bad})).unwrap();
    let st = paint(&s, c, n).strokes.last().unwrap().clone();
    assert!(st.brush.size <= filmcraft_project::paint::MAX_BRUSH_SIZE && st.brush.spacing >= filmcraft_project::paint::MIN_SPACING);
    assert!(st.color[3] <= 1.0 && st.points.iter().all(|p| p.x.abs() <= filmcraft_project::paint::MAX_COORD && (0.0..=1.0).contains(&p.pressure)));
    assert!(st.clone_time_offset.0 < i64::MAX / 2);
}

/// C3 acceptance: roto shapes (keyframed, per-point feather) and paint strokes survive save / open
/// through the project file, and the comp renders the same picture after the load.
#[test]
fn roto_and_paint_nodes_survive_save_and_open() {
    let mut s = demo();
    let dir = std::env::temp_dir().join(format!("fc-roto-paint-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let c = s.execute("comp.new", json!({"width": 320, "height": 180})).unwrap()["comp"].as_u64().unwrap();
    let solid = add(&mut s, c, json!({"kind": "solid"}));
    let r = add(&mut s, c, json!({"kind": "roto", "after": solid}));
    let p = add(&mut s, c, json!({"kind": "paint", "after": r}));
    connect(&mut s, c, p, 1, 0);
    s.execute("comp.roto.addShape", json!({"comp": c, "node": r, "path": [{"p": [20, 20], "feather_out": 9}, [200, 20], [200, 150], [20, 150]], "time": 0}))
        .unwrap();
    s.execute("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 0, "time": 5000, "path": [[40, 20], [220, 20], [220, 150], [40, 150]]}))
        .unwrap();
    s.execute(
        "comp.paint.addStroke",
        json!({"comp": c, "node": p, "stroke": {"tool": "color", "color": [0.0, 1.0, 0.0, 1.0], "points": [{"x": 10.0, "y": 90.0}, {"x": 300.0, "y": 90.0}], "duration": {"type": "hold", "from": 0, "to": 9000}}}),
    )
    .unwrap();
    let before = s.project.item(ItemId(c)).unwrap().kind.clone();
    let ItemKind::Comp(bc) = &before else { panic!("comp") };
    let ctx = filmcraft_render::comp::CompCtx { t: Tick(100), size: (320, 180), project: None, high_quality: false };
    let img_before = filmcraft_render::comp::render_cpu(bc, ctx, &filmcraft_render::comp::NoSources);
    let proj = dir.join("roto.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": proj})).unwrap();
    s.execute("file.open", json!({"path": proj})).unwrap();
    let ItemKind::Comp(ac) = &s.project.item(ItemId(c)).unwrap().kind else { panic!("comp after load") };
    assert_eq!(**ac, **bc, "roto and paint nodes survive save / open");
    assert!(
        matches!(&ac.node(r).unwrap().kind, CompKind::Roto { roto, .. } if roto.shapes[0].shape.keyframes.len() == 2 && roto.shapes[0].path_at(Tick(0)).vertices[0].feather_out == 9.0)
    );
    let img_after = filmcraft_render::comp::render_cpu(ac, ctx, &filmcraft_render::comp::NoSources);
    assert_eq!(img_before.px, img_after.px);
    assert!(img_after.px.iter().any(|v| *v > 0.0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Explain lines for Roto / Paint nodes.
#[test]
fn explain_names_roto_and_paint_nodes() {
    let mut s = demo();
    let c = s.execute("comp.new", json!({})).unwrap()["comp"].as_u64().unwrap();
    let solid = add(&mut s, c, json!({"kind": "solid"}));
    let r = add(&mut s, c, json!({"kind": "roto", "after": solid}));
    let p = add(&mut s, c, json!({"kind": "paint", "after": r}));
    connect(&mut s, c, p, 1, 0);
    connect(&mut s, c, solid, p, 1);
    let e = s.execute("comp.explain", json!({"comp": c})).unwrap().to_string();
    assert!(e.contains("passes Solid") && e.contains("no roto shape"), "{e}");
    assert!(e.contains("paints 0 stroke(s) over Roto") && e.contains("revealing Solid"), "{e}");
    s.execute("comp.roto.addShape", json!({"comp": c, "node": r})).unwrap();
    s.execute("comp.setRoto", json!({"comp": c, "node": r, "output": "matte"})).unwrap();
    let e = s.execute("comp.explain", json!({"comp": c})).unwrap().to_string();
    assert!(e.contains("shows its 1 roto shape(s) as a white-on-black matte"), "{e}");
    // C3.10: Clone strokes past the cap of other frames are named
    assert!(!e.contains("other frames"), "{e}");
    let tick = filmcraft_time::TICKS_PER_SECOND;
    let strokes: Vec<_> = (1..=6).map(|k| json!({"tool": "clone", "points": [{"x": 1.0, "y": 1.0}], "cloneTimeOffset": tick * k})).collect();
    s.execute("comp.setPaint", json!({"comp": c, "node": p, "paint": {"strokes": strokes}})).unwrap();
    let e = s.execute("comp.explain", json!({"comp": c})).unwrap().to_string();
    assert!(e.contains("2 Clone stroke(s) here would copy from more than 4 other frames"), "{e}");
}

/// Review fix guard: an inspector drag (Feather, Size, Opacity) sends one `gesture` for the whole
/// drag, so `comp.setRoto` / `comp.setPaint` with the same gesture are one undo step; a new
/// gesture (or none) is a new step.
#[test]
fn set_roto_and_set_paint_merge_one_gesture_into_one_undo_step() {
    let mut s = demo();
    let c = s.execute("comp.new", json!({"width": 640, "height": 360})).unwrap()["comp"].as_u64().unwrap();
    let r = add(&mut s, c, json!({"kind": "roto"}));
    let pn = add(&mut s, c, json!({"kind": "paint"}));
    s.execute("comp.roto.addShape", json!({"comp": c, "node": r})).unwrap();
    let depth = s.history.undo.len();
    for f in [1.0, 2.0, 3.0] {
        let mut whole = roto(&s, c, r);
        whole.shapes[0].feather = filmcraft_project::Param::new(ParamValue::Float(f));
        s.execute("comp.setRoto", json!({"comp": c, "node": r, "roto": whole, "gesture": 7})).unwrap();
    }
    assert_eq!(s.history.undo.len(), depth + 1, "one drag, one step");
    let stroke = json!({"tool": "color", "points": [{"x": 1, "y": 1}, {"x": 9, "y": 9}]});
    s.execute("comp.paint.addStroke", json!({"comp": c, "node": pn, "stroke": stroke})).unwrap();
    let depth = s.history.undo.len();
    for size in [4.0, 8.0] {
        let mut whole = paint(&s, c, pn);
        whole.strokes[0].brush.size = size;
        s.execute("comp.setPaint", json!({"comp": c, "node": pn, "paint": whole, "gesture": 8})).unwrap();
    }
    let mut whole = paint(&s, c, pn);
    whole.strokes[0].brush.size = 12.0;
    s.execute("comp.setPaint", json!({"comp": c, "node": pn, "paint": whole, "gesture": 9})).unwrap();
    assert_eq!(s.history.undo.len(), depth + 2, "two drags, two steps");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(paint(&s, c, pn).strokes[0].brush.size, 8.0);
}
