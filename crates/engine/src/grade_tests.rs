//! `grade.*` commands: node list edits, parameters, undo, hostile ids and values.

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::grade::{Grade, MAX_NODES};
use filmcraft_project::{ClipId, ParamValue};

fn demo() -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let id = q.video_tracks[0].item_at(s.playhead()).unwrap().id;
    s.state.selection = vec![id];
    (s, id)
}

fn grade(s: &Session, c: ClipId) -> Option<Grade> {
    s.active_sequence().unwrap().find_item(c).unwrap().1.grade.clone()
}

fn ids(s: &Session, c: ClipId) -> Vec<u64> {
    grade(s, c).map(|g| g.nodes.iter().map(|n| n.id).collect()).unwrap_or_default()
}

fn add(s: &mut Session, p: Value) -> u64 {
    s.execute("grade.addNode", p).unwrap()["node"].as_u64().unwrap()
}

#[test]
fn add_insert_creates_grade_and_orders_nodes() {
    let (mut s, c) = demo();
    assert!(grade(&s, c).is_none());
    let a = add(&mut s, json!({"clip": c.0}));
    let b = add(&mut s, json!({})); // selected clip
    let mid = add(&mut s, json!({"after": a, "label": "Mid"}));
    s.execute("grade.selectNode", json!({})).unwrap(); // no current node: insert goes first
    let first = s.execute("grade.insertNode", json!({"clip": c.0})).unwrap()["node"].as_u64().unwrap();
    let before_b = s.execute("grade.insertNode", json!({"before": b})).unwrap()["node"].as_u64().unwrap();
    assert_eq!(ids(&s, c), vec![first, a, mid, before_b, b]);
    let g = grade(&s, c).unwrap();
    assert_eq!(g.node(mid).unwrap().label, "Mid");
    let mut sorted = ids(&s, c);
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 5, "ids stay unique");
}

#[test]
fn each_edit_is_one_undo_step() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.setParam", json!({"node": a, "param": "saturation", "value": 1.5})).unwrap();
    s.execute("grade.toggleNode", json!({"node": a})).unwrap();
    assert!(!grade(&s, c).unwrap().node(a).unwrap().enabled);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(grade(&s, c).unwrap().node(a).unwrap().enabled);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(grade(&s, c).unwrap().node(a).unwrap().params.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(grade(&s, c).is_none(), "undoing the first node removes the grade it created");
}

#[test]
fn set_param_clamps_and_types_values() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.setParam", json!({"node": a, "param": "contrast", "value": 99.0})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "offset", "value": [0.1, 0.0, -0.1]})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "gain", "value": [1.0, 1.0, 1.0, 50.0]})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "curve.master", "value": [[1.0, 0.9], [0.0, 0.1], [0.5, 2.0]]})).unwrap();
    let g = grade(&s, c).unwrap();
    let n = g.node(a).unwrap();
    assert_eq!(n.param("contrast").unwrap().value, ParamValue::Float(4.0));
    assert_eq!(n.param("offset").unwrap().value, ParamValue::Color([0.1, 0.0, -0.1, 0.0]), "a missing master is 0, not 1");
    assert_eq!(n.param("gain").unwrap().value, ParamValue::Color([1.0, 1.0, 1.0, 4.0]));
    assert_eq!(n.param("curve.master").unwrap().value, ParamValue::Curve(vec![[0.0, 0.1], [0.5, 1.0], [1.0, 0.9]]), "sorted, clamped");
}

/// The node strip's "+ Serial" / "+ Before" send the current node as the anchor, `null` when the
/// clip has no grade yet; that used to be refused ("`after` must be a node id"), so the strip's
/// buttons did nothing on an ungraded clip (found by the Grading Coach lessons, G2.12).
#[test]
fn a_null_anchor_adds_like_no_anchor() {
    let (mut s, c) = demo();
    let a = s.execute("grade.addNode", json!({"clip": c.0, "after": null})).unwrap()["node"].as_u64().unwrap();
    let b = s.execute("grade.insertNode", json!({"clip": c.0, "before": null})).unwrap()["node"].as_u64().unwrap();
    assert_eq!(ids(&s, c).len(), 2);
    assert!(ids(&s, c).contains(&a) && ids(&s, c).contains(&b));
    // a non-id anchor is still refused
    assert!(s.execute("grade.addNode", json!({"clip": c.0, "after": "x"})).is_err());
    assert!(s.execute("grade.addNode", json!({"clip": c.0, "after": -1})).is_err());
}

#[test]
fn hostile_params_are_refused_without_changes() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    let before = s.project.clone();
    for p in [
        json!({"node": a, "param": "nope", "value": 1.0}),
        json!({"node": a, "param": "contrast", "value": "x"}),
        json!({"node": a, "param": "contrast"}),
        json!({"node": a, "param": "lift", "value": [1.0]}),
        json!({"node": a, "param": "lift", "value": [1.0, 2.0, 3.0, 4.0, 5.0]}),
        json!({"node": a, "param": "curve.r", "value": [[0.0]]}),
        json!({"node": a, "param": "curve.r", "value": vec![[0.5, 0.5]; 65]}),
        json!({"node": u64::MAX, "param": "contrast", "value": 1.0}),
        json!({"clip": "x", "node": a, "param": "contrast", "value": 1.0}),
        json!({"clip": 987_654_321u64, "node": a, "param": "contrast", "value": 1.0}),
    ] {
        assert!(s.execute("grade.setParam", p.clone()).is_err(), "{p}");
    }
    for (cmd, p) in [
        ("grade.removeNode", json!({"node": 999})),
        ("grade.moveNode", json!({"node": 999, "to": 0})),
        ("grade.moveNode", json!({"node": a})),
        ("grade.toggleNode", json!({"node": -1})),
        ("grade.labelNode", json!({"node": a})),
        ("grade.addNode", json!({"after": 999})),
        ("grade.insertNode", json!({"before": 999})),
        ("grade.resetNode", json!({"node": a, "param": "nope"})),
        ("grade.toggleAnimation", json!({"node": a, "param": "curve.r"})),
    ] {
        assert!(s.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
    }
    assert!(std::sync::Arc::ptr_eq(&before, &s.project), "refused commands change nothing");
}

#[test]
fn nan_is_not_representable_but_infinity_like_values_clamp() {
    // JSON has no NaN / ∞; huge numbers clamp to the range
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.setParam", json!({"node": a, "param": "temperature", "value": -1e308})).unwrap();
    assert_eq!(grade(&s, c).unwrap().node(a).unwrap().param("temperature").unwrap().value, ParamValue::Float(-100.0));
}

#[test]
fn grade_commands_without_a_grade_or_on_audio_are_refused() {
    let (mut s, c) = demo();
    assert!(s.execute("grade.removeNode", json!({"clip": c.0, "node": 1})).is_err());
    assert!(s.execute("grade.bypass", json!({"clip": c.0})).is_err());
    let q = s.active_sequence().unwrap();
    if let Some(audio) = q.audio_tracks.iter().flat_map(|t| t.items.iter()).next() {
        let a = audio.id;
        assert!(s.execute("grade.addNode", json!({"clip": a.0})).is_err());
        s.state.selection = vec![a];
        assert!(s.execute("grade.addNode", json!({})).is_err(), "selection without a video clip");
    }
}

#[test]
fn move_toggle_label_reset_bypass_clear() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    let b = add(&mut s, json!({}));
    let d = add(&mut s, json!({}));
    s.execute("grade.moveNode", json!({"node": d, "to": 0})).unwrap();
    assert_eq!(ids(&s, c), vec![d, a, b]);
    s.execute("grade.moveNode", json!({"node": d, "to": u64::MAX})).unwrap();
    assert_eq!(ids(&s, c), vec![a, b, d], "past the end moves to the end");
    let r = s.execute("grade.toggleNode", json!({"node": b, "on": false})).unwrap();
    assert_eq!(r["enabled"], json!(false));
    s.execute("grade.labelNode", json!({"node": b, "label": format!("Skin\u{7}{}", "x".repeat(200))})).unwrap();
    let label = grade(&s, c).unwrap().node(b).unwrap().label.clone();
    assert!(label.starts_with("Skinx") && label.chars().count() == 64, "control chars dropped, capped: {label:?}");
    s.execute("grade.setParam", json!({"node": a, "param": "saturation", "value": 0.5})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "contrast", "value": 1.2})).unwrap();
    s.execute("grade.resetNode", json!({"node": a, "param": "saturation"})).unwrap();
    assert_eq!(grade(&s, c).unwrap().node(a).unwrap().params.len(), 1);
    s.execute("grade.resetNode", json!({"node": a})).unwrap();
    assert!(grade(&s, c).unwrap().node(a).unwrap().params.is_empty());
    assert_eq!(s.execute("grade.bypass", json!({})).unwrap()["bypass"], json!(true));
    assert_eq!(s.execute("grade.bypass", json!({"on": true})).unwrap()["bypass"], json!(true));
    s.execute("grade.removeNode", json!({"node": a})).unwrap();
    assert_eq!(ids(&s, c), vec![b, d]);
    let got = s.execute("grade.get", json!({})).unwrap();
    assert_eq!(got["grade"]["nodes"].as_array().unwrap().len(), 2);
    s.execute("grade.clear", json!({})).unwrap();
    assert!(grade(&s, c).is_none());
    assert!(s.execute("grade.get", json!({})).unwrap()["grade"].is_null());
}

#[test]
fn node_count_is_capped() {
    let (mut s, c) = demo();
    for _ in 0..MAX_NODES {
        add(&mut s, json!({"clip": c.0}));
    }
    assert!(s.execute("grade.addNode", json!({})).is_err());
    assert_eq!(ids(&s, c).len(), MAX_NODES);
}

#[test]
fn keyframes_at_the_playhead_and_merged_drags() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    let r = s.execute("grade.toggleAnimation", json!({"node": a, "param": "saturation"})).unwrap();
    assert_eq!(r["animated"], json!(true));
    let (start, end) = {
        let it = s.active_sequence().unwrap().find_item(c).unwrap().1;
        (it.start, it.end())
    };
    let later = filmcraft_time::Tick(start.0 + (end.0 - start.0) / 2);
    s.execute("grade.setParam", json!({"node": a, "param": "saturation", "value": 2.0, "time": later.0})).unwrap();
    let g = grade(&s, c).unwrap();
    let prm = g.node(a).unwrap().param("saturation").unwrap();
    assert_eq!(prm.keyframes.len(), 2);
    // a drag: three merged edits are one undo step
    let undo = s.history.undo.len();
    for v in [0.2, 0.4, 0.6] {
        s.execute("grade.setParam", json!({"node": a, "param": "lift", "value": [v, 0.0, 0.0], "merge": "drag-1"})).unwrap();
    }
    assert_eq!(s.history.undo.len(), undo + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(grade(&s, c).unwrap().node(a).unwrap().param("lift").is_none());
}

#[test]
fn params_lists_the_schema() {
    let (mut s, _) = demo();
    let r = s.execute("grade.params", json!({})).unwrap();
    let keys: Vec<&str> = r["params"].as_array().unwrap().iter().map(|p| p["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&"offset") && keys.contains(&"curve.master"));
    assert_eq!(r["maxNodes"], json!(MAX_NODES));
}

#[test]
fn graded_clip_renders_differently() {
    let (mut s, c) = demo();
    let plain = s.render_program(0.125).unwrap();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.setParam", json!({"node": a, "param": "offset", "value": [0.1, 0.0, 0.0, 0.0]})).unwrap();
    let graded = s.render_program(0.125).unwrap();
    assert_ne!(plain.px, graded.px);
    s.execute("grade.bypass", json!({})).unwrap();
    assert_eq!(plain.px, s.render_program(0.125).unwrap().px, "bypass renders like no grade");
}

#[test]
fn merged_drags_split_per_clip_and_only_set_param_merges() {
    let (mut s, c) = demo();
    let other = s.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| i.id).find(|i| *i != c).unwrap();
    let a = add(&mut s, json!({"clip": c.0}));
    let b = add(&mut s, json!({"clip": other.0}));
    let undo = s.history.undo.len();
    s.execute("grade.setParam", json!({"clip": c.0, "node": a, "param": "contrast", "value": 1.1, "merge": "k"})).unwrap();
    s.execute("grade.setParam", json!({"clip": other.0, "node": b, "param": "contrast", "value": 1.1, "merge": "k"})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 2, "same key on another clip is a new step");
    s.execute("grade.toggleNode", json!({"clip": other.0, "node": b, "merge": "k"})).unwrap();
    s.execute("grade.toggleNode", json!({"clip": other.0, "node": b, "merge": "k"})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 4, "only setParam merges");
}

#[test]
fn toggle_animation_off_keeps_the_value_and_clear_undoes() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.toggleAnimation", json!({"node": a, "param": "saturation"})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "saturation", "value": 1.7})).unwrap();
    let r = s.execute("grade.toggleAnimation", json!({"node": a, "param": "saturation"})).unwrap();
    assert_eq!(r["animated"], json!(false));
    let g = grade(&s, c).unwrap();
    let prm = g.node(a).unwrap().param("saturation").unwrap();
    assert!(prm.keyframes.is_empty());
    assert_eq!(prm.value, ParamValue::Float(1.7));
    s.execute("grade.clear", json!({})).unwrap();
    assert!(s.execute("grade.clear", json!({})).is_err(), "no grade left to clear");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(grade(&s, c).is_some());
    assert!(s.execute("grade.get", json!({"clip": 987_654_321u64})).is_err());
}

#[test]
fn negative_positions_and_ids_are_refused() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    add(&mut s, json!({}));
    assert!(s.execute("grade.moveNode", json!({"node": a, "to": -5})).is_err());
    assert!(s.execute("grade.addNode", json!({"after": -1})).is_err());
    assert!(s.execute("grade.removeNode", json!({"node": 1.5})).is_err());
}

#[test]
fn keyframes_land_at_clip_time_clamped_into_the_clip() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.toggleAnimation", json!({"node": a, "param": "contrast", "time": 0})).unwrap();
    let it = s.active_sequence().unwrap().find_item(c).unwrap().1.clone();
    let far = it.end().0 + 1_000_000_000_000;
    s.execute("grade.setParam", json!({"node": a, "param": "contrast", "value": 2.0, "time": far})).unwrap();
    let g = grade(&s, c).unwrap();
    let k = &g.node(a).unwrap().param("contrast").unwrap().keyframes;
    let last = it.effect_time_at(it.end() - filmcraft_time::Tick(1));
    assert!(k.iter().any(|k| k.time == last), "{:?} vs {last:?}", k.iter().map(|k| k.time).collect::<Vec<_>>());
}

#[test]
fn graded_clip_survives_save_and_open() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.setParam", json!({"node": a, "param": "gain", "value": [0.1, 0.2, 0.3, 0.4]})).unwrap();
    let before = grade(&s, c).unwrap();
    let dir = std::env::temp_dir().join(format!("fc-grade-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("g.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(grade(&t, c), Some(before));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pick_qualifier_sets_ranges_around_the_sample_in_one_undo_step() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    let before = grade(&s, c);
    let r = s.execute("grade.pickQualifier", json!({"node": a, "x": 0.5, "y": 0.5})).unwrap();
    let n = grade(&s, c).unwrap().nodes[0].clone();
    let t = filmcraft_time::Tick(0);
    assert!(n.float_at("qual.on", t) >= 0.5, "{r}");
    let (h, sat, l) = (r["sample"]["hue"].as_f64().unwrap() as f32, r["sample"]["sat"].as_f64().unwrap() as f32, r["sample"]["luma"].as_f64().unwrap() as f32);
    let at = |k| n.params.get(k).and_then(|p| if let ParamValue::Float(v) = p.value { Some(v as f32) } else { None }).unwrap();
    assert!(at("qual.lumLow") <= l && l <= at("qual.lumHigh"), "{r}");
    assert!(at("qual.satLow") <= sat && sat <= at("qual.satHigh"), "{r}");
    assert!((at("qual.hue") - h).abs() < 1e-6, "{r}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c), before, "one undo step");
}

#[test]
fn pick_qualifier_add_widens_and_hostile_input_is_refused() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    for p in [
        json!({"node": a}),
        json!({"node": a, "x": "a", "y": 0.5}),
        json!({"node": a, "x": 0.5, "y": 0.5, "mode": "nope"}),
        json!({"node": 999, "x": 0.5, "y": 0.5}),
    ] {
        assert!(s.execute("grade.pickQualifier", p.clone()).is_err(), "{p}");
    }
    // out-of-range coordinates clamp to the frame
    s.execute("grade.pickQualifier", json!({"node": a, "x": -5.0, "y": 1e300})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "qual.lumLow", "value": 0.4})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "qual.lumHigh", "value": 0.41})).unwrap();
    let r = s.execute("grade.pickQualifier", json!({"node": a, "x": 0.5, "y": 0.5, "mode": "add"})).unwrap();
    let l = r["sample"]["luma"].as_f64().unwrap();
    let lo = r["params"]["qual.lumLow"].as_f64().unwrap();
    let hi = r["params"]["qual.lumHigh"].as_f64().unwrap();
    assert!(lo <= l.min(0.4) + 1e-6 && hi >= l.max(0.41) - 1e-6, "{r}");
}

#[test]
fn set_lut_sets_validates_and_undoes() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    let Some(b) = filmcraft_render::luts::builtins().first() else { return };
    let r = format!("builtin:{}", b.id);
    s.execute("grade.setLut", json!({"node": a, "lut": r})).unwrap();
    assert_eq!(grade(&s, c).unwrap().nodes[0].lut, r);
    assert!(s.execute("grade.setLut", json!({"node": a, "lut": "lib:nope"})).is_err());
    assert!(s.execute("grade.setLut", json!({"node": a, "path": "/no/such/file.cube"})).is_err());
    // a refused node imports nothing
    let luts = s.project.luts.len();
    let dir0 = std::env::temp_dir().join(format!("fc-grade-lut0-{}", std::process::id()));
    std::fs::create_dir_all(&dir0).unwrap();
    let f0 = dir0.join("x.cube");
    std::fs::write(&f0, "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n").unwrap();
    assert!(s.execute("grade.setLut", json!({"node": 999, "path": f0.to_string_lossy()})).is_err());
    assert_eq!(s.project.luts.len(), luts);
    let _ = std::fs::remove_dir_all(&dir0);
    assert_eq!(grade(&s, c).unwrap().nodes[0].lut, r, "refused calls change nothing");
    s.execute("grade.setLut", json!({"node": a, "lut": ""})).unwrap();
    assert!(grade(&s, c).unwrap().nodes[0].lut.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c).unwrap().nodes[0].lut, r);
    // an imported .cube file lands in the project's library
    let dir = std::env::temp_dir().join(format!("fc-grade-lut-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("warm.cube");
    std::fs::write(&f, "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n").unwrap();
    let v = s.execute("grade.setLut", json!({"node": a, "path": f.to_string_lossy()})).unwrap();
    assert!(v["lut"].as_str().unwrap().starts_with("lib:"), "{v}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two video clips of the demo project (the playhead one first).
fn two_clips(s: &Session) -> (ClipId, ClipId) {
    let q = s.active_sequence().unwrap();
    let first = q.video_tracks[0].item_at(s.playhead()).unwrap().id;
    let other = q.video_tracks.iter().flat_map(|t| &t.items).map(|i| i.id).find(|c| *c != first).unwrap();
    (first, other)
}

#[test]
fn compare_data_groups_identical_grades() {
    let (mut s, c) = demo();
    let (_, d) = two_clips(&s);
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.setParam", json!({"clip": c.0, "node": a, "param": "saturation", "value": 0.7})).unwrap();
    s.execute("grade.copy", json!({"clip": c.0})).unwrap();
    s.execute("grade.paste", json!({"clips": [d.0]})).unwrap();
    let r = s.execute("grade.compareData", json!({})).unwrap();
    let groups = r["groups"].as_array().unwrap();
    assert!(groups.iter().any(|g| g["clips"].as_array().unwrap().len() == 2), "{r}");
    // a tweak on one clip breaks the group
    let dn = grade(&s, d).unwrap().nodes[0].id;
    s.execute("grade.setParam", json!({"clip": d.0, "node": dn, "param": "saturation", "value": 0.71})).unwrap();
    let r = s.execute("grade.compareData", json!({})).unwrap();
    assert!(r["groups"].as_array().unwrap().is_empty(), "{r}");
    assert!(s.execute("grade.compareData", json!({"clips": ["x"]})).is_err());
}

#[test]
fn compare_shots_check_scene_and_shot_match() {
    let (mut s, c) = demo();
    let (_, d) = two_clips(&s);
    // the clip against itself is refused; against another clip it answers with a score and notes
    assert!(s.execute("grade.compareShots", json!({"clip": c.0, "with": c.0})).is_err());
    assert!(s.execute("grade.compareShots", json!({"clip": c.0})).is_err(), "needs a reference");
    let r = s.execute("grade.compareShots", json!({"clip": c.0, "with": d.0})).unwrap();
    let score = r["score"].as_f64().unwrap();
    assert!((0.0..=100.0).contains(&score) && r["notes"].is_array(), "{r}");
    // the scene check needs two clips
    s.state.selection = vec![c];
    assert!(s.execute("grade.checkScene", json!({})).is_err());
    let r = s.execute("grade.checkScene", json!({"clips": [c.0, d.0]})).unwrap();
    assert_eq!(r["shots"].as_array().unwrap().len(), 2, "{r}");
    // Shot Match appends one node (one undo step) and doesn't make the match worse
    let before = grade(&s, c);
    let m = s.execute("grade.shotMatch", json!({"clip": c.0, "with": d.0})).unwrap();
    let g = grade(&s, c).unwrap();
    assert_eq!(g.nodes.last().map(|n| n.label.as_str()), Some("Shot Match"), "{m}");
    assert!(m["after"].as_f64().unwrap() <= m["before"].as_f64().unwrap() + 1e-6, "{m}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, c), before);
}

#[test]
fn explain_node_and_grade_give_sentences_and_an_overlay() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.setParam", json!({"clip": c.0, "node": a, "param": "gain", "value": [0.0, 0.0, 0.0, 0.3]})).unwrap();
    let b = add(&mut s, json!({"clip": c.0}));
    let dir = std::env::temp_dir().join(format!("fc-explain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("overlay.png");
    let r = s.execute("grade.explainNode", json!({"clip": c.0, "node": a, "overlay": png.to_string_lossy()})).unwrap();
    let sentence = r["sentence"].as_str().unwrap();
    assert!(sentence.contains("stop"), "{r}");
    assert!(png.exists());
    let r = s.execute("grade.explainNode", json!({"clip": c.0, "node": b})).unwrap();
    assert!(r["sentence"].as_str().unwrap().contains("no visible change"), "{r}");
    let r = s.execute("grade.explainGrade", json!({"clip": c.0})).unwrap();
    assert_eq!(r["nodes"].as_array().unwrap().len(), 2);
    assert!(!r["text"].as_str().unwrap().is_empty());
    assert!(s.execute("grade.explainNode", json!({"clip": c.0, "node": 999})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn match_and_explain_refuse_hostile_input() {
    let (mut s, c) = demo();
    // the reference frame can't be inside the clip; an ungraded clip has nothing to explain
    let it = s.active_sequence().unwrap().find_item(c).unwrap().1.clone();
    assert!(s.execute("grade.compareShots", json!({"clip": c.0, "referenceTime": it.start.0})).is_err());
    assert!(s.execute("grade.explainNode", json!({"clip": c.0})).is_err());
    assert!(s.execute("grade.explainGrade", json!({"clip": c.0})).is_err());
    assert!(s.execute("grade.checkScene", json!({"clips": []})).is_err());
    assert!(s.execute("grade.compareShots", json!({"clip": c.0, "with": "x"})).is_err());
    // an overlay that can't be written is an error, not a panic
    let a = add(&mut s, json!({"clip": c.0}));
    let r = s.execute("grade.explainNode", json!({"clip": c.0, "node": a, "overlay": "/no/such/dir/x.png"}));
    assert!(r.is_err(), "{r:?}");
    // Shot Match refuses a full grade
    let (_, d) = two_clips(&s);
    for _ in 1..MAX_NODES {
        add(&mut s, json!({"clip": c.0}));
    }
    assert!(s.execute("grade.shotMatch", json!({"clip": c.0, "with": d.0, "faceDetection": false})).is_err());
}

/// Shot Match on a grade with a hand-made layout places its node next to the node it follows
/// (fork C2b); on an automatic layout it stays unplaced.
#[test]
fn shot_match_places_its_node_on_a_hand_made_layout() {
    let (mut s, c) = demo();
    let (_, d) = two_clips(&s);
    let a = s.execute("grade.addNode", json!({"clip": c.0, "after": null})).unwrap()["node"].as_u64().unwrap();
    s.execute("grade.moveNodes", json!({"clip": c.0, "nodes": [{"node": a, "x": 1000, "y": 500}]})).unwrap();
    let id = s.execute("grade.shotMatch", json!({"clip": c.0, "with": d.0})).unwrap()["node"].as_u64().unwrap();
    let g = grade(&s, c).unwrap();
    assert_eq!(g.stored_pos(id), Some([1000.0 + filmcraft_project::grade::LAYOUT_COLUMN, 500.0]));
    // an automatic layout stays automatic
    s.execute("grade.resetLayout", json!({"clip": c.0})).unwrap();
    let id = s.execute("grade.shotMatch", json!({"clip": c.0, "with": d.0})).unwrap()["node"].as_u64().unwrap();
    let g = grade(&s, c).unwrap();
    assert!(!g.has_positions() && g.stored_pos(id).is_none());
}

/// Fork G4.2: the HDR wheels' keys go through `grade.setParam` (clamped, typed) and
/// `grade.hdr.reset` resets one wheel or all of them in one undo step; hostile zones are refused.
#[test]
fn hdr_wheels_set_clamp_and_reset() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({"clip": c.0}));
    s.execute("grade.setParam", json!({"node": a, "param": "hdr.shadow.exposure", "value": 99.0})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "hdr.shadow.falloff", "value": 0.0})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "hdr.light.balance", "value": [0.2, -0.1, -0.1]})).unwrap();
    s.execute("grade.setParam", json!({"node": a, "param": "hdr.global.sat", "value": 1.5})).unwrap();
    assert!(s.execute("grade.setParam", json!({"node": a, "param": "hdr.global.range", "value": 1.0})).is_err());
    assert!(s.execute("grade.setParam", json!({"node": a, "param": "hdr.light.balance", "value": 1.0})).is_err());
    let n = grade(&s, c).unwrap().node(a).unwrap().clone();
    assert_eq!(n.param("hdr.shadow.exposure").unwrap().value, ParamValue::Float(4.0));
    assert_eq!(n.param("hdr.shadow.falloff").unwrap().value, ParamValue::Float(f64::from(filmcraft_project::grade::HDR_MIN_FALLOFF)));
    assert!(n.hdr_at(filmcraft_time::Tick(0)).is_some());
    for z in [json!("Shadow"), json!(3), json!("hdr"), json!("")] {
        assert!(s.execute("grade.hdr.reset", json!({"node": a, "zone": z})).is_err(), "{z}");
    }
    s.execute("grade.hdr.reset", json!({"node": a, "zone": "shadow"})).unwrap();
    let n = grade(&s, c).unwrap().node(a).unwrap().clone();
    assert!(n.param("hdr.shadow.exposure").is_none() && n.param("hdr.shadow.falloff").is_none());
    assert!(n.param("hdr.light.balance").is_some() && n.param("hdr.global.sat").is_some());
    s.execute("grade.setParam", json!({"node": a, "param": "saturation", "value": 0.5})).unwrap();
    s.execute("grade.hdr.reset", json!({"node": a})).unwrap();
    let n = grade(&s, c).unwrap().node(a).unwrap().clone();
    assert!(n.params.keys().all(|k| !k.starts_with("hdr.")) && n.param("saturation").is_some());
    // nothing left to reset: no undo step, so one undo brings the wheels back
    s.execute("grade.hdr.reset", json!({"node": a})).unwrap();
    s.execute("grade.hdr.reset", json!({"node": a, "zone": "light"})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(grade(&s, c).unwrap().node(a).unwrap().param("hdr.light.balance").is_some());
}
