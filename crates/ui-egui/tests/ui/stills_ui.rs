//! Headless UI test of the stills comparison: the wipe divider on the Program monitor, driven by
//! automation id through the control channel.

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
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).with_step_dt(1.0 / 60.0);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
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

#[test]
fn wipe_divider_appears_and_dragging_it_moves_the_position() {
    let mut d = Driver::demo();
    assert!(d.element("compare.divider").is_none(), "no comparison, no divider");
    let still = d.exec("stills.grab", json!({}))["still"].as_u64().expect("a still");
    d.exec("compare.set", json!({"still": still, "mode": "wipe", "position": 0.5}));
    d.frames(6);
    let r = d.element("compare.divider").expect("the divider is on the monitor");
    let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 60.0, "y": cy}, "steps": 6}));
    d.frames(3);
    let c = d.exec("compare.get", json!({}));
    assert!(c["position"].as_f64().unwrap() > 0.5, "dragging right moves the divider right: {c}");
    assert_eq!(c["mode"], json!("wipe"));
    // split view keeps the divider; off removes it
    d.exec("compare.set", json!({"mode": "split"}));
    d.frames(4);
    assert!(d.element("compare.divider").is_some());
    d.exec("compare.set", json!({"mode": "off"}));
    d.frames(4);
    assert!(d.element("compare.divider").is_none());
    d.exec("compare.set", json!({"still": still, "mode": "wipe", "vertical": true}));
    d.frames(4);
    let r = d.element("compare.divider").expect("vertical divider");
    assert!(r[2] > r[3], "a vertical comparison has a horizontal divider bar");
}

#[test]
fn gallery_click_selects_for_compare_and_double_click_applies_the_grade() {
    let mut d = Driver::demo();
    let seq = d.exec("sequence.inspect", json!({}));
    let items = seq["video"][0]["items"].as_array().unwrap().clone();
    let a = items[0]["clip"].as_u64().unwrap();
    let b = items[1]["clip"].as_u64().unwrap();
    // a graded still of the first clip
    d.exec("grade.addNode", json!({"clip": a}));
    let node = d.exec("grade.get", json!({"clip": a}))["grade"]["nodes"][0]["id"].as_u64().unwrap();
    d.exec("grade.setParam", json!({"clip": a, "node": node, "param": "saturation", "value": 0.25}));
    d.exec("timeline.select", json!({"clips": [a]}));
    d.ok("ui.menu.invoke", json!({"id": "window.workspace.colorgrading", "params": {}}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.gallery", "params": {}}));
    d.frames(4);
    let still = d.exec("stills.grab", json!({"clip": a}))["still"].as_u64().unwrap();
    d.frames(6);
    let id = format!("stills.item.{still}");
    let r = d.element(&id).unwrap_or_else(|| panic!("the gallery shows the still: {:?}", d.ids("stills")));
    let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.ok("ui.click", json!({"x": cx, "y": cy}));
    d.frames(3);
    let c = d.exec("compare.get", json!({}));
    assert_eq!((c["still"].clone(), c["mode"].clone()), (json!(still), json!("wipe")), "{c}");
    // double click applies the still's grade to the selected clip
    d.exec("timeline.select", json!({"clips": [b]}));
    d.frames(2);
    assert!(d.exec("grade.get", json!({"clip": b}))["grade"].is_null());
    // (the harness steps in 1/60 s; let the first click go stale so the next two are a pair)
    d.frames(60);
    d.ok("ui.click", json!({"x": cx, "y": cy, "count": 2}));
    d.frames(3);
    let g = d.exec("grade.get", json!({"clip": b}));
    assert_eq!(g["grade"]["nodes"].as_array().map(Vec::len), Some(1), "{g}");
}

#[derive(Debug)]
struct OsFile(std::path::PathBuf);

impl egui::DroppedFile for OsFile {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// A reference picture (REF.1) in a scratch folder.
fn reference_png(tag: &str) -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fc-stills-ui-{tag}-{}-{n:x}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{tag}.png"));
    image::RgbaImage::from_fn(320, 180, |x, _| image::Rgba([40, 90, 100 + (x % 60) as u8, 255])).save(&p).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn import_button_and_dropping_on_the_gallery_bring_in_reference_pictures() {
    let mut d = Driver::demo();
    d.ok("ui.menu.invoke", json!({"id": "window.workspace.colorgrading", "params": {}}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.gallery", "params": {}}));
    d.frames(4);
    let picked = reference_png("ref-button");
    d.app().hooks.pick_files = Some(Box::new(move |_| vec![picked.clone()]));
    assert!(d.element("stills.import").is_some(), "the Gallery has an Import… button: {:?}", d.ids("stills"));
    d.click("stills.import");
    d.frames(4);
    let list = d.exec("stills.list", json!({}));
    let rows = list["stills"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 1, "{list}");
    assert_eq!((rows[0]["reference"].clone(), rows[0]["label"].clone()), (json!(true), json!("ref-button")), "{list}");
    let id = format!("stills.item.{}", rows[0]["id"]);
    let r = d.element(&id).unwrap_or_else(|| panic!("the gallery shows the reference: {:?}", d.ids("stills")));
    // a picture dropped on the Gallery becomes another reference still (not a clip in the project)
    let clips_before = d.app().session.project.items.len();
    let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.harness.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(cx, cy)));
    d.frames(1);
    let dropped = reference_png("ref-drop");
    d.harness.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(dropped.into())));
    d.frames(1);
    d.harness.input_mut().dropped_files.clear();
    d.frames(2);
    let list = d.exec("stills.list", json!({}));
    assert_eq!(list["stills"].as_array().map(Vec::len), Some(2), "{list}");
    assert_eq!(list["stills"][1]["label"], json!("ref-drop"), "{list}");
    assert_eq!(d.app().session.project.items.len(), clips_before, "nothing imported as media");
    d.snapshot("gallery-reference-pictures", Some("stills"));
}
