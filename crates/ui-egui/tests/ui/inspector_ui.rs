//! Fork RS4.2 (RESOLVE-UI-PLAN): the Resolve-style Inspector, driven through the control channel
//! on the Resolve workflow's Edit page: the page toolbar opens it, its tabs follow the selection,
//! Zoom X / Y stay linked, Opacity, a section's on dot and reset, a row's keyframe diamond, the
//! File tab. The Premiere workflow keeps Effect Controls.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the window (1920×1080) with wgpu and
//! writes `inspector.png`.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::settings::Workflow;
use filmcraft_project::{ClipId, EffectInstance};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<PathBuf>,
}

impl Driver {
    fn demo(workflow: Workflow) -> Self {
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
        let name = if workflow == Workflow::Resolve { "resolve" } else { "premiere" };
        d.exec("settings.setWorkflow", json!({"workflow": name}));
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
    /// Click a number field, type a value and press Enter.
    fn type_into(&mut self, id: &str, text: &str) {
        self.click(id);
        self.ok("ui.key", json!({"key": "Cmd+A"}));
        self.ok("ui.type", json!({"text": text}));
        self.ok("ui.key", json!({"key": "Enter"}));
        self.frames(3);
    }
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn has(&mut self, id: &str) -> bool {
        self.ids(id).iter().any(|x| x == id)
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("{id} not on screen")).clone();
        let r = &e["rect"];
        [r[0].as_f64().unwrap(), r[1].as_f64().unwrap(), r[2].as_f64().unwrap(), r[3].as_f64().unwrap()]
    }
    fn effect(&self, clip: u64, id: &str) -> EffectInstance {
        let q = self.harness.state().session.active_sequence().unwrap();
        q.find_item(ClipId(clip)).unwrap().1.effect(id).unwrap_or_else(|| panic!("no {id}")).clone()
    }
    fn num(&self, clip: u64, effect: &str, param: &str) -> f64 {
        self.effect(clip, effect).params[param].value.as_f64().unwrap()
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(4);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
    /// The first video clip and the audio linked to it, selected, with the playhead in it.
    fn select_first_clip(&mut self) -> (u64, Option<u64>) {
        let seq = self.exec("sequence.inspect", json!({}));
        let v = &seq["video"][0]["items"][0];
        let (clip, start, dur) = (v["clip"].as_u64().unwrap(), v["start"].as_i64().unwrap(), v["duration"].as_i64().unwrap());
        let audio = seq["audio"][0]["items"].as_array().and_then(|a| a.iter().find(|a| a["start"].as_i64() == Some(start))).and_then(|a| a["clip"].as_u64());
        self.exec("playhead.set", json!({"time": start + dur / 2}));
        let clips: Vec<u64> = std::iter::once(clip).chain(audio).collect();
        self.exec("timeline.select", json!({"clips": clips}));
        self.frames(3);
        (clip, audio)
    }
}

#[test]
fn resolve_inspector_edits_the_clip_in_resolve_units() {
    let mut d = Driver::demo(Workflow::Resolve);
    d.click("page.edit");
    let (clip, audio) = d.select_first_clip();
    // the toolbar's Inspector opens the Resolve-style panel, not Effect Controls
    assert!(!d.has("panel.Inspector"));
    d.click("pageToolbar.Inspector");
    assert!(d.has("panel.Inspector"), "Inspector shown");
    assert!(!d.has("panel.EffectControls"));
    let (insp, prg) = (d.rect("panel.Inspector"), d.rect("panel.Program"));
    assert!(insp[0] >= prg[0] + prg[2] - 1.0, "Inspector right of the viewers: {insp:?} {prg:?}");
    for tab in ["video", "audio", "effects", "transition", "image", "file"] {
        assert!(d.has(&format!("inspector.tab.{tab}")), "tab {tab}");
    }
    // the Video tab's sections and rows; no keyframe lane
    for id in [
        "inspector.section.transform",
        "inspector.transform.enable",
        "inspector.transform.keyframe",
        "inspector.transform.reset",
        "inspector.transform.zoomX",
        "inspector.transform.zoomY",
        "inspector.transform.zoomLink",
        "inspector.transform.positionX",
        "inspector.transform.positionY",
        "inspector.transform.rotation",
        "inspector.transform.rotation.slider",
        "inspector.transform.anchorX",
        "inspector.transform.flipH",
        "inspector.transform.flipV",
        "inspector.section.cropping",
        "inspector.section.composite",
        "inspector.composite.blend",
        "inspector.composite.opacity",
        "inspector.section.speed",
    ] {
        assert!(d.has(id), "{id} on screen");
    }
    assert!(d.ids("effectControls.lane").is_empty(), "no keyframe lane");
    d.shot("inspector");
    // Zoom X 1.5 with the link on: Scale 150 %, which Zoom Y (Scale) shows too
    assert!(d.effect(clip, "motion").params["uniform_scale"].value.as_bool().unwrap());
    d.type_into("inspector.transform.zoomX", "1.5");
    assert_eq!(d.num(clip, "motion", "scale"), 150.0, "Zoom 1.5 = Scale 150 %");
    assert!(d.effect(clip, "motion").params["uniform_scale"].value.as_bool().unwrap(), "still linked: Y follows X");
    // Position in pixels from the centre, Y up
    d.type_into("inspector.transform.positionX", "100");
    let seq = d.harness.state().session.active_sequence().unwrap().settings.clone();
    let p = d.effect(clip, "motion").params["position"].value.as_vec2().unwrap();
    assert_eq!(p.x, f64::from(seq.width) / 2.0 + 100.0, "{p:?}");
    // Opacity 40
    d.type_into("inspector.composite.opacity", "40");
    assert_eq!(d.num(clip, "opacity", "opacity"), 40.0);
    // a row's diamond adds a keyframe at the playhead
    d.click("inspector.transform.rotation.keyframe");
    assert_eq!(d.effect(clip, "motion").params["rotation"].keyframes.len(), 1, "a Rotation keyframe");
    // the link off: Zoom X is Scale Width from where the zoom was
    d.click("inspector.transform.zoomLink");
    let m = d.effect(clip, "motion");
    assert!(!m.params["uniform_scale"].value.as_bool().unwrap());
    assert_eq!(m.params["scale_width"].value.as_f64(), Some(150.0));
    d.type_into("inspector.transform.zoomX", "2");
    assert_eq!((d.num(clip, "motion", "scale_width"), d.num(clip, "motion", "scale")), (200.0, 150.0), "unlinked: Y stays");
    // the section's dot turns Transform off and on
    d.click("inspector.transform.enable");
    assert!(!d.effect(clip, "motion").enabled, "Transform off");
    d.click("inspector.transform.enable");
    assert!(d.effect(clip, "motion").enabled, "Transform on");
    // reset: Transform's values back to the defaults, Opacity's row back to 100
    d.click("inspector.transform.reset");
    assert_eq!(d.num(clip, "motion", "scale"), 100.0);
    d.click("inspector.composite.opacity.reset");
    assert_eq!(d.num(clip, "opacity", "opacity"), 100.0);
    // Cropping isn't on the clip until its dot turns it on
    assert!(d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.effect("crop").is_none());
    d.click("inspector.cropping.enable");
    assert!(d.effect(clip, "crop").enabled, "Crop applied by its dot");
    // a greyed tab doesn't switch: Image, and Transition without a transition selected
    d.click("inspector.tab.image");
    assert!(d.has("inspector.section.transform"), "still on Video");
    // Audio: the linked clip's Volume and Pan
    if let Some(a) = audio {
        d.click("inspector.tab.audio");
        assert!(d.has("inspector.section.volume") && d.has("inspector.volume.level"), "{:?}", d.ids("inspector."));
        d.type_into("inspector.volume.level", "-6");
        assert_eq!(d.num(a, "volume", "level"), -6.0);
    }
    // File: the media's details
    d.click("inspector.tab.file");
    for id in ["inspector.file.name", "inspector.file.path", "inspector.file.duration"] {
        assert!(d.has(id), "{id}: {:?}", d.ids("inspector."));
    }
    d.shot("inspector-file");
    // nothing selected: a placeholder, tabs greyed
    d.exec("timeline.select", json!({"clips": []}));
    d.frames(3);
    assert!(d.has("inspector.tab.video") && !d.has("inspector.section.transform"));
}

#[test]
fn premiere_keeps_effect_controls() {
    let mut d = Driver::demo(Workflow::Premiere);
    d.select_first_clip();
    d.ok("ui.menu.invoke", json!({"id": "window.panel.EffectControls", "params": {}}));
    d.frames(3);
    assert!(d.has("panel.EffectControls"), "Effect Controls open");
    assert!(d.has("effectControls.effect.motion") && d.has("effectControls.lane"), "its Motion section and keyframe lane");
    assert!(!d.has("panel.Inspector") && d.ids("inspector.").is_empty(), "no Resolve Inspector in Premiere");
    assert!(d.ids("pageToolbar.").is_empty());
}
