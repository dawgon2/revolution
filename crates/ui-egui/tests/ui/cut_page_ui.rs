//! Fork RS4 (RESOLVE-UI-PLAN): the Resolve workflow's Cut page: the media pool and viewer over
//! the Cut timeline (fast-edit toolbar, the upper whole timeline, the lower zoomed-in timeline
//! with the playhead held in the middle), both timelines drawn by the Timeline panel's own code.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also writes `pages-resolve-cut*.png` (1920×1080).

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::settings::Workflow;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<PathBuf>,
}

impl Driver {
    fn resolve_demo() -> Self {
        let mut s = Session::default();
        s.prefs.workflow = Workflow::Premiere;
        s.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1920.0, 1080.0)).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
        d.frames(4);
        d.exec("settings.setWorkflow", json!({"workflow": "resolve"}));
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
            assert!(self.harness.state().ui_error.is_none(), "the app caught a UI panic: {:?}", self.harness.state().ui_error);
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
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("{id} not on screen")).clone();
        let r = &e["rect"];
        [r[0].as_f64().unwrap(), r[1].as_f64().unwrap(), r[2].as_f64().unwrap(), r[3].as_f64().unwrap()]
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(4);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("pages-{name}.png"))).unwrap();
    }
    fn clip_ids(&mut self, prefix: &str) -> Vec<String> {
        let mut v: Vec<String> =
            self.ids(prefix).into_iter().filter(|i| i.strip_prefix(prefix).is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))).collect();
        v.sort();
        v.dedup();
        v
    }
}

#[test]
fn cut_page_shows_the_whole_and_the_zoomed_timeline_and_edits_in_both() {
    let mut d = Driver::resolve_demo();
    d.click("page.cut");
    assert_eq!(d.harness.state().ui.workspace, "Cut Page");
    for id in [
        "panel.Project",
        "panel.Program",
        "panel.CutTimeline",
        "cut.smartInsert",
        "cut.append",
        "cut.rippleOverwrite",
        "cut.closeUp",
        "cut.placeOnTop",
        "cut.sourceOverwrite",
        "cut.blade",
    ] {
        assert!(!d.ids(id).is_empty(), "{id} on the Cut page");
    }
    // both views draw every clip under their own ids; the Timeline panel's ids are not used
    let upper = d.clip_ids("cutUpper.clip.");
    let lower_all = d.clip_ids("cutLower.clip.");
    assert!(!upper.is_empty(), "the upper timeline shows the clips");
    assert!(d.clip_ids("timeline.clip.").is_empty(), "no Timeline panel on the Cut page");
    // the upper one shows the whole timeline: every clip inside the panel
    let panel = d.rect("panel.CutTimeline");
    for id in &upper {
        let r = d.rect(id);
        assert!(r[0] >= panel[0] - 1.0 && r[0] + r[2] <= panel[0] + panel[2] + 1.0, "{id} {r:?} inside {panel:?}");
    }
    // the lower one keeps the playhead in the middle: moving it 1 s moves the clips 1 s left
    d.exec("playhead.set", json!({"seconds": 1.0}));
    d.frames(6);
    let first = lower_all.first().cloned().expect("a clip in the lower timeline");
    let a = d.rect(&first);
    d.exec("playhead.set", json!({"seconds": 2.0}));
    d.frames(6);
    let b = d.rect(&first);
    let moved = a[0] - b[0];
    assert!(moved > 100.0 && moved < 220.0, "1 s at the lower zoom (160 pt/s): moved {moved}");
    d.shot("resolve-cut");
    // zoom in: the same second moves the clips further
    d.click("cut.zoomIn");
    d.frames(30);
    let c = d.rect(&first);
    d.exec("playhead.set", json!({"seconds": 1.0}));
    d.frames(6);
    let e = d.rect(&first);
    assert!(e[0] - c[0] > moved * 1.3, "zoomed in: {} vs {moved}", e[0] - c[0]);
    // Blade cuts at the playhead: one more clip in both views
    let before = d.clip_ids("cutUpper.clip.").len();
    d.exec("playhead.set", json!({"seconds": 1.5}));
    d.frames(3);
    d.click("cut.blade");
    d.frames(4);
    let after_upper = d.clip_ids("cutUpper.clip.").len();
    assert!(after_upper > before, "blade: {before} → {after_upper}");
    // Append (fork RS4.1 `edit.append`) puts the media pool's selected clip at the end
    let clip = d
        .harness
        .state()
        .session
        .project
        .items
        .values()
        .find(|i| matches!(i.kind, filmcraft_project::ItemKind::Media(_)) && i.name.ends_with(".mp4"))
        .map(|i| i.id.0)
        .expect("a clip");
    let before_len = d.harness.state().session.active_sequence().map(|q| q.duration()).expect("sequence");
    d.exec("project.select", json!({"items": [clip]}));
    d.frames(3);
    d.click("cut.append");
    d.frames(3);
    let after_len = d.harness.state().session.active_sequence().map(|q| q.duration()).expect("sequence");
    assert!(after_len > before_len, "append made the timeline longer: {before_len:?} → {after_len:?}");
    // the Edit page's Timeline keeps its own ids
    d.click("page.edit");
    assert!(!d.clip_ids("timeline.clip.").is_empty(), "the Edit page's timeline");
    assert!(d.clip_ids("cutUpper.clip.").is_empty());
}
