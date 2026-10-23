//! Color page ▸ Qualifier ▸ 3D (fork GRADE-PLAN G4.3), after Resolve's 3D qualifier: a switch
//! between the HSL ranges and the 3D key, the Include / Exclude stroke tools (drag over colours
//! in the Program picture: `grade.qual3d.addStroke`), Softness and Despill (`grade.qual3d.set`)
//! and clearing (`grade.qual3d.clear`). The node's other qualifier settings (on, invert, view,
//! matte blur) stay on the Qualifier tab as for the HSL key.
//!
//! Automation ids: `grade.qual3d.mode.hsl`, `grade.qual3d.mode.3d`, `grade.qual3d.include`,
//! `grade.qual3d.exclude`, `grade.qual3d.softness` / `grade.qual3d.despill` (each with `.reset`),
//! `grade.qual3d.clear.include`, `grade.qual3d.clear.exclude`, `grade.qual3d.clear`.

use egui::{Color32, Rect, Stroke};
use filmcraft_project::ClipId;
use filmcraft_project::grade::GradeNode;
use filmcraft_project::qual3d::{DEFAULT_SOFTNESS, SOFTNESS};
use serde_json::{Value, json};

use super::grade::{FloatRow, RowEdit, float_row};
use crate::FilmcraftApp;
use crate::state::Qual3dTool;

/// Most stroke points sent for one drag (the engine thins longer strokes too).
const MAX_DRAG_POINTS: usize = 1024;

/// The HSL / 3D switch.
pub(crate) fn mode_row(
    ui: &mut egui::Ui,
    node: &GradeNode,
    t: &crate::theme::Tokens,
    cmds: &mut Vec<(&'static str, Value)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let three_d = node.qual3d_active().is_some();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Key")).color(t.text_dim));
        for (on, id, mode, label, tip) in [
            (false, "grade.qual3d.mode.hsl", "hsl", tl!("HSL"), tl!("Select by hue, saturation and luminance ranges")),
            (true, "grade.qual3d.mode.3d", "3d", tl!("3D"), tl!("Select the colours you paint strokes over in the Program picture")),
        ] {
            let r = ui.selectable_label(three_d == on, label).on_hover_text(tip);
            autos.push((id.into(), r.rect, label.to_string()));
            if r.clicked() && three_d != on {
                cmds.push(("grade.qual3d.set", json!({"node": node.id, "mode": mode})));
            }
        }
    });
}

/// The 3D key's controls (shown with the 3D key on): the stroke tools, Softness, Despill, clear.
/// `armed`: the armed tool on this node (`Some(exclude)`); a click on a tool goes to `arm`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn controls(
    ui: &mut egui::Ui,
    node: &GradeNode,
    t: &crate::theme::Tokens,
    gest: u64,
    armed: Option<bool>,
    arm: &mut Option<bool>,
    cmds: &mut Vec<(&'static str, Value)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let Some(k) = node.qual3d.as_ref() else { return };
    let with = |mut v: Value| {
        v["node"] = json!(node.id);
        v
    };
    ui.horizontal(|ui| {
        for (exclude, id, label, tip) in [
            (false, "grade.qual3d.include", tl!("Include"), tl!("Drag over colours in the Program picture to select them (Alt: exclude)")),
            (true, "grade.qual3d.exclude", tl!("Exclude"), tl!("Drag over colours in the Program picture to leave them out")),
        ] {
            let r = ui.selectable_label(armed == Some(exclude), label).on_hover_text(tip);
            autos.push((id.into(), r.rect, label.to_string()));
            if r.clicked() {
                *arm = Some(exclude);
            }
        }
        if armed.is_some() {
            ui.label(egui::RichText::new(tl!("drag over the Program picture · Esc cancels")).color(t.text_faint));
        }
    });
    ui.label(egui::RichText::new(format!("{} {} · {} {}", k.include.len(), tl!("colours picked"), k.exclude.len(), tl!("left out"))).color(t.text_dim));
    let merge = |key: &str| format!("qual3d:{}:{key}#{gest}", node.id);
    for (key, label, value, lo, hi, id, reset_id, reset_to) in [
        (
            "softness",
            tl!("Softness"),
            k.effective_softness(),
            *SOFTNESS.start(),
            *SOFTNESS.end(),
            "grade.qual3d.softness",
            "grade.qual3d.softness.reset",
            DEFAULT_SOFTNESS,
        ),
        ("despill", tl!("Despill"), k.effective_despill(), 0.0, 1.0, "grade.qual3d.despill", "grade.qual3d.despill.reset", 0.0),
    ] {
        let row = FloatRow { label, value, min: lo, max: hi, step: 0.0, suffix: "", enabled: true, animated: None, id, reset_id, anim_id: "" };
        let edit = float_row(ui, t, row, autos);
        if key == "despill"
            && let Some((_, r, _)) = autos.iter().find(|(a, ..)| a == id)
        {
            ui.interact(*r, egui::Id::new(("qual3d-despill-tip", node.id)), egui::Sense::hover())
                .on_hover_text(tl!("Pulls the picked colour's tint out of what the node corrects; use with Invert for green or blue screen spill"));
        }
        match edit {
            Some(RowEdit::Set(x)) => cmds.push(("grade.qual3d.set", with(json!({key: x, "merge": merge(key)})))),
            Some(RowEdit::Reset) => cmds.push(("grade.qual3d.set", with(json!({key: reset_to})))),
            Some(RowEdit::ToggleAnimation) | None => {}
        }
    }
    ui.horizontal(|ui| {
        for (which, id, label, enabled) in [
            ("include", "grade.qual3d.clear.include", tl!("Clear Include"), !k.include.is_empty()),
            ("exclude", "grade.qual3d.clear.exclude", tl!("Clear Exclude"), !k.exclude.is_empty()),
            ("all", "grade.qual3d.clear", tl!("Clear All"), true),
        ] {
            let r = ui.add_enabled(enabled, egui::Button::new(label).small());
            autos.push((id.into(), r.rect, label.to_string()));
            if r.clicked() {
                cmds.push(("grade.qual3d.clear", with(json!({"which": which}))));
            }
        }
    });
}

/// Arm (or disarm) the stroke tool; one armed picker at a time.
pub(crate) fn set_tool(app: &mut FilmcraftApp, tool: Option<Qual3dTool>) {
    app.ui.qual3d_tool = tool;
    if tool.is_some() {
        app.ui.grade_picker = None;
        app.ui.wb_picker = None;
        app.ui.eyedropper = None;
        app.ui.magic_mask.tool = None;
    }
}

/// After a pass of the panel with the Qualifier tab: a click on a tool (`arm`) toggles it;
/// leaving the tab, another node or the HSL key puts it down.
pub(crate) fn tool_state(app: &mut FilmcraftApp, clip: ClipId, node: &GradeNode, tab: &str, arm: Option<bool>) {
    let armed = app.ui.qual3d_tool.filter(|a| a.clip == clip.0 && a.node == node.id).map(|a| a.exclude);
    match arm {
        Some(ex) if armed == Some(ex) => set_tool(app, None),
        Some(exclude) => set_tool(app, Some(Qual3dTool { clip: clip.0, node: node.id, exclude })),
        None if tab != "qualifier" || armed.is_none() || node.qual3d_active().is_none() => app.ui.qual3d_tool = None,
        None => {}
    }
}

/// The frame position (0…1) of a point of the picture.
fn uv(pic: Rect, pos: egui::Pos2) -> [f32; 2] {
    [((pos.x - pic.min.x) / pic.width()).clamp(0.0, 1.0), ((pos.y - pic.min.y) / pic.height()).clamp(0.0, 1.0)]
}

/// The stroke tool on the Program picture: a drag paints a stroke (Alt, or the Exclude tool: over
/// colours to leave out), a click picks one spot. Esc puts the tool down.
pub(crate) fn monitor_tool(app: &mut FilmcraftApp, ui: &mut egui::Ui, resp: &egui::Response, pic: Rect) {
    let Some(tool) = app.ui.qual3d_tool else { return };
    let ctx = ui.ctx().clone();
    let path_id = egui::Id::new("qual3d-stroke-path");
    if !app.session.state.selection.iter().any(|c| c.0 == tool.clip) || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.qual3d_tool = None;
        ctx.data_mut(|d| d.remove::<Vec<egui::Pos2>>(path_id));
        return;
    }
    if resp.hovered() || resp.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if pic.width() <= 0.0 || pic.height() <= 0.0 {
        return;
    }
    let exclude = tool.exclude || ctx.input(|i| i.modifiers.alt);
    let colour = if exclude { Color32::from_rgb(0xf0, 0x60, 0x50) } else { Color32::from_rgb(0x50, 0xe0, 0x70) };
    let mut path: Vec<egui::Pos2> = ctx.data(|d| d.get_temp(path_id)).unwrap_or_default();
    if resp.drag_started() {
        path.clear();
    }
    if resp.dragged()
        && let Some(pos) = resp.interact_pointer_pos()
        && path.last().is_none_or(|l| l.distance(pos) >= 1.5)
        && path.len() < MAX_DRAG_POINTS
    {
        path.push(pos);
    }
    if path.len() >= 2 {
        ui.painter().add(egui::Shape::line(path.clone(), Stroke::new(2.0, colour)));
    }
    let mut stroke: Option<Vec<[f32; 2]>> = None;
    if resp.drag_stopped() {
        if !path.is_empty() {
            stroke = Some(path.iter().map(|p| uv(pic, *p)).collect());
        }
        path.clear();
    } else if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        stroke = Some(vec![uv(pic, pos)]);
    }
    ctx.data_mut(|d| d.insert_temp(path_id, path));
    if let Some(pts) = stroke {
        let p = json!({"clip": tool.clip, "node": tool.node, "points": pts, "exclude": exclude});
        if let Err(e) = app.session.execute("grade.qual3d.addStroke", p) {
            app.ui.status = e.to_string();
        }
    }
}
