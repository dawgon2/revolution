//! Resolve workflow header (fork RESOLVE-UI-PLAN RS3), in place of the Premiere header: the menu
//! bar on a row of its own (when the menus are in the window), then the page's toolbar: labelled
//! panel toggles on the left and right with the project name centred. Each page has its own
//! toggles ([`toggles`]); FilmCraft panels stand in for Resolve's (Media Pool is the Project
//! panel, Mixer is the Audio Track Mixer…; Inspector is its own panel, fork RS4.2), and Resolve
//! tools FilmCraft has no panel for (Index, Sound Library, Keyframes, ADR, LUTs, Lightbox) are
//! left out. A lit toggle's panel is on screen; clicking it closes the panel, clicking an unlit one
//! brings the panel forward or opens it as a column on its side of the page.
//!
//! Automation ids: `pageToolbar.<panel id>` (e.g. `pageToolbar.Project`,
//! `pageToolbar.Inspector`), `pageToolbar.quickExport`, `pageToolbar.title`.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Height of the menu row (only when the menus are drawn in the window).
pub const MENU_H: f32 = 26.0;
/// Height of the toolbar row.
pub const TOOLBAR_H: f32 = 36.0;

/// One toolbar button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// Show / hide a panel; a panel that isn't in the layout opens as a column on the toggle's side.
    Panel(PanelKind),
    /// The Quick Export popup (Resolve's Edit page has it top right).
    QuickExport,
}

impl Toggle {
    /// The automation id's last part.
    pub fn key(self) -> String {
        match self {
            Toggle::Panel(p) => p.id(),
            Toggle::QuickExport => "quickExport".into(),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Toggle::Panel(p) => p.title(),
            Toggle::QuickExport => "Quick Export",
        }
    }
    fn icon(self) -> Icon {
        match self {
            Toggle::QuickExport => Icon::Export,
            Toggle::Panel(p) => match p {
                PanelKind::Project => Icon::Folder,
                PanelKind::MediaBrowser => Icon::Drive,
                PanelKind::Effects => Icon::Fx,
                PanelKind::Markers => Icon::Marker,
                PanelKind::Metadata => Icon::Info,
                PanelKind::EffectControls | PanelKind::Inspector => Icon::Wrench,
                PanelKind::AudioTrackMixer => Icon::Speaker,
                PanelKind::AudioMeters => Icon::Mic,
                PanelKind::Gallery => Icon::Camera,
                PanelKind::ColorClips => Icon::Film,
                PanelKind::ColorNodes => Icon::Nest,
                PanelKind::LumetriScopes => Icon::Grid,
                PanelKind::RenderSettings => Icon::Gear,
                PanelKind::RenderQueue => Icon::ListView,
                PanelKind::EssentialSound => Icon::Audio,
                _ => Icon::Square,
            },
        }
    }
}

/// The toolbar of a page (`None`: a workspace that isn't a page): (left side, right side).
pub fn toggles(page: Option<&str>) -> (&'static [Toggle], &'static [Toggle]) {
    use PanelKind::*;
    use Toggle::{Panel as P, QuickExport};
    match page {
        Some("media") => (&[P(MediaBrowser), P(Project)], &[P(AudioMeters), P(Metadata), P(Inspector)]),
        Some("color") => (&[P(Gallery), P(Project), P(ColorClips)], &[P(ColorNodes), P(LumetriScopes), P(Effects)]),
        Some("fairlight") => (&[P(Project), P(Effects), P(Markers)], &[P(AudioTrackMixer), P(AudioMeters), P(Metadata), P(Inspector)]),
        Some("deliver") => (&[P(RenderSettings)], &[P(RenderQueue)]),
        // fork RS4: the Cut page (Resolve's: Media Pool, Sync Bin, Transitions, Titles, Effects /
        // Quick Export, Full Screen, Mixer, Inspector)
        Some("cut") => (&[P(Project), P(Effects)], &[QuickExport, P(AudioTrackMixer), P(Inspector)]),
        // fork C1: the Fusion page (Resolve's: Media Pool, Effects, Nodes / Metadata, Inspector)
        Some("fusion") => (&[P(Project), P(Effects), P(Compositor)], &[P(Metadata), P(Inspector)]),
        // the Edit page, and any other workspace
        // fork RS4: Resolve's Keyframes button is Effect Controls with its keyframe lane
        _ => (&[P(Project), P(Effects), P(Markers), P(EffectControls)], &[QuickExport, P(AudioTrackMixer), P(Metadata), P(Inspector)]),
    }
}

/// Every toolbar key any page uses (for the coach's id check).
pub fn all_keys() -> Vec<String> {
    let mut v = Vec::new();
    for page in [Some("media"), Some("cut"), Some("edit"), Some("color"), Some("fairlight"), Some("deliver"), None] {
        let (l, r) = toggles(page);
        for t in l.iter().chain(r) {
            let k = t.key();
            if !v.contains(&k) {
                v.push(k);
            }
        }
    }
    v
}

/// Height of the whole header in the Resolve workflow.
pub fn height(app: &FilmcraftApp) -> f32 {
    if app.in_window_menu() { MENU_H + TOOLBAR_H } else { TOOLBAR_H }
}

/// Whether a panel is on screen (the active tab of a docked or floating group).
fn shown(app: &FilmcraftApp, p: PanelKind) -> bool {
    app.ui.dock.is_visible(p) || app.ui.floating.iter().any(|g| g.panels.get(g.active) == Some(&p))
}

/// Click on a panel toggle: hide a shown panel, else bring it forward or open it at the side.
pub fn toggle(app: &mut FilmcraftApp, p: PanelKind, left: bool) {
    if shown(app, p) {
        if let Err(e) = app.apply_dock(craft_ui::docking::Action::Close { panel: p }) {
            app.ui.status = e;
        }
        return;
    }
    let present = app.ui.dock.contains(p) || app.ui.floating.iter().any(|g| g.panels.contains(&p));
    if present {
        app.show_panel(p);
    } else {
        app.ui.dock.add_side(p, left, if left { 380.0 } else { 360.0 });
        app.ui.focused = p;
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, t.header_bg);
    let drag = ui.interact(rect, egui::Id::new("resolve-header-drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }
    let inset = if app.integrated_titlebar { 104.0 } else { 6.0 };
    let mut bar = rect;
    // the menus on a row of their own, as in Resolve
    if app.in_window_menu() {
        let row = Rect::from_min_size(rect.min, vec2(rect.width(), MENU_H));
        let menu_rect = Rect::from_min_max(pos2(row.min.x + inset, row.min.y + 2.0), pos2(row.max.x - 8.0, row.max.y - 2.0));
        let mut mu = ui.new_child(egui::UiBuilder::new().max_rect(menu_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        mu.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        mu.style_mut().visuals.widgets.inactive.bg_stroke = Stroke::NONE;
        mu.style_mut().visuals.override_text_color = Some(t.text_dim);
        crate::menus::menu_bar(app, &mut mu);
        bar = Rect::from_min_max(pos2(rect.min.x, row.max.y), rect.max);
        p.line_segment([pos2(rect.min.x, row.max.y), pos2(rect.max.x, row.max.y)], Stroke::new(1.0, t.separator));
    }
    p.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, Color32::BLACK));
    let page = crate::panels::pages::current(app);
    let (left, right) = toggles(page);
    let bar_inset = if app.in_window_menu() { 6.0 } else { inset };
    // left toggles, then right toggles from the right edge
    let mut x = bar.min.x + bar_inset;
    let mut clicked: Option<(Toggle, bool)> = None;
    let mut quick_anchor = pos2(bar.max.x - 360.0, bar.max.y + 4.0);
    let mut left_end = x;
    for (i, tg) in left.iter().enumerate() {
        let r = button(app, ui, &p, *tg, x, bar);
        if r.1 {
            clicked = Some((*tg, true));
        }
        x = r.0.max.x;
        if i + 1 < left.len() {
            p.line_segment([pos2(x, bar.min.y + 9.0), pos2(x, bar.max.y - 9.0)], Stroke::new(1.0, t.separator));
        }
        left_end = x;
    }
    // the window buttons sit in the menu row when it shows, else at the end of this one
    let mut rx = bar.max.x - 8.0 - if app.in_window_menu() { 0.0 } else { crate::window_chrome::reserved_width(app, ui.ctx()) };
    let mut right_start = rx;
    for (i, tg) in right.iter().rev().enumerate() {
        let w = button_width(&p, *tg);
        let r = button(app, ui, &p, *tg, rx - w, bar);
        if r.1 {
            clicked = Some((*tg, false));
        }
        if *tg == Toggle::QuickExport {
            quick_anchor = pos2((r.0.max.x - 340.0).max(bar.min.x), bar.max.y + 4.0);
        }
        rx = r.0.min.x;
        if i + 1 < right.len() {
            p.line_segment([pos2(rx, bar.min.y + 9.0), pos2(rx, bar.max.y - 9.0)], Stroke::new(1.0, t.separator));
        }
        right_start = rx;
    }
    // the project name, centred in the room left between the two sides
    let title = if app.session.is_dirty() { tlf!("{name} - Edited", name = app.session.project.name) } else { app.session.project.name.clone() };
    let (l, r) = (left_end + 12.0, right_start - 12.0);
    if r > l + 40.0 {
        let mut job = egui::text::LayoutJob::simple_singleline(title.clone(), Tokens::ui(14.0), t.tab_text_active);
        job.wrap.max_width = r - l;
        job.wrap.max_rows = 1;
        let galley = p.layout_job(job);
        let half = galley.size().x / 2.0;
        let cx = if bar.center().x - half >= l && bar.center().x + half <= r { bar.center().x } else { (l + r) / 2.0 };
        let pos = pos2(cx - half, bar.center().y - galley.size().y / 2.0);
        app.auto.add("pageToolbar.title", Rect::from_min_size(pos, galley.size()), &title);
        p.with_clip_rect(Rect::from_min_max(pos2(l, bar.min.y), pos2(r, bar.max.y))).galley(pos, galley, t.tab_text_active);
    }
    match clicked {
        Some((Toggle::Panel(panel), left)) => toggle(app, panel, left),
        Some((Toggle::QuickExport, _)) => {
            app.ui.export.quick_open = !app.ui.export.quick_open;
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("quick-export-toggled"), true));
        }
        None => {}
    }
    crate::panels::export_mode::quick_export(app, ui.ctx(), quick_anchor);
}

fn button_width(p: &egui::Painter, tg: Toggle) -> f32 {
    let g = p.layout_no_wrap(crate::i18n::t(tg.label()).to_string(), Tokens::ui(12.5), Color32::WHITE);
    g.size().x + 16.0 + 6.0 + 24.0
}

/// Draw a toolbar button with its left edge at `x`; returns its rect and whether it was clicked.
fn button(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &egui::Painter, tg: Toggle, x: f32, bar: Rect) -> (Rect, bool) {
    let t = app.tokens;
    let label = crate::i18n::t(tg.label()).to_string();
    let w = button_width(p, tg);
    let r = Rect::from_min_size(pos2(x, bar.min.y + 4.0), vec2(w, bar.height() - 8.0));
    let resp = ui.interact(r, egui::Id::new(("page-toolbar", tg.key())), Sense::click());
    let lit = match tg {
        Toggle::Panel(panel) => shown(app, panel),
        Toggle::QuickExport => app.ui.export.quick_open,
    };
    if resp.hovered() {
        p.rect_filled(r, 4.0, t.hover);
    }
    let ink = if lit || resp.hovered() { t.tab_text_active } else { t.text_dim };
    let ir = Rect::from_center_size(pos2(r.min.x + 12.0 + 8.0, r.center().y), vec2(16.0, 16.0));
    icons::paint(p, ir, tg.icon(), ink);
    p.text(pos2(ir.max.x + 6.0, r.center().y), Align2::LEFT_CENTER, &label, Tokens::ui(12.5), ink);
    app.auto.add(&format!("pageToolbar.{}", tg.key()), r, tg.label());
    (r, resp.clicked())
}
