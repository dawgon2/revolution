//! The HDR tab of the grade palette (fork G4.2, Resolve-style HDR zone wheels): a graph of where
//! each tonal zone acts, then one wheel per zone (Global, Black, Dark, Shadow, Light, Highlight,
//! Specular) with its exposure and saturation and, for the six zones, the zone's range (its edge,
//! stops from mid grey) and falloff. The wheel's puck is the zone's colour balance.
//!
//! Every edit is `grade.setParam` on the node's `hdr.<zone>.<what>` keys (a drag is one undo
//! step, as on the Primaries tab); the resets are `grade.hdr.reset`.
//!
//! Automation ids: `grade.hdr.reset` (every wheel), `grade.hdr.graph`, `grade.hdr.wheel.<zone>`,
//! `grade.hdr.wheel.<zone>.reset`, `grade.hdr.<zone>.<field>` (`exposure`, `sat`, `range`,
//! `falloff`).

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::grade::{GradeNode, HDR_DARK_ZONES, HDR_ZONES, NodeParamKind, node_param};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::grade::{puck_of, rgb_of};
use crate::widgets;

/// The wheels, Global first, with their titles.
pub(crate) const HDR_WHEELS: [(&str, &str); 7] = [
    ("global", "Global"),
    ("black", "Black"),
    ("dark", "Dark"),
    ("shadow", "Shadow"),
    ("light", "Light"),
    ("highlight", "Highlight"),
    ("specular", "Specular"),
];

/// The number fields under a wheel (Global has only the first two).
pub(crate) const HDR_FIELDS: [(&str, &str); 4] = [("exposure", "Exposure"), ("sat", "Saturation"), ("range", "Range"), ("falloff", "Falloff")];

/// The balance offsets at the wheel's rim.
const WHEEL_SCALE: f32 = 0.5;
/// The stops the zone graph spans either side of mid grey.
const GRAPH_STOPS: f32 = 8.0;
const GRAPH_H: f32 = 70.0;
const WHEEL_MIN: f32 = 64.0;
const WHEEL_MAX: f32 = 150.0;
const COL_MIN: f32 = 120.0;

/// What the HDR controls ask for, on the selected node.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HdrEdit {
    /// Set a parameter (part of the current gesture's undo step).
    Set { key: String, value: Value },
    /// Reset one wheel (`Some(zone)`) or all of them.
    Reset(Option<&'static str>),
}

/// The parameter key `hdr.<zone>.<what>`.
pub(crate) fn key(zone: &str, what: &str) -> String {
    ["hdr.", zone, ".", what].concat()
}

/// A zone name as shown (Resolve's words; translated in the `hdr` context).
fn title(name: &'static str) -> &'static str {
    crate::i18n::tr_ctx("hdr", name)
}

/// The grey a zone is drawn in on the graph (darker zones darker).
fn zone_colour(i: usize) -> Color32 {
    let g = 70 + (i.min(5) as u8) * 30;
    Color32::from_gray(g)
}

/// The HDR tab's controls for `node` at clip time `t`.
pub(crate) fn controls(
    ui: &mut egui::Ui,
    node: &GradeNode,
    t: Tick,
    tk: &crate::theme::Tokens,
    edits: &mut Vec<HdrEdit>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("HDR zone wheels: exposure in stops from mid grey, per tonal zone")).color(tk.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.button(tl!("Reset All")).on_hover_text(tl!("Reset every HDR wheel"));
            autos.push(("grade.hdr.reset".to_string(), r.rect, tl!("Reset All").to_string()));
            if r.clicked() {
                edits.push(HdrEdit::Reset(None));
            }
        });
    });
    ui.add_space(4.0);
    zone_graph(ui, node, t, tk, autos);
    ui.add_space(6.0);
    let width = ui.available_width();
    let cols = ((width / COL_MIN).floor() as usize).clamp(1, HDR_WHEELS.len());
    let col_w = ((width - 8.0 * (cols as f32 - 1.0)) / cols as f32).max(WHEEL_MIN);
    let size = (col_w * 0.72).clamp(WHEEL_MIN, WHEEL_MAX);
    for row in HDR_WHEELS.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for &(zone, name) in row {
                ui.allocate_ui_with_layout(vec2(col_w, size + 150.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.set_width(col_w);
                    wheel(ui, node, t, tk, zone, name, col_w, size, edits, autos);
                });
            }
        });
        ui.add_space(6.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn wheel(
    ui: &mut egui::Ui,
    node: &GradeNode,
    t: Tick,
    tk: &crate::theme::Tokens,
    zone: &'static str,
    name: &'static str,
    col_w: f32,
    size: f32,
    edits: &mut Vec<HdrEdit>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let z = node.hdr_zone_at(zone, t);
    // title, centred, with the wheel's reset at its right
    let (tr, _) = ui.allocate_exact_size(vec2(col_w, 18.0), Sense::hover());
    ui.painter().text(tr.center(), Align2::CENTER_CENTER, title(name), egui::FontId::proportional(13.0), tk.text);
    let rr = Rect::from_center_size(pos2((tr.center().x + size / 2.0).min(tr.max.x - 9.0), tr.center().y), vec2(18.0, 18.0));
    let reset = ui.interact(rr, ui.id().with(("grade-hdr-reset", zone)), Sense::click()).on_hover_text(format!("{} {}", tl!("Reset"), title(name)));
    crate::icons::paint(ui.painter(), rr.shrink(3.0), crate::icons::Icon::Undo, if reset.hovered() { tk.text } else { tk.text_dim });
    autos.push((format!("grade.hdr.wheel.{zone}.reset"), rr, format!("Reset {name}")));
    if reset.clicked() {
        edits.push(HdrEdit::Reset(Some(zone)));
    }
    let puck = puck_of(z.balance, WHEEL_SCALE);
    let wheel_rect = Rect::from_min_size(pos2(ui.cursor().min.x + (col_w - size) / 2.0, ui.cursor().min.y), vec2(size, size));
    let ch = widgets::color::grade_wheel(ui, puck, size);
    autos.push((format!("grade.hdr.wheel.{zone}"), wheel_rect, format!("{name} balance")));
    if let Some(p) = ch {
        let c = rgb_of(p, WHEEL_SCALE);
        let mean = (z.balance[0] + z.balance[1] + z.balance[2]) / 3.0;
        edits.push(HdrEdit::Set { key: key(zone, "balance"), value: json!([mean + c[0], mean + c[1], mean + c[2], 0.0]) });
    }
    ui.add_space(4.0);
    let fields = if zone == "global" { &HDR_FIELDS[..2] } else { &HDR_FIELDS[..] };
    for &(what, label) in fields {
        let k = key(zone, what);
        let Some(NodeParamKind::Float { min, max, .. }) = node_param(&k).map(|d| d.kind) else { continue };
        let mut v = node.float_at(&k, t);
        let band = match what {
            "exposure" => widgets::color::Band::Gradient(Color32::BLACK, Color32::WHITE),
            "sat" => widgets::color::Band::Hues,
            _ => widgets::color::Band::Solid(Color32::from_gray(0x70)),
        };
        ui.horizontal(|ui| {
            let lw = (col_w * 0.45).max(40.0);
            let (lr, _) = ui.allocate_exact_size(vec2(lw, 18.0), Sense::hover());
            ui.painter().text(pos2(lr.max.x - 2.0, lr.center().y), Align2::RIGHT_CENTER, crate::i18n::t(label), egui::FontId::proportional(12.0), tk.text_dim);
            let r = widgets::color::value_box(ui, &mut v, (col_w - lw - 12.0).clamp(36.0, 64.0), min..=max, 2, band);
            let r = match what {
                "range" => r.on_hover_text(tl!("Where the zone ends, in stops from mid grey")),
                "falloff" => r.on_hover_text(tl!("How soft the zone's edge is, in stops")),
                "exposure" => r.on_hover_text(tl!("Exposure in stops")),
                _ => r,
            };
            autos.push((format!("grade.hdr.{zone}.{what}"), r.rect, format!("{name} {label}")));
            if r.changed() {
                edits.push(HdrEdit::Set { key: k.clone(), value: json!(v) });
            }
        });
        ui.add_space(4.0);
    }
}

/// Where each zone acts: its weight (0…1) over −8…+8 stops from mid grey, the darker zones in
/// darker greys; a line at mid grey.
fn zone_graph(ui: &mut egui::Ui, node: &GradeNode, t: Tick, tk: &crate::theme::Tokens, autos: &mut Vec<(String, Rect, String)>) {
    let w = ui.available_width().max(60.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, GRAPH_H), Sense::hover());
    let resp = resp.on_hover_text(tl!("How much each zone acts at each brightness (stops from mid grey)"));
    autos.push(("grade.hdr.graph".to_string(), resp.rect, "HDR zones".to_string()));
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, Color32::from_rgb(0x12, 0x12, 0x14));
    let plot = rect.shrink2(vec2(4.0, 6.0));
    let x_of = |s: f32| plot.min.x + (s + GRAPH_STOPS) / (2.0 * GRAPH_STOPS) * plot.width();
    let y_of = |v: f32| plot.max.y - v.clamp(0.0, 1.0) * plot.height();
    for s in [-6.0f32, -4.0, -2.0, 2.0, 4.0, 6.0] {
        p.line_segment([pos2(x_of(s), plot.min.y), pos2(x_of(s), plot.max.y)], Stroke::new(1.0, Color32::from_gray(0x24)));
    }
    p.line_segment([pos2(x_of(0.0), rect.min.y), pos2(x_of(0.0), rect.max.y)], Stroke::new(1.0, tk.text_dim));
    const STEPS: usize = 96;
    for (i, zone) in HDR_ZONES.iter().enumerate() {
        let z = node.hdr_zone_at(zone, t);
        let active = !z.is_identity();
        let pts: Vec<egui::Pos2> = (0..=STEPS)
            .map(|k| {
                let s = -GRAPH_STOPS + 2.0 * GRAPH_STOPS * k as f32 / STEPS as f32;
                pos2(x_of(s), y_of(filmcraft_color::ops::zone_weight(i < HDR_DARK_ZONES, s, z.range, z.falloff)))
            })
            .collect();
        let col = if active { tk.accent } else { zone_colour(i) };
        p.add(egui::Shape::line(pts, Stroke::new(if active { 1.6 } else { 1.0 }, col)));
    }
    let font = egui::FontId::proportional(10.0);
    p.text(pos2(plot.min.x, rect.max.y - 1.0), Align2::LEFT_BOTTOM, "−8", font.clone(), tk.text_dim);
    p.text(pos2(x_of(0.0) + 3.0, rect.max.y - 1.0), Align2::LEFT_BOTTOM, "0", font.clone(), tk.text_dim);
    p.text(pos2(plot.max.x, rect.max.y - 1.0), Align2::RIGHT_BOTTOM, "+8", font, tk.text_dim);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wheel_and_field_is_a_node_parameter() {
        assert_eq!(HDR_WHEELS.len(), HDR_ZONES.len() + 1);
        for (zone, _) in HDR_WHEELS {
            assert!(node_param(&key(zone, "balance")).is_some(), "{zone}");
            let fields = if zone == "global" { &HDR_FIELDS[..2] } else { &HDR_FIELDS[..] };
            for (what, _) in fields {
                assert!(node_param(&key(zone, what)).is_some(), "{zone}.{what}");
            }
        }
    }
}
