//! Grading Coach lessons (fork GRADE-PLAN G2.12): the lesson file parser (valid and hostile
//! files), parameter patterns, the runner's transitions and the bundled lessons.

use serde_json::{Value, json};

use crate::Session;
use crate::coach::{self, Advance, Coach, Expect, matches, parse};

const SMALL: &str = r#"
id = "small"
title = "Small"
provenance = ["fork"]

[[step]]
text = "Read this."
expect = { manual = true }

[[step]]
text = "Raise saturation."
why = "Because."
target = "grade.param.saturation"
expect = { command = "grade.setParam", params = { param = "saturation", value = { gt = 1.0 } } }

[[step]]
text = "Look at the nodes."
expect = { element = "panel.ColorNodes" }
"#;

fn small() -> std::sync::Arc<coach::Lesson> {
    std::sync::Arc::new(parse(SMALL).expect("valid lesson"))
}

#[test]
fn a_valid_lesson_parses() {
    let l = parse(SMALL).unwrap();
    assert_eq!(l.id, "small");
    assert_eq!(l.steps.len(), 3);
    assert_eq!(l.steps[0].expect, Expect::Manual);
    assert_eq!(l.steps[1].targets, ["grade.param.saturation"]);
    assert!(matches!(&l.steps[1].expect, Expect::Command { ids, params: Some(_) } if ids == &["grade.setParam"]));
    assert_eq!(l.steps[2].expect, Expect::Element("panel.ColorNodes".into()));
}

#[test]
fn hostile_lesson_files_are_refused_with_a_reason() {
    let head = "id = \"x\"\ntitle = \"X\"\nprovenance = [\"fork\"]\n";
    let step = |body: &str| format!("{head}[[step]]\ntext = \"t\"\n{body}\n");
    let cases: Vec<(String, &str)> = vec![
        ("not toml at all [[[".into(), ""),
        (head.to_string(), "step"),
        (step("expect = { manual = true }\nsurprise = 1"), "unknown field"),
        (step("expect = { manual = true, element = \"a\" }"), "exactly one"),
        (step("expect = { }"), "exactly one"),
        (step("expect = { manual = false }"), "must be true"),
        (step("expect = { element = \"a b\" }"), "automation id"),
        (step("expect = { command = \"x;rm\" }"), "command id"),
        (step("expect = { element = \"a\", params = { x = 1 } }"), "only goes with"),
        (step("expect = { command = \"a\", params = { x = { gt = \"big\" } } }"), "wrong kind"),
        (step("expect = { command = \"a\", params = 3 }"), "must be a table"),
        (step("target = \"\"\nexpect = { manual = true }"), "target"),
        (step("target = []\nexpect = { manual = true }"), "target needs"),
        (step("target = [\"a\", \"b c\"]\nexpect = { manual = true }"), "target is not"),
        (step("target = [1]\nexpect = { manual = true }"), ""),
        (step(&format!("target = [{}]\nexpect = {{ manual = true }}", ["\"a\""; coach::MAX_ALTERNATIVES + 1].join(","))), "target needs"),
        (step("expect = { command = [] }"), "`command` needs"),
        (step("expect = { command = [\"a\", \"\"] }"), "`command` is not"),
        ("id = \"Bad Id\"\ntitle = \"X\"\nprovenance = [\"fork\"]\n[[step]]\ntext = \"t\"\nexpect = { manual = true }\n".to_string(), "lesson id"),
        ("id = \"x\"\ntitle = \"X\"\nprovenance = [\"martian\"]\n[[step]]\ntext = \"t\"\nexpect = { manual = true }\n".to_string(), "unknown variant"),
        ("id = \"x\"\ntitle = \"X\"\nprovenance = []\n[[step]]\ntext = \"t\"\nexpect = { manual = true }\n".to_string(), "provenance"),
        (format!("{head}[[step]]\ntext = \"\"\nexpect = {{ manual = true }}\n"), "empty"),
        (format!("{head}[[step]]\ntext = \"a\\u0007b\"\nexpect = {{ manual = true }}\n"), "control"),
    ];
    for (src, want) in cases {
        let e = parse(&src).expect_err(&src);
        assert!(e.contains(want), "{src:?}: error {e:?} should mention {want:?}");
    }
    // limits: file size, step count, text length, pattern depth and size
    assert!(parse(&"#".repeat(coach::MAX_FILE_BYTES + 1)).unwrap_err().contains("larger"));
    let many: String = (0..=coach::MAX_STEPS).map(|_| "[[step]]\ntext = \"t\"\nexpect = { manual = true }\n").collect();
    assert!(parse(&format!("{head}{many}")).unwrap_err().contains("steps"));
    let long = "a".repeat(coach::MAX_TEXT + 1);
    assert!(parse(&format!("{head}[[step]]\ntext = \"{long}\"\nexpect = {{ manual = true }}\n")).unwrap_err().contains("longer"));
    let deep = format!("{}1{}", "{ a = ".repeat(coach::MAX_PATTERN_DEPTH + 2), " }".repeat(coach::MAX_PATTERN_DEPTH + 2));
    assert!(parse(&step(&format!("expect = {{ command = \"a\", params = {deep} }}"))).unwrap_err().contains("deeper"));
    let wide: Vec<String> = (0..coach::MAX_PATTERN_NODES + 1).map(|i| format!("k{i} = 1")).collect();
    assert!(parse(&step(&format!("expect = {{ command = \"a\", params = {{ {} }} }}", wide.join(", ")))).unwrap_err().contains("more than"));
}

#[test]
fn parameter_patterns_match_subsets_and_operators() {
    let p = json!({"clip": 3, "node": 7, "param": "saturation", "value": 1.4, "merge": "x"});
    assert!(matches(&json!({}), &p));
    assert!(matches(&json!({"param": "saturation"}), &p));
    assert!(!matches(&json!({"param": "contrast"}), &p));
    assert!(matches(&json!({"value": {"gt": 1.0}}), &p));
    assert!(!matches(&json!({"value": {"lt": 1.0}}), &p));
    assert!(matches(&json!({"value": {"ge": 1.4, "le": 1.4}}), &p));
    assert!(matches(&json!({"value": {"ne": 1.0}}), &p));
    assert!(!matches(&json!({"value": {"ne": 1.4}}), &p));
    assert!(matches(&json!({"merge": {"any": true}}), &p));
    assert!(!matches(&json!({"missing": {"any": true}}), &p));
    assert!(matches(&json!({"missing": {"any": false}}), &p));
    assert!(matches(&json!({"param": {"in": ["contrast", "saturation"]}}), &p));
    assert!(!matches(&json!({"param": {"in": ["contrast", "pivot"]}}), &p));
    // a number compares within rounding; a string is not a number
    assert!(matches(&json!({"value": 1.4000000000001}), &p));
    assert!(!matches(&json!({"param": {"gt": 0}}), &p));
    // wheels: lists match element by element, `nonzero` looks inside
    let w = json!({"param": "offset", "value": [0.0, 0.0, 0.02, 0.0]});
    assert!(matches(&json!({"value": {"nonzero": true}}), &w));
    assert!(!matches(&json!({"value": {"nonzero": true}}), &json!({"value": [0.0, 0.0, 0.0, 0.0]})));
    assert!(matches(&json!({"value": [0.0, 0.0]}), &w));
    assert!(!matches(&json!({"value": [0.0, 1.0]}), &w));
    assert!(!matches(&json!({"value": [0.0, 0.0, 0.02, 0.0, 9.0]}), &w));
    // a key named like an operator next to an ordinary key is an ordinary key
    assert!(matches(&json!({"gt": 1, "x": 2}), &json!({"gt": 1, "x": 2})));
}

#[test]
fn the_runner_walks_steps_and_finishes() {
    let mut c = Coach::default();
    c.start(small());
    assert_eq!(c.active.as_ref().map(|r| r.step), Some(0));
    // a command doesn't finish a manual step; Next does
    assert_eq!(c.observe_command("grade.setParam", &json!({"param": "saturation", "value": 2.0})), Advance::Stay);
    assert_eq!(c.press_next(), Ok(Advance::Next));
    // a command step ignores other commands and non-matching parameters, and refuses Next
    assert_eq!(c.observe_command("grade.addNode", &json!({})), Advance::Stay);
    assert_eq!(c.observe_command("grade.setParam", &json!({"param": "saturation", "value": 0.5})), Advance::Stay);
    assert!(c.press_next().is_err());
    assert_eq!(c.observe_command("grade.setParam", &json!({"param": "saturation", "value": 1.5})), Advance::Next);
    // Back returns to the previous step; Skip moves on and is counted
    assert_eq!(c.back(), Ok(()));
    assert_eq!(c.active.as_ref().map(|r| r.step), Some(1));
    assert_eq!(c.skip(), Ok(Advance::Next));
    assert_eq!(c.active.as_ref().map(|r| r.skipped.clone()), Some(vec![1]));
    // an element step waits for that element
    assert_eq!(c.observe_element("panel.Program"), Advance::Stay);
    assert_eq!(c.observe_element("panel.ColorNodes"), Advance::Finished);
    assert!(c.active.is_none());
    assert_eq!(c.finished.as_ref().map(|l| l.id.as_str()), Some("small"));
    // nothing running: the moves are refused, observing is harmless
    assert!(c.press_next().is_err() && c.skip().is_err() && c.back().is_err());
    assert_eq!(c.observe_command("grade.addNode", &json!({})), Advance::Stay);
    c.start(small());
    assert!(c.back().is_err(), "no step before the first");
    c.exit();
    assert!(c.active.is_none() && c.finished.is_none());
}

#[test]
fn every_bundled_lesson_parses_and_names_real_commands() {
    assert_eq!(coach::bundled().len(), coach::BUNDLED_FILES.len(), "a bundled lesson failed to parse");
    for (name, src) in coach::BUNDLED_FILES {
        let l = parse(src).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(format!("{}.toml", l.id), *name, "file name matches the lesson id");
        for (i, s) in l.steps.iter().enumerate() {
            if let Expect::Command { ids, .. } = &s.expect {
                for id in ids {
                    assert!(crate::find_command(id).is_some(), "{name} step {}: no command `{id}`", i + 1);
                }
            }
            assert!(
                s.expect == Expect::Manual || !s.targets.is_empty() || matches!(s.expect, Expect::Element(_)),
                "{name} step {}: highlight something",
                i + 1
            );
        }
    }
    let ids: Vec<&str> = coach::bundled().iter().map(|l| l.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "nodes-and-flow",
            "first-grade",
            "which-node",
            "match-shots",
            "fix-skin",
            "tracked-window",
            "log-noise",
            "first-comp",
            "key-comp",
            "parallel-nodes",
            "layer-look",
            "splitter-combiner",
            "outside-node",
            "key-mixer",
            "track-effects"
        ]
    );
}

#[test]
fn lessons_run_through_session_commands_and_are_remembered() {
    let mut s = Session::default();
    let dir = std::env::temp_dir().join(format!("filmcraft-coach-{}", std::process::id()));
    s.prefs_path = Some(dir.join("preferences.json"));
    let v = s.execute("coach.lessons", json!({})).unwrap();
    assert_eq!(v["lessons"].as_array().map(Vec::len), Some(coach::BUNDLED_FILES.len()));
    assert_eq!(v["lessons"][0]["completed"], json!(false));
    assert!(s.execute("coach.start", json!({"lesson": "nope"})).is_err());
    assert!(s.execute("coach.start", json!({})).is_err());
    assert!(s.execute("coach.next", json!({})).is_err(), "no lesson running");
    let st = s.execute("coach.start", json!({"lesson": "nodes-and-flow"})).unwrap();
    assert_eq!((st["active"].clone(), st["step"].clone(), st["expect"]["kind"].clone()), (json!(true), json!(0), json!("manual")));
    s.execute("coach.next", json!({})).unwrap();
    // the element step: reported by the UI (or an agent)
    assert!(s.execute("coach.seen", json!({"element": "bad id!"})).is_err());
    let st = s.execute("coach.seen", json!({"element": "panel.ColorNodes"})).unwrap();
    assert_eq!(st["step"], json!(2));
    // a real command, run through the session, is observed
    s.execute("file.openDemoProject", json!({})).unwrap();
    let seq = s.execute("sequence.inspect", json!({})).unwrap();
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().expect("a clip");
    s.execute("timeline.select", json!({"clips": [clip]})).unwrap();
    assert_eq!(s.execute("coach.state", json!({})).unwrap()["step"], json!(3));
    // a failed command is not
    assert!(s.execute("grade.setParam", json!({"clip": clip, "param": "saturation", "value": 2.0})).is_err());
    assert_eq!(s.coach.active.as_ref().map(|r| r.step), Some(3));
    s.execute("grade.addNode", json!({"clip": clip})).unwrap();
    s.execute("grade.setParam", json!({"clip": clip, "param": "saturation", "value": 1.3})).unwrap();
    // skip the rest
    let mut last = json!(null);
    for _ in 0..20 {
        last = s.execute("coach.skip", json!({})).unwrap();
        if last["active"] == json!(false) {
            break;
        }
    }
    assert_eq!(last["finished"]["lesson"], json!("nodes-and-flow"), "{last}");
    assert_eq!(s.prefs.color.coach.completed, vec!["nodes-and-flow".to_string()]);
    let saved = std::fs::read_to_string(dir.join("preferences.json")).unwrap();
    assert!(saved.contains("nodes-and-flow"), "{saved}");
    let v = s.execute("coach.lessons", json!({})).unwrap();
    assert_eq!(v["lessons"][0]["completed"], json!(true));
    // coach commands are not journaled (they aren't edits)
    assert!(!s.journal.iter().any(|(id, _)| id.starts_with("coach.")));
    // finishing again doesn't duplicate; Exit clears the finished card
    s.execute("coach.start", json!({"lesson": "nodes-and-flow"})).unwrap();
    for _ in 0..20 {
        if s.execute("coach.skip", json!({})).unwrap()["active"] == json!(false) {
            break;
        }
    }
    assert_eq!(s.prefs.color.coach.completed.len(), 1);
    assert_eq!(s.execute("coach.exit", json!({})).unwrap()["finished"], json!(null));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hints_default_on_and_the_completed_list_is_capped() {
    let mut s = Session::default();
    assert_eq!(s.execute("prefs.get", json!({"key": "color.coach.hints"})).unwrap(), json!(true));
    s.prefs.color.coach.completed = (0..coach::MAX_COMPLETED).map(|i| format!("old-{i}")).collect();
    s.execute("coach.start", json!({"lesson": "first-grade"})).unwrap();
    for _ in 0..40 {
        if s.execute("coach.skip", json!({})).unwrap()["active"] == json!(false) {
            break;
        }
    }
    let done = &s.prefs.color.coach.completed;
    assert_eq!(done.len(), coach::MAX_COMPLETED);
    assert_eq!(done.last().map(String::as_str), Some("first-grade"));
    assert_eq!(done.first().map(String::as_str), Some("old-1"));
}

#[test]
fn alternatives_and_selection_patterns() {
    let src = r#"
id = "alt"
title = "Alt"
provenance = ["fork"]

[[step]]
text = "Pick a clip."
target = "timeline.tracks"
expect = { command = "timeline.select", params = { clips = [{ any = true }] } }

[[step]]
text = "Add a node."
target = ["grade.addFirst", "grade.addNode"]
expect = { command = ["grade.addNode", "grade.insertNode"] }
"#;
    let l = parse(src).unwrap();
    assert_eq!(l.steps[1].targets, ["grade.addFirst", "grade.addNode"]);
    let mut c = Coach::default();
    c.start(std::sync::Arc::new(l));
    // a deselect (or selecting only a transition) is not "select a clip"
    assert_eq!(c.observe_command("timeline.select", &json!({"clips": []})), Advance::Stay);
    assert_eq!(c.observe_command("timeline.select", &json!({"transitions": [3]})), Advance::Stay);
    assert_eq!(c.observe_command("timeline.select", &json!({"clips": [7]})), Advance::Next);
    // any of the listed commands finishes the step
    assert_eq!(c.observe_command("grade.removeNode", &json!({})), Advance::Stay);
    assert_eq!(c.observe_command("grade.insertNode", &json!({"before": null})), Advance::Finished);
    let st = Coach { active: None, ..c }.to_json();
    assert_eq!(st["active"], json!(false));
}

#[test]
fn bad_coach_preferences_reset_only_themselves() {
    let dir = std::env::temp_dir().join(format!("filmcraft-coach-prefs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("preferences.json");
    let mut good = crate::autosave::Preferences::default();
    good.auto_save.interval_minutes = 7;
    good.color.hdr_graphics_white = "300".into();
    let mut v = good.to_value();
    v["color"]["coach"] = json!({"completed": 5, "hints": "yes", "assistant": "yes"});
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let p = crate::autosave::Preferences::load(&path);
    assert_eq!(p.auto_save.interval_minutes, 7, "other settings survive");
    assert_eq!(p.color.hdr_graphics_white, "300");
    assert_eq!(p.color.coach, coach::CoachPrefs::default());
    // an oversized, repetitive or partly bad list is trimmed to the newest valid ids
    let mut list: Vec<Value> = (0..coach::MAX_COMPLETED + 50).map(|i| json!(format!("l-{i}"))).collect();
    list.push(json!(3));
    list.push(json!("bad id!"));
    list.push(json!("l-0"));
    v["color"]["coach"] = json!({"completed": list, "hints": false});
    let p = crate::autosave::Preferences::from_value(v);
    let done = &p.color.coach.completed;
    assert_eq!(done.len(), coach::MAX_COMPLETED);
    assert!(!p.color.coach.hints);
    assert!(done.iter().all(|d| d != "bad id!"));
    assert_eq!(done.last().map(String::as_str), Some(format!("l-{}", coach::MAX_COMPLETED + 49).as_str()));
    let _ = std::fs::remove_dir_all(&dir);
}
