//! Fork RS2: the Project Manager home screen, driven through the control channel: shown at
//! launch in the Resolve workflow and not in Premiere, the New Project flow (lands on the Edit
//! page), a garbage project file as a "can't read" tile, Esc / Close back to the open project,
//! the File menu names per workflow, and the poster written after a save.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the screen with wgpu and writes
//! `projects-*.png`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::projects::StartupAction;
use filmcraft_engine::settings::Workflow;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    dir: PathBuf,
    shots: Option<PathBuf>,
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("filmcraft-rs2-ui-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(d.join("data")).unwrap();
    d
}

/// A session in `workflow` with its projects folder and data in a temp dir.
fn session(dir: &Path, workflow: Workflow) -> Session {
    let mut s = Session::default();
    s.prefs.workflow = workflow;
    s.prefs.general.projects_folder = Some(dir.join("Projects"));
    s.prefs_path = Some(dir.join("data/preferences.json"));
    s.trash_dir = Some(dir.join("Trash"));
    s
}

impl Driver {
    /// Launch as the desktop app does with no project on the command line.
    fn launch(tag: &str, workflow: Workflow, prepare: impl FnOnce(&Path)) -> (Self, Option<StartupAction>) {
        let dir = tmp(tag);
        prepare(&dir);
        let mut s = session(&dir, workflow);
        let action = filmcraft_ui_egui::launch_project(&mut s, None, false, true);
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(s).with_control(rx);
        app.finish_launch(action, false);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(PathBuf::from);
        // 60 fps steps: the two clicks of a double-click (one synthetic event per frame) land
        // within egui's double-click delay (the default 0.25 s step puts them 0.5 s apart)
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, dir, shots };
        d.frames(4);
        (d, action)
    }
    fn root(&self) -> PathBuf {
        self.dir.join("Projects")
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
    fn element_now(&mut self, id: &str) -> Option<Value> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == id).cloned()
    }
    /// The element, waiting a few frames for it (the folder is listed on a background thread).
    fn element(&mut self, id: &str) -> Option<Value> {
        for _ in 0..200 {
            if let Some(e) = self.element_now(id) {
                return Some(e);
            }
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        None
    }
    fn home(&self) -> Value {
        serde_json::to_value(&self.harness.state().ui.home).unwrap()
    }
    fn open(&self) -> bool {
        self.harness.state().ui.home.open
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(4);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("projects-{name}.png"))).unwrap();
    }
}

#[test]
fn home_screen_at_launch_in_resolve_not_in_premiere() {
    let (mut d, action) = Driver::launch("launch-resolve", Workflow::Resolve, |_| {});
    assert_eq!(action, Some(StartupAction::ProjectManager));
    assert!(d.open(), "Resolve workflow: the Project Manager first");
    for id in [
        "projects.tile.new",
        "projects.new",
        "projects.open",
        "projects.close",
        "projects.import",
        "projects.package",
        "projects.importPackage",
        "projects.search",
        "projects.tab.local",
        "projects.crumb.root",
    ] {
        assert!(d.element(id).is_some(), "{id} on screen");
    }
    assert!(d.element_now("page.edit").is_none(), "no panels behind it");
    d.shot("empty");
    // Esc with no project open: it stays
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.open(), "nothing to go back to");

    let (d, action) = Driver::launch("launch-premiere", Workflow::Premiere, |_| {});
    assert_eq!(action, Some(StartupAction::Demo), "Premiere keeps today's start (the demo project)");
    assert!(!d.open());
    assert!(d.harness.state().session.active_sequence().is_some());
}

/// XPS 16 (owner): Play does nothing behind the Project Manager (it covers the monitors; the
/// sound played from a screen the user couldn't see); once it is closed, Play plays.
#[test]
fn play_waits_for_the_project_manager_to_close() {
    let (mut d, _) = Driver::launch("play-behind", Workflow::Resolve, |_| {});
    let made = d.exec("file.newSequence", json!({"name": "S", "width": 64, "height": 36, "fps": 25}));
    d.frames(2);
    assert!(d.harness.state().session.active_sequence().is_some(), "a sequence to play: {made}");
    if !d.open() {
        d.ok("ui.menu.invoke", json!({"command": "projects.show"}));
        d.frames(2);
    }
    assert!(d.open(), "the Project Manager is up");
    let r = d.ok("ui.playback", json!({"action": "play"}));
    assert_eq!(r["playing"], false, "nothing plays behind the Project Manager");
    assert!(d.harness.state().ui.status.contains("Project Manager"), "{}", d.harness.state().ui.status);
    d.click("projects.close");
    assert!(!d.open(), "closed");
    let r = d.ok("ui.playback", json!({"action": "play"}));
    assert_eq!(r["playing"], true, "plays once it is closed");
    d.ok("ui.playback", json!({"action": "stop"}));
}

#[test]
fn new_project_flow_lands_on_the_edit_page() {
    let (mut d, _) = Driver::launch("new", Workflow::Resolve, |_| {});
    d.click("projects.tile.new");
    assert_eq!(d.home()["dialog"]["kind"], "newProject");
    assert!(d.element("projects.dialog.name").is_some());
    d.ok("ui.set", json!({"home": {"dialog": {"kind": "newProject", "name": "Trip"}}}));
    d.frames(2);
    d.click("projects.dialog.ok");
    let path = d.root().join("Trip.fcproj");
    assert!(path.is_file(), "{:?}", d.home());
    assert!(!d.open(), "the new project is open");
    let app = d.harness.state();
    assert_eq!(app.session.path.as_deref(), Some(path.to_string_lossy().as_ref()));
    assert_eq!(app.ui.workspace, "Edit Page", "Edit page");
    assert_eq!(app.ui.mode, filmcraft_ui_egui::state::Mode::Edit);
    // back home (Shift+1 in the Resolve keymap): the tile is there, Esc returns to the project
    d.exec("shortcuts.loadPreset", json!({"name": "DaVinci Resolve Compatible"}));
    d.ok("ui.key", json!({"key": "Shift+1"}));
    d.frames(3);
    assert!(d.open(), "Shift+1 opens the Project Manager");
    let tile = d.element("projects.tile.0").expect("the Trip tile");
    assert_eq!(tile["label"], "Trip");
    // a second project with the same name is refused in the dialog, not overwritten
    d.click("projects.new");
    d.ok("ui.set", json!({"home": {"dialog": {"kind": "newProject", "name": "Trip"}}}));
    d.frames(2);
    d.click("projects.dialog.ok");
    let h = d.home();
    assert!(h["dialog"]["error"].as_str().is_some_and(|e| e.contains("already exists")), "{h}");
    d.click("projects.dialog.cancel");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(!d.open(), "Esc returns to the open project");
}

#[test]
fn garbage_project_is_a_cant_read_tile() {
    let (mut d, _) = Driver::launch("garbage", Workflow::Resolve, |dir| {
        let root = dir.join("Projects");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Broken.fcproj"), [0xffu8, 0x00, b'{', 0x80]).unwrap();
        std::fs::write(root.join("Truncated.fcproj"), br#"{"format":"filmcraft.project","schema_version":16,"proj"#).unwrap();
        std::fs::write(root.join("Empty.fcproj"), b"").unwrap();
        std::fs::write(root.join("Wrong.fcproj"), br#"[1e999, {"name": 7}, null]"#).unwrap();
    });
    d.frames(2);
    let listing = d.exec("projects.list", json!({}));
    let projects = listing["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 4);
    assert!(projects.iter().all(|p| p["error"].is_string()), "{listing}");
    assert_eq!(d.element("projects.tile.0").unwrap()["label"], "Broken");
    d.shot("garbage");
    // the list view and the info pane draw them too
    d.ok("ui.set", json!({"home": {"listView": true, "info": true, "selected": projects[1]["path"]}}));
    d.frames(3);
    assert!(d.element("projects.tile.3").is_some());
    d.ok("ui.set", json!({"home": {"listView": false, "info": false, "selected": null}}));
    d.frames(2);
    // double-click: nothing opens, the status says why
    d.ok("ui.click", json!({"id": "projects.tile.0", "count": 2}));
    d.frames(3);
    assert!(d.open());
    assert!(d.harness.state().session.path.is_none());
    assert!(d.harness.state().ui.status.contains("Can't read"), "{}", d.harness.state().ui.status);
    // selected, Open is refused too
    d.click("projects.tile.1");
    assert!(d.call("engine.execute", json!({"command": "projects.open", "params": {}}))["ok"] == false);
    assert!(d.harness.state().session.path.is_none());
}

#[test]
fn esc_and_close_return_to_the_open_project() {
    let (mut d, _) = Driver::launch("esc", Workflow::Premiere, |_| {});
    assert!(!d.open());
    d.exec("projects.show", json!({}));
    d.frames(3);
    assert!(d.open());
    assert!(d.element("projects.tile.new").is_some());
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(!d.open(), "Esc");
    d.exec("projects.show", json!({}));
    d.frames(3);
    d.click("projects.close");
    assert!(!d.open(), "Close");
    // the File menu names: Projects… in Premiere, Project Manager / Media Management… in Resolve
    let menu = d.ok("ui.menu.list", json!({}));
    let label = |m: &Value, id: &str| m.as_array().unwrap().iter().find(|i| i["id"] == id).map(|i| i["label"].clone()).unwrap();
    assert_eq!(label(&menu, "projects.show"), "Projects…");
    assert_eq!(label(&menu, "file.projectManager"), "Project Manager…");
    d.exec("settings.setWorkflow", json!({"workflow": "resolve"}));
    let menu = d.ok("ui.menu.list", json!({}));
    assert_eq!(label(&menu, "projects.show"), "Project Manager");
    assert_eq!(label(&menu, "file.projectManager"), "Media Management…");
    // the page bar's Home button opens it too
    d.frames(3);
    d.click("pageBar.home");
    assert!(d.open(), "page bar Home");
}

#[test]
fn saving_writes_a_poster_and_reopening_shows_it() {
    let (mut d, _) = Driver::launch("poster", Workflow::Premiere, |_| {});
    let path = d.root().join("Demo.fcproj");
    std::fs::create_dir_all(d.root()).unwrap();
    d.exec("file.saveAs", json!({"path": path.to_string_lossy()}));
    let cache = d.dir.join("data").join(filmcraft_engine::projects::CACHE_DIR);
    let png = cache.join(format!("{}.png", filmcraft_engine::projects::cache_key(&path)));
    for _ in 0..600 {
        if png.is_file() {
            break;
        }
        d.frames(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(png.is_file(), "a poster after the save");
    let (w, _, _) = filmcraft_engine::projects::load_poster(&png).expect("a readable poster");
    assert!(w <= 480);
    d.exec("projects.show", json!({}));
    d.frames(4);
    let listing = d.exec("projects.list", json!({}));
    assert_eq!(listing["projects"][0]["poster"], json!(png.to_string_lossy()));
    d.shot("poster");
    // open it from the tile (double-click): back to the project
    d.ok("ui.click", json!({"id": "projects.tile.0", "count": 2}));
    d.frames(3);
    assert!(!d.open());
    assert_eq!(d.harness.state().session.path.as_deref(), Some(path.to_string_lossy().as_ref()));
}

#[test]
fn a_projects_folder_at_the_top_of_the_disk_is_not_used() {
    let (mut d, _) = Driver::launch("broad-root", Workflow::Resolve, |_| {});
    d.harness.state_mut().session.prefs.general.projects_folder = Some(PathBuf::from("/"));
    d.harness.state_mut().session.prefs.general.recent_projects.clear();
    d.exec("projects.show", json!({}));
    d.frames(6);
    let listing = d.exec("projects.list", json!({}));
    assert!(listing["warning"].is_string(), "{listing}");
    assert_ne!(listing["root"], "/");
    assert!(d.element("projects.tile.new").is_some(), "the screen still draws");
    assert!(d.element_now("projects.folder.0").is_none_or(|f| !f["label"].as_str().is_some_and(|l| l == "home" || l == "etc")), "not the disk's folders");
}

/// Fork §1a.108: a project saved where no poster could be made (here: by another session, no
/// frontend) gets one when the Project Manager lists it, rendered from its timeline in the
/// background; until then its tile shows the faded logo.
#[test]
fn a_project_without_a_poster_gets_one_when_listed() {
    let (mut d, _) = Driver::launch("backfill", Workflow::Resolve, |dir| {
        let root = dir.join("Projects");
        std::fs::create_dir_all(&root).unwrap();
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.execute("file.saveAs", json!({"path": root.join("Old.fcproj").to_string_lossy()})).unwrap();
    });
    assert!(d.open());
    let path = d.root().join("Old.fcproj");
    let cache = d.dir.join("data").join(filmcraft_engine::projects::CACHE_DIR);
    let png = cache.join(format!("{}.png", filmcraft_engine::projects::cache_key(&path)));
    for _ in 0..3000 {
        if png.is_file() {
            break;
        }
        d.frames(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(png.is_file(), "a poster made for the listed project");
    let (w, _, px) = filmcraft_engine::projects::load_poster(&png).expect("a readable poster");
    assert!(w <= 480);
    assert!(px.as_chunks::<4>().0.iter().any(|p| p[..3].iter().any(|c| *c > 16)), "a frame with something on it");
    d.frames(30);
    let listing = d.exec("projects.list", json!({}));
    assert_eq!(listing["projects"][0]["poster"], json!(png.to_string_lossy()));
    // the selection border has room above the first row (the scroll area clipped it)
    let tile = d.element("projects.tile.new").unwrap();
    let top = tile["rect"][1].as_f64().unwrap();
    assert!(top >= 36.0 + 38.0 + 10.0 + 4.0 - 0.5, "the first row starts below the border room: {tile}");
    d.shot("backfill");
    d.click("projects.tile.0");
    d.shot("backfill-selected");
    // the editor's page bar carries the logo too
    d.ok("ui.click", json!({"id": "projects.tile.0", "count": 2}));
    d.frames(6);
    assert!(!d.open());
    assert!(d.element("pageBar.app").is_some());
    d.shot("editor");
}
