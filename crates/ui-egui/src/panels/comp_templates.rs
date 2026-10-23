//! Saved comp templates on the Compositor canvas (fork C4.7): the background menu's "Save
//! Selection as Template…" (a name dialog → `comp.template.save`, with a note of the nodes a
//! template leaves out) and "Insert Template ▸" (the saved templates → `comp.template.insert` at
//! the click; "Delete Template…" inside it, with a confirm → `comp.template.delete`).
//!
//! Automation ids: `comp.canvas.saveTemplate`, `comp.canvas.insertTemplate` (the submenu),
//! `comp.canvas.insertTemplate.<name>`, `comp.canvas.deleteTemplate` (its submenu),
//! `comp.canvas.deleteTemplate.<name>`; the dialogs' `comp.templateDialog.*`.

use egui::Align2;
use filmcraft_project::ItemKind;
use filmcraft_project::comp::{CompKind, MediaInSource};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::FilmcraftApp;
use crate::automation::Registry;

/// How long the template list is kept before the open menu reads the folder again (seconds).
const REFRESH: f64 = 2.0;

/// The template dialogs' state (`ui.panels.comp.templates`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TemplateMenuState {
    /// The Save as Template dialog, while open.
    pub save: Option<SaveDraft>,
    /// The template the delete confirmation asks about, while open.
    pub delete: Option<String>,
    /// The saved templates' names and when they were read (UI time), for the menu.
    #[serde(skip)]
    pub names: Option<(f64, Vec<String>)>,
}

/// The Save as Template dialog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SaveDraft {
    pub comp: u64,
    /// The open group the nodes are in (C4.8; empty: the comp's own level).
    pub group: Vec<u64>,
    pub nodes: Vec<u64>,
    pub name: String,
    pub replace: bool,
    /// Why the last save was refused.
    pub error: Option<String>,
}

/// What was chosen in the menu this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Save,
    Insert(String, [f32; 2]),
    Delete(String),
}

/// The template entries of the canvas background menu (runs only while it is open; sets `opened`).
pub fn menu(ui: &mut egui::Ui, reg: &mut Registry, names: Option<&[String]>, can_save: bool, at: [f32; 2], pick: &mut Option<Pick>, opened: &mut bool) {
    *opened = true;
    ui.separator();
    let b = ui.add_enabled(can_save, egui::Button::new(tl!("Save Selection as Template…")));
    reg.add_response("comp.canvas.saveTemplate", &b, "Save Selection as Template…");
    if b.clicked() {
        *pick = Some(Pick::Save);
        ui.close();
    }
    let r = ui.menu_button(tl!("Insert Template"), |ui| match names {
        None => {
            ui.label(tl!("Reading templates…"));
        }
        Some([]) => {
            ui.label(tl!("No saved templates"));
        }
        Some(names) => {
            for n in names {
                let b = ui.button(n);
                reg.add_response(&format!("comp.canvas.insertTemplate.{n}"), &b, n);
                if b.clicked() {
                    *pick = Some(Pick::Insert(n.clone(), at));
                    ui.close();
                }
            }
            ui.separator();
            let r = ui.menu_button(tl!("Delete Template…"), |ui| {
                for n in names {
                    let b = ui.button(n);
                    reg.add_response(&format!("comp.canvas.deleteTemplate.{n}"), &b, n);
                    if b.clicked() {
                        *pick = Some(Pick::Delete(n.clone()));
                        ui.close();
                    }
                }
            });
            reg.add_response("comp.canvas.deleteTemplate", &r.response, "Delete Template…");
        }
    });
    reg.add_response("comp.canvas.insertTemplate", &r.response, "Insert Template");
}

/// After the canvas: refresh the open menu's list, act on what was picked, draw the dialogs.
pub fn after_canvas(app: &mut FilmcraftApp, ctx: &egui::Context, comp: u64, path: &[u64], pick: Option<Pick>, opened: bool) {
    let now = ctx.input(|i| i.time);
    let st = &app.ui.panels.comp.templates;
    if opened && st.names.as_ref().is_none_or(|(at, _)| !(now - at).is_finite() || now - at > REFRESH || now < *at) {
        refresh(app, now);
    }
    match pick {
        Some(Pick::Save) => {
            let nodes = app.ui.panels.comp.canvas.selection.clone();
            app.ui.panels.comp.templates.save = Some(SaveDraft { comp, group: path.to_vec(), nodes, ..SaveDraft::default() });
        }
        Some(Pick::Insert(name, at)) => {
            match app.session.execute("comp.template.insert", super::comp::scoped(json!({"comp": comp, "name": name, "at": at}), path)) {
                Ok(v) => {
                    let ids: Vec<u64> = v["nodes"].as_array().map(|a| a.iter().filter_map(serde_json::Value::as_u64).collect()).unwrap_or_default();
                    app.ui.status = tlf!("Inserted template “{name}” ({count} nodes)", name = name, count = ids.len());
                    app.ui.panels.comp.canvas.selection = ids;
                }
                Err(e) => app.session.toast(e.to_string()),
            }
        }
        Some(Pick::Delete(name)) => app.ui.panels.comp.templates.delete = Some(name),
        None => {}
    }
    save_dialog(app, ctx);
    delete_dialog(app, ctx);
}

fn refresh(app: &mut FilmcraftApp, now: f64) {
    let names = app
        .session
        .execute("comp.template.list", json!({"namesOnly": true}))
        .ok()
        .and_then(|v| v["templates"].as_array().map(|a| a.iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect()))
        .unwrap_or_default();
    app.ui.panels.comp.templates.names = Some((now, names));
}

/// What a template of these nodes leaves out, in words (None: nothing).
fn left_out(app: &FilmcraftApp, d: &SaveDraft) -> Option<String> {
    let Some(ItemKind::Comp(top)) = app.session.project.item(filmcraft_project::ItemId(d.comp)).map(|i| &i.kind) else { return None };
    let c = if d.group.is_empty() { &**top } else { filmcraft_project::comp::group::inner(top, &d.group).ok()? };
    let picked = || c.nodes.iter().filter(|n| d.nodes.contains(&n.id));
    let media = picked().filter(|n| matches!(n.kind, CompKind::MediaIn { source: MediaInSource::Item { .. }, .. })).count();
    let out = picked().filter(|n| matches!(n.kind, CompKind::MediaOut)).count();
    let mut parts = Vec::new();
    if media > 0 {
        parts.push(tlf!("{count} Media In node(s) that read a clip will be left out; templates can't hold your clips.", count = media));
    }
    if out > 0 {
        parts.push(tl!("Media Out is never saved in a template.").to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn save_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.panels.comp.templates.save.clone() else { return };
    let note = left_out(app, &d);
    let mut open = true;
    let (mut close, mut apply) = (false, false);
    egui::Window::new(tl!("Save as Template"))
        .id(egui::Id::new("comp-template-save"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(tlf!("{count} selected node(s)", count = d.nodes.len()));
            ui.horizontal(|ui| {
                ui.label(tl!("Name:"));
                let r = ui.text_edit_singleline(&mut d.name);
                app.auto.add("comp.templateDialog.name", r.rect, "Name");
            });
            if let Some(n) = &note {
                ui.label(egui::RichText::new(n).weak());
            }
            let r = ui.checkbox(&mut d.replace, tl!("Replace a template with this name"));
            app.auto.add_response("comp.templateDialog.replace", &r, "Replace a template with this name");
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let c = ui.button(tl!("Cancel"));
                app.auto.add_response("comp.templateDialog.cancel", &c, "Cancel");
                let o = ui.add_enabled(!d.name.trim().is_empty(), egui::Button::new(tl!("Save")));
                app.auto.add_response("comp.templateDialog.save", &o, "Save");
                apply = o.clicked() || (ui.input(|i| i.key_pressed(egui::Key::Enter)) && !d.name.trim().is_empty());
                close = c.clicked();
            });
        });
    if close || !open {
        app.ui.panels.comp.templates.save = None;
        return;
    }
    if apply {
        let r = app.session.execute(
            "comp.template.save",
            super::comp::scoped(json!({"comp": d.comp, "nodes": d.nodes, "name": d.name.trim(), "replace": d.replace}), &d.group),
        );
        match r {
            Ok(v) => {
                app.ui.status = v["message"].as_str().unwrap_or_default().to_string();
                app.ui.panels.comp.templates.save = None;
                app.ui.panels.comp.templates.names = None;
                return;
            }
            Err(e) => d.error = Some(e.to_string()),
        }
    }
    app.ui.panels.comp.templates.save = Some(d);
}

fn delete_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(name) = app.ui.panels.comp.templates.delete.clone() else { return };
    let mut open = true;
    let (mut close, mut apply) = (false, false);
    egui::Window::new(tl!("Delete Template"))
        .id(egui::Id::new("comp-template-delete"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(tlf!("Delete the template “{name}”? Comps that used it keep their nodes.", name = name));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let c = ui.button(tl!("Cancel"));
                app.auto.add_response("comp.templateDialog.deleteCancel", &c, "Cancel");
                let o = ui.button(tl!("Delete"));
                app.auto.add_response("comp.templateDialog.deleteConfirm", &o, "Delete");
                apply = o.clicked();
                close = apply || c.clicked();
            });
        });
    if close || !open {
        app.ui.panels.comp.templates.delete = None;
    }
    if apply {
        match app.session.execute("comp.template.delete", json!({"name": name})) {
            Ok(_) => app.ui.status = tlf!("Deleted template “{name}”", name = name),
            Err(e) => app.session.toast(e.to_string()),
        }
        app.ui.panels.comp.templates.names = None;
    }
}
