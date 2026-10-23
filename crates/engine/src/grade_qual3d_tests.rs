//! `grade.qual3d.*` commands: strokes pick colours, one undo step each, settings, clearing,
//! hostile strokes and parameters.

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_project::qual3d::{Qual3d, QualMode};

fn demo() -> (Session, ClipId, u64) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let id = q.video_tracks[0].item_at(s.playhead()).unwrap().id;
    s.state.selection = vec![id];
    let node = s.execute("grade.addNode", json!({"clip": id.0})).unwrap()["node"].as_u64().unwrap();
    (s, id, node)
}

fn node(s: &Session, c: ClipId, n: u64) -> filmcraft_project::grade::GradeNode {
    s.active_sequence().unwrap().find_item(c).unwrap().1.grade.as_ref().unwrap().node(n).unwrap().clone()
}

fn q3(s: &Session, c: ClipId, n: u64) -> Option<Qual3d> {
    node(s, c, n).qual3d
}

fn run(s: &mut Session, id: &str, p: Value) -> crate::Result<Value> {
    s.execute(id, p)
}

#[test]
fn a_stroke_picks_its_colours_switches_the_qualifier_on_and_undoes_in_one_step() {
    let (mut s, c, n) = demo();
    let before = node(&s, c, n);
    let r = run(&mut s, "grade.qual3d.addStroke", json!({"node": n, "points": [[0.45, 0.5], [0.55, 0.5]]})).unwrap();
    assert!(r["samples"].as_u64().unwrap() > 10, "{r}");
    let after = node(&s, c, n);
    assert!(after.float_at("qual.on", filmcraft_time::Tick(0)) >= 0.5);
    let k = after.qual3d_active().expect("a 3D qualifier");
    assert!(!k.include.is_empty() && k.exclude.is_empty());
    // the colour under the middle of the stroke is inside the picked volume
    let (_, _, img, working) = crate::grade::node_input(&mut s, &json!({"node": n}), "t").unwrap();
    let p = filmcraft_render::grade::qual3d_point(working, crate::grade::sample(&img, 0.5, 0.5).unwrap()).unwrap();
    let d = k.include.iter().map(|c| ((p[0] - c.c[0]).powi(2) + (p[1] - c.c[1]).powi(2) + (p[2] - c.c[2]).powi(2)).sqrt() - c.r).fold(f32::INFINITY, f32::min);
    assert!(d < k.softness, "{d}");
    // an exclude stroke on the side
    run(&mut s, "grade.qual3d.addStroke", json!({"node": n, "points": [{"x": 0.1, "y": 0.1}], "exclude": true})).unwrap();
    assert_eq!(q3(&s, c, n).unwrap().exclude.len(), 1);
    run(&mut s, "edit.undo", json!({})).unwrap();
    assert!(q3(&s, c, n).unwrap().exclude.is_empty());
    run(&mut s, "edit.undo", json!({})).unwrap();
    assert_eq!(node(&s, c, n), before, "one undo step per stroke");
}

#[test]
fn hostile_strokes_are_capped_or_refused() {
    let (mut s, c, n) = demo();
    // 10 000 points, some not numbers, some far outside: thinned, sampled at most MAX_SAMPLES
    let mut pts: Vec<Value> = (0..10_000).map(|i| json!([(i % 997) as f64 / 997.0, (i % 101) as f64 / 101.0])).collect();
    pts.extend([json!([1e300, 0.5]), json!([null, 0.5]), json!("x"), json!({"x": -1.0, "y": 2.0})]);
    let r = run(&mut s, "grade.qual3d.addStroke", json!({"node": n, "points": pts})).unwrap();
    assert!(r["samples"].as_u64().unwrap() <= filmcraft_project::qual3d::MAX_SAMPLES as u64, "{r}");
    let k = q3(&s, c, n).unwrap();
    assert!(k.include.len() <= filmcraft_project::qual3d::MAX_CLUSTERS);
    let snapshot = node(&s, c, n);
    for p in [
        json!({"node": n}),
        json!({"node": n, "points": []}),
        json!({"node": n, "points": [[2.0, 2.0], [-1.0, 0.5]]}),
        json!({"node": n, "points": [[1e308, 1e308]]}),
        json!({"node": n, "points": "nonsense"}),
        json!({"node": n, "points": [[0.5, 0.5]], "exclude": "yes"}),
        json!({"node": 999, "points": [[0.5, 0.5]]}),
    ] {
        assert!(run(&mut s, "grade.qual3d.addStroke", p.clone()).is_err(), "{p}");
    }
    assert_eq!(node(&s, c, n), snapshot, "refused strokes change nothing");
}

#[test]
fn settings_clamp_merge_and_switch_modes() {
    let (mut s, c, n) = demo();
    // HSL on a node without a 3D qualifier changes nothing (no undo step)
    run(&mut s, "grade.qual3d.set", json!({"node": n, "mode": "hsl"})).unwrap();
    assert!(q3(&s, c, n).is_none());
    for k in 1..=4 {
        run(&mut s, "grade.qual3d.set", json!({"node": n, "despill": 0.2 * k as f64, "merge": "drag"})).unwrap();
    }
    let k = q3(&s, c, n).unwrap();
    assert!((k.despill - 0.8).abs() < 1e-6 && k.mode == QualMode::Hsl, "{k:?}");
    run(&mut s, "edit.undo", json!({})).unwrap();
    assert!(q3(&s, c, n).is_none(), "the drag is one undo step");
    run(&mut s, "grade.qual3d.set", json!({"node": n, "mode": "3d", "softness": 1e9, "despill": -1e300})).unwrap();
    let k = q3(&s, c, n).unwrap();
    assert_eq!((k.mode, k.softness, k.despill), (QualMode::ThreeD, 0.5, 0.0));
    for p in [json!({"node": n}), json!({"node": n, "mode": "rgb"}), json!({"node": n, "mode": 3}), json!({"node": n, "softness": "a"})] {
        assert!(run(&mut s, "grade.qual3d.set", p.clone()).is_err(), "{p}");
    }
}

#[test]
fn clear_removes_a_side_or_everything() {
    let (mut s, c, n) = demo();
    assert!(run(&mut s, "grade.qual3d.clear", json!({"node": n})).is_err(), "nothing to clear");
    run(&mut s, "grade.qual3d.addStroke", json!({"node": n, "points": [[0.5, 0.5]]})).unwrap();
    run(&mut s, "grade.qual3d.addStroke", json!({"node": n, "points": [[0.2, 0.8]], "exclude": true})).unwrap();
    // (each stroke's path is kept to draw it: fork LO.3)
    let k = q3(&s, c, n).unwrap();
    assert_eq!(k.paths.iter().map(|p| (p.exclude, p.pts.clone())).collect::<Vec<_>>(), [(false, vec![[0.5, 0.5]]), (true, vec![[0.2, 0.8]])]);
    run(&mut s, "grade.qual3d.clear", json!({"node": n, "which": "exclude"})).unwrap();
    let k = q3(&s, c, n).unwrap();
    assert!(k.exclude.is_empty() && !k.include.is_empty());
    assert!(k.paths.len() == 1 && !k.paths[0].exclude, "the excluded strokes' paths go with them");
    assert!(run(&mut s, "grade.qual3d.clear", json!({"node": n, "which": "exclude"})).is_err());
    assert!(run(&mut s, "grade.qual3d.clear", json!({"node": n, "which": "both"})).is_err());
    run(&mut s, "grade.qual3d.clear", json!({"node": n})).unwrap();
    assert!(q3(&s, c, n).is_none(), "back to the HSL qualifier");
    run(&mut s, "edit.undo", json!({})).unwrap();
    assert!(q3(&s, c, n).is_some());
}
