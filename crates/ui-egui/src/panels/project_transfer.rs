//! Send Project… / Receive Project… / Manage Locations… in the Project Manager (STORAGE-PLAN S2b
//! items 1–3, FORK-PLAN §1a.111). Send packages a project as one `.revpkg` and uploads it to a
//! saved remote location (`remote.sendProject`); Receive downloads one (`remote.receiveProject`)
//! and imports it relinked into the shown projects folder (`projects.importPackage`). Manage
//! Locations is the Settings ▸ Remote Locations widget in a window of its own (one widget, two
//! places). Folder listings come from the remote browser's background lister.
//!
//! Automation ids: `projects.send`, `projects.receive`, `projects.manageLocations` (the footer
//! buttons and the commands), `projects.menu.send`, `projects.sidebar.remote.<id>`,
//! `projects.sidebar.manageLocations`, and in the window `projects.transfer.location` (+ `.<id>`),
//! `projects.transfer.up`, `projects.transfer.entry.<path>`, `projects.transfer.proxies`,
//! `projects.transfer.trim`, `projects.transfer.ok`, `projects.transfer.cancel`,
//! `projects.transfer.manage`, `projects.transfer.close`.
//!
//! Nearby computer (S2b items 4–5, ADR 0008; the engine's `nearby.*` commands): in Send the
//! "To:" choice `projects.transfer.nearby`, the fields `projects.transfer.code` /
//! `projects.transfer.address`, `projects.transfer.find` and the found computers
//! `projects.transfer.receiver.<i>`; in Receive the button `projects.transfer.nearby`, the code
//! `projects.transfer.codeShown` and `projects.transfer.accept` / `projects.transfer.decline`.

use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::panels::remote_browser::{human, job_result, listings, refresh};
use crate::panels::settings::RemoteDraft;

/// The Send / Receive / Manage Locations window (serde: agents read and set it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TransferDialog {
    /// `send` | `receive` | `locations`.
    pub kind: String,
    /// Send: the project file.
    pub project: String,
    /// The remote location id.
    pub location: String,
    /// The remote folder shown ("" = the location's folder).
    pub folder: String,
    /// Receive: the picked `.revpkg` (its remote path).
    pub selected: String,
    pub proxies: bool,
    pub trim: bool,
    /// Send: the package size for these options (`None` until known / when it can't be told).
    pub estimate: Option<u64>,
    /// The options `estimate` was made for.
    #[serde(skip)]
    pub estimate_for: Option<(bool, bool)>,
    /// The running job (upload / download).
    pub job: Option<u64>,
    pub message: String,
    /// Send: to a nearby computer instead of a server. Receive: receiving from one.
    pub nearby: bool,
    /// Send: the typed code. Receive: the code to show.
    pub code: String,
    /// Send: the other computer's address, when it isn't found on the network.
    pub address: String,
    /// Receive: this computer's port (for the address fallback).
    pub port: u16,
    pub warnings: Vec<String>,
    /// Receive: who wants to send what, waiting for Accept / Decline.
    pub offer: Option<NearbyOffer>,
    /// Receive: a line from the receive ("Someone tried a wrong code…").
    pub note: String,
    /// Send: the computers waiting to receive (Find).
    pub receivers: Vec<NearbyReceiver>,
    /// Send: Find is looking.
    pub finding: bool,
    /// Tests on one machine: no network discovery (`"discovery": false`).
    pub no_discovery: bool,
}

/// An offer waiting for Accept / Decline (from `nearby.status`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NearbyOffer {
    pub computer: String,
    pub package: String,
    pub bytes: u64,
}

/// A computer waiting to receive (from `nearby.list`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NearbyReceiver {
    pub channel: u64,
    pub address: String,
}

/// A received package waiting for its download job, then imported into `folder`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PendingReceive {
    pub job: u64,
    pub local: String,
    pub folder: String,
    /// Set once the import started: its job (the tile is selected when it finishes).
    pub import: Option<u64>,
    pub imported_path: String,
    /// Nearby: the offered package's name (the import is named after it).
    pub name: String,
}

fn str_param<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn first_location(app: &FilmcraftApp) -> String {
    app.session.prefs.remote.locations.first().map(|l| l.id.clone()).unwrap_or_default()
}

fn project_name(path: &str) -> String {
    std::path::Path::new(path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn open(app: &mut FilmcraftApp, d: TransferDialog) -> Value {
    let kind = d.kind.clone();
    // list the server afresh (a send since the last look added a package)
    listings().remove(&(d.location.clone(), d.folder.clone()));
    app.ui.home.open = true;
    app.ui.home.transfer = Some(d);
    json!({"dialog": kind})
}

/// The `projects.send` / `projects.receive` / `projects.manageLocations` entry points. Without
/// the parameters a person would pick they open the window; with them they act. None for other ids.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let r = match id {
        "projects.manageLocations" => Ok(open(app, TransferDialog { kind: "locations".into(), ..Default::default() })),
        "projects.send" => {
            let project = str_param(params, "path")
                .map(str::to_string)
                .or_else(|| app.ui.home.selected.clone().filter(|p| filmcraft_engine::projects::is_project_file(std::path::Path::new(p))))
                .or_else(|| app.session.path.clone());
            let Some(project) = project else {
                return Some(Err(tl!("Select a project to send.").to_string()));
            };
            let proxies = params.get("proxies").and_then(Value::as_bool).unwrap_or(false);
            let trim = params.get("trim").and_then(Value::as_bool).unwrap_or(false);
            match str_param(params, "id") {
                Some(loc) => {
                    let folder = str_param(params, "folder").unwrap_or("").to_string();
                    let mut d = TransferDialog { kind: "send".into(), project, location: loc.into(), folder, proxies, trim, ..Default::default() };
                    start_send(app, &mut d).inspect(|_| app.ui.home.transfer = Some(d))
                }
                None => Ok(open(app, TransferDialog { kind: "send".into(), project, location: first_location(app), proxies, trim, ..Default::default() })),
            }
        }
        "projects.receive" => {
            let location = str_param(params, "id").map(str::to_string).unwrap_or_else(|| first_location(app));
            match str_param(params, "path") {
                Some(path) => {
                    let mut d = TransferDialog { kind: "receive".into(), location, selected: path.into(), ..Default::default() };
                    start_receive(app, &mut d).inspect(|_| app.ui.home.transfer = Some(d))
                }
                None => {
                    let folder = str_param(params, "folder").unwrap_or("").to_string();
                    Ok(open(app, TransferDialog { kind: "receive".into(), location, folder, ..Default::default() }))
                }
            }
        }
        _ => return None,
    };
    if let Err(e) = &r {
        app.ui.status = e.clone();
    }
    Some(r)
}

fn start_send(app: &mut FilmcraftApp, d: &mut TransferDialog) -> Result<Value, String> {
    if d.location.is_empty() {
        return Err(tl!("Add a server to send projects through it.").to_string());
    }
    let p = json!({"id": d.location, "path": d.project, "folder": d.folder, "proxies": d.proxies, "trim": d.trim});
    let v = app.session.execute("remote.sendProject", p).map_err(|e| e.to_string())?;
    d.job = v["job"].as_u64();
    d.message = tl!("Sending…").to_string();
    Ok(v)
}

fn start_receive(app: &mut FilmcraftApp, d: &mut TransferDialog) -> Result<Value, String> {
    let v = app.session.execute("remote.receiveProject", json!({"id": d.location, "path": d.selected})).map_err(|e| e.to_string())?;
    if let (Some(job), Some(local)) = (v["job"].as_u64(), v["localPackage"].as_str()) {
        d.job = Some(job);
        app.ui.home.receiving.push(PendingReceive { job, local: local.to_string(), folder: app.ui.home.folder.clone(), ..Default::default() });
    }
    d.message = tl!("Receiving…").to_string();
    Ok(v)
}

/// The finished job's report path (the nearby receive's package).
fn job_path(app: &FilmcraftApp, id: u64) -> Option<String> {
    let job = app.session.jobs.iter().find(|j| j.id == id)?;
    let r = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
    r.ok().map(|rep| rep.path)
}

/// The received package renamed to the offered name (its folder name on import), beside it in
/// the inbox. The name comes from the other computer: only its last part, no separators or
/// leading dots, at most 120 characters; `None` when there is nothing usable or the rename fails.
fn package_named(local: &str, offered: &str) -> Option<String> {
    let stem = std::path::Path::new(offered).file_name()?.to_string_lossy().into_owned();
    let stem = stem.strip_suffix(".revpkg").unwrap_or(&stem);
    let clean: String = stem.chars().filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).take(120).collect();
    let clean = clean.trim().trim_start_matches('.').trim();
    if clean.is_empty() {
        return None;
    }
    let from = std::path::Path::new(local);
    let dir = from.parent()?.join(format!("named-{}", from.file_stem()?.to_string_lossy()));
    std::fs::create_dir_all(&dir).ok()?;
    let to = dir.join(format!("{clean}.revpkg"));
    std::fs::rename(from, &to).ok()?;
    Some(to.to_string_lossy().into_owned())
}

/// Starts the import of a finished download (a nearby receive's `local` is its job's package).
fn import_received(app: &mut FilmcraftApp, r: &mut PendingReceive) -> filmcraft_engine::Result<Value> {
    if r.local.is_empty() {
        r.local = job_path(app, r.job).unwrap_or_default();
        if let Some(named) = package_named(&r.local, &r.name) {
            r.local = named;
        }
    }
    app.session.execute("projects.importPackage", json!({"path": r.local, "folder": r.folder}))
}

/// Imports finished downloads (checked against their manifests) and selects the new tile when
/// the import is done. Runs every frame, also with the Project Manager closed.
pub fn settle(app: &mut FilmcraftApp) {
    if app.ui.home.receiving.is_empty() {
        return;
    }
    let mut still = Vec::new();
    for mut r in std::mem::take(&mut app.ui.home.receiving) {
        let line = match r.import {
            None => match job_result(app, r.job) {
                None => {
                    still.push(r);
                    continue;
                }
                Some(Ok(())) => match import_received(app, &mut r) {
                    Ok(v) => {
                        r.imported_path = v["path"].as_str().unwrap_or_default().to_string();
                        match v["job"].as_u64() {
                            Some(job) => {
                                r.import = Some(job);
                                still.push(r);
                                continue;
                            }
                            None => received(app, &r.imported_path),
                        }
                    }
                    Err(e) => e.to_string(),
                },
                Some(Err(e)) => e,
            },
            Some(job) => match job_result(app, job) {
                None => {
                    still.push(r);
                    continue;
                }
                Some(Ok(())) => {
                    // imported: the downloaded package isn't needed any more
                    let _ = std::fs::remove_file(&r.local);
                    // and the folder a nearby package was renamed in (only when empty)
                    if let Some(dir) =
                        std::path::Path::new(&r.local).parent().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("named-")))
                    {
                        let _ = std::fs::remove_dir(dir);
                    }
                    received(app, &r.imported_path)
                }
                Some(Err(e)) => e,
            },
        };
        app.ui.status = line.clone();
        if let Some(d) = app.ui.home.transfer.as_mut().filter(|d| d.kind == "receive") {
            d.message = line;
            d.job = None;
            d.offer = None;
        }
    }
    app.ui.home.receiving = still;
}

fn received(app: &mut FilmcraftApp, path: &str) -> String {
    crate::panels::home::mark_stale(app);
    if !path.is_empty() {
        app.ui.home.selected = Some(path.to_string());
    }
    tlf!("Received: {name}", name = project_name(path))
}

/// Draws the window (called by the Project Manager every frame).
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.home.transfer.clone() else { return };
    let mut open = true;
    if d.kind == "locations" {
        manage_window(app, ctx, &mut open);
    } else {
        transfer_window(app, ctx, &mut d, &mut open);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        open = false;
    }
    // closing the window ends a nearby receive (its code is only valid while the window is open)
    if !open && d.kind == "receive" && d.nearby && d.job.is_some() {
        let _ = app.session.execute("nearby.stop", json!({}));
    }
    // only write back if nothing else replaced the window meanwhile
    if app.ui.home.transfer.as_ref().is_some_and(|x| x.kind == d.kind) {
        app.ui.home.transfer = open.then_some(d);
    }
}

fn manage_window(app: &mut FilmcraftApp, ctx: &egui::Context, open: &mut bool) {
    // the Add form's draft (with its password) lives only in memory, never in serde state
    let id = egui::Id::new("projects-manage-locations-draft");
    let (mut draft, mut message) = ctx.data(|m| m.get_temp::<(RemoteDraftHolder, String)>(id)).unwrap_or_default();
    let mut close = false;
    egui::Window::new(tl!("Manage Locations"))
        .id(egui::Id::new("projects-manage-locations"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(560.0);
            egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
                crate::panels::settings::remote_rows(app, ui, &mut draft.0, &mut message);
            });
            if !message.is_empty() {
                ui.label(egui::RichText::new(&message).size(12.0));
            }
            ui.separator();
            let r = ui.button(tl!("Close"));
            app.auto.add_response("projects.transfer.close", &r, "Close");
            close = r.clicked();
        });
    if close {
        *open = false;
    }
    ctx.data_mut(|m| m.insert_temp(id, (draft, message)));
}

/// [`RemoteDraft`] for egui's temp storage (it needs `Send + Sync + Clone + 'static`).
#[derive(Clone, Default)]
struct RemoteDraftHolder(RemoteDraft);

fn no_discovery(d: &TransferDialog) -> bool {
    d.no_discovery || std::env::var_os("FILMCRAFT_NEARBY_NO_DISCOVERY").is_some()
}

fn start_nearby_send(app: &mut FilmcraftApp, d: &mut TransferDialog) -> Result<Value, String> {
    let code = d.code.trim().to_string();
    if code.is_empty() {
        return Err(tl!("Type the code shown on the other computer.").to_string());
    }
    let mut p = json!({"code": code, "path": d.project, "proxies": d.proxies, "trim": d.trim});
    let address = d.address.trim();
    if !address.is_empty() {
        p["address"] = json!(address);
    }
    if no_discovery(d) {
        p["discovery"] = json!(false);
    }
    let v = app.session.execute("nearby.send", p).map_err(|e| e.to_string())?;
    d.job = v["job"].as_u64();
    d.message = tl!("Sending…").to_string();
    Ok(v)
}

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

fn start_nearby_receive(app: &mut FilmcraftApp, d: &mut TransferDialog) -> Result<Value, String> {
    let mut p = json!({});
    if no_discovery(d) {
        p["discovery"] = json!(false);
    }
    let v = app.session.execute("nearby.receive", p).map_err(|e| e.to_string())?;
    d.nearby = true;
    d.code = v["code"].as_str().unwrap_or_default().to_string();
    d.port = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(0);
    d.warnings = strings(&v["warnings"]).unwrap_or_default();
    d.offer = None;
    d.note.clear();
    d.message.clear();
    if let Some(job) = v["job"].as_u64() {
        d.job = Some(job);
        if !app.ui.home.receiving.iter().any(|r| r.job == job) {
            // `local` stays empty: `settle` takes the job's package when it finishes
            app.ui.home.receiving.push(PendingReceive { job, folder: app.ui.home.folder.clone(), ..Default::default() });
        }
    }
    Ok(v)
}

/// Copies the receive's state (code, port, the offer, notes) into the window. Cheap: the
/// command only locks a mutex.
fn poll_nearby_receive(app: &mut FilmcraftApp, d: &mut TransferDialog) {
    let Ok(v) = app.session.execute("nearby.status", json!({})) else { return };
    if v["job"].as_u64() != d.job {
        return;
    }
    if let Some(c) = v["code"].as_str().filter(|c| !c.is_empty()) {
        d.code = c.to_string();
    }
    d.port = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(d.port);
    if let Some(w) = strings(&v["warnings"]) {
        d.warnings = w;
    }
    d.note = v["note"].as_str().unwrap_or_default().to_string();
    d.offer = v["offer"].as_object().map(|o| NearbyOffer {
        computer: o.get("computer").and_then(Value::as_str).unwrap_or_default().to_string(),
        package: o.get("package").and_then(Value::as_str).unwrap_or_default().to_string(),
        bytes: o.get("bytes").and_then(Value::as_u64).unwrap_or(0),
    });
    // the received file is named by its checksum; the import is named after the offered package
    if let (Some(o), Some(job)) = (&d.offer, d.job)
        && let Some(r) = app.ui.home.receiving.iter_mut().find(|r| r.job == job)
    {
        r.name = o.package.clone();
    }
}

/// What Find found (`None` while looking or before a look).
#[derive(Default)]
struct Finding {
    result: Option<Result<Vec<NearbyReceiver>, String>>,
}

fn finding() -> MutexGuard<'static, Finding> {
    static F: OnceLock<Mutex<Finding>> = OnceLock::new();
    F.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner)
}

/// Find: `nearby.list` on a background thread (it listens for ~1.5 s); the frame keeps drawing.
fn find_receivers(ctx: &egui::Context) {
    finding().result = None;
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new().name("filmcraft-nearby-find".into()).spawn(move || {
        let r = std::panic::catch_unwind(|| {
            let mut s = filmcraft_engine::Session::default();
            let v = s.execute("nearby.list", json!({"timeoutMs": 1500})).map_err(|e| e.to_string())?;
            let list = v["receivers"].as_array().cloned().unwrap_or_default();
            Ok(list
                .iter()
                .take(16)
                .map(|r| NearbyReceiver { channel: r["channel"].as_u64().unwrap_or(0), address: r["address"].as_str().unwrap_or_default().to_string() })
                .collect())
        })
        .unwrap_or_else(|_| Err("the search stopped unexpectedly".into()));
        finding().result = Some(r);
        ctx.request_repaint();
    });
    if let Err(e) = spawned {
        finding().result = Some(Err(e.to_string()));
    }
}

/// The typed code with its channel number set to `channel` (a found computer's).
fn with_channel(code: &str, channel: u64) -> String {
    let code = code.trim();
    match code.split_once(['-', ' ', '.']) {
        Some((n, rest)) if n.chars().all(|c| c.is_ascii_digit()) => format!("{channel}-{rest}"),
        _ if code.chars().all(|c| c.is_ascii_digit()) => format!("{channel}-"),
        _ => format!("{channel}-{code}"),
    }
}

/// What the window's widgets asked for this frame (applied after drawing).
#[derive(Default)]
struct Clicks {
    navigate: Option<String>,
    ok: bool,
    cancel: bool,
    manage: bool,
    find: bool,
    nearby_receive: bool,
    accept: bool,
    decline: bool,
}

type Elems = Vec<(String, egui::Response, String)>;

fn transfer_window(app: &mut FilmcraftApp, ctx: &egui::Context, d: &mut TransferDialog, open: &mut bool) {
    let send = d.kind == "send";
    let locations = app.session.prefs.remote.locations.clone();
    if !locations.iter().any(|l| l.id == d.location) {
        d.location = locations.first().map(|l| l.id.clone()).unwrap_or_default();
        d.folder.clear();
    }
    let key = (d.location.clone(), d.folder.clone());
    let servers_shown = if send { !d.nearby } else { !(d.nearby && d.job.is_some()) };
    if servers_shown && !d.location.is_empty() && !listings().contains_key(&key) {
        refresh(app, ctx, &d.location, &d.folder);
    }
    if send && d.job.is_none() && d.estimate_for != Some((d.proxies, d.trim)) {
        d.estimate_for = Some((d.proxies, d.trim));
        let p = json!({"path": d.project, "destination": std::env::temp_dir().to_string_lossy(), "dryRun": true, "proxies": d.proxies, "trim": d.trim});
        d.estimate = app.session.execute("projects.package", p).ok().and_then(|v| v["estimateBytes"].as_u64());
    }
    if !send && d.nearby && d.job.is_some() {
        poll_nearby_receive(app, d);
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
    if d.finding {
        if let Some(r) = finding().result.take() {
            d.finding = false;
            match r {
                Ok(list) => {
                    d.message = if list.is_empty() {
                        tl!("No computer is waiting to receive. Check that Receive is open on the other computer, or type its address.").to_string()
                    } else {
                        String::new()
                    };
                    d.receivers = list;
                }
                Err(e) => d.message = tlf!("Couldn't look for computers: {error}", error = e),
            }
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let job = d.job.and_then(|id| app.session.jobs.iter().find(|j| j.id == id).cloned());
    if let Some(j) = &job {
        if let Some(r) = j.result.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            if send {
                d.message = match r {
                    Ok(rep) => {
                        let to = if d.nearby { rep.path } else { locations.iter().find(|l| l.id == d.location).map(|l| l.name.clone()).unwrap_or_default() };
                        tlf!("Sent: {name} to {to}", name = project_name(&d.project), to = to)
                    }
                    Err(e) => e,
                };
                d.job = None;
            }
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
    let busy = d.job.is_some();
    let mut elems: Elems = Vec::new();
    let mut c = Clicks::default();
    let title = if send { tl!("Send Project") } else { tl!("Receive Project") };
    egui::Window::new(title).id(egui::Id::new("projects-transfer")).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(
        ctx,
        |ui| {
            ui.set_width(560.0);
            if send {
                ui.label(tlf!("Project: {name}", name = project_name(&d.project)));
            }
            if servers_shown || send {
                server_choice(ui, d, &locations, busy, send, &mut elems, &mut c);
            }
            if servers_shown && !locations.is_empty() {
                server_listing(app, ui, d, &key, busy, send, &mut elems, &mut c);
            }
            if send && d.nearby {
                nearby_send_fields(ui, d, busy, &mut elems, &mut c);
            }
            if !send {
                nearby_receive_section(ui, d, busy, &mut elems, &mut c);
            }
            if send && (d.nearby || !locations.is_empty()) {
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(!busy, egui::Checkbox::new(&mut d.proxies, tl!("Include proxies")));
                    elems.push(("projects.transfer.proxies".into(), r, "Include proxies".into()));
                    let r = ui.add_enabled(!busy, egui::Checkbox::new(&mut d.trim, tl!("Leave out unused media")));
                    elems.push(("projects.transfer.trim".into(), r, "Leave out unused media".into()));
                });
                if let Some(b) = d.estimate {
                    ui.label(tlf!("Size: about {size}", size = human(b)));
                }
            }
            if let Some(j) = job.as_ref().filter(|_| busy) {
                let p = &j.progress;
                let (done, total) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
                let mut line = p.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
                if total > 0 {
                    line = format!("{line} {} / {}", human(done), human(total));
                }
                if let Some(eta) = p.eta() {
                    line = tlf!("{line}, about {secs} s left", line = line, secs = eta.as_secs());
                }
                ui.add(egui::ProgressBar::new(p.fraction().clamp(0.0, 1.0)).text(line));
            }
            if !d.message.is_empty() {
                ui.label(egui::RichText::new(&d.message).size(12.0));
            }
            ui.separator();
            ui.horizontal(|ui| {
                let nearby_receiving = !send && d.nearby && busy;
                if !nearby_receiving {
                    let label = if send { tl!("Send") } else { tl!("Receive") };
                    let can = !busy && if send && d.nearby { !d.code.trim().is_empty() } else { !d.location.is_empty() && (send || !d.selected.is_empty()) };
                    let b = ui.add_enabled(can, egui::Button::new(label));
                    c.ok = b.clicked();
                    elems.push(("projects.transfer.ok".into(), b, label.to_string()));
                }
                let label = if busy { tl!("Cancel") } else { tl!("Close") };
                let b = ui.button(label);
                c.cancel = b.clicked();
                elems.push(("projects.transfer.cancel".into(), b, label.to_string()));
            });
        },
    );
    for (id, r, label) in elems {
        app.auto.add_response(&id, &r, &label);
    }
    if let Some(p) = c.navigate {
        d.folder = p;
        d.selected.clear();
        listings().remove(&(d.location.clone(), d.folder.clone()));
    }
    if c.manage {
        let _ = route(app, "projects.manageLocations", &json!({}));
        return;
    }
    if c.find && !d.finding {
        d.finding = true;
        d.receivers.clear();
        d.message = tl!("Looking for computers…").to_string();
        find_receivers(ctx);
    }
    if c.ok {
        let r = match (send, d.nearby) {
            (true, true) => start_nearby_send(app, d),
            (true, false) => start_send(app, d),
            _ => start_receive(app, d),
        };
        if let Err(e) = r {
            d.message = e;
        }
    }
    if c.nearby_receive
        && let Err(e) = start_nearby_receive(app, d)
    {
        d.message = e;
    }
    if c.accept || c.decline {
        let cmd = if c.accept { "nearby.accept" } else { "nearby.decline" };
        match app.session.execute(cmd, json!({})) {
            Ok(_) => d.offer = None,
            Err(e) => d.message = e.to_string(),
        }
    }
    if c.cancel {
        match &job {
            // ends the receive (and its code); the job's end message comes through `settle`
            Some(_) if busy && !send && d.nearby => {
                let _ = app.session.execute("nearby.stop", json!({}));
                d.message = tl!("Cancelling…").to_string();
            }
            // a stopped transfer keeps what it has: sending again starts over, receiving again continues
            Some(j) if busy => {
                j.progress.cancel.store(true, Ordering::Relaxed);
                d.message = tl!("Cancelling…").to_string();
            }
            _ => *open = false,
        }
    }
}

/// "To:" / "From:" with the saved servers (or the hint to add one), and in Send the Nearby choice.
fn server_choice(
    ui: &mut egui::Ui,
    d: &mut TransferDialog,
    locations: &[filmcraft_remote::Location],
    busy: bool,
    send: bool,
    elems: &mut Elems,
    c: &mut Clicks,
) {
    let nearby_choice = |ui: &mut egui::Ui, d: &mut TransferDialog, elems: &mut Elems| {
        if send {
            let r = ui.add_enabled(!busy, egui::Button::selectable(d.nearby, tl!("Nearby computer")));
            if r.clicked() {
                d.nearby = true;
                d.message.clear();
            }
            elems.push(("projects.transfer.nearby".into(), r, "Nearby computer".into()));
        }
    };
    if locations.is_empty() {
        ui.label(if send { tl!("Add a server to send projects through it.") } else { tl!("Add a server to receive projects from it.") });
        ui.horizontal(|ui| {
            if send {
                ui.label(tl!("To:"));
                nearby_choice(ui, d, elems);
            }
            let b = ui.button(tl!("Manage Locations…"));
            c.manage = b.clicked();
            elems.push(("projects.transfer.manage".into(), b, "Manage Locations…".into()));
        });
        return;
    }
    ui.horizontal(|ui| {
        ui.label(if send { tl!("To:") } else { tl!("From:") });
        let on_server = !(send && d.nearby);
        let cur =
            if on_server { locations.iter().find(|l| l.id == d.location).map(|l| l.name.clone()).unwrap_or_default() } else { tl!("A server…").to_string() };
        let r = egui::ComboBox::from_id_salt("projects-transfer-location").selected_text(cur.clone()).width(240.0).show_ui(ui, |ui| {
            for l in locations {
                let it = ui.selectable_label(on_server && l.id == d.location, &l.name);
                if it.clicked() && !busy {
                    if l.id != d.location {
                        d.location = l.id.clone();
                        c.navigate = Some(String::new());
                    }
                    d.nearby = false;
                }
                elems.push((format!("projects.transfer.location.{}", l.id), it, l.name.clone()));
            }
        });
        elems.push(("projects.transfer.location".into(), r.response, cur));
        nearby_choice(ui, d, elems);
        if on_server {
            let up = ui.add_enabled(!d.folder.is_empty() && !busy, egui::Button::new(tl!("Up")));
            if up.clicked() {
                c.navigate = Some(d.folder.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_default());
            }
            elems.push(("projects.transfer.up".into(), up, "Up".into()));
        }
        let m = ui.button(tl!("Manage Locations…"));
        c.manage = m.clicked();
        elems.push(("projects.transfer.manage".into(), m, "Manage Locations…".into()));
    });
}

/// The server folder's folders and packages (Receive picks one; Send shows what's there).
#[allow(clippy::too_many_arguments)]
fn server_listing(
    app: &FilmcraftApp,
    ui: &mut egui::Ui,
    d: &mut TransferDialog,
    key: &(String, String),
    busy: bool,
    send: bool,
    elems: &mut Elems,
    c: &mut Clicks,
) {
    ui.label(egui::RichText::new(format!("/{}", d.folder)).size(12.0));
    let listing = listings().get(key).cloned().flatten();
    egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, false]).show(ui, |ui| match listing {
        None => {
            ui.label(tl!("Loading…"));
        }
        Some(Err(e)) => {
            ui.label(tlf!("Couldn't list the folder: {error}", error = e));
        }
        Some(Ok(mut entries)) => {
            let is_pkg = |e: &filmcraft_remote::Entry| !e.is_dir && filmcraft_engine::package::is_package_file(std::path::Path::new(e.name()));
            // folders first; then packages, newest first
            entries.retain(|e| e.is_dir || is_pkg(e));
            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| b.modified.cmp(&a.modified)).then_with(|| a.name().cmp(b.name())));
            if entries.is_empty() {
                ui.label(tl!("No folders or packages here."));
            }
            for e in entries.iter().take(2000) {
                let path = e.path.as_str().to_string();
                let label = if e.is_dir {
                    format!("📁 {}", e.name())
                } else {
                    let when =
                        e.modified.filter(|t| *t > 0).and_then(|t| u64::try_from(t).ok()).map(|t| crate::panels::home::date(app, Some(t))).unwrap_or_default();
                    format!("{}   {}   {}", e.name(), human(e.size), when)
                };
                let r = ui.add_enabled_ui(!busy, |ui| ui.selectable_label(d.selected == path, label)).inner;
                if r.clicked() {
                    if e.is_dir {
                        c.navigate = Some(path.clone());
                    } else if !send {
                        d.selected = path.clone();
                    }
                }
                elems.push((format!("projects.transfer.entry.{path}"), r, e.name().to_string()));
            }
        }
    });
}

/// Send to a nearby computer: the code, the address fallback, Find and what it found.
fn nearby_send_fields(ui: &mut egui::Ui, d: &mut TransferDialog, busy: bool, elems: &mut Elems, c: &mut Clicks) {
    ui.add_space(4.0);
    ui.label(tl!("Type the code shown on the other computer."));
    egui::Grid::new("projects-transfer-nearby").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label(tl!("Code:"));
        let r = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut d.code).hint_text("7-velvet-river-lamp").desired_width(300.0));
        elems.push(("projects.transfer.code".into(), r, "Code".into()));
        ui.end_row();
        ui.label(tl!("Address:"));
        ui.horizontal(|ui| {
            let r = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut d.address).hint_text(tl!("address:port, if it isn't found")).desired_width(300.0));
            elems.push(("projects.transfer.address".into(), r, "Address".into()));
            let b = ui.add_enabled(!busy && !d.finding, egui::Button::new(tl!("Find")));
            c.find = b.clicked();
            elems.push(("projects.transfer.find".into(), b, "Find".into()));
            if d.finding {
                ui.spinner();
            }
        });
        ui.end_row();
    });
    for (i, r) in d.receivers.clone().iter().enumerate() {
        let label = tlf!("Channel {n} at {address}", n = r.channel, address = r.address);
        let picked = d.address == r.address;
        let b = ui.add_enabled(!busy, egui::Button::selectable(picked, label.clone()));
        if b.clicked() {
            d.address = r.address.clone();
            d.code = with_channel(&d.code, r.channel);
        }
        elems.push((format!("projects.transfer.receiver.{i}"), b, label));
    }
    ui.add_space(4.0);
}

/// Receive from a nearby computer: the button, then the code, the offer and Accept / Decline.
fn nearby_receive_section(ui: &mut egui::Ui, d: &mut TransferDialog, busy: bool, elems: &mut Elems, c: &mut Clicks) {
    if !(d.nearby && busy) {
        ui.add_space(6.0);
        let r = ui.add_enabled(!busy, egui::Button::new(tl!("Receive from a nearby computer")));
        c.nearby_receive = r.clicked();
        elems.push(("projects.transfer.nearby".into(), r, "Receive from a nearby computer".into()));
        return;
    }
    ui.label(tl!("Type this code on the other computer:"));
    let r = ui.label(egui::RichText::new(&d.code).size(30.0).monospace().strong());
    elems.push(("projects.transfer.codeShown".into(), r, d.code.clone()));
    ui.label(egui::RichText::new(tl!("Valid for 10 minutes, once.")).size(12.0));
    if d.port != 0 {
        ui.label(egui::RichText::new(tlf!("If it isn't found, type this computer's address with port {port}.", port = d.port)).size(12.0));
    }
    for w in &d.warnings {
        ui.label(egui::RichText::new(w).size(12.0));
    }
    // FORK-PLAN §1a.118: the one command that lets the other computer in (never run for the user)
    if let Some(cmd) = firewall_command(d.port) {
        ui.label(egui::RichText::new(tl!("The other computer can't connect? Allow Revolution in this computer's firewall once:")).size(12.0));
        let r = ui.add(egui::Label::new(egui::RichText::new(&cmd).monospace().size(12.0)).selectable(true));
        elems.push(("projects.transfer.firewall".into(), r, cmd));
    } else if cfg!(windows) {
        ui.label(egui::RichText::new(tl!("If Windows asks, allow Revolution on private networks.")).size(12.0));
    }
    if !d.note.is_empty() {
        ui.label(egui::RichText::new(&d.note).size(12.0).strong());
    }
    if let Some(o) = d.offer.clone() {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(tlf!("{computer} wants to send {package} ({size})", computer = o.computer, package = o.package, size = human(o.bytes)))
                .strong(),
        );
        ui.horizontal(|ui| {
            let a = ui.button(tl!("Accept"));
            c.accept = a.clicked();
            elems.push(("projects.transfer.accept".into(), a, "Accept".into()));
            let b = ui.button(tl!("Decline"));
            c.decline = b.clicked();
            elems.push(("projects.transfer.decline".into(), b, "Decline".into()));
        });
    }
    ui.add_space(4.0);
}

/// Which firewall this Linux computer has, and whether the package's descriptions are installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Firewall {
    UfwProfile,
    Ufw,
    FirewalldService,
    Firewalld,
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).take(64).any(|d| d.join(name).is_file()))
}

fn firewall() -> Option<Firewall> {
    static F: std::sync::OnceLock<Option<Firewall>> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return None;
        }
        // root-only tools usually live in sbin, which isn't always on a user's PATH
        let has = |name: &str| on_path(name) || ["/usr/sbin", "/sbin", "/usr/bin"].iter().any(|d| std::path::Path::new(d).join(name).is_file());
        let file = |p: &str| std::path::Path::new(p).is_file();
        if has("ufw") {
            Some(if file("/etc/ufw/applications.d/revolution") { Firewall::UfwProfile } else { Firewall::Ufw })
        } else if has("firewall-cmd") {
            Some(if file("/usr/lib/firewalld/services/revolution.xml") { Firewall::FirewalldService } else { Firewall::Firewalld })
        } else {
            None
        }
    })
}

/// The command for the receive window: the packaged rule when the window listens on the standard
/// port, else a rule for the port it got.
fn firewall_command(port: u16) -> Option<String> {
    if port == 0 {
        return None;
    }
    firewall_command_for(firewall()?, port)
}

fn firewall_command_for(f: Firewall, port: u16) -> Option<String> {
    let standard = port == filmcraft_engine::nearby::DEFAULT_PORT;
    Some(match f {
        Firewall::UfwProfile if standard => "sudo ufw allow Revolution".into(),
        Firewall::UfwProfile | Firewall::Ufw => format!("sudo ufw allow {port}/tcp"),
        Firewall::FirewalldService if standard => "sudo firewall-cmd --permanent --add-service=revolution && sudo firewall-cmd --reload".into(),
        Firewall::FirewalldService | Firewall::Firewalld => format!("sudo firewall-cmd --permanent --add-port={port}/tcp && sudo firewall-cmd --reload"),
    })
}

#[cfg(test)]
mod tests {
    use super::{TransferDialog, package_named, with_channel};

    #[test]
    fn a_found_computer_sets_the_codes_number() {
        assert_eq!(with_channel("", 7), "7-");
        assert_eq!(with_channel("3", 7), "7-");
        assert_eq!(with_channel("3-velvet-river-lamp", 12), "12-velvet-river-lamp");
        assert_eq!(with_channel("velvet river lamp", 4), "4-velvet river lamp");
    }

    #[test]
    fn the_offered_name_cannot_leave_the_inbox() {
        let dir = std::env::temp_dir().join(format!("filmcraft-nearby-named-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (offered, want) in [
            ("../../etc/My Film.revpkg", Some("My Film.revpkg")),
            ("..", None),
            ("a/b:c?.revpkg", Some("bc.revpkg")),
            ("", None),
            ("...hidden", Some("hidden.revpkg")),
        ] {
            let local = dir.join("abc123.revpkg");
            std::fs::write(&local, b"x").unwrap();
            let got = package_named(&local.to_string_lossy(), offered);
            match want {
                Some(name) => {
                    let got = std::path::PathBuf::from(got.expect(offered));
                    assert_eq!(got.file_name().unwrap().to_string_lossy(), name, "{offered}");
                    assert!(got.starts_with(&dir) && got.is_file(), "{offered}: {got:?}");
                    std::fs::remove_file(&got).unwrap();
                }
                None => {
                    assert!(got.is_none(), "{offered}");
                    std::fs::remove_file(&local).unwrap();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dialog_state_round_trips_without_the_estimate_key() {
        let d = TransferDialog { kind: "send".into(), estimate_for: Some((true, false)), ..Default::default() };
        let v = serde_json::to_value(&d).unwrap();
        assert!(v.get("estimateFor").is_none());
        let back: TransferDialog = serde_json::from_value(v).unwrap();
        assert_eq!(back.kind, "send");
        assert_eq!(back.estimate_for, None);
    }

    #[test]
    fn the_firewall_command_matches_the_port() {
        use super::{Firewall, firewall_command_for as c};
        assert_eq!(c(Firewall::UfwProfile, 47821).unwrap(), "sudo ufw allow Revolution");
        assert_eq!(c(Firewall::UfwProfile, 40000).unwrap(), "sudo ufw allow 40000/tcp", "a fallback port isn't in the profile");
        assert_eq!(c(Firewall::Ufw, 47821).unwrap(), "sudo ufw allow 47821/tcp");
        assert!(c(Firewall::FirewalldService, 47821).unwrap().contains("--add-service=revolution"));
        assert!(c(Firewall::Firewalld, 47821).unwrap().contains("--add-port=47821/tcp"));
        assert_eq!(super::firewall_command(0), None);
    }
}
