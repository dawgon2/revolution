//! Fork 6.7b step 6: the clip's Temporal NR rows in Effect Controls (the Inspector in the Resolve
//! workflow) and on the Color page's Noise tab (design `docs/fork/DESIGN-6.7b.md` §1.7, tests
//! 25–26).
//!
//! Set `FILMCRAFT_UI_SHOTS=<dir>` to also render offscreen with wgpu and write `tnr-*.png`.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use filmcraft_engine::Session;
use filmcraft_engine::settings::Workflow;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<std::path::PathBuf>,
}

impl Driver {
    fn new(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1920.0, 1080.0)).with_max_steps(10_000).with_pixels_per_point(1.0);
        if shots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
        d.frames(4);
        d
    }

    fn demo(workflow: Workflow) -> Self {
        let mut session = Session::default();
        session.prefs.workflow = workflow;
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        Self::new(session)
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

    fn element(&mut self, id: &str) -> Option<[f64; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array()?.iter().find(|e| e["id"] == id).map(|e| {
            let r = &e["rect"];
            [r[0].as_f64().unwrap(), r[1].as_f64().unwrap(), r[2].as_f64().unwrap(), r[3].as_f64().unwrap()]
        })
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    /// The value the slider registered as `id` shows (its accessibility value).
    fn slider_value(&mut self, id: &str) -> Option<f64> {
        let [x, y, w, h] = self.element(id)?;
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let found = std::cell::Cell::new(None);
        let _ = self
            .harness
            .query_all_by(|n| {
                let hit = n.numeric_value().is_some() && n.bounding_box().is_some_and(|b| b.x0 <= cx && cx <= b.x1 && b.y0 <= cy && cy <= b.y1);
                if hit && found.get().is_none() {
                    found.set(n.numeric_value());
                }
                hit
            })
            .count();
        found.get()
    }

    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join(format!("{name}.png"));
                img.save(&path).unwrap();
                eprintln!("shot: {}", path.display());
            }
            Err(e) => eprintln!("shot {name} skipped: {e}"),
        }
    }
}

fn first_clip(d: &mut Driver) -> u64 {
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().unwrap_or_else(|| panic!("no clip: {seq}"));
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.frames(2);
    clip
}

fn luma(d: &mut Driver, clip: u64) -> f64 {
    d.exec("tnr.get", json!({"clip": clip}))["values"]["luma"].as_f64().unwrap()
}

#[test]
fn tnr_rows_in_effect_controls_and_the_noise_tab_agree() {
    for workflow in [Workflow::Premiere, Workflow::Resolve] {
        let tag = format!("{workflow:?}").to_lowercase();
        let mut d = Driver::demo(workflow);
        let clip = first_clip(&mut d);
        d.exec("tnr.set", json!({"clip": clip, "param": "luma", "value": 0.4}));
        // Effect Controls (relabelled Keyframes in the Resolve workflow, fork RS4): the fixed section
        d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
        d.frames(3);
        let title = if workflow == Workflow::Resolve { "Keyframes" } else { "Effect Controls" };
        assert!(d.harness.query_by_label(title).is_some(), "{tag}: the {title} tab");
        assert!(d.element("effectControls.tnr").is_some(), "{tag}: the Temporal NR section");
        // (below the fixed effects: scroll it into view when the panel is short)
        for _ in 0..4 {
            if d.element("tnr.motion.anim").is_some() {
                break;
            }
            d.ok("ui.scroll", json!({"id": "effectControls.lane", "dy": -300.0}));
            d.frames(2);
        }
        let ids = d.ids("tnr.");
        for id in ["tnr.enabled", "tnr.luma", "tnr.luma.reset", "tnr.luma.anim", "tnr.chroma", "tnr.frames", "tnr.motion.anim"] {
            assert!(ids.iter().any(|i| i == id), "{tag}: {id} missing from {ids:?}");
        }
        let v = d.slider_value("tnr.luma").unwrap_or_else(|| panic!("{tag}: no slider value"));
        assert!((v - 0.4).abs() < 1e-3, "{tag}: Effect Controls shows {v}");
        d.shot(&format!("tnr-effect-controls-{tag}"));
        // a drag on the slider is one undo step
        let [x, y, w, h] = d.element("tnr.luma").unwrap();
        let cy = y + h / 2.0;
        d.ok("ui.drag", json!({"from": {"x": x + w * 0.3, "y": cy}, "to": {"x": x + w * 0.8, "y": cy}, "steps": 6}));
        d.frames(2);
        let dragged = luma(&mut d, clip);
        assert!((dragged - 0.4).abs() > 0.05, "{tag}: the drag changed luma ({dragged})");
        d.exec("edit.undo", json!({}));
        assert!((luma(&mut d, clip) - 0.4).abs() < 1e-6, "{tag}: one undo undoes the whole drag");
        // the keyframe toggle animates the value; the switch turns it off and on
        d.click("tnr.luma.anim");
        let got = d.exec("tnr.get", json!({"clip": clip}));
        assert!(got["params"]["luma"]["keyframes"].as_array().is_some_and(|k| !k.is_empty()), "{tag}: animated: {got}");
        // its keyframe shows in the lane on the row (no marker before this step)
        d.frames(2);
        let kf = d.ids("tnr.luma.keyframe.");
        assert_eq!(kf.len(), 1, "{tag}: one lane keyframe marker: {kf:?}");
        // dragging it moves the keyframe in time (one tnr.moveKeyframe)
        let kf_time = |d: &mut Driver| d.exec("tnr.get", json!({"clip": clip}))["params"]["luma"]["keyframes"][0]["time"].as_i64();
        let before = kf_time(&mut d);
        let [x, y, w, h] = d.element(&kf[0]).unwrap();
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 40.0, "y": cy}, "steps": 6}));
        d.frames(2);
        let after = kf_time(&mut d);
        assert!(after.is_some() && after > before, "{tag}: the drag moved the keyframe later ({before:?} → {after:?})");
        d.exec("edit.undo", json!({}));
        assert_eq!(kf_time(&mut d), before, "{tag}: one undo puts it back");
        d.exec("edit.undo", json!({}));
        d.click("tnr.enabled");
        assert_eq!(d.exec("tnr.get", json!({"clip": clip}))["enabled"], json!(false), "{tag}");
        d.exec("edit.undo", json!({}));
        // the Color page's Noise tab shows the same value
        d.exec("tnr.set", json!({"clip": clip, "param": "luma", "value": 0.65}));
        d.ok("ui.set", json!({"workspace": "Color Grading"}));
        d.frames(3);
        first_clip(&mut d);
        d.click("grade.addFirst");
        d.click("grade.tab.noise");
        d.frames(2);
        assert!(d.harness.query_by_label("Temporal NR (clip)").is_some(), "{tag}: the clip block");
        assert!(d.ids("tnr.").iter().any(|i| i == "tnr.luma"), "{tag}: tnr.luma on the Noise tab");
        let v = d.slider_value("tnr.luma").unwrap_or_else(|| panic!("{tag}: no Noise tab slider value"));
        assert!((v - 0.65).abs() < 1e-3, "{tag}: the Noise tab shows {v}");
        d.shot(&format!("tnr-noise-tab-{tag}"));
        // an edit there is what Effect Controls shows (the panel is short on the Color page:
        // scroll the rows up first)
        d.ok("ui.scroll", json!({"id": "grade.param.nr.luma", "dy": -400.0}));
        d.frames(3);
        d.click("tnr.frames.reset");
        d.ok("ui.click", json!({"id": "tnr.chroma", "fx": 0.5, "fy": 0.5}));
        d.frames(2);
        let chroma = d.exec("tnr.get", json!({"clip": clip}))["values"]["chroma"].as_f64().unwrap();
        assert!(chroma > 0.2, "{tag}: a click on the Noise tab's chroma slider set it ({chroma})");
        d.shot(&format!("tnr-noise-tab-scrolled-{tag}"));
    }
}

#[test]
fn the_noise_tab_shows_no_first_node_note() {
    let mut d = Driver::demo(Workflow::Resolve);
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let clip = first_clip(&mut d);
    d.click("grade.addFirst");
    d.click("grade.addNode");
    // the second node is the current one: no "only the first node" note, the clip block instead
    let g = d.exec("grade.get", json!({"clip": clip}));
    let second = g["grade"]["nodes"][1]["id"].as_u64().unwrap();
    d.click(&format!("grade.node.{second}"));
    d.click("grade.tab.noise");
    d.frames(2);
    assert!(d.harness.query_by_label_contains("first active node").is_none(), "the old node note is gone");
    assert!(d.harness.query_by_label("Temporal NR (clip)").is_some());
    assert!(d.element("tnr.luma").is_some() && d.element("grade.param.nr.luma").is_some());
    assert!(d.ids("grade.param.tnr").is_empty(), "no node-level temporal rows");

    // hidden on a clip that takes no temporal NR (a Color Matte)
    let mut session = Session::default();
    let item = session.execute("file.newColorMatte", json!({"color": "#808080", "width": 160, "height": 90, "seconds": 2})).unwrap()["item"].as_u64().unwrap();
    session.execute("file.newSequenceFromClip", json!({"items": [item]})).unwrap();
    let mut d = Driver::new(session);
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    first_clip(&mut d);
    d.click("grade.addFirst");
    d.click("grade.tab.noise");
    assert!(d.element("grade.param.nr.luma").is_some());
    assert!(d.ids("tnr.").is_empty(), "no temporal NR rows on a Color Matte");
    assert!(d.harness.query_by_label("Temporal NR (clip)").is_none());
    d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
    d.frames(3);
    assert!(d.element("effectControls.tnr").is_none(), "no Temporal NR section on a Color Matte");
}
