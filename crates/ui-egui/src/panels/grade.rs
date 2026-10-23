//! Color Grading panels (fork GRADE-PLAN G1): **Nodes**, the selected clip's node canvas (fork C2:
//! a serial grade as a row; parallel / layer / outside / splitter / key-mixer branches),
//! and **Primaries**, the Lift / Gamma / Gain / Offset wheels, contrast / pivot / saturation /
//! temperature / tint and the per-node curves of the selected node.
//!
//! Every edit goes through the `grade.*` engine commands; drags use one `merge` key per press so a
//! drag is one undo step. Automation ids: `grade.node.<id>`, `grade.addNode`, `grade.insertNode`,
//! `grade.removeNode`, `grade.toggleNode`, `grade.resetNode`, `grade.renameNode` (and its field
//! `grade.renameNode.field`), `grade.bypass`, `grade.tab.<tab>`,
//! `grade.wheel.<key>`, `grade.param.<key>`, `grade.curve.<channel>`; a mixer node's settings and a
//! correction node's key input: see `grade_mixer`.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::grade::{Grade, GradeNode, MAX_NODES, NodeKind, NodeParamKind, SOURCE, node_param};
use filmcraft_project::{ClipId, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::GradeRename;
use crate::widgets;
use crate::widgets::node_canvas::{CanvasEvent, CanvasGraph, EdgeView, NodeView};

/// The first selected clip on a video track.
fn selected(app: &FilmcraftApp) -> Option<(ClipId, TrackItem)> {
    let seq = app.session.active_sequence()?;
    app.session.state.selection.iter().find_map(|c| {
        let (tid, it) = seq.find_item(*c)?;
        (seq.track(tid)?.kind == filmcraft_project::TrackKind::Video).then(|| (*c, it.clone()))
    })
}

/// The node the panels edit: the engine's current node (`grade.selectNode`) when it belongs to
/// this clip and still exists, else the first.
fn current_node(app: &FilmcraftApp, clip: ClipId, g: &Grade) -> Option<u64> {
    app.session.state.grade_node.filter(|sel| sel.clip == clip && g.node(sel.node).is_some()).map(|sel| sel.node).or_else(|| g.nodes.first().map(|n| n.id))
}

fn select_node(app: &mut FilmcraftApp, clip: ClipId, node: u64) {
    run(app, "grade.selectNode", json!({"clip": clip.0, "node": node}));
}

/// A label cut to `n` characters (with an ellipsis), for labels from hostile files.
pub(crate) fn short(text: &str, n: usize) -> String {
    if text.chars().count() <= n { text.to_string() } else { format!("{}…", text.chars().take(n.saturating_sub(1)).collect::<String>()) }
}

/// Run a command, showing a failure in the status line.
fn run(app: &mut FilmcraftApp, cmd: &str, p: Value) -> Option<Value> {
    match app.session.execute(cmd, p) {
        Ok(v) => Some(v),
        Err(e) => {
            app.ui.status = e.to_string();
            None
        }
    }
}

/// The Primaries panel's gesture number: bumped on every pointer press and key press, so a drag
/// is one undo step while separate clicks and key nudges are separate steps.
pub(crate) fn gesture(ui: &egui::Ui) -> u64 {
    // (bumped once per frame: the Primaries and Curves panels both ask, and a frame can take
    // several passes; a second bump in the press frame would split a drag into two steps)
    let id = egui::Id::new("grade-gesture");
    let frame = ui.ctx().cumulative_frame_nr();
    let (mut g, at): (u64, u64) = ui.data(|d| d.get_temp(id)).unwrap_or((0, u64::MAX));
    if at != frame
        && ui.input(|i| i.pointer.any_pressed() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::Text(_))))
    {
        g = g.wrapping_add(1);
        ui.data_mut(|d| d.insert_temp(id, (g, frame)));
    }
    g
}

// ---------- Nodes ----------

/// Longest node name, as `grade.labelNode` keeps it.
const MAX_LABEL: usize = 64;

pub fn show_nodes(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some((clip, it)) = selected(app) else {
        crate::dock::placeholder(ui, rect, &t, "(select a video clip to grade)");
        return;
    };
    let grade = it.grade.clone().unwrap_or_default();
    let cur = current_node(app, clip, &grade);
    let mut cmds: Vec<(&'static str, Value)> = Vec::new();
    let mut select: Option<u64> = None;
    let mut rename: Option<u64> = None;
    let mut autos: Vec<(String, Rect, String)> = Vec::new();
    // a rename whose clip is no longer selected or whose node is gone ends without a change
    if app.ui.grade_rename.as_ref().is_some_and(|rn| rn.clip != clip.0 || grade.node(rn.node).is_none()) {
        app.ui.grade_rename = None;
    }

    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 4.0))).id_salt("grade-nodes"));
    bui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("Nodes · {}", short(&it.name, 40))).color(t.text_dim));
        if grade.bypass {
            ui.label(egui::RichText::new("bypassed").color(t.text_faint).italics());
        }
    });
    // the buttons wrap in a narrow panel (the Color page puts the nodes in a side column)
    bui.horizontal_wrapped(|ui| {
        let mut button = |ui: &mut egui::Ui, id: &str, text: &str, tip: &str, enabled: bool| -> bool {
            let r = ui.add_enabled(enabled, egui::Button::new(text).small()).on_hover_text(tip);
            autos.push((id.to_string(), r.rect, tip.to_string()));
            r.clicked()
        };
        let has = cur.is_some();
        if button(ui, "grade.addNode", "+ Serial", "Add a serial node after the selected one", true) {
            cmds.push(("grade.addNode", json!({"clip": clip.0, "after": cur})));
        }
        if button(ui, "grade.insertNode", "+ Before", "Add a serial node before the selected one", true) {
            cmds.push(("grade.insertNode", json!({"clip": clip.0, "before": cur})));
        }
        if button(ui, "grade.removeNode", "Delete", "Delete the selected node", has) {
            cmds.push(("grade.removeNode", json!({"clip": clip.0, "node": cur})));
        }
        let enabled = cur.and_then(|id| grade.node(id)).is_none_or(|n| n.enabled);
        if button(ui, "grade.toggleNode", if enabled { "Disable" } else { "Enable" }, "Enable / disable the selected node", has) {
            cmds.push(("grade.toggleNode", json!({"clip": clip.0, "node": cur})));
        }
        if button(ui, "grade.resetNode", "Reset", "Reset the selected node", has) {
            cmds.push(("grade.resetNode", json!({"clip": clip.0, "node": cur})));
        }
        if button(ui, "grade.renameNode", "Rename", "Name the selected node", has) {
            rename = cur;
        }
        if button(ui, "grade.bypass", "Bypass", "Bypass the clip's grade", it.grade.is_some()) {
            cmds.push(("grade.bypass", json!({"clip": clip.0})));
        }
    });

    // the rename field (the canvas draws the nodes, so the name is typed above it)
    if app.ui.grade_rename.is_some() {
        let (field, _) = bui.allocate_exact_size(vec2(200.0, 20.0), Sense::hover());
        if let Some(done) = rename_field(app, &mut bui, field, clip) {
            cmds.extend(done);
        }
        autos.push(("grade.renameNode.field".into(), field, "Node name".into()));
    }

    // the node canvas (fork C2): a serial grade lays out as a row, branches stack in columns;
    // dragged nodes keep their place (fork C2b)
    let drag_gesture = gesture(ui);
    let canvas_rect = Rect::from_min_max(pos2(rect.min.x, bui.cursor().min.y), rect.max);
    let view = canvas_graph(&grade, cur);
    let mut state = app.ui.panels.grade_nodes.clone();
    // the canvas selection follows the current node (a multi-selection made on the canvas stays)
    if state.selection.len() <= 1 && state.selection.first().copied() != cur {
        state.selection = cur.into_iter().collect();
    }
    // the whole graph is fitted into view when its shape changes (another clip, nodes added,
    // removed or rewired, the panel resized, Reset Layout); pan and zoom are the user's in
    // between, and moving a dragged node never refits (fork C2b)
    let shape = {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        clip.0.hash(&mut h);
        grade.has_positions().hash(&mut h);
        for n in &view.nodes {
            let auto = if grade.stored_pos(n.id).is_some() { None } else { Some((n.pos[0].to_bits(), n.pos[1].to_bits())) };
            (n.id, auto, n.inputs.len()).hash(&mut h);
        }
        (canvas_rect.width().to_bits(), canvas_rect.height().to_bits()).hash(&mut h);
        h.finish()
    };
    let fit_id = egui::Id::new("grade-canvas-fit");
    // (whether a node was being dragged on the canvas last frame, or was just dropped)
    let node_drag_id = egui::Id::new("grade-canvas-node-drag");
    let node_drag = ui.data(|d| d.get_temp::<bool>(node_drag_id)).unwrap_or(false);
    if ui.data(|d| d.get_temp::<u64>(fit_id)) != Some(shape) {
        if node_drag {
            // never while a node is being dragged: the view would jump under the pointer
            ui.data_mut(|d| d.insert_temp(fit_id, shape));
        } else if !ui.input(|i| i.pointer.any_down()) {
            ui.data_mut(|d| d.insert_temp(fit_id, shape));
            widgets::node_canvas::fit(&mut state, &view, canvas_rect.size());
        }
        // (any other held pointer, a dock splitter or a scrub: the fit waits for the release)
    }
    let events = if canvas_rect.is_positive() {
        ui.scope_builder(egui::UiBuilder::new().max_rect(canvas_rect).id_salt(("grade-canvas", clip.0)), |ui| {
            widgets::node_canvas::node_canvas(ui, &view, &mut state, &mut app.auto, "grade")
        })
        .inner
    } else {
        Vec::new()
    };
    app.ui.panels.grade_nodes = state;
    let dragging = events.iter().any(|e| matches!(e, CanvasEvent::Move(..) | CanvasEvent::MoveEnd(..)));
    ui.data_mut(|d| d.insert_temp(node_drag_id, dragging));
    if grade.nodes.is_empty() {
        let p = ui.painter_at(canvas_rect);
        p.text(canvas_rect.center(), Align2::CENTER_CENTER, crate::i18n::t("No nodes: + Serial adds one"), egui::FontId::proportional(12.0), t.text_faint);
    }
    let real = |ids: Vec<u64>| ids.into_iter().filter(|n| *n != SOURCE && grade.node(*n).is_some()).collect::<Vec<u64>>();
    for e in events {
        match e {
            CanvasEvent::Select(ids) => {
                if let [one] = real(ids).as_slice() {
                    select = Some(*one);
                }
            }
            CanvasEvent::View(n) if n != SOURCE => select = Some(n),
            CanvasEvent::Connect { from, to } => {
                cmds.push(("grade.connect", json!({"clip": clip.0, "from": from.0, "fromPort": from.1, "to": to.0, "toPort": to.1})))
            }
            CanvasEvent::Disconnect { to } => cmds.push(("grade.disconnect", json!({"clip": clip.0, "to": to.0, "toPort": to.1}))),
            // (every frame of a drag: one gesture, so one undo step)
            CanvasEvent::Move(n, p) | CanvasEvent::MoveEnd(n, p) => {
                if let Some(nodes) = move_entries(&grade, &view, n, p) {
                    cmds.push(("grade.moveNodes", json!({"clip": clip.0, "nodes": nodes, "gesture": drag_gesture})));
                }
            }
            CanvasEvent::CanvasAction(a) if a == "resetLayout" => cmds.push(("grade.resetLayout", json!({"clip": clip.0}))),
            CanvasEvent::AddRequested { kind, .. } => {
                if let Some(c) = add_command(&kind, clip, cur) {
                    cmds.push(c);
                }
            }
            // (several nodes: one command, one undo step)
            CanvasEvent::Bypass(ids) if !real(ids.clone()).is_empty() => cmds.push(("grade.toggleNode", json!({"clip": clip.0, "nodes": real(ids)}))),
            CanvasEvent::Delete(ids) if !real(ids.clone()).is_empty() => cmds.push(("grade.removeNode", json!({"clip": clip.0, "nodes": real(ids)}))),
            CanvasEvent::SetOutput(n) if n != SOURCE => cmds.push(("grade.setOutput", json!({"clip": clip.0, "node": n}))),
            CanvasEvent::NodeAction { node, action } if node != SOURCE => match action.as_str() {
                "rename" => rename = Some(node),
                "reset" => cmds.push(("grade.resetNode", json!({"clip": clip.0, "node": node}))),
                other => {
                    if let Some(c) = add_command(other, clip, Some(node)) {
                        cmds.push(c);
                    }
                }
            },
            _ => {}
        }
    }

    for (id, r, label) in autos {
        if r.is_positive() {
            app.auto.add(&id, r, &label);
        }
    }
    if let Some(id) = select {
        select_node(app, clip, id);
    }
    if let Some(id) = rename
        && let Some(n) = grade.node(id)
    {
        select_node(app, clip, id);
        app.ui.grade_rename = Some(GradeRename { clip: clip.0, node: id, text: n.label.clone() });
    }
    for (cmd, p) in cmds {
        if let Some(v) = run(app, cmd, p)
            && let Some(id) = v.get("node").and_then(Value::as_u64)
        {
            select_node(app, clip, id);
        }
    }
}

/// The node canvas's add entries (its background menu and each node's menu): key, label (the
/// command's label), command.
pub(crate) const ADD_ACTIONS: [(&str, &str, &str); 6] = [
    ("serial", "Add Serial Node", "grade.addNode"),
    ("parallel", "Add Parallel Node", "grade.addParallel"),
    ("layer", "Add Layer Node", "grade.addLayer"),
    ("outside", "Add Outside Node", "grade.addOutside"),
    ("splitter", "Add Splitter / Combiner Nodes", "grade.addSplitter"),
    ("keyMixer", "Add Key Mixer", "grade.addKeyMixer"),
];

/// The command an add entry runs on node `at` (none: the grade's current node).
fn add_command(key: &str, clip: ClipId, at: Option<u64>) -> Option<(&'static str, Value)> {
    let (_, _, cmd) = ADD_ACTIONS.iter().find(|(k, _, _)| *k == key)?;
    Some(if *cmd == "grade.addNode" { (*cmd, json!({"clip": clip.0, "after": at})) } else { (*cmd, json!({"clip": clip.0, "node": at})) })
}

/// What a node kind is called on the canvas.
fn kind_title(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Correction => "Corrector",
        NodeKind::ParallelMixer => "Parallel Mixer",
        NodeKind::LayerMixer => "Layer Mixer",
        NodeKind::Splitter => "Splitter",
        NodeKind::Combiner => "Combiner",
        NodeKind::KeyMixer => "Key Mixer",
    }
}

/// The `nodes` of a `grade.moveNodes` for node `n` dragged to `p` (fork C2b): the first drag on a
/// grade laid out automatically also stores where every other node is now, so nothing else moves;
/// later drags store only the dragged node. None when the position isn't usable or unchanged.
fn move_entries(g: &Grade, view: &CanvasGraph, n: u64, p: [f32; 2]) -> Option<Value> {
    if !(p[0].is_finite() && p[1].is_finite()) || (n != SOURCE && g.node(n).is_none()) {
        return None;
    }
    let shown = view.nodes.iter().find(|v| v.id == n).map(|v| v.pos);
    if g.stored_pos(n).is_some() && shown == Some(p) {
        return None;
    }
    let entry = |id: u64, at: [f32; 2]| json!({"node": id, "x": at[0], "y": at[1]});
    if g.has_positions() {
        return Some(json!([entry(n, p)]));
    }
    Some(Value::Array(view.nodes.iter().map(|v| if v.id == n { entry(n, p) } else { entry(v.id, v.pos) }).collect()))
}

/// The canvas view of a grade: the grade's input, then every node with its ports (key ports are
/// Resolve's green triangles), and the connections (the implicit chain of a serial grade).
pub(crate) fn canvas_graph(g: &Grade, cur: Option<u64>) -> CanvasGraph {
    let error = g.graph_order().err().map(|e| e.to_string());
    let output = g.output_node().ok();
    let edges = g.effective_edges();
    // (stored positions of dragged nodes, the automatic layout for the rest)
    let pos = g.layout();
    let at = |id: u64| pos.iter().find(|(i, _)| *i == id).map_or([0.0, 0.0], |(_, p)| *p);
    let mut nodes = vec![NodeView {
        id: SOURCE,
        title: crate::i18n::t("Input").to_string(),
        pos: at(SOURCE),
        inputs: Vec::new(),
        outputs: vec!["RGB".into()],
        bypassed: false,
        error: None,
        is_output: false,
        key_inputs: 0,
        key_outputs: 0,
        actions: Vec::new(),
    }];
    let key = crate::i18n::t("Key").to_string();
    for (i, n) in g.nodes.iter().take(MAX_NODES).enumerate() {
        // a mixer shows its connected inputs and one free one
        let mix_inputs = || {
            let used = edges.iter().filter(|e| e.to.node == n.id).map(|e| usize::from(e.to.index) + 2).max().unwrap_or(2);
            (1..=used.clamp(2, usize::from(filmcraft_project::grade::MAX_MIX_INPUTS))).map(|i| i.to_string()).collect::<Vec<_>>()
        };
        let (inputs, outputs, key_inputs, key_outputs): (Vec<String>, Vec<String>, u32, u32) = match n.kind {
            NodeKind::Correction => (vec!["RGB".into(), key.clone()], vec!["RGB".into(), key.clone()], 0b10, 0b10),
            NodeKind::ParallelMixer | NodeKind::LayerMixer => (mix_inputs(), vec!["RGB".into()], 0, 0),
            NodeKind::KeyMixer => (mix_inputs(), vec![key.clone()], u32::MAX, 1),
            NodeKind::Splitter => (vec!["RGB".into()], vec!["R".into(), "G".into(), "B".into()], 0, 0),
            NodeKind::Combiner => (vec!["R".into(), "G".into(), "B".into()], vec!["RGB".into()], 0, 0),
        };
        let name = if !n.label.is_empty() {
            n.label.clone()
        } else if n.kind == NodeKind::Correction {
            summary(n)
        } else {
            crate::i18n::t(kind_title(n.kind)).to_string()
        };
        let is_output = output == Some(n.id);
        nodes.push(NodeView {
            id: n.id,
            title: format!("{:02} {}", i + 1, short(&name, 16)),
            pos: at(n.id),
            inputs,
            outputs,
            bypassed: !n.enabled || g.bypass,
            // a graph that can't render says why on its output (or the current node)
            error: if is_output || (output.is_none() && cur == Some(n.id)) { error.clone() } else { None },
            is_output,
            key_inputs,
            key_outputs,
            actions: Vec::new(),
        });
    }
    let edges = edges.iter().map(|e| EdgeView { from: (e.from.node, e.from.index), to: (e.to.node, e.to.index) }).collect();
    let add_kinds: Vec<(String, String)> = ADD_ACTIONS.iter().map(|(k, l, _)| ((*k).to_string(), (*l).to_string())).collect();
    let mut node_actions = add_kinds.clone();
    node_actions.extend([("rename".to_string(), "Rename".to_string()), ("reset".to_string(), "Reset".to_string())]);
    // (the background menu offers Reset Layout once a node was placed by hand)
    let canvas_actions = if g.has_positions() { vec![("resetLayout".to_string(), "Reset Layout".to_string())] } else { Vec::new() };
    CanvasGraph { nodes, edges, add_kinds, node_actions, canvas_actions, on_screen_ids: true }
}

/// The in-place name field of the node being renamed, at `r`. Returns the command to run once the
/// rename ends (`Some(None)` when it ends without a change: Esc, or the same name).
fn rename_field(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, clip: ClipId) -> Option<Option<(&'static str, Value)>> {
    let rn = app.ui.grade_rename.clone()?;
    let mut text = rn.text;
    let id = egui::Id::new(("grade-rename-field", rn.clip, rn.node));
    let resp = ui.put(r, egui::TextEdit::singleline(&mut text).id(id).char_limit(MAX_LABEL).font(egui::FontId::proportional(11.0)).margin(vec2(3.0, 1.0)));
    // take keyboard focus the first frame the field is shown
    let shown = egui::Id::new("grade-rename-shown");
    if ui.ctx().data(|d| d.get_temp::<egui::Id>(shown)) != Some(id) {
        ui.ctx().data_mut(|d| d.insert_temp(shown, id));
        resp.request_focus();
    }
    if let Some(rn) = app.ui.grade_rename.as_mut() {
        rn.text = text.clone();
    }
    let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if !cancel && !resp.lost_focus() {
        return None;
    }
    app.ui.grade_rename = None;
    ui.ctx().data_mut(|d| d.remove::<egui::Id>(shown));
    let label = text.trim();
    let old = selected(app).and_then(|(_, it)| it.grade?.node(rn.node).map(|n| n.label.clone())).unwrap_or_default();
    if cancel || label == old {
        return Some(None);
    }
    Some(Some(("grade.labelNode", json!({"clip": clip.0, "node": rn.node, "label": label}))))
}

/// A short description of what a node changes (shown when it has no label).
fn summary(n: &GradeNode) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if ["lift", "gamma", "gain", "offset", "contrast", "saturation", "temperature", "tint"].iter().any(|k| n.params.contains_key(*k)) {
        parts.push("Primaries");
    }
    if n.params.keys().any(|k| k.starts_with("curve.")) {
        parts.push("Curves");
    }
    if n.float_at("grain.strength", Tick::ZERO) > 0.0 || n.params.get("grain.strength").is_some_and(|p| p.is_animated()) {
        parts.push("Grain");
    }
    if parts.is_empty() && n.params.contains_key("pivot") {
        parts.push("Primaries");
    }
    if parts.is_empty() { "Empty".into() } else { parts.join(" + ") }
}

// ---------- Primaries / Curves ----------

/// One wheel: node key, title, colour scale (at the rim the spread between the hue and its
/// opposite equals it), full-scale master offset.
pub(crate) const WHEELS: [(&str, &str, f32, f32); 4] =
    [("lift", "Lift", 0.25, 0.5), ("gamma", "Gamma", 0.25, 0.5), ("gain", "Gain", 0.5, 1.0), ("offset", "Offset", 0.25, 0.5)];

/// Smallest and largest wheel (points), the gap between wheel columns, and the height a wheel's
/// title, numbers and jog bar take besides the wheel itself.
const WHEEL_MIN: f32 = 72.0;
const WHEEL_MAX: f32 = 220.0;
const WHEEL_GAP: f32 = 12.0;
const WHEEL_CHROME: f32 = 74.0;
/// Height of one number strip (above and below the wheels), and of the LUT row with the spacing
/// around it.
const STRIP_H: f32 = 34.0;
const LUT_H: f32 = 44.0;
/// A strip number box's width and the gap after it.
const FIELD_W: f32 = 56.0;
const FIELD_GAP: f32 = 14.0;

/// Decimals a strip number shows (Resolve: Temp / Tint 0.0, the rest 0.000).
fn field_decimals(key: &str) -> usize {
    if matches!(key, "temperature" | "tint") { 1 } else { 3 }
}

/// The colour band under a strip number: what the control does, at a glance.
fn field_band(key: &str) -> widgets::color::Band {
    use widgets::color::Band;
    match key {
        "temperature" => Band::Gradient(Color32::from_rgb(0x3a, 0x7b, 0xf0), Color32::from_rgb(0xf0, 0x9a, 0x30)),
        "tint" => Band::Gradient(Color32::from_rgb(0x40, 0xc8, 0x50), Color32::from_rgb(0xe0, 0x40, 0xd0)),
        "contrast" => Band::Gradient(Color32::BLACK, Color32::WHITE),
        "pivot" => Band::Gradient(Color32::from_gray(0x30), Color32::from_gray(0xc0)),
        "saturation" => Band::Hues,
        _ => Band::Solid(Color32::from_gray(0x70)),
    }
}

/// A small round icon button (the white-balance tools at the start of the top strip).
fn round_icon(ui: &mut egui::Ui, glyph: &str, on: bool, t: &crate::theme::Tokens) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    let p = ui.painter();
    let col = if on {
        t.accent
    } else if resp.hovered() {
        t.text
    } else {
        t.text_dim
    };
    p.circle_stroke(r.center(), 9.0, Stroke::new(1.0, col));
    p.text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(11.0), col);
    resp
}

/// The tabs of the grade panel (`grade.tab.{id}`).
pub(crate) const TABS: [&str; 9] = ["primaries", "hdr", "curves", "warper", "qualifier", "windows", "magicMask", "noise", "match"];

/// The tabs the Curves panel takes over when it is in the layout (Resolve's middle palette).
const MIDDLE_TABS: [&str; 5] = ["curves", "warper", "qualifier", "windows", "magicMask"];

/// The window shapes the Windows tab adds (`grade.window.add.{shape}`).
pub(crate) const WINDOW_SHAPES: [&str; 3] = ["ellipse", "rectangle", "linear"];

pub(crate) const CURVES: [(&str, &str); 4] = [("curve.master", "Y"), ("curve.r", "R"), ("curve.g", "G"), ("curve.b", "B")];

/// Hue-ring angles of red, green and blue on [`widgets::color::color_wheel`] (hue = 1 − angle / τ,
/// counter-clockwise from +x): red 0°, green 240°, blue 120°.
const RGB_ANGLES: [f32; 3] = [0.0, 240.0, 120.0];

/// Colour offsets `[r, g, b]` (mean kept) → puck position (−1…1) for a wheel of full scale `scale`.
pub(crate) fn puck_of(rgb: [f32; 3], scale: f32) -> [f32; 2] {
    if scale <= 0.0 {
        return [0.0, 0.0];
    }
    let (mut x, mut y) = (0.0, 0.0);
    for (c, a) in rgb.iter().zip(RGB_ANGLES) {
        let (s, co) = a.to_radians().sin_cos();
        x += c * co;
        y += c * s;
    }
    let (x, y) = (x / scale, y / scale);
    let len = (x * x + y * y).sqrt();
    if len > 1.0 { [x / len, y / len] } else { [x, y] }
}

/// Puck position → colour offsets `[r, g, b]` (summing to 0) for a wheel of full scale `scale`.
pub(crate) fn rgb_of(puck: [f32; 2], scale: f32) -> [f32; 3] {
    RGB_ANGLES.map(|a| {
        let (s, co) = a.to_radians().sin_cos();
        2.0 / 3.0 * scale * (puck[0] * co + puck[1] * s)
    })
}

pub fn show_primaries(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    palette(app, ui, rect, false);
}

/// The Curves panel (fork, Resolve's middle palette): the Curves, Qualifier and Windows tabs of the
/// grade palette (see [`palette`]); its curve sits left of the channel buttons and soft clip.
pub fn show_curves(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    palette(app, ui, rect, true);
}

/// The grade palette: the tabbed Primaries panel, or (`middle`) the Curves panel. With the Curves
/// panel in the layout (the Color page), the tabs are split as in Resolve: Primaries, Noise
/// (Resolve's Motion Effects) and Match on the left, Curves, Qualifier and Windows in the middle;
/// without it the Primaries panel has all six.
fn palette(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, middle: bool) {
    let t = app.tokens;
    let Some((clip, it)) = selected(app) else {
        crate::dock::placeholder(ui, rect, &t, "(select a video clip to grade)");
        return;
    };
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 6.0))).id_salt(("grade-primaries", middle)));
    let Some(grade) = it.grade.clone().filter(|g| !g.nodes.is_empty()) else {
        bui.add_space(8.0);
        bui.label(egui::RichText::new(format!("{} has no grade", short(&it.name, 40))).color(t.text_dim));
        bui.add_space(6.0);
        let r = bui.button("Add Node");
        app.auto.add("grade.addFirst", r.rect, "Add Node");
        if r.clicked()
            && let Some(id) = run(app, "grade.addNode", json!({"clip": clip.0})).and_then(|v| v.get("node").and_then(Value::as_u64))
        {
            select_node(app, clip, id);
        }
        return;
    };
    let Some(node_id) = current_node(app, clip, &grade) else { return };
    let Some(node) = grade.node(node_id).cloned() else { return };
    // a mixer node shows its per-input settings instead of the correction tabs (fork C2b)
    let mixer = super::grade_mixer::is_mixer(node.kind);
    let index = grade.nodes.iter().position(|n| n.id == node_id).unwrap_or(0);
    prefetch_explanation(app, bui.ctx(), clip, node_id);
    let ph = app.session.playhead();
    let mt = it.effect_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
    let mut sets: Vec<Value> = Vec::new();
    let mut autos: Vec<(String, Rect, String)> = Vec::new();
    let gest = gesture(&bui);
    let key_of = |what: &str| format!("{node_id}:{what}#{gest}");
    let split = middle
        || app.ui.dock.contains(crate::dock::PanelKind::ColorCurves)
        || app.ui.floating.iter().any(|g| g.panels.contains(&crate::dock::PanelKind::ColorCurves));
    let shown: Vec<&'static str> = TABS.into_iter().filter(|id| !split || middle == MIDDLE_TABS.contains(id)).collect();
    let stored = if middle { app.ui.grade_tab_mid.as_str() } else { app.ui.grade_tab.as_str() };
    let mut tab: &'static str = shown.iter().copied().find(|id| *id == stored).or_else(|| shown.first().copied()).unwrap_or("primaries");
    let mut cmds: Vec<(&'static str, Value)> = Vec::new();
    let mut arm: Option<bool> = None;
    let armed = app.ui.grade_picker.filter(|a| a.clip == clip.0 && a.node == node_id).map(|a| a.add);
    // the 3D qualifier's stroke tool (fork G4.3)
    let q3_armed = app.ui.qual3d_tool.filter(|a| a.clip == clip.0 && a.node == node_id).map(|a| a.exclude);
    let mut q3_arm: Option<bool> = None;
    let wb_armed = app.ui.wb_picker.is_some_and(|w| w.clip == clip.0 && w.node == Some(node_id));
    let wb_toggle = std::cell::Cell::new(false);

    bui.horizontal(|ui| {
        let labels =
            [tl!("Primaries"), tl!("HDR"), tl!("Curves"), tl!("Warper"), tl!("Qualifier"), tl!("Windows"), tl!("Magic Mask"), tl!("Noise"), tl!("Match")];
        for (id, label) in TABS.into_iter().zip(labels) {
            if !shown.contains(&id) || mixer {
                continue;
            }
            let r = ui.selectable_label(tab == id, label);
            autos.push((format!("grade.tab.{id}"), r.rect, label.to_string()));
            if r.clicked() {
                tab = id;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let name = if node.label.is_empty() { String::new() } else { format!(" · {}", short(&node.label, 24)) };
            ui.label(egui::RichText::new(format!("Node {:02}{name}", index + 1)).color(t.text_dim));
        });
    });
    bui.add_space(4.0);
    let base = json!({"clip": clip.0, "node": node_id});
    let set = |key: &str, value: Value, merge: Option<String>| {
        let mut p = base.clone();
        p["param"] = json!(key);
        p["value"] = value;
        if let Some(m) = merge {
            p["merge"] = json!(m);
        }
        p
    };

    // the controls scroll when the panel is short (the Color page puts it below the viewer)
    let mut wview = Some(app_view(&app.session, clip, node_id));
    let mview = if tab == "match" && !mixer { Some(match_view(app, bui.ctx(), clip, &grade)) } else { None };
    let luts = lut_entries(&app.session.project);
    let tnr_ok = tab == "noise" && super::tnr::takes_tnr(&app.session, clip);
    let scroll_rect = Rect::from_min_max(bui.cursor().min, bui.max_rect().max);
    let view_h = scroll_rect.height();
    let mut sui = bui.new_child(egui::UiBuilder::new().max_rect(scroll_rect).id_salt("grade-primaries-scroll"));
    // a Grading Coach step about one of these controls scrolls it into view (once per step)
    let coach_scroll = super::coach::scroll_request(app, bui.ctx());
    let mut coach_scrolled = false;
    let mut pedits: Vec<super::grade_mixer::ParamEdit> = Vec::new();
    // the Magic Mask tab (fork G3.3)
    let mmview = (tab == "magicMask" && !mixer).then(|| super::magic_mask::view(app, clip, &it, &node));
    let mut mm_acts: Vec<super::magic_mask::Act> = Vec::new();
    egui::ScrollArea::vertical().id_salt(("grade-primaries", tab, middle)).auto_shrink([false, false]).show(&mut sui, |bui| {
        if mixer {
            if middle {
                bui.add_space(8.0);
                bui.label(egui::RichText::new(tl!("A mixer node: its settings are in the Primaries panel.")).color(t.text_dim));
            } else {
                super::grade_mixer::mixer_controls(bui, &grade, &node, mt, &t, &mut pedits, &mut autos);
            }
        } else if tab == "primaries" {
            // Resolve's Primaries palette: a strip of numbers (Temp, Tint, Contrast, Pivot) with
            // the white-balance tools, four wheels with their numbers and a master jog bar each,
            // then a strip with Saturation and film grain, and the node's LUT
            let field = |ui: &mut egui::Ui, key: &'static str, label: &str, sets: &mut Vec<Value>, autos: &mut Vec<(String, Rect, String)>| {
                let Some(def) = node_param(key) else { return };
                let NodeParamKind::Float { min, max, .. } = def.kind else { return };
                let mut v = node.float_at(key, mt);
                let lw = ui.fonts_mut(|f| f.layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), t.text_dim).size().x) + 2.0;
                let (lr, l) = ui.allocate_exact_size(vec2(lw, 18.0), Sense::click());
                let l = l.on_hover_text(format!("{} · {}", def.label, tl!("double-click to reset")));
                autos.push((format!("grade.param.{key}.reset"), l.rect, format!("Reset {}", def.label)));
                if l.double_clicked() {
                    sets.push(json!({"reset": key}));
                }
                let r = widgets::color::value_box(ui, &mut v, FIELD_W, min..=max, field_decimals(key), field_band(key));
                // the label, centred on the box's height
                let col = if l.hovered() { t.text } else { t.text_dim };
                ui.painter().text(pos2(lr.max.x, r.rect.center().y), Align2::RIGHT_CENTER, label, egui::FontId::proportional(13.0), col);
                autos.push((format!("grade.param.{key}"), r.rect, def.label.to_string()));
                if r.changed() {
                    sets.push(set(key, json!(v), Some(key_of(key))));
                }
                ui.add_space(FIELD_GAP);
            };
            bui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                // white balance: Auto (from the greys of the frame) and the picker
                let r = round_icon(ui, "A", false, &t).on_hover_text(tl!("Balance Temperature and Tint from the greys in the whole frame"));
                autos.push(("grade.wb.auto".to_string(), r.rect, tl!("Auto balance").to_string()));
                if r.clicked() {
                    cmds.push(("grade.wb.auto", json!({"node": node_id})));
                }
                let r = round_icon(ui, "✒", wb_armed, &t).on_hover_text(tl!(
                    "Click or drag on something that should be grey or white in the Program monitor: Temperature and Tint are set so it becomes neutral (Esc cancels)"
                ));
                autos.push(("grade.wb.pick".to_string(), r.rect, tl!("WB picker").to_string()));
                if r.clicked() {
                    wb_toggle.set(true);
                }
                ui.add_space(FIELD_GAP);
                for (key, label) in [("temperature", tl!("Temp")), ("tint", tl!("Tint")), ("contrast", tl!("Contrast")), ("pivot", tl!("Pivot"))] {
                    field(ui, key, label, &mut sets, &mut autos);
                }
            });
            bui.add_space(6.0);
            let width = bui.available_width();
            let cols = if width >= 4.0 * (WHEEL_MIN + WHEEL_GAP) { 4 } else { 2 };
            let col_w = ((width - WHEEL_GAP * (cols as f32 - 1.0)) / cols as f32).max(WHEEL_MIN);
            let rows = (WHEELS.len() / cols) as f32;
            let by_height = ((view_h - 2.0 * STRIP_H - LUT_H) / rows - WHEEL_CHROME).max(0.0);
            let size = (col_w * 0.78).min(by_height).clamp(WHEEL_MIN, WHEEL_MAX);
            for row in WHEELS.chunks(cols) {
                bui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = WHEEL_GAP;
                    for &(key, title, scale, master_scale) in row {
                        ui.allocate_ui_with_layout(vec2(col_w, size + WHEEL_CHROME), egui::Layout::top_down(egui::Align::Center), |ui| {
                            ui.set_width(col_w);
                            let w = node.wheel_at(key, mt);
                            let range = match node_param(key).map(|d| d.kind) {
                                Some(NodeParamKind::Wheel { range }) => range,
                                _ => 1.0,
                            };
                            // title, centred, with the wheel's reset at its right
                            let (tr, _) = ui.allocate_exact_size(vec2(col_w, 18.0), Sense::hover());
                            ui.painter().text(tr.center(), egui::Align2::CENTER_CENTER, crate::i18n::t(title), egui::FontId::proportional(13.0), t.text);
                            let rr = Rect::from_center_size(pos2((tr.center().x + size / 2.0).min(tr.max.x - 9.0), tr.center().y), vec2(18.0, 18.0));
                            let reset = ui.interact(rr, egui::Id::new(("grade-wheel-reset", clip.0, node_id, key)), Sense::click()).on_hover_text(format!("{} {}", tl!("Reset"), crate::i18n::t(title)));
                            crate::icons::paint(ui.painter(), rr.shrink(3.0), crate::icons::Icon::Undo, if reset.hovered() { t.text } else { t.text_dim });
                            autos.push((format!("grade.wheel.{key}.reset"), rr, format!("Reset {title}")));
                            if reset.clicked() {
                                sets.push(json!({"reset": key}));
                            }
                            let puck = puck_of([w[0], w[1], w[2]], scale);
                            let wheel_rect = Rect::from_min_size(pos2(ui.cursor().min.x + (col_w - size) / 2.0, ui.cursor().min.y), vec2(size, size));
                            let ch = widgets::color::grade_wheel(ui, puck, size);
                            autos.push((format!("grade.wheel.{key}"), wheel_rect, title.to_string()));
                            let mean = (w[0] + w[1] + w[2]) / 3.0;
                            if let Some(p) = ch {
                                let c = rgb_of(p, scale);
                                sets.push(set(key, json!([mean + c[0], mean + c[1], mean + c[2], w[3]]), Some(key_of(key))));
                            }
                            ui.add_space(4.0);
                            // Y (master), R, G, B, centred under the wheel
                            let bw = ((col_w - 3.0 * 4.0) / 4.0).clamp(30.0, 46.0);
                            let row_w = 4.0 * bw + 3.0 * 4.0;
                            ui.allocate_ui_with_layout(vec2(col_w, 22.0), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.add_space(((col_w - row_w) / 2.0).max(0.0) - 4.0);
                                let bands = [Color32::from_gray(0xe0), Color32::from_rgb(0xe0, 0x40, 0x40), Color32::from_rgb(0x40, 0xc8, 0x50), Color32::from_rgb(0x40, 0x70, 0xf0)];
                                for (i, ch) in [3usize, 0, 1, 2].into_iter().enumerate() {
                                    let mut v = w[ch];
                                    let r = widgets::color::value_box(ui, &mut v, bw, -range..=range, 2, widgets::color::Band::Solid(bands[i]));
                                    if r.changed() {
                                        let mut nv = w;
                                        nv[ch] = v;
                                        sets.push(set(key, json!(nv), Some(key_of(key))));
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            let (jr, dx) = widgets::color::jog_bar(ui, (size * 1.15).min(col_w), egui::Id::new(("grade-jog", clip.0, node_id, key)));
                            autos.push((format!("grade.wheel.{key}.master"), jr.rect, format!("{title} master")));
                            if dx.is_nan() {
                                sets.push(set(key, json!([w[0], w[1], w[2], 0.0]), Some(key_of(key))));
                            } else if dx != 0.0 {
                                let m = (w[3] + dx / 400.0 * master_scale).clamp(-range, range);
                                sets.push(set(key, json!([w[0], w[1], w[2], m]), Some(key_of(key))));
                            }
                        });
                    }
                });
                bui.add_space(4.0);
            }
            bui.separator();
            bui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (key, label) in [("saturation", tl!("Saturation")), ("grain.strength", tl!("Grain")), ("grain.size", tl!("Grain size")), ("grain.softness", tl!("Grain softness"))] {
                    field(ui, key, label, &mut sets, &mut autos);
                }
            });
            lut_row(bui, &node, &luts, &t, &mut cmds, &mut autos);
            if !node.lut.is_empty() {
                let mut edits = Vec::new();
                float_rows(bui, &node, mt, &t, &["lut.mix"], true, &mut edits, &mut autos);
                for (key, v) in edits {
                    sets.push(match v {
                        Some(v) => set(key, v, Some(key_of(key))),
                        None => json!({"reset": key}),
                    });
                }
            }
        } else if tab == "hdr" {
            // the HDR zone wheels (fork G4.2)
            let mut edits = Vec::new();
            super::grade_hdr::controls(bui, &node, mt, &t, &mut edits, &mut autos);
            for e in edits {
                match e {
                    super::grade_hdr::HdrEdit::Set { key, value } => sets.push(set(&key, value, Some(key_of(&key)))),
                    super::grade_hdr::HdrEdit::Reset(zone) => cmds.push(("grade.hdr.reset", json!({"node": node_id, "zone": zone}))),
                }
            }
        } else if tab == "match" {
            if let Some(mv) = &mview {
                match_controls(bui, mv, clip, node_id, &t, &mut cmds, &mut autos);
            }
        } else if tab == "warper" {
            // the colour warper (fork G4.1)
            super::grade_warper::controls(bui, &node, mt, &t, gest, &mut cmds, &mut autos);
        } else if tab == "magicMask" {
            if let Some(v) = &mmview {
                super::magic_mask::controls(bui, v, &t, gest, &mut mm_acts, &mut autos);
            }
        } else if tab == "windows" {
            if let Some(wv) = wview.take() {
                window_controls(bui, wv, &node, mt, &t, &mut cmds, &mut autos);
            }
        } else if tab == "noise" {
            let mut edits = Vec::new();
            float_rows(bui, &node, mt, &t, &["nr.luma", "nr.chroma", "nr.size"], true, &mut edits, &mut autos);
            // temporal noise reduction is a clip setting since fork 6.7b (`tnr.*` commands): its
            // block is headed with the clip's name so it doesn't read as a node setting
            if tnr_ok {
                bui.add_space(4.0);
                bui.separator();
                bui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tl!("Temporal NR (clip)")).color(t.text));
                    ui.add(egui::Label::new(egui::RichText::new(short(&it.name, 40)).color(t.text_dim)).truncate());
                });
                super::tnr::tnr_rows(bui, clip, &it, mt, &t, &format!("noise-{gest}"), &mut cmds, &mut autos);
                super::tnr::note(bui, &t);
            }
            for (key, v) in edits {
                sets.push(match v {
                    Some(v) => set(key, v, Some(key_of(key))),
                    None => json!({"reset": key}),
                });
            }
        } else if tab == "qualifier" {
            super::grade_mixer::key_input_controls(bui, &grade, &node, mt, &t, &mut pedits, &mut autos);
            // the key: HSL ranges or the 3D qualifier (fork G4.3)
            super::grade_qual3d::mode_row(bui, &node, &t, &mut cmds, &mut autos);
            let three_d = node.qual3d_active().is_some();
            let mut edits = Vec::new();
            qualifier_controls(bui, &node, mt, &t, (armed, three_d), &mut arm, &mut edits, &mut autos);
            if three_d {
                super::grade_qual3d::mark_shown(bui.ctx(), clip, node_id);
                super::grade_qual3d::controls(bui, &node, mt, &t, gest, q3_armed, &mut q3_arm, &mut cmds, &mut autos);
            }
            for (key, v) in edits {
                sets.push(match v {
                    Some(v) => set(key, v, Some(key_of(key))),
                    None => json!({"reset": key}),
                });
            }
        } else {
            // the Curves panel: the curve on the left, channels and soft clip beside it (Resolve);
            // the Primaries panel's Curves tab: one above the other
            let side = middle && bui.available_width() >= 400.0;
            let curve_w = if side { (view_h - 12.0).clamp(120.0, 300.0).min(bui.available_width() - 260.0) } else { bui.available_width() };
            let mut controls: Vec<Value> = Vec::new();
            let mut curve_part = |ui: &mut egui::Ui, autos: &mut Vec<(String, Rect, String)>| {
                ui.set_max_width(curve_w);
                let key = egui::Id::new("grade-curve-channel");
                let ch: usize = ui.data(|d| d.get_temp(key)).unwrap_or(0);
                let (k, _) = CURVES.get(ch).copied().unwrap_or(CURVES[0]);
                let col = match ch {
                    1 => Color32::from_rgb(0xe0, 0x50, 0x50),
                    2 => Color32::from_rgb(0x50, 0xc0, 0x50),
                    3 => Color32::from_rgb(0x50, 0x80, 0xf0),
                    _ => Color32::from_gray(0xd0),
                };
                let pts = node.curve(k).unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
                if let Some(new_pts) = widgets::color::curve_editor(ui, &pts, false, col, &t, egui::Id::new(("grade-curve", clip.0, node_id, k, middle))) {
                    let v: Vec<Value> = new_pts.iter().map(|q| json!([q[0], q[1]])).collect();
                    controls.push(set(k, json!(v), Some(key_of(k))));
                }
                let _ = autos;
            };
            let channel_part = |ui: &mut egui::Ui, sets: &mut Vec<Value>, autos: &mut Vec<(String, Rect, String)>| {
                let key = egui::Id::new("grade-curve-channel");
                let mut ch: usize = ui.data(|d| d.get_temp(key)).unwrap_or(0);
                ui.horizontal(|ui| {
                    for (i, (k, label)) in CURVES.iter().enumerate() {
                        let r = ui.selectable_label(ch == i, *label);
                        autos.push((format!("grade.curve.{}", k.trim_start_matches("curve.")), r.rect, format!("{label} curve")));
                        if r.clicked() {
                            ch = i;
                        }
                    }
                });
                ui.data_mut(|d| d.insert_temp(key, ch));
                let (k, _) = CURVES.get(ch).copied().unwrap_or(CURVES[0]);
                let r = ui.small_button("Reset curve");
                autos.push(("grade.curve.reset".into(), r.rect, "Reset curve".into()));
                if r.clicked() {
                    sets.push(json!({"reset": k}));
                }
            };
            let soft_clip_part = |ui: &mut egui::Ui, sets: &mut Vec<Value>, autos: &mut Vec<(String, Rect, String)>| {
                // highlight soft clip (G2.5), like the soft clip beside Resolve's curves
                ui.separator();
                ui.label(egui::RichText::new(tl!("Soft clip")).color(t.text_dim));
                let mut edits = Vec::new();
                float_rows(ui, &node, mt, &t, &["hl.rolloff", "hl.knee"], true, &mut edits, autos);
                for (key, v) in edits {
                    sets.push(match v {
                        Some(v) => set(key, v, Some(key_of(key))),
                        None => json!({"reset": key}),
                    });
                }
            };
            if side {
                bui.horizontal_top(|ui| {
                    ui.vertical(|ui| curve_part(ui, &mut autos));
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        channel_part(ui, &mut sets, &mut autos);
                        soft_clip_part(ui, &mut sets, &mut autos);
                    });
                });
            } else {
                channel_part(bui, &mut sets, &mut autos);
                curve_part(bui, &mut autos);
                soft_clip_part(bui, &mut sets, &mut autos);
            }
            sets.extend(controls);
        }
        if let Some(want) = &coach_scroll
            && let Some((_, r, _)) = autos.iter().find(|(id, ..)| want.contains(id))
        {
            bui.scroll_to_rect(*r, None);
            coach_scrolled = true;
        }
    });
    if coach_scrolled {
        super::coach::scrolled(app, bui.ctx());
    }
    for e in pedits {
        match e {
            super::grade_mixer::ParamEdit::Set { key, value, drag } => sets.push(set(key, value, drag.then(|| key_of(key)))),
            super::grade_mixer::ParamEdit::Reset(key) => sets.push(json!({"reset": key})),
            super::grade_mixer::ParamEdit::ToggleAnimation(key) => cmds.push(("grade.toggleAnimation", json!({"node": node_id, "param": key}))),
        }
    }

    for (id, r, label) in autos {
        if r.is_positive() {
            app.auto.add(&id, r, &label);
        }
    }
    if middle {
        app.ui.grade_tab_mid = tab.to_string();
    } else {
        app.ui.grade_tab = tab.to_string();
    }
    // each picker follows the panel that has its tab
    if shown.contains(&"magicMask") {
        super::magic_mask::tool_state(app, clip, node_id, tab);
    }
    super::magic_mask::apply(app, clip, node_id, mm_acts);
    if shown.contains(&"qualifier") {
        qualifier_picker_state(app, clip, node_id, tab, arm, armed, node.qual3d_active().is_some());
        super::grade_qual3d::tool_state(app, clip, &node, tab, q3_arm);
    }
    if shown.contains(&"primaries") {
        wb_picker_state(app, clip, node_id, tab, wb_toggle.get(), wb_armed);
    }
    for (cmd, mut p) in cmds {
        // (the Match tab's results: kept per clip until the next request, shown in the tab)
        if let Some(kind) = p.get("show").and_then(Value::as_str).map(str::to_string) {
            if let Some(o) = p.as_object_mut() {
                o.remove("show");
            }
            if p.get("clip").is_none() {
                p["clip"] = json!(clip.0);
            }
            let overlay = (kind == "overlay").then(|| std::env::temp_dir().join(format!("filmcraft-change-{}-{}.png", std::process::id(), clip.0)));
            if let Some(path) = &overlay {
                p["overlay"] = json!(path.to_string_lossy());
            }
            let (text, ai) = match app.session.execute(cmd, p) {
                Ok(v) => (result_text(&kind, &v), if cmd == "grade.explainNode" { AiView::from_json(&v) } else { None }),
                Err(e) => (e.to_string(), None),
            };
            let tex = overlay.and_then(|path| load_overlay(ui.ctx(), &path, clip));
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("grade-match-result", clip.0)), MatchResult { text, overlay: tex, ai }));
            continue;
        }
        if p.get("browse").is_some() {
            // the LUT is set when the dialog answers (#260: it doesn't stop the frame loop)
            let clip = clip.0;
            app.pick_ui(crate::native_dialogs::FileDialog::open_file("LUT", &["cube", "3dl"]), move |app, paths| {
                if let Some(path) = paths.into_iter().next() {
                    run(app, cmd, json!({"node": node_id, "path": path, "clip": clip}));
                }
            });
            continue;
        }
        if p.get("clip").is_none() {
            p["clip"] = json!(clip.0);
        }
        if cmd == "grade.wb.auto" {
            app.ui.status = super::monitor::wb_status(app.session.execute(cmd, p));
            continue;
        }
        run(app, cmd, p);
    }
    for p in sets {
        if let Some(k) = p.get("reset").and_then(Value::as_str) {
            run(app, "grade.resetNode", json!({"clip": clip.0, "node": node_id, "param": k}));
        } else {
            run(app, "grade.setParam", p);
        }
    }
}

/// After a pass of the panel with the Qualifier tab: keep the qualifier picker in step with what
/// was clicked (one armed picker at a time; leaving the tab disarms it). With the node's 3D key on
/// (`three_d`, fork G4.3) the HSL picker is hidden, so it is put down (a click must not rewrite
/// the ranges).
fn qualifier_picker_state(app: &mut FilmcraftApp, clip: ClipId, node_id: u64, tab: &str, arm: Option<bool>, armed: Option<bool>, three_d: bool) {
    if three_d {
        if armed.is_some() {
            app.ui.grade_picker = None;
        }
        return;
    }
    match arm {
        Some(add) if armed == Some(add) => app.ui.grade_picker = None,
        Some(add) => {
            app.ui.grade_picker = Some(crate::state::GradePicker { clip: clip.0, node: node_id, add });
            // (one armed picker at a time: a click fills one thing)
            app.ui.eyedropper = None;
            app.ui.wb_picker = None;
            app.ui.magic_mask.tool = None;
            app.ui.qual3d_tool = None;
        }
        None if tab != "qualifier" || armed.is_none() => app.ui.grade_picker = None,
        None => {}
    }
}

/// After a pass of the panel with the Primaries tab: the white-balance picker, likewise.
fn wb_picker_state(app: &mut FilmcraftApp, clip: ClipId, node_id: u64, tab: &str, wb_toggle: bool, wb_armed: bool) {
    if wb_toggle {
        app.ui.wb_picker = if wb_armed { None } else { Some(crate::state::WbPicker { clip: clip.0, node: Some(node_id) }) };
        if app.ui.wb_picker.is_some() {
            // (one armed picker at a time: a click fills one thing)
            app.ui.grade_picker = None;
            app.ui.magic_mask.tool = None;
            app.ui.eyedropper = None;
            app.ui.qual3d_tool = None;
        }
    } else if (wb_armed && tab != "primaries") || app.ui.wb_picker.is_some_and(|w| w.clip == clip.0 && w.node.is_some_and(|n| n != node_id)) {
        // (another tab, or another node selected while armed)
        app.ui.wb_picker = None;
    }
}

/// The Qualifier tab (G2.1): on / invert, the picker, the HSL ranges, softness, matte blur and
/// the highlight view; with the 3D key (`armed.1`, fork G4.3) only on / invert, the view and the
/// matte blur (`grade_qual3d::controls` has the rest). Edits go to `edits` as (key, value), a
/// reset as (key, None).
#[allow(clippy::too_many_arguments)]
fn qualifier_controls(
    ui: &mut egui::Ui,
    node: &GradeNode,
    mt: Tick,
    t: &crate::theme::Tokens,
    (armed, three_d): (Option<bool>, bool),
    arm: &mut Option<bool>,
    edits: &mut Vec<(&'static str, Option<Value>)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let on = node.float_at("qual.on", mt) >= 0.5;
    ui.horizontal(|ui| {
        let mut v = on;
        let r = ui.checkbox(&mut v, tl!("Qualifier on"));
        autos.push(("grade.qual.on".into(), r.rect, tl!("Qualifier on").to_string()));
        if r.changed() {
            edits.push(("qual.on", Some(json!(if v { 1.0 } else { 0.0 }))));
        }
        let mut inv = node.float_at("qual.invert", mt) >= 0.5;
        let r = ui.checkbox(&mut inv, tl!("Invert"));
        autos.push(("grade.qual.invert".into(), r.rect, tl!("Invert").to_string()));
        if r.changed() {
            edits.push(("qual.invert", Some(json!(if inv { 1.0 } else { 0.0 }))));
        }
    });
    ui.horizontal(|ui| {
        if three_d {
            return;
        }
        for (add, id, label, tip) in [
            (false, "grade.qual.pick", tl!("Pick"), tl!("Click the picture to select that colour")),
            (true, "grade.qual.add", tl!("Add"), tl!("Click the picture to widen the selection to that colour")),
        ] {
            let r = ui.selectable_label(armed == Some(add), label).on_hover_text(tip);
            autos.push((id.into(), r.rect, label.to_string()));
            if r.clicked() {
                *arm = Some(add);
            }
        }
        if armed.is_some() {
            ui.label(egui::RichText::new(tl!("click the Program picture · Esc cancels")).color(t.text_faint));
        }
    });
    let show = node.float_at("qual.show", mt).round() as i32;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("View")).color(t.text_dim));
        for (v, label) in [(0, tl!("Normal")), (1, tl!("Selection")), (2, tl!("Matte"))] {
            let r = ui.selectable_label(show == v, label);
            autos.push((format!("grade.qual.show.{v}"), r.rect, label.to_string()));
            if r.clicked() {
                edits.push(("qual.show", Some(json!(v))));
            }
        }
    });
    ui.separator();
    let keys: &[&str] = if three_d {
        &["qual.blur"]
    } else {
        &["qual.hue", "qual.hueWidth", "qual.satLow", "qual.satHigh", "qual.lumLow", "qual.lumHigh", "qual.soft", "qual.blur"]
    };
    float_rows(ui, node, mt, t, keys, on, edits, autos);
}

/// Slider rows for number parameters (`enabled`: editable), with a reset button each. Edits go
/// to `edits` as (key, value), a reset as (key, None).
#[allow(clippy::too_many_arguments)]
fn float_rows(
    ui: &mut egui::Ui,
    node: &GradeNode,
    mt: Tick,
    t: &crate::theme::Tokens,
    keys: &[&str],
    enabled: bool,
    edits: &mut Vec<(&'static str, Option<Value>)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    for key in keys {
        let Some(def) = node_param(key) else { continue };
        let NodeParamKind::Float { min, max, .. } = def.kind else { continue };
        let (id, reset_id) = (format!("grade.param.{key}"), format!("grade.param.{key}.reset"));
        let row = FloatRow {
            label: crate::i18n::t(def.label),
            value: node.float_at(key, mt),
            min,
            max,
            step: 0.0,
            suffix: "",
            enabled,
            animated: None,
            id: &id,
            reset_id: &reset_id,
            anim_id: "",
        };
        match float_row(ui, t, row, autos) {
            Some(RowEdit::Set(v)) => edits.push((def.key, Some(json!(v)))),
            Some(RowEdit::Reset) => edits.push((def.key, None)),
            Some(RowEdit::ToggleAnimation) | None => {}
        }
    }
}

/// One number row of the grading panels: label, slider, keyframe toggle (when `animated` is set)
/// and reset button. Shared by the grade nodes' rows and the clip's temporal NR rows
/// (`panels/tnr.rs`), so both behave alike.
pub(crate) struct FloatRow<'a> {
    pub label: &'a str,
    pub value: f32,
    pub min: f32,
    pub max: f32,
    /// Slider step (0: continuous).
    pub step: f64,
    /// Unit shown after the slider's number (e.g. " %"), or "".
    pub suffix: &'a str,
    /// Whether the slider is editable.
    pub enabled: bool,
    /// `Some(animated)`: show the keyframe toggle, lit while the value is animated.
    pub animated: Option<bool>,
    /// Automation ids of the slider, the reset button and the keyframe toggle.
    pub id: &'a str,
    pub reset_id: &'a str,
    pub anim_id: &'a str,
}

/// What a [`float_row`] asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RowEdit {
    Set(f32),
    Reset,
    ToggleAnimation,
}

pub(crate) fn float_row(ui: &mut egui::Ui, t: &crate::theme::Tokens, row: FloatRow<'_>, autos: &mut Vec<(String, Rect, String)>) -> Option<RowEdit> {
    let mut out = None;
    let (lo, hi) = if row.min <= row.max { (row.min, row.max) } else { (row.max, row.min) };
    let mut v = if row.value.is_finite() { row.value.clamp(lo, hi) } else { lo };
    ui.horizontal(|ui| {
        ui.add_sized(vec2(104.0, 18.0), egui::Label::new(egui::RichText::new(row.label).color(t.text_dim)));
        let r = ui.add_enabled(row.enabled, egui::Slider::new(&mut v, lo..=hi).max_decimals(3).step_by(row.step).suffix(row.suffix));
        autos.push((row.id.to_string(), r.rect, row.label.to_string()));
        if r.changed() {
            out = Some(RowEdit::Set(v));
        }
        if let Some(animated) = row.animated {
            let (kr, kresp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
            let kresp = kresp.on_hover_text(tl!("Toggle animation"));
            let col = if animated {
                t.accent
            } else if kresp.hovered() {
                t.text
            } else {
                t.text_dim
            };
            crate::icons::paint(ui.painter(), kr.shrink(1.0), crate::icons::Icon::Keyframe, col);
            autos.push((row.anim_id.to_string(), kr, tl!("Toggle animation").to_string()));
            if kresp.clicked() {
                out = Some(RowEdit::ToggleAnimation);
            }
        }
        let rs = ui.small_button("↺");
        autos.push((row.reset_id.to_string(), rs.rect, format!("Reset {}", row.label)));
        if rs.clicked() {
            out = Some(RowEdit::Reset);
        }
    });
    out
}

/// The LUT menu's entries: None, the built-in LUTs, the project's LUT library (reference, name).
fn lut_entries(project: &filmcraft_project::Project) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![(String::new(), tl!("None").into())];
    v.extend(filmcraft_render::luts::builtins().iter().map(|b| (format!("builtin:{}", b.id), b.label.clone())));
    v.extend(project.luts.iter().map(|l| (format!("lib:{}", l.id), l.name.clone())));
    v
}

/// The node's LUT menu (G2.6): pick a reference (`grade.setLut`) or Browse… for a file.
fn lut_row(
    ui: &mut egui::Ui,
    node: &GradeNode,
    entries: &[(String, String)],
    t: &crate::theme::Tokens,
    cmds: &mut Vec<(&'static str, Value)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let shown = entries.iter().find(|(r, _)| *r == node.lut).map_or_else(|| filmcraft_render::luts::label(None, &node.lut), |(_, n)| n.clone());
    ui.horizontal(|ui| {
        ui.add_sized(vec2(84.0, 18.0), egui::Label::new(egui::RichText::new(tl!("LUT")).color(t.text_dim)));
        let r = egui::ComboBox::from_id_salt(("grade-lut", node.id)).selected_text(shown).truncate().width((ui.available_width() - 20.0).max(80.0)).show_ui(
            ui,
            |ui| {
                for (r, name) in entries {
                    if ui.selectable_label(*r == node.lut, name).clicked() && *r != node.lut {
                        cmds.push(("grade.setLut", json!({"node": node.id, "lut": r})));
                    }
                }
                ui.separator();
                if ui.selectable_label(false, tl!("Browse…")).clicked() {
                    cmds.push(("grade.setLut", json!({"node": node.id, "browse": true})));
                }
            },
        );
        autos.push(("grade.lut".into(), r.response.rect, tl!("LUT").to_string()));
    });
}

/// The Match tab's view of the sequence: the clips that could be a reference, and the clips
/// whose grade is identical to this one (`grade_match::same_grade`).
struct MatchView {
    others: Vec<(u64, String)>,
    same: Vec<String>,
    reference: Option<u64>,
    /// Gallery stills (grabbed frames and imported reference pictures, REF.3) and the one chosen
    /// as the reference instead of a clip.
    stills: Vec<(u64, String)>,
    reference_still: Option<u64>,
    result: Option<MatchResult>,
}

#[derive(Clone)]
struct MatchResult {
    text: String,
    overlay: Option<egui::TextureHandle>,
    /// The node explainer's AI state (G2.11), None for the other results.
    ai: Option<AiView>,
}

/// Where an explanation came from: a checked local-AI text, the template (with the reason when
/// the model was asked and its text couldn't be used), or still being written (`key`).
#[derive(Clone, Debug, PartialEq)]
struct AiView {
    key: Option<String>,
    source: String,
    reason: Option<String>,
    attempted: bool,
}

impl AiView {
    fn from_outcome(o: &filmcraft_engine::ai_explain::Outcome) -> Self {
        AiView { key: None, source: o.source.to_string(), reason: o.reason.clone(), attempted: o.code.is_some_and(|c| c.attempted()) }
    }

    /// From `grade.explainNode`'s answer.
    fn from_json(v: &Value) -> Option<Self> {
        let source = v["source"].as_str()?.to_string();
        let attempted = matches!(v["reasonCode"].as_str(), Some("rejected" | "timeout" | "error"));
        Some(AiView { key: v["aiKey"].as_str().map(str::to_string), source, reason: v["reason"].as_str().map(str::to_string), attempted })
    }
}

/// AI1.3: once the user has left a node alone for [`PREFETCH_AFTER`] (not while playing), start
/// its AI explanation in the background so **Explain node** is answered from the cache. Once per
/// (edit, clip, node); the engine keeps only the newest pre-generation.
const PREFETCH_AFTER: f64 = 0.6;

fn prefetch_explanation(app: &mut FilmcraftApp, ctx: &egui::Context, clip: ClipId, node_id: u64) {
    // (the full readiness check stats the model files: only when the debounce fires)
    if app.playback.playing || !app.session.prefs.color.ai.enabled {
        return;
    }
    let id = egui::Id::new("grade-ai-prefetch");
    let sig = (app.session.revision, clip.0, node_id);
    let now = ctx.input(|i| i.time);
    match ctx.data(|d| d.get_temp::<((u64, u64, u64), f64, bool)>(id)) {
        Some((s, _, true)) if s == sig => {}
        Some((s, since, false)) if s == sig => {
            let left = PREFETCH_AFTER - (now - since);
            if left > 0.0 {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
            } else {
                ctx.data_mut(|d| d.insert_temp(id, (sig, since, true)));
                if !app.session.ai.can_prefetch(&app.session.prefs.color.ai) {
                    return;
                }
                if let Err(e) = app.session.execute("grade.explainNode", json!({"clip": clip.0, "node": node_id, "prefetch": true})) {
                    log::debug!("explanation prefetch: {e}");
                }
            }
        }
        _ => {
            ctx.data_mut(|d| d.insert_temp(id, (sig, now, false)));
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(PREFETCH_AFTER));
        }
    }
}

/// Replace a pending explanation with the model's answer once it is ready (polled each frame
/// while the Match tab shows it; never blocks).
fn poll_ai(app: &FilmcraftApp, ctx: &egui::Context, clip: ClipId, result: &mut MatchResult) {
    let Some(key) = result.ai.as_ref().and_then(|a| a.key.clone()) else { return };
    match app.session.ai.poll(&key) {
        Some(filmcraft_engine::ai_explain::Answer::Ready(o)) => {
            result.text = o.text.clone();
            result.ai = Some(AiView::from_outcome(&o));
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(("grade-match-result", clip.0)), result.clone()));
        }
        Some(filmcraft_engine::ai_explain::Answer::Pending { partial, .. }) => {
            // (the model's checked sentences so far, as they come)
            if let Some(p) = partial
                && p != result.text
            {
                result.text = p;
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(("grade-match-result", clip.0)), result.clone()));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        None => {
            if let Some(a) = result.ai.as_mut() {
                a.key = None;
            }
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(("grade-match-result", clip.0)), result.clone()));
        }
    }
}

fn match_view(app: &FilmcraftApp, ctx: &egui::Context, clip: ClipId, grade: &Grade) -> MatchView {
    // (recomputed only after an edit: the grades are hashed, per clip in the sequence)
    let cache_id = egui::Id::new(("grade-match-view", clip.0));
    let rev = app.session.revision;
    let cached = ctx.data(|d| d.get_temp::<(u64, Vec<(u64, String)>, Vec<String>)>(cache_id)).filter(|c| c.0 == rev);
    let (others, same) = match cached {
        Some((_, o, s)) => (o, s),
        None => {
            let mut others = Vec::new();
            let mut same = Vec::new();
            if let Some(q) = app.session.active_sequence() {
                for it in q.video_tracks.iter().flat_map(|t| &t.items).filter(|it| it.id != clip) {
                    others.push((it.id.0, short(&it.name, 32)));
                    if it.grade.as_ref().is_some_and(|g| !g.nodes.is_empty() && filmcraft_render::grade_match::same_grade(g, grade)) {
                        same.push(short(&it.name, 24));
                    }
                }
            }
            ctx.data_mut(|d| d.insert_temp(cache_id, (rev, others.clone(), same.clone())));
            (others, same)
        }
    };
    let reference = ctx.data(|d| d.get_temp::<u64>(egui::Id::new(("grade-match-ref", clip.0)))).filter(|r| others.iter().any(|(id, _)| id == r));
    let mut result = ctx.data(|d| d.get_temp::<MatchResult>(egui::Id::new(("grade-match-result", clip.0))));
    if let Some(r) = result.as_mut() {
        poll_ai(app, ctx, clip, r);
    }
    let stills: Vec<(u64, String)> = app
        .session
        .project
        .stills
        .iter()
        .map(|st| {
            let name = if st.label.is_empty() { format!("{} {}", tl!("Still"), st.id) } else { st.label.clone() };
            (st.id, format!("{}: {name}", if st.is_reference() { tl!("Reference picture") } else { tl!("Still") }))
        })
        .collect();
    let reference_still = ctx.data(|d| d.get_temp::<u64>(egui::Id::new(("grade-match-ref-still", clip.0)))).filter(|r| stills.iter().any(|(id, _)| id == r));
    let reference = if reference_still.is_some() { None } else { reference };
    MatchView { others, same, reference, stills, reference_still, result }
}

/// The Match tab (G2.8–G2.10): the "same grade" check, Compare shots / Shot Match against a
/// reference clip, the scene check over the selected clips, and the node explainer with its
/// what-changed overlay. Commands go to `cmds` with a `show` key naming how to show the result.
fn match_controls(
    ui: &mut egui::Ui,
    v: &MatchView,
    clip: ClipId,
    node: u64,
    t: &crate::theme::Tokens,
    cmds: &mut Vec<(&'static str, Value)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let text =
        if v.same.is_empty() { tl!("No other clip has exactly this grade.").to_string() } else { format!("{} {}", tl!("Same grade as:"), v.same.join(", ")) };
    ui.label(egui::RichText::new(text).color(t.text_dim));
    ui.separator();
    let mut reference = v.reference;
    let mut reference_still = v.reference_still;
    ui.horizontal(|ui| {
        ui.add_sized(vec2(84.0, 18.0), egui::Label::new(egui::RichText::new(tl!("Reference")).color(t.text_dim)));
        let shown = match reference_still {
            Some(r) => v.stills.iter().find(|(id, _)| *id == r).map(|(_, n)| n.clone()),
            None => reference.and_then(|r| v.others.iter().find(|(id, _)| *id == r)).map(|(_, n)| n.clone()),
        }
        .unwrap_or_else(|| tl!("(pick a clip)").to_string());
        let r = egui::ComboBox::from_id_salt(("grade-match-ref", clip.0))
            .selected_text(shown)
            .truncate()
            .width((ui.available_width() - 10.0).max(80.0))
            .show_ui(ui, |ui| {
                for (id, name) in &v.others {
                    if ui.selectable_label(reference_still.is_none() && reference == Some(*id), name).clicked() {
                        reference = Some(*id);
                        reference_still = None;
                    }
                }
                if !v.stills.is_empty() {
                    ui.separator();
                }
                for (id, name) in &v.stills {
                    if ui.selectable_label(reference_still == Some(*id), name).clicked() {
                        reference_still = Some(*id);
                    }
                }
            });
        autos.push(("grade.match.reference".into(), r.response.rect, tl!("Reference").to_string()));
    });
    if reference != v.reference
        && let Some(r) = reference
    {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("grade-match-ref", clip.0)), r));
    }
    if reference_still != v.reference_still {
        let key = egui::Id::new(("grade-match-ref-still", clip.0));
        ui.ctx().data_mut(|d| {
            if let Some(r) = reference_still {
                d.insert_temp(key, r);
            } else {
                d.remove::<u64>(key);
            }
        });
    }
    ui.horizontal_wrapped(|ui| {
        let with = match reference_still {
            Some(st) => Some(json!({"clip": clip.0, "still": st})),
            None => reference.map(|r| json!({"clip": clip.0, "with": r})),
        };
        for (id, label, cmd, show) in [
            ("grade.match.compare", tl!("Compare shots"), "grade.compareShots", "compare"),
            ("grade.match.shotMatch", tl!("Shot Match"), "grade.shotMatch", "match"),
        ] {
            let r = ui.add_enabled(with.is_some(), egui::Button::new(label));
            autos.push((id.into(), r.rect, label.to_string()));
            if r.clicked()
                && let Some(mut p) = with.clone()
            {
                p["show"] = json!(show);
                cmds.push((cmd, p));
            }
        }
        let r = ui.button(tl!("Check scene")).on_hover_text(tl!("Compare the selected clips and find the one that differs most"));
        autos.push(("grade.match.checkScene".into(), r.rect, tl!("Check scene").to_string()));
        if r.clicked() {
            cmds.push(("grade.checkScene", json!({"show": "scene"})));
        }
    });
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        for (id, label, cmd, show) in [
            ("grade.match.explainNode", tl!("Explain node"), "grade.explainNode", "explain"),
            ("grade.match.explainGrade", tl!("Explain grade"), "grade.explainGrade", "grade"),
            ("grade.match.changes", tl!("Show what changed"), "grade.explainNode", "overlay"),
        ] {
            let r = ui.button(label);
            autos.push((id.into(), r.rect, label.to_string()));
            if r.clicked() {
                cmds.push((cmd, json!({"clip": clip.0, "node": node, "show": show})));
            }
        }
    });
    if let Some(res) = &v.result {
        ui.separator();
        ui.label(egui::RichText::new(&res.text).color(t.text));
        if let Some(ai) = &res.ai {
            if ai.source == "ai" {
                let label = tl!("checked");
                let r = ui
                    .label(egui::RichText::new(format!("✔ {label}")).small().color(t.accent))
                    .on_hover_text(tl!("Written by the local AI model and checked against the measured facts."));
                autos.push(("grade.match.aiChecked".into(), r.rect, label.to_string()));
            } else if ai.key.is_some() {
                let label = tl!("Writing a plain-language version…");
                let r = ui.label(egui::RichText::new(label).small().color(t.text_faint));
                autos.push(("grade.match.aiPending".into(), r.rect, label.to_string()));
            } else if ai.attempted {
                let label = tl!("plain template");
                let why = ai.reason.clone().unwrap_or_default();
                let r = ui
                    .label(egui::RichText::new(format!("⚠ {label}")).small().color(t.text_faint))
                    .on_hover_text(tlf!("The AI text wasn't shown: {why}. This is the plain template sentence.", why));
                autos.push(("grade.match.aiFallback".into(), r.rect, label.to_string()));
            }
        }
        if let Some(tex) = &res.overlay {
            let size = tex.size_vec2();
            let w = ui.available_width().min(size.x).max(1.0);
            ui.image((tex.id(), vec2(w, w * size.y / size.x.max(1.0))));
        }
    }
}

/// A command's answer as text for the Match tab.
fn result_text(kind: &str, v: &Value) -> String {
    let notes = |v: &Value| v["notes"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")).unwrap_or_default();
    match kind {
        "compare" => {
            let n = notes(v);
            let score = v["score"].as_f64().unwrap_or(0.0);
            if n.is_empty() { format!("{} {score:.0} / 100", tl!("Match score")) } else { format!("{} {score:.0} / 100 · {n}", tl!("Match score")) }
        }
        "match" => tl!("Added a Shot Match node at the end of the grade.").to_string(),
        "scene" => {
            v["shots"].as_array().and_then(|a| a.first()).map(|s| format!("{} {} · {}", tl!("Differs most: clip"), s["clip"], notes(s))).unwrap_or_default()
        }
        "grade" => v["text"].as_str().unwrap_or_default().to_string(),
        // (the node explainer: `text` is the checked AI version or the template sentence)
        _ => v["text"].as_str().or(v["sentence"].as_str()).unwrap_or_default().to_string(),
    }
}

/// The what-changed overlay PNG (written by `grade.explainNode`) as a texture; the file is
/// removed once read.
fn load_overlay(ctx: &egui::Context, path: &std::path::Path, clip: ClipId) -> Option<egui::TextureHandle> {
    let img = image::open(path);
    let _ = std::fs::remove_file(path);
    let img = img.ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], img.as_raw());
    Some(ctx.load_texture(format!("grade-change-{}", clip.0), ci, egui::TextureOptions::LINEAR))
}

/// What the Windows tab needs to know beyond the node: which window is selected and which are
/// being tracked.
struct WindowView {
    clip: ClipId,
    node: u64,
    selected: Option<usize>,
    tracking: Vec<usize>,
    /// Windows being tracked as a plane (fork G4.4) and their jobs.
    planar: Vec<(usize, u64)>,
}

fn app_view(session: &filmcraft_engine::Session, clip: ClipId, node: u64) -> WindowView {
    let selected = session.state.selected_mask.filter(|s| s.clip == clip && s.node == Some(node)).map(|s| s.mask);
    let mine = |t: &filmcraft_engine::masks::MaskSel| t.clip == clip && t.node == Some(node);
    let planar: Vec<(usize, u64)> = session.planar_jobs.iter().filter(|j| mine(&j.target)).map(|j| (j.target.mask, j.job)).collect();
    let mut tracking: Vec<usize> = session.mask_jobs.iter().filter(|j| mine(&j.target)).map(|j| j.target.mask).collect();
    tracking.extend(planar.iter().map(|(m, _)| *m));
    WindowView { clip, node, selected, tracking, planar }
}

/// The Windows tab (G2.2 / G2.3): add a window (`masks.add` with `node`), and per window select
/// (handles on the Program monitor), invert, softness, delete, track backward / forward
/// (`masks.track`) and track as a plane both ways (`masks.trackPlanar`, fork G4.4; clicking
/// again while it runs stops it, ✕ planar clears it). Commands go to `cmds`.
fn window_controls(
    ui: &mut egui::Ui,
    v: WindowView,
    node: &GradeNode,
    mt: Tick,
    t: &crate::theme::Tokens,
    cmds: &mut Vec<(&'static str, Value)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let base = json!({"clip": v.clip.0, "node": v.node});
    let with = |extra: Value| {
        let mut p = base.clone();
        if let (Some(o), Some(x)) = (p.as_object_mut(), extra.as_object()) {
            o.extend(x.clone());
        }
        p
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(tl!("Add window")).color(t.text_dim));
        for (shape, label) in WINDOW_SHAPES.into_iter().zip([tl!("Circle"), tl!("Rectangle"), tl!("Linear")]) {
            let r = ui.button(label);
            autos.push((format!("grade.window.add.{shape}"), r.rect, label.to_string()));
            if r.clicked() {
                cmds.push(("masks.add", with(json!({"shape": shape}))));
            }
        }
    });
    if node.windows.is_empty() {
        ui.label(egui::RichText::new(tl!("No windows: the node acts on the whole picture.")).color(t.text_faint));
        return;
    }
    ui.separator();
    for (i, w) in node.windows.iter().enumerate().take(filmcraft_project::grade::MAX_WINDOWS) {
        ui.horizontal(|ui| {
            let r = ui.selectable_label(v.selected == Some(i), short(&w.name, 20));
            autos.push((format!("grade.window.{i}"), r.rect, w.name.clone()));
            if r.clicked() {
                cmds.push(("masks.select", with(json!({"mask": i}))));
            }
            let mut inv = w.inverted;
            let r = ui.checkbox(&mut inv, tl!("Invert"));
            autos.push((format!("grade.window.{i}.invert"), r.rect, tl!("Invert").to_string()));
            if r.changed() {
                cmds.push(("masks.set", with(json!({"mask": i, "inverted": inv}))));
            }
            let mut soft = w.feather.f64_at(mt).max(0.0) as f32;
            let r = ui.add(egui::DragValue::new(&mut soft).range(0.0..=1000.0).speed(1.0).suffix(" px")).on_hover_text(tl!("Softness"));
            autos.push((format!("grade.window.{i}.softness"), r.rect, tl!("Softness").to_string()));
            if r.changed() {
                cmds.push(("masks.set", with(json!({"mask": i, "feather": soft, "merge": format!("grade-window-{}-{}-{i}", v.clip.0, v.node)}))));
            }
            let tracking = v.tracking.contains(&i);
            for (dir, label, tip) in [("backward", "◀", tl!("Track backward")), ("forward", "▶", tl!("Track forward"))] {
                let r = ui.add_enabled(!tracking, egui::Button::new(label).small()).on_hover_text(tip);
                autos.push((format!("grade.window.{i}.track.{dir}"), r.rect, tip.to_string()));
                if r.clicked() {
                    cmds.push(("masks.track", with(json!({"mask": i, "direction": dir}))));
                }
            }
            // planar track (fork G4.4): both ways from the playhead; a running one stops
            let running = v.planar.iter().find(|(m, _)| *m == i).map(|(_, j)| *j);
            let on = w.planar.is_some();
            let (label, tip) = match running {
                Some(_) => (tl!("Stop"), tl!("Stop planar tracking (keeps what was tracked)")),
                None => (tl!("Planar"), tl!("Track the window as a plane (a sign, a screen, a wall), both ways from the playhead")),
            };
            let r = ui.add_enabled(running.is_some() || !tracking, egui::Button::new(label).small().selected(on && running.is_none())).on_hover_text(tip);
            autos.push((format!("grade.window.{i}.track.planar"), r.rect, tip.to_string()));
            if r.clicked() {
                match running {
                    Some(job) => cmds.push(("jobs.cancel", json!({"job": job}))),
                    None => cmds.push(("masks.trackPlanar", with(json!({"mask": i})))),
                }
            }
            if on && running.is_none() {
                let tip = tl!("Clear the planar track (the window stays where it is seen)");
                let r = ui.small_button(tl!("✕ planar")).on_hover_text(tip);
                autos.push((format!("grade.window.{i}.track.clearPlanar"), r.rect, tip.to_string()));
                if r.clicked() {
                    cmds.push(("masks.clearPlanar", with(json!({"mask": i}))));
                }
            }
            let r = ui.small_button("✕").on_hover_text(tl!("Delete window"));
            autos.push((format!("grade.window.{i}.delete"), r.rect, tl!("Delete window").to_string()));
            if r.clicked() {
                cmds.push(("masks.remove", with(json!({"mask": i}))));
            }
        });
    }
    ui.label(egui::RichText::new(tl!("Select a window to drag its shape on the Program monitor.")).color(t.text_faint));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puck_and_rgb_round_trip() {
        for p in [[0.0, 0.0], [1.0, 0.0], [0.0, -1.0], [0.3, 0.4], [-0.6, 0.2]] {
            let rgb = rgb_of(p, 0.25);
            assert!((rgb.iter().sum::<f32>()).abs() < 1e-6, "zero-sum");
            let back = puck_of(rgb, 0.25);
            assert!((back[0] - p[0]).abs() < 1e-5 && (back[1] - p[1]).abs() < 1e-5, "{p:?} → {back:?}");
        }
        // red is at the wheel's 0° (hue 0)
        let r = rgb_of([1.0, 0.0], 0.3);
        assert!(r[0] > 0.0 && r[1] < 0.0 && r[2] < 0.0);
        // a neutral offset has no puck; scale 0 and huge values stay inside the wheel
        let n = puck_of([0.2, 0.2, 0.2], 0.25);
        assert!(n[0].abs() < 1e-6 && n[1].abs() < 1e-6, "{n:?}");
        assert_eq!(puck_of([1.0, 0.0, 0.0], 0.0), [0.0, 0.0]);
        let a = puck_of([0.3, 0.1, 0.1], 0.25);
        let b = puck_of([0.2, 0.0, 0.0], 0.25);
        assert!((a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6, "the neutral part has no puck");
        assert_eq!(short("abcdef", 4), "abc…");
        assert_eq!(short("abc", 4), "abc");
        let far = puck_of([100.0, -50.0, 0.0], 0.25);
        assert!((far[0] * far[0] + far[1] * far[1]).sqrt() <= 1.0 + 1e-5);
    }
}
