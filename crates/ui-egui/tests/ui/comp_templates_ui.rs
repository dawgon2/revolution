//! Fork C4.7: the Compositor canvas menu saves the selected nodes as a template, inserts a saved
//! template where the menu was opened, and deletes one after a confirmation (data folder in a
//! temporary directory).

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
    fn new(data: &std::path::Path) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.prefs_path = Some(data.join("prefs.json"));
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

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    /// Opens the canvas background menu near its bottom-left corner (away from the nodes).
    fn canvas_menu(&mut self) {
        self.ok("ui.click", json!({"id": "comp.canvas", "fx": 0.1, "fy": 0.9, "button": "right"}));
        self.frames(3);
    }

    fn node_count(&mut self, comp: u64) -> usize {
        self.exec("comp.get", json!({"comp": comp}))["nodes"].as_array().unwrap().len()
    }
}

#[test]
fn save_insert_and_delete_a_template_from_the_canvas_menu() {
    let data = std::env::temp_dir().join(format!("filmcraft-comp-templates-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    let file = data.join("templates/comp/Glow.fctemplate");
    let mut d = Driver::new(&data);
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Compositor"}));
    d.frames(3);
    d.click("comp.newClip");
    let comp = d.ok("ui.inspect", json!({}))["ui"]["panels"]["comp"]["comp"].as_u64().expect("a comp is shown");
    // two connected nodes near the top of the canvas, plus an item Media In the template leaves out
    let media = d.exec("comp.addNode", json!({"comp": comp, "kind": "mediaIn", "track": 0, "pos": [0, 0]}))["node"].as_u64().unwrap();
    let solid = d.exec("comp.addNode", json!({"comp": comp, "kind": "solid", "pos": [160, 0]}))["node"].as_u64().unwrap();
    let blur = d.exec("comp.addNode", json!({"comp": comp, "kind": "blur", "pos": [320, 0]}))["node"].as_u64().unwrap();
    d.exec("comp.connect", json!({"comp": comp, "from": solid, "to": blur}));

    // nothing selected: Save Selection as Template… is there but does nothing
    d.canvas_menu();
    assert!(d.has("comp.canvas.saveTemplate") && d.has("comp.canvas.insertTemplate"));
    d.click("comp.canvas.saveTemplate");
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["panels"]["comp"]["templates"]["save"], json!(null));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);

    // select, save through the dialog
    d.ok("ui.set", json!({"panels": {"comp": {"canvas": {"selection": [media, solid, blur]}}}}));
    d.frames(2);
    d.canvas_menu();
    d.click("comp.canvas.saveTemplate");
    assert!(d.has("comp.templateDialog.name"), "the name dialog is open");
    d.click("comp.templateDialog.name");
    d.ok("ui.type", json!({"text": "Glow"}));
    d.frames(2);
    d.click("comp.templateDialog.save");
    assert!(file.is_file(), "saved to {}", file.display());
    assert!(!d.has("comp.templateDialog.name"), "the dialog closed");
    let saved = d.exec("comp.template.list", json!({}));
    // a track Media In is kept (it doesn't refer to a project item)
    assert_eq!(saved["templates"][0]["nodes"], json!(3), "{saved}");

    // insert where the menu was opened: one undo step, new nodes selected
    let before = d.node_count(comp);
    d.canvas_menu();
    d.click("comp.canvas.insertTemplate");
    assert!(d.has("comp.canvas.insertTemplate.Glow"), "the saved template is listed");
    d.click("comp.canvas.insertTemplate.Glow");
    assert_eq!(d.node_count(comp), before + 3);
    let sel = d.ok("ui.inspect", json!({}))["ui"]["panels"]["comp"]["canvas"]["selection"].clone();
    assert_eq!(sel.as_array().map(Vec::len), Some(3), "{sel}");
    d.exec("edit.undo", json!({}));
    assert_eq!(d.node_count(comp), before);

    // delete with a confirmation; Cancel keeps it
    for (confirm, gone) in [("comp.templateDialog.deleteCancel", false), ("comp.templateDialog.deleteConfirm", true)] {
        d.canvas_menu();
        d.click("comp.canvas.insertTemplate");
        d.click("comp.canvas.deleteTemplate");
        assert!(d.has("comp.canvas.deleteTemplate.Glow"));
        d.click("comp.canvas.deleteTemplate.Glow");
        assert!(d.has(confirm), "the confirmation is open");
        d.click(confirm);
        assert_eq!(!file.exists(), gone, "{confirm}");
    }
    d.canvas_menu();
    d.click("comp.canvas.insertTemplate");
    assert!(!d.has("comp.canvas.insertTemplate.Glow"), "the menu no longer lists it");
    let _ = std::fs::remove_dir_all(&data);
}
