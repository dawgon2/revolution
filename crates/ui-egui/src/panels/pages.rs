//! Resolve workflow page bar (fork GRADE-PLAN G1.8, restyled by RESOLVE-UI-PLAN RS3): the app
//! name on the left; Media, Edit, Color, Fairlight and Deliver centred, each an icon over its
//! label, the current one lit and underlined in the accent; Lessons, Ask the coach and Assistant
//! (fork, [`crate::panels::coach::bar_buttons`]), Home (the Project Manager) and Settings on the
//! right. Each page is a built-in workspace ([`crate::dock::PAGE_WORKSPACES`],
//! Color is Color Grading), so its panels can be rearranged and saved like any workspace. Shown
//! only when Settings ▸ General ▸ Workflow is DaVinci Resolve.
//!
//! Resolve's Cut and Fusion pages are left out until FilmCraft has a Cut layout and a compositor
//! to show (RESOLVE-UI-PLAN RS3 item 2); its Photo page is not planned.
//!
//! Automation ids: `page.<id>` (the pages), `pageBar.home`, `pageBar.settings`.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Mode;
use crate::theme::Tokens;

/// Height of the page bar.
pub const HEIGHT: f32 = 46.0;

/// (id, label, workspace, icon).
pub const PAGES: [(&str, &str, &str, Icon); 6] = [
    ("media", "Media", "Media Page", Icon::Image),
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
