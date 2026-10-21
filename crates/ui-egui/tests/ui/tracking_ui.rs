//! Corner Pin's Track Surface buttons (fork Y3.3c, `tracking.cornerPin`) in Effect Controls (the
//! Inspector in the Resolve workflow): present in both workflows, a one-frame track writes keyframes.

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
}

impl Driver {
    fn demo(workflow: filmcraft_engine::settings::Workflow) -> Self {
        let mut session = Session::default();
        session.prefs.workflow = workflow;
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
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}: {v}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }
}

#[test]
fn corner_pin_track_surface_buttons_in_both_workflows() {
    for workflow in [Workflow::Premiere, Workflow::Resolve] {
        let mut d = Driver::demo(workflow);
        if workflow == Workflow::Resolve {
            d.ok("ui.click", json!({"id": "page.edit"}));
            d.frames(2);
            // the Resolve-style Inspector (RS4.2): Corner Pin on its Effects tab
            d.ok("ui.click", json!({"id": "pageToolbar.Inspector"}));
        } else {
            d.ok("ui.set", json!({"workspace": "Effects"}));
        }
        d.frames(2);
        let seq = d.exec("sequence.inspect", json!({}));
        let clip = &seq["video"][0]["items"][0];
        let (id, start, dur) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap(), clip["duration"].as_i64().unwrap());
        d.exec("playhead.set", json!({"time": start + dur / 2}));
        d.exec("timeline.select", json!({"clips": [id]}));
        d.exec("effects.apply", json!({"effect": "corner_pin"}));
        d.frames(3);
        if workflow == Workflow::Resolve {
            d.ok("ui.click", json!({"id": "inspector.tab.effects"}));
            d.frames(2);
        } else {
            // fold the fixed effects so the Corner Pin rows are on screen
            for fx in ["motion", "time_remap"] {
                d.ok("ui.click", json!({"id": format!("effectControls.effect.{fx}")}));
                d.frames(2);
            }
            d.ok("ui.click", json!({"id": "effectControls.tnr"}));
            d.frames(2);
        }
        for b in ["back", "backFrame", "fwdFrame", "fwd"] {
            d.rect(&format!("effectControls.cornerPin.track.{b}"));
        }
        d.ok("ui.click", json!({"id": "effectControls.cornerPin.track.fwdFrame"}));
        let mut finished = false;
        for _ in 0..400 {
            d.frames(1);
            let jobs = d.exec("jobs.list", json!({}));
            if jobs.as_array().unwrap().iter().any(|j| j["label"].as_str().unwrap_or("").starts_with("Track Corner Pin") && j["finished"] == true) {
                finished = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(finished, "{workflow:?}: the tracking job ran");
        d.frames(3);
        let undo = d.exec("history.list", json!({}))["undo"].as_array().unwrap().clone();
        assert_eq!(undo.last(), Some(&json!("Track Corner Pin")), "{workflow:?}: {undo:?}");
        // Motion's Attach to Track row: there, with its method menu; the demo has no clip below the
        // first video clip, so a click reports that instead of tracking
        if workflow == Workflow::Resolve {
            d.ok("ui.click", json!({"id": "inspector.tab.video"}));
        } else {
            d.ok("ui.click", json!({"id": "effectControls.effect.motion"}));
        }
        d.frames(2);
        for b in ["back", "backFrame", "fwdFrame", "fwd", "method"] {
            d.rect(&format!("effectControls.motion.attach.{b}"));
        }
        let n = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
        d.ok("ui.click", json!({"id": "effectControls.motion.attach.fwdFrame"}));
        d.frames(3);
        assert_eq!(d.exec("history.list", json!({}))["undo"].as_array().unwrap().len(), n, "{workflow:?}: nothing to follow, no edit");
    }
}
