//! Grading Coach lessons end to end (fork GRADE-PLAN G2.12): each bundled lesson is started with
//! `coach.start` and walked step by step through the control channel, doing what the step asks
//! (clicks by automation id, or the command a control would run). Every step's highlighted control
//! must be on screen with the spotlight around it, the step must advance once done, and the lesson
//! must end remembered as completed.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        // the lessons are written for the Premiere workflow (the fork build defaults to Resolve)
        session.prefs.workflow = filmcraft_engine::settings::Workflow::Premiere;
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn element(&mut self, id: &str) -> Option<[f32; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array()?.iter().find(|e| e["id"] == id).map(|e| {
            let r = &e["rect"];
            [r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32]
        })
    }

    fn state(&mut self) -> Value {
        self.exec("coach.state", json!({}))
    }
}

/// What the test does for one step.
enum Do {
    /// Press Next on the card.
    Next,
    /// Switch to the Color Grading workspace (what the Workspaces menu does).
    Workspace,
    /// Switch to another workspace by name.
    WorkspaceNamed(&'static str),
    /// Select the n-th clip on V1.
    Select(usize),
    /// Click a control by automation id.
    Click(&'static str),
    /// Click a control at a fraction of its size (a colour wheel's rim).
    ClickAt(&'static str, f32, f32),
    /// Run a command; `{clip}`, `{node}` and `{other}` in string values are filled in (the
    /// selected clip, its current node, the first clip).
    Exec(&'static str, Value),
    /// Nothing: the step is already done when it comes up (in the Color Grading workspace the
    /// Curves panel holds the Qualifier and Windows tabs, so "Open the Primaries tab" finishes as
    /// soon as it is shown, the wheels never having left the screen).
    Done,
}

fn clips(d: &mut Driver) -> Vec<u64> {
    let seq = d.exec("sequence.inspect", json!({}));
    seq["video"][0]["items"].as_array().map(|a| a.iter().filter_map(|i| i["clip"].as_u64()).collect()).unwrap_or_default()
}

fn fill(v: &Value, clip: u64, node: u64, other: u64, still: u64) -> Value {
    fill_comp(v, clip, node, other, still, 0)
}

/// [`fill`], also filling `{comp}` (the comp the last command that returned one made).
fn fill_comp(v: &Value, clip: u64, node: u64, other: u64, still: u64, comp: u64) -> Value {
    match v {
        Value::String(s) if s == "{comp}" => json!(comp),
        Value::String(s) if s == "{clip}" => json!(clip),
        Value::String(s) if s == "{node}" => json!(node),
        Value::String(s) if s == "{other}" => json!(other),
        Value::String(s) if s == "{still}" => json!(still),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), fill_comp(x, clip, node, other, still, comp))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(|x| fill_comp(x, clip, node, other, still, comp)).collect()),
        x => x.clone(),
    }
}

/// Walk `lesson` doing `steps` (one per lesson step) and check it ends completed.
fn walk(lesson: &str, steps: &[Do]) {
    walk_on(lesson, steps, false);
}

/// [`walk`], with the first clip already graded when `graded` (a node with a saturation change).
fn walk_on(lesson: &str, steps: &[Do], graded: bool) {
    let mut d = Driver::demo();
    let all = clips(&mut d);
    assert!(all.len() >= 2, "the demo needs two clips on V1: {all:?}");
    if graded {
        d.exec("grade.addNode", json!({"clip": all[0]}));
        d.exec("grade.setParam", json!({"clip": all[0], "param": "saturation", "value": 1.2}));
    }
    let st = d.exec("coach.start", json!({"lesson": lesson}));
    assert_eq!(st["steps"].as_u64(), Some(steps.len() as u64), "{lesson}: one action per step: {st}");
    let mut clip = all[0];
    let mut comp = 0u64;
    for (i, act) in steps.iter().enumerate() {
        d.frames(2);
        let st = d.state();
        if matches!(act, Do::Done) {
            assert!(st["step"].as_u64().is_some_and(|s| s > i as u64), "{lesson}: step {i} should finish by itself: {st}");
            continue;
        }
        assert_eq!(st["step"], json!(i), "{lesson}: at step {i}: {st}");
        // the card is up; the highlighted control is on screen with the spotlight on it
        assert!(d.element("coach.card").is_some(), "{lesson} step {i}: no coach card");
        let targets: Vec<String> = st["targets"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if !targets.is_empty() {
            // the first target on screen is the one highlighted
            let shown = targets.iter().find_map(|t| d.element(t).filter(|r| r[2] > 0.0 && r[3] > 0.0).map(|r| (t.clone(), r)));
            let (target, r) = shown.unwrap_or_else(|| panic!("{lesson} step {i}: none of {targets:?} on screen ({})", st["text"]));
            let v = d.ok("ui.elements", json!({"prefix": "coach.spotlight"}));
            let spot = v.as_array().and_then(|a| a.first().cloned()).unwrap_or_else(|| panic!("{lesson} step {i}: no spotlight"));
            assert_eq!(spot["label"], json!(target), "{lesson} step {i}: spotlight on the first target on screen");
            let s: Vec<f32> = (0..4).map(|k| spot["rect"][k].as_f64().unwrap_or(0.0) as f32).collect();
            let overlap = s[0] < r[0] + r[2] && r[0] < s[0] + s[2] && s[1] < r[1] + r[3] && r[1] < s[1] + s[3];
            assert!(overlap, "{lesson} step {i}: spotlight {s:?} on {target} {r:?}");
        }
        let manual = st["expect"]["kind"] == json!("manual");
        assert_eq!(d.element("coach.next").is_some(), manual, "{lesson} step {i}: Next only on manual steps");
        match act {
            Do::Done => {}
            Do::Next => d.click("coach.next"),
            Do::Workspace => {
                d.ok("ui.set", json!({"workspace": "Color Grading"}));
                d.frames(3);
            }
            Do::WorkspaceNamed(name) => {
                d.ok("ui.set", json!({"workspace": name}));
                d.frames(3);
            }
            Do::Select(n) => {
                clip = all[*n];
                d.exec("timeline.select", json!({"clips": [clip]}));
            }
            Do::Click(id) => d.click(id),
            Do::ClickAt(id, fx, fy) => {
                d.ok("ui.click", json!({"id": id, "fx": fx, "fy": fy}));
                d.frames(2);
            }
            Do::Exec(cmd, params) => {
                let g = d.exec("grade.get", json!({"clip": clip}));
                let node = d.exec("state.inspect", json!({}))["grade_node"]["node"]
                    .as_u64()
                    .or_else(|| g["grade"]["nodes"].as_array().and_then(|n| n.last()).and_then(|n| n["id"].as_u64()))
                    .unwrap_or(0);
                let still = d.exec("stills.list", json!({}))["stills"][0]["id"].as_u64().unwrap_or(0);
                let p = fill_comp(params, clip, node, all[0], still, comp);
                let r = d.exec(cmd, p);
                if let Some(c) = r["comp"].as_u64() {
                    comp = c;
                }
            }
        }
        d.frames(3);
        let after = d.state();
        let moved = after["active"] == json!(false) || after["step"].as_u64().is_some_and(|s| s > i as u64);
        assert!(moved, "{lesson}: step {i} ({}) did not advance: {after}", st["text"]);
    }
    let st = d.state();
    assert_eq!((st["active"].clone(), st["finished"]["lesson"].clone()), (json!(false), json!(lesson)), "{st}");
    let list = d.exec("coach.lessons", json!({}));
    let me = list["lessons"].as_array().and_then(|a| a.iter().find(|l| l["id"] == json!(lesson)).cloned()).unwrap_or_default();
    assert_eq!(me["completed"], json!(true), "{list}");
    // the "lesson complete" card closes
    d.frames(2);
    assert!(d.element("coach.close").is_some());
    d.click("coach.close");
    d.frames(2);
    assert!(d.element("coach.card").is_none());
}

fn set(param: &str, value: Value) -> Do {
    Do::Exec("grade.setParam", json!({"clip": "{clip}", "node": "{node}", "param": param, "value": value}))
}

#[test]
fn coach_first_grade_end_to_end() {
    walk(
        "first-grade",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addFirst"),
            set("temperature", json!(12.0)),
            Do::ClickAt("grade.wheel.offset", 0.8, 0.5),
            set("contrast", json!(1.15)),
            set("saturation", json!(1.1)),
            Do::Click("grade.addNode"),
            Do::ClickAt("grade.wheel.gamma", 0.8, 0.5),
            Do::Click("grade.bypass"),
            Do::Next,
        ],
    );
}

/// A clip that already has a grade shows "+ Serial" instead of "Add Node": the step highlights
/// that and accepts it.
#[test]
fn coach_first_grade_on_a_graded_clip() {
    walk_on(
        "first-grade",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            set("temperature", json!(12.0)),
            Do::ClickAt("grade.wheel.offset", 0.8, 0.5),
            set("contrast", json!(1.15)),
            set("saturation", json!(1.1)),
            Do::Click("grade.addNode"),
            Do::ClickAt("grade.wheel.gamma", 0.8, 0.5),
            Do::Click("grade.bypass"),
            Do::Next,
        ],
        true,
    );
}

/// "Select a clip" isn't done by a deselect (clicking empty timeline space).
#[test]
fn coach_select_step_ignores_a_deselect() {
    let mut d = Driver::demo();
    let all = clips(&mut d);
    d.exec("coach.start", json!({"lesson": "first-grade"}));
    d.frames(2);
    d.click("coach.next");
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(4);
    assert_eq!(d.state()["step"], json!(2));
    d.exec("timeline.select", json!({"clips": []}));
    d.frames(2);
    assert_eq!(d.state()["step"], json!(2), "a deselect doesn't count");
    d.exec("timeline.select", json!({"clips": [all[0]]}));
    d.frames(2);
    assert_eq!(d.state()["step"], json!(3));
}

/// "Select a clip" with a clip already selected finishes by itself (clicking the selected clip
/// again sends no command, so the step could never finish).
#[test]
fn coach_select_step_with_a_clip_already_selected() {
    let mut d = Driver::demo();
    let all = clips(&mut d);
    d.exec("timeline.select", json!({"clips": [all[0]]}));
    d.exec("coach.start", json!({"lesson": "first-grade"}));
    d.frames(2);
    d.click("coach.next");
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(4);
    let st = d.state();
    assert_eq!(st["step"], json!(3), "{st}");
    assert!(d.element("coach.card").is_some());
}

#[test]
fn coach_log_noise_end_to_end() {
    walk(
        "log-noise",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            Do::Click("grade.tab.noise"),
            set("nr.luma", json!(0.3)),
            set("nr.chroma", json!(0.4)),
            Do::Next,
            Do::Click("grade.addNode"),
            Do::Click("grade.tab.primaries"),
            set("lift", json!([0.0, 0.0, 0.0, 0.05])),
            Do::Click("grade.toggleNode"),
            Do::Next,
        ],
    );
}

#[test]
fn coach_nodes_and_flow_end_to_end() {
    walk(
        "nodes-and-flow",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addFirst"),
            set("saturation", json!(1.3)),
            Do::Click("grade.addNode"),
            set("contrast", json!(1.2)),
            Do::Click("grade.toggleNode"),
            Do::Next,
        ],
    );
}

#[test]
fn coach_which_node_end_to_end() {
    walk(
        "which-node",
        &[
            Do::Next,
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            set("temperature", json!(-8.0)),
            Do::Click("grade.addNode"),
            set("contrast", json!(1.1)),
            Do::Click("grade.addNode"),
            set("saturation", json!(1.05)),
            Do::Click("grade.addNode"),
            set("grain.strength", json!(0.2)),
            Do::Next,
        ],
    );
}

#[test]
fn coach_fix_skin_end_to_end() {
    walk(
        "fix-skin",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            Do::Click("grade.tab.qualifier"),
            Do::Exec("grade.pickQualifier", json!({"clip": "{clip}", "node": "{node}", "x": 0.5, "y": 0.5})),
            Do::Click("grade.qual.show.1"),
            set("qual.soft", json!(0.12)),
            Do::Click("grade.qual.show.0"),
            Do::Done,
            set("saturation", json!(1.1)),
            Do::Click("grade.toggleNode"),
            Do::Next,
        ],
    );
}

#[test]
fn coach_tracked_window_end_to_end() {
    walk(
        "tracked-window",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            Do::Click("grade.tab.windows"),
            Do::Click("grade.window.add.ellipse"),
            Do::Next,
            Do::Exec("masks.set", json!({"clip": "{clip}", "node": "{node}", "mask": 0, "feather": 40.0})),
            Do::Exec("masks.track", json!({"clip": "{clip}", "node": "{node}", "mask": 0, "direction": "forward", "frames": 3, "wait": true})),
            Do::Done,
            set("gamma", json!([0.0, 0.0, 0.0, 0.05])),
            Do::Next,
        ],
    );
}

#[test]
fn coach_match_shots_end_to_end() {
    walk(
        "match-shots",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("stills.grab"),
            Do::Select(1),
            Do::Exec("compare.set", json!({"still": "{still}", "mode": "off"})),
            Do::Click("stills.mode.wipe"),
            Do::Click("grade.addNode"),
            Do::ClickAt("grade.wheel.offset", 0.8, 0.5),
            Do::Click("grade.tab.match"),
            Do::Exec("grade.shotMatch", json!({"clip": "{clip}", "with": "{other}"})),
            Do::Click("stills.mode.off"),
            Do::Next,
        ],
    );
}

/// C1 item 7: the comp lesson, built through the commands the canvas sends (node ids are fixed:
/// Media Out is 1, then 2, 3, … in add order).
#[test]
fn coach_first_comp_end_to_end() {
    let node = |kind: &str| Do::Exec("comp.addNode", json!({"comp": "{comp}", "kind": kind}));
    let connect = |from: u64, to: u64, port: u64| Do::Exec("comp.connect", json!({"comp": "{comp}", "from": from, "to": to, "toPort": port}));
    walk(
        "first-comp",
        &[
            Do::Next,
            Do::WorkspaceNamed("Compositing"),
            Do::Exec("comp.newClip", json!({"track": "V2"})),
            node("text"),
            node("transform"),
            connect(2, 3, 0),
            node("merge"),
            connect(3, 4, 1),
            connect(4, 1, 0),
            Do::Exec("comp.setParam", json!({"comp": "{comp}", "node": 3, "param": "scale", "value": 60.0})),
            Do::Next,
        ],
    );
}

/// C1 item 7: the keying comp lesson (Media In 2, Key 3, Solid 4, Merge 5, Color 6; Media Out 1).
#[test]
fn coach_key_comp_end_to_end() {
    let connect = |from: u64, to: u64, port: u64| Do::Exec("comp.connect", json!({"comp": "{comp}", "from": from, "to": to, "toPort": port}));
    walk(
        "key-comp",
        &[
            Do::Next,
            Do::WorkspaceNamed("Compositing"),
            Do::Exec("comp.newClip", json!({"track": "V2"})),
            Do::Exec("comp.addNode", json!({"comp": "{comp}", "kind": "mediaIn", "track": 0})),
            Do::Exec("comp.addNode", json!({"comp": "{comp}", "kind": "key", "effect": "luma_key"})),
            connect(2, 3, 0),
            Do::Exec("comp.addNode", json!({"comp": "{comp}", "kind": "solid"})),
            Do::Exec("comp.addNode", json!({"comp": "{comp}", "kind": "merge"})),
            connect(4, 5, 0),
            connect(3, 5, 1),
            Do::Exec("comp.addNode", json!({"comp": "{comp}", "kind": "color"})),
            connect(5, 6, 0),
            connect(6, 1, 0),
            Do::Exec("comp.setParam", json!({"comp": "{comp}", "node": 3, "param": "threshold", "value": 30.0})),
            Do::Next,
        ],
    );
}

/// C2 lane 5: the branch lessons, built through the commands the canvas sends. Node ids on the
/// demo clip: the first node is 1, each add takes the next ids (see `grade_graph.rs`).
fn on_node(n: u64, param: &str, value: Value) -> Do {
    Do::Exec("grade.setParam", json!({"clip": "{clip}", "node": n, "param": param, "value": value}))
}

fn explain() -> Do {
    Do::Exec("grade.explainGrade", json!({"clip": "{clip}"}))
}

#[test]
fn coach_parallel_nodes_end_to_end() {
    walk(
        "parallel-nodes",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            set("temperature", json!(6.0)),
            // node 2 beside node 1, mixer 3
            Do::Exec("grade.addParallel", json!({"clip": "{clip}", "node": "{node}"})),
            set("saturation", json!(1.2)),
            Do::Exec("grade.selectNode", json!({"clip": "{clip}", "node": 1})),
            set("contrast", json!(1.1)),
            on_node(3, "mix.gain.1", json!(0.5)),
            explain(),
            Do::Click("grade.toggleNode"),
            Do::Next,
        ],
    );
}

#[test]
fn coach_layer_look_end_to_end() {
    walk(
        "layer-look",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            set("temperature", json!(-6.0)),
            Do::Exec("grade.addLayer", json!({"clip": "{clip}", "node": "{node}"})),
            set("saturation", json!(1.3)),
            on_node(3, "layer.opacity.1", json!(0.6)),
            on_node(3, "layer.mode.1", json!(13.0)),
            explain(),
            Do::Click("grade.toggleNode"),
            Do::Next,
        ],
    );
}

#[test]
fn coach_splitter_combiner_end_to_end() {
    walk(
        "splitter-combiner",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            // splitter 2, Red 3, Green 4, Blue 5, combiner 6
            Do::Exec("grade.addSplitter", json!({"clip": "{clip}", "node": "{node}"})),
            Do::Exec("grade.selectNode", json!({"clip": "{clip}", "node": 5})),
            set("gain", json!([0.0, 0.0, 0.0, 0.1])),
            explain(),
            Do::Click("grade.toggleNode"),
            Do::Next,
        ],
    );
}

#[test]
fn coach_outside_node_end_to_end() {
    walk(
        "outside-node",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            Do::Click("grade.tab.qualifier"),
            Do::Exec("grade.pickQualifier", json!({"clip": "{clip}", "node": "{node}", "x": 0.5, "y": 0.5})),
            Do::Exec("grade.addOutside", json!({"clip": "{clip}", "node": "{node}"})),
            Do::Done,
            set("saturation", json!(0.8)),
            explain(),
            Do::Click("grade.toggleNode"),
            Do::Next,
        ],
    );
}

#[test]
fn coach_key_mixer_end_to_end() {
    let connect = |from: u64, from_port: u64, to: u64, to_port: u64| {
        Do::Exec("grade.connect", json!({"clip": "{clip}", "from": from, "fromPort": from_port, "to": to, "toPort": to_port}))
    };
    let pick = || Do::Exec("grade.pickQualifier", json!({"clip": "{clip}", "node": "{node}", "x": 0.5, "y": 0.5}));
    walk(
        "key-mixer",
        &[
            Do::Next,
            Do::Workspace,
            Do::Select(0),
            Do::Click("grade.addNode"),
            Do::Click("grade.tab.qualifier"),
            pick(),
            Do::Click("grade.addNode"),
            pick(),
            Do::Click("grade.addNode"),
            // the key mixer is node 4, fed by node 1's key
            Do::Exec("grade.addKeyMixer", json!({"clip": "{clip}", "node": 1})),
            connect(2, 1, 4, 1),
            connect(4, 0, 3, 1),
            Do::Exec("grade.selectNode", json!({"clip": "{clip}", "node": 3})),
            set("saturation", json!(1.3)),
            explain(),
            Do::Next,
        ],
    );
}

#[test]
fn coach_back_skip_and_exit_from_the_card() {
    let mut d = Driver::demo();
    d.exec("coach.start", json!({"lesson": "first-grade"}));
    d.frames(2);
    assert!(d.element("coach.card").is_some());
    // Back is disabled on the first step; Next moves on, Back returns
    d.click("coach.next");
    assert_eq!(d.state()["step"], json!(1));
    d.click("coach.back");
    assert_eq!(d.state()["step"], json!(0));
    d.click("coach.skip");
    d.click("coach.skip");
    let st = d.state();
    assert_eq!(st["step"], json!(2));
    assert_eq!(st["skipped"], json!(2), "{st}");
    d.click("coach.exit");
    let st = d.state();
    assert_eq!(st["active"], json!(false));
    d.frames(2);
    assert!(d.element("coach.card").is_none());
    assert!(d.element("coach.spotlight").is_none());
}

// ---------- Ask the coach (G2.12 part 2) ----------

/// The ids the guide names as `{ui:…}`, with the page.
fn guide_ui_ids() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guide");
    let mut out = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
                continue;
            }
            if p.extension().is_none_or(|x| x != "md") {
                continue;
            }
            let page = p.strip_prefix(&root).unwrap().to_string_lossy().to_string();
            let src = std::fs::read_to_string(&p).unwrap();
            for chunk in src.split("{ui:").skip(1) {
                // (prose about the syntax, such as `{ui:…}`, isn't an id)
                if let Some(id) =
                    chunk.split('}').next().map(str::trim).filter(|id| id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) && !id.is_empty())
                {
                    out.push((page.clone(), id.to_string()));
                }
            }
        }
    }
    out
}

/// Every automation id on screen now, added to `seen`.
fn snap(d: &mut Driver, seen: &mut BTreeSet<String>) {
    d.frames(2);
    let v = d.ok("ui.elements", json!({"prefix": ""}));
    seen.extend(v.as_array().into_iter().flatten().filter_map(|e| e["id"].as_str().map(str::to_string)));
}

/// Run a command (engine or UI) and ignore a refusal.
fn try_exec(d: &mut Driver, command: &str, params: Value) {
    d.call("engine.execute", json!({"command": command, "params": params}));
    d.frames(2);
}

/// The demo project in the Color Grading workspace, walked through the places the guide talks
/// about (each grade tab, a window, a still and its wipe, the Ask box, a lesson card to its end,
/// the colour dialogs, Settings pages, Lumetri Color, the Export page with H.265 / CRF and its Preset
/// Manager), with every automation id seen on the way.
fn walked() -> (Driver, BTreeSet<String>) {
    let mut d = Driver::demo();
    let mut seen = BTreeSet::new();
    let all = clips(&mut d);
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    snap(&mut d, &mut seen);
    d.exec("timeline.select", json!({"clips": [all[0]]}));
    snap(&mut d, &mut seen);
    d.click("grade.addFirst");
    // the first node's context hint (its card, Got it, its lesson link, Turn hints off); then
    // hints off so no card covers the rest of the walk
    snap(&mut d, &mut seen);
    assert!(d.element("coach.hint.card").is_some(), "the first node shows a hint");
    d.click("coach.hint.off");
    d.click("grade.addNode");
    snap(&mut d, &mut seen);
    for tab in ["primaries", "curves", "qualifier", "windows", "noise", "match"] {
        d.click(&format!("grade.tab.{tab}"));
        snap(&mut d, &mut seen);
        if tab == "windows" {
            d.click("grade.window.add.ellipse");
            snap(&mut d, &mut seen);
        }
        if tab == "curves" {
            for c in ["r", "g", "b", "master"] {
                d.click(&format!("grade.curve.{c}"));
                snap(&mut d, &mut seen);
            }
        }
        if tab == "primaries" {
            // a node LUT shows its mix slider
            let g = d.exec("grade.get", json!({"clip": all[0]}));
            let node = g["grade"]["nodes"].as_array().and_then(|n| n.last()).and_then(|n| n["id"].as_u64()).expect("a node");
            let lut = filmcraft_engine::render::luts::builtins().first().map(|b| format!("builtin:{}", b.id)).expect("a built-in LUT");
            d.exec("grade.setLut", json!({"clip": all[0], "node": node, "lut": lut}));
            // the lower part of the panel (grain, soft clip, LUT) scrolls into view
            for _ in 0..6 {
                d.call("ui.scroll", json!({"id": "grade.param.saturation", "dy": -400.0}));
                snap(&mut d, &mut seen);
            }
        }
    }
    // the mixer and key-input controls (fork C2b) on another clip's branched grade: a parallel
    // mixer, a layer mixer, a key mixer and an outside node's key input
    let other = all.get(1).copied().unwrap_or(all[0]);
    d.exec("timeline.select", json!({"clips": [other]}));
    d.exec("grade.addNode", json!({"clip": other}));
    let first = d.exec("grade.get", json!({"clip": other}))["grade"]["nodes"][0]["id"].as_u64().expect("a node");
    let r = d.exec("grade.addParallel", json!({"clip": other, "node": first}));
    let (y, pm) = (r["nodes"][0].as_u64().expect("node"), r["nodes"][1].as_u64().expect("mixer"));
    d.exec("grade.selectNode", json!({"clip": other, "node": pm}));
    snap(&mut d, &mut seen);
    let r = d.exec("grade.addLayer", json!({"clip": other, "node": y}));
    d.exec("grade.selectNode", json!({"clip": other, "node": r["nodes"][1].as_u64().expect("mixer")}));
    snap(&mut d, &mut seen);
    let r = d.exec("grade.addKeyMixer", json!({"clip": other, "node": first}));
    d.exec("grade.selectNode", json!({"clip": other, "node": r["nodes"][0].as_u64().expect("key mixer")}));
    snap(&mut d, &mut seen);
    let r = d.exec("grade.addOutside", json!({"clip": other, "node": first}));
    d.exec("grade.selectNode", json!({"clip": other, "node": r["nodes"][0].as_u64().expect("outside node")}));
    d.click("grade.tab.qualifier");
    snap(&mut d, &mut seen);
    d.exec("timeline.select", json!({"clips": [all[0]]}));
    d.frames(2);
    d.click("grade.tab.primaries");
    // the Ask the coach box, with an answer that offers a lesson
    d.click("coach.ask.open");
    d.harness.state_mut().ui.coach_ask.input = "how do I undo a node".into();
    d.frames(1);
    d.click("coach.ask.go");
    snap(&mut d, &mut seen);
    assert!(d.element("coach.ask.lesson").is_some());
    d.click("coach.ask.close");
    // the lesson list
    d.click("coach.lessons.open");
    snap(&mut d, &mut seen);
    d.click("coach.lessons.close");
    // the Assistant panel (opened floating from the header, then closed)
    d.click("assistant.open");
    snap(&mut d, &mut seen);
    d.click("assistant.open");
    d.click("stills.grab");
    snap(&mut d, &mut seen);
    let still = d.exec("stills.list", json!({}))["stills"][0]["id"].as_u64().expect("a still");
    d.click(&format!("stills.item.{still}"));
    snap(&mut d, &mut seen);
    d.click("stills.mode.split");
    snap(&mut d, &mut seen);
    d.click("stills.mode.off");
    snap(&mut d, &mut seen);
    // a lesson card, skipped through to "lesson complete" (the first step is a manual one: Next)
    d.exec("coach.start", json!({"lesson": "nodes-and-flow"}));
    snap(&mut d, &mut seen);
    for _ in 0..40 {
        if d.state()["active"] != json!(true) {
            break;
        }
        d.click("coach.skip");
        snap(&mut d, &mut seen);
    }
    d.click("coach.close");
    // the colour dialogs
    try_exec(&mut d, "clip.interpretFootage", json!({}));
    snap(&mut d, &mut seen);
    if d.element("colorDialog.cancel").is_some() {
        d.click("colorDialog.cancel");
    }
    try_exec(&mut d, "sequence.colorSettings", json!({}));
    snap(&mut d, &mut seen);
    if d.element("colorDialog.cancel").is_some() {
        d.click("colorDialog.cancel");
    }
    // Settings pages the guide names
    for page in ["playback", "memory", "media", "color", "general", "mediaCache", "audio"] {
        try_exec(&mut d, &format!("app.settings.{page}"), json!({}));
        snap(&mut d, &mut seen);
    }
    if d.element("settings.cancel").is_some() {
        d.click("settings.cancel");
    }
    // Essential Sound on a Dialogue clip in the Audio workspace (audio.md: the manual voice clean-up
    // and Clean Up Voice), then its Loudness section opened
    d.ok("ui.set", json!({"workspace": "Audio"}));
    let a1 = d.exec("sequence.inspect", json!({}))["audio"][0]["items"][0]["clip"].as_u64().expect("an audio clip");
    d.exec("timeline.select", json!({"clips": [a1]}));
    snap(&mut d, &mut seen);
    d.exec("essentialSound.setType", json!({"type": "dialogue"}));
    snap(&mut d, &mut seen);
    d.click("essentialSound.section.Loudness");
    snap(&mut d, &mut seen);
    // Lumetri Color in the Premiere-style Color workspace: Basic Correction's WB Selector and Auto
    // balance, then the Color Wheels & Match section (Comparison View, Apply Match)
    d.ok("ui.set", json!({"workspace": "Color"}));
    d.exec("timeline.select", json!({"clips": [all[0]]}));
    d.exec("effects.apply", json!({"clips": [all[0]], "effect": "lumetri"}));
    snap(&mut d, &mut seen);
    assert!(d.element("lumetri.wb.pick").is_some() && d.element("lumetri.wb.auto").is_some(), "the WB Selector row");
    d.click("lumetri.section.Color Wheels & Match");
    snap(&mut d, &mut seen);
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    snap(&mut d, &mut seen);
    // the Export page: its settings, H.265 with constant quality (encoder, profile, CRF) and the
    // Preset Manager
    d.ok("ui.set", json!({"mode": "export"}));
    snap(&mut d, &mut seen);
    {
        use filmcraft_engine::export::{BitrateMode, ExportSettings, Format};
        d.harness.state_mut().ui.export.settings = ExportSettings { format: Format::Hevc, bitrate_mode: BitrateMode::Crf, ..Default::default() };
    }
    snap(&mut d, &mut seen);
    d.click("export.preset.more");
    snap(&mut d, &mut seen);
    d.click("presetManager.cancel");
    d.harness.state_mut().ui.export.settings = Default::default();
    d.ok("ui.set", json!({"mode": "edit"}));
    snap(&mut d, &mut seen);
    // the Compositor panel (fork C1) in its empty state: New Comp Clip
    d.exec("timeline.select", json!({"clips": []}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Compositor"}));
    snap(&mut d, &mut seen);
    // the Project Manager home screen (fork RS2) and its New Project dialog
    try_exec(&mut d, "projects.show", json!({}));
    snap(&mut d, &mut seen);
    d.click("projects.new");
    snap(&mut d, &mut seen);
    d.click("projects.dialog.cancel");
    try_exec(&mut d, "projects.close", json!({}));
    snap(&mut d, &mut seen);
    // the page bar of the Resolve workflow, and each page's layout and toolbar (fork RS3; Cut: RS4)
    d.exec("settings.setWorkflow", json!({"workflow": "resolve"}));
    snap(&mut d, &mut seen);
    for page in ["media", "cut", "color", "fairlight", "deliver", "edit"] {
        d.click(&format!("page.{page}"));
        snap(&mut d, &mut seen);
    }
    d.exec("settings.setWorkflow", json!({"workflow": "premiere"}));
    snap(&mut d, &mut seen);
    (d, seen)
}

/// Guide ids the walk doesn't reach, each with the reason. Each must still be known to the app's
/// check (a dynamic id) or a literal automation id in the UI source.
const NOT_WALKED: &[(&str, &str)] = &[
    ("inspector.tab.video", "the Resolve workflow's Inspector with a clip selected (inspector_ui covers its tabs and sections)"),
    ("inspector.tab.audio", "the Resolve workflow's Inspector with a clip selected (inspector_ui covers its tabs and sections)"),
    ("inspector.tab.file", "the Resolve workflow's Inspector with a clip selected (inspector_ui covers its tabs and sections)"),
    ("inspector.section.transform", "the Resolve workflow's Inspector with a clip selected (inspector_ui covers its tabs and sections)"),
    ("coach.ask.reference", "only under an answer about a reference picture (coach_reference_tests cover its content)"),
    ("coach.ask.doThis", "only under an answer about a reference picture (coach_reference_tests cover its content)"),
    ("update.notes", "only when an update check found a newer release (updates_ui covers it)"),
    ("update.install", "only when an update check found a newer release (updates_ui covers it)"),
    ("update.later", "only when an update check found a newer release (updates_ui covers it)"),
    ("update.optOut", "only when an update check found a newer release (updates_ui covers it)"),
    ("update.restart", "only when an update check found a newer release (updates_ui covers it)"),
    ("update.releasePage", "only when an update check found a newer release (updates_ui covers it)"),
    ("update.close", "only when an update check found a newer release (updates_ui covers it)"),
    ("settings.update.check", "only when an update check found a newer release (updates_ui covers it)"),
    ("settings.update.checkNow", "only when an update check found a newer release (updates_ui covers it)"),
    ("settings.update.rollback", "only when an update check found a newer release (updates_ui covers it)"),
    ("settings.color.ai.vision.cancel", "Settings ▸ Color ▸ Picture model: only while its download runs (grade_ui's AI settings test covers the row)"),
    (
        "export.video.hevcMain10Note",
        "Export page: only for an 8-bit HEVC export of an HDR sequence when this PC's H.265 encoder can write HDR (a hardware probe)",
    ),
    (
        "coach.ask.assistStop",
        "Ask the coach box: only while the assistant model is writing (coach_ask_box_shows_the_assistant_answer_or_why_not covers the box)",
    ),
    (
        "coach.ask.assistDontKnow",
        "Ask the coach box: only when the assistant model says the guide doesn't cover the question (coach_ask_box_shows_the_assistant_answer_or_why_not covers it)",
    ),
    (
        "coach.ask.assistLesson",
        "Ask the coach box: only when the assistant's lesson differs from the guide answer's (coach_ask_box_shows_the_assistant_answer_or_why_not clicks it)",
    ),
    ("coach.ask.newTopic", "Ask the coach box: only after the assistant model has answered (coach_ask_box_shows_the_assistant_answer_or_why_not clicks it)"),
    (
        "coach.ask.rate.up",
        "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)",
    ),
    (
        "coach.ask.rate.down",
        "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)",
    ),
    (
        "coach.ask.rate.correction",
        "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)",
    ),
    (
        "coach.ask.rate.share",
        "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)",
    ),
    (
        "share.preview",
        "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)",
    ),
    ("share.grade", "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)"),
    (
        "share.picture",
        "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)",
    ),
    ("share.frame", "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)"),
    (
        "share.consent",
        "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)",
    ),
    ("share.github", "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)"),
    ("share.save", "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)"),
    ("share.close", "only under a finished assistant answer / in its Help improve the Assistant dialog (assistant_answers_can_be_rated_and_shared covers it)"),
    ("project.card.copy", "Project panel card bar: only while the desktop has a camera card mounted (L2.4)"),
    ("project.card.eject", "Project panel card bar: only after a card's copy is verified (L2.4)"),
    ("project.card.dismiss", "Project panel card bar: only while the desktop has a camera card mounted (L2.4)"),
];

fn ui_source() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut all = String::new();
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                all.push_str(&std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    all
}

/// Every `{ui:…}` id in the guide is one the UI shows (seen on the walk), one it builds at run
/// time (a dynamic pattern), or a literal id of a dialog the walk doesn't open.
#[test]
fn coach_ask_every_guide_ui_id_is_known() {
    let (d, seen) = walked();
    let app = d.harness.state();
    let src = ui_source();
    // the list stays short: an id the walk now shows comes off it
    let walked_now: Vec<&str> = NOT_WALKED.iter().map(|(id, _)| *id).filter(|id| seen.contains(*id)).collect();
    assert!(walked_now.is_empty(), "the walk shows these now: drop them from NOT_WALKED: {walked_now:?}");
    let mut unseen = Vec::new();
    for (page, id) in guide_ui_ids() {
        if seen.contains(&id) {
            assert!(app.known_ui_id(&id), "{id} was shown, so the app knows it");
            continue;
        }
        match NOT_WALKED.iter().find(|(x, _)| *x == id) {
            Some(_) => assert!(app.known_ui_id(&id) || src.contains(&format!("\"{id}\"")), "{id}: not a control of this UI"),
            None => unseen.push(format!("{page}: {id}")),
        }
    }
    unseen.sort();
    unseen.dedup();
    assert!(unseen.is_empty(), "the guide names controls the walk never showed (open them in `walked` or list them in NOT_WALKED with a reason): {unseen:#?}");
    assert!(!app.known_ui_id("grade.flux.capacitor"));
    assert!(!app.known_ui_id("grade.param.saturaton"), "a misspelt parameter is unknown");
}

/// The questions of `assets/coach/questions.toml` (its `q = "…"` lines).
fn questions() -> Vec<String> {
    include_str!("../../../../assets/coach/questions.toml")
        .lines()
        .filter_map(|l| l.trim().strip_prefix("q = \"")?.strip_suffix('"').map(str::to_string))
        .collect()
}

/// The 30 beginner questions, asked in the app: every control a remaining step names is known to
/// this UI (the engine removed the others), and the commands are registered.
#[test]
fn coach_ask_thirty_questions_name_only_known_controls() {
    let (mut d, _) = walked();
    let qs = questions();
    assert_eq!(qs.len(), 30);
    let mut removed = 0;
    let mut removed_ids: Vec<String> = Vec::new();
    let mut steps = 0;
    for q in &qs {
        let a = d.exec("coach.ask", json!({"question": q}));
        assert_eq!(a["uiChecked"], json!(true), "the app installs its id check");
        removed += a["removed"].as_u64().unwrap_or(0);
        for id in a["removedIds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            removed_ids.push(format!("{q:?}: {id}"));
        }
        for st in a["steps"].as_array().unwrap() {
            steps += 1;
            let ids: Vec<&str> = st["ui"].as_str().into_iter().chain(st["uis"].as_array().into_iter().flatten().filter_map(Value::as_str)).collect();
            for id in ids {
                assert!(d.harness.state().known_ui_id(id), "{q:?}: unknown control {id}");
            }
            if let Some(c) = st["command"].as_str() {
                assert!(d.harness.state().session.shortcuts.is_known_command(c), "{q:?}: unknown command {c}");
            }
        }
    }
    eprintln!("ask the coach in the app: {steps} steps kept, {removed} removed: {removed_ids:#?}");
}

/// The box: open it, type a question, ask; click a step to outline its control; start the
/// suggested lesson.
#[test]
fn coach_ask_box_answers_and_spotlights_a_step() {
    let mut d = Driver::demo();
    let all = clips(&mut d);
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    d.exec("timeline.select", json!({"clips": [all[0]]}));
    d.click("grade.addFirst");
    d.frames(2);
    assert!(d.element("coach.ask.open").is_some(), "the Color Grading workspace shows the button");
    d.click("coach.ask.open");
    assert!(d.element("coach.ask.input").is_some());
    d.click("coach.ask.input");
    d.ok("ui.type", json!({"text": "how do I make the sky bluer without changing skin?"}));
    d.frames(2);
    d.click("coach.ask.go");
    d.frames(2);
    assert!(d.element("coach.ask.step.1").is_some(), "steps are listed");
    // the first step that names a control: clicking it outlines that control
    let answer = d.harness.state().ui.coach_ask.answer.clone().expect("an answer");
    let steps = answer["steps"].as_array().cloned().unwrap_or_default();
    let i = steps.iter().position(|st| st["ui"].is_string()).expect("a step naming a control");
    let want = steps[i]["ui"].clone();
    let n = i + 1;
    d.click(&format!("coach.ask.step.{n}"));
    d.frames(3);
    let v = d.ok("ui.elements", json!({"prefix": "coach.spotlight"}));
    assert_eq!(v[0]["label"], want, "{v}");
    // the same step again: the outline goes
    d.click(&format!("coach.ask.step.{n}"));
    d.frames(2);
    assert!(d.element("coach.spotlight").is_none());
    // a question without steps offers its lesson; the link starts it
    d.harness.state_mut().ui.coach_ask.input.clear();
    d.click("coach.ask.input");
    d.ok("ui.type", json!({"text": "how do I undo a node"}));
    d.frames(2);
    d.click("coach.ask.go");
    d.frames(2);
    assert!(d.element("coach.ask.step.1").is_none());
    assert!(d.element("coach.ask.lesson").is_some());
    d.click("coach.ask.lesson");
    d.frames(2);
    assert_eq!(d.state()["active"], json!(true));
    // the box sits above the lesson card, and closes
    let card = d.element("coach.card").unwrap();
    let ask = d.element("coach.ask").unwrap();
    assert!(ask[1] + ask[3] <= card[1] + 1.0, "ask box {ask:?} above the card {card:?}");
    d.click("coach.ask.close");
    assert!(d.element("coach.ask.input").is_none());
}

/// A model backend for the assistant tests: ready or not, one fixed reply.
struct ScriptedAssistant {
    ready: bool,
    reply: &'static str,
}

impl filmcraft_engine::ai_explain::ExplainBackend for ScriptedAssistant {
    fn ready(&self, _: &filmcraft_engine::ai_explain::ModelInfo) -> Result<(), (filmcraft_engine::ai_explain::Fallback, String)> {
        if self.ready { Ok(()) } else { Err((filmcraft_engine::ai_explain::Fallback::NotInstalled, "not installed".into())) }
    }
    fn generate(&self, _: &filmcraft_engine::ai_explain::ModelInfo, _: &str, _: &str, _: filmcraft_engine::ai_explain::GenOptions) -> Result<String, String> {
        Ok(self.reply.to_string())
    }
}

/// Ask the box with the assistant switched on, and wait until its answer is in.
fn ask_assistant(d: &mut Driver, question: &str) {
    d.harness.state_mut().ui.coach_ask.input = question.into();
    d.frames(1);
    d.click("coach.ask.go");
    for _ in 0..200 {
        d.frames(1);
        if d.harness.state().ui.coach_ask.assist_id.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // the box is anchored at its bottom: a longer answer (a completed recipe, AI3.6 lane E) moves
    // its top controls until the layout settles; wait for two frames with the same layout
    let mut last = None;
    let mut settled = false;
    for i in 0..20 {
        d.frames(1);
        let now = d.element("coach.ask.assistLine.1");
        if i > 0 && now == last {
            settled = true;
            break;
        }
        last = now;
    }
    assert!(settled, "the ask box's layout never settled");
}

/// Fork AI3.5c: "Also ask the assistant" adds the model's checked lines and their sources below
/// the guide's answer; without a model the box says why and keeps the guide's answer; unticking
/// stops and clears it.
#[test]
fn coach_ask_box_shows_the_assistant_answer_or_why_not() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    d.click("coach.ask.open");
    assert!(!d.harness.state().session.prefs.color.coach.assistant, "off by default (slow on the CPU)");
    // off: only the keyword coach answers
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: true, reply: "x" }));
    d.harness.state_mut().ui.coach_ask.input = "why use several nodes?".into();
    d.frames(1);
    d.click("coach.ask.go");
    d.frames(2);
    assert!(d.harness.state().ui.coach_ask.assist_id.is_none() && d.harness.state().ui.coach_ask.assist.is_none());
    // on, with no model: the guide's answer stays and the assistant says why
    d.click("coach.ask.assistant");
    d.frames(1);
    assert!(d.harness.state().session.prefs.color.coach.assistant, "a preference");
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: false, reply: "" }));
    ask_assistant(&mut d, "why use several nodes?");
    let a = d.harness.state().ui.coach_ask.assist.clone().expect("an assistant result");
    assert_eq!(a["state"], "done", "{a}");
    assert_eq!(a["source"], "coach", "{a}");
    assert!(d.element("coach.ask.assistLine.1").is_none());
    assert!(d.harness.state().ui.coach_ask.answer.is_some(), "the guide's answer stays");
    // with a model: its checked lines are listed
    // (the box asks in the pick style: the model names guide steps by key, Q35)
    let reply = "A1 B1";
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: true, reply }));
    ask_assistant(&mut d, "why use several nodes?");
    let a = d.harness.state().ui.coach_ask.assist.clone().expect("an assistant result");
    assert_eq!(a["source"], "model", "{a}");
    assert!(d.element("coach.ask.assistLine.1").is_some(), "{a}");
    assert!(d.element("coach.ask.assistStop").is_none(), "no Stop once it's done");
    assert!(d.element("coach.ask.assistDontKnow").is_none(), "{a}");
    // the answer is remembered for a follow-up; "New topic" forgets it
    let (q, a) = d.harness.state().ui.coach_ask.last_exchange.clone().expect("remembered");
    assert_eq!(q, "why use several nodes?");
    assert!(!a.trim().is_empty(), "{a}");
    d.click("coach.ask.newTopic");
    assert!(d.harness.state().ui.coach_ask.last_exchange.is_none());
    // "Show me": the assistant's answer offers its lesson unless the guide's answer above already does
    let reply = "A1 A2";
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: true, reply }));
    ask_assistant(&mut d, "how do I fix skin with a qualifier?");
    let a = d.harness.state().ui.coach_ask.assist.clone().expect("an assistant result");
    assert_eq!(a["lesson"]["id"], "fix-skin", "{a}");
    assert!(d.element("coach.ask.lesson").is_some() && d.element("coach.ask.assistLesson").is_none(), "offered once, above");
    if let Some(g) = d.harness.state_mut().ui.coach_ask.answer.as_mut() {
        g["lesson"] = serde_json::Value::Null;
    }
    d.frames(2);
    d.click("coach.ask.assistLesson");
    assert!(d.harness.state().session.coach.active.is_some(), "the lesson started");
    d.exec("coach.exit", json!({}));
    // the box lays out again once the lesson ends: a click at the old place would miss New topic,
    // and the skin question would then lead the next one (the clear-winner rule gives its recipe)
    d.frames(3);
    d.click("coach.ask.newTopic");
    assert!(d.harness.state().ui.coach_ask.last_exchange.is_none(), "New topic forgot the skin question");
    // the model says the guide doesn't cover it: its lines stay, with a pointer to the guide's answer
    let reply = "NONE";
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: true, reply }));
    ask_assistant(&mut d, "why use several nodes?");
    let a = d.harness.state().ui.coach_ask.assist.clone().expect("an assistant result");
    assert_eq!((a["source"].as_str(), a["dontKnow"].as_bool()), (Some("model"), Some(true)), "{a}");
    assert!(d.element("coach.ask.assistDontKnow").is_some(), "{a}");
    d.click("coach.ask.newTopic");
    // unticking clears it
    d.click("coach.ask.assistant");
    d.frames(2);
    assert!(d.harness.state().ui.coach_ask.assist.is_none());
    assert!(d.element("coach.ask.assistLine.1").is_none());
    // closing the box stops a question still being answered
    d.click("coach.ask.assistant");
    d.harness.state_mut().ui.coach_ask.input = "why use several nodes?".into();
    d.frames(1);
    d.click("coach.ask.go");
    assert!(d.harness.state().ui.coach_ask.assist_id.is_some() || d.harness.state().ui.coach_ask.assist.is_some(), "asked");
    // (the box grows upwards as the answer comes in: let it settle before clicking ×)
    d.frames(3);
    d.click("coach.ask.close");
    assert!(d.element("coach.ask.input").is_none(), "closed");
    assert!(d.harness.state().ui.coach_ask.assist_id.is_none() && d.harness.state().ui.coach_ask.assist.is_none());
}

// ---------- context hints and the lesson list (G2.12 part 3) ----------

fn hint_id(d: &mut Driver) -> Value {
    d.exec("coach.hint", json!({}))["hint"]["id"].clone()
}

/// A hint's card appears the first time (a command, then a control on screen), Got it dismisses
/// it, its link starts the lesson, none shows during a lesson, and Turn hints off switches them
/// off in the preferences.
#[test]
fn coach_hint_card_shows_dismisses_links_and_turns_off() {
    let mut d = Driver::demo();
    let all = clips(&mut d);
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    assert!(d.element("coach.hint.card").is_none());
    d.exec("timeline.select", json!({"clips": [all[0]]}));
    d.click("grade.addFirst");
    d.frames(2);
    assert_eq!(hint_id(&mut d), json!("first-node"));
    assert!(d.element("coach.hint.card").is_some());
    assert!(d.element("coach.hint.lesson").is_some(), "the hint links its lesson");
    // the Ask / Lessons buttons are in the header (FORK-PLAN §1a.78), never over the card
    let card = d.element("coach.hint.card").unwrap();
    let ask = d.element("coach.ask.open").unwrap();
    assert!(ask[1] + ask[3] <= 40.0 && ask[1] + ask[3] <= card[1], "buttons {ask:?} in the header, above the hint {card:?}");
    d.click("coach.hint.dismiss");
    assert!(d.element("coach.hint.card").is_none());
    assert_eq!(hint_id(&mut d), json!(null));
    // opening the qualifier tab: the hint waiting for its Pick control
    d.click("grade.tab.qualifier");
    d.frames(3);
    assert_eq!(hint_id(&mut d), json!("first-qualifier"));
    assert!(d.element("coach.hint.card").is_some());
    d.click("coach.hint.lesson");
    d.frames(2);
    let st = d.state();
    assert_eq!((st["active"].clone(), st["lesson"].clone()), (json!(true), json!("fix-skin")), "{st}");
    assert!(d.element("coach.hint.card").is_none(), "no hint during a lesson");
    // nothing new during the lesson (the windows tab would show one)
    d.click("grade.tab.windows");
    d.frames(3);
    assert_eq!(hint_id(&mut d), json!(null));
    d.click("coach.exit");
    d.frames(3);
    assert_eq!(hint_id(&mut d), json!("first-window"), "after the lesson it shows");
    d.click("coach.hint.off");
    assert!(d.element("coach.hint.card").is_none());
    assert_eq!(d.exec("prefs.get", json!({"key": "color.coach.hints"})), json!(false));
    d.click("stills.grab");
    d.frames(2);
    assert_eq!(hint_id(&mut d), json!(null), "hints are off");
}

/// The lesson list: from the Lessons button and Help ▸ Grading Lessons…; every lesson listed
/// with a Start button; a finished one marked; Start runs it and closes the list.
#[test]
fn coach_lessons_list_lists_and_starts_lessons() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    d.click("coach.lessons.open");
    let lessons = d.exec("coach.lessons", json!({}));
    let ids: Vec<String> = lessons["lessons"].as_array().unwrap().iter().map(|l| l["id"].as_str().unwrap().to_string()).collect();
    assert!(ids.len() >= 7);
    assert!(d.element("coach.lessons").is_some());
    // (the list scrolls: the first ones are on screen)
    assert!(d.element(&format!("coach.lessons.item.{}", ids[0])).is_some());
    d.click("coach.lessons.close");
    assert!(d.element("coach.lessons").is_none());
    assert!(d.element("coach.lessons.open").is_some());
    // from the Help menu, in any workspace
    d.ok("ui.set", json!({"workspace": "Editing"}));
    d.frames(2);
    assert!(d.element("coach.lessons.open").is_some(), "the buttons are in the header in every workspace");
    d.exec("coach.showLessons", json!({}));
    d.frames(2);
    assert!(d.element("coach.lessons").is_some());
    d.click("coach.lessons.item.first-grade");
    d.frames(2);
    let st = d.state();
    assert_eq!((st["active"].clone(), st["lesson"].clone()), (json!(true), json!("first-grade")), "{st}");
    assert!(d.element("coach.lessons").is_none(), "starting closes the list");
    d.click("coach.exit");
    // a finished lesson: listed with "Again"
    d.harness.state_mut().session.prefs.color.coach.completed = vec!["nodes-and-flow".into()];
    d.exec("coach.showLessons", json!({}));
    d.frames(2);
    let v = d.ok("ui.elements", json!({"prefix": "coach.lessons.item.nodes-and-flow"}));
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
}

/// Wait until the Assistant panel's answer is in.
fn wait_for_chat(d: &mut Driver) {
    for _ in 0..200 {
        d.frames(1);
        if d.harness.state().ui.assistant.running.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    d.frames(2);
}

/// Fork §1a.78: the Assistant button opens the chat as a floating panel; questions are answered
/// with checked lines, or the guide's answer and why when the model can't run; the panel docks
/// between other panels like any panel; New chat clears it and the button closes it.
#[test]
fn assistant_panel_floats_answers_docks_and_closes() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    assert!(d.element("panel.Assistant").is_none());
    let shown_before = d.harness.state().ui.dock.clone();
    d.click("assistant.open");
    d.frames(2);
    assert!(d.harness.state().ui.floating.iter().any(|g| g.panels.contains(&filmcraft_ui_egui::dock::PanelKind::Assistant)), "opens floating");
    assert_eq!(d.harness.state().ui.dock, shown_before, "the docked panels and their shown tabs are unchanged");
    assert!(d.element("panel.Assistant").is_some() && d.element("assistant.input").is_some());
    // no model: the guide's answer and why
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: false, reply: "" }));
    d.harness.state_mut().ui.assistant.input = "why use several nodes?".into();
    d.frames(1);
    d.click("assistant.send");
    wait_for_chat(&mut d);
    let turns = d.harness.state().ui.assistant.turns.clone();
    assert_eq!(turns.len(), 1);
    let a = turns[0].result.clone().expect("a result");
    assert_eq!((a["state"].clone(), a["source"].clone()), (json!("done"), json!("coach")), "{a}");
    assert!(d.element("assistant.turn.1").is_some());
    // with a model: its checked lines (pick style: guide step keys)
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: true, reply: "A1 B1" }));
    d.harness.state_mut().ui.assistant.input = "why use several nodes?".into();
    d.frames(1);
    d.click("assistant.send");
    wait_for_chat(&mut d);
    let a = d.harness.state().ui.assistant.turns.last().and_then(|t| t.result.clone()).expect("a result");
    assert_eq!(a["source"], "model", "{a}");
    assert!(d.element("assistant.line.2.1").is_some(), "{a}");
    assert!(d.element("assistant.stop").is_none(), "Send again once it's done");
    // docked between two panels, like any panel (here: left of Curves)
    let ctx = d.harness.ctx.clone();
    filmcraft_ui_egui::menus::invoke(d.harness.state_mut(), &ctx, "window.dockPanel", json!({"panel": "Assistant", "anchor": "ColorCurves", "zone": "left"}))
        .expect("docks");
    d.frames(3);
    assert!(d.harness.state().ui.dock.contains(filmcraft_ui_egui::dock::PanelKind::Assistant), "docked");
    let (chat, curves) = (d.element("panel.Assistant").expect("on screen"), d.element("panel.ColorCurves").expect("on screen"));
    assert!(chat[0] + chat[2] <= curves[0] + 1.0, "{chat:?} left of {curves:?}");
    assert_eq!(d.harness.state().ui.assistant.turns.len(), 2, "the chat survives the move");
    // New chat, then the button closes the panel
    // everything fits a narrow docked panel
    let chat = d.element("panel.Assistant").unwrap();
    for id in ["assistant.new", "assistant.input", "assistant.send"] {
        let r = d.element(id).unwrap_or_else(|| panic!("{id} on screen"));
        assert!(r[0] >= chat[0] - 1.0 && r[0] + r[2] <= chat[0] + chat[2] + 1.0, "{id} {r:?} inside {chat:?}");
    }
    d.click("assistant.new");
    assert!(d.harness.state().ui.assistant.turns.is_empty());
    d.click("assistant.open");
    d.frames(2);
    assert!(d.element("panel.Assistant").is_none());
}

/// Without a model the Assistant's fallback answer links to Settings ▸ Color, where the model is
/// downloaded; a model answer has no such link.
#[test]
fn assistant_without_a_model_links_to_its_settings() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    d.click("assistant.open");
    d.frames(2);
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: true, reply: "A1 B1" }));
    d.harness.state_mut().ui.assistant.input = "why use several nodes?".into();
    d.frames(1);
    d.click("assistant.send");
    wait_for_chat(&mut d);
    assert!(d.element("assistant.setup.1").is_none(), "no link under a model answer");
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: false, reply: "" }));
    d.harness.state_mut().ui.assistant.input = "why use several nodes?".into();
    d.frames(1);
    d.click("assistant.send");
    wait_for_chat(&mut d);
    d.click("assistant.setup.2");
    d.frames(2);
    assert_eq!(d.harness.state().ui.settings.as_ref().map(|s| s.page.clone()).as_deref(), Some("color"));
    assert!(d.element("settings.color.ai.download").is_some(), "the model row is on screen");
}

/// AI5: under an answer, 👎 is stored with it; Help improve the Assistant… shows the preview, Open
/// on GitHub waits for the consent tick and then opens the fork's New-issue page, and Save
/// example… writes the previewed text to the chosen folder without consent.
#[test]
fn assistant_answers_can_be_rated_and_shared() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    d.click("assistant.open");
    d.frames(2);
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: true, reply: "A1 B1" }));
    d.harness.state_mut().ui.assistant.input = "why use several nodes?".into();
    d.frames(1);
    d.click("assistant.send");
    wait_for_chat(&mut d);
    let id = d.harness.state().ui.assistant.turns.last().and_then(|t| t.result.clone()).and_then(|r| r["id"].as_u64()).expect("the answer keeps its id");
    d.click("assistant.rate.1.down");
    assert_eq!(d.harness.state().session.assist.records.get(id).and_then(|r| r.rating), Some(filmcraft_engine::coach_share::Rating::Down));
    assert!(d.element("assistant.rate.1.correction").is_some(), "the correction box shows once rated");
    let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = opened.clone();
    d.harness.state_mut().hooks.open_url = Some(Box::new(move |u: &str| {
        seen.lock().unwrap().push(u.to_string());
        Ok(())
    }));
    let dir = std::env::temp_dir().join(format!("filmcraft-share-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let folder = dir.display().to_string();
    d.harness.state_mut().hooks.pick_folder = Some(Box::new(move || Some(folder.clone())));
    d.click("assistant.rate.1.share");
    d.frames(2);
    let preview = d.harness.state().share.dialog.as_ref().map(|x| x.preview.clone()).expect("dialog open");
    assert!(preview.contains("## Question") && preview.contains("why use several nodes?") && preview.contains("👎"), "{preview}");
    for el in ["share.preview", "share.grade", "share.picture", "share.frame", "share.consent", "share.github", "share.save", "share.close"] {
        assert!(d.element(el).is_some(), "{el} on screen");
    }
    // no consent: nothing opens
    d.click("share.github");
    assert!(opened.lock().unwrap().is_empty(), "opened without consent");
    // saving needs no consent and writes exactly the preview
    d.click("share.save");
    d.frames(3);
    let saved = std::fs::read_to_string(dir.join(format!("assistant-example-{id}.md"))).expect("saved");
    assert_eq!(saved, preview);
    d.click("share.consent");
    d.click("share.github");
    let urls = opened.lock().unwrap().clone();
    assert!(urls.len() == 1 && urls[0].starts_with("https://github.com/dawgon2/revolution/issues/new?labels=assistant-example"), "{urls:?}");
    d.click("share.close");
    assert!(d.harness.state().share.dialog.is_none());
    let _ = std::fs::remove_dir_all(&dir);
    // the Ask box's assistant answer has the same row
    d.click("coach.ask.open");
    if !d.harness.state().session.prefs.color.coach.assistant {
        d.click("coach.ask.assistant");
    }
    ask_assistant(&mut d, "why use several nodes?");
    let ask_id = d.harness.state().ui.coach_ask.assist.as_ref().and_then(|a| a["id"].as_u64()).expect("the answer keeps its id");
    d.click("coach.ask.rate.up");
    assert_eq!(d.harness.state().session.assist.records.get(ask_id).and_then(|r| r.rating), Some(filmcraft_engine::coach_share::Rating::Up));
    for el in ["coach.ask.rate.down", "coach.ask.rate.correction"] {
        assert!(d.element(el).is_some(), "{el} on screen");
    }
    d.click("coach.ask.rate.share");
    assert_eq!(d.harness.state().share.dialog.as_ref().map(|x| x.id), Some(ask_id));
}

/// AI4.1d: the Ask box sends the selection and the frame's measured facts (`context: "auto"`) and
/// shows the facts that fit the question under "On this frame (measured)".
#[test]
fn coach_ask_box_shows_the_measured_facts_of_the_frame() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = {
        let s = &d.harness.state().session;
        s.active_sequence().and_then(|q| q.video_tracks.first()).and_then(|t| t.item_at(s.playhead())).map(|i| i.id).expect("a clip")
    };
    d.harness.state_mut().session.state.selection = vec![clip];
    d.click("coach.ask.open");
    d.click("coach.ask.assistant");
    d.harness.state_mut().session.assist.backend = Some(std::sync::Arc::new(ScriptedAssistant { ready: true, reply: "x" }));
    ask_assistant(&mut d, "why does my shot look so dark?");
    let a = d.harness.state().ui.coach_ask.assist.clone().expect("an assistant result");
    assert!(a["seen"][0].as_str().is_some_and(|l| l.starts_with("Exposure: mids about")), "{a}");
    assert!(d.element("coach.ask.assistSeen").is_some(), "{a}");
    // a question that isn't about the picture shows none
    d.click("coach.ask.newTopic");
    ask_assistant(&mut d, "how do I add a node?");
    assert!(d.element("coach.ask.assistSeen").is_none());
}
