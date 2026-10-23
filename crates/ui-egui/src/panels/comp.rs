//! The Compositor panel (fork C1, ADR 0007): the node graph of a comp. Shows the comp of the
//! selected comp clip (else the last one shown) on the generic node canvas
//! ([`crate::widgets::node_canvas`]) and turns its events into `comp.*` commands, so every edit is
//! an undo step and agents can do the same through the control channel.

use egui::{Align2, Rect, UiBuilder, vec2};
use filmcraft_project::comp::{self, Comp, CompKind, CompNode, KINDS};
use filmcraft_project::{ItemId, ItemKind};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::FilmcraftApp;
use crate::theme::Tokens;
use crate::widgets::node_canvas::{CanvasEvent, CanvasGraph, CanvasState, EdgeView, NodeView, node_canvas_with_menu};

/// The panel's state (`ui.panels.comp`): the comp shown and the canvas view.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CompPanelState {
    /// The comp project item last shown.
    pub comp: Option<u64>,
    /// The clip selection when `comp.open` chose `comp`: while the selection stays this, the
    /// opened comp is shown even if a selected comp clip holds another one.
    pub opened_with: Option<Vec<u64>>,
    pub canvas: CanvasState,
    /// C4.6: the open group, as the group node ids from the comp inwards (empty: the comp itself).
    pub path: Vec<u64>,
    /// The canvas menu's template dialogs (fork C4.7).
    pub templates: super::comp_templates::TemplateMenuState,
}

/// What a node kind is called on the canvas.
pub fn kind_label(key: &str) -> &'static str {
    match key {
        "mediaIn" => "Media In",
        "mediaOut" => "Media Out",
        "merge" => "Merge",
        "transform" => "Transform",
        "key" => "Key",
        "mask" => "Mask",
        "blur" => "Blur",
        "color" => "Color",
        "text" => "Text",
        "solid" => "Solid",
        "gradient" => "Gradient",
        "roto" => "Roto",
        "paint" => "Paint",
        "tracker" => "Tracker",
        "planarTracker" => "Planar Tracker",
        "group" => "Group",
        "groupInput" => "Group Input",
        _ => "Node",
    }
}

/// The comp the panel shows: the one `comp.open` chose while the selection is unchanged since,
/// else the selected comp clip's item, else the remembered one.
fn current(app: &FilmcraftApp) -> Option<ItemId> {
    let s = &app.session;
    let is_comp = |id: ItemId| matches!(s.project.item(id).map(|i| &i.kind), Some(ItemKind::Comp(_)));
    let panel = &app.ui.panels.comp;
    let selection: Vec<u64> = s.state.selection.iter().map(|c| c.0).collect();
    if panel.opened_with.as_ref() == Some(&selection)
        && let Some(id) = panel.comp.map(ItemId).filter(|i| is_comp(*i))
    {
        return Some(id);
    }
    let q = s.active_sequence();
    s.state
        .selection
        .iter()
        .filter_map(|c| q.and_then(|q| q.find_item(*c)).map(|(_, i)| i.item))
        .find(|i| is_comp(*i))
        .or_else(|| app.ui.panels.comp.comp.map(ItemId).filter(|i| is_comp(*i)))
}

/// `comp.open` (Clip ▸ Compositor ▸ Open in Compositor; UI state, no undo step): shows the comp of
/// `clip` (else of `comp`, a comp item; else of the selected comp clip) in the Compositor panel.
pub(crate) fn route(app: &mut FilmcraftApp, id: &str, params: &serde_json::Value) -> Option<Result<serde_json::Value, String>> {
    if id == "comp.view" {
        return Some(view(app, params));
    }
    if id != "comp.open" {
        return None;
    }
    let s = &app.session;
    let is_comp = |id: ItemId| matches!(s.project.item(id).map(|i| &i.kind), Some(ItemKind::Comp(_)));
    let q = s.active_sequence();
    let of_clip = |c: u64| q.and_then(|q| q.find_item(filmcraft_project::ClipId(c))).map(|(_, i)| i.item);
    let comp = match (params.get("clip"), params.get("comp")) {
        (Some(c), _) => match c.as_u64().and_then(of_clip) {
            Some(item) if is_comp(item) => item,
            Some(_) => return Some(Err("that clip is not a comp clip".into())),
            None => return Some(Err("`clip` must be a clip of the active sequence".into())),
        },
        (None, Some(c)) => match c.as_u64().map(ItemId).filter(|i| is_comp(*i)) {
            Some(item) => item,
            None => return Some(Err("`comp` must be a comp project item".into())),
        },
        (None, None) => match s.state.selection.iter().filter_map(|c| of_clip(c.0)).find(|i| is_comp(*i)) {
            Some(item) => item,
            None => return Some(Err("select a comp clip first".into())),
        },
    };
    set_comp(app, Some(comp.0));
    app.ui.panels.comp.opened_with = Some(app.session.state.selection.iter().map(|c| c.0).collect());
    app.show_panel(crate::dock::PanelKind::Compositor);
    Some(Ok(json!({ "comp": comp.0 })))
}

/// `comp.view` (UI state, no undo step): the Program monitor shows `node`'s output of the shown
/// comp (its clip under the playhead) while the Compositor is open; no `node` (or null) goes back
/// to the comp's own output.
fn view(app: &mut FilmcraftApp, params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let node = match params.get("node") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or("`node` must be a node id")?),
    };
    if let Some(n) = node {
        let comp = current(app).ok_or("open a comp in the Compositor first")?;
        let Some(ItemKind::Comp(c)) = app.session.project.item(comp).map(|i| &i.kind) else { return Err("open a comp in the Compositor first".into()) };
        if c.node(n).is_none() {
            return Err(format!("the comp has no node {n}"));
        }
    }
    app.ui.panels.comp.canvas.viewed = node;
    Ok(json!({ "node": node }))
}

/// The Program monitor's target while the Compositor views a node (double-click a node, its
/// menu's View, or `comp.view`): that node's output of the shown comp's clip under the playhead,
/// with the comp's size. `None` (the sequence as usual) when the panel is closed, nothing or the
/// comp's output is viewed, or no clip of the comp is under the playhead.
pub fn viewer_target(app: &FilmcraftApp) -> Option<(crate::frames::Target, (u32, u32))> {
    let p = &app.ui.panels.comp;
    let node = p.canvas.viewed?;
    let kind = crate::dock::PanelKind::Compositor;
    if !app.ui.dock.contains(kind) && !app.ui.floating.iter().any(|g| g.panels.contains(&kind)) {
        return None;
    }
    let comp = ItemId(p.comp?);
    let ItemKind::Comp(c) = &app.session.project.item(comp)?.kind else { return None };
    if c.node(node).is_none() || c.output == Some(node) {
        return None;
    }
    let seq = app.session.state.active_sequence?;
    let at = app.session.playhead();
    let clip = app.session.active_sequence()?.video_tracks.iter().flat_map(|t| &t.items).find(|i| i.item == comp && i.range().contains(at))?.id;
    Some((crate::frames::Target::CompNode(seq, clip.0, node), (c.width, c.height)))
}

/// The canvas view of a comp.
pub fn graph(c: &Comp) -> CanvasGraph {
    let error = c.validate().err().map(|e| e.to_string());
    let nodes = c
        .nodes
        .iter()
        .map(|n| {
            let key = n.kind.key();
            let inputs: Vec<String> = match &n.kind {
                CompKind::Merge => vec!["Background".into(), "Foreground".into()],
                CompKind::Paint { .. } => vec!["Input".into(), "Reveal".into()],
                CompKind::Tracker { .. } => vec!["Input".into(), "Foreground".into()],
                CompKind::PlanarTracker { .. } => vec!["Background".into(), "Insert".into()],
                // C4.3: a group's inputs, one per Group Input index
                CompKind::Group { .. } => (1..=n.kind.inputs()).map(|i| format!("Input {i}")).collect(),
                k if k.inputs() == 1 => vec!["Input".into()],
                _ => Vec::new(),
            };
            let is_output = c.output == Some(n.id);
            NodeView {
                id: n.id,
                title: if n.label.is_empty() { crate::i18n::t(kind_label(key)).to_string() } else { n.label.clone() },
                pos: n.pos,
                inputs,
                outputs: if matches!(n.kind, CompKind::MediaOut) { Vec::new() } else { vec!["Output".into()] },
                bypassed: n.bypass,
                error: if is_output { error.clone() } else { None },
                is_output,
                key_inputs: 0,
                key_outputs: 0,
                actions: Vec::new(),
            }
        })
        .collect();
    let edges = c.edges.iter().map(|e| EdgeView { from: (e.from.node, e.from.index), to: (e.to.node, e.to.index) }).collect();
    let add_kinds = KINDS.iter().filter(|k| **k != "mediaOut").map(|k| ((*k).to_string(), kind_label(k).to_string())).collect();
    CanvasGraph { nodes, edges, add_kinds, node_actions: Vec::new(), canvas_actions: Vec::new(), on_screen_ids: false }
}

/// The comp the panel shows; another comp than before opens at its own level with nothing
/// selected (node ids are per comp, C4.8 review).
fn set_comp(app: &mut FilmcraftApp, comp: Option<u64>) {
    let p = &mut app.ui.panels.comp;
    if p.comp != comp {
        p.path.clear();
        p.canvas.selection.clear();
        p.canvas.viewed = None;
    }
    p.comp = comp;
}

/// `p` with the open group's path as `group` (C4.8; unchanged at the comp's own level).
pub(crate) fn scoped(mut p: serde_json::Value, path: &[u64]) -> serde_json::Value {
    if !path.is_empty()
        && let Some(o) = p.as_object_mut()
    {
        o.insert("group".into(), json!(path));
    }
    p
}

fn run(app: &mut FilmcraftApp, command: &str, params: serde_json::Value) {
    if let Err(e) = app.session.execute(command, params) {
        app.session.toast(e.to_string());
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t: Tokens = app.tokens;
    // a comp chosen by `comp.open` holds only until the selection changes
    let selection: Vec<u64> = app.session.state.selection.iter().map(|c| c.0).collect();
    if app.ui.panels.comp.opened_with.as_ref().is_some_and(|w| *w != selection) {
        app.ui.panels.comp.opened_with = None;
    }
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let Some(id) = current(app) else {
        // empty state: what to do next
        ui.painter().text(
            rect.center() - vec2(0.0, 18.0),
            Align2::CENTER_CENTER,
            crate::i18n::t("Select a comp clip to edit its nodes."),
            Tokens::ui(12.0),
            t.text_dim,
        );
        let r = Rect::from_center_size(rect.center() + vec2(0.0, 12.0), vec2(140.0, 24.0));
        let has_seq = app.session.state.active_sequence.is_some();
        let b = ui.put(r, egui::Button::new(crate::i18n::t("New Comp Clip")));
        app.auto.add_response("comp.newClip", &b, "New Comp Clip");
        if b.clicked() && has_seq {
            match app.session.execute("comp.newClip", json!({})) {
                Ok(r) => set_comp(app, r["comp"].as_u64()),
                Err(e) => app.session.toast(e.to_string()),
            }
        }
        return;
    };
    set_comp(app, Some(id.0));
    let Some(ItemKind::Comp(c)) = app.session.project.item(id).map(|i| &i.kind) else { return };
    // C4.6 / C4.8: an open group shows its inside (`path`; a stale path is cut back) under a
    // breadcrumb, editable like the comp's own nodes: every command names it as `group`
    let depth = crate::panels::comp_group::valid_depth(c, &app.ui.panels.comp.path);
    if depth < app.ui.panels.comp.path.len() {
        // the selection named nodes of the level that is gone (an undo, a delete): drop it, so
        // a key press can't act on same-numbered nodes of the level shown now
        let p = &mut app.ui.panels.comp;
        p.path.truncate(depth);
        p.canvas.selection.clear();
        p.canvas.viewed = None;
    }
    let path = app.ui.panels.comp.path.clone();
    let top = std::sync::Arc::clone(c);
    let rect = if path.is_empty() {
        rect
    } else {
        let Some(body) = crate::panels::comp_group::breadcrumb(app, ui, rect, &top) else { return };
        body
    };
    let c: &Comp = if path.is_empty() {
        &top
    } else {
        match comp::group::inner(&top, &path) {
            Ok(c) => c,
            Err(_) => return,
        }
    };
    let mut g = graph(c);
    crate::panels::comp_group::actions(&mut g, c, !path.is_empty());
    let groups: Vec<u64> = c.nodes.iter().filter(|n| matches!(n.kind, CompKind::Group { .. })).map(|n| n.id).collect();
    let viewed_before = app.ui.panels.comp.canvas.viewed;
    // the inspector of the one selected node, under the canvas (C1 step 10b)
    let inspected = match app.ui.panels.comp.canvas.selection.as_slice() {
        [n] => c
            .node(*n)
            .filter(|n| {
                matches!(
                    n.kind,
                    CompKind::Mask { .. }
                        | CompKind::Roto { .. }
                        | CompKind::Paint { .. }
                        | CompKind::Tracker { .. }
                        | CompKind::PlanarTracker { .. }
                        | CompKind::Group { .. }
                ) || !inspector_rows(n, filmcraft_time::Tick::ZERO).is_empty()
                    // (inside a group: its settings can be exposed)
                    || !path.is_empty()
            })
            .cloned(),
        _ => None,
    };
    let (rect, inspector_rect) = match &inspected {
        Some(_) if rect.height() > 220.0 => {
            let split = rect.max.y - (rect.height() * 0.4).min(260.0);
            (Rect::from_min_max(rect.min, egui::pos2(rect.max.x, split)), Some(Rect::from_min_max(egui::pos2(rect.min.x, split), rect.max)))
        }
        _ => (rect, None),
    };
    let size = (f64::from(c.width), f64::from(c.height));
    let mut state = app.ui.panels.comp.canvas.clone();
    // the background menu's template entries (fork C4.7, `comp_templates`)
    let template_names = app.ui.panels.comp.templates.names.as_ref().map(|(_, n)| n.clone());
    let can_save = !state.selection.is_empty();
    let (mut template_pick, mut template_menu_open) = (None, false);
    let mut template_menu = |ui: &mut egui::Ui, reg: &mut crate::automation::Registry, at: [f32; 2]| {
        super::comp_templates::menu(ui, reg, template_names.as_deref(), can_save, at, &mut template_pick, &mut template_menu_open)
    };
    let events =
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| node_canvas_with_menu(ui, &g, &mut state, &mut app.auto, "comp", &mut template_menu)).inner;
    if !path.is_empty() {
        // (the Program monitor views the comp's own nodes only)
        state.viewed = None;
    }
    app.ui.panels.comp.canvas = state;
    // "Show": the node the Program monitor shows (`comp.view`), top right of the canvas
    let viewed = app.ui.panels.comp.canvas.viewed.filter(|v| c.node(*v).is_some() && c.output != Some(*v));
    let name = |n: &CompNode| {
        if n.label.is_empty() { format!("{} {}", crate::i18n::t(kind_label(n.kind.key())), n.id) } else { n.label.clone() }
    };
    let mut pick: Option<Option<u64>> = None;
    let r = Rect::from_min_size(egui::pos2((rect.max.x - 200.0).max(rect.min.x), rect.min.y + 6.0), vec2(190.0, 22.0));
    ui.scope_builder(UiBuilder::new().max_rect(r), |ui| {
        if !path.is_empty() {
            return;
        }
        let shown = viewed.and_then(|v| c.node(v)).map_or_else(|| crate::i18n::t("Media Out").to_string(), name);
        let resp = egui::ComboBox::from_id_salt("comp-view").selected_text(format!("{}: {shown}", crate::i18n::t("Show"))).width(180.0).show_ui(ui, |ui| {
            if ui.selectable_label(viewed.is_none(), crate::i18n::t("Media Out")).clicked() {
                pick = Some(None);
            }
            for n in c.nodes.iter().filter(|n| c.output != Some(n.id)) {
                if ui.selectable_label(viewed == Some(n.id), name(n)).clicked() {
                    pick = Some(Some(n.id));
                }
            }
        });
        app.auto.add_response("comp.viewer", &resp.response, "Show");
    });
    if let Some(v) = pick {
        app.ui.panels.comp.canvas.viewed = v;
    }
    let comp = id.0;
    let sc = |p: serde_json::Value| scoped(p, &path);
    for e in events {
        match e {
            CanvasEvent::Select(_) => {}
            CanvasEvent::Move(node, pos) => run(app, "comp.moveNode", sc(json!({"comp": comp, "node": node, "pos": pos}))),
            CanvasEvent::MoveEnd(..) => {}
            CanvasEvent::Connect { from, to } => {
                run(app, "comp.connect", sc(json!({"comp": comp, "from": from.0, "fromPort": from.1, "to": to.0, "toPort": to.1})))
            }
            CanvasEvent::Disconnect { to } => run(app, "comp.disconnect", sc(json!({"comp": comp, "to": to.0, "toPort": to.1}))),
            CanvasEvent::AddRequested { kind, pos } => run(app, "comp.addNode", sc(json!({"comp": comp, "kind": kind, "pos": pos}))),
            CanvasEvent::Bypass(ids) => {
                for node in ids {
                    run(app, "comp.bypass", sc(json!({"comp": comp, "node": node})));
                }
            }
            CanvasEvent::Delete(ids) => run(app, "comp.removeNode", sc(json!({"comp": comp, "nodes": ids}))),
            CanvasEvent::SetOutput(node) => run(app, "comp.setOutput", sc(json!({"comp": comp, "node": node}))),
            // C4.6: a double-click on a group opens it
            CanvasEvent::View(node) if groups.contains(&node) => {
                app.ui.panels.comp.canvas.viewed = viewed_before;
                crate::panels::comp_group::enter(app, node);
            }
            // (inside a group the monitor keeps showing the comp's result)
            CanvasEvent::View(_) if !path.is_empty() => {}
            // the Program monitor shows this node's output (`viewer_target`)
            CanvasEvent::View(node) => app.ui.panels.comp.canvas.viewed = Some(node),
            CanvasEvent::NodeAction { node, action } => crate::panels::comp_group::node_action(app, id, &path, node, &action),
            CanvasEvent::CanvasAction(_) => {}
        }
    }
    crate::panels::comp_group::toolbar(app, ui, rect, id, &path);
    if let (Some(node), Some(r)) = (inspected, inspector_rect) {
        if path.is_empty() {
            inspector(app, ui, r, id, &path, &node, size);
        } else {
            // inside a group: the node's settings, and Expose buttons beside them
            let split = r.min.x + (r.width() * 0.6).max(r.width() - 320.0);
            let (left, right) = (Rect::from_min_max(r.min, egui::pos2(split, r.max.y)), Rect::from_min_max(egui::pos2(split, r.min.y), r.max));
            inspector(app, ui, left, id, &path, &node, size);
            crate::panels::comp_group::expose_rows(app, ui, right, id, &path, &node);
        }
    }
    // the canvas menu's templates (fork C4.7)
    super::comp_templates::after_canvas(app, &ui.ctx().clone(), comp, &path, template_pick, template_menu_open);
}

/// Comp time at the playhead: through the first clip of the comp under it, else the comp's start.
fn comp_time(app: &FilmcraftApp, comp: ItemId) -> filmcraft_time::Tick {
    let at = app.session.playhead();
    app.session
        .active_sequence()
        .and_then(|q| q.video_tracks.iter().flat_map(|t| &t.items).find(|i| i.item == comp && i.range().contains(at)))
        .map_or(filmcraft_time::Tick::ZERO, |i| i.source_time_at(at))
}

/// Size of a Color node's wheels in the Compositor inspector (points).
const COMP_WHEEL: f32 = 84.0;
/// Size of a Color node's curve editor in the Compositor inspector (points).
const COMP_CURVE: f32 = 180.0;

/// A node's rows: its schema parameters, or a Text node's layer parameters (the graphic text
/// controls), each with its value at comp time `at` and whether it is keyframed.
fn inspector_rows(node: &CompNode, at: filmcraft_time::Tick) -> Vec<(filmcraft_project::ParamDef, filmcraft_project::ParamValue, bool)> {
    match &node.kind {
        CompKind::Text { layer } => layer
            .def()
            .map(|d| {
                d.params
                    .iter()
                    .map(|pd| match layer.params.get(pd.id) {
                        Some(p) => (pd.clone(), p.value_at(at), p.is_animated()),
                        None => (pd.clone(), pd.default.clone(), false),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        // a Color node's numeric grade settings (its wheels and curves: a later step, C1 10b)
        CompKind::Color { node: g } => filmcraft_project::grade::NODE_PARAMS
            .iter()
            // (the HDR zone wheels have their own tab on the Color page, fork G4.2)
            .filter(|np| !np.key.starts_with("hdr."))
            .filter_map(|np| {
                let filmcraft_project::grade::NodeParamKind::Float { identity, min, max } = np.kind else { return None };
                let pd = filmcraft_project::ParamDef {
                    id: np.key,
                    label: np.label,
                    kind: filmcraft_project::ParamKind::Float {
                        min: f64::from(min),
                        max: f64::from(max),
                        soft_min: f64::from(min),
                        soft_max: f64::from(max),
                        unit: "",
                        decimals: 2,
                    },
                    default: filmcraft_project::ParamValue::Float(f64::from(identity)),
                    animatable: true,
                    group: None,
                };
                let (value, animated) = match g.params.get(np.key) {
                    Some(p) => (p.value_at(at), p.is_animated()),
                    None => (pd.default.clone(), false),
                };
                Some((pd, value, animated))
            })
            .collect(),
        kind => comp::params(kind)
            .into_iter()
            .map(|pd| {
                let value = node.value_at(pd.id, at).unwrap_or_else(|| pd.default.clone());
                let animated = node.params.get(pd.id).is_some_and(filmcraft_project::Param::is_animated);
                (pd, value, animated)
            })
            .collect(),
    }
}

/// The selected node's parameters as rows (Effect Controls style): `comp.param.<key>` widgets that
/// run `comp.setParam` (a keyframed parameter is set at the playhead's comp time).
fn inspector(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, comp: ItemId, path: &[u64], node: &CompNode, size: (f64, f64)) {
    let t: Tokens = app.tokens;
    ui.painter().line_segment([rect.left_top(), rect.right_top()], egui::Stroke::new(1.0, t.separator));
    let at = comp_time(app, comp);
    let title = if node.label.is_empty() { crate::i18n::t(kind_label(node.kind.key())).to_string() } else { node.label.clone() };
    let mut elems: crate::panels::transition_controls::Elems = Vec::new();
    let mut changes: Vec<serde_json::Value> = Vec::new();
    let mut toggles: Vec<serde_json::Value> = Vec::new();
    // C4.5: expression commits (comp.setExpression) and the comp now, for values after expressions
    let mut exprs: Vec<serde_json::Value> = Vec::new();
    // (C4.8: inside a group, that group's inside)
    let comp_now: Option<std::sync::Arc<filmcraft_project::comp::Comp>> = match app.session.project.item(comp).map(|i| &i.kind) {
        Some(ItemKind::Comp(c)) if path.is_empty() => Some(std::sync::Arc::clone(c)),
        Some(ItemKind::Comp(c)) => comp::group::inner(c, path).ok().map(|i| std::sync::Arc::new(i.clone())),
        _ => None,
    };
    let mut mask_change: Option<serde_json::Value> = None;
    // C3.5b: a Roto / Paint node's list edits, (command, params)
    let mut list_change: Option<(&'static str, serde_json::Value)> = None;
    let mut track_cmds: Vec<(&'static str, serde_json::Value)> = Vec::new();
    // (analysis jobs run on the comp's own nodes only)
    let running = path.is_empty() && app.session.comp_track_jobs.iter().any(|j| j.comp == comp && j.node == node.id);
    // C3.7: the comp's trackers (for Follow tracker) and the Roto node's running shape track
    let roto_cx = RotoCtx {
        trackers: comp_now
            .as_deref()
            .map(|c| {
                c.nodes
                    .iter()
                    .filter(|n| matches!(n.kind, CompKind::Tracker { .. } | CompKind::PlanarTracker { .. }))
                    .map(|n| (n.id, filmcraft_engine::comp_explain::node_name(n)))
                    .collect()
            })
            .unwrap_or_default(),
        tracking: app.session.roto_track_jobs.iter().find(|j| path.is_empty() && j.comp == comp && j.node == node.id).map(|j| j.shape),
        inside: !path.is_empty(),
    };
    let rate = comp_now.as_deref().map_or_else(filmcraft_time::FrameRate::default, |c| c.rate);
    ui.scope_builder(UiBuilder::new().max_rect(rect.shrink(8.0)), |ui| {
        ui.label(egui::RichText::new(title).strong());
        egui::ScrollArea::vertical().id_salt("comp-inspector").show(ui, |ui| {
            // a Color node's Lift / Gamma / Gain / Offset wheels (the grade panel's mapping: a puck
            // moves the colour balance and keeps the master); ids `comp.wheel.<key>`
            if let CompKind::Color { node: g } = &node.kind {
                ui.add_space(6.0);
                ui.horizontal_top(|ui| {
                    for &(key, title, scale, _) in &crate::panels::grade::WHEELS {
                        ui.vertical(|ui| {
                            ui.label(crate::i18n::t(title));
                            let w = g.wheel_at(key, at);
                            let r = ui.cursor().min;
                            let ch = crate::widgets::color::grade_wheel(ui, crate::panels::grade::puck_of([w[0], w[1], w[2]], scale), COMP_WHEEL);
                            elems.push((format!("comp.wheel.{key}"), Rect::from_min_size(r, vec2(COMP_WHEEL, COMP_WHEEL)), title.to_string()));
                            if let Some(puck) = ch {
                                let c = crate::panels::grade::rgb_of(puck, scale);
                                let mean = (w[0] + w[1] + w[2]) / 3.0;
                                let animated = g.params.get(key).is_some_and(filmcraft_project::Param::is_animated);
                                let mut p = json!({"comp": comp.0, "node": node.id, "param": key, "value": [mean + c[0], mean + c[1], mean + c[2], w[3]]});
                                if animated {
                                    p["time"] = json!(at.0);
                                }
                                changes.push(p);
                            }
                        });
                    }
                });
            }
            // its curves: a channel picker (Y R G B, ids `comp.curve.<channel>`) and the editor
            // (`comp.curve`); one curve shown at a time, as in the Color page's Curves tab
            if let CompKind::Color { node: g } = &node.kind {
                let key = egui::Id::new(("comp-curve-channel", comp.0, node.id));
                let mut ch: usize = ui.data(|d| d.get_temp(key)).unwrap_or(0);
                ui.horizontal(|ui| {
                    for (i, (k, label)) in crate::panels::grade::CURVES.iter().enumerate() {
                        let r = ui.selectable_label(ch == i, *label);
                        elems.push((format!("comp.curve.{}", k.trim_start_matches("curve.")), r.rect, format!("{label} curve")));
                        if r.clicked() {
                            ch = i;
                        }
                    }
                });
                ui.data_mut(|d| d.insert_temp(key, ch));
                let (k, _) = crate::panels::grade::CURVES.get(ch).copied().unwrap_or(crate::panels::grade::CURVES[0]);
                let pts = g.curve(k).unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
                let col = match ch {
                    1 => egui::Color32::from_rgb(0xe0, 0x50, 0x50),
                    2 => egui::Color32::from_rgb(0x50, 0xc0, 0x50),
                    3 => egui::Color32::from_rgb(0x50, 0x80, 0xf0),
                    _ => egui::Color32::from_gray(0xd0),
                };
                ui.scope(|ui| {
                    ui.set_max_width(COMP_CURVE);
                    let r = ui.cursor().min;
                    let edited = crate::widgets::color::curve_editor(ui, &pts, false, col, &t, egui::Id::new(("comp-curve", comp.0, node.id, k)));
                    elems.push(("comp.curve".to_string(), Rect::from_min_size(r, vec2(COMP_CURVE, COMP_CURVE)), "Curve".to_string()));
                    if let Some(new_pts) = edited {
                        let v: Vec<serde_json::Value> = new_pts.iter().map(|q| json!([q[0], q[1]])).collect();
                        changes.push(json!({"comp": comp.0, "node": node.id, "param": k, "value": v}));
                    }
                });
                ui.add_space(6.0);
            }
            // C4.5: values after expressions; an "=" toggle beside each keyframable own parameter
            // opens a one-line expression field (`comp.inspector.<param>.expr`, committed on Enter
            // or leaving the field with comp.setExpression), its error underneath
            let frame = comp_now.as_deref().map(|c| c.expr_frame(at));
            let own = !matches!(node.kind, CompKind::Text { .. } | CompKind::Color { .. });
            egui::Grid::new("comp-inspector-grid").num_columns(3).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                for (pd, value, animated) in inspector_rows(node, at) {
                    let src = if own { node.expression(pd.id).map(str::to_string) } else { None };
                    let (value, eval_error) = match (&src, &frame) {
                        (Some(_), Some(f)) => f.param_at(node.id, pd.id),
                        _ => (value, None),
                    };
                    let can_expr = own
                        && pd.animatable
                        && filmcraft_project::expr::kind_allowed(&pd.default)
                        && !matches!(
                            pd.kind,
                            filmcraft_project::ParamKind::Text | filmcraft_project::ParamKind::Curve { .. } | filmcraft_project::ParamKind::Path
                        );
                    // the stopwatch: keyframes on / off at the playhead's comp time
                    let (sw, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::click());
                    if pd.animatable {
                        crate::icons::paint(ui.painter(), sw, crate::icons::Icon::Stopwatch, if animated { t.accent } else { t.text_dim });
                        elems.push((format!("comp.param.{}.stopwatch", pd.id), sw, "Toggle animation".into()));
                        if resp.on_hover_text(crate::i18n::t("Toggle animation")).clicked() {
                            toggles.push(json!({"comp": comp.0, "node": node.id, "param": pd.id, "time": at.0}));
                        }
                    }
                    let open_key = egui::Id::new(("comp-expr-open", comp.0, node.id, pd.id));
                    let mut open = can_expr && ui.data(|d| d.get_temp::<bool>(open_key)).unwrap_or(src.is_some());
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::t(pd.label));
                        if can_expr {
                            let r = ui.selectable_label(open, egui::RichText::new("=").monospace());
                            elems.push((format!("comp.inspector.{}.expr.toggle", pd.id), r.rect, "Expression".into()));
                            if r.on_hover_text(crate::i18n::t("Expression")).clicked() {
                                open = !open;
                                ui.data_mut(|d| d.insert_temp(open_key, open));
                            }
                        }
                    });
                    let eid = format!("comp.param.{}", pd.id);
                    if let Some(c) = crate::panels::transition_controls::param_widget(ui, &mut elems, &eid, pd.id, &pd.kind, &value, size) {
                        let mut p = json!({"comp": comp.0, "node": node.id, "param": pd.id, "value": c["params"][pd.id].clone()});
                        if animated {
                            p["time"] = json!(at.0);
                        }
                        changes.push(p);
                    }
                    ui.end_row();
                    if !open {
                        continue;
                    }
                    // the expression field: a draft while it is being typed (or refused), else the stored one
                    let draft_key = egui::Id::new(("comp-expr-draft", comp.0, node.id, pd.id));
                    let error_key = egui::Id::new(("comp-expr-error", comp.0, node.id, pd.id));
                    let stored = src.clone().unwrap_or_default();
                    let mut text: String = ui.data(|d| d.get_temp::<String>(draft_key)).unwrap_or_else(|| stored.clone());
                    ui.label("");
                    ui.label("");
                    ui.vertical(|ui| {
                        let fid = format!("comp.inspector.{}.expr", pd.id);
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut text)
                                .id(egui::Id::new(("comp-expr-field", comp.0, node.id, pd.id)))
                                .hint_text(crate::i18n::t("e.g. time * 10"))
                                .font(egui::TextStyle::Monospace)
                                .desired_width(220.0)
                                .char_limit(filmcraft_project::expr::MAX_SOURCE),
                        );
                        elems.push((fid, resp.rect, if text.is_empty() { "Expression".into() } else { text.clone() }));
                        if resp.changed() {
                            ui.data_mut(|d| d.insert_temp(draft_key, text.clone()));
                        }
                        if resp.lost_focus() {
                            if text.trim() != stored.trim() {
                                exprs.push(json!({"comp": comp.0, "node": node.id, "param": pd.id, "expression": text, "time": at.0}));
                            } else {
                                ui.data_mut(|d| d.remove::<String>(draft_key));
                            }
                        }
                        let refused: Option<String> = ui.data(|d| d.get_temp(error_key));
                        let shown = refused.or_else(|| eval_error.as_ref().map(|e| e.to_string()));
                        if let Some(msg) = shown {
                            let r = ui.label(egui::RichText::new(msg.clone()).color(ui.visuals().warn_fg_color).small());
                            elems.push((format!("comp.inspector.{}.expr.error", pd.id), r.rect, msg));
                        }
                    });
                    ui.end_row();
                }
            });
            if let CompKind::Mask { masks } = &node.kind
                && let Some(list) = mask_rows(ui, &mut elems, masks, size, at)
            {
                mask_change = Some(json!({"comp": comp.0, "node": node.id, "masks": list}));
            }
            if let CompKind::Roto { roto, output } = &node.kind
                && let Some((cmd, mut p)) = roto_rows(ui, &mut elems, roto, *output, at, &roto_cx)
            {
                p["comp"] = json!(comp.0);
                p["node"] = json!(node.id);
                list_change = Some((cmd, p));
            }
            if let CompKind::Paint { paint } = &node.kind
                && let Some((cmd, mut p)) = paint_rows(ui, &mut elems, paint)
            {
                p["comp"] = json!(comp.0);
                p["node"] = json!(node.id);
                list_change = Some((cmd, p));
            }
            track_cmds = tracker_rows(ui, &mut elems, TrackerCtx { comp: comp.0, at, size, running, rate, inside: !path.is_empty() }, node);
            // C4.3: a group's exposed parameters
            group_rows(ui, &mut elems, (&mut changes, &mut toggles), comp.0, node, at, size);
        });
    });
    for (eid, r, l) in elems {
        app.auto.add(&eid, r, &l);
    }
    // C4.8: inside a group every command names it
    let changes: Vec<serde_json::Value> = changes.into_iter().map(|p| scoped(p, path)).collect();
    let toggles: Vec<serde_json::Value> = toggles.into_iter().map(|p| scoped(p, path)).collect();
    let exprs: Vec<serde_json::Value> = exprs.into_iter().map(|p| scoped(p, path)).collect();
    let mask_change = mask_change.map(|p| scoped(p, path));
    let list_change = list_change.map(|(c, p)| (c, scoped(p, path)));
    let track_cmds: Vec<(&'static str, serde_json::Value)> = track_cmds.into_iter().map(|(c, p)| (c, scoped(p, path))).collect();
    for p in changes {
        run(app, "comp.setParam", p);
    }
    for p in toggles {
        run(app, "comp.toggleAnimation", p);
    }
    // a refused expression (it doesn't parse) keeps its draft and shows why; a stored one drops both
    for p in exprs {
        let (node_id, key) = (p["node"].as_u64().unwrap_or(0), p["param"].as_str().unwrap_or("").to_string());
        let draft_key = egui::Id::new(("comp-expr-draft", comp.0, node_id, key.as_str()));
        let error_key = egui::Id::new(("comp-expr-error", comp.0, node_id, key.as_str()));
        match app.session.execute("comp.setExpression", p) {
            Ok(r) if r["stored"] == json!(false) => {
                let msg = r["error"]["text"].as_str().unwrap_or("").to_string();
                ui.ctx().data_mut(|d| d.insert_temp(error_key, msg));
            }
            Ok(_) => ui.ctx().data_mut(|d| {
                d.remove::<String>(draft_key);
                d.remove::<String>(error_key);
            }),
            Err(e) => app.session.toast(e.to_string()),
        }
    }
    if let Some(p) = mask_change {
        run(app, "comp.setMasks", p);
    }
    if let Some((cmd, p)) = list_change {
        run(app, cmd, p);
    }
    for (command, p) in track_cmds {
        run(app, command, p);
    }
}

/// A Roto node's rows (C3.5b): Output (Alpha / Matte), Add shape (a centred ellipse), and per
/// shape Enabled, Inverted, Mode, Feather (set at comp time `at`) and Delete. Returns the command
/// to run (`comp.setRoto` with the whole new roto, `comp.roto.addShape` or
/// `comp.roto.removeShape`; one undo step). Ids `comp.roto.output`, `comp.roto.add`,
/// `comp.roto<k>.{enabled,inverted,mode,feather,delete}`. Drawing shapes on the viewer is a later
/// step. C3.7 adds a second row per shape: Follow tracker (`comp.roto<k>.follow`: None or one of
/// the comp's trackers, `comp.roto.linkTracker` / `unlinkTracker` at the playhead, so the shape
/// stays where it is on screen there; a link whose tracker is gone
/// shows "Missing tracker") and Track backward / forward from the playhead
/// (`comp.roto<k>.track.{backward,forward}`, `comp.roto.trackShape`), Stop while it runs
/// (`comp.roto<k>.track.stop`).
fn roto_rows(
    ui: &mut egui::Ui,
    elems: &mut crate::panels::transition_controls::Elems,
    roto: &filmcraft_project::roto::Roto,
    output: comp::RotoOutput,
    at: filmcraft_time::Tick,
    cx: &RotoCtx,
) -> Option<(&'static str, serde_json::Value)> {
    use filmcraft_project::roto::{MAX_FEATHER, MAX_SHAPES, RotoMode};
    let mut out: Option<(&'static str, serde_json::Value)> = None;
    let set = |r: filmcraft_project::roto::Roto| ("comp.setRoto", json!({ "roto": r }));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(crate::i18n::t("Output"));
        let r = egui::ComboBox::from_id_salt("comp-roto-output").selected_text(crate::i18n::t(output.label())).show_ui(ui, |ui| {
            for o in comp::RotoOutput::ALL {
                if ui.selectable_label(o == output, crate::i18n::t(o.label())).clicked() && o != output {
                    out = Some(("comp.setRoto", json!({ "output": o.label().to_ascii_lowercase() })));
                }
            }
        });
        elems.push(("comp.roto.output".to_string(), r.response.rect, "Output".into()));
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::t("Shapes")).strong());
        let resp = ui.add_enabled(roto.shapes.len() < MAX_SHAPES, egui::Button::new(crate::i18n::t("Add shape")));
        elems.push(("comp.roto.add".to_string(), resp.rect, "Add shape".into()));
        if resp.clicked() {
            out = Some(("comp.roto.addShape", json!({})));
        }
    });
    for (k, sh) in roto.shapes.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(&sh.name);
            let mut on = sh.enabled;
            let r = ui.checkbox(&mut on, crate::i18n::t("Enabled"));
            elems.push((format!("comp.roto{k}.enabled"), r.rect, "Enabled".into()));
            let mut inv = sh.inverted;
            let r2 = ui.checkbox(&mut inv, crate::i18n::t("Inverted"));
            elems.push((format!("comp.roto{k}.inverted"), r2.rect, "Inverted".into()));
            if r.changed() || r2.changed() {
                let mut list = roto.clone();
                if let Some(x) = list.shapes.get_mut(k) {
                    x.enabled = on;
                    x.inverted = inv;
                }
                out = Some(set(list));
            }
            let mode = egui::ComboBox::from_id_salt(("comp-roto-mode", k)).selected_text(crate::i18n::t(sh.mode.label())).show_ui(ui, |ui| {
                for m in RotoMode::ALL {
                    if ui.selectable_label(m == sh.mode, crate::i18n::t(m.label())).clicked() && m != sh.mode {
                        let mut list = roto.clone();
                        if let Some(x) = list.shapes.get_mut(k) {
                            x.mode = m;
                        }
                        out = Some(set(list));
                    }
                }
            });
            elems.push((format!("comp.roto{k}.mode"), mode.response.rect, "Mode".into()));
            ui.label(crate::i18n::t("Feather"));
            let mut f = sh.feather_at(at);
            let r = ui.add(egui::DragValue::new(&mut f).range(0.0..=MAX_FEATHER).speed(0.5).suffix(" px"));
            elems.push((format!("comp.roto{k}.feather"), r.rect, "Feather".into()));
            let g = drag_gesture(ui, &r);
            if r.changed() && f.is_finite() {
                let mut list = roto.clone();
                if let Some(x) = list.shapes.get_mut(k) {
                    x.feather.set_at(at, filmcraft_project::ParamValue::Float(f.clamp(0.0, MAX_FEATHER)));
                }
                out = Some(with_gesture(set(list), g));
            }
            let resp = ui.button(crate::i18n::t("Delete"));
            elems.push((format!("comp.roto{k}.delete"), resp.rect, "Delete".into()));
            if resp.clicked() {
                out = Some(("comp.roto.removeShape", json!({ "shape": k })));
            }
        });
        if let Some(c) = shape_track_row(ui, elems, k, sh, at, cx) {
            out = Some(c);
        }
    }
    out
}

/// What a Roto node's shape rows need besides the roto (C3.7): the comp's Tracker / Planar
/// Tracker nodes (id, name) and the shape being tracked with Magic Mask, if one is.
struct RotoCtx {
    trackers: Vec<(u64, String)>,
    tracking: Option<usize>,
    /// Inside a group (C4.8): shape tracking runs on the comp's own nodes only.
    inside: bool,
}

/// Why tracking is off inside a group (the hover note on its disabled buttons).
const TRACK_INSIDE_NOTE: &str = "Tracking works on the comp's own nodes only for now; ungroup these nodes to track.";

/// Shape `k`'s Follow tracker / Track shape row (C3.7, see [`roto_rows`]).
fn shape_track_row(
    ui: &mut egui::Ui,
    elems: &mut crate::panels::transition_controls::Elems,
    k: usize,
    sh: &filmcraft_project::roto::RotoShape,
    at: filmcraft_time::Tick,
    cx: &RotoCtx,
) -> Option<(&'static str, serde_json::Value)> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(crate::i18n::t("Follow tracker"));
        let linked = sh.track_link.as_ref().map(|l| l.tracker.trim().parse::<u64>().ok());
        let current = match linked {
            None => crate::i18n::t("None").to_string(),
            Some(id) => match cx.trackers.iter().find(|(t, _)| Some(*t) == id) {
                Some((_, name)) => name.clone(),
                None => crate::i18n::t("Missing tracker").to_string(),
            },
        };
        let combo = egui::ComboBox::from_id_salt(("comp-roto-follow", k)).selected_text(current).show_ui(ui, |ui| {
            if ui.selectable_label(linked.is_none(), crate::i18n::t("None")).clicked() && linked.is_some() {
                out = Some(("comp.roto.unlinkTracker", json!({ "shape": k, "at": at.0 })));
            }
            for (id, name) in &cx.trackers {
                let on = linked == Some(Some(*id));
                if ui.selectable_label(on, name).clicked() && !on {
                    out = Some(("comp.roto.linkTracker", json!({ "shape": k, "tracker": id, "at": at.0 })));
                }
            }
        });
        elems.push((format!("comp.roto{k}.follow"), combo.response.rect, "Follow tracker".into()));
        if cx.tracking == Some(k) {
            let resp = ui.button(crate::i18n::t("Stop"));
            elems.push((format!("comp.roto{k}.track.stop"), resp.rect, "Stop".into()));
            if resp.clicked() {
                out = Some(("comp.roto.stopTrack", json!({})));
            }
        } else {
            // (a linked shape moves with its tracker; tracking it as well would move it twice)
            let can = !cx.inside && cx.tracking.is_none() && sh.track_link.is_none() && sh.kind == filmcraft_project::roto::RotoKind::Bezier;
            for (key, label, dir) in [("backward", "Track backward", "backward"), ("forward", "Track forward", "forward")] {
                let mut resp = ui.add_enabled(can, egui::Button::new(crate::i18n::t(label)));
                if cx.inside {
                    resp = resp.on_disabled_hover_text(crate::i18n::t(TRACK_INSIDE_NOTE));
                }
                let label = if cx.inside { format!("{label} (off: {TRACK_INSIDE_NOTE})") } else { label.to_string() };
                elems.push((format!("comp.roto{k}.track.{key}"), resp.rect, label));
                if resp.clicked() {
                    out = Some(("comp.roto.trackShape", json!({ "shape": k, "from": at.0, "direction": dir })));
                }
            }
        }
    });
    out
}

/// The `gesture` of a Roto / Paint inspector drag (C3.5b): one number for the whole drag, so a
/// drag is one undo step (`comp.setRoto` / `comp.setPaint` merge the same gesture). None when
/// the value isn't being dragged (a typed value is its own step).
fn drag_gesture(ui: &egui::Ui, r: &egui::Response) -> Option<u64> {
    let id = egui::Id::new("comp-roto-paint-gesture");
    let mut g: u64 = ui.data(|d| d.get_temp(id)).unwrap_or(0);
    if r.drag_started() {
        g = g.wrapping_add(1);
        ui.data_mut(|d| d.insert_temp(id, g));
    }
    (r.drag_started() || r.dragged()).then_some(g)
}

/// `(command, params)` with `gesture` added when there is one.
fn with_gesture((cmd, mut p): (&'static str, serde_json::Value), g: Option<u64>) -> (&'static str, serde_json::Value) {
    if let Some(g) = g {
        p["gesture"] = json!(g);
    }
    (cmd, p)
}

/// What a paint stroke's duration shows in the list.
fn stroke_duration(d: &filmcraft_project::paint::StrokeDuration) -> String {
    use filmcraft_project::paint::StrokeDuration;
    let secs = |t: filmcraft_time::Tick| t.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
    match *d {
        StrokeDuration::SingleFrame { at } => format!("{} {:.2} s", crate::i18n::t("Single frame"), secs(at)),
        StrokeDuration::Hold { from, to: Some(to) } => format!("{} {:.2}–{:.2} s", crate::i18n::t("Hold"), secs(from), secs(to)),
        StrokeDuration::Hold { from, to: None } => format!("{} {:.2} s –", crate::i18n::t("Hold"), secs(from)),
    }
}

/// A Paint node's rows (C3.5b): per stroke its tool, Size and Opacity (`comp.setPaint` with the
/// whole new list), its duration, Move up / Move down (`comp.paint.reorder`) and Delete
/// (`comp.paint.removeStroke`). Ids `comp.paint<k>.{size,opacity,up,down,delete}`. Strokes are
/// drawn on the viewer in a later step.
fn paint_rows(
    ui: &mut egui::Ui,
    elems: &mut crate::panels::transition_controls::Elems,
    paint: &filmcraft_project::paint::Paint,
) -> Option<(&'static str, serde_json::Value)> {
    use filmcraft_project::paint::{MAX_BRUSH_SIZE, PaintTool};
    let mut out: Option<(&'static str, serde_json::Value)> = None;
    ui.add_space(6.0);
    ui.label(egui::RichText::new(crate::i18n::t("Strokes")).strong());
    let n = paint.strokes.len();
    // (a long list shows its first strokes; agents use the commands for the rest)
    for (k, st) in paint.strokes.iter().enumerate().take(200) {
        ui.horizontal(|ui| {
            let tool = match st.tool {
                PaintTool::Color => "Color",
                PaintTool::Clone => "Clone",
                PaintTool::Reveal => "Reveal",
                PaintTool::Erase => "Erase",
            };
            ui.label(format!("{} {}", k.saturating_add(1), crate::i18n::t(tool)));
            ui.label(crate::i18n::t("Size"));
            let mut size = st.brush.size;
            let r = ui.add(egui::DragValue::new(&mut size).range(0.0..=MAX_BRUSH_SIZE).speed(0.5).suffix(" px"));
            elems.push((format!("comp.paint{k}.size"), r.rect, "Size".into()));
            ui.label(crate::i18n::t("Opacity"));
            let mut op = st.brush.opacity * 100.0;
            let r2 = ui.add(egui::DragValue::new(&mut op).range(0.0..=100.0).speed(0.5).suffix(" %"));
            elems.push((format!("comp.paint{k}.opacity"), r2.rect, "Opacity".into()));
            let g = drag_gesture(ui, &r).or_else(|| drag_gesture(ui, &r2));
            if (r.changed() || r2.changed()) && size.is_finite() && op.is_finite() {
                let mut list = paint.clone();
                if let Some(x) = list.strokes.get_mut(k) {
                    x.brush.size = size.clamp(0.0, MAX_BRUSH_SIZE);
                    x.brush.opacity = (op / 100.0).clamp(0.0, 1.0);
                }
                out = Some(with_gesture(("comp.setPaint", json!({ "paint": list })), g));
            }
            ui.label(egui::RichText::new(stroke_duration(&st.duration)).weak());
            for (key, label, to, ok) in [("up", "Move up", k.saturating_sub(1), k > 0), ("down", "Move down", k.saturating_add(1), k + 1 < n)] {
                let resp = ui.add_enabled(ok, egui::Button::new(crate::i18n::t(label)));
                elems.push((format!("comp.paint{k}.{key}"), resp.rect, label.into()));
                if resp.clicked() {
                    out = Some(("comp.paint.reorder", json!({ "from": k, "to": to })));
                }
            }
            let resp = ui.button(crate::i18n::t("Delete"));
            elems.push((format!("comp.paint{k}.delete"), resp.rect, "Delete".into()));
            if resp.clicked() {
                out = Some(("comp.paint.removeStroke", json!({ "stroke": k })));
            }
        });
    }
    out
}

/// Where a tracker node's controls are drawn: its comp, the playhead's comp time, the comp size,
/// whether its analysis is running and the comp's frame rate.
struct TrackerCtx {
    comp: u64,
    at: filmcraft_time::Tick,
    size: (f64, f64),
    running: bool,
    rate: filmcraft_time::FrameRate,
    /// Inside a group (C4.8): analysis runs on the comp's own nodes only.
    inside: bool,
}

/// A Tracker / Planar Tracker node's controls (C3.5a; numeric for now, viewer tools later):
/// Operation, Method and Smooth radius (Tracker), the reference frame (Set to playhead), Analyse
/// backward / forward from the playhead (Stop while it runs), Clear track, then the points (add,
/// x / y at the playhead, delete) or the plane's four corners at the reference. Returns the
/// commands to run. Ids `comp.track.{operation,method,smoothRadius,reference,analyse.backward,
/// analyse.forward,cancel,clear,addPoint}`, `comp.track.point<k>.{x,y,delete}`,
/// `comp.planar.corner<k>.{x,y}`.
fn tracker_rows(
    ui: &mut egui::Ui,
    elems: &mut crate::panels::transition_controls::Elems,
    cx: TrackerCtx,
    node: &CompNode,
) -> Vec<(&'static str, serde_json::Value)> {
    use filmcraft_geom::Vec2;
    use filmcraft_project::comp::TrackOperation;
    let t = crate::i18n::t;
    let mut out: Vec<(&'static str, serde_json::Value)> = Vec::new();
    let (comp, id) = (cx.comp, node.id);
    let (operation, reference) = match &node.kind {
        CompKind::Tracker { operation, reference, .. } | CompKind::PlanarTracker { operation, reference, .. } => (*operation, *reference),
        _ => return out,
    };
    let options = |extra: serde_json::Value| {
        let mut p = json!({"comp": comp, "node": id});
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        p
    };
    ui.add_space(6.0);
    egui::Grid::new(("comp-track-grid", id)).num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
        ui.label(t("Operation"));
        let resp = egui::ComboBox::from_id_salt(("comp-track-op", id)).selected_text(t(operation.label())).show_ui(ui, |ui| {
            for op in TrackOperation::ALL {
                if ui.selectable_label(op == operation, t(op.label())).clicked() && op != operation {
                    out.push(("comp.track.setOptions", options(json!({"operation": op.key()}))));
                }
            }
        });
        elems.push(("comp.track.operation".into(), resp.response.rect, "Operation".into()));
        ui.end_row();
        if let CompKind::Tracker { method, smooth_radius, .. } = &node.kind {
            ui.label(t("Method"));
            let resp = egui::ComboBox::from_id_salt(("comp-track-method", id)).selected_text(t(method.label())).show_ui(ui, |ui| {
                for m in filmcraft_project::TrackMethod::ALL {
                    if ui.selectable_label(m == *method, t(m.label())).clicked() && m != *method {
                        out.push(("comp.track.setOptions", options(json!({"method": m.label()}))));
                    }
                }
            });
            elems.push(("comp.track.method".into(), resp.response.rect, "Method".into()));
            ui.end_row();
            ui.label(t("Smooth radius"));
            let mut r = *smooth_radius;
            let resp = ui.add(egui::DragValue::new(&mut r).range(0..=filmcraft_project::comp::MAX_SMOOTH_RADIUS).suffix(" f"));
            elems.push(("comp.track.smoothRadius".into(), resp.rect, "Smooth radius".into()));
            if resp.changed() && r != *smooth_radius {
                out.push(("comp.track.setOptions", options(json!({"smoothRadius": r, "gesture": resp.id.value()}))));
            }
            ui.end_row();
        }
        ui.label(t("Reference"));
        ui.horizontal(|ui| {
            ui.label(format!("{}", cx.rate.frame_at(reference)));
            let resp = ui.button(t("Set to playhead"));
            elems.push(("comp.track.reference".into(), resp.rect, "Set to playhead".into()));
            if resp.clicked() {
                out.push(("comp.track.setOptions", options(json!({"reference": cx.at.0}))));
            }
        });
        ui.end_row();
    });
    ui.horizontal(|ui| {
        if cx.running {
            let resp = ui.button(t("Stop"));
            elems.push(("comp.track.cancel".into(), resp.rect, "Stop".into()));
            if resp.clicked() {
                out.push(("comp.track.cancel", options(json!({}))));
            }
        } else {
            for (key, label, dir) in [("backward", "Analyse backward", "backward"), ("forward", "Analyse forward", "forward")] {
                let mut resp = ui.add_enabled(!cx.inside, egui::Button::new(t(label)));
                if cx.inside {
                    resp = resp.on_disabled_hover_text(t(TRACK_INSIDE_NOTE));
                }
                // (the automation label carries the note while it is off)
                let label = if cx.inside { format!("{label} (off: {TRACK_INSIDE_NOTE})") } else { label.to_string() };
                elems.push((format!("comp.track.analyse.{key}"), resp.rect, label));
                if resp.clicked() {
                    out.push(("comp.track.analyse", options(json!({"from": cx.at.0, "direction": dir}))));
                }
            }
        }
        let resp = ui.add_enabled(!cx.running, egui::Button::new(t("Clear track")));
        elems.push(("comp.track.clear".into(), resp.rect, "Clear track".into()));
        if resp.clicked() {
            out.push(("comp.track.clear", options(json!({}))));
        }
    });
    let drag = |ui: &mut egui::Ui, elems: &mut crate::panels::transition_controls::Elems, eid: String, v: f64| -> Option<f64> {
        let mut x = v;
        let resp = ui.add(egui::DragValue::new(&mut x).speed(0.5).max_decimals(2));
        elems.push((eid.clone(), resp.rect, eid));
        (resp.changed() && x.is_finite() && x != v).then_some(x)
    };
    match &node.kind {
        CompKind::Tracker { points, .. } => {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t("Points")).strong());
                let resp = ui.add_enabled(points.len() < filmcraft_project::comp::MAX_TRACK_POINTS, egui::Button::new(t("Add point")));
                elems.push(("comp.track.addPoint".into(), resp.rect, "Add point".into()));
                if resp.clicked() {
                    let mut list = points.clone();
                    let k = list.len().saturating_add(1);
                    list.push(filmcraft_project::comp::TrackerPoint::at(format!("P{k}"), Vec2::new(cx.size.0 / 2.0, cx.size.1 / 2.0)));
                    out.push(("comp.track.setPoints", options(json!({"points": list}))));
                }
            });
            for (k, pt) in points.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(if pt.name.is_empty() { format!("P{}", k + 1) } else { pt.name.clone() });
                    let at = pt.track.value_at(cx.at).as_vec2().unwrap_or(Vec2::ZERO);
                    let x = drag(ui, elems, format!("comp.track.point{k}.x"), at.x);
                    let y = drag(ui, elems, format!("comp.track.point{k}.y"), at.y);
                    if x.is_some() || y.is_some() {
                        let mut list = points.clone();
                        if let Some(p) = list.get_mut(k) {
                            p.track.set_at(cx.at, filmcraft_project::ParamValue::Vec2(Vec2::new(x.unwrap_or(at.x), y.unwrap_or(at.y))));
                        }
                        out.push(("comp.track.setPoints", options(json!({"points": list}))));
                    }
                    let resp = ui.button(t("Delete"));
                    elems.push((format!("comp.track.point{k}.delete"), resp.rect, "Delete".into()));
                    if resp.clicked() {
                        let mut list = points.clone();
                        if k < list.len() {
                            list.remove(k);
                        }
                        out.push(("comp.track.setPoints", options(json!({"points": list}))));
                    }
                });
            }
        }
        CompKind::PlanarTracker { region, quad, .. } => {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(t("Plane corners")).strong());
            let poly: Vec<Vec2> = region.vertices.iter().map(|v| v.p).collect();
            let q = filmcraft_render::comp::track::reference_quad(quad, &poly, cx.size);
            let mut changed = None;
            for (k, (label, c)) in ["Upper Left", "Upper Right", "Lower Right", "Lower Left"].iter().zip(q).enumerate() {
                ui.horizontal(|ui| {
                    ui.label(t(label));
                    let x = drag(ui, elems, format!("comp.planar.corner{k}.x"), c.x);
                    let y = drag(ui, elems, format!("comp.planar.corner{k}.y"), c.y);
                    if x.is_some() || y.is_some() {
                        let mut nq = q;
                        if let Some(p) = nq.get_mut(k) {
                            *p = Vec2::new(x.unwrap_or(c.x), y.unwrap_or(c.y));
                        }
                        changed = Some(nq);
                    }
                });
            }
            if let Some(nq) = changed {
                // a region that is the quad (or none yet) moves with it
                let follows = poly.len() < 3 || (poly.len() == 4 && poly.iter().zip(q.iter()).all(|(a, b)| (*a - *b).length() < 1e-6));
                let pts = |v: &[Vec2]| v.iter().map(|p| json!([p.x, p.y])).collect::<Vec<_>>();
                let region = if follows { pts(&nq) } else { pts(&poly) };
                out.push(("comp.planar.setRegion", options(json!({"region": region, "quad": pts(&nq)}))));
            }
        }
        _ => {}
    }
    out
}

/// A Mask node's mask list: add an ellipse or a rectangle (centred, a fifth of the comp's short
/// side), and per mask Inverted, Feather / Opacity / Expansion (set at comp time `at`, so a
/// keyframed one gets a keyframe) and Delete. Returns the whole new list when something changed
/// (for `comp.setMasks`, one undo step). Ids `comp.mask.{ellipse,rect}`, `comp.mask<k>.inverted`,
/// `comp.mask<k>.{feather,opacity,expansion}`, `comp.mask<k>.delete`. Shapes are drawn in the comp's pixels; editing them on the viewer is a
/// later step (C1 10b).
fn mask_rows(
    ui: &mut egui::Ui,
    elems: &mut crate::panels::transition_controls::Elems,
    masks: &[filmcraft_project::Mask],
    size: (f64, f64),
    at: filmcraft_time::Tick,
) -> Option<Vec<filmcraft_project::Mask>> {
    use filmcraft_geom::Vec2;
    use filmcraft_project::{Mask, MaskPath};
    let mut out: Option<Vec<Mask>> = None;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::t("Masks")).strong());
        let full = masks.len() >= filmcraft_project::grade::MAX_WINDOWS;
        let (w, h) = (size.0.max(1.0), size.1.max(1.0));
        let (c, r) = (Vec2::new(w / 2.0, h / 2.0), w.min(h) * 0.2);
        for (key, label) in [("ellipse", "Add ellipse"), ("rect", "Add rectangle")] {
            let resp = ui.add_enabled(!full, egui::Button::new(crate::i18n::t(label)));
            elems.push((format!("comp.mask.{key}"), resp.rect, label.into()));
            if resp.clicked() {
                let name = format!("Mask {}", masks.len().saturating_add(1));
                let path = if key == "ellipse" { MaskPath::ellipse(c, Vec2::new(r, r)) } else { MaskPath::rect(c.x - r, c.y - r, c.x + r, c.y + r) };
                let mut list = masks.to_vec();
                list.push(Mask::new(name, path));
                out = Some(list);
            }
        }
    });
    for (k, m) in masks.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(&m.name);
            let mut inv = m.inverted;
            let resp = ui.checkbox(&mut inv, crate::i18n::t("Inverted"));
            elems.push((format!("comp.mask{k}.inverted"), resp.rect, "Inverted".into()));
            if resp.changed() {
                let mut list = masks.to_vec();
                if let Some(x) = list.get_mut(k) {
                    x.inverted = inv;
                }
                out = Some(list);
            }
            let resp = ui.button(crate::i18n::t("Delete"));
            elems.push((format!("comp.mask{k}.delete"), resp.rect, "Delete".into()));
            if resp.clicked() {
                let mut list = masks.to_vec();
                if k < list.len() {
                    list.remove(k);
                }
                out = Some(list);
            }
        });
        egui::Grid::new(("comp-mask-grid", k)).num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            // feather / opacity / expansion (the path is drawn, not typed)
            for pd in filmcraft_project::mask::mask_param_defs().iter().filter(|pd| pd.id != "path") {
                let value = m.param(pd.id).map_or_else(|| pd.default.clone(), |p| p.value_at(at));
                let eid = format!("comp.mask{k}.{}", pd.id);
                ui.label(crate::i18n::t(pd.label));
                if let Some(c) = crate::panels::transition_controls::param_widget(ui, elems, &eid, pd.id, &pd.kind, &value, size)
                    && let Some(v) = c["params"][pd.id].as_f64().filter(|v| v.is_finite())
                {
                    let v = match pd.kind {
                        filmcraft_project::ParamKind::Float { min, max, .. } => v.clamp(min, max),
                        _ => v,
                    };
                    let mut list = masks.to_vec();
                    if let Some(p) = list.get_mut(k).and_then(|x| x.param_mut(pd.id)) {
                        p.set_at(at, filmcraft_project::ParamValue::Float(v));
                    }
                    out = Some(list);
                }
                ui.end_row();
            }
        });
    }
    out
}

/// Editing a Mask node's shapes on the Program monitor (C1 10b): while the Compositor is open and
/// one Mask node is selected, its masks are drawn over the picture (comp pixels through the comp
/// clip's Motion, or straight onto the picture while a node is viewed); dragging a vertex moves
/// it, dragging inside a shape moves the shape. Each change goes through `comp.setMasks` at comp
/// time (a keyframed path gets a keyframe). Id `comp.maskOverlay`.
pub fn mask_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    use egui::{Pos2, pos2};
    use filmcraft_geom::{Affine, Vec2};
    let kind = crate::dock::PanelKind::Compositor;
    if !app.ui.dock.contains(kind) && !app.ui.floating.iter().any(|g| g.panels.contains(&kind)) {
        return;
    }
    let p = &app.ui.panels.comp;
    // (C4.8: inside a group the monitor shows the comp's own result; no overlay there yet)
    if !p.path.is_empty() {
        return;
    }
    let (Some(comp), [node_id]) = (p.comp.map(ItemId), p.canvas.selection.as_slice()) else { return };
    let node_id = *node_id;
    let Some(ItemKind::Comp(c)) = app.session.project.item(comp).map(|i| &i.kind) else { return };
    let Some(CompKind::Mask { masks }) = c.node(node_id).map(|n| &n.kind) else { return };
    if masks.is_empty() {
        return;
    }
    let (masks, cw, ch) = (masks.clone(), c.width.max(1), c.height.max(1));
    let at = app.session.playhead();
    let Some(it) =
        app.session.active_sequence().and_then(|q| q.video_tracks.iter().flat_map(|t| &t.items).find(|i| i.item == comp && i.range().contains(at)).cloned())
    else {
        return;
    };
    let ct = it.source_time_at(at);
    // comp pixels → screen
    let to_screen = if viewer_target(app).is_some() {
        Affine::translate(f64::from(pic.min.x), f64::from(pic.min.y))
            .then_apply(&Affine::scale(f64::from(pic.width()) / f64::from(cw), f64::from(pic.height()) / f64::from(ch)))
    } else {
        let Some(seq) = app.session.active_sequence() else { return };
        let motion = filmcraft_render::motion_matrix(seq, &it, (cw, ch), None, it.effect_time_at(at));
        let (fw, fh) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
        Affine::translate(f64::from(pic.min.x), f64::from(pic.min.y))
            .then_apply(&Affine::scale(f64::from(pic.width()) / fw, f64::from(pic.height()) / fh))
            .then_apply(&motion)
    };
    let Some(to_comp) = to_screen.inverse() else { return };
    let lin = Affine { e: 0.0, f: 0.0, ..to_comp };
    let sp = |v: Vec2| {
        let q = to_screen.apply(v);
        pos2(q.x as f32, q.y as f32)
    };
    let col = egui::Color32::from_rgb(0x5a, 0xb4, 0xff);
    let painter = ui.painter().with_clip_rect(pic.expand(16.0));
    let paths: Vec<filmcraft_project::MaskPath> = masks.iter().map(|m| m.path_at(ct)).collect();
    // Bézier handles of smooth points (a linear mask's two points have none)
    let curved = |k: usize| masks.get(k).is_some_and(|m| m.shape != filmcraft_project::MaskShape::Linear);
    for (k, path) in paths.iter().enumerate() {
        let pts: Vec<Pos2> = path.flatten(0.5).into_iter().map(sp).collect();
        painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.5, col)));
        for v in path.vertices.iter().filter(|v| curved(k) && !v.is_corner()) {
            for tan in [v.t_in, v.t_out] {
                let h = sp(v.p + tan);
                painter.line_segment([sp(v.p), h], egui::Stroke::new(1.0, col.gamma_multiply(0.8)));
                painter.circle_filled(h, 3.5, egui::Color32::WHITE);
                painter.circle_stroke(h, 3.5, egui::Stroke::new(1.0, col));
            }
        }
        for v in &path.vertices {
            painter.rect_filled(Rect::from_center_size(sp(v.p), vec2(6.0, 6.0)), 0.0, col);
        }
    }
    let resp = ui.interact(pic, egui::Id::new("comp-mask-overlay"), egui::Sense::drag());
    app.auto.add("comp.maskOverlay", pic, "Comp masks");
    let grab_id = egui::Id::new("comp-mask-grab");
    let gesture_id = egui::Id::new("comp-mask-gesture");
    // one undo step per pointer press
    let mut gesture: u64 = ui.data(|d| d.get_temp(gesture_id)).unwrap_or(0);
    let alt = ui.input(|i| i.modifiers.alt);
    // the nearest point (handle 0) or Bézier handle (1 in, 2 out) within 8 points of `pos`
    let nearest = |pos: Pos2| {
        paths
            .iter()
            .enumerate()
            .flat_map(|(k, path)| {
                path.vertices.iter().enumerate().flat_map(move |(i, v)| {
                    let handles = if curved(k) && !v.is_corner() { vec![(1u8, v.p + v.t_in), (2, v.p + v.t_out)] } else { vec![] };
                    std::iter::once((0u8, v.p)).chain(handles).map(move |(h, q)| (k, i, h, q))
                })
            })
            .map(|(k, i, h, q)| (k, i, h, sp(q).distance(pos)))
            .filter(|x| x.3 < 8.0)
            // a point wins a tie with its own handle (a handle drawn on top of its point)
            .min_by(|a, b| a.3.total_cmp(&b.3).then(a.2.cmp(&b.2)))
            .map(|(k, i, h, _)| (k, i, h))
    };
    // (the overlay senses drags only, so clicks still reach the monitor; an Alt-click is read here)
    let click = ui.input(|i| i.pointer.primary_clicked().then(|| i.pointer.interact_pos()).flatten());
    if alt
        && resp.hovered()
        && let Some((k, i, 0)) = click.and_then(nearest)
        && curved(k)
    {
        // Alt-click a point: smooth ⇄ corner (one undo step)
        let mut list = masks.clone();
        if let Some(m) = list.get_mut(k) {
            let mut path = m.path_at(ct);
            if path.toggle_smooth(i) {
                m.path.set_at(ct, filmcraft_project::ParamValue::Path(path));
                gesture = gesture.wrapping_add(1);
                ui.data_mut(|d| d.insert_temp(gesture_id, gesture));
                run(app, "comp.setMasks", json!({"comp": comp.0, "node": node_id, "masks": list, "gesture": gesture}));
            }
        }
        return;
    }
    // Ctrl-click (Cmd on macOS) a point: delete it; on an edge: add a point there
    if ui.input(|i| i.modifiers.command)
        && resp.hovered()
        && let Some(pos) = click
    {
        let edit = match nearest(pos) {
            Some((k, i, 0)) => Some((k, Err(i))),
            _ => paths
                .iter()
                .enumerate()
                .filter(|(k, _)| curved(*k))
                .flat_map(|(k, path)| {
                    let n = path.vertices.len();
                    (0..n).filter_map(move |i| {
                        let (a, b) = (*path.vertices.get(i)?, *path.vertices.get((i + 1) % n)?);
                        let seg = filmcraft_project::MaskPath { vertices: vec![a, b], closed: false }.flatten(0.5);
                        let d = seg
                            .windows(2)
                            .filter_map(|w| match w {
                                [a, b] => Some(seg_dist(pos, sp(*a), sp(*b))),
                                _ => None,
                            })
                            .fold(f32::INFINITY, f32::min);
                        Some((k, i, d))
                    })
                })
                .filter(|x| x.2 < 8.0)
                .min_by(|a, b| a.2.total_cmp(&b.2))
                .map(|(k, i, _)| (k, Ok(i))),
        };
        if let Some((k, what)) = edit {
            let mut list = masks.clone();
            let done = list.get_mut(k).map(|m| match what {
                Ok(after) => m.add_vertex(after, to_comp.apply(Vec2::new(f64::from(pos.x), f64::from(pos.y))), ct),
                Err(i) => m.remove_vertex(i, ct),
            });
            match done {
                Some(Ok(())) => {
                    gesture = gesture.wrapping_add(1);
                    ui.data_mut(|d| d.insert_temp(gesture_id, gesture));
                    run(app, "comp.setMasks", json!({"comp": comp.0, "node": node_id, "masks": list, "gesture": gesture}));
                }
                Some(Err(e)) => app.ui.status = e.to_string(),
                None => {}
            }
        }
        return;
    }
    if resp.drag_started() {
        gesture = gesture.wrapping_add(1);
        ui.data_mut(|d| d.insert_temp(gesture_id, gesture));
    }
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        // the nearest point or handle within 8 points, else the shape the pointer is in
        let near = nearest(pos).map(|(k, i, h)| (k, Some((i, h))));
        let q = to_comp.apply(Vec2::new(f64::from(pos.x), f64::from(pos.y)));
        let inside = || paths.iter().position(|path| contains(&path.flatten(0.5), q)).map(|k| (k, None));
        ui.data_mut(|d| d.insert_temp(grab_id, near.or_else(inside)));
    }
    type Grab = Option<(usize, Option<(usize, u8)>)>;
    let grab: Grab = ui.data(|d| d.get_temp(grab_id)).flatten();
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<Grab>(grab_id));
    }
    let d = resp.drag_delta();
    let (Some((k, vertex)), true) = (grab, resp.dragged() && d != egui::Vec2::ZERO) else { return };
    let delta = lin.apply(Vec2::new(f64::from(d.x), f64::from(d.y)));
    let mut list = masks;
    let Some(m) = list.get_mut(k) else { return };
    let mut path = m.path_at(ct);
    match vertex {
        Some((i, h)) => {
            if let Some(v) = path.vertices.get_mut(i) {
                match h {
                    0 => v.p = v.p + delta,
                    // a handle: its tangent follows the pointer; Alt moves it alone (a cusp)
                    _ => {
                        let tan = if h == 2 { v.t_out } else { v.t_in };
                        v.set_tangent(h == 2, tan + delta, alt);
                    }
                }
            }
        }
        None => path.vertices.iter_mut().for_each(|v| v.p = v.p + delta),
    }
    m.path.set_at(ct, filmcraft_project::ParamValue::Path(path));
    run(app, "comp.setMasks", json!({"comp": comp.0, "node": node_id, "masks": list, "gesture": gesture}));
}

/// Distance from `p` to the segment `a`–`b`.
fn seg_dist(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    let t = if l2 > 0.0 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + ab * t)).length()
}

/// Whether `p` is inside the closed polygon `pts` (nonzero winding).
pub(crate) fn contains(pts: &[filmcraft_geom::Vec2], p: filmcraft_geom::Vec2) -> bool {
    let n = pts.len();
    let mut w = 0i32;
    for i in 0..n {
        let (Some(a), Some(b)) = (pts.get(i), pts.get((i + 1) % n)) else { continue };
        if (a.y <= p.y) != (b.y <= p.y) && a.x + (b.x - a.x) * (p.y - a.y) / (b.y - a.y) < p.x {
            w += if b.y > a.y { 1 } else { -1 };
        }
    }
    w != 0
}

/// A Group node's rows (C4.3): its exposed parameters (`comp.param.<id>` and its stopwatch, like
/// any other row, through `comp.setParam` / `comp.toggleAnimation` on the exposed id), or a note
/// when it has none.
fn group_rows(
    ui: &mut egui::Ui,
    elems: &mut crate::panels::transition_controls::Elems,
    (changes, toggles): (&mut Vec<serde_json::Value>, &mut Vec<serde_json::Value>),
    comp: u64,
    node: &CompNode,
    at: filmcraft_time::Tick,
    size: (f64, f64),
) {
    if !matches!(node.kind, CompKind::Group { .. }) {
        return;
    }
    let rows = comp::group::exposed_params(node);
    if rows.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::t("No settings on this group yet: open it and expose an inner node's setting.")).weak());
        return;
    }
    egui::Grid::new("comp-group-grid").num_columns(3).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
        for (e, pd) in rows {
            let (value, animated) = match node.params.get(&e.id) {
                Some(p) => (p.value_at(at), p.is_animated()),
                None => (pd.default.clone(), false),
            };
            let (sw, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::click());
            if pd.animatable {
                let col = if animated { ui.visuals().selection.stroke.color } else { ui.visuals().weak_text_color() };
                crate::icons::paint(ui.painter(), sw, crate::icons::Icon::Stopwatch, col);
                elems.push((format!("comp.param.{}.stopwatch", e.id), sw, "Toggle animation".into()));
                if resp.on_hover_text(crate::i18n::t("Toggle animation")).clicked() {
                    toggles.push(json!({"comp": comp, "node": node.id, "param": e.id, "time": at.0}));
                }
            }
            ui.label(if e.label.is_empty() { crate::i18n::t(pd.label).to_string() } else { e.label.clone() });
            let eid = format!("comp.param.{}", e.id);
            if let Some(c) = crate::panels::transition_controls::param_widget(ui, elems, &eid, &e.id, &pd.kind, &value, size) {
                let mut p = json!({"comp": comp, "node": node.id, "param": e.id, "value": c["params"][e.id.as_str()].clone()});
                if animated {
                    p["time"] = json!(at.0);
                }
                changes.push(p);
            }
            ui.end_row();
        }
    });
}
