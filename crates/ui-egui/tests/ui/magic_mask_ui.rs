//! Color page ▸ Magic Mask (fork GRADE-PLAN G3.3), driven by automation id: add the mask, stroke
//! on the Program picture, Refine sliders (one undo step per drag), Track with a fake segmenter
//! (the Program frame re-renders with the matte) and Track without a model (a plain error).

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::background::{BackgroundWorker, WorkerConfig};
use filmcraft_mask::{Embedding, LOW_RES, MaskError, Prompt, RgbImage, Segment, Segmenter};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new(session: Session) -> Self {
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

    /// Steps until the frame workers have been idle for a few frames (LO.2): in an optimized build
    /// the Program picture and the panels that follow it land mid-gesture and move what a drag
    /// measured, so a drag waits for the layout to come to rest first.
    fn settle(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut idle = 0;
        while idle < 4 && std::time::Instant::now() < deadline {
            self.frames(1);
            idle = if self.harness.state().frames.queue_len() == 0 { idle + 1 } else { 0 };
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }

    /// [`Self::settle`], then steps until `id`'s rectangle stays the same for three looks in a
    /// row (LO.2: under a loaded ci, panels still moved between measuring and dragging after the
    /// frame workers were idle).
    fn settle_on(&mut self, id: &str) {
        self.settle();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let (mut last, mut same) = (serde_json::Value::Null, 0);
        while same < 3 && std::time::Instant::now() < deadline {
            let v = self.ok("ui.elements", json!({"prefix": id}));
            let rect = v.as_array().and_then(|a| a.iter().find(|e| e["id"] == id)).map(|e| e["rect"].clone()).unwrap_or_default();
            same = if rect == last && !rect.is_null() { same + 1 } else { 0 };
            last = rect;
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(15));
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

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().is_some_and(|a| a.iter().any(|e| e["id"] == id))
    }
}

/// The fake segmenter: the left half of the frame, whatever the prompt.
struct LeftHalf(Duration);

impl Segmenter for LeftHalf {
    fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError> {
        Ok(Embedding::new(image.width(), image.height(), Box::new(())))
    }
    fn segment(&self, _: &Embedding, _: &Prompt) -> Result<Segment, MaskError> {
        std::thread::sleep(self.0);
        let mut s = Segment::empty((1.0, 1.0));
        for (i, l) in s.logits.iter_mut().enumerate() {
            *l = if i % LOW_RES < LOW_RES / 2 { 10.0 } else { -10.0 };
        }
        s.score = 1.0;
        Ok(s)
    }
}

/// A 2 s colour matte in its own sequence on the Color page, its clip selected with a grade node.
fn setup(segmenter: bool) -> (Driver, u64, u64) {
    setup_with(segmenter.then_some(Duration::ZERO))
}

/// [`setup`] with a fake segmenter taking `delay` per frame (None: no model).
fn setup_with(segmenter: Option<Duration>) -> (Driver, u64, u64) {
    let mut s = Session::default();
    // the Premiere-style layout these tests measure (the fork build defaults to Resolve; LO.2)
    s.prefs.workflow = filmcraft_engine::settings::Workflow::Premiere;
    let item = s.execute("file.newColorMatte", json!({"color": "#a08c78", "width": 160, "height": 90, "seconds": 2})).unwrap()["item"].as_u64().unwrap();
    s.execute("file.newSequenceFromClip", json!({"items": [item]})).unwrap();
    s.background = BackgroundWorker::new(WorkerConfig { resume_delay: Duration::ZERO, scrub_window: Duration::ZERO, pace: None, memory: None });
    // never the machine's own models (the data folder may hold SAM 2.1 / MobileSAM)
    s.magic_mask.sam2_dir = Some(std::env::temp_dir().join("filmcraft-no-such-dir").join("sam2"));
    if let Some(delay) = segmenter {
        s.magic_mask.segmenter = Some(Arc::new(LeftHalf(delay)));
    } else {
        s.magic_mask.model_path = Some(std::env::temp_dir().join("filmcraft-no-such-dir").join("mobile_sam.pt"));
    }
    let mut d = Driver::new(s);
    d.ok("ui.set", json!({"workspace": "Color Grading"}));
    d.frames(3);
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().unwrap_or_else(|| panic!("no clip: {seq}"));
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("playhead.set", json!({"frame": 5}));
    d.frames(2);
    d.click("grade.addFirst");
    let node = grade(&mut d, clip)["nodes"][0]["id"].as_u64().unwrap();
    d.click("grade.tab.magicMask");
    (d, clip, node)
}

fn grade(d: &mut Driver, clip: u64) -> Value {
    d.exec("grade.get", json!({"clip": clip}))["grade"].clone()
}

fn mask(d: &mut Driver, clip: u64) -> Value {
    grade(d, clip)["nodes"][0]["magicMask"].clone()
}

#[test]
fn magic_mask_add_stroke_refine_and_track() {
    let (mut d, clip, node) = setup(true);
    assert!(d.has("grade.magicMask.add") && d.has("grade.magicMask.mode.person"));
    d.click("grade.magicMask.mode.person");
    d.click("grade.magicMask.add");
    let m = mask(&mut d, clip);
    assert_eq!(m["mode"], json!("person"), "{m}");
    // the stroke tool comes ready: a click on the picture is a point on the subject
    assert!(d.harness.state().ui.magic_mask.tool.is_some(), "Add arms the stroke tool");
    d.ok("ui.click", json!({"id": "program.picture", "fx": 0.25, "fy": 0.5}));
    d.frames(2);
    let m = mask(&mut d, clip);
    let pt = &m["strokes"][0]["points"][0];
    assert!((pt["x"].as_f64().unwrap() - 0.25).abs() < 0.05 && pt.get("positive").is_none_or(|p| p == true), "{m}");
    // Stroke − leaves out; a drag draws a box
    d.click("grade.magicMask.tool.subtract");
    d.ok("ui.click", json!({"id": "program.picture", "fx": 0.8, "fy": 0.5}));
    d.frames(2);
    d.click("grade.magicMask.tool.add");
    d.ok("ui.drag", json!({"from": {"id": "program.picture", "fx": 0.1, "fy": 0.2}, "to": {"id": "program.picture", "fx": 0.45, "fy": 0.8}}));
    d.frames(2);
    let m = mask(&mut d, clip);
    let strokes = m["strokes"].as_array().cloned().unwrap_or_default();
    assert_eq!(strokes.len(), 3, "{m}");
    assert_eq!(strokes[1]["points"][0]["positive"], json!(false), "{m}");
    assert!(strokes[2]["rect"].is_array(), "{m}");
    // Esc puts the tool down
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(d.harness.state().ui.magic_mask.tool.is_none());

    // a Refine slider drag is one undo step
    d.settle_on("grade.magicMask.refine.soften");
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().map_or(0, Vec::len);
    d.ok("ui.drag", json!({"from": {"id": "grade.magicMask.refine.soften", "fx": 0.05, "fy": 0.5}, "to": {"id": "grade.magicMask.refine.soften", "fx": 0.4, "fy": 0.5}, "steps": 6}));
    d.frames(2);
    let soften = mask(&mut d, clip)["refine"]["soften"].as_f64().unwrap_or(0.0);
    assert!(soften > 0.0, "{}", mask(&mut d, clip));
    let undo1 = d.exec("history.list", json!({}))["undo"].as_array().map_or(0, Vec::len);
    assert_eq!(undo1, undo0 + 1, "one undo step per drag");
    // (the bottom of the tab scrolls into view in a short panel)
    d.call("ui.scroll", json!({"id": "grade.magicMask.refine.soften", "dy": -400.0}));
    d.frames(2);
    d.click("grade.magicMask.refine.invert");
    assert_eq!(mask(&mut d, clip)["refine"]["invert"], json!(true));

    // Track (both ways) with the fake segmenter: progress, then Done; the Program frame's
    // cache key moves so it re-renders with the matte
    let rev0 = d.harness.state().seq_revision();
    d.click("grade.magicMask.track");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        d.frames(2);
        let st = d.exec("grade.magicMask.status", json!({}));
        if st["tracks"].as_array().and_then(|t| t.last()).is_some_and(|t| t["state"] == "done") {
            break;
        }
        assert!(Instant::now() < deadline, "track did not finish: {st}");
        std::thread::sleep(Duration::from_millis(20));
    }
    d.frames(3);
    assert!(d.has("grade.magicMask.status"));
    let st = d.exec("grade.magicMask.status", json!({}));
    assert_eq!(st["tracks"].as_array().and_then(|t| t.last()).map(|t| t["used"].clone()), Some(json!("MobileSAM")), "{st}");
    let line = d.ok("ui.elements", json!({"prefix": "grade.magicMask.status"}));
    assert!(line.to_string().contains("MobileSAM"), "the status line names the model: {line}");
    let app = d.harness.state();
    assert_ne!(app.seq_revision(), rev0, "a finished track changes the frames' revision");
    assert!(
        filmcraft_engine::magic_mask::magic_matte(&app.session, filmcraft_project::ClipId(clip), node, filmcraft_time::Tick(0)).is_some(),
        "frame 0 tracked"
    );
    // the matte overlay draws without trouble
    d.click("grade.magicMask.overlay");
    assert!(d.harness.state().ui.magic_mask.overlay);
    d.frames(2);
    // Clear All, then Remove (the top of the tab back in view)
    d.call("ui.scroll", json!({"id": "grade.magicMask.overlay", "dy": 400.0}));
    d.frames(2);
    d.click("grade.magicMask.clearAll");
    assert!(mask(&mut d, clip)["strokes"].as_array().is_none_or(Vec::is_empty));
    d.click("grade.magicMask.remove");
    assert!(mask(&mut d, clip).is_null());
    assert!(d.has("grade.magicMask.add"), "the tab offers Add again");
}

#[test]
fn magic_mask_track_without_the_model_says_so() {
    let (mut d, clip, _) = setup(false);
    d.click("grade.magicMask.add");
    d.ok("ui.click", json!({"id": "program.picture", "fx": 0.5, "fy": 0.5}));
    d.frames(2);
    assert_eq!(mask(&mut d, clip)["strokes"].as_array().map(Vec::len), Some(1));
    d.click("grade.magicMask.trackForward");
    d.frames(2);
    let err = d.harness.state().ui.magic_mask.error.clone().map(|e| e.2).unwrap_or_default();
    assert!(err.contains("mobile_sam.pt"), "a plain error naming the model file: {err:?}");
    assert!(d.has("grade.magicMask.status"), "the error shows in the tab");
    assert!(d.exec("grade.magicMask.status", json!({}))["tracks"].as_array().is_none_or(Vec::is_empty), "no track was queued");
}

/// Review of G3.3: a running track re-renders only the frame being watched. With the playhead off
/// the tracked clip the sequence frames' revision stays put while it runs, and moves once when it
/// ends.
#[test]
fn magic_mask_track_off_the_playhead_leaves_the_frames_alone() {
    let (mut d, clip, node) = setup_with(Some(Duration::from_millis(25)));
    d.click("grade.magicMask.add");
    d.ok("ui.click", json!({"id": "program.picture", "fx": 0.25, "fy": 0.5}));
    d.frames(2);
    // the playhead past the clip's end (the sequence's end)
    d.exec("playhead.set", json!({"seconds": 10}));
    d.frames(2);
    let rev0 = d.harness.state().seq_revision();
    d.exec("grade.magicMask.track", json!({"clip": clip, "node": node}));
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut samples = 0;
    loop {
        d.frames(1);
        if d.harness.state().session.magic_mask.running().is_empty() {
            break;
        }
        assert_eq!(d.harness.state().seq_revision(), rev0, "a track of a clip off the playhead re-rendered the frames");
        samples += 1;
        assert!(Instant::now() < deadline, "track did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(samples > 3, "the track ran while sampled ({samples})");
    d.frames(2);
    assert_ne!(d.harness.state().seq_revision(), rev0, "the end of the track moves the revision once");
}

/// The model selector (fork G3): each option sets the session's model (`tracker` in the status),
/// and Auto names what it picks (MobileSAM here: no SAM 2.1 with a test segmenter).
#[test]
fn magic_mask_model_selector_sets_the_engine_model() {
    let (mut d, _clip, _node) = setup(true);
    d.click("grade.magicMask.add");
    for (id, want) in [("mobileSam", "mobileSam"), ("faster", "faster"), ("better", "better"), ("auto", "auto")] {
        let full = format!("grade.magicMask.model.{id}");
        assert!(d.has(&full), "{full} is registered");
        d.click(&full);
        let st = d.exec("grade.magicMask.status", json!({}));
        assert_eq!(st["tracker"], json!(want), "{full}: {st}");
        // remembered in the preferences (LO.6)
        let saved = serde_json::to_value(d.harness.state().session.prefs.color.magic_mask).unwrap();
        assert_eq!(saved, json!(want), "{full}");
    }
    let auto = d.ok("ui.elements", json!({"prefix": "grade.magicMask.model.auto"}));
    assert!(auto.to_string().contains("Auto (MobileSAM)"), "Auto shows what it resolves to: {auto}");
}
