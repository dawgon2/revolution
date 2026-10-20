//! Fork UPD: the "Update available" window and Settings ▸ Updates, driven like a user against a
//! mock release server: Update installs beside the running build and offers Restart Now; Later
//! and Don't check close it; a build that can't update itself offers the release page; Settings
//! shows Check Now and Roll Back. `FILMCRAFT_UI_SHOTS=<dir>` saves screenshots.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::update::{Channel, Current, Fetch, Updater};
use filmcraft_engine::update_install::Layout;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

const CH: Channel = Channel { repo: "dawgon2/revolution", tag_prefix: "revolution-" };
const DL: &str = "https://github.com/dawgon2/revolution/releases/download/revolution-next-1/";
const TARBALL: &str = "revolution-next-1-linux-x86_64.tar.gz";

struct Server(HashMap<String, Vec<u8>>);

impl Fetch for Server {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
        let b = self.0.get(url).cloned().ok_or_else(|| format!("{url}: 404"))?;
        if b.len() as u64 > max_bytes { Err("too large".into()) } else { Ok(b) }
    }
}

/// A release `revolution-next-1` with a real tarball (made with the system `tar`) and its SHA256SUMS.
fn server(root: &Path) -> Server {
    let src = root.join("src");
    let top = src.join("revolution-next-1");
    std::fs::create_dir_all(&top).unwrap();
    std::fs::write(top.join("revolution"), b"#!/bin/sh\n").unwrap();
    std::fs::write(top.join("NOTES.md"), b"# next-1\n").unwrap();
    let tgz = root.join(TARBALL);
    let ok = std::process::Command::new("tar").arg("czf").arg(&tgz).arg("-C").arg(&src).arg("revolution-next-1").status().unwrap();
    assert!(ok.success());
    let out = std::process::Command::new("sha256sum").arg(&tgz).output().unwrap();
    let sha = String::from_utf8(out.stdout).unwrap().split_whitespace().next().unwrap().to_string();
    let bytes = std::fs::read(&tgz).unwrap();
    let list = json!([{
        "tag_name": "revolution-next-1", "name": "Revolution next-1", "draft": false, "prerelease": false,
        "published_at": "2026-10-20T10:00:00Z", "body": "- Faster export\n- A new title tool",
        "assets": [
            {"name": TARBALL, "size": bytes.len(), "browser_download_url": format!("{DL}{TARBALL}")},
            {"name": "SHA256SUMS", "size": 100, "browser_download_url": format!("{DL}SHA256SUMS")}
        ]
    }]);
    Server(HashMap::from([
        (CH.releases_url(), serde_json::to_vec(&list).unwrap()),
        (format!("{DL}{TARBALL}"), bytes),
        (format!("{DL}SHA256SUMS"), format!("{sha}  {TARBALL}\n").into_bytes()),
    ]))
}

/// A builds folder holding the running build `yt-3` and `latest` → it.
fn builds(root: &Path) -> Layout {
    let b = root.join("builds");
    let current = b.join("yt-3");
    std::fs::create_dir_all(&current).unwrap();
    std::fs::write(current.join("filmcraft"), b"old").unwrap();
    std::os::unix::fs::symlink(&current, b.join("latest")).unwrap();
    Layout { builds: b, current, appimage: None }
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<PathBuf>,
}

impl Driver {
    fn new(root: &Path, layout: Result<Layout, String>) -> Self {
        let mut session = Session::default();
        session.updater = Updater::new(Some(CH), Arc::new(server(root)), Current { tag: Some("fork-yt-3".into()), built: Some(1_760_000_000) });
        session.updater.set_layout(layout);
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
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
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == id)
    }
    fn wait_for(&mut self, id: &str) {
        for _ in 0..500 {
            if self.has(id) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{id} never appeared");
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(4);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("updates-{name}.png"))).unwrap();
    }
}

fn root(name: &str) -> PathBuf {
    let r = std::env::temp_dir().join(format!("filmcraft-updates-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&r);
    std::fs::create_dir_all(&r).unwrap();
    r
}

/// UPD accept: the prompt shows the notes and what the check sends; Update installs beside the
/// running build (`latest` switched, old build kept) and offers Restart Now; Settings ▸ Updates
/// shows the state, Check Now and Roll Back, which points `latest` back.
#[test]
fn update_installs_from_the_prompt_and_rolls_back_in_settings() {
    let r = root("install");
    let l = builds(&r);
    let mut d = Driver::new(&r, Ok(l.clone()));
    let v = d.exec("app.update.check", json!({"wait": true}));
    assert_eq!(v["state"], "available", "{v}");
    d.frames(3);
    for id in ["update.notes", "update.install", "update.later", "update.optOut"] {
        assert!(d.has(id), "{id} on screen");
    }
    d.shot("prompt");
    d.click("update.install");
    d.wait_for("update.restart");
    assert_eq!(std::fs::read_link(l.builds.join("latest")).unwrap(), l.builds.join("next-1"));
    assert_eq!(std::fs::read(l.current.join("filmcraft")).unwrap(), b"old", "the running build is kept");
    assert!(d.has("update.close"));
    d.shot("installed");
    // the first answer counts as having read what the check sends
    assert_eq!(d.exec("app.update.status", json!({}))["explained"], true);
    d.click("update.close");
    assert!(!d.has("update.restart"), "Later closes the installed window");
    d.ok("ui.menu.invoke", json!({"id": "app.settings.update"}));
    d.frames(3);
    let text = d.ok("ui.inspect", json!({})).to_string();
    assert!(text.contains("Installed next-1") || d.harness.state().session.updater.status.install == "installed");
    for id in ["settings.update.check", "settings.update.checkNow", "settings.update.rollback"] {
        assert!(d.has(id), "{id} in Settings ▸ Updates");
    }
    d.shot("settings");
    d.click("settings.update.rollback");
    assert_eq!(std::fs::read_link(l.builds.join("latest")).unwrap(), l.current);
}

/// Later closes the prompt until something newer comes; Don't check turns the daily check off.
#[test]
fn later_and_dont_check_close_the_prompt() {
    let r = root("later");
    let mut d = Driver::new(&r, Ok(builds(&r)));
    d.exec("app.update.check", json!({"wait": true}));
    d.frames(3);
    d.click("update.later");
    assert!(!d.has("update.install"));
    assert_eq!(d.exec("app.update.status", json!({}))["state"], "available");
    // the same release found again stays quiet
    d.exec("app.update.check", json!({"wait": true}));
    d.frames(3);
    assert!(!d.has("update.install"));
    let r2 = root("optout");
    let mut d = Driver::new(&r2, Ok(builds(&r2)));
    d.exec("app.update.check", json!({"wait": true}));
    d.frames(3);
    d.click("update.optOut");
    assert!(!d.has("update.install"));
    assert_eq!(d.exec("prefs.get", json!({"key": "update.check"})), json!(false));
}

/// A build that can't update itself offers the release page instead of Update.
#[test]
fn a_build_that_cant_update_itself_offers_the_release_page() {
    let r = root("manual");
    let mut d = Driver::new(&r, Err("this copy of Revolution is a Flatpak: update it with your software centre".into()));
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = opened.clone();
    d.harness.state_mut().hooks.open_url = Some(Box::new(move |u: &str| {
        seen.lock().unwrap().push(u.to_string());
        Ok(())
    }));
    d.exec("app.update.check", json!({"wait": true}));
    d.frames(3);
    assert!(!d.has("update.install") && d.has("update.releasePage"));
    d.shot("manual");
    d.click("update.releasePage");
    assert_eq!(opened.lock().unwrap().as_slice(), ["https://github.com/dawgon2/revolution/releases/latest"]);
}
