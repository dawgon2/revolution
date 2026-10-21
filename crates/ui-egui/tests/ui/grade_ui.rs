//! Headless UI tests of the Color Grading workspace (fork GRADE-PLAN G1): the Nodes strip and the
//! Primaries panel, driven by automation id through the control channel.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write `grade-*.png`.

#![allow(dead_code)]

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        Self::from_session(session)
    }

    fn from_session(session: Session) -> Self {
        Self::from_session_sized(session, egui::vec2(1600.0, 980.0))
    }

    fn from_session_sized(session: Session, size: egui::Vec2) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(size).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
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

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    /// Offscreen render of the window (or the bounding box of the elements under `prefix`).
    fn snapshot(&mut self, name: &str, prefix: Option<&str>) {
        let Some(dir) = self.snapshots.clone() else { return };
        let crop = prefix.map(|p| {
            let v = self.ok("ui.elements", json!({"prefix": p}));
            let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for e in v.as_array().unwrap() {
                let r = &e["rect"];
                let (x, y, w, h) = (r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32);
                bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x + w), bb[3].max(y + h)];
            }
            bb
        });
        self.frames(2);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        let ppp = img.width() as f32 / 1600.0;
        let img = match crop {
            Some(bb) => {
                let x0 = ((bb[0] - 24.0) * ppp).max(0.0) as u32;
                let y0 = ((bb[1] - 30.0) * ppp).max(0.0) as u32;
                let x1 = (((bb[2] + 16.0) * ppp) as u32).min(img.width());
                let y1 = (((bb[3] + 46.0) * ppp) as u32).min(img.height());
                image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image()
            }
            None => img,
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        img.save(&path).unwrap();
        eprintln!("snapshot: {}", path.display());
    }
}

/// Select the first clip on V1 and park the playhead on it.
fn select_first_clip(d: &mut Driver) -> u64 {
    let seq = d.exec("sequence.inspect", json!({}));
    let first = seq["video"][0]["items"][0].clone();
    let clip = first["clip"].as_u64().unwrap_or_else(|| panic!("no clip: {seq}"));
    d.exec("timeline.select", json!({"clips": [clip]}));
    clip
}

fn grade(d: &mut Driver, clip: u64) -> Value {
    d.exec("grade.get", json!({"clip": clip}))["grade"].clone()
}

#[test]
fn node_strip_and_primaries_edit_the_grade() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.frames(3);
    // no grade yet: the Primaries panel offers Add Node
    d.click("grade.addFirst");
    let g = grade(&mut d, clip);
    assert_eq!(g["nodes"].as_array().map(Vec::len), Some(1), "{g}");
    // two more serial nodes from the strip
    d.click("grade.addNode");
    d.click("grade.addNode");
    d.frames(2);
    let g = grade(&mut d, clip);
    let ids: Vec<u64> = g["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_u64().unwrap()).collect();
    assert_eq!(ids.len(), 3, "{g}");
    let strip = d.ids("grade.node.");
    for id in &ids {
        assert!(strip.iter().any(|s| s == &format!("grade.node.{id}")), "{strip:?}");
    }
    // select the first node and check the wheels and sliders are there
    d.click(&format!("grade.node.{}", ids[0]));
    d.frames(2);
    let ids_now = d.ids("grade.");
    for id in [
        "grade.wheel.lift",
        "grade.wheel.gamma",
        "grade.wheel.gain",
        "grade.wheel.offset",
        "grade.param.contrast",
        "grade.param.saturation",
        "grade.param.grain.strength",
    ] {
        assert!(ids_now.iter().any(|i| i == id), "{id} missing from {ids_now:?}");
    }
    // clicking the Offset wheel's rim moves its puck: a colour offset on the selected node
    assert!(d.element("grade.wheel.offset").is_some());
    d.ok("ui.click", json!({"id": "grade.wheel.offset", "fx": 0.8, "fy": 0.5}));
    d.frames(2);
    let g = grade(&mut d, clip);
    let first = &g["nodes"][0];
    assert!(first["params"]["offset"].is_object(), "offset set on node 1: {g}");
    d.snapshot("grade-primaries", None);
    // curves tab
    d.click("grade.tab.curves");
    d.frames(2);
    assert!(d.ids("grade.curve.").iter().any(|i| i == "grade.curve.r"));
    d.snapshot("grade-curves", None);
    // disable and delete from the strip
    d.click("grade.toggleNode");
    let g = grade(&mut d, clip);
    assert_eq!(g["nodes"][0]["enabled"], json!(false), "{g}");
    d.click("grade.removeNode");
    let g = grade(&mut d, clip);
    assert_eq!(g["nodes"].as_array().map(Vec::len), Some(2), "{g}");
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(2);
    d.snapshot("grade-workspace", None);
}

#[test]
fn clicks_are_separate_undo_steps_and_full_strips_scroll() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.frames(2);
    d.click("grade.addFirst");
    d.ok("ui.click", json!({"id": "grade.wheel.offset", "fx": 0.8, "fy": 0.5}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "grade.wheel.offset", "fx": 0.5, "fy": 0.2}));
    d.frames(2);
    let g = grade(&mut d, clip);
    let second = g["nodes"][0]["params"]["offset"].clone();
    d.exec("edit.undo", json!({}));
    let g = grade(&mut d, clip);
    assert!(g["nodes"][0]["params"]["offset"].is_object(), "one undo removes only the second click: {g}");
    assert_ne!(g["nodes"][0]["params"]["offset"], second);
    // a full grade (64 nodes, long labels) draws without panicking and keeps ids on screen only
    for _ in 0..63 {
        d.exec("grade.addNode", json!({"clip": clip}));
    }
    let first = grade(&mut d, clip)["nodes"][0]["id"].as_u64().unwrap();
    d.exec("grade.labelNode", json!({"clip": clip, "node": first, "label": "x".repeat(64)}));
    d.frames(3);
    let strip = d.ids("grade.node.");
    assert!(!strip.is_empty() && strip.len() < 64, "only visible nodes register: {}", strip.len());
    d.snapshot("grade-full-strip", None);
}

#[test]
fn rename_names_a_node_in_place_and_escape_cancels() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.frames(2);
    d.click("grade.addFirst");
    d.click("grade.addNode");
    let first = grade(&mut d, clip)["nodes"][0]["id"].as_u64().unwrap();
    d.click(&format!("grade.node.{first}"));
    // Rename opens a field on the node; Enter keeps the name (one undo step)
    d.click("grade.renameNode");
    assert!(d.element("grade.renameNode.field").is_some(), "rename field shown");
    d.ok("ui.type", json!({"text": "Balance"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(2);
    assert!(d.element("grade.renameNode.field").is_none(), "field closed");
    assert_eq!(grade(&mut d, clip)["nodes"][0]["label"], json!("Balance"));
    d.snapshot("grade-node-renamed", Some("grade.node."));
    // Esc leaves the name as it was
    d.click("grade.renameNode");
    d.ok("ui.type", json!({"text": " more"}));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(d.element("grade.renameNode.field").is_none());
    assert_eq!(grade(&mut d, clip)["nodes"][0]["label"], json!("Balance"));
    d.exec("edit.undo", json!({}));
    assert_eq!(grade(&mut d, clip)["nodes"][0]["label"].as_str().unwrap_or(""), "");
}

#[test]
fn color_page_clips_strip_and_split_screen() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(4);
    // Resolve's Color page: the clips strip spans the width with no meters or tools beside it
    let ids = d.ids("");
    assert!(ids.iter().any(|i| i == "clips.strip" || i.starts_with("clips.card.")), "clips strip shown");
    assert!(!ids.iter().any(|i| i.starts_with("audioMeters.")), "no audio meters on the Color page");
    let seq = d.exec("sequence.inspect", json!({}));
    let items = seq["video"][0]["items"].as_array().cloned().unwrap_or_default();
    assert!(items.len() >= 2, "demo has two clips: {seq}");
    let (a, b) = (items[0]["clip"].as_u64().unwrap(), items[1]["clip"].as_u64().unwrap());
    // a card selects its clip and moves the playhead into it
    d.click(&format!("clips.card.{b}"));
    let st = d.exec("sequence.inspect", json!({}));
    assert!(st["selection"].as_array().is_some_and(|v| v.contains(&json!(b)) && !v.contains(&json!(a))), "clip {b} selected: {}", st["selection"]);
    let (start, end) = (items[1]["start"].as_i64().unwrap(), items[1]["end"].as_i64().unwrap());
    let ph = st["playhead"].as_i64().unwrap();
    assert!(start <= ph && ph < end, "playhead {ph} in clip {b} ({start}..{end})");
    // the mini timeline seeks
    let ph0 = d.exec("sequence.inspect", json!({}))["playhead"].as_i64().unwrap_or(-1);
    d.ok("ui.click", json!({"id": "clips.mini", "fx": 0.02, "fy": 0.5}));
    d.frames(2);
    let ph1 = d.exec("sequence.inspect", json!({}))["playhead"].as_i64().unwrap_or(-1);
    assert_ne!(ph0, ph1, "mini timeline click moved the playhead");
    d.click("grade.addFirst");
    d.frames(3);
    d.snapshot("color-page", None);
    // split screen: adjacent clips tile the Program monitor; a tile click selects that clip
    d.ok("ui.menu.invoke", json!({"id": "view.splitScreen.adjacent"}));
    d.frames(3);
    let tiles = d.ids("program.split.");
    assert!(tiles.iter().any(|t| t == &format!("program.split.{a}")) && tiles.iter().any(|t| t == &format!("program.split.{b}")), "{tiles:?}");
    d.snapshot("color-page-split", None);
    d.click(&format!("program.split.{a}"));
    let st = d.exec("sequence.inspect", json!({}));
    assert!(st["selection"].as_array().is_some_and(|v| v.contains(&json!(a)) && !v.contains(&json!(b))), "tile selects clip {a}: {}", st["selection"]);
    // Ctrl+Alt+W toggles it off
    d.ok("ui.menu.invoke", json!({"id": "view.splitScreen.toggle"}));
    d.frames(2);
    assert!(d.ids("program.split.").is_empty(), "split screen off");
}

#[test]
fn resolve_workflow_shows_pages_and_color_opens_the_node_grader() {
    let mut d = Driver::demo();
    // start in Premiere whatever the build's default workflow is (the fork's is Resolve)
    d.exec("settings.setWorkflow", json!({"workflow": "premiere"}));
    d.frames(2);
    assert!(d.ids("page.").is_empty(), "no page bar in the Premiere workflow");
    d.exec("settings.setWorkflow", json!({"workflow": "resolve"}));
    d.frames(3);
    let pages = d.ids("page.");
    assert_eq!(pages.len(), 7, "{pages:?}");
    d.click("page.color");
    d.frames(2);
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["workspace"], json!("Color Grading"), "Color page");
    d.click("page.edit");
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["workspace"], json!("Edit Page"), "Edit page (fork RS3)");
    // Window ▸ Workspaces ▸ Color opens the node grader in the Resolve workflow
    d.ok("ui.set", json!({"workspace": "Color"}));
    d.frames(2);
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["workspace"], json!("Color Grading"));
    d.click("page.edit");
    d.snapshot("grade-resolve-pages", None);
    // Resolve's Shift+6 (Color page) and Shift+2 (Media) land on the same pages as the bar
    d.ok("ui.key", json!({"key": "Shift+6"}));
    d.frames(2);
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["workspace"], json!("Color Grading"));
    d.ok("ui.key", json!({"key": "Shift+2"}));
    d.frames(2);
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["workspace"], json!("Media Page"));
    assert!(d.call("engine.execute", json!({"command": "page.bogus", "params": {}})).get("error").is_some());
    d.click("page.deliver");
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["workspace"], json!("Deliver Page"), "Deliver page (fork RS3)");
    d.click("page.edit");
    d.exec("settings.setWorkflow", json!({"workflow": "premiere"}));
    d.frames(3);
    assert!(d.ids("page.").is_empty(), "back to Premiere: no page bar");
}

/// A local-AI stand-in (G2.11): answers with `f(prompt)`.
struct FakeAi(fn(&str) -> Result<String, String>);

impl filmcraft_engine::ai_explain::ExplainBackend for FakeAi {
    fn ready(&self, _: &filmcraft_engine::ai_explain::ModelInfo) -> Result<(), (filmcraft_engine::ai_explain::Fallback, String)> {
        Ok(())
    }
    fn generate(
        &self,
        _: &filmcraft_engine::ai_explain::ModelInfo,
        _: &str,
        user: &str,
        _: filmcraft_engine::ai_explain::GenOptions,
    ) -> Result<String, String> {
        (self.0)(user)
    }
}

/// The template body from the prompt: a text that passes the fact check.
fn echo_template(user: &str) -> Result<String, String> {
    Ok(user.split("Reference sentence:\n").nth(1).and_then(|r| r.split("\n\n").next()).unwrap_or_default().to_string())
}

fn wait_for(d: &mut Driver, id: &str) -> bool {
    for _ in 0..300 {
        if d.ids(id).iter().any(|i| i == id) {
            return true;
        }
        d.frames(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

#[test]
fn ai_explanations_setting_and_checked_badge() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    session.ai.set_backend(std::sync::Arc::new(FakeAi(echo_template)));
    let mut d = Driver::from_session(session);
    // Settings ▸ Color ▸ AI explanations: off by default; the switch applies on OK
    d.ok("ui.menu.invoke", json!({"id": "app.settings.color"}));
    d.frames(3);
    for id in [
        "settings.color.ai.enabled",
        "settings.color.ai.model",
        "settings.color.ai.download",
        "settings.color.ai.remove",
        "settings.color.ai.vision.download",
        "settings.color.ai.vision.remove",
    ] {
        assert!(d.element(id).is_some(), "{id} missing: {:?}", d.ids("settings.color"));
    }
    assert_eq!(d.exec("prefs.get", json!({"key": "color.ai.enabled"})), json!(false));
    d.click("settings.color.ai.enabled");
    d.click("settings.ok");
    assert_eq!(d.exec("prefs.get", json!({"key": "color.ai.enabled"})), json!(true));
    // the node explainer shows the checked AI text with its badge
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    let node = d.exec("grade.addNode", json!({"clip": clip}))["node"].as_u64().unwrap();
    d.exec("grade.setParam", json!({"clip": clip, "node": node, "param": "gain", "value": [0.0, 0.0, 0.0, 0.3]}));
    d.exec("grade.selectNode", json!({"clip": clip, "node": node}));
    d.frames(2);
    d.click("grade.tab.match");
    d.click("grade.match.explainNode");
    assert!(wait_for(&mut d, "grade.match.aiChecked"), "badge: {:?}", d.ids("grade.match"));
    assert!(!d.ids("grade.match").iter().any(|i| i == "grade.match.aiFallback"));
    d.snapshot("grade-ai-checked", Some("grade.match"));
    // a text that fails the check: the template, with the reason behind a hint
    d.harness.state_mut().session.ai.set_backend(std::sync::Arc::new(FakeAi(|_| Ok("It makes the picture much darker.".into()))));
    d.click("grade.match.explainNode");
    assert!(wait_for(&mut d, "grade.match.aiFallback"), "fallback hint: {:?}", d.ids("grade.match"));
    assert!(!d.ids("grade.match").iter().any(|i| i == "grade.match.aiChecked"));
}

/// A fine-tuned explainer built on this computer: selectable like any model, its Download stays
/// off and the note says where it comes from instead of a download source.
#[test]
fn ai_settings_show_local_tuned_models_without_download() {
    use egui_kittest::kittest::{NodeT, Queryable};
    let mut d = Driver::demo();
    let models = d.exec("ai.models", json!({}));
    let local = models["models"].as_array().unwrap().iter().find(|m| m["local"] == true).expect("a local entry")["id"].as_str().unwrap().to_string();
    d.ok("ui.menu.invoke", json!({"id": "app.settings.color"}));
    d.frames(3);
    d.exec("ai.select", json!({"model": local}));
    d.frames(3);
    assert_eq!(d.exec("ai.models", json!({}))["selected"], json!(local));
    // (the first Download is the assistant model's; the Picture model row has its own)
    let download = d.harness.get_all_by_label("Download").next().expect("the assistant model's Download");
    assert!(download.accesskit_node().is_disabled(), "no download for a local model");
    assert!(d.harness.query_by_label_contains("Built on this computer (Revolution fine-tune of").is_some());
    // only the Picture model row (always a download) names a download source
    assert!(d.harness.query_all_by_label_contains("downloaded from https://").count() <= 1);
}

/// GRADE-PLAN G2c: Primaries ▸ WB picker arms a crosshair; a click in the Program picture
/// balances the node's Temperature / Tint and disarms; Auto balance does it in one click.
#[test]
fn wb_picker_and_auto_balance_set_temperature_and_tint() {
    // the Premiere chrome leaves the Primaries panel tall enough at 980 pt for its WB row (in the
    // Resolve workflow the page toolbar and page bar take 40 pt more, and the row sits under
    // the page bar; fork RS3)
    wb_picker_case(filmcraft_engine::settings::Workflow::Premiere, egui::vec2(1600.0, 980.0));
}

/// Fork RS3: the same on the Resolve workflow's Color page (the fork's default), at 1920×1080.
#[test]
fn wb_picker_and_auto_balance_on_the_resolve_color_page() {
    wb_picker_case(filmcraft_engine::settings::Workflow::Resolve, egui::vec2(1920.0, 1080.0));
}

fn wb_picker_case(workflow: filmcraft_engine::settings::Workflow, size: egui::Vec2) {
    let mut session = Session::default();
    session.prefs.workflow = workflow;
    let item = session.execute("file.newColorMatte", json!({"color": "#a08c78", "width": 160, "height": 90, "seconds": 2})).unwrap()["item"].as_u64().unwrap();
    session.execute("file.newSequenceFromClip", json!({"items": [item]})).unwrap();
    let mut d = Driver::from_session_sized(session, size);
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.frames(3);
    d.click("grade.addFirst");
    d.click("grade.tab.primaries");
    assert!(d.element("grade.wb.pick").is_some() && d.element("grade.wb.auto").is_some());
    let temp = |d: &mut Driver| grade(d, clip)["nodes"][0]["params"]["temperature"].to_string();
    let before = temp(&mut d);
    assert_eq!(before, "null", "a new node has no Temperature yet");
    d.click("grade.wb.pick");
    // (the clicks reach the picture: graphic / mask overlays stand aside while a picker is armed)
    d.ok("ui.click", json!({"id": "program.picture", "fx": 0.5, "fy": 0.5}));
    d.frames(3);
    let g = grade(&mut d, clip);
    let t = g["nodes"][0]["params"]["temperature"].to_string();
    let status = d.ok("ui.inspect", json!({}))["ui"]["status"].clone();
    assert_ne!(t, before, "{g} status {status}");
    assert!(status.as_str().is_some_and(|s| s.starts_with("White balance")), "{status}");
    // a box drag balances the same flat frame to the same values
    d.click("grade.wb.pick");
    d.ok("ui.drag", json!({"from": {"id": "program.picture", "fx": 0.2, "fy": 0.2}, "to": {"id": "program.picture", "fx": 0.7, "fy": 0.8}}));
    d.frames(3);
    assert_eq!(temp(&mut d), t);
    // back to neutral sliders, then Auto balance gives the same result on a flat frame
    d.exec("grade.resetNode", json!({"clip": clip}));
    d.frames(2);
    assert_ne!(temp(&mut d), t);
    d.click("grade.wb.auto");
    d.frames(2);
    assert_eq!(temp(&mut d), t);
}

/// Fork C2: the Nodes panel is a node canvas. A parallel node added by command shows its mixer;
/// a node's right-click menu adds a layer node; dragging a key output to a key input connects.
#[test]
fn node_canvas_shows_branches_and_connects_by_drag() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.frames(2);
    d.click("grade.addFirst");
    d.click("grade.addNode");
    let ids: Vec<u64> = grade(&mut d, clip)["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_u64().unwrap()).collect();
    let (a, b) = (ids[0], ids[1]);
    let r = d.exec("grade.addParallel", json!({"clip": clip, "node": b}));
    let (y, m) = (r["nodes"][0].as_u64().unwrap(), r["nodes"][1].as_u64().unwrap());
    d.frames(3);
    // the mixer is on the canvas, named, with the grade's input and the new node's key port
    let els = d.ok("ui.elements", json!({"prefix": "grade."}));
    let label = |id: String| els.as_array().unwrap().iter().find(|e| e["id"] == id.as_str()).map(|e| e["label"].as_str().unwrap_or("").to_string());
    let mixer = label(format!("grade.node.{m}")).unwrap_or_else(|| panic!("mixer {m} not on the canvas: {els}"));
    assert!(mixer.contains("Parallel Mixer"), "{mixer}");
    assert!(label("grade.node.0".into()).is_some(), "the grade's input is drawn");
    assert!(label(format!("grade.port.{y}.in.1")).is_some(), "key input port of the new node");
    assert!(label(format!("grade.edge.{y}-{m}.1")).is_some(), "the new node feeds mixer input 2");
    d.snapshot("grade-canvas-parallel", Some("grade.node."));
    // a node's menu: Add Layer Node on node a
    d.ok("ui.click", json!({"id": format!("grade.node.{a}"), "button": "right"}));
    d.frames(3);
    d.click("grade.nodeMenu.layer");
    let g = grade(&mut d, clip);
    assert!(g["nodes"].as_array().unwrap().iter().any(|n| n["kind"] == "LayerMixer"), "{g}");
    // drag a's key output onto y's key input
    d.frames(2);
    d.ok("ui.drag", json!({"from": {"id": format!("grade.port.{a}.out.1")}, "to": {"id": format!("grade.port.{y}.in.1")}, "steps": 8}));
    d.frames(3);
    let g = grade(&mut d, clip);
    let edge = json!({"from": {"node": a, "index": 1}, "to": {"node": y, "index": 1}});
    assert!(g["edges"].as_array().is_some_and(|e| e.contains(&edge)), "{g}");
}

/// A node's number parameter as stored (its static value), when set.
fn float_param(g: &Value, node: u64, key: &str) -> Option<f64> {
    let n = g["nodes"].as_array()?.iter().find(|n| n["id"] == json!(node))?;
    n["params"][key]["value"]["Float"].as_f64()
}

fn label_of(d: &mut Driver, id: &str) -> Option<String> {
    let v = d.ok("ui.elements", json!({"prefix": id}));
    v.as_array()?.iter().find(|e| e["id"] == id).and_then(|e| e["label"].as_str().map(str::to_string))
}

/// Fork C2b: a selected mixer shows one row per connected input instead of the correction tabs.
/// Parallel: dragging a weight sets `mix.gain.<i>` in one undo step and the shares follow; layer:
/// the blend mode dropdown; key mixer: invert; an outside node: its key input on the Qualifier tab.
#[test]
fn mixer_and_key_input_controls_edit_the_selected_node() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.frames(2);
    d.click("grade.addFirst");
    let a = grade(&mut d, clip)["nodes"][0]["id"].as_u64().unwrap();
    let r = d.exec("grade.addParallel", json!({"clip": clip, "node": a}));
    let (y, m) = (r["nodes"][0].as_u64().unwrap(), r["nodes"][1].as_u64().unwrap());
    d.exec("grade.selectNode", json!({"clip": clip, "node": m}));
    d.frames(3);
    let ids = d.ids("grade.");
    for id in ["grade.mixer.0.weight", "grade.mixer.1.weight", "grade.mixer.1.weight.reset", "grade.mixer.1.weight.anim"] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    assert!(!ids.iter().any(|i| i.starts_with("grade.mixer.2.")), "only connected inputs: {ids:?}");
    assert!(!ids.iter().any(|i| i == "grade.tab.primaries"), "a mixer shows no correction tabs: {ids:?}");
    assert_eq!(label_of(&mut d, "grade.mixer.1.share").as_deref(), Some("50 %"));
    d.snapshot("grade-mixer-parallel", Some("grade.mixer."));

    // drag input 2's weight down: one parameter, one undo step
    let [x, top, w, h] = d.element("grade.mixer.1.weight").expect("weight slider");
    let cy = top + h / 2.0;
    d.ok("ui.drag", json!({"from": {"x": x + w * 0.3, "y": cy}, "to": {"x": x + w * 0.1, "y": cy}, "steps": 8}));
    d.frames(3);
    let g = grade(&mut d, clip);
    let gain = float_param(&g, m, "mix.gain.1").unwrap_or_else(|| panic!("mix.gain.1 set: {g}"));
    assert!(gain < 0.9, "dragged down: {gain}");
    assert_ne!(label_of(&mut d, "grade.mixer.1.share").as_deref(), Some("50 %"), "the share follows the weight");
    d.exec("edit.undo", json!({}));
    let g = grade(&mut d, clip);
    assert_eq!(float_param(&g, m, "mix.gain.1"), None, "one undo removes the whole drag: {g}");

    // a layer mixer: blend mode from the dropdown, an opacity slider
    let r = d.exec("grade.addLayer", json!({"clip": clip, "node": y}));
    let lm = r["nodes"][1].as_u64().unwrap();
    d.exec("grade.selectNode", json!({"clip": clip, "node": lm}));
    d.frames(3);
    assert!(d.element("grade.mixer.1.opacity").is_some());
    d.click("grade.mixer.1.mode");
    d.frames(2);
    d.click("grade.mixer.1.mode.3");
    let g = grade(&mut d, clip);
    assert_eq!(float_param(&g, lm, "layer.mode.1"), Some(3.0), "Multiply: {g}");
    d.snapshot("grade-mixer-layer", Some("grade.mixer."));

    // a key mixer: invert input 1
    let r = d.exec("grade.addKeyMixer", json!({"clip": clip, "node": a}));
    let k = r["nodes"][0].as_u64().unwrap();
    d.exec("grade.selectNode", json!({"clip": clip, "node": k}));
    d.frames(3);
    assert!(d.element("grade.mixer.0.mode").is_some());
    d.click("grade.mixer.0.invert");
    let g = grade(&mut d, clip);
    assert_eq!(float_param(&g, k, "kmix.invert.0"), Some(1.0), "{g}");

    // an outside node: its inverted key input, shown on the Qualifier tab
    let r = d.exec("grade.addOutside", json!({"clip": clip, "node": a}));
    let o = r["nodes"][0].as_u64().unwrap();
    d.exec("grade.selectNode", json!({"clip": clip, "node": o}));
    d.frames(2);
    d.click("grade.tab.qualifier");
    assert!(d.element("grade.key.gain").is_some() && d.element("grade.key.offset").is_some());
    d.click("grade.key.invert");
    let g = grade(&mut d, clip);
    assert_eq!(float_param(&g, o, "key.invert"), Some(0.0), "unticked: {g}");
}

fn node_pos(g: &Value, id: u64) -> Option<[f64; 2]> {
    let n = g["nodes"].as_array()?.iter().find(|n| n["id"] == id)?;
    let p = n["pos"].as_array()?;
    Some([p.first()?.as_f64()?, p.get(1)?.as_f64()?])
}

/// Fork C2b: a node dragged on the Color page canvas keeps its place, after another edit and
/// through a save / open round trip; the drag is one undo step; Reset Layout (the canvas's
/// background menu) lays the grade out automatically again.
#[test]
fn dragged_nodes_keep_their_place_and_reset_layout_restores_the_auto_layout() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.frames(2);
    d.click("grade.addFirst");
    d.click("grade.addNode");
    let ids: Vec<u64> = grade(&mut d, clip)["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_u64().unwrap()).collect();
    let (a, b) = (ids[0], ids[1]);
    d.frames(3);
    let g0 = grade(&mut d, clip);
    assert!(!g0.to_string().contains("pos"), "an undragged grade stores no positions: {g0}");
    assert!(d.element("grade.canvas").is_some());
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().map_or(0, Vec::len);
    let [x, y, w, h] = d.element(&format!("grade.node.{b}")).unwrap_or_else(|| panic!("node {b} not on the canvas"));
    let from = [x + w * 0.5, y + h * 0.15];
    d.ok("ui.drag", json!({"from": {"x": from[0], "y": from[1]}, "to": {"x": from[0] + 40.0, "y": from[1] + 90.0}, "steps": 8}));
    d.frames(3);
    let g1 = grade(&mut d, clip);
    let pb = node_pos(&g1, b).unwrap_or_else(|| panic!("the dragged node has a position: {g1}"));
    assert!(node_pos(&g1, a).is_some(), "the first drag keeps every other node where it was: {g1}");
    let undo1 = d.exec("history.list", json!({}))["undo"].as_array().map_or(0, Vec::len);
    assert_eq!(undo1, undo0 + 1, "one undo step for the drag");
    let [x1, y1, _, _] = d.element(&format!("grade.node.{b}")).unwrap();
    assert!((x1 - x - 40.0).abs() < 6.0 && (y1 - y - 90.0).abs() < 6.0, "the node is where it was dropped: {x},{y} → {x1},{y1}");
    // another edit: the node stays
    d.exec("grade.setParam", json!({"clip": clip, "node": a, "param": "saturation", "value": 0.7}));
    d.frames(3);
    assert_eq!(node_pos(&grade(&mut d, clip), b), Some(pb));
    let [x2, y2, _, _] = d.element(&format!("grade.node.{b}")).unwrap();
    assert!((x2 - x1).abs() < 1.0 && (y2 - y1).abs() < 1.0, "the canvas didn't move: {x1},{y1} → {x2},{y2}");
    // save and open again: the layout comes back
    let dir = std::env::temp_dir().join(format!("fc-grade-ui-pos-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pos.fcproj").to_string_lossy().to_string();
    d.exec("file.saveAs", json!({"path": path}));
    d.exec("file.open", json!({"path": path}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.frames(3);
    assert_eq!(node_pos(&grade(&mut d, clip), b), Some(pb), "the position survives save / open");
    let _ = std::fs::remove_dir_all(&dir);
    // Reset Layout from the background menu (a spot without nodes)
    d.ok("ui.click", json!({"id": "grade.canvas", "fx": 0.97, "fy": 0.95, "button": "right"}));
    d.frames(3);
    assert!(d.element("grade.menu.resetLayout").is_some(), "Reset Layout is in the canvas menu");
    d.click("grade.menu.resetLayout");
    d.frames(2);
    let g2 = grade(&mut d, clip);
    assert!(!g2.to_string().contains("pos"), "Reset Layout clears the positions: {g2}");
    d.exec("edit.undo", json!({}));
    assert_eq!(node_pos(&grade(&mut d, clip), b), Some(pb), "Reset Layout is undoable");
}

/// Whether the node's canvas element lies inside the canvas.
fn node_in_canvas(d: &mut Driver, node: u64) -> bool {
    let (Some(c), Some(n)) = (d.element("grade.canvas"), d.element(&format!("grade.node.{node}"))) else { return false };
    n[0] >= c[0] - 0.5 && n[1] >= c[1] - 0.5 && n[0] + n[2] <= c[0] + c[2] + 0.5 && n[1] + n[3] <= c[1] + c[3] + 0.5
}

/// Fork C2b review: a shape change while the pointer is held for something other than a node
/// drag (a dock splitter, a scrub that changes the clip) refits the canvas on release.
#[test]
fn canvas_refits_on_release_after_a_change_with_the_pointer_held() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let seq = d.exec("sequence.inspect", json!({}));
    let clips: Vec<u64> = seq["video"][0]["items"].as_array().unwrap().iter().filter_map(|i| i["clip"].as_u64()).collect();
    let (c1, c2) = (clips[0], clips[1]);
    let n1 = d.exec("grade.addNode", json!({"clip": c1, "after": null}))["node"].as_u64().unwrap();
    let n2 = d.exec("grade.addNode", json!({"clip": c2, "after": null}))["node"].as_u64().unwrap();
    d.exec("timeline.select", json!({"clips": [c1]}));
    d.frames(4);
    assert!(node_in_canvas(&mut d, n1), "fitted at first");
    // pan the view far away (middle drag on the background)
    let c = d.element("grade.canvas").unwrap();
    let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Middle, pressed, modifiers: Default::default() };
    let mut p = egui::pos2(c[0] + c[2] * 0.97, c[1] + c[3] * 0.95);
    push(&mut d, egui::Event::PointerMoved(p));
    d.frames(1);
    push(&mut d, button(p, true));
    d.frames(1);
    for _ in 0..8 {
        p.x -= c[2] * 0.2;
        push(&mut d, egui::Event::PointerMoved(p));
        d.frames(1);
    }
    push(&mut d, button(p, false));
    d.frames(3);
    assert!(!node_in_canvas(&mut d, n1), "panned out of view");
    // hold the pointer (not on a node) and change the clip: no refit while held…
    let hold = egui::pos2(c[0] + c[2] * 0.97, c[1] + c[3] * 0.95);
    push(&mut d, egui::Event::PointerMoved(hold));
    d.frames(1);
    push(&mut d, button(hold, true));
    d.frames(1);
    d.exec("timeline.select", json!({"clips": [c2]}));
    d.frames(3);
    assert!(!node_in_canvas(&mut d, n2), "no refit while the pointer is held");
    // …and the fit on release
    push(&mut d, button(hold, false));
    d.frames(3);
    assert!(node_in_canvas(&mut d, n2), "the canvas refits on release");
}
