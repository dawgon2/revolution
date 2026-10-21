//! Grading Coach context hints (fork GRADE-PLAN G2.12 part 3): the bundled file, hostile files,
//! which hint the engine shows (commands, elements, the heavy-look rule; never during a lesson,
//! once each, off with the switch), the commands and the preferences; and the lessons' `already`
//! condition (a "select a clip" step with a clip already selected).

use serde_json::{Value, json};

use crate::Session;
use crate::coach_hints::{self, Trigger, parse};

fn demo() -> (Session, u64) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let seq = s.execute("sequence.inspect", json!({})).unwrap();
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().expect("a clip");
    (s, clip)
}

fn shown(s: &mut Session) -> Value {
    s.execute("coach.hint", json!({})).unwrap()["hint"]["id"].clone()
}

#[test]
fn the_bundled_hints_parse_and_name_real_things() {
    let hints = coach_hints::bundled();
    assert!(hints.len() >= 10, "{}", hints.len());
    assert_eq!(parse(coach_hints::BUNDLED).unwrap().len(), hints.len());
    let mut rule = false;
    for h in hints {
        if let Some(l) = &h.lesson {
            assert!(crate::coach::lesson(l).is_some(), "{}: no lesson `{l}`", h.id);
        }
        for t in &h.when {
            match t {
                Trigger::Command { ids, .. } => {
                    for id in ids {
                        assert!(crate::find_command(id).is_some(), "{}: no command `{id}`", h.id);
                    }
                }
                Trigger::Element(e) => assert!(e.contains('.'), "{}: {e}", h.id),
                Trigger::Rule(_) => rule = true,
            }
        }
        // why before what: both say something
        assert!(h.why.len() > 20 && h.text.len() > 20, "{}", h.id);
    }
    assert!(rule, "the heavy-look rule is used");
}

#[test]
fn hostile_hint_files_are_refused_with_a_reason() {
    let ok = r#"
[[hint]]
id = "a"
title = "A"
why = "Because."
text = "This."
when = [{ command = "grade.addNode" }]
"#;
    assert_eq!(parse(ok).unwrap().len(), 1);
    let bad = |src: &str, want: &str| {
        let e = parse(src).expect_err(src);
        assert!(e.contains(want), "{e} (want {want})");
    };
    bad(&ok.replace("id = \"a\"", "id = \"A b\""), "id");
    bad(&ok.replace("when = [{ command = \"grade.addNode\" }]", "when = []"), "triggers");
    bad(&ok.replace("{ command = \"grade.addNode\" }", "{ command = \"grade.addNode\", element = \"x.y\" }"), "exactly one");
    bad(&ok.replace("{ command = \"grade.addNode\" }", "{ element = \"x.y\", params = { a = 1 } }"), "params");
    bad(&ok.replace("{ command = \"grade.addNode\" }", "{ rule = \"nope\" }"), "unknown variant");
    bad(&ok.replace("{ command = \"grade.addNode\" }", "{ command = \"a\", params = { v = { gt = \"x\" } } }"), "wrong kind");
    bad(&ok.replace("why = \"Because.\"", "why = \"\""), "empty");
    bad(&ok.replace("why = \"Because.\"", "why = \"x\"\nsurprise = 1"), "unknown field");
    bad(&format!("{ok}{ok}"), "duplicate");
    bad(&ok.replace("text = \"This.\"", &format!("text = \"{}\"", "x".repeat(2000))), "longer");
    bad(&"#".repeat(70_000), "larger");
    bad(&ok.repeat(0), "hint");
}

#[test]
fn a_hint_shows_once_and_never_during_a_lesson() {
    let (mut s, clip) = demo();
    assert_eq!(shown(&mut s), json!(null));
    s.execute("grade.addNode", json!({"clip": clip})).unwrap();
    assert_eq!(shown(&mut s), json!("first-node"));
    let h = s.execute("coach.hint", json!({})).unwrap();
    assert_eq!(h["hint"]["lesson"]["id"], json!("nodes-and-flow"), "{h}");
    assert!(h["hint"]["lesson"]["title"].is_string(), "{h}");
    assert_eq!(h["seen"], json!(["first-node"]));
    // one at a time: a second trigger while it shows is not used up
    s.execute("stills.grab", json!({"clip": clip})).unwrap();
    assert_eq!(shown(&mut s), json!("first-node"));
    s.execute("coach.dismissHint", json!({})).unwrap();
    assert_eq!(shown(&mut s), json!(null));
    s.execute("grade.addNode", json!({"clip": clip})).unwrap();
    assert_eq!(shown(&mut s), json!(null), "each hint shows once");
    s.execute("stills.grab", json!({"clip": clip})).unwrap();
    assert_eq!(shown(&mut s), json!("first-still"));
    // a lesson clears the hint and no hint shows while it runs
    s.execute("coach.start", json!({"lesson": "first-grade"})).unwrap();
    assert_eq!(shown(&mut s), json!(null));
    let still = s.execute("stills.list", json!({})).unwrap()["stills"][0]["id"].as_u64().expect("a still");
    s.execute("compare.set", json!({"still": still, "mode": "wipe"})).unwrap();
    s.execute("coach.seen", json!({"element": "grade.qual.pick"})).unwrap();
    assert_eq!(shown(&mut s), json!(null));
    assert!(!s.prefs.color.coach.hints_seen.contains(&"first-wipe".to_string()), "not used up during a lesson");
    assert!(s.coach_hint_elements().is_empty(), "no element is watched during a lesson");
    s.execute("coach.exit", json!({})).unwrap();
    // an element trigger: reported by the UI (or an agent)
    assert!(s.coach_hint_elements().contains(&"grade.qual.pick"));
    s.execute("coach.seen", json!({"element": "grade.qual.pick"})).unwrap();
    assert_eq!(shown(&mut s), json!("first-qualifier"));
    assert!(s.coach_hint_elements().is_empty(), "nothing is watched while a hint shows");
}

#[test]
fn param_conditions_and_the_ai_switch_paths() {
    let (mut s, clip) = demo();
    s.prefs.color.coach.hints_seen = vec!["first-node".into()];
    s.execute("grade.addNode", json!({"clip": clip})).unwrap();
    // grain at 0 is not "adding grain"
    s.execute("grade.setParam", json!({"clip": clip, "param": "grain.strength", "value": 0.0})).unwrap();
    assert_eq!(shown(&mut s), json!(null));
    s.execute("grade.setParam", json!({"clip": clip, "param": "grain.strength", "value": 0.2})).unwrap();
    assert_eq!(shown(&mut s), json!("first-grain"));
    s.execute("coach.dismissHint", json!({})).unwrap();
    // the Settings dialog's OK turns AI explanations on with prefs.set
    s.execute("prefs.set", json!({"values": {"color.ai.enabled": true}})).unwrap();
    assert_eq!(shown(&mut s), json!("ai-explanations-on"));
}

#[test]
fn a_qualifier_after_a_heavy_look_gets_its_hint() {
    let (mut s, clip) = demo();
    s.prefs.color.coach.hints_seen = vec!["first-node".into()];
    let look = s.execute("grade.addNode", json!({"clip": clip})).unwrap()["node"].as_u64().unwrap();
    let skin = s.execute("grade.addNode", json!({"clip": clip})).unwrap()["node"].as_u64().unwrap();
    // a qualifier after a mild node: no hint
    s.execute("grade.setParam", json!({"clip": clip, "node": look, "param": "saturation", "value": 1.1})).unwrap();
    s.execute("grade.setParam", json!({"clip": clip, "node": skin, "param": "qual.on", "value": 1.0})).unwrap();
    assert_eq!(shown(&mut s), json!(null));
    // the look gets heavy: the rule holds
    s.execute("grade.setParam", json!({"clip": clip, "node": look, "param": "saturation", "value": 1.6})).unwrap();
    assert_eq!(shown(&mut s), json!("qualifier-after-look"));
    // the rule itself
    let g = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.grade.clone().unwrap();
    assert!(coach_hints::qualifier_after_heavy_look(&g));
    let mut before = g.clone();
    before.nodes.reverse();
    assert!(!coach_hints::qualifier_after_heavy_look(&before), "a qualifier before the look is fine");
    // a curve only counts when it bends visibly
    let mut node = filmcraft_project::grade::GradeNode::new(9);
    let curve = |pts: Vec<[f32; 2]>| filmcraft_project::Param::new(filmcraft_project::ParamValue::Curve(pts));
    node.params.insert("curve.master".into(), curve(vec![[0.0, 0.0], [0.5, 0.52], [1.0, 1.0]]));
    assert!(!coach_hints::heavy_look(&node), "a 0.02 nudge is not a look");
    node.params.insert("curve.master".into(), curve(vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]));
    assert!(coach_hints::heavy_look(&node));
    let mut off = g.clone();
    off.nodes[0].enabled = false;
    assert!(!coach_hints::qualifier_after_heavy_look(&off), "a disabled look doesn't count");
}

#[test]
fn the_switch_turns_hints_off_and_reset_shows_them_again() {
    let (mut s, clip) = demo();
    let dir = std::env::temp_dir().join(format!("filmcraft-coach-hints-{}", std::process::id()));
    s.prefs_path = Some(dir.join("preferences.json"));
    s.execute("grade.addNode", json!({"clip": clip})).unwrap();
    assert_eq!(shown(&mut s), json!("first-node"));
    let saved = std::fs::read_to_string(dir.join("preferences.json")).unwrap();
    assert!(saved.contains("hintsSeen") && saved.contains("first-node"), "{saved}");
    // "Turn hints off" on the card
    let v = s.execute("coach.dismissHint", json!({"off": true})).unwrap();
    assert_eq!((v["hint"].clone(), v["enabled"].clone()), (json!(null), json!(false)));
    s.execute("stills.grab", json!({"clip": clip})).unwrap();
    assert_eq!(shown(&mut s), json!(null), "off");
    assert!(s.coach_hint_elements().is_empty());
    // back on with prefs.set; Settings turning them off again clears a hint on screen
    s.execute("prefs.set", json!({"key": "color.coach.hints", "value": true})).unwrap();
    let still = s.execute("stills.list", json!({})).unwrap()["stills"][0]["id"].as_u64().expect("a still");
    s.execute("compare.set", json!({"still": still, "mode": "split"})).unwrap();
    assert_eq!(shown(&mut s), json!("first-wipe"));
    let rev = s.coach.revision;
    s.execute("prefs.set", json!({"key": "color.coach.hints", "value": false})).unwrap();
    assert_eq!(shown(&mut s), json!(null));
    assert_ne!(s.coach.revision, rev, "the cleared hint is a change watchers see");
    s.execute("prefs.set", json!({"key": "color.coach.hints", "value": true})).unwrap();
    let v = s.execute("coach.resetHints", json!({})).unwrap();
    assert_eq!(v["seen"], json!([]));
    s.execute("grade.addNode", json!({"clip": clip})).unwrap();
    assert_eq!(shown(&mut s), json!("first-node"), "shown again after a reset");
    let all = s.execute("coach.hint", json!({"all": true})).unwrap();
    assert_eq!(all["all"].as_array().map(Vec::len), Some(coach_hints::bundled().len()));
    // coach commands aren't edits
    assert!(!s.journal.iter().any(|(id, _)| id.starts_with("coach.")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hints_seen_is_lenient_and_capped() {
    let mut v = crate::autosave::Preferences::default().to_value();
    v["color"]["coach"]["hintsSeen"] = json!("nope");
    let p = crate::autosave::Preferences::from_value(v.clone());
    assert!(p.color.coach.hints_seen.is_empty());
    assert!(p.color.coach.hints, "the switch survives");
    let mut list: Vec<Value> = (0..coach_hints::MAX_SEEN + 40).map(|i| json!(format!("h-{i}"))).collect();
    list.push(json!(7));
    list.push(json!("bad id!"));
    v["color"]["coach"]["hintsSeen"] = json!(list);
    let p = crate::autosave::Preferences::from_value(v);
    let seen = &p.color.coach.hints_seen;
    assert_eq!(seen.len(), coach_hints::MAX_SEEN);
    assert_eq!(seen.last().map(String::as_str), Some(format!("h-{}", coach_hints::MAX_SEEN + 39).as_str()));
    // showing more hints than the cap keeps the newest
    let (mut s, clip) = demo();
    s.prefs.color.coach.hints_seen = (0..coach_hints::MAX_SEEN).map(|i| format!("old-{i}")).collect();
    s.execute("grade.addNode", json!({"clip": clip})).unwrap();
    assert_eq!(s.prefs.color.coach.hints_seen.len(), coach_hints::MAX_SEEN);
    assert_eq!(s.prefs.color.coach.hints_seen.last().map(String::as_str), Some("first-node"));
}

// ---------- the lessons' `already` condition ----------

#[test]
fn a_select_step_finishes_when_a_clip_is_already_selected() {
    let (mut s, clip) = demo();
    s.execute("timeline.select", json!({"clips": [clip]})).unwrap();
    let st = s.execute("coach.start", json!({"lesson": "first-grade"})).unwrap();
    assert_eq!(st["step"], json!(0));
    s.execute("coach.next", json!({})).unwrap();
    // the workspace step finishes; the select step (a clip is selected) finishes by itself
    let st = s.execute("coach.seen", json!({"element": "panel.ColorPrimaries"})).unwrap();
    assert_eq!(st["step"], json!(3), "{st}");
    // Back to it: it stays (the reader went back on purpose), also on coach.state
    s.execute("coach.back", json!({})).unwrap();
    let st = s.execute("coach.state", json!({})).unwrap();
    assert_eq!((st["step"].clone(), st["already"].clone()), (json!(2), json!("clipSelected")), "{st}");
    s.execute("timeline.select", json!({"clips": [clip]})).unwrap();
    assert_eq!(s.execute("coach.state", json!({})).unwrap()["step"], json!(3));
}

#[test]
fn coach_state_settles_a_step_whose_condition_became_true() {
    let (mut s, clip) = demo();
    s.execute("coach.start", json!({"lesson": "nodes-and-flow"})).unwrap();
    s.execute("coach.next", json!({})).unwrap();
    s.execute("coach.seen", json!({"element": "panel.ColorNodes"})).unwrap();
    assert_eq!(s.execute("coach.state", json!({})).unwrap()["step"], json!(2));
    // selected without a command the step sees (e.g. restored state): coach.state finishes it
    s.state.selection = vec![filmcraft_project::ClipId(clip)];
    assert_eq!(s.execute("coach.state", json!({})).unwrap()["step"], json!(3));
    // only lessons that work on any clip skip the step; those that need a particular clip (a
    // face, a noisy shot, a reference) always wait for the click
    for l in crate::coach::bundled() {
        let any = l.steps.iter().any(|st| st.already.is_some());
        assert_eq!(
            any,
            ["first-grade", "nodes-and-flow", "which-node", "parallel-nodes", "layer-look", "splitter-combiner"].contains(&l.id.as_str()),
            "{}",
            l.id
        );
    }
}
