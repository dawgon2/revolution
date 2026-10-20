//! **Project Manager** home screen (fork RESOLVE-UI-PLAN RS2, modelled on DaVinci Resolve's
//! Project Manager): a full-window screen listing the projects folder and recent projects, shown
//! at launch in the Resolve workflow (Settings ▸ General ▸ At startup show) and opened by File ▸
//! Project Manager (Resolve workflow) / File ▸ Projects… (Premiere workflow), Shift+1 in the Resolve
//! keymap and the Home button on the page bar. The engine side is `filmcraft_engine::projects`.
//!
//! Its state is [`HomeState`] (`UiState::home`, serde: `ui.inspect` / `ui.set {"home": …}`).
//! Automation ids:
//! - title row: `projects.close.x` (quits FilmCraft, as Quit does), `projects.tab.local`;
//! - row 2: `projects.sidebar`, `projects.crumb.root`, `projects.crumb.<n>`, `projects.newFolder`,
//!   `projects.thumbSize`, `projects.sort` (`projects.sort.<name|modified|created>`,
//!   `projects.sort.descending`), `projects.info`, `projects.view.grid`, `projects.view.list`,
//!   `projects.search`;
//! - body: `projects.tile.new`, `projects.folder.<n>`, `projects.tile.<n>`, `projects.other.<n>`
//!   (by position), `projects.sidebar.<root|other|n>`;
//! - context menu: `projects.menu.<open|rename|duplicate|reveal|export|trash|remove|locate>`;
//! - footer: `projects.export`, `projects.import`, `projects.new`, `projects.open`, `projects.close`;
//! - dialogs: `projects.dialog.name`, `projects.dialog.ok`, `projects.dialog.save`,
//!   `projects.dialog.discard`, `projects.dialog.cancel`.

use std::collections::HashMap;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::projects::{self, FolderEntry, Listing, ProjectSummary};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::automation::Registry;
use crate::frames::{FrameKey, Target};
use crate::icons::{self, Icon};
use crate::native_dialogs::FileDialog;
use crate::theme::Tokens;

/// Tile thumbnail size at the default thumbnail scale.
pub const TILE_W: f32 = 214.0;
pub const TILE_H: f32 = 120.0;
pub(crate) const TITLE_H: f32 = 36.0;
const BAR_H: f32 = 38.0;
const FOOTER_H: f32 = 48.0;
const SIDEBAR_W: f32 = 200.0;
const INFO_W: f32 = 260.0;
/// How often an open screen lists the folder again (other apps add files).
const RELIST_SECS: f64 = 5.0;
/// How long a frame waits for a background listing before drawing the previous tiles (a small
/// folder then shows without a blank frame).
const LIST_WAIT: std::time::Duration = std::time::Duration::from_millis(15);
/// A poster render that hasn't arrived after this long is given up.
const POSTER_TIMEOUT: f64 = 30.0;
/// Room left of and above the grid for a tile's selection border (2 px, drawn outside the tile;
/// the scroll area cut the top row's off).
const BORDER_ROOM: f32 = 4.0;
/// Posters decoded per frame at most (the rest on the next frames).
const POSTER_LOADS_PER_FRAME: usize = 4;

/// The sort keys (`projects.sort.<key>`; [`HomeState::sort`]).
pub const SORT_KEYS: [&str; 3] = ["name", "modified", "created"];
/// The right-click menu items (`projects.menu.<item>`).
pub const MENU_ITEMS: [&str; 9] = ["open", "rename", "duplicate", "reveal", "export", "package", "trash", "remove", "locate"];

/// The home screen's state (serde, so agents can read and set it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HomeState {
    pub open: bool,
    /// The shown folder relative to the projects folder (`""` = the projects folder).
    pub folder: String,
    /// `name` | `modified` | `created`.
    pub sort: String,
    pub descending: bool,
    pub search: String,
    pub list_view: bool,
    /// The info pane (details of the selected project).
    pub info: bool,
    pub sidebar: bool,
    /// Thumbnail scale (0.6–1.6; 1 = 214 × 120).
    pub thumb: f32,
    /// The selected project or folder (its path).
    pub selected: Option<String>,
    /// A project was open when the screen opened: Close / Esc go back to it.
    pub can_return: bool,
    pub dialog: Option<HomeDialog>,
}

impl Default for HomeState {
    fn default() -> Self {
        Self {
            open: false,
            folder: String::new(),
            sort: "name".into(),
            descending: false,
            search: String::new(),
            list_view: false,
            info: false,
            sidebar: false,
            thumb: 1.0,
            selected: None,
            can_return: false,
            dialog: None,
        }
    }
}

/// A question the screen is asking.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HomeDialog {
    /// `newProject` | `newFolder` | `rename` | `trash` | `unsaved`.
    pub kind: String,
    pub path: String,
    pub name: String,
    pub error: String,
    /// `unsaved`: the command to run after Save / Don't Save, and its params.
    pub then: Option<(String, Value)>,
}

/// (root, folder, sort, descending, search) of a listing.
type ListKey = (String, String, String, bool, String);

/// Run-time data of the screen (not serde).
#[derive(Default)]
pub struct Runtime {
    listing: Option<Listing>,
    /// The key of `listing`.
    key: Option<ListKey>,
    /// The listing being made on a background thread (reading uncached project files can take a
    /// while); the previous tiles stay up until it lands.
    job: Option<std::sync::mpsc::Receiver<(ListKey, Result<Listing, String>)>>,
    listed_at: f64,
    stale: bool,
    error: Option<String>,
    /// Poster textures by `<png>@<project mtime>`.
    textures: HashMap<String, Option<egui::TextureHandle>>,
    posters: Vec<PendingPoster>,
    /// The project shown when the screen opened (a different, freshly loaded one closes it).
    shown_project: usize,
    /// The poster being made for a project that has none ([`backfill_posters`]): its path.
    backfill: Option<std::sync::mpsc::Receiver<String>>,
    /// Projects a poster was already tried for this run (made or failed: not tried again).
    backfill_tried: std::collections::HashSet<String>,
}

struct PendingPoster {
    path: String,
    key: FrameKey,
    since: f64,
    page: Option<String>,
}

/// One shown entry (grid tile / list row).
#[derive(Clone)]
enum Entry {
    Folder(FolderEntry),
    Project(ProjectSummary, bool),
}

impl Entry {
    fn path(&self) -> &str {
        match self {
            Entry::Folder(f) => &f.path,
            Entry::Project(p, _) => &p.path,
        }
    }
    fn name(&self) -> &str {
        match self {
            Entry::Folder(f) => &f.name,
            Entry::Project(p, _) => &p.name,
        }
    }
}

fn entries(l: &Listing) -> Vec<(String, Entry)> {
    let mut v: Vec<(String, Entry)> = Vec::new();
    v.extend(l.folders.iter().enumerate().map(|(i, f)| (format!("projects.folder.{i}"), Entry::Folder(f.clone()))));
    v.extend(l.projects.iter().enumerate().map(|(i, p)| (format!("projects.tile.{i}"), Entry::Project(p.clone(), false))));
    v.extend(l.other.iter().enumerate().map(|(i, p)| (format!("projects.other.{i}"), Entry::Project(p.clone(), true))));
    v
}

fn project_ptr(app: &FilmcraftApp) -> usize {
    std::sync::Arc::as_ptr(&app.session.project) as usize
}

/// Open the home screen.
pub fn show_home(app: &mut FilmcraftApp) {
    app.ui.home.open = true;
    app.ui.home.can_return = projects::project_open(&app.session);
    app.ui.home.dialog = None;
    app.home_rt.stale = true;
    app.home_rt.shown_project = project_ptr(app);
    app.stop();
}

/// Close it (Close, Esc): back to the open project; with none open it stays. Returns whether it
/// closed.
pub fn close(app: &mut FilmcraftApp) -> bool {
    if !app.ui.home.can_return && !projects::project_open(&app.session) {
        app.ui.status = tl!("Open or create a project first.").into();
        return false;
    }
    app.ui.home.open = false;
    app.ui.home.dialog = None;
    true
}

/// Close the screen into the empty project the session already holds (an untitled project, as
/// Resolve does), when there is nothing to return to.
fn close_untitled(app: &mut FilmcraftApp) -> bool {
    app.ui.home.open = false;
    app.ui.home.dialog = None;
    app.ui.status = tl!("Untitled project: File ▸ Save As keeps it").into();
    true
}

/// After opening or creating a project: close the screen and show the page it was saved on (Edit
/// when unknown).
fn land(app: &mut FilmcraftApp, page: Option<&str>) {
    app.ui.home.open = false;
    app.ui.home.dialog = None;
    app.ui.home.selected = None;
    app.home_rt.stale = true;
    if app.resolve_workflow() {
        let page = page.filter(|p| crate::panels::pages::PAGES.iter().any(|x| x.0 == *p)).unwrap_or("edit");
        crate::panels::pages::open(app, page);
    } else {
        app.ui.mode = crate::state::Mode::Edit;
    }
}

fn selected_summary(app: &FilmcraftApp) -> Option<ProjectSummary> {
    let sel = app.ui.home.selected.as_deref()?;
    let l = app.home_rt.listing.as_ref()?;
    l.projects.iter().chain(l.other.iter()).find(|p| p.path == sel).cloned()
}

fn selected_folder(app: &FilmcraftApp) -> Option<FolderEntry> {
    let sel = app.ui.home.selected.as_deref()?;
    app.home_rt.listing.as_ref()?.folders.iter().find(|f| f.path == sel).cloned()
}

fn param<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn dialog(app: &mut FilmcraftApp, kind: &str, path: &str, name: &str) -> Value {
    app.ui.home.dialog = Some(HomeDialog { kind: kind.into(), path: path.into(), name: name.into(), ..Default::default() });
    json!({"dialog": kind})
}

/// The engine command, then a fresh listing.
fn run(app: &mut FilmcraftApp, id: &str, params: Value) -> Result<Value, String> {
    let r = app.session.execute(id, params).map_err(|e| e.to_string());
    app.home_rt.stale = true;
    r
}

/// Menu / shortcut / control entry points of the `projects.*` commands. Without the parameters
/// a person would type, they ask (a dialog, a file picker); with them they act. None for other ids.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !id.starts_with("projects.") {
        return None;
    }
    let r = match id {
        "projects.show" => {
            show_home(app);
            Ok(json!({"open": true}))
        }
        "projects.close" => {
            // `untitled`: with no project open, close into a new untitled project (the title
            // row's ×, owner 2026-10-13) instead of staying
            let closed = close(app) || (params.get("untitled").and_then(Value::as_bool) == Some(true) && close_untitled(app));
            Ok(json!({"open": !closed}))
        }
        "projects.open" => {
            let path = param(params, "path").map(str::to_string).or_else(|| selected_summary(app).map(|s| s.path));
            let Some(path) = path else {
                // a selected folder opens (shows its projects)
                if let Some(f) = selected_folder(app) {
                    app.ui.home.folder = f.folder;
                    app.ui.home.selected = None;
                    return Some(Ok(json!({"folder": app.ui.home.folder})));
                }
                return Some(Err("select a project to open".into()));
            };
            let discard = params.get("discard").and_then(Value::as_bool).unwrap_or(false);
            if app.session.is_dirty() && !discard {
                app.ui.home.open = true;
                app.ui.home.dialog =
                    Some(HomeDialog { kind: "unsaved".into(), then: Some(("projects.open".into(), json!({"path": path}))), ..Default::default() });
                return Some(Ok(json!({"dialog": "unsaved"})));
            }
            match run(app, "projects.open", json!({"path": path, "discard": true})) {
                Ok(v) => {
                    let page = v.get("page").and_then(Value::as_str).map(str::to_string);
                    land(app, page.as_deref());
                    Ok(v)
                }
                Err(e) => Err(e),
            }
        }
        "projects.new" => match param(params, "name") {
            None => Ok(dialog(app, "newProject", "", tl!("Untitled Project"))),
            Some(name) => {
                let mut p = json!({"name": name, "folder": param(params, "folder").unwrap_or(&app.ui.home.folder)});
                let discard = params.get("discard").and_then(Value::as_bool).unwrap_or(false);
                if app.session.is_dirty() && !discard {
                    app.ui.home.open = true;
                    app.ui.home.dialog = Some(HomeDialog { kind: "unsaved".into(), then: Some(("projects.new".into(), p)), ..Default::default() });
                    return Some(Ok(json!({"dialog": "unsaved"})));
                }
                p["discard"] = json!(true);
                run(app, "projects.new", p).inspect(|_| land(app, Some("edit")))
            }
        },
        "projects.newFolder" => match param(params, "name") {
            None => Ok(dialog(app, "newFolder", "", tl!("New Folder"))),
            Some(_) => {
                let mut p = params.clone();
                if p.get("folder").is_none() {
                    p["folder"] = json!(app.ui.home.folder);
                }
                run(app, id, p)
            }
        },
        "projects.rename" if param(params, "name").is_none() => {
            let target = param(params, "path").map(str::to_string).or_else(|| app.ui.home.selected.clone());
            match target {
                Some(path) => {
                    let name = std::path::Path::new(&path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let name = if std::path::Path::new(&path).is_dir() {
                        std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name)
                    } else {
                        name
                    };
                    Ok(dialog(app, "rename", &path, &name))
                }
                None => Err("select a project or folder to rename".into()),
            }
        }
        // a person confirms; an agent naming the path acts
        "projects.trash" if param(params, "path").is_none() => match app.ui.home.selected.clone() {
            Some(path) => {
                let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                Ok(dialog(app, "trash", &path, &name))
            }
            None => Err("select a project or folder to move to the trash".into()),
        },
        "projects.duplicate" | "projects.removeFromList" if param(params, "path").is_none() => match app.ui.home.selected.clone() {
            Some(path) => run(app, id, json!({"path": path})),
            None => Err("select a project first".into()),
        },
        "projects.reveal" => {
            let mut p = params.clone();
            if param(params, "path").is_none() {
                p = match app.ui.home.selected.clone() {
                    Some(path) => json!({"path": path}),
                    None => json!({"folder": app.ui.home.folder}),
                };
            }
            match app.session.execute(id, p).map_err(|e| e.to_string()) {
                Ok(v) => {
                    let path = v["path"].as_str().unwrap_or_default().to_string();
                    let shown = match app.hooks.open_path.as_mut() {
                        Some(open) => open(&path, true).map(|_| true),
                        None => Ok(false),
                    };
                    shown.map(|s| json!({"path": path, "revealed": s}))
                }
                Err(e) => Err(e),
            }
        }
        "projects.locate" if param(params, "newPath").is_none() => {
            let Some(path) = param(params, "path").map(str::to_string).or_else(|| app.ui.home.selected.clone()) else {
                return Some(Err("select a missing project first".into()));
            };
            let id = id.to_string();
            app.pick(FileDialog::OpenProject, move |app, paths| match paths.into_iter().next() {
                Some(to) => run(app, &id, json!({"path": path, "newPath": to})),
                None => Ok(Value::Null),
            })
        }
        "projects.export" if param(params, "destination").is_none() => {
            let Some(path) = param(params, "path").map(str::to_string).or_else(|| selected_summary(app).filter(|s| s.error.is_none()).map(|s| s.path)) else {
                return Some(Err("select a project to export".into()));
            };
            let name = std::path::Path::new(&path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let id = id.to_string();
            app.pick(FileDialog::Folder { at: None }, move |app, paths| match paths.into_iter().next() {
                Some(dir) => {
                    let dest = std::path::Path::new(&dir).join(tlf!("{name} archive", name));
                    let r = run(app, &id, json!({"path": path, "destination": dest.to_string_lossy()}));
                    if r.is_ok() {
                        app.ui.status = tlf!("Exporting {name} to {dest}", name, dest = dest.display());
                    }
                    r
                }
                None => Ok(Value::Null),
            })
        }
        // STORAGE-PLAN S1 item 5: a checked package of the project and all its media
        "projects.package" if param(params, "destination").is_none() => {
            let Some(path) = param(params, "path").map(str::to_string).or_else(|| selected_summary(app).filter(|s| s.error.is_none()).map(|s| s.path)) else {
                return Some(Err("select a project to package".into()));
            };
            let name = std::path::Path::new(&path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let id = id.to_string();
            // `"format": "file"`: one .revpkg file (the tile menu's Package as One File…)
            let format = param(params, "format").unwrap_or("folder").to_string();
            app.pick(FileDialog::Folder { at: None }, move |app, paths| match paths.into_iter().next() {
                Some(dir) => {
                    // the size first (nothing written), so the status line can say it
                    let est = run(app, &id, json!({"path": path, "destination": dir, "dryRun": true})).ok().and_then(|v| v["estimateBytes"].as_u64());
                    let r = run(app, &id, json!({"path": path, "destination": dir, "format": format}));
                    if r.is_ok() {
                        app.ui.status = match est {
                            Some(b) => tlf!("Packaging {name} (about {size})", name, size = package_size(b)),
                            None => tlf!("Packaging {name}", name),
                        };
                    }
                    r
                }
                None => Ok(Value::Null),
            })
        }
        // one picker for both kinds: a .revpkg (or older .fcpackage) file, or a package folder's manifest.json
        "projects.importPackage" if param(params, "path").is_none() => {
            let id = id.to_string();
            let pick = FileDialog::OpenFile {
                filter: tl!("Revolution package (.revpkg or manifest.json)").into(),
                exts: vec!["revpkg".into(), "fcpackage".into(), "json".into()],
            };
            app.pick(pick, move |app, paths| match paths.into_iter().next() {
                Some(path) => run(app, &id, json!({"path": package_pick(path), "folder": app.ui.home.folder})),
                None => Ok(Value::Null),
            })
        }
        "projects.import" if param(params, "path").is_none() => {
            let id = id.to_string();
            app.pick(FileDialog::OpenProject, move |app, paths| match paths.into_iter().next() {
                Some(path) => run(app, &id, json!({"path": path, "folder": app.ui.home.folder})),
                None => Ok(Value::Null),
            })
        }
        _ => run(app, id, params.clone()),
    };
    if let Err(e) = &r {
        app.ui.status = e.clone();
    }
    Some(r)
}

// ------------------------------------------------------------------ posters

/// Render the poster of each project just saved (the frame under the playhead of the active
/// sequence) and store it with the page it was saved on. Runs every frame.
pub fn tick_posters(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    for path in std::mem::take(&mut app.session.poster_requests) {
        let Some(seq_id) = app.session.state.active_sequence else { continue };
        let Some(seq) = app.session.project.sequence(seq_id) else { continue };
        let rate = seq.settings.frame_rate;
        let width = seq.settings.width.max(1);
        let size = width.min(projects::POSTER_WIDTH);
        let frame = rate.frame_at(app.session.playhead());
        let key = FrameKey { target: Target::Item(seq_id), frame, size, revision: app.item_revision(seq_id), draft: false, temporal: false };
        let project = app.session.project.clone();
        app.frames.request(key, rate.tick_of(frame), size as f32 / width as f32, &project, 50);
        let page = app.resolve_workflow().then(|| crate::panels::pages::current(app)).flatten().map(str::to_string);
        app.home_rt.posters.retain(|p| p.path != path);
        app.home_rt.posters.push(PendingPoster { path, key, since: now, page });
    }
    if app.home_rt.posters.is_empty() {
        return;
    }
    let cache = projects::cache_dir(&app.session);
    let mut done = Vec::new();
    for (i, p) in app.home_rt.posters.iter().enumerate() {
        if let Some(img) = app.frames.get(&p.key) {
            if let Some(c) = &cache {
                let (w, h) = (u32::try_from(img.w).unwrap_or(0), u32::try_from(img.h).unwrap_or(0));
                if let Err(e) = projects::store_poster(c, std::path::Path::new(&p.path), w, h, &img.px, p.page.as_deref()) {
                    log::warn!("poster of {}: {e}", p.path);
                }
            }
            done.push(i);
        } else if now - p.since > POSTER_TIMEOUT {
            log::debug!("poster of {}: no frame in time", p.path);
            done.push(i);
        }
    }
    for i in done.into_iter().rev() {
        app.home_rt.posters.remove(i);
        app.home_rt.stale = true;
    }
    if !app.home_rt.posters.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Make a poster for each listed project that has none (fork, owner 2026-10-20, FORK-PLAN
/// §1a.108: every project shows a picture of its timeline, as in Resolve; projects saved before
/// posters existed, or saved while their frame wasn't ready, had the placeholder for good). One
/// project at a time on a background thread under `catch_unwind`, each tried once per run; it
/// waits while the machine is low on memory. The tile shows the faded logo until the poster lands.
fn backfill_posters(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if let Some(rx) = &app.home_rt.backfill {
        match rx.try_recv() {
            Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                app.home_rt.backfill = None;
                app.home_rt.stale = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
                return;
            }
        }
    }
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let Some(cache) = projects::cache_dir(&app.session) else { return };
    let Some(listing) = &app.home_rt.listing else { return };
    let tried = &app.home_rt.backfill_tried;
    let Some(path) = listing
        .projects
        .iter()
        .chain(&listing.other)
        .find(|s| s.poster.is_none() && !s.missing && s.error.is_none() && !tried.contains(&s.path))
        .map(|s| s.path.clone())
    else {
        return;
    };
    if filmcraft_engine::background::memory_low_now() {
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
        return;
    }
    app.home_rt.backfill_tried.insert(path.clone());
    let services = app.session.services.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let job = path.clone();
    let run = move || {
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| projects::make_poster(services, &cache, std::path::Path::new(&job))));
        match made {
            Ok(Ok(())) => log::debug!("poster made for {job}"),
            Ok(Err(e)) => log::debug!("no poster for {job}: {e}"),
            Err(_) => log::warn!("making the poster of {job} failed"),
        }
        let _ = tx.send(job);
    };
    match std::thread::Builder::new().name("filmcraft-project-poster".into()).spawn(run) {
        Ok(_) => app.home_rt.backfill = Some(rx),
        Err(e) => log::warn!("poster of {path}: {e}"),
    }
}

// ------------------------------------------------------------------ listing

fn refresh(app: &mut FilmcraftApp, ctx: &egui::Context, now: f64) {
    let h = &app.ui.home;
    let root = projects::projects_root(&app.session);
    let root_s = root.as_ref().map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
    let key = (root_s, h.folder.clone(), h.sort.clone(), h.descending, h.search.clone());
    let due = app.home_rt.stale || app.home_rt.key.as_ref() != Some(&key) || now - app.home_rt.listed_at > RELIST_SECS;
    if due && app.home_rt.job.is_none() {
        start_listing(app, root, key, now);
    }
    let Some(rx) = app.home_rt.job.as_ref() else { return };
    match rx.recv_timeout(LIST_WAIT) {
        Ok((key, r)) => {
            app.home_rt.job = None;
            match r {
                Ok(l) => {
                    app.home_rt.listing = Some(l);
                    app.home_rt.error = None;
                }
                Err(e) => {
                    // a folder that went away: back to the projects folder
                    if !key.1.is_empty() && app.ui.home.folder == key.1 {
                        app.ui.home.folder.clear();
                    }
                    app.home_rt.error = Some(e);
                }
            }
            app.home_rt.key = Some(key);
            ctx.request_repaint();
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            app.home_rt.job = None;
            app.home_rt.error = Some(tl!("The projects folder could not be listed.").into());
        }
    }
}

/// List the folder on a background thread (under `catch_unwind`: a bug on a hostile file is an
/// error line, not a crash); on wasm, in place.
fn start_listing(app: &mut FilmcraftApp, root: Option<std::path::PathBuf>, key: ListKey, now: f64) {
    app.home_rt.stale = false;
    app.home_rt.listed_at = now;
    let (tx, rx) = std::sync::mpsc::channel();
    app.home_rt.job = Some(rx);
    let Some(root) = root else {
        let _ = tx.send((key, Err(tl!("No projects folder: set Settings ▸ General ▸ Projects folder.").into())));
        return;
    };
    let sort = projects::SortKey::parse(&key.2).unwrap_or_default();
    let recent = app.session.prefs.general.recent_projects.clone();
    let cache = projects::cache_dir(&app.session);
    let warning = projects::root_warning(&app.session);
    if let Some(w) = &warning {
        log::warn!("{w}");
    }
    let run = move || {
        let listed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| projects::list(&root, &key.1, sort, key.3, &key.4, &recent, cache.as_deref())));
        let r = match listed {
            Ok(r) => r.map(|mut l| {
                l.warning = warning;
                l
            }),
            Err(_) => Err("listing the projects folder failed".to_string()),
        };
        let _ = tx.send((key, r));
    };
    if cfg!(target_arch = "wasm32") {
        run();
    } else if let Err(e) = std::thread::Builder::new().name("filmcraft-projects-list".into()).spawn(run) {
        app.home_rt.job = None;
        app.home_rt.error = Some(e.to_string());
    }
}

fn poster_texture(rt: &mut Runtime, ctx: &egui::Context, s: &ProjectSummary, budget: &mut usize) -> Option<egui::TextureId> {
    let png = s.poster.as_deref()?;
    let key = format!("{png}@{}", s.modified.unwrap_or(0));
    if let Some(t) = rt.textures.get(&key) {
        return t.as_ref().map(egui::TextureHandle::id);
    }
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    let tex = projects::load_poster(std::path::Path::new(png)).and_then(|(w, h, px)| {
        let size = [usize::try_from(w).ok()?, usize::try_from(h).ok()?];
        let img = egui::ColorImage::from_rgba_unmultiplied(size, &px);
        Some(ctx.load_texture(format!("poster-{key}"), img, egui::TextureOptions::LINEAR))
    });
    if rt.textures.len() > 512 {
        rt.textures.clear();
    }
    let id = tex.as_ref().map(egui::TextureHandle::id);
    rt.textures.insert(key, tex);
    id
}

fn date(app: &FilmcraftApp, secs: Option<u64>) -> String {
    let Some(secs) = secs.and_then(|s| i64::try_from(s).ok()) else { return "—".into() };
    let offset = app.session.persistence.as_ref().map_or(0, |p| (p.local_offset)(secs));
    let s = projects::iso_local(secs, offset);
    s.get(..16).unwrap_or(&s).replace('T', " ")
}

fn fps_text(fps: f64) -> String {
    if fps <= 0.0 {
        "—".into()
    } else if (fps - fps.round()).abs() < 0.005 {
        format!("{}", fps.round())
    } else {
        format!("{fps:.3}")
    }
}

fn res_text(s: &ProjectSummary) -> String {
    if s.width == 0 || s.height == 0 { "—".into() } else { format!("{} × {}", s.width, s.height) }
}

// ------------------------------------------------------------------ drawing

/// What a click asked for (run after drawing).
enum Act {
    Cmd(String, Value),
    Select(Option<String>),
    Folder(String),
    Open(Entry),
}

#[allow(clippy::too_many_arguments)]
fn button(ui: &mut egui::Ui, auto: &mut Registry, rect: Rect, id: &str, label: &str, primary: bool, t: &Tokens, enabled: bool) -> bool {
    let b = egui::Button::new(egui::RichText::new(label).size(13.0).color(if primary { Color32::WHITE } else { t.text })).fill(if primary {
        t.accent
    } else {
        t.control_surface
    });
    let r = ui.add_enabled_ui(enabled, |ui| ui.put(rect, b)).inner;
    auto.add_response(id, &r, label);
    r.clicked()
}

#[allow(clippy::too_many_arguments)]
fn icon_button(ui: &mut egui::Ui, auto: &mut Registry, rect: Rect, id: &str, icon: Icon, active: bool, t: &Tokens, tip: &str) -> bool {
    let r = ui.interact(rect, egui::Id::new(id), Sense::click()).on_hover_text(tip);
    if r.hovered() || active {
        ui.painter().rect_filled(rect, 4.0, if active { t.pill_active_bg } else { t.hover });
    }
    icons::paint(ui.painter(), rect.shrink(rect.height() * 0.22), icon, if active { t.icon_active } else { t.icon });
    auto.add(id, rect, tip);
    r.clicked()
}

/// Draw the screen over the whole window.
pub fn draw(app: &mut FilmcraftApp, ui: &mut egui::Ui, full: Rect) {
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    // a project loaded some other way (recovery, File ▸ Open from an agent) closes the screen
    if project_ptr(app) != app.home_rt.shown_project && app.session.history.undo.is_empty() && projects::project_open(&app.session) {
        app.ui.home.open = false;
        return;
    }
    refresh(app, &ctx, now);
    backfill_posters(app, &ctx);
    let t = app.tokens;
    let mut acts: Vec<Act> = Vec::new();
    let p = ui.painter().clone();
    let resolve_bg = Color32::from_rgb(0x21, 0x21, 0x26);
    let body_bg = if t.kind == crate::theme::ThemeKind::Resolve { resolve_bg } else { t.app_bg };
    p.rect_filled(full, 0.0, body_bg);

    // ---- title row: × , Local tab, title
    let title = Rect::from_min_size(full.min, vec2(full.width(), TITLE_H));
    p.rect_filled(title, 0.0, t.header_bg);
    let drag = ui.interact(title, egui::Id::new("projects-title-drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    let x_left = title.min.x + if app.integrated_titlebar { 84.0 } else { 8.0 };
    let xr = Rect::from_min_size(pos2(x_left, title.min.y + 6.0), vec2(24.0, 24.0));
    if icon_button(ui, &mut app.auto, xr, "projects.close.x", Icon::Close, false, &t, tl!("Quit Revolution")) {
        // the Project Manager's × quits FilmCraft (owner, 2026-10-14; unsaved changes ask first)
        acts.push(Act::Cmd("app.quit".into(), json!({})));
    }
    let local = Rect::from_min_size(pos2(xr.max.x + 14.0, title.min.y + 4.0), vec2(70.0, TITLE_H - 4.0));
    p.text(local.center(), Align2::CENTER_CENTER, tl!("Local"), Tokens::ui(13.0), t.text);
    p.line_segment([pos2(local.min.x + 10.0, local.max.y - 1.0), pos2(local.max.x - 10.0, local.max.y - 1.0)], Stroke::new(2.0, t.accent));
    app.auto.add("projects.tab.local", local, "Local");
    // Revolution's logo left of the name, where Resolve shows its mark (owner, §1a.108)
    let name = p.layout_no_wrap("Revolution".into(), Tokens::semibold(14.0), t.text);
    let logo_side = 18.0;
    let left = title.center().x - (logo_side + 8.0 + name.size().x) / 2.0;
    crate::app_logo::paint(&ctx, &p, Rect::from_min_size(pos2(left, title.center().y - logo_side / 2.0), vec2(logo_side, logo_side)), Color32::WHITE);
    p.galley(pos2(left + logo_side + 8.0, title.center().y - name.size().y / 2.0), name, t.text);

    // ---- row 2: sidebar, breadcrumb | new folder, size, sort, info, grid / list, search
    let bar = Rect::from_min_size(pos2(full.min.x, title.max.y), vec2(full.width(), BAR_H));
    p.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.separator));
    let mut h = app.ui.home.clone();
    let cy = bar.center().y;
    let sb = Rect::from_center_size(pos2(bar.min.x + 20.0, cy), vec2(26.0, 26.0));
    if icon_button(ui, &mut app.auto, sb, "projects.sidebar", Icon::Hamburger, h.sidebar, &t, tl!("Show the sidebar")) {
        h.sidebar = !h.sidebar;
    }
    let mut x = sb.max.x + 12.0;
    let crumbs: Vec<String> = h.folder.split('/').filter(|c| !c.is_empty()).map(str::to_string).collect();
    let crumb = |ui: &mut egui::Ui, auto: &mut Registry, x: &mut f32, id: &str, text: &str, last: bool| -> bool {
        let g = p.layout_no_wrap(text.to_string(), Tokens::ui(13.0), if last { t.text } else { t.text_dim });
        let r = Rect::from_min_size(pos2(*x, cy - 11.0), vec2(g.size().x + 8.0, 22.0));
        let resp = ui.interact(r, egui::Id::new(id), Sense::click());
        if resp.hovered() {
            p.rect_filled(r, 3.0, t.hover);
        }
        p.galley(pos2(r.min.x + 4.0, cy - g.size().y / 2.0), g, t.text);
        auto.add(id, r, text);
        *x = r.max.x + 2.0;
        resp.clicked()
    };
    if crumb(ui, &mut app.auto, &mut x, "projects.crumb.root", tl!("Projects"), crumbs.is_empty()) {
        acts.push(Act::Folder(String::new()));
    }
    for (i, c) in crumbs.iter().enumerate() {
        p.text(pos2(x + 4.0, cy), Align2::LEFT_CENTER, "›", Tokens::ui(13.0), t.text_dim);
        x += 14.0;
        if crumb(ui, &mut app.auto, &mut x, &format!("projects.crumb.{i}"), c, i + 1 == crumbs.len()) {
            acts.push(Act::Folder(crumbs.get(..=i).unwrap_or_default().join("/")));
        }
    }
    // right side, from the right edge
    let mut rx = bar.max.x - 10.0;
    let search = Rect::from_min_max(pos2(rx - 200.0, cy - 12.0), pos2(rx, cy + 12.0));
    let sr = ui.put(search, egui::TextEdit::singleline(&mut h.search).hint_text(tl!("Search")));
    app.auto.add_response("projects.search", &sr, "Search");
    rx = search.min.x - 10.0;
    for (id, icon, list, tip) in
        [("projects.view.list", Icon::ListView, true, tl!("List view")), ("projects.view.grid", Icon::IconView, false, tl!("Grid view"))]
    {
        let r = Rect::from_min_max(pos2(rx - 26.0, cy - 13.0), pos2(rx, cy + 13.0));
        if icon_button(ui, &mut app.auto, r, id, icon, h.list_view == list, &t, tip) {
            h.list_view = list;
        }
        rx = r.min.x - 4.0;
    }
    let info = Rect::from_min_max(pos2(rx - 30.0, cy - 13.0), pos2(rx - 4.0, cy + 13.0));
    if icon_button(ui, &mut app.auto, info, "projects.info", Icon::Info, h.info, &t, tl!("Project info")) {
        h.info = !h.info;
    }
    rx = info.min.x - 10.0;
    let sort_rect = Rect::from_min_max(pos2(rx - 150.0, cy - 12.0), pos2(rx, cy + 12.0));
    let sort_label = |s: &str| match s {
        "modified" => tl!("Date modified"),
        "created" => tl!("Date created"),
        _ => tl!("Name"),
    };
    let mut sort_items: Vec<(String, egui::Response, String)> = Vec::new();
    let combo = ui
        .scope_builder(egui::UiBuilder::new().max_rect(sort_rect), |ui| {
            egui::ComboBox::from_id_salt("projects.sort")
                .width(sort_rect.width() - 8.0)
                .selected_text(format!("{} {}", tl!("Sort:"), sort_label(&h.sort)))
                .show_ui(ui, |ui| {
                    for key in SORT_KEYS {
                        let r = ui.selectable_value(&mut h.sort, key.to_string(), sort_label(key));
                        sort_items.push((format!("projects.sort.{key}"), r, sort_label(key).to_string()));
                    }
                    ui.separator();
                    let r = ui.checkbox(&mut h.descending, tl!("Reverse order"));
                    sort_items.push(("projects.sort.descending".into(), r, tl!("Reverse order").to_string()));
                })
        })
        .inner;
    app.auto.add_response("projects.sort", &combo.response, "Sort");
    for (id, r, label) in &sort_items {
        app.auto.add_response(id, r, label);
    }
    rx = sort_rect.min.x - 10.0;
    let slider = Rect::from_min_max(pos2(rx - 110.0, cy - 10.0), pos2(rx, cy + 10.0));
    let sl = ui.put(slider, egui::Slider::new(&mut h.thumb, 0.6..=1.6).show_value(false));
    app.auto.add_response("projects.thumbSize", &sl, "Thumbnail size");
    rx = slider.min.x - 8.0;
    let nf = Rect::from_min_max(pos2(rx - 26.0, cy - 13.0), pos2(rx, cy + 13.0));
    if icon_button(ui, &mut app.auto, nf, "projects.newFolder", Icon::Folder, false, &t, tl!("New Folder")) {
        acts.push(Act::Cmd("projects.newFolder".into(), json!({})));
    }
    h.thumb = if h.thumb.is_finite() { h.thumb.clamp(0.6, 1.6) } else { 1.0 };

    // ---- footer
    let footer = Rect::from_min_max(pos2(full.min.x, full.max.y - FOOTER_H), full.max);
    p.rect_filled(footer, 0.0, t.header_bg);
    p.line_segment([footer.left_top(), footer.right_top()], Stroke::new(1.0, t.separator));
    let fy = footer.center().y;
    let sel = selected_summary(app);
    let has_sel = app.ui.home.selected.is_some();
    let readable = sel.as_ref().is_some_and(|s| s.error.is_none());
    let mut fx = footer.min.x + 14.0;
    for (id, label, enabled) in [
        ("projects.export", tl!("Export"), readable),
        ("projects.import", tl!("Import"), true),
        ("projects.package", tl!("Package"), readable),
        ("projects.importPackage", tl!("Import Package"), true),
    ] {
        // wide enough for the label on one line (translations run longer)
        let text_w = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), Color32::WHITE).size().x;
        let r = Rect::from_min_size(pos2(fx, fy - 14.0), vec2((text_w + 28.0).max(90.0), 28.0));
        if button(ui, &mut app.auto, r, id, label, false, &t, enabled) {
            acts.push(Act::Cmd(id.into(), json!({})));
        }
        fx = r.max.x + 8.0;
    }
    let mut fr = footer.max.x - 14.0;
    for (id, label, primary, enabled) in [
        ("projects.close", tl!("Close"), false, h.can_return || projects::project_open(&app.session)),
        ("projects.open", tl!("Open"), false, has_sel),
        ("projects.new", tl!("New Project"), true, true),
    ] {
        let r = Rect::from_min_max(pos2(fr - 110.0, fy - 14.0), pos2(fr, fy + 14.0));
        if button(ui, &mut app.auto, r, id, label, primary, &t, enabled) {
            acts.push(Act::Cmd(id.into(), json!({})));
        }
        fr = r.min.x - 8.0;
    }

    // ---- body: sidebar | grid / list | info
    let mut body = Rect::from_min_max(pos2(full.min.x, bar.max.y), pos2(full.max.x, footer.min.y));
    let listing = app.home_rt.listing.clone().unwrap_or_default();
    if h.sidebar {
        let side = Rect::from_min_size(body.min, vec2(SIDEBAR_W, body.height()));
        p.rect_filled(side, 0.0, t.panel_bg);
        let mut y = side.min.y + 10.0;
        let mut row = |ui: &mut egui::Ui, auto: &mut Registry, id: &str, text: &str, active: bool, indent: f32| -> bool {
            let r = Rect::from_min_size(pos2(side.min.x + 6.0, y), vec2(SIDEBAR_W - 12.0, 24.0));
            let resp = ui.interact(r, egui::Id::new(id), Sense::click());
            if active || resp.hovered() {
                p.rect_filled(r, 3.0, if active { t.row_selected } else { t.hover });
            }
            p.text(pos2(r.min.x + 8.0 + indent, r.center().y), Align2::LEFT_CENTER, text, Tokens::ui(13.0), t.text);
            auto.add(id, r, text);
            y += 26.0;
            resp.clicked()
        };
        if row(ui, &mut app.auto, "projects.sidebar.root", tl!("Projects"), h.folder.is_empty(), 0.0) {
            acts.push(Act::Folder(String::new()));
        }
        if h.folder.is_empty() {
            for (i, f) in listing.folders.iter().enumerate().take(40) {
                if row(ui, &mut app.auto, &format!("projects.sidebar.{i}"), &f.name, false, 14.0) {
                    acts.push(Act::Folder(f.folder.clone()));
                }
            }
        }
        body.min.x = side.max.x;
    }
    if h.info {
        let pane = Rect::from_min_max(pos2(body.max.x - INFO_W, body.min.y), body.max);
        p.rect_filled(pane, 0.0, t.panel_bg);
        info_pane(app, ui, pane, sel.as_ref(), &t);
        body.max.x = pane.min.x;
    }
    let shown = entries(&listing);
    let mut budget = POSTER_LOADS_PER_FRAME;
    let body_ui_rect = body.shrink2(vec2(16.0, 10.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body_ui_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    if let Some(w) = &listing.warning {
        child.colored_label(t.hot_text, w.as_str());
    }
    if let Some(e) = &app.home_rt.error {
        child.colored_label(t.text_dim, e.as_str());
    } else if !listing.exists && listing.folder.is_empty() {
        child.colored_label(t.text_dim, tlf!("Your projects folder ({path}) is made when you create the first project.", path = listing.root));
    }
    egui::ScrollArea::vertical().id_salt("projects-body").auto_shrink([false, false]).show(&mut child, |ui| {
        if h.list_view {
            list_view(app, ui, &shown, &mut acts, &t, &mut budget);
        } else {
            grid_view(app, ui, &shown, &mut acts, &t, &mut budget, h.thumb, !listing.other.is_empty());
        }
    });
    // only commit what the widgets changed if nothing else changed the state meanwhile
    if app.ui.home.open {
        let dialog = app.ui.home.dialog.clone();
        app.ui.home = HomeState { dialog, ..h };
    }

    // ---- keys
    let typing = ctx.memory(|m| m.focused().is_some());
    if app.ui.home.dialog.is_none() {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            acts.push(Act::Cmd("projects.close".into(), json!({})));
        } else if !typing && ctx.input(|i| i.key_pressed(egui::Key::Enter)) && app.ui.home.selected.is_some() {
            acts.push(Act::Cmd("projects.open".into(), json!({})));
        }
    }
    dialogs(app, &ctx, &mut acts);
    for a in acts {
        match a {
            Act::Cmd(id, params) => {
                let _ = crate::menus::invoke(app, &ctx, &id, params);
            }
            Act::Select(s) => app.ui.home.selected = s,
            Act::Folder(f) => {
                app.ui.home.folder = f;
                app.ui.home.selected = None;
            }
            Act::Open(Entry::Folder(f)) => {
                app.ui.home.folder = f.folder;
                app.ui.home.selected = None;
            }
            Act::Open(Entry::Project(s, _)) => {
                app.ui.home.selected = Some(s.path.clone());
                if s.missing {
                    let _ = crate::menus::invoke(app, &ctx, "projects.locate", json!({"path": s.path}));
                } else if s.error.is_none() {
                    let _ = crate::menus::invoke(app, &ctx, "projects.open", json!({"path": s.path}));
                } else {
                    app.ui.status = tlf!("Can't read {name}: {error}", name = s.name, error = s.error.unwrap_or_default());
                }
            }
        }
    }
}

fn info_pane(app: &mut FilmcraftApp, ui: &mut egui::Ui, pane: Rect, sel: Option<&ProjectSummary>, t: &Tokens) {
    let p = ui.painter().clone();
    let mut y = pane.min.y + 14.0;
    let x = pane.min.x + 14.0;
    let Some(s) = sel else {
        p.text(pos2(x, y), Align2::LEFT_TOP, tl!("Select a project to see its details."), Tokens::ui(12.0), t.text_dim);
        return;
    };
    p.text(pos2(x, y), Align2::LEFT_TOP, &s.name, Tokens::semibold(14.0), t.text);
    y += 26.0;
    let rows = [
        (tl!("Date modified"), date(app, s.modified)),
        (tl!("Date created"), date(app, s.created)),
        (tl!("Resolution"), res_text(s)),
        (tl!("Frame rate"), fps_text(s.fps)),
        (tl!("Timelines"), s.sequence_count.to_string()),
        (tl!("Location"), s.path.clone()),
    ];
    for (k, v) in rows {
        p.text(pos2(x, y), Align2::LEFT_TOP, k, Tokens::ui(11.0), t.text_dim);
        y += 15.0;
        let g = p.layout(v, Tokens::ui(12.0), t.text, pane.width() - 28.0);
        let gh = g.size().y;
        p.galley(pos2(x, y), g, t.text);
        y += gh + 8.0;
    }
    if let Some(e) = &s.error {
        let g = p.layout(tlf!("Can't read: {error}", error = e), Tokens::ui(12.0), t.hot_text, pane.width() - 28.0);
        p.galley(pos2(x, y), g, t.hot_text);
    }
    app.auto.add("projects.infoPane", pane, "Project info");
}

#[allow(clippy::too_many_arguments)]
fn grid_view(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    shown: &[(String, Entry)],
    acts: &mut Vec<Act>,
    t: &Tokens,
    budget: &mut usize,
    scale: f32,
    other: bool,
) {
    let (tw, th) = (TILE_W * scale, TILE_H * scale);
    let (cw, ch, gap) = (tw, th + 26.0, 18.0);
    // the scroll area clips at its edges: the outside border needs room above and left of the tiles
    ui.add_space(BORDER_ROOM);
    let avail = (ui.available_width() - 2.0 * BORDER_ROOM).max(cw);
    let cols = (((avail + gap) / (cw + gap)).floor() as usize).max(1);
    let ctx = ui.ctx().clone();
    // the New Project tile, then folders and projects; recent projects elsewhere in their own group
    let split = shown.iter().position(|(id, _)| id.starts_with("projects.other.")).unwrap_or(shown.len());
    let (local, elsewhere) = shown.split_at_checked(split).unwrap_or((shown, &[]));
    let groups: [(&[(String, Entry)], bool); 2] = [(local, true), (elsewhere, false)];
    for (gi, (items, with_new)) in groups.into_iter().enumerate() {
        if gi == 1 {
            if !other {
                break;
            }
            ui.add_space(14.0);
            ui.label(egui::RichText::new(tl!("Other locations")).size(13.0).color(t.text_dim));
            ui.add_space(6.0);
        }
        let n = items.len() + usize::from(with_new);
        let rows = n.div_ceil(cols);
        let height = rows as f32 * (ch + gap);
        let (area, _) = ui.allocate_exact_size(vec2(avail + 2.0 * BORDER_ROOM, height), Sense::hover());
        let area = area.with_min_x(area.min.x + BORDER_ROOM);
        let p = ui.painter().clone();
        for k in 0..n {
            let (r, c) = (k / cols, k % cols);
            let cell = Rect::from_min_size(pos2(area.min.x + c as f32 * (cw + gap), area.min.y + r as f32 * (ch + gap)), vec2(cw, ch));
            if !ui.is_rect_visible(cell) {
                continue;
            }
            let thumb = Rect::from_min_size(cell.min, vec2(tw, th));
            if with_new && k == 0 {
                let resp = ui.interact(cell, egui::Id::new("projects.tile.new"), Sense::click());
                p.rect_filled(thumb, 4.0, if resp.hovered() { t.hover } else { t.panel_bg });
                p.rect_stroke(thumb, 4.0, Stroke::new(1.0, t.separator), StrokeKind::Inside);
                // Revolution's logo with a plus badge, as Resolve's New Project tile (owner, §1a.108)
                let side = th * 0.42;
                let logo = Rect::from_center_size(thumb.center(), vec2(side, side));
                crate::app_logo::paint(&ctx, &p, logo, Color32::from_gray(200));
                let badge = pos2(logo.max.x - side * 0.04, logo.max.y - side * 0.04);
                let br = side * 0.2;
                p.circle_filled(badge, br, t.panel_bg);
                p.circle_stroke(badge, br, Stroke::new(1.5, t.text));
                p.line_segment([pos2(badge.x - br * 0.5, badge.y), pos2(badge.x + br * 0.5, badge.y)], Stroke::new(1.5, t.text));
                p.line_segment([pos2(badge.x, badge.y - br * 0.5), pos2(badge.x, badge.y + br * 0.5)], Stroke::new(1.5, t.text));
                p.text(pos2(cell.min.x + 2.0, thumb.max.y + 6.0), Align2::LEFT_TOP, tl!("New Project"), Tokens::ui(13.0), t.text);
                app.auto.add("projects.tile.new", cell, "New Project");
                if resp.clicked() {
                    acts.push(Act::Cmd("projects.new".into(), json!({})));
                }
                continue;
            }
            let Some((id, e)) = items.get(k - usize::from(with_new)) else { continue };
            let resp = ui.interact(cell, egui::Id::new(id), Sense::click());
            let selected = app.ui.home.selected.as_deref() == Some(e.path());
            p.rect_filled(thumb, 4.0, t.panel_bg);
            let mut dim = false;
            match e {
                Entry::Folder(_) => icons::paint(&p, Rect::from_center_size(thumb.center(), vec2(th * 0.42, th * 0.42)), Icon::Folder, t.icon),
                Entry::Project(s, _) => {
                    if let Some(tex) = poster_texture(&mut app.home_rt, &ctx, s, budget) {
                        p.image(tex, thumb.shrink(1.0), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    } else if s.missing {
                        dim = true;
                        p.text(thumb.center(), Align2::CENTER_CENTER, tl!("Missing"), Tokens::ui(13.0), t.text_dim);
                    } else if s.error.is_some() {
                        dim = true;
                        p.text(thumb.center(), Align2::CENTER_CENTER, tl!("Can't read"), Tokens::ui(13.0), t.hot_text);
                    } else {
                        // no poster yet (one is being made, §1a.108): the logo, faded, as in Resolve
                        crate::app_logo::paint(&ctx, &p, Rect::from_center_size(thumb.center(), vec2(th * 0.42, th * 0.42)), Color32::from_gray(110));
                    }
                }
            }
            if selected {
                p.rect_stroke(thumb, 4.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
            } else if resp.hovered() {
                p.rect_stroke(thumb, 4.0, Stroke::new(1.0, t.text_dim), StrokeKind::Outside);
            }
            let name_col = if dim { t.text_dim } else { t.text };
            let g = p.layout(e.name().to_string(), Tokens::ui(13.0), name_col, cw - 4.0);
            p.galley(pos2(cell.min.x + 2.0, thumb.max.y + 6.0), g, name_col);
            app.auto.add(id, cell, e.name());
            tile_response(app, &resp, e, acts);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn menu_item(ui: &mut egui::Ui, auto: &mut Registry, path: &str, mid: &str, label: &str, cmd: &str, params: Value, acts: &mut Vec<Act>) {
    let r = ui.button(label);
    auto.add_response(&format!("projects.menu.{mid}"), &r, label);
    if r.clicked() {
        acts.push(Act::Select(Some(path.to_string())));
        acts.push(Act::Cmd(cmd.into(), params));
        ui.close();
    }
}

fn tile_response(app: &mut FilmcraftApp, resp: &egui::Response, e: &Entry, acts: &mut Vec<Act>) {
    if resp.double_clicked() {
        acts.push(Act::Open(e.clone()));
    } else if resp.clicked() || resp.secondary_clicked() {
        acts.push(Act::Select(Some(e.path().to_string())));
    }
    let auto = &mut app.auto;
    resp.context_menu(|ui| {
        let path = e.path().to_string();
        match e {
            Entry::Folder(_) => {
                let r = ui.button(tl!("Open"));
                auto.add_response("projects.menu.open", &r, "Open");
                if r.clicked() {
                    acts.push(Act::Open(e.clone()));
                    ui.close();
                }
                menu_item(ui, auto, &path, "rename", tl!("Rename…"), "projects.rename", json!({"path": path}), acts);
                menu_item(ui, auto, &path, "reveal", tl!("Reveal in File Manager"), "projects.reveal", json!({"path": path}), acts);
                ui.separator();
                menu_item(ui, auto, &path, "trash", tl!("Move to Trash…"), "projects.trash", json!({}), acts);
            }
            Entry::Project(s, other) => {
                if s.missing {
                    menu_item(ui, auto, &path, "locate", tl!("Locate…"), "projects.locate", json!({"path": path}), acts);
                    menu_item(ui, auto, &path, "remove", tl!("Remove from List"), "projects.removeFromList", json!({"path": path}), acts);
                    return;
                }
                let r = ui.add_enabled(s.error.is_none(), egui::Button::new(tl!("Open")));
                auto.add_response("projects.menu.open", &r, "Open");
                if r.clicked() {
                    acts.push(Act::Open(e.clone()));
                    ui.close();
                }
                menu_item(ui, auto, &path, "rename", tl!("Rename…"), "projects.rename", json!({"path": path}), acts);
                menu_item(ui, auto, &path, "duplicate", tl!("Duplicate"), "projects.duplicate", json!({"path": path}), acts);
                menu_item(ui, auto, &path, "reveal", tl!("Reveal in File Manager"), "projects.reveal", json!({"path": path}), acts);
                if s.error.is_none() {
                    menu_item(ui, auto, &path, "export", tl!("Export…"), "projects.export", json!({"path": path}), acts);
                    menu_item(ui, auto, &path, "package", tl!("Package…"), "projects.package", json!({"path": path}), acts);
                    menu_item(ui, auto, &path, "package-file", tl!("Package as One File…"), "projects.package", json!({"path": path, "format": "file"}), acts);
                }
                ui.separator();
                if *other {
                    menu_item(ui, auto, &path, "remove", tl!("Remove from List"), "projects.removeFromList", json!({"path": path}), acts);
                }
                menu_item(ui, auto, &path, "trash", tl!("Move to Trash…"), "projects.trash", json!({}), acts);
            }
        }
    });
}

fn list_view(app: &mut FilmcraftApp, ui: &mut egui::Ui, shown: &[(String, Entry)], acts: &mut Vec<Act>, t: &Tokens, budget: &mut usize) {
    let _ = budget;
    let w = ui.available_width();
    let cols: [(&str, f32); 6] = [
        (tl!("Name"), 0.30),
        (tl!("Date modified"), 0.17),
        (tl!("Date created"), 0.17),
        (tl!("Resolution"), 0.13),
        (tl!("Frame rate"), 0.11),
        (tl!("Timelines"), 0.12),
    ];
    let (head, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
    let p = ui.painter().clone();
    let mut x = head.min.x;
    for (label, f) in cols {
        p.text(pos2(x + 6.0, head.center().y), Align2::LEFT_CENTER, label, Tokens::semibold(12.0), t.text_dim);
        x += w * f;
    }
    p.line_segment([head.left_bottom(), head.right_bottom()], Stroke::new(1.0, t.separator));
    let (nr, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    let resp = ui.interact(nr, egui::Id::new("projects.tile.new"), Sense::click());
    if resp.hovered() {
        p.rect_filled(nr, 0.0, t.hover);
    }
    p.text(pos2(nr.min.x + 6.0, nr.center().y), Align2::LEFT_CENTER, format!("+  {}", tl!("New Project")), Tokens::ui(13.0), t.text);
    app.auto.add("projects.tile.new", nr, "New Project");
    if resp.clicked() {
        acts.push(Act::Cmd("projects.new".into(), json!({})));
    }
    for (i, (id, e)) in shown.iter().enumerate() {
        let (row, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
        if !ui.is_rect_visible(row) {
            continue;
        }
        let resp = ui.interact(row, egui::Id::new(id), Sense::click());
        let selected = app.ui.home.selected.as_deref() == Some(e.path());
        if selected {
            p.rect_filled(row, 0.0, t.row_selected);
        } else if resp.hovered() {
            p.rect_filled(row, 0.0, t.hover);
        } else if i % 2 == 1 {
            p.rect_filled(row, 0.0, t.row_alt);
        }
        let cells: Vec<String> = match e {
            Entry::Folder(f) => vec![format!("▸ {}", f.name), date(app, f.modified), date(app, f.created), String::new(), String::new(), String::new()],
            Entry::Project(s, _) => {
                let name = if s.missing {
                    format!("{} ({})", s.name, tl!("Missing"))
                } else if s.error.is_some() {
                    format!("{} ({})", s.name, tl!("Can't read"))
                } else {
                    s.name.clone()
                };
                vec![name, date(app, s.modified), date(app, s.created), res_text(s), fps_text(s.fps), s.sequence_count.to_string()]
            }
        };
        let dim = matches!(e, Entry::Project(s, _) if s.missing || s.error.is_some());
        let mut x = row.min.x;
        for ((_, f), text) in cols.iter().zip(cells) {
            let clip = Rect::from_min_size(pos2(x, row.min.y), vec2(w * f - 4.0, row.height()));
            p.with_clip_rect(clip).text(pos2(x + 6.0, row.center().y), Align2::LEFT_CENTER, text, Tokens::ui(12.5), if dim { t.text_dim } else { t.text });
            x += w * f;
        }
        app.auto.add(id, row, e.name());
        tile_response(app, &resp, e, acts);
    }
}

fn dialogs(app: &mut FilmcraftApp, ctx: &egui::Context, acts: &mut Vec<Act>) {
    let Some(mut d) = app.ui.home.dialog.clone() else { return };
    let title = match d.kind.as_str() {
        "newProject" => tl!("New Project"),
        "newFolder" => tl!("New Folder"),
        "rename" => tl!("Rename"),
        "trash" => tl!("Move to Trash"),
        _ => tl!("Unsaved Changes"),
    };
    let mut close = false;
    let mut ok = false;
    let mut save = false;
    let mut discard = false;
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    egui::Window::new(title).id(egui::Id::new("projects-dialog")).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(
        ctx,
        |ui| {
            ui.set_min_width(340.0);
            match d.kind.as_str() {
                "newProject" | "newFolder" | "rename" => {
                    ui.label(if d.kind == "newFolder" { tl!("Folder name") } else { tl!("Name") });
                    let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(320.0));
                    app.auto.add_response("projects.dialog.name", &r, "Name");
                    if !r.has_focus() && d.error.is_empty() && !ui.memory(|m| m.focused().is_some()) {
                        r.request_focus();
                    }
                    ok |= r.lost_focus() && enter;
                }
                "trash" => {
                    ui.label(tlf!("Move \u{201c}{name}\u{201d} to the trash? You can restore it from the trash later.", name = d.name));
                }
                _ => {
                    ui.label(tlf!("Save the changes to \u{201c}{name}\u{201d} first?", name = app.session.project.name));
                }
            }
            if !d.error.is_empty() {
                ui.colored_label(app.tokens.hot_text, &d.error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if d.kind == "unsaved" {
                    let r = ui.button(tl!("Save"));
                    app.auto.add_response("projects.dialog.save", &r, "Save");
                    save = r.clicked();
                    let r = ui.button(tl!("Don't Save"));
                    app.auto.add_response("projects.dialog.discard", &r, "Don't Save");
                    discard = r.clicked();
                } else {
                    let label = match d.kind.as_str() {
                        "newProject" | "newFolder" => tl!("Create"),
                        "rename" => tl!("Rename"),
                        _ => tl!("Move to Trash"),
                    };
                    let r = ui.button(label);
                    app.auto.add_response("projects.dialog.ok", &r, label);
                    ok |= r.clicked();
                }
                let r = ui.button(tl!("Cancel"));
                app.auto.add_response("projects.dialog.cancel", &r, "Cancel");
                close = r.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        },
    );
    if close {
        app.ui.home.dialog = None;
        return;
    }
    let result = if ok {
        match d.kind.as_str() {
            "newProject" => Some(route(app, "projects.new", &json!({"name": d.name, "folder": app.ui.home.folder}))),
            "newFolder" => Some(route(app, "projects.newFolder", &json!({"name": d.name, "folder": app.ui.home.folder}))),
            "rename" => Some(route(app, "projects.rename", &json!({"path": d.path, "name": d.name}))),
            "trash" => Some(route(app, "projects.trash", &json!({"path": d.path}))),
            _ => None,
        }
        .flatten()
    } else if save || discard {
        let saved = if save {
            if app.session.path.is_some() {
                app.session.execute("file.save", json!({})).map_err(|e| e.to_string())
            } else {
                match app.file_dialog("file.saveAs", &json!({})) {
                    Ok(Value::Null) => Err(tl!("The project was not saved.").to_string()),
                    r => r,
                }
            }
        } else {
            Ok(Value::Null)
        };
        match (saved, d.then.clone()) {
            (Err(e), _) => Some(Err(e)),
            (Ok(_), Some((cmd, mut params))) => {
                params["discard"] = json!(true);
                app.ui.home.dialog = None;
                acts.push(Act::Cmd(cmd, params));
                return;
            }
            (Ok(_), None) => Some(Ok(Value::Null)),
        }
    } else {
        app.ui.home.dialog = Some(d);
        return;
    };
    match result {
        Some(Err(e)) => {
            d.error = e;
            // a route that opened another dialog (unsaved) keeps that one
            if app.ui.home.dialog.as_ref().is_none_or(|x| x.kind == d.kind) {
                app.ui.home.dialog = Some(d);
            }
        }
        Some(Ok(v)) => {
            if v.get("dialog").is_none() {
                app.ui.home.dialog = None;
            }
            if d.kind == "trash" || d.kind == "rename" {
                app.ui.home.selected = None;
            }
        }
        None => app.ui.home.dialog = None,
    }
}

/// What Import Package imports for a picked file: a package folder's `manifest.json` stands for
/// its folder; anything else (a `.revpkg`) is itself.
fn package_pick(path: String) -> String {
    let p = std::path::Path::new(&path);
    match p.parent() {
        Some(d) if p.file_name().is_some_and(|n| n == "manifest.json") && !d.as_os_str().is_empty() => d.to_string_lossy().into_owned(),
        _ => path,
    }
}

/// A package size for the status line: "1.4 GB", "820 MB".
fn package_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 { format!("{:.1} GB", mb / 1000.0) } else { format!("{:.0} MB", mb.max(1.0)) }
}

#[cfg(test)]
mod tests {
    use super::{package_pick, package_size};

    #[test]
    fn import_package_takes_a_manifest_as_its_folder() {
        assert_eq!(package_pick("/a/Trip package/manifest.json".into()), "/a/Trip package");
        assert_eq!(package_pick("/a/Trip.fcpackage".into()), "/a/Trip.fcpackage");
        assert_eq!(package_pick("manifest.json".into()), "manifest.json", "no folder to stand for");
        assert_eq!(package_pick("/a/other.json".into()), "/a/other.json");
    }

    #[test]
    fn package_sizes_read_plainly() {
        assert_eq!(package_size(0), "1 MB");
        assert_eq!(package_size(820_000_000), "820 MB");
        assert_eq!(package_size(1_400_000_000), "1.4 GB");
        assert!(package_size(u64::MAX).ends_with("GB"));
    }
}
