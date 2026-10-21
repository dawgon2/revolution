//! The Resolve workflow's Cut page timeline (fork RESOLVE-UI-PLAN RS4, laid out from observation
//! of Resolve 21's Cut page; our own drawings, no Blackmagic artwork): a toolbar of fast edits
//! that take the media pool's selected clip, then two views of the active timeline drawn by the
//! Timeline panel's own code ([`timeline::show_instance`]), so every tool, trim and drag works in
//! both: the upper one always shows the whole timeline, the lower one is zoomed in around a
//! playhead that stays in the middle while the timeline moves under it.
//!
//! Automation ids: `cut.<edit>` (the toolbar's edits: `smartInsert`, `append`, `rippleOverwrite`,
//! `closeUp`, `placeOnTop`, `sourceOverwrite`), `cut.tool.<tool>`, `cut.blade`, `cut.snap`,
//! `cut.linked`, `cut.zoomIn` / `cut.zoomOut`; the timelines' own ids as `cutUpper.…` and
//! `cutLower.…` in place of `timeline.…`.

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use serde_json::json;

use super::timeline::{self, Chrome, Instance};
use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::{TimelineView, Tool};
use crate::theme::Tokens;

/// The toolbar's height.
pub const TOOLBAR_H: f32 = 38.0;
/// The left column: the upper timeline's tools, the lower timeline's track headers.
pub const LEFT_W: f32 = 172.0;
/// The lower timeline's zoom (points a second) at first, and its limits.
const LOWER_PPS: f64 = 160.0;
const MIN_LOWER_PPS: f64 = 4.0;
const MAX_LOWER_PPS: f64 = 4_000.0;

/// The upper timeline: the whole timeline, a thin ruler, thin tracks, no headers.
const UPPER: Chrome = Chrome {
    top_h: 22.0,
    ruler_h: 22.0,
    scrollbar_h: 0.0,
    master_h: 0.0,
    top_block: false,
    headers: false,
    extras: false,
    fit_all: true,
    centre_playhead: false,
    salt: 0x5c07_0001,
    resolve_toolbar: false,
    resolve_headers: false,
};

/// The lower timeline: track headers, the playhead held in the middle.
const LOWER: Chrome = Chrome {
    top_h: 28.0,
    ruler_h: 28.0,
    scrollbar_h: 0.0,
    master_h: 0.0,
    top_block: false,
    headers: true,
    extras: true,
    fit_all: false,
    centre_playhead: true,
    salt: 0x5c07_0002,
    resolve_toolbar: false,
    resolve_headers: true,
};

/// The two views (zoom, scroll, track heights and the drag in progress of each).
pub struct CutState {
    pub upper: Instance,
    pub lower: Instance,
}

impl Default for CutState {
    fn default() -> Self {
        let upper = TimelineView { video_track_h: 22.0, audio_track_h: 16.0, header_w: 0.0, show_thumbnails: false, follow: false, ..Default::default() };
        let lower = TimelineView {
            pps: LOWER_PPS,
            target_pps: LOWER_PPS,
            video_track_h: 72.0,
            audio_track_h: 40.0,
            header_w: LEFT_W,
            follow: false,
            fit_pending: false,
            ..Default::default()
        };
        Self { upper: Instance::new(upper), lower: Instance::new(lower) }
    }
}

/// The toolbar's fast edits: (automation id, command, label). Each takes the media pool's selected clip.
pub const EDITS: [(&str, &str, &str); 6] = [
    ("cut.smartInsert", "edit.smartInsert", "Smart Insert: insert at the edit point nearest the playhead"),
    ("cut.append", "edit.append", "Append: add to the end of the timeline"),
    ("cut.rippleOverwrite", "edit.rippleOverwrite", "Ripple Overwrite: replace the clip under the playhead and close up the difference"),
    ("cut.closeUp", "edit.closeUp", "Close Up: a closer framing on the track above"),
    ("cut.placeOnTop", "edit.placeOnTop", "Place on Top: on the track above, at the playhead"),
    ("cut.sourceOverwrite", "edit.sourceOverwrite", "Source Overwrite: on the track above, lined up by source timecode"),
];

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), TOOLBAR_H.min(rect.height())));
    toolbar(app, ui, bar, &t);
    let body = Rect::from_min_max(pos2(rect.min.x, bar.max.y), rect.max);
    if body.height() < 40.0 || body.width() < LEFT_W + 40.0 {
        return;
    }
    if app.session.active_sequence().is_none() {
        timeline::show(app, ui, body);
        return;
    }
    let upper_h = (body.height() * 0.28).clamp(60.0, 170.0);
    let upper = Rect::from_min_max(pos2(body.min.x + LEFT_W, body.min.y), pos2(body.max.x, body.min.y + upper_h));
    let tools = Rect::from_min_max(body.min, pos2(body.min.x + LEFT_W, upper.max.y));
    let lower = Rect::from_min_max(pos2(body.min.x, upper.max.y + 3.0), body.max);
    tool_column(app, ui, tools, &t);
    // the two views; the one being dragged in draws last so it has the pointer to itself
    let mut cut = std::mem::take(&mut app.cut);
    timeline::show_instance(app, ui, upper, UPPER, &mut cut.upper, "cutUpper");
    ui.painter().rect_filled(Rect::from_min_max(pos2(body.min.x, upper.max.y), pos2(body.max.x, lower.min.y)), 0.0, t.app_bg);
    timeline::show_instance(app, ui, lower, LOWER, &mut cut.lower, "cutLower");
    app.cut = cut;
}

fn toolbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, t: &Tokens) {
    let p = ui.painter_at(bar);
    p.rect_filled(bar, 0.0, t.header_bg);
    p.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.separator));
    let source = app.session.state.project_selection.first().copied();
    let mut x = bar.min.x + 12.0;
    let cy = bar.center().y;
    for (i, (key, cmd, label)) in EDITS.iter().enumerate() {
        let r = Rect::from_center_size(pos2(x + 17.0, cy), vec2(34.0, 28.0));
        let enabled = source.is_some() && filmcraft_engine::commands::find(cmd).is_some();
        let resp = ui.interact(r, egui::Id::new(("cut-edit", *key)), Sense::click()).on_hover_text(*label);
        app.auto.add(key, r, label);
        if resp.hovered() && enabled {
            p.rect_filled(r, 3.0, t.hover);
        }
        let col = if enabled { t.icon } else { t.text_faint };
        edit_glyph(&p, r.shrink2(vec2(7.0, 7.0)), i, col, t.accent);
        if resp.clicked() && enabled {
            let params = source.map(|id| json!({"clip": id.0})).unwrap_or_else(|| json!({}));
            if let Err(e) = app.session.execute(cmd, params) {
                app.ui.status = e.to_string();
            }
        }
        x += 40.0;
    }
    // the lower timeline's zoom, right
    let mut rx = bar.max.x - 12.0;
    for (key, icon, tip, factor) in
        [("cut.zoomIn", Icon::Plus, "Zoom in the lower timeline", 1.5), ("cut.zoomOut", Icon::Zoom, "Zoom out the lower timeline", 1.0 / 1.5)]
    {
        let r = Rect::from_center_size(pos2(rx - 14.0, cy), vec2(28.0, 28.0));
        let resp = ui.interact(r, egui::Id::new(("cut-zoom", key)), Sense::click()).on_hover_text(tip);
        app.auto.add(key, r, tip);
        if resp.hovered() {
            p.rect_filled(r, 3.0, t.hover);
        }
        icons::paint(&p, r.shrink(7.0), icon, t.icon);
        if resp.clicked() {
            let v = &mut app.cut.lower.view;
            v.target_pps = (v.target_pps * factor).clamp(MIN_LOWER_PPS, MAX_LOWER_PPS);
        }
        rx -= 32.0;
    }
    if source.is_none() {
        p.text(pos2(x + 8.0, cy), Align2::LEFT_CENTER, tl!("Select a clip in the media pool to edit it in"), Tokens::ui(11.0), t.text_faint);
    }
}

/// Selection, Trim and Blade, then Snapping and Linked Selection, over the upper timeline.
fn tool_column(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, t: &Tokens) {
    let p = ui.painter_at(r);
    p.rect_filled(r, 0.0, t.tl_header_bg);
    let mut x = r.min.x + 10.0;
    let y = r.min.y + 8.0;
    let tool = app.ui.tool;
    let buttons: [(&str, Icon, bool, &str); 5] = [
        ("cut.tool.selection", Icon::Selection, tool == Tool::Selection, "Selection"),
        ("cut.tool.trim", Icon::Ripple, matches!(tool, Tool::Ripple | Tool::Rolling | Tool::Slip | Tool::Slide), "Trim"),
        ("cut.blade", Icon::Razor, false, "Cut at the playhead"),
        ("cut.snap", Icon::Magnet, app.session.state.snapping, "Snapping"),
        ("cut.linked", Icon::Link, app.session.state.linked_selection, "Linked Selection"),
    ];
    for (key, icon, on, tip) in buttons {
        let b = Rect::from_min_size(pos2(x, y), vec2(28.0, 28.0));
        let resp = ui.interact(b, egui::Id::new(("cut-tool", key)), Sense::click()).on_hover_text(tip);
        app.auto.add(key, b, tip);
        if on {
            p.rect_filled(b, 3.0, t.pressed);
        } else if resp.hovered() {
            p.rect_filled(b, 3.0, t.hover);
        }
        icons::paint(&p, b.shrink(7.0), icon, if on { t.accent } else { t.icon });
        if resp.clicked() {
            let result = match key {
                "cut.tool.selection" => {
                    app.ui.tool = Tool::Selection;
                    Ok(serde_json::Value::Null)
                }
                "cut.tool.trim" => {
                    app.ui.tool = Tool::Ripple;
                    Ok(serde_json::Value::Null)
                }
                "cut.blade" => app.session.execute("sequence.addEdit", json!({})),
                "cut.snap" => app.session.execute("sequence.snap", json!({})),
                _ => app.session.execute("sequence.linkedSelection", json!({})),
            };
            if let Err(e) = result {
                app.ui.status = e.to_string();
            }
        }
        x += 31.0;
    }
    // the timeline's name and length under the buttons
    if let Some(seq) = app.session.active_sequence() {
        let rate = seq.settings.frame_rate;
        let len = filmcraft_time::format_time(seq.duration(), rate, seq.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48_000);
        let name = app.session.state.active_sequence.and_then(|id| app.session.project.items.get(&id)).map(|i| i.name.clone()).unwrap_or_default();
        if r.height() > 60.0 {
            p.text(pos2(r.min.x + 12.0, y + 44.0), Align2::LEFT_CENTER, name, Tokens::semibold(11.5), t.text);
            p.text(pos2(r.min.x + 12.0, y + 60.0), Align2::LEFT_CENTER, len, Tokens::ui(11.0), t.text_dim);
        }
    }
    p.line_segment([r.right_top(), r.right_bottom()], Stroke::new(1.0, t.separator));
}

/// The fast edits' own drawings: the timeline as two clips on a track, and where the new clip
/// (accent) lands.
fn edit_glyph(p: &egui::Painter, r: Rect, which: usize, col: Color32, accent: Color32) {
    let stroke = Stroke::new(1.3, col);
    let w = r.width();
    let h = r.height();
    let at = |fx: f32, fy: f32| -> Pos2 { pos2(r.min.x + fx * w, r.min.y + fy * h) };
    let block = |a: Pos2, b: Pos2, fill: Option<Color32>| {
        let br = Rect::from_two_pos(a, b);
        if let Some(f) = fill {
            p.rect_filled(br, 1.0, f);
        } else {
            p.rect_stroke(br, 1.0, stroke, egui::StrokeKind::Inside);
        }
    };
    let arrow_down = |x: f32, y0: f32, y1: f32| {
        p.line_segment([pos2(x, y0), pos2(x, y1)], stroke);
        p.line_segment([pos2(x - 2.5, y1 - 2.5), pos2(x, y1)], stroke);
        p.line_segment([pos2(x + 2.5, y1 - 2.5), pos2(x, y1)], stroke);
    };
    match which {
        // smart insert: a new clip pushed in between two
        0 => {
            block(at(0.0, 0.55), at(0.38, 1.0), None);
            block(at(0.62, 0.55), at(1.0, 1.0), None);
            arrow_down(at(0.5, 0.0).x, at(0.5, 0.0).y, at(0.5, 0.95).y);
        }
        // append: after the last clip
        1 => {
            block(at(0.0, 0.55), at(0.55, 1.0), None);
            block(at(0.62, 0.55), at(1.0, 1.0), Some(accent));
            arrow_down(at(0.81, 0.0).x, at(0.0, 0.0).y, at(0.0, 0.45).y);
        }
        // ripple overwrite: the new clip replaces one, the rest slides
        2 => {
            block(at(0.0, 0.55), at(0.3, 1.0), None);
            block(at(0.34, 0.55), at(0.62, 1.0), Some(accent));
            block(at(0.66, 0.55), at(1.0, 1.0), None);
            p.line_segment([at(0.68, 0.25), at(1.0, 0.25)], stroke);
            p.line_segment([at(0.68, 0.25), at(0.78, 0.1)], stroke);
            p.line_segment([at(0.68, 0.25), at(0.78, 0.4)], stroke);
        }
        // close up: a closer framing above (a head in a frame)
        3 => {
            block(at(0.0, 0.62), at(1.0, 1.0), None);
            block(at(0.25, 0.0), at(0.75, 0.5), None);
            p.circle_filled(at(0.5, 0.22), h * 0.11, accent);
            p.line_segment([at(0.38, 0.45), at(0.62, 0.45)], Stroke::new(1.3, accent));
        }
        // place on top: a new clip on the track above
        4 => {
            block(at(0.0, 0.62), at(1.0, 1.0), None);
            block(at(0.3, 0.05), at(0.75, 0.45), Some(accent));
        }
        // source overwrite: above, lined up with the clip under it (dashed guide)
        _ => {
            block(at(0.0, 0.62), at(1.0, 1.0), None);
            block(at(0.3, 0.05), at(0.75, 0.42), Some(accent));
            let mut y = at(0.0, 0.42).y;
            while y < at(0.0, 0.62).y {
                p.line_segment([pos2(at(0.3, 0.0).x, y), pos2(at(0.3, 0.0).x, (y + 1.5).min(at(0.0, 0.62).y))], stroke);
                y += 3.0;
            }
        }
    }
}
