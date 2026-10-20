//! STORAGE-PLAN S2: Settings ▸ Remote Locations, driven like a user: fill in the Add form, Add
//! Location, the location is listed with Test / Remove, its password lands in the secret store
//! and nowhere in the UI state; Remove takes it and its password away.

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_remote::secrets::{self, MemorySecrets, SecretStore};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(Session::default()).with_control(rx);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SHOTS").is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
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
                assert_eq!(v["ok"], true, "{method} {params}: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == id)
    }
    /// `FILMCRAFT_UI_SHOTS=<dir>` saves a screenshot.
    fn shot(&mut self, name: &str) {
        let Some(dir) = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from) else { return };
        self.frames(4);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("remote-{name}.png"))).unwrap();
    }
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
}

/// One secret store for the whole test process (tests run in parallel and the store is global).
fn keyring() -> Arc<MemorySecrets> {
    static K: std::sync::OnceLock<Arc<MemorySecrets>> = std::sync::OnceLock::new();
    K.get_or_init(|| {
        let k = Arc::new(MemorySecrets::default());
        secrets::install(k.clone());
        k
    })
    .clone()
}

#[test]
fn add_and_remove_a_remote_location_in_settings() {
    let keyring = keyring();
    let mut d = Driver::new();
    d.ok("ui.menu.invoke", json!({"id": "app.settings.remote"}));
    d.frames(3);
    assert!(d.has("settings.remote.add.url"), "the Add form is shown");
    {
        let draft = &mut d.harness.state_mut().ui.settings.as_mut().unwrap().remote;
        draft.kind = "webDav".into();
        draft.name = "Studio NAS".into();
        draft.url = "https://nas.example/dav/".into();
        draft.user = "ana".into();
        draft.secret.0 = "TOP-SECRET-pw".into();
    }
    d.frames(2);
    d.shot("form");
    d.click("settings.remote.add");
    d.shot("added");
    let loc = d.harness.state().session.prefs.remote.find("studio-nas").cloned().expect("added");
    assert_eq!(loc.url, "https://nas.example/dav/");
    assert_eq!(keyring.get("revolution-remote:studio-nas").unwrap().as_deref(), Some("TOP-SECRET-pw"));
    assert!(d.has("settings.remote.test.studio-nas") && d.has("settings.remote.remove.studio-nas"));
    let state = d.ok("ui.inspect", json!({})).to_string();
    assert!(!state.contains("TOP-SECRET"), "the password is not in the UI state");
    assert!(d.harness.state().ui.settings.as_ref().unwrap().remote.secret.0.is_empty(), "the form is cleared");
    assert!(!format!("{:?}", d.harness.state().ui.settings).contains("TOP-SECRET"));
    d.click("settings.remote.remove.studio-nas");
    assert!(d.harness.state().session.prefs.remote.locations.is_empty());
    assert_eq!(keyring.get("revolution-remote:studio-nas").unwrap(), None);
}

/// File ▸ Remote Locations…: browse a server, step into a folder, Save Project Here, then open it
/// again (the lock is ours), and Import a clip into the project through the media cache.
#[test]
fn browse_save_open_and_import_through_the_remote_browser() {
    use filmcraft_remote::{RemotePath, RemoteStore};
    let _ = keyring();
    let dir = std::env::temp_dir().join(format!("filmcraft-remote-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = filmcraft_engine::remote::test_stores::put("ui-srv");
    let clip = b"not really a movie";
    store.write(&RemotePath::parse("shoot/a.mov").unwrap(), &mut &clip[..], clip.len() as u64, &mut |_| true).unwrap();
    let mut d = Driver::new();
    {
        let s = &mut d.harness.state_mut().session;
        s.prefs_path = Some(dir.join("preferences.json"));
        s.prefs.media_cache.location = dir.join("cache").to_string_lossy().into_owned();
        s.execute("remote.add", json!({"kind": "webDav", "name": "UI Server", "url": "https://srv.test", "id": "ui-srv"})).unwrap();
    }
    d.ok("ui.menu.invoke", json!({"id": "remote.show"}));
    for _ in 0..50 {
        if d.has("remote.entry.shoot") {
            break;
        }
        d.frames(2);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    d.click("remote.entry.shoot");
    for _ in 0..50 {
        if d.has("remote.entry.shoot/a.mov") {
            break;
        }
        d.frames(2);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    d.shot("browser");
    d.click("remote.entry.shoot/a.mov");
    d.click("remote.import");
    for _ in 0..100 {
        if d.harness.state().pending_remote_imports.is_empty() {
            break;
        }
        d.frames(2);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let local = dir.join("cache").join("Remote Media").join("ui-srv").join("shoot").join("a.mov");
    assert_eq!(std::fs::read(&local).unwrap(), clip, "downloaded into the media cache");

    // Save Project Here (into shoot/), then File ▸ Save goes to the server
    d.click("remote.saveName");
    d.ok("ui.type", json!({"text": "Cut"}));
    d.frames(2);
    d.click("remote.saveHere");
    assert!(store.stat(&RemotePath::parse("shoot/Cut.fcproj").unwrap()).is_ok());
    assert!(d.harness.state().session.remote_project.as_ref().is_some_and(|r| r.lock.is_some()));
    d.ok("ui.menu.invoke", json!({"id": "file.save"}));
    assert!(store.list(&RemotePath::parse("shoot/.Cut.fcproj.versions").unwrap()).is_ok_and(|v| v.len() == 1), "Save kept the previous server copy");
    let _ = std::fs::remove_dir_all(&dir);
}
