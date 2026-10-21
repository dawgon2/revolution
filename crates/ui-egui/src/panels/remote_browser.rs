//! File ▸ Remote Locations… (STORAGE-PLAN S2 items 3, 4, 6): browse a server saved in Settings ▸
//! Remote Locations, and from there Import media, Open a project, Receive a package, or Save
//! the open project here. Listings are fetched on a thread (a slow server never freezes the
//! window); the actions are the `remote.*` commands.
//!
//! Automation ids: `remote.location` (+ `.<id>` items), `remote.up`, `remote.refresh`,
//! `remote.entry.<path>`, `remote.import`, `remote.open`, `remote.receive`, `remote.saveName`,
//! `remote.saveHere`, `remote.close`.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use serde_json::json;

use crate::FilmcraftApp;

#[derive(Clone, Debug, Default)]
struct Draft {
    location: String,
    path: String,
    selected: BTreeSet<String>,
    save_name: String,
    message: String,
    /// `remote.receiveProject` jobs to import once downloaded: (job id, local package).
    receiving: Vec<(u64, String)>,
}

/// One folder listing: `None` while it loads.
pub(crate) type Listing = Option<Result<Vec<filmcraft_remote::Entry>, String>>;

pub(crate) fn listings() -> MutexGuard<'static, HashMap<(String, String), Listing>> {
    static L: OnceLock<Mutex<HashMap<(String, String), Listing>>> = OnceLock::new();
    L.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner)
}

fn draft_id() -> egui::Id {
    egui::Id::new("remote-browser-draft")
}

/// Opens the window (on the first saved location, or the one asked for).
pub fn open(app: &mut FilmcraftApp, ctx: &egui::Context, location: Option<&str>) -> Result<serde_json::Value, String> {
    let first = app.session.prefs.remote.locations.first().map(|l| l.id.clone());
    let Some(id) = location.map(str::to_string).or(first) else {
        return Err(tl!("Add a server in Settings ▸ Remote Locations first.").to_string());
    };
    if app.session.prefs.remote.find(&id).is_none() {
        return Err(format!("no remote location {id:?}"));
    }
    let d = Draft { location: id.clone(), ..Default::default() };
    ctx.data_mut(|m| m.insert_temp(draft_id(), Some(d)));
    refresh(app, ctx, &id, "");
    Ok(json!({"open": true, "location": id}))
}

pub(crate) fn refresh(app: &FilmcraftApp, ctx: &egui::Context, id: &str, path: &str) {
    let Some(loc) = app.session.prefs.remote.find(id).cloned() else { return };
    let key = (id.to_string(), path.to_string());
    listings().insert(key.clone(), None);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(|| {
            let dir = filmcraft_remote::RemotePath::parse(&key.1).map_err(|e| e.to_string())?;
            let store = filmcraft_engine::remote::connect(&loc)?;
            let mut entries = store.list(&dir).map_err(|e| e.to_string())?;
            entries.retain(|e| !e.name().starts_with('.'));
            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name().to_lowercase().cmp(&b.name().to_lowercase())));
            Ok(entries)
        })
        .unwrap_or_else(|_| Err("the listing stopped unexpectedly".into()));
        listings().insert(key, Some(r));
        ctx.request_repaint();
    });
}

pub(crate) fn job_result(app: &FilmcraftApp, id: u64) -> Option<Result<(), String>> {
    let job = app.session.jobs.iter().find(|j| j.id == id)?;
    let r = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
    Some(r.map(|_| ()))
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(Some(mut d)) = ctx.data(|m| m.get_temp::<Option<Draft>>(draft_id())) else { return };
    // finished downloads of received packages are imported (checked against their manifests)
    let mut still = Vec::new();
    for (job, local) in std::mem::take(&mut d.receiving) {
        match job_result(app, job) {
            None => still.push((job, local)),
            Some(Ok(())) => {
                d.message = match app.session.execute("projects.importPackage", json!({"path": local})) {
                    Ok(_) => tl!("Received: the project is in the Project Manager.").to_string(),
                    Err(e) => e.to_string(),
                };
            }
            Some(Err(e)) => d.message = e,
        }
    }
    d.receiving = still;
    if !d.receiving.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    let locations = app.session.prefs.remote.locations.clone();
    let mut open = true;
    let mut navigate: Option<String> = None;
    let mut action: Option<&str> = None;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new(tl!("Remote Locations"))
        .id(egui::Id::new("Remote Locations"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(620.0);
            ui.horizontal(|ui| {
                let cur = locations.iter().find(|l| l.id == d.location).map(|l| l.name.clone()).unwrap_or_default();
                let r = egui::ComboBox::from_id_salt("remote-location").selected_text(cur.clone()).width(240.0).show_ui(ui, |ui| {
                    for l in &locations {
                        let it = ui.selectable_label(l.id == d.location, &l.name);
                        elems.push((format!("remote.location.{}", l.id), it.rect, l.name.clone()));
                        if it.clicked() && l.id != d.location {
                            d.location = l.id.clone();
                            navigate = Some(String::new());
                        }
                    }
                });
                elems.push(("remote.location".into(), r.response.rect, cur));
                let up = ui.add_enabled(!d.path.is_empty(), egui::Button::new(tl!("Up")));
                elems.push(("remote.up".into(), up.rect, "Up".into()));
                if up.clicked() {
                    navigate = Some(d.path.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_default());
                }
                let rf = ui.button(tl!("Refresh"));
                elems.push(("remote.refresh".into(), rf.rect, "Refresh".into()));
                if rf.clicked() {
                    navigate = Some(d.path.clone());
                }
                ui.label(format!("/{}", d.path));
            });
            ui.separator();
            let listing = listings().get(&(d.location.clone(), d.path.clone())).cloned().flatten();
            egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, false]).show(ui, |ui| match listing {
                None => {
                    ui.label(tl!("Loading…"));
                }
                Some(Err(e)) => {
                    ui.label(tlf!("Couldn't list the folder: {error}", error = e));
                }
                Some(Ok(entries)) if entries.is_empty() => {
                    ui.label(tl!("This folder is empty."));
                }
                Some(Ok(entries)) => {
                    for e in entries {
                        let path = e.path.as_str().to_string();
                        let label = if e.is_dir { format!("📁 {}", e.name()) } else { format!("{}   {}", e.name(), human(e.size)) };
                        let sel = d.selected.contains(&path);
                        let r = ui.selectable_label(sel, label);
                        elems.push((format!("remote.entry.{path}"), r.rect, e.name().to_string()));
                        if r.double_clicked() && e.is_dir || (r.clicked() && e.is_dir) {
                            navigate = Some(path);
                        } else if r.clicked() {
                            if !ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                                d.selected.clear();
                            }
                            if sel {
                                d.selected.remove(&path);
                            } else {
                                d.selected.insert(path);
                            }
                        }
                    }
                }
            });
            ui.separator();
            let sel: Vec<String> = d.selected.iter().cloned().collect();
            let one = (sel.len() == 1).then(|| sel.first().cloned()).flatten();
            let is_project = one.as_deref().is_some_and(|p| filmcraft_engine::projects::is_project_file(std::path::Path::new(p)));
            let is_package = one.as_deref().is_some_and(|p| filmcraft_engine::package::is_package_file(std::path::Path::new(p)));
            let media = !sel.is_empty() && !is_project && !is_package;
            ui.horizontal(|ui| {
                for (id, label, on) in
                    [("import", tl!("Import"), media), ("open", tl!("Open Project"), is_project), ("receive", tl!("Receive Package"), is_package)]
                {
                    let b = ui.add_enabled(on, egui::Button::new(label));
                    elems.push((format!("remote.{id}"), b.rect, label.to_string()));
                    if b.clicked() {
                        action = Some(id);
                    }
                }
            });
            ui.horizontal(|ui| {
                let r = ui.add_sized([280.0, 22.0], egui::TextEdit::singleline(&mut d.save_name).hint_text(tl!("Project name")));
                elems.push(("remote.saveName".into(), r.rect, d.save_name.clone()));
                let b = ui.add_enabled(!d.save_name.trim().is_empty(), egui::Button::new(tl!("Save Project Here")));
                elems.push(("remote.saveHere".into(), b.rect, "Save Project Here".into()));
                if b.clicked() {
                    action = Some("save");
                }
            });
            if !d.message.is_empty() {
                ui.label(egui::RichText::new(&d.message).size(12.0));
            }
        });
    for (id, rect, label) in elems {
        app.auto.add(&id, rect, &label);
    }
    let loc = d.location.clone();
    match action {
        Some("import") => {
            let paths: Vec<String> = d.selected.iter().cloned().collect();
            d.message = match app.session.execute("remote.import", json!({"id": loc, "paths": paths})) {
                Ok(v) => {
                    // the job downloads; the files are imported when it is done
                    let locals = v["localPaths"].clone();
                    if let Some(job) = v["job"].as_u64() {
                        app.pending_remote_imports.push((job, locals));
                    }
                    tl!("Downloading into the media cache: the clips appear in the project when done.").to_string()
                }
                Err(e) => e.to_string(),
            };
        }
        Some("open") => {
            if let Some(path) = d.selected.iter().next().cloned() {
                d.message = match app.session.execute("remote.openProject", json!({"id": loc, "path": path})) {
                    Ok(v) if v["readOnly"] == true => {
                        let who = format!("{} ({})", v["heldBy"]["holder"].as_str().unwrap_or("someone"), v["heldBy"]["machine"].as_str().unwrap_or("?"));
                        open = false;
                        app.ui.status = tlf!("Opened read-only: {who} has this project open. Save As to keep your changes.", who = who);
                        String::new()
                    }
                    Ok(_) => {
                        open = false;
                        String::new()
                    }
                    Err(e) => e.to_string(),
                };
            }
        }
        Some("receive") => {
            if let Some(path) = d.selected.iter().next().cloned() {
                d.message = match app.session.execute("remote.receiveProject", json!({"id": loc, "path": path})) {
                    Ok(v) => {
                        if let (Some(job), Some(local)) = (v["job"].as_u64(), v["localPackage"].as_str()) {
                            d.receiving.push((job, local.to_string()));
                        }
                        tl!("Downloading the package…").to_string()
                    }
                    Err(e) => e.to_string(),
                };
            }
        }
        Some("save") => {
            let name = d.save_name.trim().trim_end_matches(".fcproj").to_string();
            let path = if d.path.is_empty() { format!("{name}.fcproj") } else { format!("{}/{name}.fcproj", d.path) };
            d.message = match app.session.execute("remote.saveProjectAs", json!({"id": loc, "path": path})) {
                Ok(_) => {
                    navigate = Some(d.path.clone());
                    tlf!("Saved to {path}.", path = path)
                }
                Err(e) => e.to_string(),
            };
        }
        _ => {}
    }
    if let Some(p) = navigate {
        d.path = p;
        d.selected.clear();
        refresh(app, ctx, &d.location.clone(), &d.path.clone());
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        open = false;
    }
    ctx.data_mut(|m| m.insert_temp(draft_id(), open.then_some(d)));
}

pub(crate) fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).ceil())
    }
}

/// Imports the clips of finished `remote.import` jobs (called every frame).
pub fn settle_imports(app: &mut FilmcraftApp) {
    if app.pending_remote_imports.is_empty() {
        return;
    }
    let mut still = Vec::new();
    for (job, locals) in std::mem::take(&mut app.pending_remote_imports) {
        match job_result(app, job) {
            None => still.push((job, locals)),
            Some(Ok(())) => {
                app.ui.status = match app.session.execute("file.import", json!({"paths": locals})) {
                    Ok(_) => tl!("Imported from the remote location.").to_string(),
                    Err(e) => e.to_string(),
                };
            }
            Some(Err(e)) => app.ui.status = e,
        }
    }
    app.pending_remote_imports = still;
}
