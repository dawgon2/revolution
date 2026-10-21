//! The Inspector's Video and File tabs (fork RS4.2; the panel is `inspector.rs`).
//!
//! Video: Transform (Motion, with Attach to Track), Cropping (Crop), Composite (Opacity), Speed Change (the clip's
//! speed), Stabilization (Warp Stabilizer) and Temporal NR (the clip's, shared rows of
//! `tnr.rs`). Values are shown in Resolve's units and written back in the engine's, which are
//! Premiere's (the same parameters Effect Controls edits):
//!
//! - Zoom 1.000 = Scale 100 %; Zoom X is Scale Width while the link is off.
//! - Position and Anchor Point in pixels from the frame's (the clip's) centre, Y up; the engine
//!   keeps them from the top-left corner, Y down, and an unset ("auto") point is the centre.
//! - Rotation Angle counter-clockwise; the engine's Rotation turns clockwise.
//! - Opacity 0–100, as the engine has it.
//!
//! File: the clip's name and its media's path, codec, resolution, frame rate, duration and audio
//! format, from the media info in the project.

use egui::{Align2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_geom::Vec2;
use filmcraft_project::{MediaRef, TrackItem};
use serde_json::json;

use super::inspector::{self as insp, Acts, Cx, Fx, Key, key_all, key_of};
use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Resolve's position from the engine's: pixels from the centre of `size`, Y up. An unset
/// ("auto", NaN) point is the centre.
pub(crate) fn from_engine_point(p: Vec2, size: (f64, f64)) -> (f64, f64) {
    let x = if p.x.is_finite() { p.x - size.0 / 2.0 } else { 0.0 };
    let y = if p.y.is_finite() { size.1 / 2.0 - p.y } else { 0.0 };
    (x, y)
}

/// The engine's point from Resolve's (the inverse of [`from_engine_point`]).
pub(crate) fn to_engine_point(x: f64, y: f64, size: (f64, f64)) -> [f64; 2] {
    [size.0 / 2.0 + x, size.1 / 2.0 - y]
}

pub(crate) fn video_tab(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &Cx, acts: &mut Acts) {
    let Some(seq) = app.session.active_sequence() else { return };
    let frame = (f64::from(seq.settings.width.max(1)), f64::from(seq.settings.height.max(1)));
    // the clip's own size (the anchor is in its pixels); the frame's when the media has none
    let src = app
        .session
        .project
        .resolve_media(cx.it.item)
        .and_then(|(_, m, _)| m.info.video.as_ref().map(|v| (f64::from(v.width.max(1)), f64::from(v.height.max(1)))))
        .unwrap_or(frame);
    transform(app, ui, cx, frame, src, acts);
    let it = cx.it.clone();
    let inst = |id: &str| it.effects.iter().enumerate().find(|(_, e)| e.effect == id);
    if let Some(def) = filmcraft_project::find_effect("crop") {
        let fx = Fx {
            sec: "cropping",
            title: tl!("Cropping"),
            def,
            inst: inst("crop"),
            order: &["left", "right", "top", "bottom", "feather"],
            skip: &[],
            labels: &[("left", "Crop Left"), ("right", "Crop Right"), ("top", "Crop Top"), ("bottom", "Crop Bottom"), ("feather", "Softness")],
            removable: false,
        };
        insp::fx_section(app, ui, cx, fx, acts);
    }
    if let Some(def) = filmcraft_project::find_effect("opacity") {
        let fx = Fx {
            sec: "composite",
            title: tl!("Composite"),
            def,
            inst: inst("opacity"),
            order: &["blend", "opacity"],
            skip: &[],
            labels: &[("blend", "Composite Mode")],
            removable: false,
        };
        insp::fx_section(app, ui, cx, fx, acts);
    }
    speed(app, ui, cx, acts);
    if let Some(def) = filmcraft_project::find_effect("warp_stabilizer") {
        let fx =
            Fx { sec: "stabilization", title: tl!("Stabilization"), def, inst: inst("warp_stabilizer"), order: &[], skip: &[], labels: &[], removable: false };
        insp::fx_section(app, ui, cx, fx, acts);
    }
    if crate::panels::tnr::takes_tnr(&app.session, cx.clip) {
        temporal_nr(app, ui, cx, acts);
    }
}

/// Transform: Zoom X / Y (linked), Position, Rotation Angle, Anchor Point, Flip Image.
fn transform(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &Cx, frame: (f64, f64), src: (f64, f64), acts: &mut Acts) {
    let t = app.tokens;
    let it = &cx.it;
    let Some((mi, m)) = it.effects.iter().enumerate().find(|(_, e)| e.effect == "motion") else { return };
    let num = |p: &str, d: f64| m.param(p).and_then(|p| p.value_at(cx.mt).as_f64()).filter(|v| v.is_finite()).unwrap_or(d);
    let point = |p: &str| m.param(p).and_then(|p| p.value_at(cx.mt).as_vec2()).unwrap_or(Vec2::new(f64::NAN, f64::NAN));
    let uniform = m.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
    let scale = num("scale", 100.0);
    let width = if uniform { scale } else { num("scale_width", 100.0) };
    let zoom_params: &[&str] = if uniform { &["scale"] } else { &["scale", "scale_width"] };
    let k = |p: &str| key_of(Some(m), p, cx.mt);
    let zoom_keys: Vec<(&str, Key)> = zoom_params.iter().map(|p| (*p, k(p))).collect();
    let mut all: Vec<(&str, Key)> = vec![("position", k("position")), ("rotation", k("rotation")), ("anchor", k("anchor"))];
    all.extend(zoom_keys.iter().copied());
    let states: Vec<Key> = all.iter().map(|x| x.1).collect();
    let head = insp::section(app, ui, "transform", tl!("Transform"), Some(m.enabled), Some(key_all(&states)), true, false);
    let clip = cx.clip.0;
    let set = |acts: &mut Acts, param: &str, value: serde_json::Value| {
        acts.push(("effects.setParam".into(), json!({"clip": clip, "effect": "motion", "param": param, "value": value})));
    };
    let reset = |acts: &mut Acts, param: &str| acts.push(("effects.resetParam".into(), json!({"clip": clip, "effect": "motion", "param": param})));
    if head.dot {
        acts.push(("effects.toggleEnabled".into(), json!({"clip": clip, "index": mi})));
    }
    if head.reset {
        acts.push(("effects.reset".into(), json!({"clip": clip, "index": mi})));
    }
    if head.key {
        insp::section_keyframes(acts, cx.clip, json!("motion"), &all);
    }
    if !head.open {
        return;
    }
    // Zoom X / Y with the link between them
    let row = insp::row(ui, tl!("Zoom"), &t);
    let (a, b) = insp::pair_rects(row.value);
    insp::axis_label(ui, pos2(a.min.x - 11.0, a.center().y), "X", &t);
    insp::axis_label(ui, pos2(b.min.x - 11.0, b.center().y), "Y", &t);
    if let Some(v) = insp::num_field(app, ui, a, "inspector.transform.zoomX", width / 100.0, 0.005, (0.0, 100.0), 3, false) {
        set(acts, if uniform { "scale" } else { "scale_width" }, json!(v * 100.0));
    }
    if let Some(v) = insp::num_field(app, ui, b, "inspector.transform.zoomY", scale / 100.0, 0.005, (0.0, 100.0), 3, false) {
        set(acts, "scale", json!(v * 100.0));
    }
    let lr = Rect::from_center_size(pos2((a.max.x + b.min.x - 12.0) / 2.0, row.value.center().y), vec2(16.0, 16.0));
    let tip = if uniform { tl!("Unlink Zoom X and Y") } else { tl!("Link Zoom X and Y") };
    let lresp = ui.interact(lr, egui::Id::new(("insp-zoom-link", clip)), Sense::click()).on_hover_text(tip);
    icons::paint(ui.painter(), lr.shrink(1.0), Icon::Link, if uniform { t.accent } else { t.text_faint });
    app.auto.add("inspector.transform.zoomLink", lr, tip);
    if lresp.clicked() {
        set(acts, "uniform_scale", json!(!uniform));
        if uniform {
            // unlinked, Zoom X becomes Scale Width: it starts where the linked zoom was
            set(acts, "scale_width", json!(scale));
        }
    }
    if insp::keyframe_button(app, ui, row.key, "inspector.transform.zoom.keyframe", key_all(&zoom_keys.iter().map(|x| x.1).collect::<Vec<_>>())) {
        insp::section_keyframes(acts, cx.clip, json!("motion"), &zoom_keys);
    }
    if insp::reset_button(app, ui, row.reset, "inspector.transform.zoom.reset") {
        reset(acts, "scale");
        reset(acts, "scale_width");
    }
    // Position: pixels from the frame's centre, Y up
    let (px, py) = from_engine_point(point("position"), frame);
    let row = insp::row(ui, tl!("Position"), &t);
    let (a, b) = insp::pair_rects(row.value);
    insp::axis_label(ui, pos2(a.min.x - 11.0, a.center().y), "X", &t);
    insp::axis_label(ui, pos2(b.min.x - 11.0, b.center().y), "Y", &t);
    let nx = insp::num_field(app, ui, a, "inspector.transform.positionX", px, 1.0, (-100_000.0, 100_000.0), 3, false);
    let ny = insp::num_field(app, ui, b, "inspector.transform.positionY", py, 1.0, (-100_000.0, 100_000.0), 3, false);
    if nx.is_some() || ny.is_some() {
        set(acts, "position", json!(to_engine_point(nx.unwrap_or(px), ny.unwrap_or(py), frame)));
    }
    if insp::keyframe_button(app, ui, row.key, "inspector.transform.position.keyframe", k("position")) {
        insp::section_keyframes(acts, cx.clip, json!("motion"), &[("position", k("position"))]);
    }
    if insp::reset_button(app, ui, row.reset, "inspector.transform.position.reset") {
        reset(acts, "position");
    }
    // Rotation Angle: counter-clockwise
    // (+ 0.0: no "-0.000" for an unrotated clip)
    let angle = -num("rotation", 0.0) + 0.0;
    let row = insp::row(ui, tl!("Rotation Angle"), &t);
    let fw = 64.0;
    let sr = Rect::from_min_max(row.value.min, pos2(row.value.max.x - fw - 8.0, row.value.max.y));
    let fr = Rect::from_min_max(pos2(row.value.max.x - fw, row.value.min.y), row.value.max);
    if let Some(v) = insp::slider(app, ui, sr, "inspector.transform.rotation", angle, (-180.0, 180.0)) {
        set(acts, "rotation", json!(-v));
    }
    if let Some(v) = insp::num_field(app, ui, fr, "inspector.transform.rotation", angle, 0.5, (-36000.0, 36000.0), 3, false) {
        set(acts, "rotation", json!(-v));
    }
    if insp::keyframe_button(app, ui, row.key, "inspector.transform.rotation.keyframe", k("rotation")) {
        insp::section_keyframes(acts, cx.clip, json!("motion"), &[("rotation", k("rotation"))]);
    }
    if insp::reset_button(app, ui, row.reset, "inspector.transform.rotation.reset") {
        reset(acts, "rotation");
    }
    // Anchor Point: pixels from the clip's centre, Y up
    let (ax, ay) = from_engine_point(point("anchor"), src);
    let row = insp::row(ui, tl!("Anchor Point"), &t);
    let (a, b) = insp::pair_rects(row.value);
    insp::axis_label(ui, pos2(a.min.x - 11.0, a.center().y), "X", &t);
    insp::axis_label(ui, pos2(b.min.x - 11.0, b.center().y), "Y", &t);
    let nx = insp::num_field(app, ui, a, "inspector.transform.anchorX", ax, 1.0, (-100_000.0, 100_000.0), 3, false);
    let ny = insp::num_field(app, ui, b, "inspector.transform.anchorY", ay, 1.0, (-100_000.0, 100_000.0), 3, false);
    if nx.is_some() || ny.is_some() {
        set(acts, "anchor", json!(to_engine_point(nx.unwrap_or(ax), ny.unwrap_or(ay), src)));
    }
    if insp::keyframe_button(app, ui, row.key, "inspector.transform.anchor.keyframe", k("anchor")) {
        insp::section_keyframes(acts, cx.clip, json!("motion"), &[("anchor", k("anchor"))]);
    }
    if insp::reset_button(app, ui, row.reset, "inspector.transform.anchor.reset") {
        reset(acts, "anchor");
    }
    // Flip Image: the Horizontal / Vertical Flip effects, on or off
    let row = insp::row(ui, tl!("Flip Image"), &t);
    for (n, (effect, id, tip)) in
        [("horizontal_flip", "inspector.transform.flipH", tl!("Flip Horizontal")), ("vertical_flip", "inspector.transform.flipV", tl!("Flip Vertical"))]
            .into_iter()
            .enumerate()
    {
        let br = Rect::from_min_size(pos2(row.value.min.x + n as f32 * 34.0, row.value.min.y), vec2(28.0, row.value.height()));
        let on = it.effects.iter().position(|e| e.effect == effect);
        let resp = ui.interact(br, egui::Id::new(("insp-flip", effect, clip)), Sense::click()).on_hover_text(tip);
        ui.painter().rect_filled(br, 2.0, if resp.hovered() { t.hover } else { t.field_bg });
        ui.painter().rect_stroke(br, 2.0, Stroke::new(1.0, if on.is_some() { t.accent } else { t.field_border }), egui::StrokeKind::Inside);
        flip_glyph(ui.painter(), br.shrink(4.0), n == 1, if on.is_some() { t.accent } else { t.text_dim });
        app.auto.add(id, br, tip);
        if resp.clicked() {
            match on {
                Some(i) => acts.push(("effects.remove".into(), json!({"clip": clip, "index": i}))),
                None => acts.push(("effects.apply".into(), json!({"clips": [clip], "effect": effect}))),
            }
        }
    }
    // Attach to Track (fork Y3.2b): Effect Controls' row
    let (body, lane) = insp::ec_row_rects(ui);
    crate::panels::effect_controls::attach_track_row(app, ui, body, &lane, cx.clip, acts);
}

/// Two triangles back to back across a dashed axis (turned for the vertical flip).
fn flip_glyph(p: &egui::Painter, r: Rect, vertical: bool, ink: egui::Color32) {
    let c = r.center();
    let (h, w) = (r.height().min(r.width()) * 0.42, r.width().min(r.height()) * 0.42);
    let tri = |s: f32| -> Vec<egui::Pos2> {
        if vertical {
            vec![c + vec2(-w, s * 1.5), c + vec2(w, s * 1.5), c + vec2(0.0, s * (h + 1.5))]
        } else {
            vec![c + vec2(s * 1.5, -h), c + vec2(s * 1.5, h), c + vec2(s * (w + 1.5), 0.0)]
        }
    };
    p.add(egui::Shape::convex_polygon(tri(-1.0), ink, Stroke::NONE));
    p.add(egui::Shape::closed_line(tri(1.0), Stroke::new(1.0, ink)));
}

/// Speed Change: direction, speed and the duration it gives (`clip.speedDuration`, one undo step
/// per change; the clip's linked audio follows).
fn speed(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &Cx, acts: &mut Acts) {
    let t = app.tokens;
    let it = &cx.it;
    let clip = cx.clip.0;
    let pct = if it.speed.is_finite() { it.speed.abs() * 100.0 } else { 100.0 };
    let head = insp::section(app, ui, "speed", tl!("Speed Change"), None, None, true, false);
    if head.reset {
        acts.push(("clip.speedDuration".into(), json!({"clips": [clip], "speed": 100.0, "reverse": false})));
    }
    if !head.open {
        return;
    }
    let row = insp::row(ui, tl!("Direction"), &t);
    for (n, (reverse, id, label)) in
        [(false, "inspector.speed.forward", tl!("Forward")), (true, "inspector.speed.reverse", tl!("Reverse"))].into_iter().enumerate()
    {
        let br = Rect::from_min_size(pos2(row.value.min.x + n as f32 * 34.0, row.value.min.y), vec2(28.0, row.value.height()));
        let on = it.reverse == reverse;
        let resp = ui.interact(br, egui::Id::new(("insp-dir", reverse, clip)), Sense::click()).on_hover_text(label);
        ui.painter().rect_filled(br, 2.0, if resp.hovered() { t.hover } else { t.field_bg });
        ui.painter().rect_stroke(br, 2.0, Stroke::new(1.0, if on { t.accent } else { t.field_border }), egui::StrokeKind::Inside);
        let c = br.center();
        let d = if reverse { -1.0 } else { 1.0 };
        let tri = vec![c + vec2(-4.0 * d, -5.0), c + vec2(-4.0 * d, 5.0), c + vec2(5.0 * d, 0.0)];
        ui.painter().add(egui::Shape::convex_polygon(tri, if on { t.accent } else { t.text_dim }, Stroke::NONE));
        app.auto.add(id, br, label);
        if resp.clicked() && !on {
            acts.push(("clip.speedDuration".into(), json!({"clips": [clip], "speed": pct, "reverse": reverse})));
        }
    }
    let row = insp::row(ui, tl!("Speed"), &t);
    let fr = Rect::from_min_size(row.value.min, vec2(row.value.width().min(90.0), row.value.height()));
    if let Some(v) = insp::num_field(app, ui, fr, "inspector.speed.speed", pct, 0.5, (1.0, 10_000.0), 2, true) {
        acts.push(("clip.speedDuration".into(), json!({"clips": [clip], "speed": v, "reverse": it.reverse})));
    }
    ui.painter().text(pos2(fr.max.x + 6.0, fr.center().y), Align2::LEFT_CENTER, "%", Tokens::ui(12.0), t.text_dim);
    if let Some(seq) = app.session.active_sequence() {
        let row = insp::row(ui, tl!("Duration"), &t);
        let tc =
            filmcraft_time::format_time(it.duration, seq.settings.frame_rate.sane(), seq.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48000);
        ui.painter().text(pos2(row.value.min.x + 4.0, row.value.center().y), Align2::LEFT_CENTER, tc, Tokens::mono(12.0), t.text);
    }
}

/// Temporal NR (fork 6.7b): the clip's shared rows (`tnr.rs`), under a header whose dot is
/// `tnr.toggle` and whose arrow is `tnr.reset`.
fn temporal_nr(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &Cx, acts: &mut Acts) {
    let t = app.tokens;
    let it = &cx.it;
    let present = it.temporal_nr.is_some();
    let on = it.temporal_nr.as_ref().is_some_and(|n| n.enabled);
    let head = insp::section(app, ui, "tnr", tl!("Temporal NR"), Some(on), None, present, false);
    if head.dot {
        acts.push(("tnr.toggle".into(), json!({"clip": cx.clip.0, "on": !on})));
    }
    if head.reset {
        acts.push(("tnr.reset".into(), json!({"clip": cx.clip.0})));
    }
    if !head.open {
        return;
    }
    let merge = format!("insp-{}", crate::panels::grade::gesture(ui));
    let mut cmds: Vec<(&'static str, serde_json::Value)> = Vec::new();
    let mut autos: Vec<(String, Rect, String)> = Vec::new();
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 12.0).max(120.0));
            crate::panels::tnr::tnr_rows(ui, cx.clip, it, cx.mt, &t, &merge, &mut cmds, &mut autos);
            crate::panels::tnr::note(ui, &t);
        });
    });
    let visible = ui.clip_rect();
    for (id, rect, label) in autos {
        if rect.is_positive() && visible.intersects(rect) {
            app.auto.add(&id, rect, &label);
        }
    }
    acts.extend(cmds.into_iter().map(|(c, p)| (c.to_string(), p)));
}

/// The File tab: the clip's media, from the media info the project keeps.
pub(crate) fn file_tab(app: &mut FilmcraftApp, ui: &mut egui::Ui, it: &TrackItem) {
    let t = app.tokens;
    let Some((_, media, _)) = app.session.project.resolve_media(it.item) else { return };
    let info = &media.info;
    let path = match &media.media {
        MediaRef::File { path } => path.clone(),
        MediaRef::Generator(_) => info.name.clone(),
    };
    let rate = media.frame_rate().sane();
    let mut rows: Vec<(&str, &str, String)> = vec![("name", tl!("Name"), it.name.clone()), ("path", tl!("Path"), path)];
    if let Some(v) = &info.video {
        rows.push(("codec", tl!("Codec"), v.codec.clone()));
        rows.push(("resolution", tl!("Resolution"), format!("{} × {}", v.width, v.height)));
        rows.push(("frameRate", tl!("Frame Rate"), rate.label()));
        if let Some(t) = media.vfr() {
            rows.push(("vfr", tl!("Variable Frame Rate"), t.numbers()));
        }
    } else if let Some(a) = info.audio_streams.first() {
        rows.push(("codec", tl!("Codec"), a.codec.clone()));
    }
    rows.push(("duration", tl!("Duration"), filmcraft_time::format_time(info.duration, rate, false, filmcraft_time::TimeDisplay::Timecode, 48000)));
    if let Some(a) = info.audio_streams.first() {
        rows.push(("sampleRate", tl!("Sample Rate"), format!("{} Hz", a.sample_rate)));
        rows.push(("channels", tl!("Channels"), a.channels.to_string()));
    }
    ui.add_space(6.0);
    for (key, label, value) in rows {
        let row = insp::row(ui, label, &t);
        let r = Rect::from_min_max(row.value.min, pos2(row.reset.x + 8.0, row.value.max.y));
        ui.painter().with_clip_rect(r.intersect(ui.clip_rect())).text(pos2(r.min.x, r.center().y), Align2::LEFT_CENTER, &value, Tokens::ui(12.0), t.text);
        app.auto.add(&format!("inspector.file.{key}"), r, &value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_points_are_pixels_from_the_centre_with_y_up() {
        let frame = (1920.0, 1080.0);
        assert_eq!(from_engine_point(Vec2::new(960.0, 540.0), frame), (0.0, 0.0));
        assert_eq!(from_engine_point(Vec2::new(f64::NAN, f64::NAN), frame), (0.0, 0.0), "auto is the centre");
        assert_eq!(from_engine_point(Vec2::new(1060.0, 440.0), frame), (100.0, 100.0), "right and up");
        assert_eq!(to_engine_point(100.0, 100.0, frame), [1060.0, 440.0]);
        assert_eq!(to_engine_point(0.0, 0.0, frame), [960.0, 540.0]);
        // hostile numbers stay numbers
        let (x, y) = from_engine_point(Vec2::new(f64::INFINITY, f64::NEG_INFINITY), frame);
        assert!(x.is_finite() && y.is_finite());
    }
}
