//! Settings ▸ General ▸ Full screen (fork, owner 2026-10-13): on by default; the window's
//! full-screen button, F11, View ▸ Full Screen and `window.toggleFullScreen` all switch the same
//! setting, and the menu item shows it. In full screen and on the Project Manager only the
//! full-screen and minimise buttons show. The FilmCraft menu (Preferences…, Quit FilmCraft) sits
//! left of File. Both workflows.

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
    fn new(workflow: Workflow) -> Self {
        Self::with(workflow, true)
    }

    fn with(workflow: Workflow, demo: bool) -> Self {
        let mut s = Session::default();
        s.prefs.workflow = workflow;
        if demo {
            s.execute("file.openDemoProject", json!({})).unwrap();
        }
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
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

    fn shows(&self, id: &str) -> bool {
        self.harness.state().auto.previous.iter().any(|e| e.id == id)
    }

    fn full_screen(&self) -> bool {
        self.harness.state().session.prefs.general.full_screen
    }

    fn menu_checked(&self) -> Option<bool> {
        let items = filmcraft_ui_egui::menus::menu_items(self.harness.state());
        let it = items.iter().find(|i| i.id == "window.toggleFullScreen").expect("View ▸ Full Screen");
        assert_eq!(it.path, vec!["View".to_string()]);
        it.checked
    }
}

fn switches(workflow: Workflow) {
    let mut d = Driver::new(workflow);
    assert!(d.full_screen(), "{workflow:?}: on by default");
    assert_eq!(d.menu_checked(), Some(true));

    let r = d.ok("engine.execute", json!({"command": "window.toggleFullScreen", "params": {}}));
    assert_eq!(r["fullScreen"], json!(false), "{workflow:?}: {r}");
    assert!(!d.full_screen());
    assert_eq!(d.menu_checked(), Some(false));

    d.ok("ui.key", json!({"key": "F11"}));
    d.frames(3);
    assert!(d.full_screen(), "{workflow:?}: F11 switches it back on");

    // `on` sets instead of switching
    d.ok("engine.execute", json!({"command": "window.toggleFullScreen", "params": {"on": true}}));
    assert!(d.full_screen());

    // the window button (FilmCraft draws the title bar on Linux / Windows)
    if filmcraft_ui_egui::window_chrome::SUPPORTED {
        d.ok("ui.click", json!({"id": "window.fullScreen"}));
        d.frames(2);
        assert!(!d.full_screen(), "{workflow:?}: the window's full-screen button switches it");
    }
}

#[test]
fn full_screen_switches_in_premiere() {
    switches(Workflow::Premiere);
}

#[test]
fn full_screen_switches_in_resolve() {
    switches(Workflow::Resolve);
}

#[test]
fn old_preferences_open_full_screen() {
    // a preferences file from before the setting has no `fullScreen`: it reads as on
    let p: filmcraft_engine::autosave::Preferences = serde_json::from_value(json!({"general": {"titleBar": "system"}})).expect("old preferences");
    assert!(p.general.full_screen);
    assert_eq!(p.general.title_bar, "system");
}

fn project_manager_buttons_and_x(workflow: Workflow) {
    if !filmcraft_ui_egui::window_chrome::SUPPORTED {
        return;
    }
    let mut d = Driver::with(workflow, false);
    // a normal window: all four buttons
    d.ok("ui.set", json!({"home": {"open": false}}));
    d.frames(2);
    for id in ["window.fullScreen", "window.minimize", "window.maximize", "window.close"] {
        assert!(d.shows(id), "{workflow:?}: {id} in a normal window");
    }
    // the Project Manager: full screen and minimise only
    d.ok("engine.execute", json!({"command": "projects.show", "params": {}}));
    d.frames(2);
    assert!(d.harness.state().ui.home.open);
    assert!(d.shows("window.fullScreen") && d.shows("window.minimize"), "{workflow:?}");
    assert!(!d.shows("window.maximize") && !d.shows("window.close"), "{workflow:?}: no maximise / close on the Project Manager");
    // its × quits FilmCraft (runs `app.quit`, which asks about unsaved changes first)
    let x = d.harness.state().auto.previous.iter().find(|e| e.id == "projects.close.x").map(|e| e.label.clone());
    assert_eq!(x.as_deref(), Some("Quit Revolution"), "{workflow:?}");
}

#[test]
fn project_manager_in_premiere() {
    project_manager_buttons_and_x(Workflow::Premiere);
}

#[test]
fn project_manager_in_resolve() {
    project_manager_buttons_and_x(Workflow::Resolve);
}

fn filmcraft_menu(workflow: Workflow) {
    let mut d = Driver::new(workflow);
    let items = filmcraft_ui_egui::menus::menu_items(d.harness.state());
    let app: Vec<&str> = items.iter().filter(|i| i.path == vec!["Revolution".to_string()]).map(|i| i.id.as_str()).collect();
    assert_eq!(app, ["app.preferences", "app.quit"], "{workflow:?}");
    d.ok("engine.execute", json!({"command": "app.preferences", "params": {}}));
    d.frames(2);
    assert!(d.harness.state().ui.settings.is_some(), "{workflow:?}: Preferences… opens Settings");
    if d.harness.state().in_window_menu() {
        assert!(d.shows("menu.Revolution"), "{workflow:?}: the Revolution menu left of File");
    }
}

#[test]
fn filmcraft_menu_in_premiere() {
    filmcraft_menu(Workflow::Premiere);
}

#[test]
fn filmcraft_menu_in_resolve() {
    filmcraft_menu(Workflow::Resolve);
}

fn project_manager_remembers_its_own(workflow: Workflow) {
    let mut d = Driver::new(workflow);
    let general = |d: &Driver| {
        let g = &d.harness.state().session.prefs.general;
        (g.full_screen, g.project_manager_full_screen)
    };
    assert_eq!(general(&d), (true, false), "{workflow:?}: editor full screen, Project Manager in a window");
    d.ok("engine.execute", json!({"command": "projects.show", "params": {}}));
    d.frames(2);
    assert_eq!(d.menu_checked(), Some(false), "{workflow:?}: the Project Manager's own state");
    assert!(!filmcraft_ui_egui::window_chrome::wanted_full_screen(d.harness.state()));
    d.ok("engine.execute", json!({"command": "window.toggleFullScreen", "params": {"on": true}}));
    assert_eq!(general(&d), (true, true), "{workflow:?}: only the Project Manager's state changed");
    d.ok("engine.execute", json!({"command": "window.toggleFullScreen", "params": {"on": false}}));
    assert_eq!(general(&d), (true, false));
    // opening a project (here: back to the open one) brings the editor's state back
    d.ok("engine.execute", json!({"command": "projects.close", "params": {}}));
    d.frames(2);
    assert!(!d.harness.state().ui.home.open);
    assert!(filmcraft_ui_egui::window_chrome::wanted_full_screen(d.harness.state()), "{workflow:?}: the editor is full screen again");
    // closing the project goes back to the Project Manager, as in Resolve
    d.ok("engine.execute", json!({"command": "file.closeProject", "params": {"force": true}}));
    d.frames(2);
    assert!(d.harness.state().ui.home.open, "{workflow:?}: Close Project shows the Project Manager");
}

#[test]
fn project_manager_full_screen_in_premiere() {
    project_manager_remembers_its_own(Workflow::Premiere);
}

#[test]
fn project_manager_full_screen_in_resolve() {
    project_manager_remembers_its_own(Workflow::Resolve);
}

/// Fork §1a.108 (owner): the app opens into the Project Manager in a plain window (not full
/// screen even when Project Manager full screen was on); the editor's size and maximised state
/// come back when a project opens, and only the editor's window is remembered for the next start.
#[test]
fn project_manager_opens_windowed_and_the_editor_gets_its_window_back() {
    let mut d = Driver::new(Workflow::Resolve);
    d.harness.state_mut().session.prefs.general.project_manager_full_screen = true;
    let editor = filmcraft_engine::window_state::WindowState { inner_size: [1500.0, 900.0], maximized: true };
    d.harness.state_mut().launch_project_manager_windowed(editor);
    d.ok("engine.execute", json!({"command": "projects.show", "params": {}}));
    d.frames(2);
    assert!(d.harness.state().ui.home.open);
    assert!(!d.harness.state().session.prefs.general.project_manager_full_screen, "not full screen at launch");
    assert!(!filmcraft_ui_egui::window_chrome::wanted_full_screen(d.harness.state()));
    assert!(!d.harness.state().window_is_editors(), "the Project Manager's window is not remembered");
    assert_eq!(d.harness.state().editor_window, Some(editor), "kept until a project opens");
    d.ok("engine.execute", json!({"command": "projects.close", "params": {}}));
    d.frames(2);
    assert!(!d.harness.state().ui.home.open);
    assert_eq!(d.harness.state().editor_window, None, "given back once");
    assert!(d.harness.state().editor_window_restored.is_some());
    assert!(!d.harness.state().window_is_editors(), "not while the window settles");
    assert!(filmcraft_ui_egui::window_chrome::wanted_full_screen(d.harness.state()), "the editor's full screen comes back");
}
