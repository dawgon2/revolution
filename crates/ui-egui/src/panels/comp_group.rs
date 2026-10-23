//! Node groups on the Compositor canvas (fork C4.6, ADR 0009 §2): Group / Ungroup / Open Group
//! (the toolbar over the canvas, `comp.canvas.group`, `comp.canvas.ungroup`,
//! `comp.canvas.enterGroup`, and the node menu), double-click a group to open it, and a breadcrumb
//! (`comp.canvas.breadcrumb.<depth>`) back out. Which group is open is UI navigation state only
//! (`ui.panels.comp.path`, serde). Inside a group the canvas edits like the comp's own level: the
//! comp commands get the path as `group` (C4.8), the background menu also adds Group Inputs, and
//! the inspector exposes an inner node's setting on the open group with `comp.group.expose`.

use std::sync::Arc;

use egui::{Rect, UiBuilder, vec2};
use filmcraft_project::comp::{self, Comp, CompKind, CompNode};
use filmcraft_project::{ItemId, ItemKind};
use serde_json::json;

use crate::FilmcraftApp;
use crate::widgets::node_canvas::CanvasGraph;

fn run(app: &mut FilmcraftApp, command: &str, params: serde_json::Value) -> Option<serde_json::Value> {
    match app.session.execute(command, params) {
        Ok(v) => Some(v),
        Err(e) => {
            app.session.toast(e.to_string());
            None
        }
    }
}

fn comp_of(app: &FilmcraftApp, comp: ItemId) -> Option<Arc<Comp>> {
    match &app.session.project.item(comp)?.kind {
        ItemKind::Comp(c) => Some(Arc::clone(c)),
        _ => None,
    }
}

/// The comp the canvas shows: `comp` itself, or the inside of the group at `path`.
fn level_of(app: &FilmcraftApp, comp: ItemId, path: &[u64]) -> Option<Comp> {
    let c = comp_of(app, comp)?;
    if path.is_empty() {
        return Some((*c).clone());
    }
    comp::group::inner(&c, path).ok().cloned()
}

fn is_group(c: &Comp, node: u64) -> bool {
    c.node(node).is_some_and(|n| matches!(n.kind, CompKind::Group { .. }))
}

fn name(n: &CompNode) -> String {
    if n.label.is_empty() { format!("{} {}", crate::i18n::t("Group"), n.id) } else { n.label.clone() }
}

/// How many entries of `path` lead through groups of `top` (the rest is stale).
pub(crate) fn valid_depth(top: &Comp, path: &[u64]) -> usize {
    let mut cur = top;
    for (k, id) in path.iter().enumerate() {
        match cur.node(*id).map(|n| &n.kind) {
            Some(CompKind::Group { comp, .. }) if k < comp::MAX_GROUP_DEPTH => cur = comp,
            _ => return k,
        }
    }
    path.len()
}

/// Opens group `node` of the comp shown (one level further in).
pub(crate) fn enter(app: &mut FilmcraftApp, node: u64) {
    let p = &mut app.ui.panels.comp;
    p.path.push(node);
    p.canvas.selection.clear();
    p.canvas.viewed = None;
}

/// The node menu's group entries on the canvas of comp level `c` (Group on every node, Ungroup
/// and Open Group on group nodes only); `inside` a group the background menu also adds a Group
/// Input.
pub(crate) fn actions(g: &mut CanvasGraph, c: &Comp, inside: bool) {
    g.node_actions.push(("group".into(), "Group".into()));
    for v in &mut g.nodes {
        if is_group(c, v.id) {
            v.actions.push(("ungroup".into(), "Ungroup".into()));
            v.actions.push(("enterGroup".into(), "Open Group".into()));
        }
    }
    if inside {
        g.add_kinds.push(("groupInput".into(), "Group Input".into()));
    }
}

/// A node menu entry chosen on `node` of comp `comp` (inside the group at `path`).
pub(crate) fn node_action(app: &mut FilmcraftApp, comp: ItemId, path: &[u64], node: u64, action: &str) {
    match action {
        "group" => {
            let mut nodes = app.ui.panels.comp.canvas.selection.clone();
            if !nodes.contains(&node) {
                nodes = vec![node];
            }
            group(app, comp, path, nodes);
        }
        "ungroup" => ungroup(app, comp, path, node),
        "enterGroup" if level_of(app, comp, path).is_some_and(|c| is_group(&c, node)) => enter(app, node),
        "enterGroup" => app.session.toast(crate::i18n::t("That node is not a group.").to_string()),
        _ => {}
    }
}

fn group(app: &mut FilmcraftApp, comp: ItemId, path: &[u64], nodes: Vec<u64>) {
    if let Some(v) = run(app, "comp.group", super::comp::scoped(json!({"comp": comp.0, "nodes": nodes}), path)) {
        app.ui.panels.comp.canvas.selection = v["node"].as_u64().into_iter().collect();
    }
}

fn ungroup(app: &mut FilmcraftApp, comp: ItemId, path: &[u64], node: u64) {
    if let Some(v) = run(app, "comp.ungroup", super::comp::scoped(json!({"comp": comp.0, "node": node}), path)) {
        app.ui.panels.comp.canvas.selection = v["nodes"].as_array().map(|a| a.iter().filter_map(serde_json::Value::as_u64).collect()).unwrap_or_default();
    }
}

/// The Group / Ungroup / Open Group buttons over the canvas (top left), at the level `path` shows.
pub(crate) fn toolbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, comp: ItemId, path: &[u64]) {
    let Some(c) = level_of(app, comp, path) else { return };
    let sel = app.ui.panels.comp.canvas.selection.clone();
    let one_group = match sel.as_slice() {
        [n] if is_group(&c, *n) => Some(*n),
        _ => None,
    };
    let r = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(360.0, 22.0));
    let mut clicked = None;
    ui.scope_builder(UiBuilder::new().max_rect(r), |ui| {
        ui.horizontal(|ui| {
            let buttons = [
                ("group", "comp.canvas.group", "Group", !sel.is_empty()),
                ("ungroup", "comp.canvas.ungroup", "Ungroup", one_group.is_some()),
                ("enterGroup", "comp.canvas.enterGroup", "Open Group", one_group.is_some()),
            ];
            for (key, id, label, on) in buttons {
                let b = ui.add_enabled(on, egui::Button::new(crate::i18n::t(label)).small());
                app.auto.add_response(id, &b, label);
                if b.clicked() {
                    clicked = Some(key);
                }
            }
        });
    });
    match (clicked, one_group) {
        (Some("group"), _) => group(app, comp, path, sel),
        (Some("ungroup"), Some(g)) => ungroup(app, comp, path, g),
        (Some("enterGroup"), Some(g)) => enter(app, g),
        _ => {}
    }
}

/// The breadcrumb over the canvas inside an open group (`comp.canvas.breadcrumb.<depth>`: the
/// comp, then each group along the path). Returns the canvas area under it, or None when a crumb
/// was clicked (the path is cut back to it; the panel redraws next frame).
pub(crate) fn breadcrumb(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, top: &Comp) -> Option<Rect> {
    let path = app.ui.panels.comp.path.clone();
    let mut cur = top;
    let mut crumbs: Vec<String> = vec![crate::i18n::t("Comp").to_string()];
    for id in &path {
        let Some(n) = cur.node(*id) else { break };
        let CompKind::Group { comp: inner, .. } = &n.kind else { break };
        crumbs.push(name(n));
        cur = inner;
    }
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 28.0));
    let mut back: Option<usize> = None;
    ui.scope_builder(UiBuilder::new().max_rect(bar.shrink2(vec2(6.0, 3.0))), |ui| {
        ui.horizontal(|ui| {
            for (k, c) in crumbs.iter().enumerate() {
                if k > 0 {
                    ui.label("▸");
                }
                let last = k + 1 == crumbs.len();
                let b = ui.add_enabled(!last, egui::Button::new(c.as_str()).small().frame(false));
                app.auto.add_response(&format!("comp.canvas.breadcrumb.{k}"), &b, c);
                if b.clicked() {
                    back = Some(k);
                }
            }
        });
    });
    if let Some(k) = back {
        let p = &mut app.ui.panels.comp;
        p.path.truncate(k);
        p.canvas.selection.clear();
        ui.ctx().request_repaint();
        return None;
    }
    Some(Rect::from_min_max(egui::pos2(rect.min.x, bar.max.y), rect.max))
}

/// One inner node's settings with an Expose button each (`comp.expose.<param>`): exposed on the
/// open group (the last of `path`, at any depth).
pub(crate) fn expose_rows(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, comp: ItemId, path: &[u64], n: &CompNode) {
    let Some((&group, parent)) = path.split_last() else { return };
    let exposed: Vec<(u64, String)> = level_of(app, comp, parent)
        .and_then(|c| c.node(group).cloned())
        .map(|gn| match gn.kind {
            CompKind::Group { exposed, .. } => exposed.into_iter().map(|e| (e.node, e.param)).collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default();
    // (id, label) of what can be exposed: schema settings, or a nested group's own exposed ones
    let rows: Vec<(String, String)> = match &n.kind {
        CompKind::Group { .. } => {
            comp::group::exposed_params(n).into_iter().map(|(e, d)| (e.id.clone(), if e.label.is_empty() { d.label.to_string() } else { e.label })).collect()
        }
        kind => comp::params(kind).into_iter().map(|d| (d.id.to_string(), crate::i18n::t(d.label).to_string())).collect(),
    };
    let mut expose: Option<String> = None;
    ui.painter().line_segment([rect.left_top(), rect.right_top()], egui::Stroke::new(1.0, app.tokens.separator));
    ui.scope_builder(UiBuilder::new().max_rect(rect.shrink(8.0)), |ui| {
        ui.label(egui::RichText::new(crate::i18n::t("Put on the group")).strong());
        if rows.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::t("This node has no settings that can be exposed.")).weak());
        }
        egui::ScrollArea::vertical().id_salt("comp-expose").show(ui, |ui| {
            egui::Grid::new("comp-expose-grid").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                for (id, label) in &rows {
                    ui.label(label);
                    let done = exposed.iter().any(|(node, param)| *node == n.id && param == id);
                    let b = ui.add_enabled(!done, egui::Button::new(crate::i18n::t(if done { "Exposed" } else { "Expose" })).small());
                    app.auto.add_response(&format!("comp.expose.{id}"), &b, "Expose");
                    if b.clicked() {
                        expose = Some(id.clone());
                    }
                    ui.end_row();
                }
            });
        });
    });
    if let Some(param) = expose {
        run(app, "comp.group.expose", super::comp::scoped(json!({"comp": comp.0, "node": group, "inner": n.id, "param": param}), parent));
    }
}
