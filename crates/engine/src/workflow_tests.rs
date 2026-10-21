//! G1.8: the current grade node, the Resolve keyboard preset and the Workflow preference.

use serde_json::{Value, json};

use crate::autosave::Preferences;
use crate::media_test_util::tmp_dir;
use crate::settings;
use crate::shortcuts::{Chord, CommandInfo, Platform};
use crate::{GradeSel, Session};
use filmcraft_project::ClipId;
use filmcraft_project::grade::Grade;

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

/// A session that starts in the Premiere workflow whatever the default is (the fork build's is
/// Resolve), so the switch tests below exercise the same transitions in both builds.
fn premiere_session() -> Session {
    let mut s = Session::default();
    s.prefs.workflow = settings::Workflow::Premiere;
    s
}

fn with_ui_commands(s: &mut Session) {
    s.shortcuts.register_external(vec![
        CommandInfo::new("playback.toggle", "Play/Stop", &[], Some("Space")),
        CommandInfo::new("playback.forward", "Shuttle Right", &[], Some("L")),
        CommandInfo::new("playback.slowForward", "Shuttle Slow Right", &[], None),
        CommandInfo::new("playback.playAround", "Play Around", &[], None),
        CommandInfo::new("tool.razor", "Razor Tool", &[], Some("C")),
    ]);
}

#[test]
fn current_node_is_the_default_target() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({}));
    assert_eq!(s.state.grade_node, Some(GradeSel { clip: c, node: a }), "a new node becomes current");
    let b = add(&mut s, json!({}));
    assert_eq!(s.state.grade_node, Some(GradeSel { clip: c, node: b }));
    // select the first node: add goes after it, insert goes before the current one
    s.execute("grade.selectNode", json!({"node": a})).unwrap();
    let mid = add(&mut s, json!({}));
    assert_eq!(ids(&s, c), vec![a, mid, b]);
    s.execute("grade.selectNode", json!({"node": b})).unwrap();
    let before = s.execute("grade.insertNode", json!({})).unwrap()["node"].as_u64().unwrap();
    assert_eq!(ids(&s, c), vec![a, mid, before, b]);
    // node-less commands act on the current node (the inserted one)
    s.execute("grade.toggleNode", json!({})).unwrap();
    s.execute("grade.setParam", json!({"param": "saturation", "value": 1.5})).unwrap();
    s.execute("grade.labelNode", json!({"label": "Cur"})).unwrap();
    let g = grade(&s, c).unwrap();
    assert!(!g.node(before).unwrap().enabled);
    assert_eq!(g.node(before).unwrap().label, "Cur");
    assert!(g.node(before).unwrap().params.contains_key("saturation"));
    assert!(g.node(b).unwrap().enabled);
    s.execute("grade.resetNode", json!({})).unwrap();
    assert!(grade(&s, c).unwrap().node(before).unwrap().params.is_empty());
    // an explicit node still wins
    s.execute("grade.toggleNode", json!({"node": a})).unwrap();
    assert!(!grade(&s, c).unwrap().node(a).unwrap().enabled);
}

#[test]
fn removing_the_current_node_selects_the_previous_then_the_next() {
    let (mut s, c) = demo();
    let a = add(&mut s, json!({}));
    let b = add(&mut s, json!({}));
    let d = add(&mut s, json!({}));
    s.execute("grade.selectNode", json!({"node": b})).unwrap();
    s.execute("grade.removeNode", json!({})).unwrap();
    assert_eq!(ids(&s, c), vec![a, d]);
    assert_eq!(s.state.grade_node, Some(GradeSel { clip: c, node: a }), "previous");
    s.execute("grade.removeNode", json!({})).unwrap();
    assert_eq!(s.state.grade_node, Some(GradeSel { clip: c, node: d }), "no previous: next");
    s.execute("grade.removeNode", json!({})).unwrap();
    assert_eq!(s.state.grade_node, None);
    // removing another node leaves the selection alone
    let x = add(&mut s, json!({}));
    let y = add(&mut s, json!({}));
    s.execute("grade.removeNode", json!({"node": x})).unwrap();
    assert_eq!(s.state.grade_node, Some(GradeSel { clip: c, node: y }));
}

#[test]
fn node_less_commands_need_a_current_node_unless_the_grade_has_one() {
    let (mut s, c) = demo();
    // no grade: an error, never a panic
    assert!(s.execute("grade.toggleNode", json!({})).is_err());
    let a = add(&mut s, json!({}));
    // a single node needs no selection
    s.execute("grade.selectNode", json!({})).unwrap();
    s.execute("grade.toggleNode", json!({})).unwrap();
    assert!(!grade(&s, c).unwrap().node(a).unwrap().enabled);
    let b = add(&mut s, json!({}));
    let _ = b;
    s.execute("grade.selectNode", json!({})).unwrap();
    let e = s.execute("grade.toggleNode", json!({})).unwrap_err().to_string();
    assert!(e.contains("no current node"), "{e}");
    assert!(s.execute("grade.setParam", json!({"param": "contrast", "value": 1.2})).is_err());
    // add / insert keep their defaults without a current node: end / start
    let end = add(&mut s, json!({}));
    s.execute("grade.selectNode", json!({})).unwrap();
    let first = s.execute("grade.insertNode", json!({})).unwrap()["node"].as_u64().unwrap();
    s.execute("grade.selectNode", json!({})).unwrap();
    let last = add(&mut s, json!({}));
    assert_eq!(ids(&s, c), vec![first, a, b, end, last]);
    // hostile node arguments
    assert!(s.execute("grade.toggleNode", json!({"node": "x"})).is_err());
    assert!(s.execute("grade.toggleNode", json!({"node": -1})).is_err());
    assert!(s.execute("grade.selectNode", json!({"node": -1})).is_err());
    assert!(s.execute("grade.toggleNode", json!({"node": 9999})).is_err());
    // selecting a node the grade does not hold is refused
    s.execute("grade.selectNode", json!({})).unwrap();
    assert!(s.execute("grade.selectNode", json!({"node": 9999})).is_err());
    assert_eq!(s.state.grade_node, None);
    // a current node that no longer exists (undo) is treated as none
    s.execute("grade.selectNode", json!({"node": last})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.execute("grade.toggleNode", json!({})).is_err());
}

#[test]
fn the_current_node_belongs_to_its_clip() {
    let (mut s, a_clip) = demo();
    let other = {
        let q = s.active_sequence().unwrap();
        q.video_tracks.iter().flat_map(|t| t.items.iter()).map(|i| i.id).find(|i| *i != a_clip).expect("a second video clip")
    };
    // node ids restart per grade, so both clips get a node 1 and a node 2
    let a1 = add(&mut s, json!({"clip": a_clip.0}));
    let a2 = add(&mut s, json!({"clip": a_clip.0}));
    let b1 = add(&mut s, json!({"clip": other.0}));
    let b2 = add(&mut s, json!({"clip": other.0}));
    assert_eq!((a1, a2), (b1, b2), "same node ids on both clips");
    // node a2 is current on clip A
    s.execute("grade.selectNode", json!({"clip": a_clip.0, "node": a2})).unwrap();
    assert_eq!(s.state.grade_node, Some(GradeSel { clip: a_clip, node: a2 }));
    // a node-less edit of clip B must not use A's selection
    assert!(s.execute("grade.toggleNode", json!({"clip": other.0})).is_err());
    assert!(grade(&s, other).unwrap().nodes.iter().all(|n| n.enabled));
    // adding to clip B appends (A's selection is not B's anchor)
    let b3 = add(&mut s, json!({"clip": other.0}));
    assert_eq!(ids(&s, other), vec![b1, b2, b3]);
    // the selection moved to B's new node; A's node-less edit now needs a selection again
    assert_eq!(s.state.grade_node, Some(GradeSel { clip: other, node: b3 }));
    assert!(s.execute("grade.toggleNode", json!({"clip": a_clip.0})).is_err());
    // selecting on a clip without that node is refused
    assert!(s.execute("grade.selectNode", json!({"clip": a_clip.0, "node": b3})).is_err());
}

#[test]
fn resolve_preset_maps_editing_and_grade_commands() {
    let mut s = Session::default();
    with_ui_commands(&mut s);
    s.execute("shortcuts.loadPreset", json!({"name": "DaVinci Resolve Compatible"})).unwrap();
    assert_eq!(s.shortcuts.preset, "DaVinci Resolve Compatible");
    assert!(!s.shortcuts.modified);
    let key = |c: &str| s.shortcuts.primary(c);
    assert_eq!(key("sequence.addEdit").as_deref(), Some("Cmd+B"));
    assert_eq!(key("tool.razor").as_deref(), Some("B"));
    assert_eq!(key("sequence.snap").as_deref(), Some("N"));
    assert_eq!(key("markers.markIn").as_deref(), Some("I"));
    assert_eq!(key("markers.markOut").as_deref(), Some("O"));
    assert_eq!(key("playback.forward").as_deref(), Some("L"));
    assert_eq!(key("clip.enable").as_deref(), Some("D"));
    assert_eq!(key("grade.addNode").as_deref(), Some("Alt+S"));
    assert_eq!(key("grade.insertNode").as_deref(), Some("Shift+S"));
    assert_eq!(key("grade.toggleNode").as_deref(), Some("Cmd+D"));
    assert_eq!(key("grade.bypass").as_deref(), Some("Shift+D"));
    // fork C2: Resolve's node keys, Alt+P parallel, Alt+L layer, Alt+O outside (provisional, owner
    // question: Resolve keys Alt+P / Alt+O); the Premiere preset keeps its own Alt+P / Alt+O
    let at = |s: &Session, k: &str, plat| s.shortcuts.resolve(&Chord::parse(k).unwrap(), None, plat).map(|b| b.command.clone());
    for plat in [Platform::Mac, Platform::Linux] {
        assert_eq!(at(&s, "Alt+L", plat).as_deref(), Some("grade.addLayer"));
        assert_eq!(at(&s, "Alt+P", plat).as_deref(), Some("grade.addParallel"));
        assert_eq!(at(&s, "Alt+O", plat).as_deref(), Some("grade.addOutside"));
    }
    let mut prem = Session::default();
    with_ui_commands(&mut prem);
    prem.execute("shortcuts.loadPreset", json!({"name": "Premiere Pro Compatible"})).unwrap();
    assert_eq!(at(&prem, "Alt+P", Platform::Mac).as_deref(), Some("clip.clearPosterFrame"));
    assert_eq!(at(&prem, "Alt+O", Platform::Mac).as_deref(), Some("markers.clearOut"));
    assert!(s.shortcuts.for_command("edit.rippleDelete").iter().any(|b| b.keys == "Shift+Backspace"));
    // Shift+D belongs to bypass, not to the Premiere default transition; Cmd+D is not a transition
    let hit = s.shortcuts.resolve(&Chord::parse("Shift+D").unwrap(), None, Platform::Mac).map(|b| b.command.clone());
    assert_eq!(hit.as_deref(), Some("grade.bypass"));
    let hit = s.shortcuts.resolve(&Chord::parse("Cmd+D").unwrap(), None, Platform::Mac).map(|b| b.command.clone());
    assert_eq!(hit.as_deref(), Some("grade.toggleNode"));
    assert!(s.shortcuts.conflicts(Platform::Mac).is_empty(), "{:?}", s.shortcuts.conflicts(Platform::Mac));
    // elsewhere (Cmd = Ctrl) the inherited Premiere keys collide in places (Ctrl+9 and Cmd+9…), but
    // no Resolve-table command may be part of a conflict
    for plat in [Platform::Windows, Platform::Linux] {
        for (ctx, keys, cmds) in s.shortcuts.conflicts(plat) {
            let mine = crate::shortcut_presets::RESOLVE.iter().any(|(c, _, _)| cmds.iter().any(|x| x == c));
            assert!(!mine, "{plat:?} {ctx} {keys}: {cmds:?}");
        }
    }
    // every id the table lists is a real command, except the frontend's and the stills / compare
    // commands that ship separately (unknown ids are skipped when the preset is built)
    for (c, _, _) in crate::shortcut_presets::RESOLVE {
        let ui = ["playback.", "page.", "window.workspace.", "window.toggleFullScreen", "mode.", "tool.", "view."].iter().any(|p| c.starts_with(p));
        let later = ["stills.grab", "compare.set"].contains(c);
        assert!(ui || later || crate::commands::find(c).is_some(), "unknown command {c}");
    }
    let list = s.execute("shortcuts.presets", json!({})).unwrap();
    assert!(list["builtin"].as_array().unwrap().iter().any(|n| n == "DaVinci Resolve Compatible"));
}

#[test]
fn old_preferences_without_workflow_load_as_premiere() {
    let dir = tmp_dir("prefs-workflow-old");
    let path = dir.join("preferences.json");
    std::fs::write(&path, serde_json::to_vec(&json!({"version": 2, "timeline": {"stillImageDuration": 7}})).unwrap()).unwrap();
    let p = Preferences::load(&path);
    assert_eq!(p.workflow, settings::Workflow::Premiere);
    assert_eq!(p.workflow_return_preset, "");
    assert_eq!(p.timeline.still_image_duration, 7.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn workflow_switch_loads_resolve_keys_and_restores_the_previous_preset() {
    let dir = tmp_dir("prefs-workflow");
    let mut s = premiere_session();
    s.start_autosave(crate::autosave::AutosaveConfig::new(&dir)).unwrap();
    s.shortcuts.set_dir(&dir);
    // the fork build's one-time migration already moved this fresh directory to Resolve
    s.set_workflow(settings::Workflow::Premiere).unwrap();
    assert_eq!(s.execute("settings.getWorkflow", json!({})).unwrap()["workflow"], "premiere");
    // start from Premiere keys plus a personal edit
    s.execute("shortcuts.loadPreset", json!({"name": "Premiere Pro Compatible"})).unwrap();
    s.execute("shortcuts.set", json!({"command": "markers.add", "keys": "Cmd+Alt+J"})).unwrap();
    assert!(s.shortcuts.modified);
    let r = s.execute("settings.setWorkflow", json!({"workflow": "resolve"})).unwrap();
    assert_eq!(r["workflow"], "resolve");
    assert_eq!(s.shortcuts.preset, "DaVinci Resolve Compatible");
    assert_eq!(s.shortcuts.primary("grade.addNode").as_deref(), Some("Alt+S"));
    assert_eq!(s.prefs.workflow, settings::Workflow::Resolve);
    // the edited set was kept as a custom preset
    assert_eq!(s.shortcuts.user_presets(), vec!["Premiere Pro Compatible (before Resolve)".to_string()]);
    // the choice persists
    let file: Value = serde_json::from_slice(&std::fs::read(dir.join("preferences.json")).unwrap()).unwrap();
    assert_eq!(file["workflow"], "resolve");
    // back to Premiere: the edited set returns exactly
    let r = s.execute("settings.setWorkflow", json!({"workflow": "premiere"})).unwrap();
    assert_eq!(r["workflow"], "premiere");
    assert_eq!(s.shortcuts.preset, "Premiere Pro Compatible (before Resolve)");
    assert_eq!(s.shortcuts.primary("markers.add").as_deref(), Some("Cmd+Alt+J"));
    assert_eq!(s.shortcuts.primary("markers.markClip").as_deref(), Some("X"));
    assert!(s.shortcuts.primary("grade.addNode").is_none());
    assert_eq!(s.prefs.workflow_return_preset, "");
    // an unmodified built-in comes back as itself; same-workflow calls change nothing
    s.execute("shortcuts.loadPreset", json!({"name": "Avid Media Composer Compatible"})).unwrap();
    s.execute("settings.setWorkflow", json!({"workflow": "premiere"})).unwrap();
    assert_eq!(s.shortcuts.preset, "Avid Media Composer Compatible");
    s.execute("settings.setWorkflow", json!({"workflow": "resolve"})).unwrap();
    s.execute("settings.setWorkflow", json!({"workflow": "premiere"})).unwrap();
    assert_eq!(s.shortcuts.preset, "Avid Media Composer Compatible");
    assert!(s.execute("settings.setWorkflow", json!({"workflow": "avid"})).is_err());
    assert!(s.execute("prefs.set", json!({"key": "workflow", "value": "avid"})).is_err());
    // a fresh default session returns to FilmCraft Default
    let mut t = premiere_session();
    t.execute("settings.setWorkflow", json!({"workflow": "resolve"})).unwrap();
    t.execute("settings.setWorkflow", json!({"workflow": "premiere"})).unwrap();
    assert_eq!(t.shortcuts.preset, "Revolution Default");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn garbage_workflow_values_keep_every_other_preference() {
    let dir = tmp_dir("prefs-workflow-garbage");
    let path = dir.join("preferences.json");
    for bad in [json!(7), json!("garbage"), json!(null), json!({"a": 1}), json!("Resolve")] {
        let file = json!({"version": 2, "timeline": {"stillImageDuration": 7}, "workflow": bad, "workflowReturnPreset": 5});
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let p = Preferences::load(&path);
        assert_eq!(p.workflow, settings::Workflow::Premiere, "{bad}");
        assert_eq!(p.workflow_return_preset, "");
        assert_eq!(p.timeline.still_image_duration, 7.0, "other preferences survive {bad}");
    }
    let file = json!({"version": 2, "timeline": {"stillImageDuration": 7}, "workflow": "resolve", "workflowReturnPreset": "Mine"});
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    let p = Preferences::load(&path);
    assert_eq!((p.workflow, p.workflow_return_preset.as_str()), (settings::Workflow::Resolve, "Mine"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prefs_commands_cannot_bypass_set_workflow() {
    let mut s = premiere_session();
    assert!(s.execute("prefs.set", json!({"key": "workflow", "value": "resolve"})).is_err());
    assert!(s.execute("prefs.set", json!({"values": {"workflow": "resolve", "timeline.stillImageDuration": 9}})).is_err());
    assert!(s.execute("prefs.set", json!({"key": "workflowReturnPreset", "value": "X"})).is_err());
    assert_eq!(s.prefs.workflow, settings::Workflow::Premiere);
    assert_eq!(s.prefs.timeline.still_image_duration, 5.0, "a refused prefs.set changes nothing");
    s.execute("settings.setWorkflow", json!({"workflow": "resolve"})).unwrap();
    s.execute("prefs.set", json!({"key": "timeline.stillImageDuration", "value": 9})).unwrap();
    // a full reset keeps the workflow (it follows the keyboard preset)
    s.execute("prefs.reset", json!({})).unwrap();
    assert_eq!(s.prefs.workflow, settings::Workflow::Resolve);
    assert_eq!(s.prefs.timeline.still_image_duration, 5.0);
    assert_eq!(s.shortcuts.preset, "DaVinci Resolve Compatible");
}

#[test]
fn a_failed_save_refuses_the_switch() {
    // no data directory: custom presets cannot be saved
    let mut s = premiere_session();
    s.execute("shortcuts.set", json!({"command": "markers.add", "keys": "Cmd+Alt+J"})).unwrap();
    assert!(s.shortcuts.modified);
    assert!(s.execute("settings.setWorkflow", json!({"workflow": "resolve"})).is_err());
    assert_eq!(s.prefs.workflow, settings::Workflow::Premiere);
    assert_eq!(s.shortcuts.preset, "Revolution Default");
    assert_eq!(s.shortcuts.primary("markers.add").as_deref(), Some("Cmd+Alt+J"), "edits untouched");
    // and the same on the way back
    let mut t = premiere_session();
    t.execute("settings.setWorkflow", json!({"workflow": "resolve"})).unwrap();
    t.execute("shortcuts.set", json!({"command": "markers.add", "keys": "Cmd+Alt+J"})).unwrap();
    assert!(t.execute("settings.setWorkflow", json!({"workflow": "premiere"})).is_err());
    assert_eq!(t.prefs.workflow, settings::Workflow::Resolve);
    assert_eq!(t.shortcuts.preset, "DaVinci Resolve Compatible");
    assert_eq!(t.shortcuts.primary("markers.add").as_deref(), Some("Cmd+Alt+J"));
}

#[test]
fn saved_presets_are_never_overwritten_and_resolve_edits_are_kept() {
    let dir = tmp_dir("prefs-workflow-names");
    let mut s = premiere_session();
    s.shortcuts.set_dir(&dir);
    for round in 1..=2 {
        s.execute("shortcuts.loadPreset", json!({"name": "Premiere Pro Compatible"})).unwrap();
        s.execute("shortcuts.set", json!({"command": "markers.add", "keys": format!("Cmd+Alt+{round}")})).unwrap();
        s.execute("settings.setWorkflow", json!({"workflow": "resolve"})).unwrap();
        // an edit made in Resolve is saved on the way back
        s.execute("shortcuts.set", json!({"command": "markers.add", "keys": "Cmd+Alt+Q"})).unwrap();
        s.execute("settings.setWorkflow", json!({"workflow": "premiere"})).unwrap();
    }
    let custom = s.shortcuts.user_presets();
    assert_eq!(
        custom,
        vec![
            "DaVinci Resolve Compatible (edited)".to_string(),
            "DaVinci Resolve Compatible (edited) 2".to_string(),
            "Premiere Pro Compatible (before Resolve)".to_string(),
            "Premiere Pro Compatible (before Resolve) 2".to_string(),
        ]
    );
    // each round restored its own edited Premiere set; the saved Resolve edits kept theirs
    assert_eq!(s.shortcuts.preset, "Premiere Pro Compatible (before Resolve) 2");
    assert_eq!(s.shortcuts.primary("markers.add").as_deref(), Some("Cmd+Alt+2"));
    s.execute("shortcuts.loadPreset", json!({"name": "Premiere Pro Compatible (before Resolve)"})).unwrap();
    assert_eq!(s.shortcuts.primary("markers.add").as_deref(), Some("Cmd+Alt+1"));
    s.execute("shortcuts.loadPreset", json!({"name": "DaVinci Resolve Compatible (edited)"})).unwrap();
    assert_eq!(s.shortcuts.primary("markers.add").as_deref(), Some("Cmd+Alt+Q"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_preset_loaded_by_hand_in_resolve_stays_when_switching_back() {
    let dir = tmp_dir("prefs-workflow-manual");
    let mut s = premiere_session();
    s.shortcuts.set_dir(&dir);
    s.execute("settings.setWorkflow", json!({"workflow": "resolve"})).unwrap();
    s.execute("shortcuts.loadPreset", json!({"name": "Avid Media Composer Compatible"})).unwrap();
    s.execute("settings.setWorkflow", json!({"workflow": "premiere"})).unwrap();
    assert_eq!(s.shortcuts.preset, "Avid Media Composer Compatible");
    assert_eq!(s.prefs.workflow, settings::Workflow::Premiere);
    assert_eq!(s.prefs.workflow_return_preset, "");
    assert!(s.shortcuts.user_presets().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
