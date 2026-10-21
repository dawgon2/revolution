use super::*;
use filmcraft_time::{Tick, TimeRange};
use serde_json::json;

fn secs(x: f64) -> Tick {
    Tick::from_seconds_f64(x)
}

fn setup() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Resolve edit","width":1280,"height":720,"fps":24})).unwrap();
    s.execute("source.open", json!({"item":5})).unwrap();
    s
}

fn marks(s: &mut Session, a: Option<Tick>, b: Option<Tick>) {
    s.edit_sequence("marks", |q, _, _| {
        q.mark_in = a;
        q.mark_out = b;
        Ok(())
    })
    .unwrap();
}

fn set_source_marks(s: &mut Session, a: f64, b: f64) -> TimeRange {
    s.execute("project.setMarks", json!({"item":5,"in":secs(a).0,"out":secs(b).0})).unwrap();
    clip_ops::source_view(s, ItemId(5)).unwrap().selected_range()
}

/// (start, duration, source_in) of the clips on V1.
fn v1(s: &Session) -> Vec<(Tick, Tick, Tick)> {
    s.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| (i.start, i.duration, i.source_in)).collect()
}

fn lock_v1(s: &mut Session) {
    s.edit_sequence("lock", |q, _, _| {
        q.video_tracks[0].locked = true;
        Ok(())
    })
    .unwrap();
}

#[test]
fn all_ten_commands_are_registered_and_enabled_by_a_source() {
    let s = setup();
    for id in [
        "edit.insert",
        "edit.overwrite",
        "edit.replace",
        "edit.placeOnTop",
        "edit.append",
        "edit.rippleOverwrite",
        "edit.fitToFill",
        "edit.smartInsert",
        "edit.closeUp",
        "edit.sourceOverwrite",
    ] {
        assert!(crate::commands::find(id).is_some(), "{id} missing");
    }
    assert!((crate::commands::find("edit.insert").unwrap().enabled)(&s).is_ok());
    // fit to fill needs timeline In and Out
    assert!((crate::commands::find("edit.fitToFill").unwrap().enabled)(&s).is_err());
    // no sequence: disabled
    assert!((crate::commands::find("edit.insert").unwrap().enabled)(&Session::default()).is_err());
}

#[test]
fn insert_and_overwrite_use_the_in_mark_else_the_playhead_and_undo_in_one_step() {
    for id in ["edit.insert", "edit.overwrite"] {
        let mut s = setup();
        let r = set_source_marks(&mut s, 1.0, 3.0);
        let before = s.project.to_json();
        s.set_playhead(secs(2.0));
        s.execute(id, json!({})).unwrap();
        assert_eq!(v1(&s)[0].0, secs(2.0));
        assert_eq!(v1(&s)[0].2, r.start);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.to_json(), before);
        marks(&mut s, Some(secs(5.0)), None);
        s.execute(id, json!({})).unwrap();
        assert_eq!(v1(&s)[0].0, secs(5.0));
    }
}

#[test]
fn clip_param_stands_in_for_the_source_viewer() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"x","width":1280,"height":720,"fps":24})).unwrap();
    s.state.source_item = None;
    s.state.project_selection.clear();
    assert!(s.execute("edit.insert", json!({})).is_err(), "no source");
    s.execute("edit.insert", json!({"clip":5})).unwrap();
    assert_eq!(v1(&s).len(), 1);
    assert!(s.execute("edit.overwrite", json!({"clip":999_999})).is_err());
    assert!(s.execute("edit.overwrite", json!({"clip":"x"})).is_err());
}

#[test]
fn every_command_accepts_the_clip_param() {
    for id in ["edit.replace", "edit.placeOnTop", "edit.append", "edit.rippleOverwrite", "edit.smartInsert", "edit.closeUp", "edit.fitToFill"] {
        let mut s = setup();
        set_source_marks(&mut s, 1.0, 2.0);
        s.execute("edit.append", json!({})).unwrap();
        s.state.source_item = None;
        s.set_playhead(secs(0.2));
        marks(&mut s, Some(secs(10.0)), Some(secs(12.0)));
        s.execute(id, json!({"clip":5})).unwrap_or_else(|e| panic!("{id}: {e}"));
    }
}

#[test]
fn replace_lines_up_source_playhead_and_keeps_the_clip_duration() {
    let mut s = setup();
    set_source_marks(&mut s, 0.0, 1.0);
    s.execute("edit.overwrite", json!({})).unwrap();
    let old = v1(&s)[0];
    s.set_playhead(secs(0.5));
    s.state.source_playhead = secs(4.0);
    let before = s.project.to_json();
    s.execute("edit.replace", json!({})).unwrap();
    let v = v1(&s);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].0, old.0);
    assert_eq!(v[0].2, secs(4.0) - secs(0.5));
    assert!(v[0].1 <= old.1 && v[0].1.0 > 0);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn replace_refuses_bad_states_without_changing_anything() {
    let mut s = setup();
    assert!(s.execute("edit.replace", json!({})).is_err(), "empty timeline");
    set_source_marks(&mut s, 0.0, 1.0);
    s.execute("edit.overwrite", json!({})).unwrap();
    s.set_playhead(secs(0.5));
    // the Source playhead sits before the offset into the clip: not enough media
    s.state.source_playhead = Tick::ZERO;
    let before = s.project.to_json();
    assert!(s.execute("edit.replace", json!({})).is_err());
    assert_eq!(s.project.to_json(), before);
    s.state.source_playhead = secs(3.0);
    lock_v1(&mut s);
    let before = s.project.to_json();
    assert!(s.execute("edit.replace", json!({})).is_err());
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn place_on_top_uses_the_track_above_and_adds_one_when_needed() {
    let mut s = setup();
    set_source_marks(&mut s, 1.0, 2.0);
    s.execute("edit.overwrite", json!({})).unwrap();
    s.set_playhead(secs(0.5));
    marks(&mut s, None, None);
    let n = s.history.undo.len();
    s.execute("edit.placeOnTop", json!({})).unwrap();
    assert_eq!(s.history.undo.len(), n + 1);
    assert_eq!(s.active_sequence().unwrap().video_tracks[1].items.len(), 1);
    // fill every track at the point, then a new track appears; one undo takes track and clip away
    let tracks = s.active_sequence().unwrap().video_tracks.len();
    for _ in 0..tracks {
        s.set_playhead(secs(0.5));
        s.execute("edit.placeOnTop", json!({})).unwrap();
    }
    let tracks = s.active_sequence().unwrap().video_tracks.len();
    let before = s.project.to_json();
    s.set_playhead(secs(0.5));
    s.execute("edit.placeOnTop", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), tracks + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn place_on_top_refuses_when_the_new_track_cannot_be_used_and_rolls_back() {
    let mut s = setup();
    set_source_marks(&mut s, 1.0, 2.0);
    s.state.source_item = None;
    s.state.project_selection.clear();
    assert!(s.execute("edit.placeOnTop", json!({})).is_err());
}

#[test]
fn append_goes_to_the_end_of_the_timeline() {
    let mut s = setup();
    let r = set_source_marks(&mut s, 1.0, 3.0);
    s.execute("edit.append", json!({})).unwrap();
    assert_eq!(v1(&s)[0].0, Tick::ZERO, "empty timeline: at 0");
    s.execute("edit.append", json!({})).unwrap();
    let v = v1(&s);
    assert_eq!(v.len(), 2);
    assert_eq!(v[1].0, v[0].0 + v[0].1);
    assert_eq!(v[1].2, r.start);
    lock_v1(&mut s);
    assert!(s.execute("edit.append", json!({})).is_err());
}

#[test]
fn ripple_overwrite_moves_the_rest_by_the_duration_difference() {
    let mut s = setup();
    set_source_marks(&mut s, 0.0, 1.0);
    s.execute("edit.append", json!({})).unwrap();
    s.execute("edit.append", json!({})).unwrap();
    let (first, second) = (v1(&s)[0], v1(&s)[1]);
    let r = set_source_marks(&mut s, 0.0, 2.0);
    s.set_playhead(first.0 + secs(0.2));
    let before = s.project.to_json();
    let n = s.history.undo.len();
    s.execute("edit.rippleOverwrite", json!({})).unwrap();
    assert_eq!(s.history.undo.len(), n + 1);
    let v = v1(&s);
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].0, first.0);
    assert_eq!(v[0].1, s.sequence_rate().snap_nearest(r.duration));
    assert_eq!(v[1].0, second.0 + (v[0].1 - first.1));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn ripple_overwrite_refuses_empty_timeline_and_locked_track() {
    let mut s = setup();
    assert!(s.execute("edit.rippleOverwrite", json!({})).is_err());
    s.execute("edit.overwrite", json!({})).unwrap();
    s.set_playhead(secs(0.1));
    lock_v1(&mut s);
    let before = s.project.to_json();
    assert!(s.execute("edit.rippleOverwrite", json!({})).is_err());
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn fit_to_fill_changes_speed_so_the_source_fills_in_to_out() {
    let mut s = setup();
    let r = set_source_marks(&mut s, 1.0, 3.0);
    assert!(s.execute("edit.fitToFill", json!({})).is_err(), "needs timeline In and Out");
    let fd = s.sequence_rate().frame_duration();
    marks(&mut s, Some(secs(10.0)), Some(secs(14.0) - fd)); // a 4 s fill
    let n = s.history.undo.len();
    s.execute("edit.fitToFill", json!({})).unwrap();
    assert_eq!(s.history.undo.len(), n + 1);
    let it = &s.active_sequence().unwrap().video_tracks[0].items[0];
    assert_eq!(it.start, secs(10.0));
    assert_eq!(it.duration, secs(4.0));
    assert!((it.speed - r.duration.0 as f64 / secs(4.0).0 as f64).abs() < 1e-9);
}

#[test]
fn fit_to_fill_refuses_absurd_ratios_and_inverted_marks() {
    let mut s = setup();
    set_source_marks(&mut s, 0.0, 3.0);
    let fd = s.sequence_rate().frame_duration();
    marks(&mut s, Some(secs(12.0)), Some(secs(10.0) - fd));
    assert!(s.execute("edit.fitToFill", json!({})).is_err());
    assert_eq!(counts_v(&s), 0);
}

fn counts_v(s: &Session) -> usize {
    s.active_sequence().unwrap().video_tracks.iter().map(|t| t.items.len()).sum()
}

#[test]
fn smart_insert_goes_to_the_nearest_edit_point() {
    let mut s = setup();
    set_source_marks(&mut s, 1.0, 3.0);
    s.execute("edit.append", json!({})).unwrap();
    s.execute("edit.append", json!({})).unwrap();
    let boundary = v1(&s)[1].0;
    s.set_playhead(boundary + secs(0.1));
    s.execute("edit.smartInsert", json!({})).unwrap();
    let v = v1(&s);
    assert_eq!(v.len(), 3);
    assert_eq!(v[1].0, boundary);
    // empty timeline: at the playhead
    let mut e = setup();
    e.set_playhead(secs(1.0));
    e.execute("edit.smartInsert", json!({})).unwrap();
    assert_eq!(v1(&e)[0].0, secs(1.0));
    lock_v1(&mut e);
    assert!(e.execute("edit.smartInsert", json!({})).is_err());
}

#[test]
fn close_up_places_on_the_track_above_scaled_in() {
    let mut s = setup();
    let r = set_source_marks(&mut s, 1.0, 3.0);
    s.execute("edit.append", json!({})).unwrap();
    s.set_playhead(secs(0.5));
    let n = s.history.undo.len();
    s.execute("edit.closeUp", json!({})).unwrap();
    assert_eq!(s.history.undo.len(), n + 1);
    let q = s.active_sequence().unwrap();
    let it = &q.video_tracks[1].items[0];
    assert_eq!(it.start, secs(0.5));
    assert_eq!(it.duration, q.settings.frame_rate.snap_nearest(r.duration));
    let m = it.effects.iter().find(|e| e.effect == "motion").unwrap();
    let scale = match &m.params.get("scale").unwrap().value {
        filmcraft_project::ParamValue::Float(f) => *f,
        _ => panic!("scale is not a float"),
    };
    assert!((scale - 150.0).abs() < 1e-6 || scale > 100.0, "scale {scale}");
}

#[test]
fn source_overwrite_lines_up_timecodes_or_refuses() {
    let mut s = setup();
    set_source_marks(&mut s, 0.0, 4.0);
    s.execute("edit.append", json!({})).unwrap();
    set_source_marks(&mut s, 1.0, 2.0);
    s.set_playhead(secs(2.0)); // the clip's media time there is 2 s; the source In is 1 s earlier
    let n = s.history.undo.len();
    s.execute("edit.sourceOverwrite", json!({})).unwrap();
    assert_eq!(s.history.undo.len(), n + 1);
    let it = &s.active_sequence().unwrap().video_tracks[1].items[0];
    assert_eq!(it.start, secs(1.0));
    assert_eq!(it.source_in, secs(1.0));
    // no clip under the playhead
    s.set_playhead(secs(50.0));
    let before = s.project.to_json();
    assert!(s.execute("edit.sourceOverwrite", json!({})).is_err());
    assert_eq!(s.project.to_json(), before);
    // timecodes that do not meet: the source ends before the clip's timecode at the playhead
    set_source_marks(&mut s, 0.0, 0.5);
    s.set_playhead(secs(3.0));
    let before = s.project.to_json();
    assert!(s.execute("edit.sourceOverwrite", json!({})).is_err());
    assert_eq!(s.project.to_json(), before);
}
