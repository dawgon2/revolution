//! `grade.warper.*` commands: moving, pinning, resetting, one undo per drag, hostile params.

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_project::warper::{WarpMode, Warper};

fn demo() -> (Session, ClipId, u64) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let id = q.video_tracks[0].item_at(s.playhead()).unwrap().id;
    s.state.selection = vec![id];
    let node = s.execute("grade.addNode", json!({"clip": id.0})).unwrap()["node"].as_u64().unwrap();
    (s, id, node)
}

fn warper(s: &Session, c: ClipId, node: u64) -> Option<Warper> {
    s.active_sequence().unwrap().find_item(c).unwrap().1.grade.as_ref()?.node(node)?.warper.clone()
}

fn run(s: &mut Session, id: &str, p: Value) -> crate::Result<Value> {
    s.execute(id, p)
}

#[test]
fn moving_a_point_adds_a_warper_and_a_drag_is_one_undo_step() {
    let (mut s, c, n) = demo();
    assert!(warper(&s, c, n).is_none());
    for k in 1..=5 {
        let to = [0.0 + 0.02 * k as f64, 0.5];
        run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 8, "to": to, "merge": "drag-1"})).unwrap();
    }
    let w = warper(&s, c, n).unwrap();
    let p = w.hue_sat.position(WarpMode::HueSat, 8).unwrap();
    assert!((p[0] - 0.1).abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6, "{p:?}");
    run(&mut s, "edit.undo", json!({})).unwrap();
    assert!(warper(&s, c, n).is_none(), "the whole drag undoes at once");
}

#[test]
fn pinned_points_refuse_to_move_and_reset_clears() {
    let (mut s, c, n) = demo();
    run(&mut s, "grade.warper.pin", json!({"node": n, "index": 3})).unwrap();
    assert!(warper(&s, c, n).unwrap().hue_sat.is_pinned(3));
    assert!(run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 3, "to": [0.5, 0.5]})).is_err());
    run(&mut s, "grade.warper.pin", json!({"node": n, "index": 3, "on": false})).unwrap();
    run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 3, "to": [0.5, 0.5]})).unwrap();
    run(&mut s, "grade.warper.movePoint", json!({"node": n, "mesh": "chromaLuma", "index": 7, "to": [0.4, 0.2]})).unwrap();
    let w = warper(&s, c, n).unwrap();
    assert_eq!((w.hue_sat.points.len(), w.chroma_luma.points.len()), (1, 1));
    run(&mut s, "grade.warper.reset", json!({"node": n, "mesh": "hueSat", "index": 3})).unwrap();
    assert!(warper(&s, c, n).unwrap().hue_sat.points.is_empty());
    run(&mut s, "grade.warper.reset", json!({"node": n, "mesh": "chromaLuma"})).unwrap();
    assert!(warper(&s, c, n).unwrap().is_identity());
    run(&mut s, "grade.warper.reset", json!({"node": n})).unwrap();
    assert!(warper(&s, c, n).is_none());
    assert!(run(&mut s, "grade.warper.reset", json!({"node": n})).is_err(), "nothing to reset");
}

#[test]
fn set_changes_settings_and_a_new_grid_starts_afresh() {
    let (mut s, c, n) = demo();
    run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 2, "to": [0.3, 0.25]})).unwrap();
    run(&mut s, "grade.warper.set", json!({"node": n, "strength": 0.5, "range": 2, "preserveLuma": true, "mode": "chromaLuma"})).unwrap();
    let w = warper(&s, c, n).unwrap();
    assert_eq!((w.strength, w.range, w.preserve_luma, w.mode), (0.5, 2.0, true, WarpMode::ChromaLuma));
    assert_eq!(w.hue_sat.points.len(), 1);
    run(&mut s, "grade.warper.set", json!({"node": n, "hueColumns": 12, "satRings": 6})).unwrap();
    let w = warper(&s, c, n).unwrap();
    assert_eq!((w.hue_sat.cols, w.hue_sat.rows), (12, 6));
    assert!(w.hue_sat.points.is_empty());
    // a whole warper, cleaned on the way in
    let whole = json!({"hueSat": {"cols": 8, "rows": 4, "points": [{"index": 1, "to": [0.4, 3.0]}]}, "strength": 5.0});
    run(&mut s, "grade.warper.set", json!({"node": n, "warper": whole})).unwrap();
    let w = warper(&s, c, n).unwrap();
    assert_eq!(w.strength, 1.0);
    assert_eq!(w.hue_sat.position(WarpMode::HueSat, 1), Some([0.4, 1.0]));
}

#[test]
fn hostile_params_are_refused() {
    let (mut s, c, n) = demo();
    for (id, p) in [
        ("grade.warper.movePoint", json!({"node": n, "index": 8})),
        ("grade.warper.movePoint", json!({"node": n, "index": 8, "to": [0.1]})),
        ("grade.warper.movePoint", json!({"node": n, "index": 8, "to": ["a", 0.1]})),
        ("grade.warper.movePoint", json!({"node": n, "index": 32, "to": [0.1, 0.1]})),
        ("grade.warper.movePoint", json!({"node": n, "index": -1, "to": [0.1, 0.1]})),
        ("grade.warper.movePoint", json!({"node": n, "index": 1e20, "to": [0.1, 0.1]})),
        ("grade.warper.movePoint", json!({"node": n, "index": 4294967296u64, "to": [0.1, 0.1]})),
        ("grade.warper.movePoint", json!({"node": n, "mesh": "nope", "index": 1, "to": [0.1, 0.1]})),
        ("grade.warper.movePoint", json!({"node": 999, "index": 1, "to": [0.1, 0.1]})),
        ("grade.warper.pin", json!({"node": n, "index": 10_000})),
        ("grade.warper.pin", json!({"node": n, "index": 1, "on": "yes"})),
        ("grade.warper.set", json!({"node": n})),
        ("grade.warper.set", json!({"node": n, "hueColumns": 1000})),
        ("grade.warper.set", json!({"node": n, "satRings": 0})),
        ("grade.warper.set", json!({"node": n, "strength": "lots"})),
        ("grade.warper.set", json!({"node": n, "warper": {"hueSat": 5}})),
        ("grade.warper.reset", json!({"node": n, "index": 1})),
    ] {
        assert!(run(&mut s, id, p.clone()).is_err(), "{id} {p}");
    }
    // huge but finite numbers are clamped onto the plane
    run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 8, "to": [1e300, -1e300]})).unwrap();
    run(&mut s, "grade.warper.set", json!({"node": n, "strength": 1e300, "range": -1e300})).unwrap();
    let w = warper(&s, c, n).unwrap();
    assert_eq!((w.strength, w.range), (1.0, 0.0));
    let p = w.hue_sat.position(WarpMode::HueSat, 8).unwrap();
    assert!((0.0..1.0).contains(&p[0]) && p[1] == 0.0, "{p:?}");
    // the warped node still renders finite pixels
    let g = s.active_sequence().unwrap().find_item(c).unwrap().1.grade.clone().unwrap();
    let ops = filmcraft_render::grade::compile(&g, filmcraft_time::Tick(0), filmcraft_color::WorkingSpace::DaVinciWide);
    assert!(!ops.is_empty());
    assert!(filmcraft_color::ops::apply_chain(&ops, [0.4, 0.2, 0.1]).iter().all(|v| v.is_finite()));
}

#[test]
fn failed_and_no_op_edits_leave_no_trace() {
    let (mut s, c, n) = demo();
    let undo = |s: &Session| s.history.undo.len();
    let before = undo(&s);
    // a refused move on a node without a warper adds none
    assert!(run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 999, "to": [0.1, 0.1]})).is_err());
    assert!(warper(&s, c, n).is_none());
    assert_eq!(undo(&s), before);
    run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 8, "to": [0.1, 0.5]})).unwrap();
    let after_move = undo(&s);
    // resetting a point that isn't stored, unpinning an unpinned point, pinning a pinned one,
    // moving a point to where it is: no undo step
    run(&mut s, "grade.warper.reset", json!({"node": n, "index": 2})).unwrap();
    run(&mut s, "grade.warper.pin", json!({"node": n, "index": 3, "on": false})).unwrap();
    run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 8, "to": [0.1, 0.5]})).unwrap();
    assert_eq!(undo(&s), after_move);
    run(&mut s, "grade.warper.pin", json!({"node": n, "index": 3, "on": true})).unwrap();
    let pinned = undo(&s);
    assert_eq!(pinned, after_move + 1);
    run(&mut s, "grade.warper.pin", json!({"node": n, "index": 3, "on": true})).unwrap();
    assert_eq!(undo(&s), pinned);
}

/// Fork LO.3: the warper's strength and range and the 3D qualifier's softness and despill
/// keyframe like node parameters (`grade.toggleAnimation`, then `grade.warper.set` /
/// `grade.qual3d.set` / `grade.setParam` at a time), one undo step per edit.
#[test]
fn warper_and_3d_qualifier_settings_keyframe() {
    let (mut s, c, n) = demo();
    run(&mut s, "grade.warper.movePoint", json!({"node": n, "index": 2, "to": [0.3, 0.25]})).unwrap();
    run(&mut s, "grade.warper.set", json!({"node": n, "strength": 0.6})).unwrap();
    run(&mut s, "grade.qual3d.set", json!({"node": n, "mode": "3d", "despill": 0.2})).unwrap();
    let it = s.active_sequence().unwrap().find_item(c).unwrap().1.clone();
    let (t0, t1) = (it.start, it.start + filmcraft_time::Tick(254_016_000_000 / 2));
    let (m0, m1) = (it.effect_time_at(t0), it.effect_time_at(t1));
    let node = |s: &Session| s.active_sequence().unwrap().find_item(c).unwrap().1.grade.clone().unwrap().node(n).unwrap().clone();
    let undo = |s: &Session| s.history.undo.len();
    // animating starts from the static value
    let r = run(&mut s, "grade.toggleAnimation", json!({"node": n, "param": "warp.strength", "time": t0.0})).unwrap();
    assert_eq!(r["animated"], true);
    run(&mut s, "grade.toggleAnimation", json!({"node": n, "param": "q3d.despill", "time": t0.0})).unwrap();
    assert!((node(&s).warper_at(m1).unwrap().strength - 0.6).abs() < 1e-6);
    // the panel's slider (grade.warper.set) now writes a keyframe at the time: one undo step per drag
    let before = undo(&s);
    for v in [0.8, 0.9, 1.0] {
        run(&mut s, "grade.warper.set", json!({"node": n, "strength": v, "time": t1.0, "merge": "drag-1"})).unwrap();
    }
    assert_eq!(undo(&s), before + 1);
    run(&mut s, "grade.qual3d.set", json!({"node": n, "despill": 0.9, "time": t1.0})).unwrap();
    let nd = node(&s);
    assert!((nd.warper_at(m0).unwrap().strength - 0.6).abs() < 1e-6);
    assert!((nd.warper_at(m1).unwrap().strength - 1.0).abs() < 1e-6);
    assert!((nd.qual3d_active_at(m0).unwrap().despill - 0.2).abs() < 1e-6);
    assert!((nd.qual3d_active_at(m1).unwrap().despill - 0.9).abs() < 1e-6);
    // the static values are untouched
    assert_eq!(nd.warper.as_ref().map(|w| w.strength), Some(0.6));
    // grade.setParam reaches it too; a mixed set (strength + preserve luma) still works
    run(&mut s, "grade.setParam", json!({"node": n, "param": "warp.strength", "value": 0.5, "time": t1.0})).unwrap();
    assert!((node(&s).warper_at(m1).unwrap().strength - 0.5).abs() < 1e-6);
    run(&mut s, "grade.warper.set", json!({"node": n, "strength": 0.4, "preserveLuma": true, "time": t1.0})).unwrap();
    let nd = node(&s);
    assert!((nd.warper_at(m1).unwrap().strength - 0.4).abs() < 1e-6);
    assert!(nd.warper.as_ref().is_some_and(|w| w.preserve_luma));
    // undo walks back
    run(&mut s, "edit.undo", json!({})).unwrap();
    run(&mut s, "edit.undo", json!({})).unwrap();
    assert!((node(&s).warper_at(m1).unwrap().strength - 0.5).abs() < 1e-6);
}
