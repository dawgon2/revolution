//! Color page ▸ Magic Mask (fork GRADE-PLAN G3.3): the palette tab of the selected node (Add /
//! Remove, the stroke tool, Track Backward / Track / Track Forward / Stop with progress, Refine)
//! and the Program monitor's side of it (strokes drawn while the tool is armed, the matte
//! overlay). Every edit is a `grade.magicMask.*` command.
//!
//! Automation ids: `grade.magicMask.mode.object`, `grade.magicMask.mode.person`,
//! `grade.magicMask.add`, `grade.magicMask.remove`, `grade.magicMask.tool.add`,
//! `grade.magicMask.tool.subtract`, `grade.magicMask.clearFrame`, `grade.magicMask.clearAll`,
//! `grade.magicMask.trackBackward`, `grade.magicMask.track`, `grade.magicMask.trackForward`,
//! `grade.magicMask.cancel`, `grade.magicMask.status`, `grade.magicMask.overlay`,
//! `grade.magicMask.refine.{grow,soften,cleanBlack,cleanWhite}` (each with `.reset`) and
//! `grade.magicMask.refine.invert`, and the model selector (fork G3)
//! `grade.magicMask.model.{auto,better,faster,mobileSam}`.

use std::sync::Arc;

use egui::{Color32, Rect, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::magic_mask::{MaskModel, TrackState, TrackStatus};
use filmcraft_project::magic_mask::{MAX_GROW, MAX_SOFTEN, MagicMask, MagicMaskMode};
use filmcraft_project::{ClipId, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::grade::{FloatRow, RowEdit, float_row};
use crate::FilmcraftApp;
use crate::state::MagicMaskTool;

/// What the tab shows of the node's Magic Mask, gathered before drawing.
pub(crate) struct View {
    clip: ClipId,
    node: u64,
    mask: Option<MagicMask>,
    /// Strokes on the frame at the playhead.
    here: usize,
    /// The playhead is on the clip (strokes can be drawn).
    on_clip: bool,
    /// The newest track of this node.
    track: Option<TrackStatus>,
    tool: Option<MagicMaskTool>,
    overlay: bool,
    error: Option<String>,
    /// The session's model choice and what Auto picks now.
    model: MaskModel,
    auto: MaskModel,
}

/// What a click in the tab asks for (applied after the panel is drawn).
pub(crate) enum Act {
    Run(&'static str, Value),
    Tool(Option<bool>),
    Overlay(bool),
    Model(MaskModel),
}

/// Whether a track is still going.
fn live(st: &TrackStatus) -> bool {
    matches!(st.state, TrackState::Queued | TrackState::Running)
}

/// The clip time at the playhead, when the playhead is on the clip.
fn clip_time(app: &FilmcraftApp, it: &TrackItem) -> Option<Tick> {
    let ph = app.session.playhead();
    (ph >= it.start && ph < it.end()).then(|| it.effect_time_at(ph))
}

pub(crate) fn view(app: &FilmcraftApp, clip: ClipId, it: &TrackItem, node: &filmcraft_project::grade::GradeNode) -> View {
    let mt = clip_time(app, it);
    let mask = node.magic_mask.clone();
    let here = match (&mask, mt) {
        (Some(m), Some(t)) => m.strokes_at(t).count(),
        _ => 0,
    };
    let track = app.session.magic_mask.latest(clip, node.id);
    let error = app.ui.magic_mask.error.as_ref().filter(|(c, n, _)| *c == clip.0 && *n == node.id).map(|(_, _, e)| e.clone());
    View {
        clip,
        node: node.id,
        mask,
        here,
        on_clip: mt.is_some(),
        track,
        tool: app.ui.magic_mask.tool.filter(|t| t.clip == clip.0 && t.node == node.id),
        overlay: app.ui.magic_mask.overlay,
        error,
        model: app.session.magic_mask.model,
        auto: app.session.magic_mask.auto_choice(),
    }
}

/// A plain-language line for a track's state.
fn status_line(st: &TrackStatus) -> String {
    let n = |v: usize| v.to_string();
    match st.state {
        TrackState::Queued => tl!("Waiting to track…").to_string(),
        TrackState::Running => crate::i18n::fmt(tl!("Tracking… {done} of {frames} frames"), &[("done", &n(st.done)), ("frames", &n(st.frames))]),
        TrackState::Done => crate::i18n::fmt(
            tl!("Tracked {frames} frames ({segmented} by the model, {reused} already done)"),
            &[("frames", &n(st.done)), ("segmented", &n(st.segmented)), ("reused", &n(st.reused))],
        ),
        TrackState::Failed => st.error.clone().unwrap_or_else(|| tl!("Tracking failed").to_string()),
        TrackState::Cancelled => tl!("Tracking stopped").to_string(),
        TrackState::Superseded => tl!("The strokes changed while tracking: track again").to_string(),
    }
}

/// The selector's name of a model ("MobileSAM" is a name, never translated).
fn model_name(m: MaskModel) -> String {
    match m {
        MaskModel::Auto => tl!("Auto").to_string(),
        MaskModel::Better => tl!("Better").to_string(),
        MaskModel::Faster => tl!("Faster").to_string(),
        MaskModel::MobileSam => "MobileSAM".to_string(),
    }
}

/// The model selector (like Resolve Studio's Better / Faster): sets the session's model, which
/// every Track uses.
fn model_row(ui: &mut egui::Ui, v: &View, t: &crate::theme::Tokens, acts: &mut Vec<Act>, autos: &mut Vec<(String, Rect, String)>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(tl!("Model")).color(t.text_dim));
        for (m, id, tip) in [
            (MaskModel::Auto, "grade.magicMask.model.auto", tl!("Better on a discrete GPU when SAM 2.1 Large is installed, else MobileSAM")),
            (MaskModel::Better, "grade.magicMask.model.better", tl!("SAM 2.1 Large on the GPU: the most accurate, about 0.9 s a frame")),
            (MaskModel::Faster, "grade.magicMask.model.faster", tl!("SAM 2.1 Base+ on the GPU: a little less accurate, a little quicker")),
            (
                MaskModel::MobileSam,
                "grade.magicMask.model.mobileSam",
                tl!("MobileSAM, frame by frame: quickest and needs no discrete GPU, but weaker when similar objects touch"),
            ),
        ] {
            let label = if m == MaskModel::Auto { crate::i18n::fmt(tl!("Auto ({model})"), &[("model", &model_name(v.auto))]) } else { model_name(m) };
            let r = ui.selectable_label(v.model == m, label.as_str()).on_hover_text(tip);
            autos.push((id.into(), r.rect, label));
            if r.clicked() && v.model != m {
                acts.push(Act::Model(m));
            }
        }
    });
}

/// The Magic Mask tab.
pub(crate) fn controls(ui: &mut egui::Ui, v: &View, t: &crate::theme::Tokens, gest: u64, acts: &mut Vec<Act>, autos: &mut Vec<(String, Rect, String)>) {
    let base = json!({"clip": v.clip.0, "node": v.node});
    let with = |extra: Value| {
        let mut p = base.clone();
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            for (k, x) in e {
                o.insert(k.clone(), x.clone());
            }
        }
        p
    };
    let Some(mask) = &v.mask else {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(tl!(
                "Magic Mask finds the subject you mark with a few strokes and follows it through the shot, so this node only changes that."
            ))
            .color(t.text_dim),
        );
        ui.add_space(4.0);
        let person_id = egui::Id::new("magic-mask-person");
        let mut person: bool = ui.data(|d| d.get_temp(person_id)).unwrap_or(false);
        ui.horizontal(|ui| {
            for (p, id, label, tip) in [
                (false, "grade.magicMask.mode.object", tl!("Object"), tl!("Whatever the strokes cover")),
                (true, "grade.magicMask.mode.person", tl!("Person"), tl!("A whole person: the strokes pick which one")),
            ] {
                let r = ui.selectable_label(person == p, label).on_hover_text(tip);
                autos.push((id.into(), r.rect, label.to_string()));
                if r.clicked() {
                    person = p;
                }
            }
            let r = ui.button(tl!("Add Magic Mask"));
            autos.push(("grade.magicMask.add".into(), r.rect, tl!("Add Magic Mask").to_string()));
            if r.clicked() {
                acts.push(Act::Run("grade.magicMask.add", with(json!({"mode": if person { "person" } else { "object" }}))));
                // (the stroke tool comes ready, as in Resolve)
                acts.push(Act::Tool(Some(false)));
            }
        });
        ui.data_mut(|d| d.insert_temp(person_id, person));
        return;
    };
    // mode, Remove
    ui.horizontal(|ui| {
        let mode = match mask.mode {
            MagicMaskMode::Object => tl!("Object"),
            MagicMaskMode::Person => tl!("Person"),
        };
        ui.label(egui::RichText::new(tl!("Magic Mask")).color(t.text));
        ui.label(egui::RichText::new(mode).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.small_button(tl!("Remove")).on_hover_text(tl!("Remove the Magic Mask from this node"));
            autos.push(("grade.magicMask.remove".into(), r.rect, tl!("Remove Magic Mask").to_string()));
            if r.clicked() {
                acts.push(Act::Run("grade.magicMask.remove", base.clone()));
            }
        });
    });
    // the stroke tool and clearing strokes
    ui.horizontal_wrapped(|ui| {
        let armed = v.tool.map(|t| t.subtract);
        for (sub, id, label, tip) in [
            (
                false,
                "grade.magicMask.tool.add",
                tl!("Stroke +"),
                tl!("Click the subject in the Program picture, or drag a box round it (Alt-click: leave out)"),
            ),
            (true, "grade.magicMask.tool.subtract", tl!("Stroke −"), tl!("Click what the mask should leave out")),
        ] {
            let r = ui.selectable_label(armed == Some(sub), label).on_hover_text(tip);
            autos.push((id.into(), r.rect, label.to_string()));
            if r.clicked() {
                acts.push(Act::Tool(if armed == Some(sub) { None } else { Some(sub) }));
            }
        }
        let r =
            ui.add_enabled(v.here > 0 && v.on_clip, egui::Button::new(tl!("Clear Frame")).small()).on_hover_text(tl!("Remove the strokes drawn on this frame"));
        autos.push(("grade.magicMask.clearFrame".into(), r.rect, tl!("Clear Frame").to_string()));
        if r.clicked() {
            acts.push(Act::Run("grade.magicMask.clearStrokes", with(json!({"atPlayhead": true}))));
        }
        let r = ui.add_enabled(!mask.strokes.is_empty(), egui::Button::new(tl!("Clear All")).small()).on_hover_text(tl!("Remove every stroke of this mask"));
        autos.push(("grade.magicMask.clearAll".into(), r.rect, tl!("Clear All").to_string()));
        if r.clicked() {
            acts.push(Act::Run("grade.magicMask.clearStrokes", base.clone()));
        }
    });
    let strokes = crate::i18n::fmt(tl!("{n} strokes, {here} on this frame"), &[("n", &mask.strokes.len().to_string()), ("here", &v.here.to_string())]);
    ui.label(egui::RichText::new(strokes).color(t.text_faint));
    if v.tool.is_some() {
        ui.label(egui::RichText::new(tl!("click or drag on the Program picture · Esc ends")).color(t.text_faint));
    }
    ui.separator();
    model_row(ui, v, t, acts, autos);
    // tracking
    let running = v.track.as_ref().is_some_and(live);
    let can_track = !mask.strokes.is_empty() && !running;
    ui.horizontal_wrapped(|ui| {
        for (dir, id, label, tip) in [
            ("backward", "grade.magicMask.trackBackward", tl!("Track Backward"), tl!("Follow the subject from the strokes towards the start of the clip")),
            ("both", "grade.magicMask.track", tl!("Track"), tl!("Follow the subject both ways from the strokes")),
            ("forward", "grade.magicMask.trackForward", tl!("Track Forward"), tl!("Follow the subject from the strokes towards the end of the clip")),
        ] {
            let r = ui.add_enabled(can_track, egui::Button::new(label)).on_hover_text(tip);
            autos.push((id.into(), r.rect, label.to_string()));
            if r.clicked() {
                acts.push(Act::Run("grade.magicMask.track", with(json!({"direction": dir}))));
            }
        }
        let r = ui.add_enabled(running, egui::Button::new(tl!("Stop"))).on_hover_text(tl!("Stop tracking (the frames done so far are kept)"));
        autos.push(("grade.magicMask.cancel".into(), r.rect, tl!("Stop tracking").to_string()));
        if r.clicked()
            && let Some(st) = &v.track
        {
            acts.push(Act::Run("grade.magicMask.cancel", json!({"job": st.job})));
        }
    });
    if let Some(st) = &v.track {
        if running {
            let f = if st.frames == 0 { 0.0 } else { (st.done as f32 / st.frames as f32).clamp(0.0, 1.0) };
            let r = ui.add(egui::ProgressBar::new(f).desired_height(6.0));
            autos.push(("grade.magicMask.progress".into(), r.rect, tl!("Tracking progress").to_string()));
        }
        let col = if st.state == TrackState::Failed { t.render_red } else { t.text_dim };
        let mut line = status_line(st);
        if !st.used.is_empty() && st.state != TrackState::Failed {
            line = format!("{line} · {}", crate::i18n::fmt(tl!("Model: {model}"), &[("model", &st.used)]));
        }
        let r = ui.add(egui::Label::new(egui::RichText::new(line.as_str()).color(col)).wrap());
        autos.push(("grade.magicMask.status".into(), r.rect, line));
        for note in st.notes.iter().rev().take(3) {
            ui.add(egui::Label::new(egui::RichText::new(crate::panels::grade::short(note, 160)).color(t.text_faint)).wrap());
        }
    }
    if let Some(e) = &v.error {
        let r = ui.add(egui::Label::new(egui::RichText::new(crate::panels::grade::short(e, 300)).color(t.render_red)).wrap());
        if v.track.is_none() {
            autos.push(("grade.magicMask.status".into(), r.rect, e.clone()));
        }
    }
    let mut ov = v.overlay;
    let r = ui.checkbox(&mut ov, tl!("Show matte")).on_hover_text(tl!("Tint the tracked matte over the Program picture (frames not tracked yet show none)"));
    autos.push(("grade.magicMask.overlay".into(), r.rect, tl!("Show matte").to_string()));
    if r.changed() {
        acts.push(Act::Overlay(ov));
    }
    ui.separator();
    // Refine: applied to the tracked mattes, no new model run
    ui.label(egui::RichText::new(tl!("Refine")).color(t.text));
    let rf = mask.refine;
    let merge = |key: &str| format!("magic-mask-{}-{}-{key}#{gest}", v.clip.0, v.node);
    for (key, label, value, min, max, id, reset_id) in [
        ("grow", tl!("Grow / Shrink"), rf.grow, -MAX_GROW, MAX_GROW, "grade.magicMask.refine.grow", "grade.magicMask.refine.grow.reset"),
        ("soften", tl!("Soften"), rf.soften, 0.0, MAX_SOFTEN, "grade.magicMask.refine.soften", "grade.magicMask.refine.soften.reset"),
        ("cleanBlack", tl!("Clean Black"), rf.clean_black, 0.0, 1.0, "grade.magicMask.refine.cleanBlack", "grade.magicMask.refine.cleanBlack.reset"),
        ("cleanWhite", tl!("Clean White"), rf.clean_white, 0.0, 1.0, "grade.magicMask.refine.cleanWhite", "grade.magicMask.refine.cleanWhite.reset"),
    ] {
        let row = FloatRow { label, value, min, max, step: 0.0, suffix: "", enabled: true, animated: None, id, reset_id, anim_id: "" };
        match float_row(ui, t, row, autos) {
            Some(RowEdit::Set(x)) => acts.push(Act::Run("grade.magicMask.setRefine", with(json!({key: x, "merge": merge(key)})))),
            Some(RowEdit::Reset) => acts.push(Act::Run("grade.magicMask.setRefine", with(json!({key: 0.0})))),
            Some(RowEdit::ToggleAnimation) | None => {}
        }
    }
    let mut inv = rf.invert;
    let r = ui.checkbox(&mut inv, tl!("Invert"));
    autos.push(("grade.magicMask.refine.invert".into(), r.rect, tl!("Invert").to_string()));
    if r.changed() {
        acts.push(Act::Run("grade.magicMask.setRefine", with(json!({"invert": inv}))));
    }
}

/// Run what the tab asked for. `clip` / `node`: the node the tab showed.
pub(crate) fn apply(app: &mut FilmcraftApp, clip: ClipId, node: u64, acts: Vec<Act>) {
    for a in acts {
        match a {
            Act::Tool(sub) => set_tool(app, sub.map(|subtract| MagicMaskTool { clip: clip.0, node, subtract })),
            Act::Overlay(on) => app.ui.magic_mask.overlay = on,
            Act::Model(m) => app.session.magic_mask.model = m,
            Act::Run(cmd, mut p) => {
                if p.get("atPlayhead").is_some() {
                    if let Some(o) = p.as_object_mut() {
                        o.remove("atPlayhead");
                    }
                    p["time"] = json!(app.session.playhead().0);
                }
                match app.session.execute(cmd, p) {
                    Ok(_) => {
                        if cmd == "grade.magicMask.track" || cmd == "grade.magicMask.remove" {
                            app.ui.magic_mask.error = None;
                        }
                        if cmd == "grade.magicMask.remove" {
                            set_tool(app, None);
                        }
                    }
                    Err(e) => fail(app, clip.0, node, e.to_string()),
                }
            }
        }
    }
}

/// A command failed: the status line and the tab say why.
fn fail(app: &mut FilmcraftApp, clip: u64, node: u64, e: String) {
    app.ui.status = e.clone();
    app.ui.magic_mask.error = Some((clip, node, e));
}

/// Arm (or disarm) the stroke tool; one armed picker at a time.
pub(crate) fn set_tool(app: &mut FilmcraftApp, tool: Option<MagicMaskTool>) {
    app.ui.magic_mask.tool = tool;
    if tool.is_some() {
        app.ui.grade_picker = None;
        app.ui.wb_picker = None;
        app.ui.eyedropper = None;
        app.ui.qual3d_tool = None;
    }
}

/// After a pass of the panel with the Magic Mask tab: leaving the tab or selecting another node
/// puts the stroke tool down.
pub(crate) fn tool_state(app: &mut FilmcraftApp, clip: ClipId, node: u64, tab: &str) {
    if let Some(tool) = app.ui.magic_mask.tool
        && (tab != "magicMask" || tool.clip != clip.0 || tool.node != node)
    {
        app.ui.magic_mask.tool = None;
    }
}

/// The clip and node the monitor shows strokes / the matte of: the armed tool's, else (with the
/// matte overlay on) the selected clip's current grade node.
fn target(app: &FilmcraftApp) -> Option<(ClipId, u64, bool)> {
    if let Some(t) = app.ui.magic_mask.tool {
        return Some((ClipId(t.clip), t.node, true));
    }
    if !app.ui.magic_mask.overlay {
        return None;
    }
    let sel = app.session.state.grade_node?;
    app.session.state.selection.contains(&sel.clip).then_some((sel.clip, sel.node, false))
}

/// The frame position (0…1) of a point of the picture.
fn uv(pic: Rect, pos: egui::Pos2) -> [f32; 2] {
    [((pos.x - pic.min.x) / pic.width()).clamp(0.0, 1.0), ((pos.y - pic.min.y) / pic.height()).clamp(0.0, 1.0)]
}

/// The stroke tool on the Program picture: a click adds a point (Alt, or the Stroke − tool: a
/// point to leave out), a drag a box round the subject. Esc puts the tool down.
pub(crate) fn monitor_tool(app: &mut FilmcraftApp, ui: &mut egui::Ui, resp: &egui::Response, pic: Rect) {
    let Some(tool) = app.ui.magic_mask.tool else { return };
    let ctx = ui.ctx().clone();
    let start_id = egui::Id::new("magic-mask-drag-start");
    if !app.session.state.selection.iter().any(|c| c.0 == tool.clip) || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.magic_mask.tool = None;
        ctx.data_mut(|d| d.remove::<egui::Pos2>(start_id));
        return;
    }
    if resp.hovered() || resp.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if pic.width() <= 0.0 || pic.height() <= 0.0 {
        return;
    }
    let base = json!({"clip": tool.clip, "node": tool.node});
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        ctx.data_mut(|d| d.insert_temp(start_id, pos));
    }
    let start: Option<egui::Pos2> = ctx.data(|d| d.get_temp(start_id));
    if let (Some(a), Some(b)) = (start, resp.interact_pointer_pos())
        && resp.dragged()
    {
        ui.painter().rect_stroke(Rect::from_two_pos(a, b), 0.0, Stroke::new(1.5, Color32::from_rgb(0x50, 0xe0, 0x70)), StrokeKind::Middle);
    }
    let mut stroke: Option<Value> = None;
    if resp.drag_stopped() {
        ctx.data_mut(|d| d.remove::<egui::Pos2>(start_id));
        if let (Some(a), Some(b)) = (start, resp.interact_pointer_pos()) {
            let (p, q) = (uv(pic, a), uv(pic, b));
            if (p[0] - q[0]).abs() * pic.width() >= 4.0 && (p[1] - q[1]).abs() * pic.height() >= 4.0 {
                let mut s = base.clone();
                s["rect"] = json!([p[0].min(q[0]), p[1].min(q[1]), p[0].max(q[0]), p[1].max(q[1])]);
                stroke = Some(s);
            }
        }
    } else if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let positive = !(tool.subtract || ctx.input(|i| i.modifiers.alt));
        let p = uv(pic, pos);
        let mut s = base.clone();
        s["points"] = json!([{"x": p[0], "y": p[1], "positive": positive}]);
        stroke = Some(s);
    }
    if let Some(s) = stroke {
        match app.session.execute("grade.magicMask.stroke", s) {
            Ok(_) => {
                app.ui.magic_mask.error = None;
            }
            Err(e) => fail(app, tool.clip, tool.node, e.to_string()),
        }
    }
}

/// The matte texture of the last frame shown, by the matte's identity.
#[derive(Clone)]
struct MatteTex {
    ptr: usize,
    tex: egui::TextureHandle,
}

/// Most matte pixels turned into an overlay texture (the cache keeps them at most 1024 wide).
const MAX_OVERLAY_PIXELS: usize = 4096 * 4096;

/// The overlay texture of a matte (`ptr`: the matte's identity; `alpha`: `w` x `h` values).
fn matte_texture(ctx: &egui::Context, ptr: usize, w: usize, h: usize, alpha: &[u8]) -> Option<egui::TextureHandle> {
    let id = egui::Id::new("magic-mask-overlay");
    if let Some(c) = ctx.data(|d| d.get_temp::<MatteTex>(id))
        && c.ptr == ptr
    {
        return Some(c.tex);
    }
    let n = w.checked_mul(h)?;
    if n == 0 || n > MAX_OVERLAY_PIXELS || alpha.len() != n {
        return None;
    }
    let mut px = Vec::with_capacity(n.checked_mul(4)?);
    for a in alpha {
        let a = (u16::from(*a) * 140 / 255) as u8;
        px.extend_from_slice(&[255, 70, 60, a]);
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &px);
    let tex = ctx.load_texture("magic-mask-overlay", img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, MatteTex { ptr, tex: tex.clone() }));
    Some(tex)
}

/// The strokes of the frame at the playhead (while the tool is armed) and the tracked matte (with
/// Show matte on), over the Program picture.
pub(crate) fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect) {
    let Some((clip, node, armed)) = target(app) else { return };
    let Some(seq) = app.session.active_sequence() else { return };
    let Some((_, it)) = seq.find_item(clip) else { return };
    let Some(mask) = it.grade.as_ref().and_then(|g| g.node(node)).and_then(|n| n.magic_mask.clone()) else { return };
    let Some(mt) = clip_time(app, it) else { return };
    if app.ui.magic_mask.overlay
        && let Some(m) = filmcraft_engine::magic_mask::magic_matte(&app.session, clip, node, mt)
        && let Some(tex) = matte_texture(ui.ctx(), Arc::as_ptr(&m) as usize, m.width, m.height, &m.alpha)
    {
        ui.painter().image(tex.id(), pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    if !armed {
        return;
    }
    let at = |x: f32, y: f32| pos2(pic.min.x + x * pic.width(), pic.min.y + y * pic.height());
    let (plus, minus) = (Color32::from_rgb(0x50, 0xe0, 0x70), Color32::from_rgb(0xf0, 0x50, 0x50));
    let p = ui.painter();
    for s in mask.strokes_at(mt) {
        if let Some(r) = s.rect {
            p.rect_stroke(Rect::from_two_pos(at(r[0], r[1]), at(r[2], r[3])), 0.0, Stroke::new(1.5, plus), StrokeKind::Middle);
        }
        let pts: Vec<egui::Pos2> = s.points.iter().map(|q| at(q.x, q.y)).collect();
        if pts.len() > 1 {
            let col = if s.points.first().is_some_and(|q| q.positive) { plus } else { minus };
            p.add(egui::Shape::line(pts.clone(), Stroke::new(2.0, col)));
        }
        for (q, c) in s.points.iter().zip(&pts) {
            let col = if q.positive { plus } else { minus };
            p.circle_filled(*c, 5.0, col);
            p.circle_stroke(*c, 5.0, Stroke::new(1.0, Color32::BLACK));
            let sign = if q.positive { "+" } else { "−" };
            p.text(*c + vec2(8.0, -8.0), egui::Align2::LEFT_BOTTOM, sign, egui::FontId::proportional(12.0), col);
        }
    }
}
