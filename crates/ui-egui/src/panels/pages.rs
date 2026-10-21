//! Resolve workflow page bar (fork GRADE-PLAN G1.8, restyled by RESOLVE-UI-PLAN RS3): the app
//! name on the left; Media, Cut, Edit, Fusion, Color, Fairlight and Deliver centred, each an icon over its
//! label, the current one lit and underlined in the accent; Lessons, Ask the coach and Assistant
//! (fork, [`crate::panels::coach::bar_buttons`]), Home (the Project Manager) and Settings on the
//! right. Each page is a built-in workspace ([`crate::dock::PAGE_WORKSPACES`],
//! Color is Color Grading), so its panels can be rearranged and saved like any workspace. Shown
//! only when Settings ▸ General ▸ Workflow is DaVinci Resolve.
//!
//! The Cut page (fork RS4) is the media pool and viewer over [`crate::panels::cut_page`]; the
//! Fusion page is the compositor (fork C1). Resolve's Photo page is not planned.
//!
//! **Save Layout** (fork LAY, owner 2026-10-21) left of the coach's buttons saves the page's
//! current arrangement (`window.workspace.saveChanges`); its right-click menu resets to the saved
//! layout or restores the page's original one.
//!
//! Automation ids: `page.<id>` (the pages), `pageBar.home`, `pageBar.settings`,
//! `pageBar.saveLayout` (+ `pageBar.saveLayout.reset` / `.original` / `.preset` / `.presets` in its
//! right-click menu).

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Mode;
use crate::theme::Tokens;

/// Height of the page bar.
pub const HEIGHT: f32 = 46.0;

/// (id, label, workspace, icon).
pub const PAGES: [(&str, &str, &str, Icon); 7] = [
    ("media", "Media", "Media Page", Icon::Image),
    // fork RS4: fast edits over a whole + zoomed-in timeline
    ("cut", "Cut", "Cut Page", Icon::Razor),
    ("edit", "Edit", "Edit Page", Icon::Sequence),
    // fork C1: node comps, between Edit and Color as in Resolve
    ("fusion", "Fusion", "Compositing", Icon::Fx),
    ("color", "Color", "Color Grading", Icon::Adjust),
    ("fairlight", "Fairlight", "Fairlight Page", Icon::Audio),
    ("deliver", "Deliver", "Deliver Page", Icon::Export),
];

/// The page the window shows now, if any.
pub fn current(app: &FilmcraftApp) -> Option<&'static str> {
    if app.ui.mode == Mode::Export {
        return Some("deliver");
    }
    if app.ui.mode != Mode::Edit {
        return None;
    }
    PAGES.iter().find(|p| p.2 == app.ui.workspace.as_str()).map(|p| p.0)
}

/// Switch to page `id`; `false` for an unknown id.
pub fn open(app: &mut FilmcraftApp, id: &str) -> bool {
    let Some(&(_, _, ws, _)) = PAGES.iter().find(|p| p.0 == id) else { return false };
    app.ui.mode = Mode::Edit;
    if app.ui.workspace != ws {
        app.set_workspace(ws);
    }
    true
}

/// Keep the window on the workflow's own layouts (fork RS3), once per frame before drawing: in
/// the Resolve workflow the Export mode is the Deliver page, the Import mode the Media page, and
/// the Premiere workspaces a page stands for open that page; in the Premiere workflow a page
/// workspace opens the Premiere workspace it stands for. A workflow switch keeps each
/// workflow's layout as it was left (unsaved changes too) and puts it back on return.
pub fn follow_workflow(app: &mut FilmcraftApp) {
    let resolve = app.resolve_workflow();
    if let Some(was) = app.layout_workflow.replace(resolve)
        && was != resolve
    {
        let outgoing = (app.ui.workspace.clone(), app.ui.dock.clone(), app.ui.floating.clone());
        let back = if resolve { app.layouts_by_workflow.1.take() } else { app.layouts_by_workflow.0.take() };
        if was {
            app.layouts_by_workflow.1 = Some(outgoing);
        } else {
            app.layouts_by_workflow.0 = Some(outgoing);
        }
        if let Some((ws, dock, floating)) = back
            && crate::dock::for_workflow(&ws, resolve) == ws
            && (crate::dock::Layout { root: dock.clone(), floating: floating.clone() }).validate().is_ok()
        {
            app.ui.workspace = ws;
            app.ui.dock = dock;
            app.ui.floating = floating;
            app.ui.keys.maximized = None;
        }
    }
    if resolve && matches!(app.ui.mode, Mode::Export | Mode::Import) {
        let ws = if app.ui.mode == Mode::Export { "Deliver Page" } else { "Media Page" };
        app.ui.mode = Mode::Edit;
        if app.ui.workspace != ws {
            app.set_workspace(ws);
        }
        return;
    }
    let want = crate::dock::for_workflow(&app.ui.workspace, resolve).to_string();
    if want != app.ui.workspace {
        app.set_workspace(&want);
    }
}

/// A page button's width with room to spare, the narrowest one, and the narrowest that still
/// shows its label under the icon (narrower: the icon alone; the name stays in the tooltip-less
/// automation label).
const PAGE_W: f32 = 102.0;
const MIN_PAGE_W: f32 = 36.0;
const LABEL_PAGE_W: f32 = 64.0;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, t.header_bg);
    p.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, t.separator));
    // the app's name: Revolution's own logo where Resolve shows its mark (owner, §1a.108)
    let mark = Rect::from_center_size(pos2(rect.min.x + 26.0, rect.center().y), vec2(18.0, 18.0));
    crate::app_logo::paint(ui.ctx(), &p, mark, Color32::WHITE);
    let name = p.text(pos2(mark.max.x + 8.0, rect.center().y), Align2::LEFT_CENTER, "Revolution", Tokens::semibold(13.0), t.text);
    app.auto.add("pageBar.app", mark.union(name), "Revolution");
    // the coach's buttons left of Home (FORK-PLAN §1a.78)
    let pills = crate::panels::coach::bar_buttons(app, ui, rect.max.x - 92.0, rect.center().y);
    // Save Layout left of them while the pages still fit at their narrowest (else Window ▸ Workspaces)
    let pills = save_layout_button(app, ui, pills - 6.0, rect.center().y, name.max.x + 24.0 + MIN_PAGE_W * PAGES.len() as f32).unwrap_or(pills);
    // the pages, centred: icon over label; the current one lit with an accent underline; moved
    // left on a narrow window so they stay clear of the coach's buttons (never over the name)
    let cur = current(app);
    let n = PAGES.len() as f32;
    // narrower when the room between the name and the coach's buttons is short (a small window,
    // or the 16" laptop panel at 2×: XPS item 15), down to icons alone
    let room = (pills - 12.0) - (name.max.x + 12.0);
    let w = (room / n).clamp(MIN_PAGE_W, PAGE_W);
    let labels = w >= LABEL_PAGE_W;
    let total = w * n;
    let mut x = (rect.center().x - total / 2.0).min(pills - 12.0 - total).max(name.max.x + 12.0);
    let mut clicked = None;
    for (id, label, _, icon) in PAGES {
        let r = Rect::from_min_size(pos2(x, rect.min.y + 1.0), vec2(w, rect.height() - 1.0));
        let resp = ui.interact(r, egui::Id::new(("page", id)), Sense::click());
        let sel = cur == Some(id);
        if sel {
            p.rect_filled(r, 0.0, t.hover);
            p.line_segment([pos2(r.min.x, r.max.y - 1.5), pos2(r.max.x, r.max.y - 1.5)], Stroke::new(3.0, t.accent));
        } else if resp.hovered() {
            p.rect_filled(r, 0.0, t.hover.gamma_multiply(0.6));
        }
        let ink = if sel || resp.hovered() { t.text } else { t.text_dim };
        let ir = Rect::from_center_size(pos2(r.center().x, if labels { r.min.y + 15.0 } else { r.center().y }), vec2(16.0, 16.0));
        icons::paint(&p, ir, icon, if sel { t.accent } else { ink });
        if labels {
            p.text(pos2(r.center().x, r.min.y + 33.0), Align2::CENTER_CENTER, crate::i18n::t(label), Tokens::ui(11.5), ink);
        }
        app.auto.add(&format!("page.{id}"), r, label);
        if resp.clicked() {
            clicked = Some(id);
        }
        x += w;
    }
    // Home (the Project Manager, RS2) and Settings, bottom right
    let mut button = |id: &str, icon: Icon, tip: &str, right: f32| -> bool {
        let r = Rect::from_center_size(pos2(rect.max.x - right, rect.center().y), vec2(32.0, 32.0));
        let resp = ui.interact(r, egui::Id::new(("page-bar", id)), Sense::click()).on_hover_text(crate::i18n::t(tip));
        if resp.hovered() {
            p.rect_filled(r, 4.0, t.hover);
        }
        icons::paint(&p, r.shrink(8.0), icon, if resp.hovered() { t.text } else { t.text_dim });
        app.auto.add(&format!("pageBar.{id}"), r, tip);
        resp.clicked()
    };
    let settings = button("settings", Icon::Gear, "Settings", 28.0);
    let home = button("home", Icon::Home, "Project Manager", 68.0);
    if let Some(id) = clicked {
        open(app, id);
    }
    if home {
        crate::panels::home::show_home(app);
    }
    if settings && let Err(e) = crate::menus::invoke(app, ui.ctx(), "app.settings.general", serde_json::json!({})) {
        app.ui.status = e;
    }
}

/// Save the window's arrangement as the current page's layout (fork LAY.1); the status line says
/// which page and how to undo it.
pub fn save_layout(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let page = current(app).and_then(|id| PAGES.iter().find(|p| p.0 == id)).map(|p| crate::i18n::t(p.1));
    let name = app.ui.language.tr(&app.ui.workspace).to_string();
    app.ui.status = match crate::menus::invoke(app, ctx, "window.workspace.saveChanges", json!({})) {
        Ok(_) => match page {
            Some(page) => tlf!("Saved the {page} page layout. Right-click Save Layout to reset it.", page),
            None => tlf!("Saved the {name} layout. Right-click Save Layout to reset it.", name),
        },
        Err(e) => e,
    };
}

/// The Save Layout pill ending at `right`; returns its left edge, or `None` (not drawn) when it
/// would start left of `min_left`. Right-click: Reset to Saved Layout, and Restore Original
/// Layout when the page has saved changes.
fn save_layout_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, right: f32, cy: f32, min_left: f32) -> Option<f32> {
    let t = app.tokens;
    let label = tl!("Save Layout");
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(11.5), t.text);
    let w = galley.size().x + 22.0;
    let r = Rect::from_min_size(pos2(right - w, cy - 11.0), vec2(w, 22.0));
    if r.min.x < min_left {
        return None;
    }
    let resp = ui
        .interact(r, egui::Id::new(("page-bar", "saveLayout")), Sense::click())
        .on_hover_text(tl!("Keep this page arranged the way it is now: it opens like this every time. Right-click to reset."));
    let fill = if resp.hovered() { t.hover.gamma_multiply(0.6) } else { t.panel_bg };
    ui.painter().rect(r, CornerRadius::same(11), fill, Stroke::new(1.0, t.control_border), StrokeKind::Inside);
    let ink = if resp.hovered() { t.text } else { t.text_dim };
    ui.painter().galley(pos2(r.center().x - galley.size().x / 2.0, r.center().y - galley.size().y / 2.0), galley, ink);
    app.auto.add_response("pageBar.saveLayout", &resp, label);
    if resp.clicked() {
        save_layout(app, ui.ctx());
    }
    let has_saved = app.workspaces.saved.iter().any(|s| s.name == app.ui.workspace);
    let mut chosen: Option<(&str, serde_json::Value)> = None;
    resp.context_menu(|ui| {
        let b = ui.button(tl!("Reset to Saved Layout"));
        app.auto.add("pageBar.saveLayout.reset", b.rect, "Reset to Saved Layout");
        if b.clicked() {
            chosen = Some(("window.workspace.reset", json!({})));
            ui.close();
        }
        let b = ui.add_enabled(has_saved, egui::Button::new(tl!("Restore Original Layout")));
        app.auto.add("pageBar.saveLayout.original", b.rect, "Restore Original Layout");
        if b.clicked() {
            chosen = Some(("window.workspace.delete", json!({"name": app.ui.workspace.clone()})));
            ui.close();
        }
        // fork LAY.3: every page at once
        ui.separator();
        let b = ui.button(tl!("Save Layout as Preset…"));
        app.auto.add("pageBar.saveLayout.preset", b.rect, "Save Layout as Preset…");
        if b.clicked() {
            chosen = Some(("window.layoutPreset.save", json!({})));
            ui.close();
        }
        let b = ui.button(tl!("Manage Layout Presets…"));
        app.auto.add("pageBar.saveLayout.presets", b.rect, "Manage Layout Presets…");
        if b.clicked() {
            chosen = Some(("window.layoutPreset.manage", json!({})));
            ui.close();
        }
    });
    if let Some((id, params)) = chosen
        && let Err(e) = crate::menus::invoke(app, ui.ctx(), id, params)
    {
        app.ui.status = e;
    }
    Some(r.min.x - 6.0)
}
