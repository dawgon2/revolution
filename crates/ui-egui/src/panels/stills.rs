//! Stills gallery strip and the Program monitor's wipe / split comparison (GRADE-PLAN G1.5).
//!
//! The strip ([`show`]) lists the project's stills as thumbnails: a click selects a still for
//! comparison (`compare.set`), a double click applies its grade to the selected clips
//! (`stills.apply`). The comparison itself ([`compare_overlay`]) is drawn over the Program
//! monitor's picture; dragging its divider calls `compare.set`. All state lives in the engine
//! (`stills.*`, `compare.*`), so the control channel can drive and read it. Textures are cached in
//! egui's memory, keyed by still, so the app needs no state fields.
//!
//! Automation ids: `stills.grab`, `stills.delete`, `stills.mode.<off|wipe|split>`,
//! `stills.item.<id>`, `compare.divider`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureId, pos2, vec2};
use filmcraft_engine::stills::{CompareMode, CompareState};
use serde_json::json;

use crate::FilmcraftApp;
use crate::theme::Tokens;

/// Thumbnails are at most this wide in pixels.
const THUMB_W: usize = 192;
/// Frames to wait before trying a picture that failed to load again.
const RETRY_FRAMES: u64 = 600;
/// The divider's grab width in points.
const GRAB: f32 = 14.0;

struct Entry {
    sig: u64,
    tex: Option<TextureHandle>,
    tried: u64,
}

#[derive(Default)]
struct Cache {
    /// (still id, thumbnail?) to its texture.
    map: HashMap<(u64, bool), Entry>,
}

type Shared = Arc<Mutex<Cache>>;

fn cache(ctx: &egui::Context) -> Shared {
    let id = egui::Id::new("filmcraft.stills.cache");
    if let Some(c) = ctx.data(|d| d.get_temp::<Shared>(id)) {
        return c;
    }
    let c: Shared = Arc::default();
    ctx.data_mut(|d| d.insert_temp(id, c.clone()));
    c
}

/// A cheap fingerprint of what a still's picture depends on.
fn signature(st: &filmcraft_project::Still) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (st.id, st.nonce, st.time.0, st.clip.map(|c| c.0), st.width, st.height).hash(&mut h);
    // the grade's content (a grab that reuses an id after an undo has another grade)
    serde_json::to_string(&st.grade).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// Box-reduce RGBA8 by an integer factor so the longest side fits `max_w` (never panics on a
/// short buffer: the caller checks `rgba.len() == w * h * 4`).
fn reduce(w: usize, h: usize, rgba: &[u8], max_w: usize) -> (usize, usize, Vec<u8>) {
    let step = w.div_ceil(max_w.max(1)).max(1);
    if step == 1 {
        return (w, h, rgba.to_vec());
    }
    let (nw, nh) = ((w / step).max(1), (h / step).max(1));
    let mut out = Vec::with_capacity(nw * nh * 4);
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for dy in 0..step {
                for dx in 0..step {
                    let i = ((y * step + dy) * w + x * step + dx) * 4;
                    if let Some(px) = rgba.get(i..i + 4) {
                        for (a, v) in acc.iter_mut().zip(px) {
                            *a += u32::from(*v);
                        }
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            out.extend(acc.iter().map(|a| (a / n) as u8));
        }
    }
    (nw, nh, out)
}

/// The texture of still `id` (a thumbnail, or the full picture), loading it on first use.
fn texture(app: &FilmcraftApp, ctx: &egui::Context, id: u64, thumb: bool) -> Option<TextureId> {
    let st = app.session.project.stills.iter().find(|s| s.id == id)?;
    let sig = signature(st);
    let shared = cache(ctx);
    let mut c = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let now = ctx.cumulative_pass_nr();
    if let Some(e) = c.map.get(&(id, thumb)) {
        let stale = e.sig != sig || (e.tex.is_none() && now.saturating_sub(e.tried) > RETRY_FRAMES);
        if !stale {
            return e.tex.as_ref().map(TextureHandle::id);
        }
    }
    let tex = app.session.still_picture(id).and_then(|p| {
        let (w, h) = (p.w as usize, p.h as usize);
        if w == 0 || h == 0 || p.rgba.len() != w.checked_mul(h)?.checked_mul(4)? {
            return None;
        }
        let (w, h, px) = if thumb { reduce(w, h, &p.rgba, THUMB_W) } else { (w, h, p.rgba) };
        let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &px);
        Some(ctx.load_texture(format!("still-{id}-{thumb}"), img, egui::TextureOptions::LINEAR))
    });
    let out = tex.as_ref().map(TextureHandle::id);
    c.map.insert((id, thumb), Entry { sig, tex, tried: now });
    out
}

/// Drop the textures of stills that no longer exist.
fn prune(app: &FilmcraftApp, ctx: &egui::Context) {
    let shared = cache(ctx);
    let mut c = shared.lock().unwrap_or_else(PoisonError::into_inner);
    c.map.retain(|(id, _), _| app.session.project.stills.iter().any(|s| s.id == *id));
}

fn button(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, label: &str, id: &str, on: bool, enabled: bool) -> bool {
    let t = app.tokens;
    let resp = ui.interact(r, egui::Id::new(("stills.btn", id)), if enabled { Sense::click() } else { Sense::hover() });
    let fill = if on {
        t.accent
    } else if resp.hovered() && enabled {
        t.hover
    } else {
        t.field_bg
    };
    ui.painter().rect(r, CornerRadius::same(t.radius_sm as u8), fill, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(11.0), if enabled { t.text } else { t.text_faint });
    app.auto.add(id, r, label);
    enabled && resp.clicked()
}

fn run(app: &mut FilmcraftApp, command: &str, params: serde_json::Value) {
    if let Err(e) = app.session.execute(command, params) {
        app.session.toast(e.to_string());
    }
}

/// egui temp-data key of the Gallery's rect and the pass it was drawn in (where dropped
/// pictures become stills; a Gallery not drawn lately takes no drops).
pub const STRIP_RECT: &str = "stills.strip_rect";

/// The stills gallery strip.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    prune(app, &ctx);
    let cmp = app.session.state.compare.clone();
    let have = |id: Option<u64>| id.is_some_and(|i| app.session.project.stills.iter().any(|s| s.id == i));
    let selected = cmp.still.filter(|i| have(Some(*i)));
    // ---- header: Grab Still | Delete | compare mode
    let head = Rect::from_min_size(rect.min + vec2(8.0, 6.0), vec2((rect.width() - 16.0).max(0.0), 24.0));
    let x = std::cell::Cell::new(head.min.x);
    let y = std::cell::Cell::new(head.min.y);
    // buttons wrap to a second row in a narrow panel
    let next = |w: f32| {
        if x.get() + w > head.max.x && x.get() > head.min.x {
            x.set(head.min.x);
            y.set(y.get() + head.height() + 6.0);
        }
        let r = Rect::from_min_size(pos2(x.get(), y.get()), vec2(w, head.height()));
        x.set(x.get() + w + 6.0);
        r
    };
    if button(app, ui, next(84.0), "Grab Still", "stills.grab", false, app.session.state.active_sequence.is_some()) {
        run(app, "stills.grab", json!({}));
    }
    if button(app, ui, next(76.0), "Import…", "stills.import", false, true) {
        // a reference picture to compare against and match to (REF.1)
        if let Err(e) = app.file_dialog("stills.import", &json!({})) {
            app.session.toast(e);
        }
    }
    if button(app, ui, next(60.0), "Delete", "stills.delete", false, selected.is_some())
        && let Some(id) = selected
    {
        run(app, "stills.delete", json!({ "still": id }));
    }
    x.set(x.get() + 10.0);
    for (mode, label, key) in [(CompareMode::Off, "Off", "off"), (CompareMode::Wipe, "Wipe", "wipe"), (CompareMode::Split, "Split", "split")] {
        let id = format!("stills.mode.{key}");
        if button(app, ui, next(48.0), label, &id, cmp.mode == mode, mode == CompareMode::Off || selected.is_some()) {
            run(app, "compare.set", json!({ "mode": key }));
        }
    }
    // ---- thumbnails, scrolled sideways with the wheel
    let strip = Rect::from_min_max(pos2(rect.min.x + 8.0, y.get() + head.height() + 8.0), pos2(rect.max.x - 8.0, rect.max.y - 6.0));
    // pictures dropped here are imported as reference stills (see the app's drop handling)
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(STRIP_RECT), (rect, pass)));
    if strip.height() < 24.0 || strip.width() < 24.0 {
        return;
    }
    let label_h = 16.0;
    let th = (strip.height() - label_h).clamp(20.0, 160.0);
    let rows: Vec<(u64, String, f32)> = app
        .session
        .project
        .stills
        .iter()
        .map(|s| {
            let aspect = if s.width > 0 && s.height > 0 { s.width as f32 / s.height as f32 } else { 16.0 / 9.0 };
            (s.id, s.label.clone(), aspect.clamp(0.25, 4.0))
        })
        .collect();
    if rows.is_empty() {
        ui.painter().text(
            strip.left_center(),
            Align2::LEFT_CENTER,
            "No stills yet. Grab Still saves the Program frame and its grade; Import… (or drop a picture here) brings in a reference look.",
            Tokens::ui(11.0),
            t.text_dim,
        );
        return;
    }
    let total: f32 = rows.iter().map(|(_, _, a)| th * a + 8.0).sum();
    let scroll_id = egui::Id::new("stills.scroll");
    let mut scroll: f32 = ctx.data(|d| d.get_temp(scroll_id)).unwrap_or(0.0);
    if ui.rect_contains_pointer(strip) {
        let d = ctx.input(|i| i.smooth_scroll_delta);
        scroll -= if d.x != 0.0 { d.x } else { d.y };
    }
    scroll = scroll.clamp(0.0, (total - strip.width()).max(0.0));
    ctx.data_mut(|d| d.insert_temp(scroll_id, scroll));
    let painter = ui.painter().with_clip_rect(strip);
    let mut x = strip.min.x - scroll;
    for (id, label, aspect) in rows {
        let w = th * aspect;
        let r = Rect::from_min_size(pos2(x, strip.min.y), vec2(w, th));
        x += w + 8.0;
        if r.max.x < strip.min.x || r.min.x > strip.max.x {
            continue;
        }
        painter.rect_filled(r, 2.0, t.monitor_bg);
        if let Some(tex) = texture(app, &ctx, id, true) {
            painter.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        } else {
            painter.text(r.center(), Align2::CENTER_CENTER, "no picture", Tokens::ui(10.0), t.text_faint);
        }
        let on = selected == Some(id);
        painter.rect_stroke(r, 2.0, Stroke::new(if on { 2.0 } else { 1.0 }, if on { t.accent } else { t.separator }), StrokeKind::Inside);
        painter.text(
            pos2(r.min.x, r.max.y + 2.0),
            Align2::LEFT_TOP,
            if label.is_empty() { format!("Still {id}") } else { label },
            Tokens::ui(10.0),
            t.text_dim,
        );
        let resp = ui.interact(r.intersect(strip), egui::Id::new(("stills.item", id)), Sense::click());
        app.auto.add(&format!("stills.item.{id}"), r.intersect(strip), "Still");
        if resp.double_clicked() {
            run(app, "stills.apply", json!({ "still": id }));
        } else if resp.clicked() {
            let mode = if cmp.mode == CompareMode::Split { "split" } else { "wipe" };
            run(app, "compare.set", json!({ "still": id, "mode": mode }));
        }
    }
}

/// Draw the comparison over the Program monitor's picture `pic`. `live` is the texture of the
/// live frame (drawn already; split view redraws it beside the still). The divider's drag
/// handling is [`compare_divider`].
pub fn compare_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, live: Option<TextureId>) {
    let cmp: CompareState = app.session.state.compare.clone();
    let Some(id) = cmp.still.filter(|_| cmp.mode != CompareMode::Off) else { return };
    if pic.width() < 8.0 || pic.height() < 8.0 || !app.session.project.stills.iter().any(|s| s.id == id) {
        return;
    }
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let tex = texture(app, &ctx, id, false);
    let pos = if cmp.position.is_finite() { cmp.position.clamp(0.0, 1.0) } else { 0.5 };
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let painter = ui.painter();
    let (first, second, line) = if cmp.vertical {
        let y = pic.min.y + pic.height() * pos;
        (Rect::from_min_max(pic.min, pos2(pic.max.x, y)), Rect::from_min_max(pos2(pic.min.x, y), pic.max), [pos2(pic.min.x, y), pos2(pic.max.x, y)])
    } else {
        let x = pic.min.x + pic.width() * pos;
        (Rect::from_min_max(pic.min, pos2(x, pic.max.y)), Rect::from_min_max(pos2(x, pic.min.y), pic.max), [pos2(x, pic.min.y), pos2(x, pic.max.y)])
    };
    let uv_of = |part: Rect| {
        Rect::from_min_max(
            pos2((part.min.x - pic.min.x) / pic.width(), (part.min.y - pic.min.y) / pic.height()),
            pos2((part.max.x - pic.min.x) / pic.width(), (part.max.y - pic.min.y) / pic.height()),
        )
    };
    match cmp.mode {
        CompareMode::Wipe => {
            // the still covers the first part of the picture, in the same coordinates
            if let Some(tex) = tex {
                painter.image(tex, first, uv_of(first), Color32::WHITE);
            }
        }
        CompareMode::Split => {
            // side by side: the whole still in the first part, the whole live frame in the second
            painter.rect_filled(pic, 0.0, t.monitor_bg);
            let aspect = pic.width() / pic.height();
            let fit = |part: Rect| crate::panels::monitor::fit(part, aspect, 1.0);
            if let Some(tex) = tex {
                painter.image(tex, fit(first), full, Color32::WHITE);
            }
            if let Some(live) = live {
                painter.image(live, fit(second), full, Color32::WHITE);
            }
        }
        CompareMode::Off => {}
    }
    painter.line_segment(line, Stroke::new(1.5, Color32::WHITE));
    painter.line_segment(line, Stroke::new(0.5, Color32::BLACK));
}

/// The divider's grab area while a comparison shows.
fn divider_handle(cmp: &CompareState, pic: Rect) -> Rect {
    let pos = if cmp.position.is_finite() { cmp.position.clamp(0.0, 1.0) } else { 0.5 };
    if cmp.vertical {
        let y = pic.min.y + pic.height() * pos;
        Rect::from_min_max(pos2(pic.min.x, y - GRAB / 2.0), pos2(pic.max.x, y + GRAB / 2.0))
    } else {
        let x = pic.min.x + pic.width() * pos;
        Rect::from_min_max(pos2(x - GRAB / 2.0, pic.min.y), pos2(x + GRAB / 2.0, pic.max.y))
    }
}

/// The draggable divider of the comparison drawn by [`compare_overlay`]. Call it after the other
/// monitor overlays so it sits on top of them (the last widget added wins the pointer). Dragging
/// calls `compare.set`.
pub fn compare_divider(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect) {
    let cmp: CompareState = app.session.state.compare.clone();
    let Some(id) = cmp.still.filter(|_| cmp.mode != CompareMode::Off) else { return };
    if pic.width() < 8.0 || pic.height() < 8.0 || !app.session.project.stills.iter().any(|s| s.id == id) {
        return;
    }
    let handle = divider_handle(&cmp, pic);
    let resp = ui.interact(handle, egui::Id::new("compare.divider"), Sense::drag());
    app.auto.add("compare.divider", handle, "Wipe");
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(if cmp.vertical { egui::CursorIcon::ResizeVertical } else { egui::CursorIcon::ResizeHorizontal });
    }
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let v = if cmp.vertical { (p.y - pic.min.y) / pic.height() } else { (p.x - pic.min.x) / pic.width() };
        if v.is_finite() {
            run(app, "compare.set", json!({ "position": v.clamp(0.0, 1.0) }));
        }
    }
}
