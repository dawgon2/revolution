//! The selected mixer node's settings on the Color page (fork C2b, FORK-PLAN §1a.113): one row per
//! connected input of a parallel mixer (weight, with its share of the mix), a layer mixer (blend
//! mode, opacity) or a key mixer (mode, invert); and, on a correction node another node's key
//! reaches, that key input (invert, gain, offset). Every edit is `grade.setParam`,
//! `grade.resetNode` or `grade.toggleAnimation` on the node, like the other node parameters, so a
//! drag is one undo step and the numbers keyframe.
//!
//! Automation ids (`<i>` is the input's index from 0, as in the parameter keys; the panel shows it
//! from 1, as the canvas ports do): `grade.mixer.<i>.weight`, `grade.mixer.<i>.mode` (its entries
//! `grade.mixer.<i>.mode.<m>`), `grade.mixer.<i>.opacity`, `grade.mixer.<i>.invert`;
//! `grade.key.invert`, `grade.key.gain`, `grade.key.offset`. Each slider also has `<id>.reset` and
//! `<id>.anim` (the keyframe toggle).

use egui::{Rect, vec2};
use filmcraft_project::dag::Port;
use filmcraft_project::effect::BLEND_MODES;
use filmcraft_project::grade::{Grade, GradeNode, MAX_MIX_INPUTS, NodeKind, NodeParamKind, SOURCE, node_param};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::grade::{FloatRow, RowEdit, float_row, short};

/// The key mixer's modes, by `kmix.mode.<i>` value.
pub(crate) const KEY_MODES: [&str; 3] = ["Add", "Subtract", "Intersect"];

/// What the controls ask for, on the selected node.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ParamEdit {
    /// Set the parameter; `drag`: part of a drag (merged into one undo step per gesture).
    Set {
        key: &'static str,
        value: Value,
        drag: bool,
    },
    Reset(&'static str),
    ToggleAnimation(&'static str),
}

/// Whether a node of this kind shows mixer controls instead of the correction tabs.
pub(crate) fn is_mixer(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::ParallelMixer | NodeKind::LayerMixer | NodeKind::KeyMixer)
}

/// Node `node`'s connected mixer inputs (input index, the port feeding it), in index order.
pub(crate) fn connected_inputs(g: &Grade, node: u64) -> Vec<(u8, Port)> {
    let mut v: Vec<(u8, Port)> =
        g.effective_edges().into_iter().filter(|e| e.to.node == node && e.to.index < MAX_MIX_INPUTS).map(|e| (e.to.index, e.from)).collect();
    v.sort_by_key(|(i, _)| *i);
    v.dedup_by_key(|(i, _)| *i);
    v
}

/// What an input is called: the source node's name, else "Node N" (its place in the grade, as
/// the panel heading counts), "Input" for the grade's input; a splitter's output adds its channel.
pub(crate) fn source_label(g: &Grade, from: Port) -> String {
    if from.node == SOURCE {
        return tl!("Input").to_string();
    }
    let Some((pos, n)) = g.nodes.iter().enumerate().find(|(_, n)| n.id == from.node) else {
        return tlf!("Node {n}", n = from.node);
    };
    let name = if n.label.is_empty() { tlf!("Node {n}", n = format!("{:02}", pos.saturating_add(1))) } else { short(&n.label, 24) };
    match ["R", "G", "B"].get(usize::from(from.index)) {
        Some(ch) if n.kind == NodeKind::Splitter => format!("{name} · {ch}"),
        _ => name,
    }
}

/// The share of the mix each connected input of a parallel mixer gets at `t`, as the render
/// weighs them (`filmcraft_render::grade::graph`): its gain over the sum of the gains, an equal
/// share when they sum to 0.
pub(crate) fn parallel_shares(node: &GradeNode, inputs: &[(u8, Port)], t: Tick) -> Vec<f32> {
    let gains: Vec<f32> = inputs.iter().map(|(i, _)| node.float_at(&format!("mix.gain.{i}"), t)).collect();
    let sum: f32 = gains.iter().sum();
    let equal = 1.0 / inputs.len().max(1) as f32;
    gains.into_iter().map(|g| if sum.is_finite() && sum > 0.0 { g / sum } else { equal }).collect()
}

/// A keyframable number row for node parameter `key`, shown × `scale` (100 for a percentage).
#[allow(clippy::too_many_arguments)]
fn number(
    ui: &mut egui::Ui,
    t: &crate::theme::Tokens,
    node: &GradeNode,
    mt: Tick,
    key: &str,
    label: &str,
    percent: bool,
    id: &str,
    edits: &mut Vec<ParamEdit>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let Some(def) = node_param(key) else { return };
    let NodeParamKind::Float { min, max, .. } = def.kind else { return };
    let scale = if percent { 100.0 } else { 1.0 };
    let (reset_id, anim_id) = (format!("{id}.reset"), format!("{id}.anim"));
    let row = FloatRow {
        label,
        value: node.float_at(def.key, mt) * scale,
        min: min * scale,
        max: max * scale,
        step: 0.0,
        suffix: if percent { " %" } else { "" },
        enabled: true,
        animated: Some(node.param(def.key).is_some_and(|p| p.is_animated())),
        id,
        reset_id: &reset_id,
        anim_id: &anim_id,
    };
    match float_row(ui, t, row, autos) {
        Some(RowEdit::Set(v)) => edits.push(ParamEdit::Set { key: def.key, value: json!(v / scale), drag: true }),
        Some(RowEdit::Reset) => edits.push(ParamEdit::Reset(def.key)),
        Some(RowEdit::ToggleAnimation) => edits.push(ParamEdit::ToggleAnimation(def.key)),
        None => {}
    }
}

/// A dropdown for node parameter `key` holding an index into `options` (shown translated).
#[allow(clippy::too_many_arguments)]
fn choice(
    ui: &mut egui::Ui,
    t: &crate::theme::Tokens,
    node: &GradeNode,
    mt: Tick,
    key: &str,
    label: &str,
    options: &[&str],
    id: &str,
    edits: &mut Vec<ParamEdit>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let Some(def) = node_param(key) else { return };
    let v = node.float_at(def.key, mt);
    let sel = if v.is_finite() && v > 0.0 { v.round() as usize } else { 0 };
    ui.horizontal(|ui| {
        ui.add_sized(vec2(104.0, 18.0), egui::Label::new(egui::RichText::new(label).color(t.text_dim)));
        let shown = options.get(sel).map_or("", |o| crate::i18n::t(o));
        let r = egui::ComboBox::from_id_salt(("grade-mixer-choice", node.id, def.key)).selected_text(shown).width(150.0).show_ui(ui, |ui| {
            for (m, o) in options.iter().enumerate() {
                let name = crate::i18n::t(o);
                let r = ui.selectable_label(m == sel, name);
                autos.push((format!("{id}.{m}"), r.rect, name.to_string()));
                if r.clicked() && m != sel {
                    edits.push(ParamEdit::Set { key: def.key, value: json!(m), drag: false });
                }
            }
        });
        autos.push((id.to_string(), r.response.rect, label.to_string()));
    });
}

/// A checkbox for a switch parameter (≥ ½ is on).
#[allow(clippy::too_many_arguments)]
fn switch(
    ui: &mut egui::Ui,
    node: &GradeNode,
    mt: Tick,
    key: &str,
    label: &str,
    id: &str,
    edits: &mut Vec<ParamEdit>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let Some(def) = node_param(key) else { return };
    let mut on = node.float_at(def.key, mt) >= 0.5;
    let r = ui.checkbox(&mut on, label);
    autos.push((id.to_string(), r.rect, label.to_string()));
    if r.changed() {
        edits.push(ParamEdit::Set { key: def.key, value: json!(if on { 1.0 } else { 0.0 }), drag: false });
    }
}

/// The mixer node's settings: a heading, then one block per connected input.
pub(crate) fn mixer_controls(
    ui: &mut egui::Ui,
    g: &Grade,
    node: &GradeNode,
    mt: Tick,
    t: &crate::theme::Tokens,
    edits: &mut Vec<ParamEdit>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let (title, hint) = match node.kind {
        NodeKind::ParallelMixer => (tl!("Parallel Mixer"), tl!("Each input's weight over the sum of the weights is its share of the mix.")),
        NodeKind::LayerMixer => (tl!("Layer Mixer"), tl!("Higher inputs are on top: each one blends over the inputs below it.")),
        NodeKind::KeyMixer => (tl!("Key Mixer"), tl!("Each key adds to, subtracts from or intersects the keys before it.")),
        _ => return,
    };
    ui.label(egui::RichText::new(title).color(t.text).strong());
    ui.label(egui::RichText::new(hint).color(t.text_faint));
    let inputs = connected_inputs(g, node.id);
    if inputs.is_empty() {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("No inputs connected: drag a node's output onto one of this mixer's inputs.")).color(t.text_dim));
        return;
    }
    let shares = if node.kind == NodeKind::ParallelMixer { parallel_shares(node, &inputs, mt) } else { Vec::new() };
    for (k, (i, from)) in inputs.iter().enumerate() {
        ui.add_space(6.0);
        let name = source_label(g, *from);
        ui.label(egui::RichText::new(tlf!("Input {n}: {name}", n = u32::from(*i) + 1, name)).color(t.text));
        match node.kind {
            NodeKind::ParallelMixer => {
                ui.horizontal(|ui| {
                    number(ui, t, node, mt, &format!("mix.gain.{i}"), tl!("Weight"), false, &format!("grade.mixer.{i}.weight"), edits, autos);
                    let share = shares.get(k).copied().unwrap_or(0.0);
                    let r =
                        ui.label(egui::RichText::new(format!("{:.0} %", share * 100.0)).color(t.text_dim)).on_hover_text(tl!("This input's share of the mix"));
                    autos.push((format!("grade.mixer.{i}.share"), r.rect, format!("{:.0} %", share * 100.0)));
                });
            }
            NodeKind::LayerMixer => {
                choice(ui, t, node, mt, &format!("layer.mode.{i}"), tl!("Blend Mode"), BLEND_MODES, &format!("grade.mixer.{i}.mode"), edits, autos);
                number(ui, t, node, mt, &format!("layer.opacity.{i}"), tl!("Opacity"), true, &format!("grade.mixer.{i}.opacity"), edits, autos);
            }
            NodeKind::KeyMixer => {
                ui.horizontal(|ui| {
                    choice(ui, t, node, mt, &format!("kmix.mode.{i}"), tl!("Mode"), &KEY_MODES, &format!("grade.mixer.{i}.mode"), edits, autos);
                    switch(ui, node, mt, &format!("kmix.invert.{i}"), tl!("Invert"), &format!("grade.mixer.{i}.invert"), edits, autos);
                });
            }
            _ => {}
        }
    }
}

/// A correction node's key input (an outside node's, or any key wired to its Key input): invert,
/// gain and offset. Nothing when no key reaches the node.
pub(crate) fn key_input_controls(
    ui: &mut egui::Ui,
    g: &Grade,
    node: &GradeNode,
    mt: Tick,
    t: &crate::theme::Tokens,
    edits: &mut Vec<ParamEdit>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    if node.kind != NodeKind::Correction {
        return;
    }
    let Some(from) = g.effective_edges().into_iter().find(|e| e.to.node == node.id && e.to.index == 1).map(|e| e.from) else { return };
    let name = source_label(g, from);
    ui.label(egui::RichText::new(tlf!("Key input: {name}", name)).color(t.text));
    switch(ui, node, mt, "key.invert", tl!("Invert key input"), "grade.key.invert", edits, autos);
    number(ui, t, node, mt, "key.gain", tl!("Key gain"), false, "grade.key.gain", edits, autos);
    number(ui, t, node, mt, "key.offset", tl!("Key offset"), false, "grade.key.offset", edits, autos);
    ui.separator();
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::dag::Edge;
    use filmcraft_project::keyframe::{Param, ParamValue};

    fn mixer_grade() -> Grade {
        let mut g = Grade::with_node();
        g.nodes.push(GradeNode::new(2));
        let mut m = GradeNode::new(3);
        m.kind = NodeKind::ParallelMixer;
        g.nodes.push(m);
        g.edges = vec![
            Edge::new(Port::new(SOURCE, 0), Port::new(1, 0)),
            Edge::new(Port::new(SOURCE, 0), Port::new(2, 0)),
            Edge::new(Port::new(2, 0), Port::new(3, 1)),
            Edge::new(Port::new(1, 0), Port::new(3, 0)),
        ];
        g
    }

    #[test]
    fn inputs_in_index_order_with_labels_and_shares() {
        let mut g = mixer_grade();
        if let Some(n) = g.node_mut(2) {
            n.label = "Skin".into();
        }
        let ins = connected_inputs(&g, 3);
        assert_eq!(ins.iter().map(|(i, p)| (*i, p.node)).collect::<Vec<_>>(), vec![(0, 1), (1, 2)]);
        assert_eq!(source_label(&g, Port::new(1, 0)), "Node 01");
        assert_eq!(source_label(&g, Port::new(2, 0)), "Skin");
        assert_eq!(source_label(&g, Port::new(SOURCE, 0)), "Input");
        let Some(m) = g.node(3).cloned() else { return };
        assert_eq!(parallel_shares(&m, &ins, Tick(0)), vec![0.5, 0.5]);
        let mut m = m;
        m.params.insert("mix.gain.1".into(), Param::new(ParamValue::Float(3.0)));
        // (clamped to the schema's 0…2: 1 / 3 and 2 / 3)
        let s = parallel_shares(&m, &ins, Tick(0));
        assert!((s[0] - 1.0 / 3.0).abs() < 1e-6 && (s[1] - 2.0 / 3.0).abs() < 1e-6, "{s:?}");
        m.params.insert("mix.gain.0".into(), Param::new(ParamValue::Float(0.0)));
        m.params.insert("mix.gain.1".into(), Param::new(ParamValue::Float(0.0)));
        assert_eq!(parallel_shares(&m, &ins, Tick(0)), vec![0.5, 0.5], "all zero: an equal share");
        assert!(parallel_shares(&m, &[], Tick(0)).is_empty());
    }
}
