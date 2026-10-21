//! Resolve's timeline toolbar for the Resolve workflow's Timeline panel (fork RESOLVE-UI-PLAN
//! RS4, laid out from observation of Resolve 21's Edit page; our own icons): the edit-mode tools
//! (Selection, Trim, Blade), the three edits that take the Source Viewer's clip (Insert,
//! Overwrite, Replace), Snapping and Linked Selection, Add Marker, then the zoom buttons and
//! slider on the right. It replaces the Premiere workflow's vertical Tools panel on the Resolve
//! pages; the Premiere workflow never shows it.
//!
//! Automation ids: `timeline.rtool.<tool>` (`selection`, `trim`, `blade`), `timeline.redit.<edit>`
//! (`insert`, `overwrite`, `replace`), `timeline.rtoggle.<key>` (`snap`, `linked`, `marker`),
//! `timeline.rzoom.<key>` (`fit`, `detail`, `out`, `in`, `slider`).

use egui::{Rect, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};

use super::timeline::{Layout, zoom_about};
use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Tool;
use crate::theme::Tokens;

const BTN: f32 = 28.0;
const GAP: f32 = 3.0;
/// The zoom slider's range in points a second (log scale).
const MIN_PPS: f64 = 0.5;
const MAX_PPS: f64 = 4_000.0;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, layout: &Layout, t: &Tokens) {
    let p = ui.painter_at(bar);
    p.rect_filled(bar, 0.0, t.header_bg);
    p.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.separator));
    let cy = bar.center().y;
    let tool = app.ui.tool;
    let trim = matches!(tool, Tool::Ripple | Tool::Rolling | Tool::Slip | Tool::Slide);
    // the tools and edits sit just right of the track headers, as Resolve's sit over the timeline
    let mut x = bar.min.x + app.ui.timeline.header_w.clamp(0.0, bar.width() * 0.4) + 8.0;
    let sep = |x: &mut f32| {
        p.line_segment([pos2(*x + 4.0, cy - 9.0), pos2(*x + 4.0, cy + 9.0)], Stroke::new(1.0, t.separator));
        *x += 11.0;
    };
    let groups: [&[(&str, Icon, bool, &str)]; 3] = [
        &[
            ("timeline.rtool.selection", Icon::Selection, tool == Tool::Selection, "Selection Mode"),
            ("timeline.rtool.trim", Icon::Ripple, trim, "Trim Edit Mode"),
            ("timeline.rtool.blade", Icon::Razor, tool == Tool::Razor, "Blade Edit Mode"),
        ],
        &[
            ("timeline.redit.insert", Icon::Insert, false, "Insert Clip (F9)"),
            ("timeline.redit.overwrite", Icon::Overwrite, false, "Overwrite Clip (F10)"),
            ("timeline.redit.replace", Icon::Lift, false, "Replace Clip (F11)"),
        ],
        &[
            ("timeline.rtoggle.snap", Icon::Magnet, app.session.state.snapping, "Snapping"),
            ("timeline.rtoggle.linked", Icon::Link, app.session.state.linked_selection, "Linked Selection"),
            ("timeline.rtoggle.marker", Icon::Marker, false, "Add Marker (M)"),
        ],
    ];
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            sep(&mut x);
        }
        for &(key, icon, on, tip) in group.iter() {
            let r = Rect::from_center_size(pos2(x + BTN / 2.0, cy), vec2(BTN, BTN));
            if r.max.x > bar.max.x - 260.0 {
                break;
            }
            if button(app, ui, &p, r, key, icon, on, tip, t) {
                run(app, key);
            }
            x += BTN + GAP;
        }
    }
    // zoom, right: full extent, detail, out, slider, in
    let slider_w = 110.0;
    let mut rx = bar.max.x - 12.0;
    let zin = Rect::from_center_size(pos2(rx - BTN / 2.0, cy), vec2(BTN, BTN));
    rx -= BTN + GAP;
    let slider = Rect::from_min_max(pos2(rx - slider_w, cy - 8.0), pos2(rx, cy + 8.0));
    rx -= slider_w + GAP;
    let zout = Rect::from_center_size(pos2(rx - BTN / 2.0, cy), vec2(BTN, BTN));
    rx -= BTN + GAP + 8.0;
    let detail = Rect::from_center_size(pos2(rx - BTN / 2.0, cy), vec2(BTN, BTN));
    rx -= BTN + GAP;
    let fit = Rect::from_center_size(pos2(rx - BTN / 2.0, cy), vec2(BTN, BTN));
    if fit.min.x < x + 8.0 {
        return;
    }
    let width = layout.content.width();
    let ph = app.session.playhead().seconds();
    if button(app, ui, &p, fit, "timeline.rzoom.fit", Icon::Fullscreen, false, "Full Extent Zoom (Shift+Z)", t) {
        app.ui.timeline.fit_pending = true;
    }
    if button(app, ui, &p, detail, "timeline.rzoom.detail", Icon::Zoom, false, "Detail Zoom: around the playhead", t) {
        let v = &mut app.ui.timeline;
        let factor = (200.0 / v.target_pps.max(1e-9)).max(1.0);
        zoom_about(v, factor, ph, width);
    }
    if button(app, ui, &p, zout, "timeline.rzoom.out", Icon::ChevronLeft, false, "Zoom Out", t) {
        zoom_about(&mut app.ui.timeline, 1.0 / 1.5, ph, width);
    }
    if button(app, ui, &p, zin, "timeline.rzoom.in", Icon::Plus, false, "Zoom In", t) {
        zoom_about(&mut app.ui.timeline, 1.5, ph, width);
    }
    // the slider: log of the zoom
    let resp = ui.interact(slider, super::timeline::tid("rzoom-slider"), Sense::click_and_drag()).on_hover_text(tl!("Timeline zoom"));
    app.auto.add("timeline.rzoom.slider", slider, "Timeline zoom");
    let span = (MAX_PPS / MIN_PPS).ln();
    let frac = ((app.ui.timeline.target_pps.clamp(MIN_PPS, MAX_PPS) / MIN_PPS).ln() / span) as f32;
    let track_y = slider.center().y;
    p.line_segment([pos2(slider.min.x + 4.0, track_y), pos2(slider.max.x - 4.0, track_y)], Stroke::new(2.0, t.control_well));
    let knob_x = slider.min.x + 4.0 + frac.clamp(0.0, 1.0) * (slider.width() - 8.0);
    p.circle_filled(pos2(knob_x, track_y), 5.0, if resp.hovered() || resp.dragged() { t.text } else { t.control_handle });
    if (resp.dragged() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let f = ((pos.x - slider.min.x - 4.0) / (slider.width() - 8.0).max(1.0)).clamp(0.0, 1.0) as f64;
        let want = MIN_PPS * (f * span).exp();
        let v = &mut app.ui.timeline;
        let factor = want / v.target_pps.max(1e-9);
        if factor.is_finite() && factor > 0.0 {
            zoom_about(v, factor, ph, width);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn button(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &egui::Painter, r: Rect, key: &str, icon: Icon, on: bool, tip: &str, t: &Tokens) -> bool {
    let resp = ui.interact(r, super::timeline::tid(("rtb", key)), Sense::click()).on_hover_text(tip);
    app.auto.add(key, r, tip);
    if on {
        p.rect_filled(r, 3.0, t.pressed);
    } else if resp.hovered() {
        p.rect_filled(r, 3.0, t.hover);
    }
    icons::paint(p, r.shrink(7.0), icon, if on { t.accent } else { t.icon });
    resp.clicked()
}

fn run(app: &mut FilmcraftApp, key: &str) {
    let tool = |app: &mut FilmcraftApp, tool: Tool| {
        app.ui.tool = tool;
        Ok(Value::Null)
    };
    let result = match key {
        "timeline.rtool.selection" => tool(app, Tool::Selection),
        "timeline.rtool.trim" => tool(app, Tool::Ripple),
        "timeline.rtool.blade" => tool(app, Tool::Razor),
        "timeline.redit.insert" => edit(app, "edit.insert", "source.insert"),
        "timeline.redit.overwrite" => edit(app, "edit.overwrite", "source.overwrite"),
        "timeline.redit.replace" => edit(app, "edit.replace", "edit.replace"),
        "timeline.rtoggle.snap" => app.session.execute("sequence.snap", json!({})),
        "timeline.rtoggle.linked" => app.session.execute("sequence.linkedSelection", json!({})),
        _ => app.session.execute("markers.add", json!({})),
    };
    if let Err(e) = result {
        app.ui.status = e.to_string();
    }
}

/// An edit by Resolve's command, or by the older one where Resolve's isn't there.
fn edit(app: &mut FilmcraftApp, id: &str, fallback: &str) -> Result<Value, filmcraft_engine::EngineError> {
    let id = if filmcraft_engine::commands::find(id).is_some() { id } else { fallback };
    app.session.execute(id, json!({}))
}
