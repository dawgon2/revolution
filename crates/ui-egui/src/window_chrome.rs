//! FilmCraft's own window title bar on Linux and Windows (fork; Settings ▸ General ▸ Window title
//! bar), as DaVinci Resolve does it: no desktop title bar, the minimise / maximise / close buttons
//! at the right end of the top row, and resize edges along the window border. The top rows already
//! move the window (`ViewportCommand::StartDrag`) and maximise it on a double click; every top row
//! leaves [`reserved_width`] free at its right end for the buttons drawn here.
//!
//! Full screen (Settings ▸ General ▸ Full screen, on by default) has its own button left of the
//! three, so it is always one click away; F11 and View ▸ Full Screen switch it too. In full screen
//! and on the Project Manager only the full-screen and minimise buttons show.
//!
//! Automation ids: `window.fullScreen`, `window.minimize`, `window.maximize`, `window.close`.

use egui::{Color32, CursorIcon, Rect, ResizeDirection, Sense, Stroke, ViewportCommand, pos2, vec2};

use crate::FilmcraftApp;

/// Width of one window button.
const BUTTON_W: f32 = 40.0;
/// Width of the resize band along each edge, and of the corner squares.
const EDGE: f32 = 4.0;
const CORNER: f32 = 12.0;
/// Hover colour of the close button (the usual "danger" red, not tied to the theme).
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xc4, 0x2b, 0x1c);

/// Whether this platform can draw its own title bar (macOS keeps the system's traffic lights).
pub const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "windows"));

/// What Settings ▸ General ▸ Window title bar asks for on this platform: `true` = FilmCraft draws
/// the title bar (the default), `false` = the desktop's. Unknown values count as the default.
pub fn wants_own(title_bar: &str) -> bool {
    SUPPORTED && title_bar != "system"
}

/// Width the top rows leave free at their right end for the window buttons: as many as [`show`]
/// draws, so both read full screen from the window itself (the preference can say full screen
/// while the window isn't yet, or the window manager refused it; header icons then slid under
/// the window buttons).
pub fn reserved_width(app: &FilmcraftApp, ctx: &egui::Context) -> f32 {
    if app.window_controls { shown_buttons(app, window_full_screen(ctx)) as f32 * BUTTON_W } else { 0.0 }
}

/// Whether the window is full screen now, as the window reports it.
fn window_full_screen(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
}

/// How many of the buttons show (owner, 2026-10-13): all four in a normal window; only full
/// screen and minimise in full screen and on the Project Manager screen (no maximise, no close:
/// quit through FilmCraft ▸ Quit FilmCraft or Cmd+Q).
fn shown_buttons(app: &FilmcraftApp, full_screen: bool) -> usize {
    if full_screen || app.ui.home.open { 2 } else { BUTTONS }
}

/// Number of window buttons: full screen, minimise, maximise, close.
const BUTTONS: usize = 4;

/// Whether the window should be full screen now: the Project Manager and the editor remember their
/// own state (owner, 2026-10-13, as in Resolve): Settings ▸ General ▸ Project Manager full screen
/// while it is open, Settings ▸ General ▸ Full screen otherwise.
pub fn wanted_full_screen(app: &FilmcraftApp) -> bool {
    let g = &app.session.prefs.general;
    if app.ui.home.open { g.project_manager_full_screen } else { g.full_screen }
}

/// Switch full screen for the current screen (the Project Manager or the editor), saved with the
/// preferences; [`sync_full_screen`] sends it to the window. Returns the new state.
pub fn toggle_full_screen(app: &mut FilmcraftApp) -> bool {
    // from what the window shows when known (an agent's window opens windowed although the
    // setting is on; a compositor may refuse full screen), else from the setting
    let now = app.shown_full_screen.unwrap_or_else(|| wanted_full_screen(app));
    set_full_screen(app, !now)
}

/// Set full screen for the current screen; the status line says how to switch back.
pub fn set_full_screen(app: &mut FilmcraftApp, on: bool) -> bool {
    store_full_screen(app, on);
    app.ui.status =
        if on { tl!("Full screen: F11 or the full-screen button at the top right leaves it") } else { tl!("Full screen off: F11 switches it back on") }.into();
    on
}

/// Save `on` as the current screen's full-screen setting.
fn store_full_screen(app: &mut FilmcraftApp, on: bool) {
    if wanted_full_screen(app) == on {
        return;
    }
    let mut p = app.session.prefs.clone();
    if app.ui.home.open {
        p.general.project_manager_full_screen = on;
    } else {
        p.general.full_screen = on;
    }
    if let Err(e) = app.session.set_prefs(p) {
        log::warn!("saving preferences: {e}");
    }
}

/// Keep the window's full-screen state and the settings in step, once per frame:
/// - the current screen's setting changed (a button, F11, Settings, the Project Manager opening or
///   closing): the window follows;
/// - the desktop changed it (a KWin shortcut, the window menu): the current screen's setting
///   takes the window's new state.
///
/// On the first frame the window is only told when the host asks for it
/// ([`FilmcraftApp::full_screen_at_start`]; an agent's window stays windowed).
pub fn sync_full_screen(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let (now, time) = ctx.input(|i| (i.viewport().fullscreen, i.time));
    // the window answers a request a few frames later: changes seen right after one are its answer
    // (switching screens quickly can cross two answers), not the desktop's doing
    let settling = app.full_screen_sent_at.is_some_and(|t| time - t < FULL_SCREEN_SETTLE_S);
    if let Some(now) = now {
        if settling {
            app.shown_full_screen = Some(now);
        } else {
            window_full_screen_is(app, now);
        }
    }
    let wanted = wanted_full_screen(app);
    let send = match app.sent_full_screen {
        Some(sent) => sent != wanted,
        None => app.full_screen_at_start,
    };
    if send {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(wanted));
        app.full_screen_sent_at = Some(time);
    }
    app.sent_full_screen = Some(wanted);
}

/// Once the Project Manager the app opened into has closed, give the window the editor's size and
/// maximised state back (fork, owner 2026-10-20, FORK-PLAN §1a.108; full screen follows through
/// [`sync_full_screen`]). Once only.
pub fn restore_editor_window(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if app.ui.home.open {
        return;
    }
    let Some(w) = app.editor_window.take() else { return };
    app.editor_window_restored = Some(web_time::Instant::now());
    let [width, height] = w.inner_size;
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(width, height)));
    if w.maximized {
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
    }
}

/// How long after a full-screen request the window's changes count as its answer (seconds).
const FULL_SCREEN_SETTLE_S: f64 = 1.5;

/// [`sync_full_screen`]'s desktop half, with the window's state already read.
fn window_full_screen_is(app: &mut FilmcraftApp, now: bool) {
    let before = app.shown_full_screen.replace(now);
    if before.is_some_and(|b| b != now) && now != wanted_full_screen(app) && app.sent_full_screen.is_some() {
        store_full_screen(app, now);
        // already applied by the desktop: don't send it back
        app.sent_full_screen = Some(now);
    }
}

/// Height of the topmost row of the current screen (where the buttons sit).
fn top_row_height(app: &FilmcraftApp) -> f32 {
    if app.ui.home.open {
        crate::panels::home::TITLE_H
    } else if !app.ui.show_header {
        30.0
    } else if app.resolve_workflow() {
        if app.in_window_menu() { crate::panels::page_toolbar::MENU_H } else { crate::panels::page_toolbar::height(app) }
    } else {
        38.0
    }
}

/// Draw the window buttons and handle the resize edges; called last in the UI pass so they sit on
/// top of the panels. Does nothing while the desktop draws the title bar.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    if !app.window_controls {
        return;
    }
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let fullscreen = window_full_screen(&ctx);
    let full = ui.max_rect();
    let t = app.tokens;
    let h = top_row_height(app);
    let p = ui.painter().clone();
    let shown = shown_buttons(app, fullscreen);
    let mut x = full.max.x - shown as f32 * BUTTON_W;
    let buttons: [(&str, &str); BUTTONS] = [
        ("window.fullScreen", if fullscreen { tl!("Exit Full Screen (F11)") } else { tl!("Full Screen (F11)") }),
        ("window.minimize", tl!("Minimize")),
        ("window.maximize", if maximized { tl!("Restore") } else { tl!("Maximize") }),
        ("window.close", tl!("Close")),
    ];
    for (n, (id, label)) in buttons.into_iter().enumerate().take(shown) {
        // 0 = full screen, then the usual three
        let i = n.wrapping_sub(1);
        let r = Rect::from_min_size(pos2(x, full.min.y), vec2(BUTTON_W, h));
        x += BUTTON_W;
        let resp = ui.interact(r, egui::Id::new(id), Sense::click()).on_hover_text(label);
        app.auto.add(id, r, label);
        let close = n == 3;
        if resp.hovered() {
            p.rect_filled(r, 0.0, if close { CLOSE_HOVER } else { t.hover });
        }
        let color = if resp.hovered() { if close { Color32::WHITE } else { t.text } } else { t.text_dim };
        let stroke = Stroke::new(1.0, color);
        let c = r.center();
        if n == 0 {
            // four corner brackets: pointing out (enter) or in (leave)
            let (a, b) = if fullscreen { (2.0, 6.0) } else { (6.0, 2.0) };
            for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let corner = pos2(c.x + sx * a, c.y + sy * a);
                p.line_segment([corner, pos2(c.x + sx * a, c.y + sy * b)], stroke);
                p.line_segment([corner, pos2(c.x + sx * b, c.y + sy * a)], stroke);
            }
            if resp.clicked() {
                toggle_full_screen(app);
            }
            continue;
        }
        match i {
            0 => {
                p.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], stroke);
            }
            1 if maximized || fullscreen => {
                // restore: two overlapping squares
                p.rect_stroke(Rect::from_center_size(pos2(c.x - 1.5, c.y + 1.5), vec2(8.0, 8.0)), 0.0, stroke, egui::StrokeKind::Middle);
                p.line_segment([pos2(c.x - 1.5, c.y - 4.5), pos2(c.x + 4.5, c.y - 4.5)], stroke);
                p.line_segment([pos2(c.x + 4.5, c.y - 4.5), pos2(c.x + 4.5, c.y + 1.5)], stroke);
            }
            1 => {
                p.rect_stroke(Rect::from_center_size(c, vec2(10.0, 10.0)), 0.0, stroke, egui::StrokeKind::Middle);
            }
            _ => {
                p.line_segment([pos2(c.x - 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 5.0)], stroke);
                p.line_segment([pos2(c.x + 5.0, c.y - 5.0), pos2(c.x - 5.0, c.y + 5.0)], stroke);
            }
        }
        if resp.clicked() {
            match i {
                0 => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                1 if fullscreen => {
                    set_full_screen(app, false);
                }
                1 => ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized)),
                // a close request: unsaved changes still ask first (`intercept_quit`)
                _ => ctx.send_viewport_cmd(ViewportCommand::Close),
            }
        }
    }
    // after the buttons, so the outer few points of the top-right corner still resize
    if !maximized && !fullscreen {
        resize_edges(ui, full);
    }
}

/// The resize bands along the edges and the corner squares: hover shows the resize cursor, a drag
/// hands the resize to the window manager.
fn resize_edges(ui: &mut egui::Ui, full: Rect) {
    use ResizeDirection as D;
    let (l, r, top, b) = (full.min.x, full.max.x, full.min.y, full.max.y);
    let zones = [
        (Rect::from_min_max(pos2(l, top), pos2(l + CORNER, top + CORNER)), D::NorthWest, CursorIcon::ResizeNorthWest),
        (Rect::from_min_max(pos2(r - CORNER, top), pos2(r, top + CORNER)), D::NorthEast, CursorIcon::ResizeNorthEast),
        (Rect::from_min_max(pos2(l, b - CORNER), pos2(l + CORNER, b)), D::SouthWest, CursorIcon::ResizeSouthWest),
        (Rect::from_min_max(pos2(r - CORNER, b - CORNER), pos2(r, b)), D::SouthEast, CursorIcon::ResizeSouthEast),
        (Rect::from_min_max(pos2(l + CORNER, top), pos2(r - CORNER, top + EDGE)), D::North, CursorIcon::ResizeNorth),
        (Rect::from_min_max(pos2(l + CORNER, b - EDGE), pos2(r - CORNER, b)), D::South, CursorIcon::ResizeSouth),
        (Rect::from_min_max(pos2(l, top + CORNER), pos2(l + EDGE, b - CORNER)), D::West, CursorIcon::ResizeWest),
        (Rect::from_min_max(pos2(r - EDGE, top + CORNER), pos2(r, b - CORNER)), D::East, CursorIcon::ResizeEast),
    ];
    for (i, (rect, dir, cursor)) in zones.into_iter().enumerate() {
        // corners are squares at the very corner only (the band, not the whole square), so the
        // window buttons and the panels' edges stay clickable
        let bands = if i < 4 { corner_band(rect, full) } else { vec![rect] };
        for (j, band) in bands.into_iter().enumerate() {
            let resp = ui.interact(band, egui::Id::new(("window-resize", i, j)), Sense::drag());
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(cursor);
            }
            if resp.drag_started() {
                ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(dir));
            }
        }
    }
}

/// The L-shaped band of a corner square that touches the window border (two rects).
fn corner_band(square: Rect, full: Rect) -> Vec<Rect> {
    let horizontal = if square.min.y <= full.min.y {
        Rect::from_min_max(square.min, pos2(square.max.x, square.min.y + EDGE))
    } else {
        Rect::from_min_max(pos2(square.min.x, square.max.y - EDGE), square.max)
    };
    let vertical = if square.min.x <= full.min.x {
        Rect::from_min_max(square.min, pos2(square.min.x + EDGE, square.max.y))
    } else {
        Rect::from_min_max(pos2(square.max.x - EDGE, square.min.y), square.max)
    };
    vec![horizontal, vertical]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_title_bar_follows_the_setting() {
        assert_eq!(wants_own("filmcraft"), SUPPORTED);
        assert_eq!(wants_own("garbage"), SUPPORTED);
        assert!(!wants_own("system"));
    }

    #[test]
    fn desktop_full_screen_changes_reach_the_setting() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.sent_full_screen = Some(true);
        assert!(app.session.prefs.general.full_screen);
        // an agent's window opens windowed: the first state seen is only recorded
        window_full_screen_is(&mut app, false);
        assert!(app.session.prefs.general.full_screen);
        window_full_screen_is(&mut app, true);
        assert!(app.session.prefs.general.full_screen);
        // the desktop takes the window out of full screen (KWin shortcut, window menu)
        window_full_screen_is(&mut app, false);
        assert!(!app.session.prefs.general.full_screen, "the setting follows the window");
        window_full_screen_is(&mut app, true);
        assert!(app.session.prefs.general.full_screen);
    }

    #[test]
    fn corner_bands_touch_the_border() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 80.0));
        let se = Rect::from_min_max(pos2(88.0, 68.0), pos2(100.0, 80.0));
        let b = corner_band(se, full);
        assert_eq!(b[0], Rect::from_min_max(pos2(88.0, 76.0), pos2(100.0, 80.0)));
        assert_eq!(b[1], Rect::from_min_max(pos2(96.0, 68.0), pos2(100.0, 80.0)));
    }
}
