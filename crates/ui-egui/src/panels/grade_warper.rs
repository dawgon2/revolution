//! Color page ▸ Warper (fork GRADE-PLAN G4.1): the colour warper of the selected node, after
//! Resolve's Color Warper. A hue-sat disc (hue round the rim as on the colour wheels, saturation
//! out from the centre) or a chroma-luma plane (saturation across, luma up) with the mesh's
//! points; drag a point to warp the colours under it, right-click a point (or Pin) to pin it.
//! Every edit is a `grade.warper.*` command; a drag is one undo step (`merge` per press).
//!
//! Automation ids: `grade.warper.mode.hueSat`, `grade.warper.mode.chromaLuma`,
//! `grade.warper.plane`, `grade.warper.point.<index>` (each mesh point), `grade.warper.pin`,
//! `grade.warper.resetPoint`, `grade.warper.resetMesh`, `grade.warper.remove`,
//! `grade.warper.strength` / `grade.warper.range` (each with `.reset` and `.anim`, the keyframe
//! toggle: fork LO.3),
//! `grade.warper.preserveLuma`, `grade.warper.cols`, `grade.warper.rows`.

use egui::{Color32, Rect, Sense, Shape, Stroke, pos2, vec2};
use filmcraft_project::grade::GradeNode;
use filmcraft_project::warper::{CL_SIDE, HUE_COLUMNS, MAX_RANGE, SAT_RINGS, WarpMesh, WarpMode, Warper};
use serde_json::{Value, json};

use super::grade::{FloatRow, RowEdit, float_row};

/// Largest side of the plane (points).
const PLANE: f32 = 260.0;
/// How near the pointer must be to grab a point (points).
const GRAB: f32 = 10.0;

fn mode_key(m: WarpMode) -> &'static str {
    match m {
        WarpMode::HueSat => "hueSat",
        WarpMode::ChromaLuma => "chromaLuma",
    }
}

/// A plane position (u, v) on screen.
fn to_screen(mode: WarpMode, r: Rect, uv: [f32; 2]) -> egui::Pos2 {
    match mode {
        WarpMode::HueSat => {
            let a = uv[0] * std::f32::consts::TAU;
            let rad = r.width().min(r.height()) / 2.0 * uv[1];
            r.center() + vec2(a.cos(), a.sin()) * rad
        }
        WarpMode::ChromaLuma => pos2(r.left() + uv[0] * r.width(), r.bottom() - uv[1] * r.height()),
    }
}

/// A screen position as plane coordinates (clamped onto the plane).
pub(crate) fn from_screen(mode: WarpMode, r: Rect, p: egui::Pos2) -> [f32; 2] {
    match mode {
        WarpMode::HueSat => {
            let d = p - r.center();
            let rad = (r.width().min(r.height()) / 2.0).max(1.0);
            let h = d.y.atan2(d.x) / std::f32::consts::TAU;
            [h - h.floor(), (d.length() / rad).clamp(0.0, 1.0)]
        }
        WarpMode::ChromaLuma => {
            let (w, h) = (r.width().max(1.0), r.height().max(1.0));
            [((p.x - r.left()) / w).clamp(0.0, 1.0), ((r.bottom() - p.y) / h).clamp(0.0, 1.0)]
        }
    }
}

/// A display colour for the plane's background.
fn swatch(mode: WarpMode, uv: [f32; 2]) -> Color32 {
    let rgb = match mode {
        WarpMode::HueSat => filmcraft_color::warp::hsv_to_rgb(uv[0], uv[1], 0.85),
        // (an orange hue, saturation across, brightness up)
        WarpMode::ChromaLuma => filmcraft_color::warp::hsv_to_rgb(0.07, uv[0], 0.15 + 0.8 * uv[1]),
    };
    let c = rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8);
    Color32::from_rgb(c[0], c[1], c[2])
}

/// The plane's background as a mesh of coloured cells.
fn background(mode: WarpMode, r: Rect) -> egui::Mesh {
    let mut m = egui::Mesh::default();
    let (nu, nv) = (48u32, 12u32);
    for j in 0..=nv {
        for i in 0..=nu {
            let uv = [i as f32 / nu as f32, j as f32 / nv as f32];
            m.colored_vertex(to_screen(mode, r, uv), swatch(mode, uv));
        }
    }
    let row = nu + 1;
    for j in 0..nv {
        for i in 0..nu {
            let (a, b, c, d) = (j * row + i, j * row + i + 1, (j + 1) * row + i, (j + 1) * row + i + 1);
            m.add_triangle(a, b, d);
            m.add_triangle(a, d, c);
        }
    }
    m
}

/// The point (index) of `mesh` nearest to `p` within [`GRAB`].
fn nearest(mode: WarpMode, mesh: &WarpMesh, r: Rect, p: egui::Pos2) -> Option<u32> {
    (0..mesh.len())
        .filter_map(|i| Some((i, to_screen(mode, r, mesh.position(mode, i)?).distance(p))))
        .filter(|(_, d)| *d <= GRAB)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The Warper tab of the node `node`: commands go to `cmds` (the panel adds the clip).
pub(crate) fn controls(
    ui: &mut egui::Ui,
    node: &GradeNode,
    mt: filmcraft_time::Tick,
    t: &crate::theme::Tokens,
    gest: u64,
    cmds: &mut Vec<(&'static str, Value)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let base = json!({"node": node.id});
    let with = |extra: Value| {
        let mut p = base.clone();
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            for (k, x) in e {
                o.insert(k.clone(), x.clone());
            }
        }
        p
    };
    let w = node.warper.clone().unwrap_or_default();
    let mode = w.mode;
    let mesh = w.mesh(mode).clone();
    let key = mode_key(mode);
    let sel_id = egui::Id::new(("grade-warper-sel", node.id, key));
    let drag_id = egui::Id::new(("grade-warper-drag", node.id, key));
    let mut sel: Option<u32> = ui.data(|d| d.get_temp::<u32>(sel_id)).filter(|i| *i < mesh.len());

    // which mesh, and Remove
    ui.horizontal(|ui| {
        for (m, label, tip) in [
            (WarpMode::HueSat, tl!("Hue-Sat"), tl!("Move colours round the hue circle and in or out in saturation")),
            (WarpMode::ChromaLuma, tl!("Chroma-Luma"), tl!("Change the saturation and brightness of colours by how saturated and bright they are")),
        ] {
            let r = ui.selectable_label(mode == m, label).on_hover_text(tip);
            autos.push((format!("grade.warper.mode.{}", mode_key(m)), r.rect, label.to_string()));
            if r.clicked() && mode != m {
                cmds.push(("grade.warper.set", with(json!({"mode": mode_key(m)}))));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r =
                ui.add_enabled(node.warper.is_some(), egui::Button::new(tl!("Remove")).small()).on_hover_text(tl!("Remove the colour warper from this node"));
            autos.push(("grade.warper.remove".into(), r.rect, tl!("Remove Colour Warper").to_string()));
            if r.clicked() {
                cmds.push(("grade.warper.reset", base.clone()));
            }
        });
    });
    ui.add_space(4.0);

    // the plane
    let avail = ui.available_width();
    let side = if avail.is_finite() { avail.clamp(80.0, PLANE) } else { PLANE };
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let plane = rect.shrink(8.0);
    autos.push(("grade.warper.plane".into(), rect, tl!("Colour warper").to_string()));
    let painter = ui.painter_at(rect);
    painter.add(Shape::mesh(background(mode, plane)));
    let line = Stroke::new(1.0, Color32::from_black_alpha(150));
    let pos = |i: u32| mesh.position(mode, i).map(|uv| to_screen(mode, plane, uv));
    let (cols, rows) = (mesh.cols, mesh.rows);
    for j in 0..rows {
        for i in 0..cols {
            let k = j * cols + i;
            let Some(p) = pos(k) else { continue };
            // the next point across (round the ring for hue-sat) and down
            let across = if i + 1 < cols {
                Some(k + 1)
            } else if mode == WarpMode::HueSat {
                Some(j * cols)
            } else {
                None
            };
            if let Some(q) = across.and_then(pos) {
                painter.line_segment([p, q], line);
            }
            let inner = if j > 0 {
                pos(k - cols)
            } else if mode == WarpMode::HueSat {
                Some(plane.center())
            } else {
                None
            };
            if let Some(q) = inner {
                painter.line_segment([q, p], line);
            }
        }
    }
    for i in 0..mesh.len() {
        let Some(p) = pos(i) else { continue };
        let moved = mesh.points.iter().any(|q| q.index == i && Some(q.to) != mesh.original(mode, i));
        let pinned = mesh.is_pinned(i);
        let fill = if moved { t.accent } else { Color32::WHITE };
        if pinned {
            painter.rect_filled(Rect::from_center_size(p, vec2(8.0, 8.0)), 1.0, Color32::from_rgb(220, 60, 60));
        }
        painter.circle(p, if sel == Some(i) { 5.0 } else { 3.5 }, fill, Stroke::new(1.0, Color32::BLACK));
        autos.push((format!("grade.warper.point.{i}"), Rect::from_center_size(p, vec2(12.0, 12.0)), format!("{} {i}", tl!("Point"))));
    }
    // dragging a point
    let mut drag: Option<u32> = ui.data(|d| d.get_temp::<u32>(drag_id));
    if resp.drag_started()
        && let Some(p) = resp.interact_pointer_pos()
    {
        drag = nearest(mode, &mesh, plane, p).filter(|i| !mesh.is_pinned(*i));
        if drag.is_some() {
            sel = drag;
        }
    }
    if let (Some(i), Some(p)) = (drag, resp.interact_pointer_pos())
        && resp.dragged()
    {
        let to = from_screen(mode, plane, p);
        if mesh.position(mode, i) != Some(to) {
            cmds.push(("grade.warper.movePoint", with(json!({"mesh": key, "index": i, "to": to, "merge": format!("warper:{}:{gest}", node.id)}))));
        }
    }
    if resp.drag_stopped() {
        drag = None;
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        sel = nearest(mode, &mesh, plane, p);
    }
    if resp.secondary_clicked()
        && let Some(i) = resp.interact_pointer_pos().and_then(|p| nearest(mode, &mesh, plane, p))
    {
        sel = Some(i);
        cmds.push(("grade.warper.pin", with(json!({"mesh": key, "index": i}))));
    }
    ui.data_mut(|d| {
        match sel {
            Some(i) => {
                d.insert_temp(sel_id, i);
            }
            None => d.remove::<u32>(sel_id),
        }
        match drag {
            Some(i) => {
                d.insert_temp(drag_id, i);
            }
            None => d.remove::<u32>(drag_id),
        }
    });

    // the selected point
    ui.horizontal(|ui| {
        let pinned = sel.is_some_and(|i| mesh.is_pinned(i));
        let r =
            ui.add_enabled(sel.is_some(), egui::Button::selectable(pinned, tl!("Pin"))).on_hover_text(tl!("A pinned point stays put when its neighbours move"));
        autos.push(("grade.warper.pin".into(), r.rect, tl!("Pin").to_string()));
        if r.clicked()
            && let Some(i) = sel
        {
            cmds.push(("grade.warper.pin", with(json!({"mesh": key, "index": i}))));
        }
        let r = ui.add_enabled(sel.is_some() && node.warper.is_some(), egui::Button::new(tl!("Reset Point")));
        autos.push(("grade.warper.resetPoint".into(), r.rect, tl!("Reset Point").to_string()));
        if r.clicked()
            && let Some(i) = sel
        {
            cmds.push(("grade.warper.reset", with(json!({"mesh": key, "index": i}))));
        }
        let r = ui.add_enabled(node.warper.is_some(), egui::Button::new(tl!("Reset Mesh")));
        autos.push(("grade.warper.resetMesh".into(), r.rect, tl!("Reset Mesh").to_string()));
        if r.clicked() {
            cmds.push(("grade.warper.reset", with(json!({"mesh": key}))));
        }
        if let Some(i) = sel
            && let Some(uv) = mesh.position(mode, i)
        {
            let (a, b) = match mode {
                WarpMode::HueSat => (tl!("Hue"), tl!("Sat")),
                WarpMode::ChromaLuma => (tl!("Sat"), tl!("Luma")),
            };
            ui.label(
                egui::RichText::new(format!("{a} {:.0}  {b} {:.0}", uv[0] * if mode == WarpMode::HueSat { 360.0 } else { 100.0 }, uv[1] * 100.0))
                    .color(t.text_dim),
            );
        }
    });
    ui.add_space(4.0);

    // strength, range, luma
    let merge = |k: &str| format!("warper:{}:{k}#{gest}", node.id);
    let def = Warper::default();
    // (each keyframes like a node parameter, `warp.<key>`, once the node has a warper: fork LO.3)
    for (k, param, label, value, max, id, reset_id, reset_to) in [
        ("strength", "warp.strength", tl!("Strength"), w.effective_strength(), 1.0, "grade.warper.strength", "grade.warper.strength.reset", def.strength),
        ("range", "warp.range", tl!("Falloff range"), w.effective_range(), MAX_RANGE, "grade.warper.range", "grade.warper.range.reset", def.range),
    ] {
        let anim_id = if k == "strength" { "grade.warper.strength.anim" } else { "grade.warper.range.anim" };
        let animated = node.warper.is_some().then(|| node.param(param).is_some_and(|p| p.is_animated()));
        let value = node.keyed_at(param, value, mt);
        let row = FloatRow { label, value, min: 0.0, max, step: 0.0, suffix: "", enabled: true, animated, id, reset_id, anim_id };
        match float_row(ui, t, row, autos) {
            Some(RowEdit::Set(x)) => cmds.push(("grade.warper.set", with(json!({k: x, "merge": merge(k)})))),
            Some(RowEdit::Reset) => cmds.push(("grade.warper.set", with(json!({k: reset_to})))),
            Some(RowEdit::ToggleAnimation) => cmds.push(("grade.toggleAnimation", with(json!({"param": param})))),
            None => {}
        }
    }
    let mut keep = w.preserve_luma;
    let r = ui.checkbox(&mut keep, tl!("Preserve luma")).on_hover_text(tl!("Hue-Sat moves keep each colour's brightness"));
    autos.push(("grade.warper.preserveLuma".into(), r.rect, tl!("Preserve luma").to_string()));
    if r.changed() {
        cmds.push(("grade.warper.set", with(json!({"preserveLuma": keep}))));
    }
    // the grid size (a new size starts the mesh afresh)
    ui.horizontal(|ui| {
        let (cr, rr, ck, rk, cl, rl) = match mode {
            WarpMode::HueSat => (HUE_COLUMNS, SAT_RINGS, "hueColumns", "satRings", tl!("Hue columns"), tl!("Saturation rings")),
            WarpMode::ChromaLuma => (CL_SIDE, CL_SIDE, "chromaColumns", "lumaRows", tl!("Chroma columns"), tl!("Luma rows")),
        };
        for (k, label, now, range, id) in [(ck, cl, mesh.cols, cr, "grade.warper.cols"), (rk, rl, mesh.rows, rr, "grade.warper.rows")] {
            ui.label(egui::RichText::new(label).color(t.text_dim));
            let mut v = now;
            let r = ui.add(egui::DragValue::new(&mut v).range(range)).on_hover_text(tl!("A new grid size starts this mesh afresh"));
            autos.push((id.into(), r.rect, label.to_string()));
            if r.changed() && v != now {
                cmds.push(("grade.warper.set", with(json!({k: v}))));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_and_plane_round_trip() {
        let r = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 200.0));
        for mode in [WarpMode::HueSat, WarpMode::ChromaLuma] {
            for uv in [[0.1f32, 0.5f32], [0.6, 0.9], [0.95, 0.25]] {
                let back = from_screen(mode, r, to_screen(mode, r, uv));
                assert!((back[0] - uv[0]).abs() < 1e-4 && (back[1] - uv[1]).abs() < 1e-4, "{mode:?} {uv:?} → {back:?}");
            }
        }
        // off the plane: clamped
        assert_eq!(from_screen(WarpMode::ChromaLuma, r, pos2(-100.0, 900.0)), [0.0, 0.0]);
        assert_eq!(from_screen(WarpMode::HueSat, r, pos2(110.0, 120.0))[1], 0.0);
    }
}
