//! Window ▸ Layout Presets (fork LAY.3, owner 2026-10-21), like Resolve's Workspace ▸ Layout
//! Presets: one name for the arrangement of every page (in the Premiere workflow, every
//! workspace) at once. The presets are listed first (choose one to load it), then Save Layout as
//! Preset…, Import Preset… and Manage Layout Presets… (load, update, rename, delete, export).
//!
//! Loading a preset gives each workspace in it that arrangement as its unsaved one (fork LAY.2),
//! so Reset to Saved Layout still goes back to the page's saved layout, and Save Layout keeps it.
//! Presets live in `workspaces.json`; Export… writes one to a `.revlayout` file (JSON) for another
//! computer.
//!
//! Commands, `window.layoutPreset.*`: `save {name?}` (no name: the dialog), `load {name}` (the
//! menu's `load.<slug>`), `update {name}`, `rename {from, to}`, `delete {name}`,
//! `export {name, path?}`, `import {path?}` (no path: a file dialog), `manage`.
//! Automation ids: `layoutPresets.save.name` / `.ok` / `.cancel`, `layoutPresets.row.<i>`,
//! `layoutPresets.load` / `.update` / `.name` / `.rename` / `.delete` / `.export` / `.import` / `.close`.

use egui::Align2;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::{self, SavedWorkspace, slug};
use crate::menus::MenuItem;
use crate::native_dialogs::FileDialog;

/// The preset file's extension and the `format` it carries.
pub const EXT: &str = "revlayout";
const FORMAT: &str = "revolution-layout-preset";
/// At most this many presets; a preset file bigger than this isn't read.
const MAX_PRESETS: usize = 100;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_NAME_CHARS: usize = 80;

/// A named arrangement of every workspace, and the one that was in use.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LayoutPreset {
    pub name: String,
    pub current: String,
    pub layouts: Vec<SavedWorkspace>,
}

/// Layout Presets dialogs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PresetDialog {
    Save {
        name: String,
    },
    /// `selected`: the preset picked in the list; `name`: its new name being typed.
    Manage {
        selected: Option<String>,
        name: String,
    },
}

/// Command ids under `window.layoutPreset.` (the rest are `load.<slug>`).
const ACTIONS: [&str; 8] = ["save", "load", "update", "rename", "delete", "export", "import", "manage"];

pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let key = id.strip_prefix("window.layoutPreset.")?;
    let arg = |k: &str| params.get(k).and_then(Value::as_str).map(str::to_string);
    let r = match key {
        "save" => match arg("name") {
            Some(n) => save(app, &n, false),
            None => {
                app.ui.layout_preset_dialog = Some(PresetDialog::Save { name: tl!("My Layout").into() });
                Ok(json!({"dialog": "saveLayoutPreset"}))
            }
        },
        "update" => save(app, &arg("name").unwrap_or_default(), true),
        "load" => load(app, &arg("name").unwrap_or_default()),
        "rename" => rename(app, &arg("from").unwrap_or_default(), &arg("to").unwrap_or_default()),
        "delete" => delete(app, &arg("name").unwrap_or_default()),
        "export" => export(app, &arg("name").unwrap_or_default(), arg("path")),
        "import" => import(app, arg("path")),
        "manage" => {
            app.ui.layout_preset_dialog = Some(PresetDialog::Manage { selected: None, name: String::new() });
            Ok(json!({"dialog": "manageLayoutPresets", "presets": names(app)}))
        }
        other => match other.strip_prefix("load.").and_then(|s| find(app, s)) {
            Some(n) => load(app, &n),
            None => Err(format!("unknown layout preset `{other}`")),
        },
    };
    Some(r)
}

fn names(app: &FilmcraftApp) -> Vec<String> {
    app.workspaces.presets.iter().map(|p| p.name.clone()).collect()
}

/// The preset called `query` (any case, spaces optional).
fn find(app: &FilmcraftApp, query: &str) -> Option<String> {
    app.workspaces.presets.iter().find(|p| slug(&p.name) == slug(query)).map(|p| p.name.clone())
}

fn as_saved(name: &str, layout: &dock::Layout) -> Result<SavedWorkspace, String> {
    let root = serde_json::to_value(&layout.root).map_err(|e| e.to_string())?;
    let floating = if layout.floating.is_empty() { Value::Null } else { serde_json::to_value(&layout.floating).map_err(|e| e.to_string())? };
    Ok(SavedWorkspace { name: name.to_string(), layout: root, floating })
}

/// Every workspace of the workflow as it is now: the one in use as on screen, the others as they
/// would open.
fn capture(app: &FilmcraftApp, name: &str) -> Result<LayoutPreset, String> {
    let live = dock::Layout { root: app.ui.dock.clone(), floating: app.ui.floating.clone() };
    live.validate()?;
    let mut layouts = Vec::new();
    for ws in dock::names_for(&app.workspaces, app.resolve_workflow()) {
        let layout = if ws == app.ui.workspace { live.clone() } else { dock::current_docking(&app.workspaces, &ws) };
        layouts.push(as_saved(&ws, &layout)?);
    }
    Ok(LayoutPreset { name: name.to_string(), current: app.ui.workspace.clone(), layouts })
}

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the layout preset needs a name".into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("a layout preset name has at most {MAX_NAME_CHARS} characters"));
    }
    if ACTIONS.iter().any(|a| a.eq_ignore_ascii_case(&slug(name))) {
        return Err(format!("`{name}` can't be used as a layout preset name"));
    }
    Ok(name.to_string())
}

/// Save the current arrangement as a new preset, or (`update`) over an existing one.
fn save(app: &mut FilmcraftApp, name: &str, update: bool) -> Result<Value, String> {
    let existing = find(app, name);
    let name = match (&existing, update) {
        (Some(n), true) => n.clone(),
        (None, true) => return Err(format!("unknown layout preset `{}`", name.trim())),
        (Some(n), false) => return Err(format!("a layout preset named `{n}` already exists")),
        (None, false) => clean_name(name)?,
    };
    let preset = capture(app, &name)?;
    let mut next = app.workspaces.clone();
    match next.presets.iter_mut().find(|p| p.name == name) {
        Some(p) => *p = preset,
        None => {
            if next.presets.len() >= MAX_PRESETS {
                return Err(format!("at most {MAX_PRESETS} layout presets; delete one first"));
            }
            next.presets.push(preset);
        }
    }
    app.set_workspaces(next)?;
    Ok(if update { json!({"updated": name}) } else { json!({"saved": name}) })
}

/// Give every workspace in the preset its arrangement and open the one it was saved on.
fn load(app: &mut FilmcraftApp, name: &str) -> Result<Value, String> {
    let preset = find(app, name).and_then(|n| app.workspaces.presets.iter().find(|p| p.name == n).cloned());
    let Some(preset) = preset else { return Err(format!("unknown layout preset `{}`", name.trim())) };
    let mut next = app.workspaces.clone();
    // the workspace on screen keeps its own arrangement if the preset doesn't name it
    let live = dock::Layout { root: app.ui.dock.clone(), floating: app.ui.floating.clone() };
    next.keep_working(&app.ui.workspace, &live);
    let mut applied = 0_usize;
    for saved in &preset.layouts {
        if dock::find(&next, &saved.name).as_deref() != Some(saved.name.as_str()) {
            continue; // a workspace deleted since
        }
        if let Some(layout) = dock::read_layout(saved) {
            next.keep_working(&saved.name, &layout);
            applied = applied.saturating_add(1);
        }
    }
    app.set_workspaces(next)?;
    let target = dock::find(&app.workspaces, &preset.current).unwrap_or_else(|| app.ui.workspace.clone());
    // nothing to keep from the screen now: the arrangement above is the one to open
    app.ui.workspace = String::new();
    app.set_workspace(&target);
    Ok(json!({"loaded": preset.name, "workspaces": applied, "workspace": app.ui.workspace}))
}

fn rename(app: &mut FilmcraftApp, from: &str, to: &str) -> Result<Value, String> {
    let Some(from) = find(app, from) else { return Err(format!("unknown layout preset `{}`", from.trim())) };
    let to = clean_name(to)?;
    if let Some(n) = find(app, &to).filter(|n| *n != from) {
        return Err(format!("a layout preset named `{n}` already exists"));
    }
    let mut next = app.workspaces.clone();
    for p in next.presets.iter_mut().filter(|p| p.name == from) {
        p.name = to.clone();
    }
    app.set_workspaces(next)?;
    Ok(json!({"renamed": from, "to": to}))
}

fn delete(app: &mut FilmcraftApp, name: &str) -> Result<Value, String> {
    let Some(name) = find(app, name) else { return Err(format!("unknown layout preset `{}`", name.trim())) };
    let mut next = app.workspaces.clone();
    next.presets.retain(|p| p.name != name);
    app.set_workspaces(next)?;
    Ok(json!({"deleted": name}))
}

fn write_file(preset: &LayoutPreset, path: &str) -> Result<Value, String> {
    let body = json!({"format": FORMAT, "version": 1, "preset": preset});
    let bytes = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| format!("writing {path}: {e}"))?;
    Ok(json!({"exported": preset.name, "path": path}))
}

fn export(app: &mut FilmcraftApp, name: &str, path: Option<String>) -> Result<Value, String> {
    let preset = find(app, name).and_then(|n| app.workspaces.presets.iter().find(|p| p.name == n).cloned());
    let Some(preset) = preset else { return Err(format!("unknown layout preset `{}`", name.trim())) };
    match path {
        Some(p) => write_file(&preset, &p),
        None => {
            let suggested = format!("{}.{EXT}", preset.name);
            app.pick(FileDialog::save_as("Layout preset", &[EXT], &suggested), move |_app, paths| match paths.first() {
                Some(p) => write_file(&preset, p),
                None => Ok(json!({"cancelled": true})),
            })
        }
    }
}

/// Read a preset file: bounded, its format checked, its name cleaned; layouts that don't read are
/// dropped (a panel a newer version added), the others kept.
pub fn read_file(path: &str) -> Result<LayoutPreset, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| format!("opening {path}: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| format!("reading {path}: {e}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{path} is too big for a layout preset"));
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| format!("{path} isn't a layout preset"))?;
    if v.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(format!("{path} isn't a layout preset"));
    }
    let mut preset: LayoutPreset =
        v.get("preset").cloned().and_then(|p| serde_json::from_value(p).ok()).ok_or_else(|| format!("{path} isn't a layout preset"))?;
    preset.layouts.retain(|l| dock::read_layout(l).is_some());
    preset.layouts.truncate(64);
    if preset.layouts.is_empty() {
        return Err(format!("{path} has no layout this version can open"));
    }
    Ok(preset)
}

/// Add a preset file's preset, renamed "<name> 2"… when the name is taken.
fn add_imported(app: &mut FilmcraftApp, path: &str) -> Result<Value, String> {
    let mut preset = read_file(path)?;
    let base = clean_name(&preset.name).unwrap_or_else(|_| tl!("Imported Layout").to_string());
    let mut name = base.clone();
    let mut k = 2_u32;
    while find(app, &name).is_some() && k < 1000 {
        name = format!("{base} {k}");
        k = k.saturating_add(1);
    }
    preset.name = name.clone();
    let mut next = app.workspaces.clone();
    if next.presets.len() >= MAX_PRESETS {
        return Err(format!("at most {MAX_PRESETS} layout presets; delete one first"));
    }
    next.presets.push(preset);
    app.set_workspaces(next)?;
    Ok(json!({"imported": name}))
}

fn import(app: &mut FilmcraftApp, path: Option<String>) -> Result<Value, String> {
    match path {
        Some(p) => add_imported(app, &p),
        None => app.pick(FileDialog::open_file("Layout preset", &[EXT]), |app, paths| match paths.first() {
            Some(p) => add_imported(app, p),
            None => Ok(json!({"cancelled": true})),
        }),
    }
}

/// Window ▸ Layout Presets: each preset (choose to load), then the commands in table order.
pub fn menu(app: &FilmcraftApp, items: &mut Vec<MenuItem>) {
    let is_lp = |it: &MenuItem| it.path.len() == 2 && it.path[0] == "Window" && it.path[1] == "Layout Presets";
    let Some(at) = items.iter().position(is_lp) else { return };
    let sub: Vec<MenuItem> = app
        .workspaces
        .presets
        .iter()
        .map(|p| MenuItem {
            id: format!("window.layoutPreset.load.{}", slug(&p.name)),
            label: p.name.clone(),
            path: vec!["Window".into(), "Layout Presets".into()],
            shortcut: None,
            enabled: true,
            checked: None,
        })
        .collect();
    items.splice(at..at, sub);
}

/// Save Layout as Preset… and Manage Layout Presets… dialogs.
pub fn dialogs(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.layout_preset_dialog.clone() else { return };
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let mut push = |id: &str, r: &egui::Response, label: &str| elems.push((id.to_string(), r.rect, label.to_string()));
    let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let mut act: Option<(&str, Value)> = None;
    let names = names(app);
    let (title, shown) = match &d {
        PresetDialog::Save { .. } => ("Save Layout as Preset", tl!("Save Layout as Preset")),
        PresetDialog::Manage { .. } => ("Layout Presets", tl!("Layout Presets")),
    };
    egui::Window::new(shown).id(egui::Id::new(title)).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(
        ctx,
        |ui| match &mut d {
            PresetDialog::Save { name } => {
                ui.label(tl!("Saves how every page is arranged now under one name."));
                ui.horizontal(|ui| {
                    ui.label(tl!("Name:"));
                    let r = ui.text_edit_singleline(name);
                    push("layoutPresets.save.name", &r, "Name");
                });
                ui.horizontal(|ui| {
                    let r = ui.button(tl!("Cancel"));
                    push("layoutPresets.save.cancel", &r, "Cancel");
                    close |= r.clicked();
                    let r = ui.button(tl!("OK"));
                    push("layoutPresets.save.ok", &r, "OK");
                    if r.clicked() {
                        act = Some(("window.layoutPreset.save", json!({"name": name.clone()})));
                    }
                });
            }
            PresetDialog::Manage { selected, name } => {
                if names.is_empty() {
                    ui.label(tl!("No layout presets yet: arrange the pages, then Save Layout as Preset…"));
                }
                for (i, n) in names.iter().enumerate() {
                    let r = ui.selectable_label(selected.as_ref() == Some(n), n);
                    push(&format!("layoutPresets.row.{i}"), &r, n);
                    if r.clicked() {
                        *selected = Some(n.clone());
                        *name = n.clone();
                    }
                }
                ui.separator();
                let sel = selected.clone().filter(|s| names.contains(s));
                let has = sel.is_some();
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(has, egui::Button::new(tl!("Load")));
                    push("layoutPresets.load", &r, "Load");
                    if r.clicked()
                        && let Some(s) = &sel
                    {
                        act = Some(("window.layoutPreset.load", json!({"name": s})));
                    }
                    let r = ui.add_enabled(has, egui::Button::new(tl!("Update"))).on_hover_text(tl!("Replace it with how the pages are arranged now"));
                    push("layoutPresets.update", &r, "Update");
                    if r.clicked()
                        && let Some(s) = &sel
                    {
                        act = Some(("window.layoutPreset.update", json!({"name": s})));
                    }
                    let r = ui.add_enabled(has, egui::Button::new(tl!("Export…")));
                    push("layoutPresets.export", &r, "Export…");
                    if r.clicked()
                        && let Some(s) = &sel
                    {
                        act = Some(("window.layoutPreset.export", json!({"name": s})));
                    }
                    let r = ui.button(tl!("Import…"));
                    push("layoutPresets.import", &r, "Import…");
                    if r.clicked() {
                        act = Some(("window.layoutPreset.import", json!({})));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(tl!("Name:"));
                    let r = ui.add_enabled(has, egui::TextEdit::singleline(name));
                    push("layoutPresets.name", &r, "Name");
                    let r = ui.add_enabled(has && sel.as_deref() != Some(name.trim()), egui::Button::new(tl!("Rename")));
                    push("layoutPresets.rename", &r, "Rename");
                    if r.clicked()
                        && let Some(s) = &sel
                    {
                        act = Some(("window.layoutPreset.rename", json!({"from": s, "to": name.clone()})));
                        *selected = Some(name.trim().to_string());
                    }
                });
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(has, egui::Button::new(tl!("Delete")));
                    push("layoutPresets.delete", &r, "Delete");
                    if r.clicked()
                        && let Some(s) = &sel
                    {
                        act = Some(("window.layoutPreset.delete", json!({"name": s})));
                        *selected = None;
                        name.clear();
                    }
                    let r = ui.button(tl!("Close"));
                    push("layoutPresets.close", &r, "Close");
                    close |= r.clicked();
                });
            }
        },
    );
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    let keep_open = matches!(d, PresetDialog::Manage { .. });
    app.ui.layout_preset_dialog = Some(d);
    if let Some((id, p)) = act {
        match route(app, id, &p) {
            Some(Err(e)) => app.ui.status = e,
            Some(Ok(v)) => {
                if let Some(n) = v.get("loaded").and_then(Value::as_str) {
                    app.ui.status = tlf!("Loaded the layout preset {n}.", n);
                }
                close |= !keep_open;
            }
            None => {}
        }
    }
    if close {
        app.ui.layout_preset_dialog = None;
    }
}
