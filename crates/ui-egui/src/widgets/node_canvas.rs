//! Node canvas (fork C1, ADR 0007): a generic node-graph editor. It knows nothing about comps or
//! grades: the caller passes a [`CanvasGraph`] view and turns the returned [`CanvasEvent`]s into
//! commands (`comp.*` now, the grader's nodes in C2). Pan with the middle button (or Space +
//! drag), zoom with the wheel around the pointer, drag nodes, drag from an output port to an input
//! port to connect, click a connected input to disconnect, drag on the background to box-select,
//! right-click the background to add a node, Delete / Ctrl+B on the selection, double-click to
//! view a node. Every node, port, edge and menu item registers an automation id under `ns`.

use egui::epaint::CubicBezierShape;
use egui::{Align2, Color32, FontId, Id, Key, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};

use crate::automation::Registry;

/// One node as the canvas shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeView {
    pub id: u64,
    pub title: String,
    pub pos: [f32; 2],
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub bypassed: bool,
    /// Shown on the node (e.g. "cycle" on the output node).
    pub error: Option<String>,
    pub is_output: bool,
    /// Bit `i` set: input port `i` carries a key (a matte), drawn as a green triangle (Resolve's
    /// key ports); clear: a picture port, a round dot.
    pub key_inputs: u32,
    /// Bit `i` set: output port `i` carries a key.
    pub key_outputs: u32,
}

/// A connection from an output port to an input port, as (node, port).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeView {
    pub from: (u64, u8),
    pub to: (u64, u8),
}

/// What the canvas draws, plus the node kinds the background menu offers as (key, label).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CanvasGraph {
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeView>,
    pub add_kinds: Vec<(String, String)>,
    /// Extra entries of a node's right-click menu as (key, label), after the built-in ones; a
    /// click gives [`CanvasEvent::NodeAction`].
    pub node_actions: Vec<(String, String)>,
    /// Extra entries of the background menu as (key, label), after the add entries (e.g. the
    /// grade canvas's Reset Layout); a click gives [`CanvasEvent::CanvasAction`]. Automation ids
    /// `<ns>.menu.<key>`.
    pub canvas_actions: Vec<(String, String)>,
    /// Register automation ids only for the nodes, ports and connections on screen (a graph
    /// wider than its panel, like a long serial grade).
    pub on_screen_ids: bool,
}

/// The canvas's view state (serde, so the control channel can read and set it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CanvasState {
    pub pan: [f32; 2],
    pub zoom: f32,
    pub selection: Vec<u64>,
    pub viewed: Option<u64>,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self { pan: [40.0, 40.0], zoom: 1.0, selection: Vec::new(), viewed: None }
    }
}

/// What the user did this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasEvent {
    Select(Vec<u64>),
    /// A node is being dragged to `pos` (canvas units).
    Move(u64, [f32; 2]),
    /// The drag ended at `pos`.
    MoveEnd(u64, [f32; 2]),
    Connect {
        from: (u64, u8),
        to: (u64, u8),
    },
    Disconnect {
        to: (u64, u8),
    },
    AddRequested {
        kind: String,
        pos: [f32; 2],
    },
    Bypass(Vec<u64>),
    Delete(Vec<u64>),
    SetOutput(u64),
    View(u64),
    /// An entry of [`CanvasGraph::node_actions`] chosen on a node.
    NodeAction {
        node: u64,
        action: String,
    },
    /// An entry of [`CanvasGraph::canvas_actions`] chosen on the background.
    CanvasAction(String),
}

const NODE_W: f32 = 136.0;
const HEADER: f32 = 22.0;
const ROW: f32 = 16.0;
const PORT_R: f32 = 5.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 3.0;

fn node_size(n: &NodeView) -> Vec2 {
    let rows = n.inputs.len().max(n.outputs.len()).max(1) as f32;
    vec2(NODE_W, HEADER + ROW * rows + 6.0)
}

/// Whether bit `i` of a port mask is set.
fn is_key(mask: u32, i: usize) -> bool {
    u32::try_from(i).ok().and_then(|i| mask.checked_shr(i)).is_some_and(|m| m & 1 == 1)
}

/// The colour of key ports and key connections.
const KEY_GREEN: Color32 = Color32::from_rgb(0x5c, 0xc0, 0x5a);

fn finite_or(v: f32, d: f32) -> f32 {
    if v.is_finite() { v } else { d }
}

/// Canvas units ↔ screen points.
#[derive(Clone, Copy)]
struct View {
    origin: Pos2,
    pan: Vec2,
    zoom: f32,
}

impl View {
    fn screen(&self, p: [f32; 2]) -> Pos2 {
        self.origin + (self.pan + vec2(finite_or(p[0], 0.0), finite_or(p[1], 0.0))) * self.zoom
    }
    fn canvas(&self, s: Pos2) -> [f32; 2] {
        let v = (s - self.origin) / self.zoom - self.pan;
        [v.x, v.y]
    }
    fn node_rect(&self, n: &NodeView) -> Rect {
        Rect::from_min_size(self.screen(n.pos), node_size(n) * self.zoom)
    }
    fn port(&self, n: &NodeView, input: bool, i: usize) -> Pos2 {
        let r = self.node_rect(n);
        let y = r.min.y + (HEADER + ROW * (i as f32 + 0.5)) * self.zoom;
        pos2(if input { r.min.x } else { r.max.x }, y)
    }
}

/// Zooms (at most 1:1, at least the smallest zoom) and pans `state` so the whole graph fits a
/// canvas of `size` points with a small margin.
pub fn fit(state: &mut CanvasState, graph: &CanvasGraph, size: Vec2) {
    const MARGIN: f32 = 20.0;
    let (mut min, mut max) = (vec2(f32::MAX, f32::MAX), vec2(f32::MIN, f32::MIN));
    for n in &graph.nodes {
        let p = vec2(finite_or(n.pos[0], 0.0), finite_or(n.pos[1], 0.0));
        min = min.min(p);
        max = max.max(p + node_size(n));
    }
    if graph.nodes.is_empty() {
        *state = CanvasState { selection: std::mem::take(&mut state.selection), viewed: state.viewed, ..Default::default() };
        return;
    }
    let span = max - min;
    let avail = size - Vec2::splat(MARGIN * 2.0);
    let zoom = (avail.x / span.x.max(1.0)).min(avail.y / span.y.max(1.0));
    state.zoom = finite_or(zoom, 1.0).clamp(MIN_ZOOM, 1.0);
    state.pan = [finite_or(MARGIN / state.zoom - min.x, 0.0), finite_or(MARGIN / state.zoom - min.y, 0.0)];
}

/// Draws the graph and returns what the user did. `ns` prefixes every automation id
/// (`<ns>.canvas`, `<ns>.node.<id>`, `<ns>.port.<id>.in.<n>` / `.out.<n>`, `<ns>.edge.<from>-<to>.<port>`,
/// `<ns>.add.<kind>`).
pub fn node_canvas(ui: &mut Ui, graph: &CanvasGraph, state: &mut CanvasState, reg: &mut Registry, ns: &str) -> Vec<CanvasEvent> {
    let mut events = Vec::new();
    state.zoom = finite_or(state.zoom, 1.0).clamp(MIN_ZOOM, MAX_ZOOM);
    state.pan = [finite_or(state.pan[0], 0.0), finite_or(state.pan[1], 0.0)];
    let rect = ui.available_rect_before_wrap();
    let bg = ui.allocate_rect(rect, Sense::click_and_drag());
    reg.add_response(&format!("{ns}.canvas"), &bg, "Node canvas");
    let base = Id::new(("node_canvas", ns));
    let space = ui.input(|i| i.key_down(Key::Space));

    // pan and zoom
    if bg.dragged_by(PointerButton::Middle) || (space && bg.dragged_by(PointerButton::Primary)) {
        let d = bg.drag_delta() / state.zoom;
        state.pan = [state.pan[0] + d.x, state.pan[1] + d.y];
    }
    if let Some(p) = bg.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let before = View { origin: rect.min, pan: Vec2::from(state.pan), zoom: state.zoom }.canvas(p);
            state.zoom = (state.zoom * (scroll * 0.002).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
            let pan = (p - rect.min) / state.zoom - Vec2::from(before);
            state.pan = [pan.x, pan.y];
        }
    }
    let view = View { origin: rect.min, pan: Vec2::from(state.pan), zoom: state.zoom };
    let painter = ui.painter_at(rect);
    let vis = ui.visuals().clone();

    // grid
    painter.rect_filled(rect, 0.0, vis.extreme_bg_color);
    let step = 24.0 * state.zoom;
    if step >= 6.0 {
        let grid = Stroke::new(1.0, vis.faint_bg_color);
        let off = Vec2::from(state.pan) * state.zoom;
        let mut x = rect.min.x + off.x.rem_euclid(step);
        while x < rect.max.x {
            painter.line_segment([pos2(x, rect.min.y), pos2(x, rect.max.y)], grid);
            x += step;
        }
        let mut y = rect.min.y + off.y.rem_euclid(step);
        while y < rect.max.y {
            painter.line_segment([pos2(rect.min.x, y), pos2(rect.max.x, y)], grid);
            y += step;
        }
    }

    let node = |id: u64| graph.nodes.iter().find(|n| n.id == id);
    let edge_stroke = Stroke::new(2.0 * state.zoom.sqrt(), vis.widgets.inactive.fg_stroke.color);

    // edges
    for e in &graph.edges {
        let (Some(a), Some(b)) = (node(e.from.0), node(e.to.0)) else { continue };
        let (p0, p3) = (view.port(a, false, usize::from(e.from.1)), view.port(b, true, usize::from(e.to.1)));
        let dx = ((p3.x - p0.x).abs() * 0.5).max(30.0 * state.zoom);
        let stroke = if is_key(a.key_outputs, usize::from(e.from.1)) { Stroke::new(edge_stroke.width, KEY_GREEN) } else { edge_stroke };
        painter.add(CubicBezierShape::from_points_stroke([p0, p0 + vec2(dx, 0.0), p3 - vec2(dx, 0.0), p3], false, Color32::TRANSPARENT, stroke));
        let mid = pos2((p0.x + p3.x) / 2.0, (p0.y + p3.y) / 2.0);
        if !graph.on_screen_ids || rect.contains(mid) {
            reg.add(&format!("{ns}.edge.{}-{}.{}", e.from.0, e.to.0, e.to.1), Rect::from_center_size(mid, vec2(8.0, 8.0)), "Connection");
        }
    }

    // nodes
    let shift = ui.input(|i| i.modifiers.shift);
    let font = FontId::proportional((12.0 * state.zoom).clamp(6.0, 24.0));
    let small = FontId::proportional((10.0 * state.zoom).clamp(5.0, 20.0));
    // (hit rect, node, is input, port)
    let mut port_hits: Vec<(Rect, u64, bool, u8)> = Vec::new();
    for n in &graph.nodes {
        let r = view.node_rect(n);
        let resp = ui.interact(r, base.with(("node", n.id)), Sense::click_and_drag());
        // (only nodes on screen register)
        if !graph.on_screen_ids || r.intersects(rect) {
            reg.add_response(&format!("{ns}.node.{}", n.id), &resp, &n.title);
        }
        let selected = state.selection.contains(&n.id);
        // dragging: the position accumulates in temp memory so rounding never drifts
        let drag_id = base.with(("drag", n.id));
        if resp.drag_started_by(PointerButton::Primary) && !space {
            if !selected {
                state.selection = vec![n.id];
                events.push(CanvasEvent::Select(state.selection.clone()));
            }
            ui.data_mut(|d| d.insert_temp(drag_id, n.pos));
        }
        if resp.dragged_by(PointerButton::Primary) && !space {
            let start: [f32; 2] = ui.data(|d| d.get_temp(drag_id)).unwrap_or(n.pos);
            let d = resp.drag_delta() / state.zoom;
            let p = [start[0] + d.x, start[1] + d.y];
            ui.data_mut(|m| m.insert_temp(drag_id, p));
            events.push(CanvasEvent::Move(n.id, p));
        }
        if resp.drag_stopped() {
            if let Some(p) = ui.data(|d| d.get_temp::<[f32; 2]>(drag_id)) {
                events.push(CanvasEvent::MoveEnd(n.id, p));
            }
            ui.data_mut(|d| d.remove::<[f32; 2]>(drag_id));
        }
        if resp.double_clicked() {
            state.viewed = Some(n.id);
            events.push(CanvasEvent::View(n.id));
        } else if resp.clicked() {
            if shift {
                if selected {
                    state.selection.retain(|s| *s != n.id);
                } else {
                    state.selection.push(n.id);
                }
            } else {
                state.selection = vec![n.id];
            }
            events.push(CanvasEvent::Select(state.selection.clone()));
        }
        resp.context_menu(|ui| {
            let items = [("bypass", "Bypass"), ("output", "Set as Output"), ("view", "View"), ("delete", "Delete")];
            for (key, label) in items {
                let b = ui.button(crate::i18n::t(label));
                reg.add_response(&format!("{ns}.nodeMenu.{key}"), &b, label);
                if b.clicked() {
                    events.push(match key {
                        "bypass" => CanvasEvent::Bypass(vec![n.id]),
                        "output" => CanvasEvent::SetOutput(n.id),
                        "view" => CanvasEvent::View(n.id),
                        _ => CanvasEvent::Delete(vec![n.id]),
                    });
                    ui.close();
                }
            }
            for (key, label) in &graph.node_actions {
                let b = ui.button(crate::i18n::t(label));
                reg.add_response(&format!("{ns}.nodeMenu.{key}"), &b, label);
                if b.clicked() {
                    events.push(CanvasEvent::NodeAction { node: n.id, action: key.clone() });
                    ui.close();
                }
            }
        });

        // body
        let rounding = 4.0 * state.zoom;
        let fill = if n.bypassed { vis.widgets.noninteractive.bg_fill.gamma_multiply(0.6) } else { vis.widgets.inactive.bg_fill };
        painter.rect_filled(r, rounding, fill);
        let header = Rect::from_min_size(r.min, vec2(r.width(), HEADER * state.zoom));
        painter.rect_filled(header, rounding, if n.is_output { vis.selection.bg_fill } else { vis.widgets.active.bg_fill });
        let outline = if selected {
            Stroke::new(2.0, vis.selection.stroke.color)
        } else if n.error.is_some() {
            Stroke::new(2.0, vis.error_fg_color)
        } else {
            Stroke::new(1.0, vis.widgets.noninteractive.bg_stroke.color)
        };
        painter.rect_stroke(r, rounding, outline, StrokeKind::Inside);
        painter.text(header.left_center() + vec2(6.0 * state.zoom, 0.0), Align2::LEFT_CENTER, &n.title, font.clone(), vis.text_color());
        if let Some(err) = &n.error {
            painter.text(r.center_bottom() + vec2(0.0, 4.0 * state.zoom), Align2::CENTER_TOP, err, small.clone(), vis.error_fg_color);
        }
        for (input, names) in [(true, &n.inputs), (false, &n.outputs)] {
            for (i, name) in names.iter().enumerate() {
                let Ok(port) = u8::try_from(i) else { break };
                let c = view.port(n, input, i);
                let hit = Rect::from_center_size(c, Vec2::splat((PORT_R * 2.0 + 4.0) * state.zoom.max(0.8)));
                port_hits.push((hit, n.id, input, port));
                if is_key(if input { n.key_inputs } else { n.key_outputs }, i) {
                    // a key port: a triangle pointing along the flow
                    let s = PORT_R * state.zoom * 1.2;
                    let pts = vec![c + vec2(-s * 0.8, -s), c + vec2(s, 0.0), c + vec2(-s * 0.8, s)];
                    painter.add(egui::Shape::convex_polygon(pts, KEY_GREEN, Stroke::NONE));
                } else {
                    painter.circle_filled(c, PORT_R * state.zoom, vis.widgets.hovered.fg_stroke.color);
                }
                let (align, dx) = if input { (Align2::LEFT_CENTER, 8.0) } else { (Align2::RIGHT_CENTER, -8.0) };
                painter.text(c + vec2(dx * state.zoom, 0.0), align, name, small.clone(), vis.weak_text_color());
            }
        }
    }

    // ports (after the nodes, so they win the pointer over the node body)
    let pending_id = base.with("pending");
    for &(hit, id, input, port) in &port_hits {
        let pid = base.with(("port", id, input, port));
        let resp = ui.interact(hit, pid, Sense::click_and_drag());
        let dir = if input { "in" } else { "out" };
        if !graph.on_screen_ids || rect.contains(hit.center()) {
            reg.add_response(&format!("{ns}.port.{id}.{dir}.{port}"), &resp, if input { "Input" } else { "Output" });
        }
        if input {
            if resp.clicked() && graph.edges.iter().any(|e| e.to == (id, port)) {
                events.push(CanvasEvent::Disconnect { to: (id, port) });
            }
        } else {
            if resp.drag_started_by(PointerButton::Primary) {
                ui.data_mut(|d| d.insert_temp(pending_id, (id, port)));
            }
            if resp.dragged()
                && let Some(p) = resp.interact_pointer_pos()
            {
                painter.line_segment([hit.center(), p], Stroke::new(2.0, vis.selection.stroke.color));
            }
            if resp.drag_stopped() {
                let from: Option<(u64, u8)> = ui.data(|d| d.get_temp(pending_id));
                ui.data_mut(|d| d.remove::<(u64, u8)>(pending_id));
                let at = ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
                if let (Some(from), Some(at)) = (from, at)
                    && let Some(&(_, to, _, to_port)) = port_hits.iter().find(|(r, _, is_in, _)| *is_in && r.contains(at))
                    && to != from.0
                {
                    events.push(CanvasEvent::Connect { from, to: (to, to_port) });
                }
            }
        }
    }

    // box select on the background
    let box_id = base.with("box");
    if bg.drag_started_by(PointerButton::Primary)
        && !space
        && let Some(p) = bg.interact_pointer_pos()
    {
        ui.data_mut(|d| d.insert_temp(box_id, p));
    }
    if let Some(start) = ui.data(|d| d.get_temp::<Pos2>(box_id)) {
        let now = ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos())).unwrap_or(start);
        let sel = Rect::from_two_pos(start, now);
        painter.rect_stroke(sel, 0.0, Stroke::new(1.0, vis.selection.stroke.color), StrokeKind::Inside);
        if bg.drag_stopped() {
            ui.data_mut(|d| d.remove::<Pos2>(box_id));
            state.selection = graph.nodes.iter().filter(|n| sel.intersects(view.node_rect(n))).map(|n| n.id).collect();
            events.push(CanvasEvent::Select(state.selection.clone()));
        }
    } else if bg.clicked() && !state.selection.is_empty() {
        state.selection.clear();
        events.push(CanvasEvent::Select(Vec::new()));
    }

    // background menu: add a node where the menu was opened
    let menu_at = base.with("menu_at");
    if bg.secondary_clicked()
        && let Some(p) = bg.interact_pointer_pos()
    {
        ui.data_mut(|d| d.insert_temp(menu_at, view.canvas(p)));
    }
    bg.context_menu(|ui| {
        for (key, label) in &graph.add_kinds {
            let b = ui.button(crate::i18n::t(label));
            reg.add_response(&format!("{ns}.add.{key}"), &b, label);
            if b.clicked() {
                let pos = ui.data(|d| d.get_temp(menu_at)).unwrap_or([0.0, 0.0]);
                events.push(CanvasEvent::AddRequested { kind: key.clone(), pos });
                ui.close();
            }
        }
        if !graph.canvas_actions.is_empty() && !graph.add_kinds.is_empty() {
            ui.separator();
        }
        for (key, label) in &graph.canvas_actions {
            let b = ui.button(crate::i18n::t(label));
            reg.add_response(&format!("{ns}.menu.{key}"), &b, label);
            if b.clicked() {
                events.push(CanvasEvent::CanvasAction(key.clone()));
                ui.close();
            }
        }
    });

    // keys over the canvas
    if bg.hovered() || bg.has_focus() || ui.rect_contains_pointer(rect) {
        let (del, bypass) = ui.input(|i| (i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace), i.modifiers.command && i.key_pressed(Key::B)));
        if del && !state.selection.is_empty() {
            events.push(CanvasEvent::Delete(state.selection.clone()));
        }
        if bypass && !state.selection.is_empty() {
            events.push(CanvasEvent::Bypass(state.selection.clone()));
        }
    }
    events
}

#[cfg(test)]
mod tests;
