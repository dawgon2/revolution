//! Comp commands (`comp.*`, fork C1, ADR 0007): build and edit a comp's node graph. Every edit is
//! one undo step through [`Session::edit`] (a canvas drag or a slider drag through
//! [`Session::edit_merged`]).
//!
//! Commands address the comp **project item** by `comp`; without it, the selected comp clip's item
//! (the first selected clip whose item is a comp). Nodes are addressed by their stable `node` id;
//! with `group` (a path of group node ids, outer first, C4.8) the node-editing commands work inside
//! that group ([`edit_comp`], [`comp_at`]).
//! Refusals are `BadParams` with the reason; a refused command changes nothing.

use std::sync::Arc;

use filmcraft_project::comp::{self, Comp, CompKind, CompNode, MAX_LABEL, MediaInSource};
use filmcraft_project::dag::{self, DagError, Edge, NodeId, Port};
use filmcraft_project::grade::{NodeParamKind, node_param};
use filmcraft_project::{ItemId, ItemKind, Label, Param, ParamKind, ParamValue, Project};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, checked_u32_p, fps_p, json_to_param, str_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: true }
}
fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("comp.new", "New Comp", r#"{"name":str?,"width":n?,"height":n?,"fps":n?,"duration":ticks?}"#, new),
        query("comp.get", "Get Comp", r#"{"comp":id?,"group":[id]?}"#, get),
        query("comp.params", "List Comp Node Parameters", r#"{"kind":str}"#, params),
        query("comp.explain", "Explain Comp", r#"{"comp":id?,"time":ticks?}"#, explain),
        spec("comp.addNode", "Add Node", r#"{"comp":id?,"group":[id]?,"kind":str (or "groupInput" inside a group),"index":n? (groupInput),"label":str?,"pos":[x,y]?,"after":node?,"item":id?,"track":n?,"effect":str?}"#, add_node),
        spec("comp.removeNode", "Delete Node", r#"{"comp":id?,"group":[id]?,"nodes":[id]}"#, remove_nodes),
        spec("comp.connect", "Connect", r#"{"comp":id?,"group":[id]?,"from":node,"fromPort":n?,"to":node,"toPort":n?}"#, connect),
        spec("comp.disconnect", "Disconnect", r#"{"comp":id?,"group":[id]?,"to":node,"toPort":n?}"#, disconnect),
        spec(
            "comp.setParam",
            "Set Node Parameter",
            r#"{"comp":id?,"group":[id]?,"node":id,"param":str,"value":any?,"reset":bool?,"time":ticks?,"keyframes":[{"time":ticks,"value":any}]?}"#,
            set_param,
        ),
        spec("comp.toggleAnimation", "Toggle Node Parameter Animation", r#"{"comp":id?,"group":[id]?,"node":id,"param":str,"time":ticks?}"#, toggle_animation),
        spec("comp.setSource", "Set Media In Source", r#"{"comp":id?,"group":[id]?,"node":id,"item":id?,"track":n?,"offset":ticks?}"#, set_source),
        spec("comp.bypass", "Bypass Node", r#"{"comp":id?,"group":[id]?,"node":id,"on":bool?}"#, bypass),
        spec("comp.labelNode", "Label Node", r#"{"comp":id?,"group":[id]?,"node":id,"label":str}"#, label_node),
        spec("comp.moveNode", "Move Node", r#"{"comp":id?,"group":[id]?,"node":id,"pos":[x,y]}"#, move_node),
        spec("comp.setOutput", "Set Media Out", r#"{"comp":id?,"group":[id]?,"node":id}"#, set_output),
        spec("comp.setSize", "Comp Settings", r#"{"comp":id?,"width":n?,"height":n?,"fps":n?,"duration":ticks?}"#, set_size),
        spec("comp.setMasks", "Set Mask Node Masks", r#"{"comp":id?,"group":[id]?,"node":id,"masks":[mask],"gesture":n?}"#, set_masks),
        CommandSpec {
            id: "comp.newClip",
            label: "New Comp Clip",
            menu: &["Clip", "Compositor"],
            shortcut: None,
            params: r#"{"comp":id?,"track":"V1"|id?,"time":ticks?|"seconds":f64?}"#,
            enabled: crate::commands::has_seq,
            run: new_clip,
            journal: true,
        },
        CommandSpec {
            id: "comp.convertToCompClip",
            label: "Convert to Comp Clip",
            menu: &["Clip", "Compositor"],
            shortcut: None,
            params: r#"{"clips":[id]?,"name":str?}"#,
            enabled: crate::commands::has_selection,
            run: convert_to_comp_clip,
            journal: true,
        },
    ]
    .into_iter()
    .chain(roto_paint::commands())
    .chain(expression::commands())
    .collect()
}

// C3.5b: the Roto and Paint node commands (`comp.setRoto`, `comp.roto.*`, `comp.setPaint`,
// `comp.paint.*`)
mod roto_paint;

// C4.5: parameter expressions (`comp.setExpression`, `comp.clearExpression`)
pub(crate) mod expression;

/// The comp item a command addresses: `comp`, else the first selected clip whose item is a comp.
pub(crate) fn target(s: &Session, p: &Value, cmd: &str) -> Result<ItemId> {
    if p.get("comp").is_some() {
        let id = u64_p(p, "comp").map(ItemId).ok_or_else(|| bad(cmd, "`comp` must be a project item id"))?;
        return match s.project.item(id).map(|i| &i.kind) {
            Some(ItemKind::Comp(_)) => Ok(id),
            Some(_) => Err(bad(cmd, format!("item {} is not a comp", id.0))),
            None => Err(bad(cmd, format!("no project item {}", id.0))),
        };
    }
    let q = s.active_sequence();
    s.state
        .selection
        .iter()
        .filter_map(|c| q.and_then(|q| q.find_item(*c)).map(|(_, i)| i.item))
        .find(|i| matches!(s.project.item(*i).map(|it| &it.kind), Some(ItemKind::Comp(_))))
        .ok_or_else(|| bad(cmd, "need `comp` (or a selected comp clip)"))
}

pub(crate) fn comp_of(p: &Project, id: ItemId) -> Option<&Comp> {
    match &p.item(id)?.kind {
        ItemKind::Comp(c) => Some(c),
        _ => None,
    }
}

/// The optional `group` param (fork C4.8): the path to a group's inside, outer group node id
/// first, then nested group ids (at most [`comp::MAX_GROUP_DEPTH`]); empty when absent (the
/// comp's own nodes). A single id is a one-group path.
pub(crate) fn group_p(p: &Value, cmd: &str) -> Result<Vec<NodeId>> {
    let wrong = || bad(cmd, "`group` must be a list of group node ids (the outer group first)");
    match p.get("group") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => {
            if a.is_empty() {
                return Err(bad(cmd, "`group`: the group path is empty; leave it out to edit the comp's own nodes"));
            }
            if a.len() > comp::MAX_GROUP_DEPTH {
                return Err(bad(cmd, format!("`group`: groups nest at most {} deep, so a group path has at most that many node ids", comp::MAX_GROUP_DEPTH)));
            }
            a.iter().map(|v| v.as_u64().ok_or_else(wrong)).collect()
        }
        Some(v) => v.as_u64().map(|id| vec![id]).ok_or_else(wrong),
    }
}

/// The comp a command works on: the addressed comp item, or the inside of the group its `group`
/// path names.
pub(crate) fn comp_at<'a>(s: &'a Session, p: &Value, cmd: &str) -> Result<(ItemId, &'a Comp)> {
    let id = target(s, p, cmd)?;
    let path = group_p(p, cmd)?;
    let c = comp_of(&s.project, id).ok_or_else(|| bad(cmd, "not a comp"))?;
    if path.is_empty() {
        return Ok((id, c));
    }
    let inner = comp::group::inner(c, &path).map_err(|e| bad(cmd, format!("`group`: {e}")))?;
    Ok((id, inner))
}

/// Refuses a command addressing a group's inside where that isn't supported yet.
pub(crate) fn top_level_only(p: &Value, cmd: &str, what: &str) -> Result<()> {
    if group_p(p, cmd)?.is_empty() {
        return Ok(());
    }
    Err(bad(cmd, format!("{what} works on the comp's own nodes only for now; ungroup the nodes to do it")))
}

/// Runs `f` on the addressed comp (the inside of the group its `group` path names, C4.8) as one
/// undo step (merged with the previous step of the same `merge` key). Refused when the edit takes
/// the whole comp past its node cap or nesting depth; the comp is brought within its caps
/// afterwards.
pub(crate) fn edit_comp<R>(s: &mut Session, p: &Value, cmd: &str, label: &str, merge: Option<String>, f: impl FnOnce(&mut Comp) -> Result<R>) -> Result<R> {
    let id = target(s, p, cmd)?;
    let path = group_p(p, cmd)?;
    let merge = merge.map(|k| if path.is_empty() { k } else { format!("{k}@{path:?}") });
    let cmd = cmd.to_string();
    let body = move |proj: &mut Project, _: &mut crate::EditorState| -> Result<R> {
        let it = proj.item_mut(id).ok_or_else(|| bad(&cmd, "the comp is gone"))?;
        let ItemKind::Comp(c) = &mut it.kind else { return Err(bad(&cmd, "not a comp")) };
        let c = Arc::make_mut(c);
        let (count, depth) = (c.all_nodes().len(), comp::group::nesting(c));
        let r = if path.is_empty() { f(c)? } else { f(comp::group::inner_mut(c, &path).map_err(|e| bad(&cmd, format!("`group`: {e}")))?)? };
        comp::group::prune_group_inputs(c, &path);
        // the whole tree's bounds (an edit inside a group sees only its level)
        let now = c.all_nodes().len();
        if now > dag::MAX_NODES && now > count {
            return Err(bad(&cmd, format!("a comp holds at most {} nodes (groups' insides included)", dag::MAX_NODES)));
        }
        let nest = comp::group::nesting(c);
        if nest > comp::MAX_GROUP_DEPTH && nest > depth {
            return Err(bad(&cmd, format!("groups nest at most {} deep", comp::MAX_GROUP_DEPTH)));
        }
        c.sanitize();
        Ok(r)
    };
    match merge {
        Some(key) => s.edit_merged(label, &key, body),
        None => s.edit(label, body),
    }
}

fn node_p(p: &Value, key: &str, cmd: &str) -> Result<NodeId> {
    u64_p(p, key).ok_or_else(|| bad(cmd, format!("need `{key}` (a node id)")))
}

fn port_p(p: &Value, key: &str, cmd: &str) -> Result<u8> {
    match p.get(key) {
        None => Ok(0),
        Some(v) => v.as_u64().and_then(|n| u8::try_from(n).ok()).ok_or_else(|| bad(cmd, format!("`{key}` must be a port number"))),
    }
}

fn ticks_p(p: &Value, key: &str, cmd: &str) -> Result<Option<Tick>> {
    match p.get(key) {
        None => Ok(None),
        Some(v) => v.as_i64().map(|t| Some(Tick(t))).ok_or_else(|| bad(cmd, format!("`{key}` must be whole ticks ({TICKS_PER_SECOND}/s)"))),
    }
}

fn node_mut<'a>(c: &'a mut Comp, id: NodeId, cmd: &str) -> Result<&'a mut CompNode> {
    c.node_mut(id).ok_or_else(|| bad(cmd, format!("no node {id}")))
}

/// Refuse a graph the edit made cyclic or too deep (checked from `at`, a node that exists).
fn check_graph(c: &Comp, at: NodeId, cmd: &str) -> Result<()> {
    match dag::validate(&c.nodes, &c.edges, at) {
        Err(e @ (DagError::Cycle(_) | DagError::TooDeep | DagError::TooManyEdges | DagError::TooManyNodes)) => Err(bad(cmd, e.to_string())),
        _ => Ok(()),
    }
}

fn new(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.new";
    let q = s.active_sequence();
    let width = checked_u32_p(p, "width", CMD)?.or(q.map(|q| q.settings.width)).unwrap_or(1920);
    let height = checked_u32_p(p, "height", CMD)?.or(q.map(|q| q.settings.height)).unwrap_or(1080);
    if width == 0 || height == 0 || width > comp::MAX_SIDE || height > comp::MAX_SIDE {
        return Err(bad(CMD, format!("width and height must be 1..={}", comp::MAX_SIDE)));
    }
    let rate = fps_p(p, CMD)?.or(q.map(|q| q.settings.frame_rate)).unwrap_or(FrameRate::FPS_24);
    let duration = ticks_p(p, "duration", CMD)?.unwrap_or(Tick(10 * TICKS_PER_SECOND));
    if duration <= Tick::ZERO {
        return Err(bad(CMD, "duration must be positive"));
    }
    let color = q.map(|q| q.settings.color);
    let name = str_p(p, "name").filter(|n| !n.trim().is_empty()).unwrap_or("Comp").chars().take(256).collect::<String>();
    let id = s.edit("New Comp", |proj, _| {
        let mut c = Comp::new(width, height, rate, duration);
        if let Some(color) = color {
            c.color = color;
        }
        Ok(proj.add_item(&name, Label::Iris, ItemKind::Comp(Arc::new(c)), None))
    })?;
    Ok(json!({ "comp": id.0, "output": 1 }))
}

fn get(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.get";
    // C4.8: `group` returns that group's inside
    let (id, c) = comp_at(s, p, CMD)?;
    let (order, error) = match c.validate() {
        Ok(o) => (o.nodes().collect::<Vec<_>>(), Value::Null),
        Err(e) => (Vec::new(), Value::String(e.to_string())),
    };
    let mut v = serde_json::to_value(c).map_err(|e| EngineError::Other(e.to_string()))?;
    if let Some(o) = v.as_object_mut() {
        o.insert("comp".into(), json!(id.0));
        o.insert("order".into(), json!(order));
        o.insert("error".into(), error);
        // C4.3: each group node's parameters (its exposed ones, with the inner schema and value)
        let groups: serde_json::Map<String, Value> = c
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, CompKind::Group { .. }))
            .map(|n| {
                let list: Vec<Value> = comp::group::exposed_params(n).into_iter().map(|(e, d)| json!({"exposed": e, "def": d})).collect();
                (n.id.to_string(), Value::Array(list))
            })
            .collect();
        if !groups.is_empty() {
            o.insert("groupParams".into(), Value::Object(groups));
        }
    }
    Ok(v)
}

/// What the comp does, node by node (C1 item 7): template sentences from the graph itself.
fn explain(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.explain";
    let id = target(s, p, CMD)?;
    let t = ticks_p(p, "time", CMD)?.unwrap_or(Tick::ZERO);
    let c = comp_of(&s.project, id).ok_or_else(|| bad(CMD, "not a comp"))?;
    let e = crate::comp_explain::explain(c, &s.project, t);
    let lines = |v: &[crate::comp_explain::Line]| v.iter().map(|l| json!({"node": l.node, "name": l.name, "text": l.text})).collect::<Vec<_>>();
    // C4.5: each parameter expression, its value now and its error (the keyframed value is used then)
    let exprs: Vec<Value> = crate::comp_explain::expressions(c, t)
        .iter()
        .map(|x| {
            json!({
                "node": x.node,
                "name": x.name,
                "param": x.param,
                "expression": x.expression,
                "value": serde_json::to_value(&x.value).unwrap_or(Value::Null),
                "error": x.error.as_ref().map(expression::error_json),
            })
        })
        .collect();
    Ok(json!({"comp": id.0, "summary": e.summary, "lines": lines(&e.lines), "unused": lines(&e.unused), "expressions": exprs}))
}

fn params(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.params";
    let key = str_p(p, "kind").ok_or_else(|| bad(CMD, format!("need `kind` (one of {:?})", comp::KINDS)))?;
    let kind = CompKind::from_key(key).ok_or_else(|| bad(CMD, format!("unknown kind `{key}` (one of {:?})", comp::KINDS)))?;
    match &kind {
        CompKind::Color { .. } => crate::grade::params(s, p),
        CompKind::Text { layer } => Ok(json!(layer.def().map(|d| d.params.clone()).unwrap_or_default())),
        _ => Ok(json!(comp::params(&kind))),
    }
}

fn pos_p(p: &Value, cmd: &str) -> Result<Option<[f32; 2]>> {
    let Some(v) = p.get("pos") else { return Ok(None) };
    let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, "`pos` must be [x, y]"))?;
    let f = |i: usize| a.get(i).and_then(Value::as_f64).filter(|f| f.is_finite() && f.abs() <= 1e6).map(|f| f as f32);
    match (f(0), f(1)) {
        (Some(x), Some(y)) => Ok(Some([x, y])),
        _ => Err(bad(cmd, "`pos` must be two finite numbers")),
    }
}

/// A Media In source from `item` / `track`.
fn source_p(s: &Session, p: &Value, cmd: &str) -> Result<Option<MediaInSource>> {
    if let Some(item) = p.get("item") {
        let id = item.as_u64().map(ItemId).ok_or_else(|| bad(cmd, "`item` must be a project item id"))?;
        let it = s.project.item(id).ok_or_else(|| bad(cmd, format!("no project item {}", id.0)))?;
        if !it.has_video() {
            return Err(bad(cmd, format!("`{}` has no picture", it.name)));
        }
        return Ok(Some(MediaInSource::Item { item: id }));
    }
    if p.get("track").is_some() {
        let track = checked_u32_p(p, "track", cmd)?.ok_or_else(|| bad(cmd, "`track` must be a video track index"))?;
        return Ok(Some(MediaInSource::Layer { track }));
    }
    Ok(None)
}

fn add_node(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.addNode";
    let key = str_p(p, "kind").ok_or_else(|| bad(CMD, format!("need `kind` (one of {:?}, or groupInput inside a group)", comp::KINDS)))?;
    let inside = !group_p(p, CMD)?.is_empty();
    // C4.8: a Group Input (inside a group only) reads the group node's input `index`
    let group_input = if key == "groupInput" {
        if !inside {
            return Err(bad(CMD, "a Group Input goes inside a group (pass `group`, the path to it)"));
        }
        match checked_u32_p(p, "index", CMD)? {
            Some(i) if i >= comp::group::MAX_GROUP_INPUTS => {
                return Err(bad(
                    CMD,
                    format!("`index` must be 0..{} (a group has at most {} inputs)", comp::group::MAX_GROUP_INPUTS, comp::group::MAX_GROUP_INPUTS),
                ));
            }
            i => Some(i),
        }
    } else {
        None
    };
    let mut kind = match group_input {
        Some(_) => CompKind::MediaIn { source: MediaInSource::GroupInput { index: 0 }, offset: Tick::ZERO },
        None => CompKind::from_key(key).ok_or_else(|| bad(CMD, format!("unknown kind `{key}` (one of {:?}, or groupInput inside a group)", comp::KINDS)))?,
    };
    match &mut kind {
        CompKind::MediaIn { .. } if group_input.is_some() => {}
        CompKind::MediaIn { source, .. } => {
            if let Some(src) = source_p(s, p, CMD)? {
                *source = src;
            }
        }
        CompKind::Key { effect } => {
            if let Some(e) = str_p(p, "effect") {
                if !comp::KEY_EFFECTS.contains(&e) {
                    return Err(bad(CMD, format!("`effect` must be one of {:?}", comp::KEY_EFFECTS)));
                }
                *effect = e.to_string();
            }
        }
        _ => {}
    }
    let label = str_p(p, "label").map(|l| l.chars().take(MAX_LABEL).collect::<String>()).unwrap_or_default();
    let pos = pos_p(p, CMD)?;
    let after = u64_p(p, "after");
    // a comp item read by this one must not read it back (checked by `Session::edit`)
    let id = edit_comp(s, p, CMD, "Add Node", None, |c| {
        // (groups' inner nodes count too, C4.3)
        if c.all_nodes().len() >= dag::MAX_NODES {
            return Err(bad(CMD, format!("a comp holds at most {} nodes (groups' insides included)", dag::MAX_NODES)));
        }
        let id = c.next_id();
        let ins = kind.inputs();
        let mut n = CompNode::new(id, kind);
        n.label = label;
        if let (Some(want), CompKind::MediaIn { source: MediaInSource::GroupInput { index }, .. }) = (group_input, &mut n.kind) {
            // without `index`: the next input the group doesn't read yet
            *index = want.unwrap_or_else(|| u32::from(comp::group::input_count(c)).min(comp::group::MAX_GROUP_INPUTS - 1));
            if n.label.is_empty() {
                n.label = format!("Input {}", index.saturating_add(1));
            }
        }
        if let CompKind::Color { node } = &mut n.kind {
            node.id = 1;
        }
        n.pos = pos.or_else(|| after.and_then(|a| c.node(a)).map(|a| [a.pos[0] + 160.0, a.pos[1]])).unwrap_or_else(|| {
            // not on top of another node: step down until the spot is free (bounded by the node cap)
            let taken = |p: [f32; 2]| c.nodes.iter().any(|m| (m.pos[0] - p[0]).abs() < 1.0 && (m.pos[1] - p[1]).abs() < 1.0);
            (0..=c.nodes.len()).map(|k| [0.0, k as f32 * 60.0]).find(|p| !taken(*p)).unwrap_or([0.0, 0.0])
        });
        c.nodes.push(n);
        if let Some(a) = after {
            if c.node(a).is_none() {
                return Err(bad(CMD, format!("no node {a}")));
            }
            // splice into `a`'s output: its consumers now read the new node
            if ins > 0 {
                for e in c.edges.iter_mut().filter(|e| e.from.node == a) {
                    e.from = Port::new(id, 0);
                }
                c.edges.push(Edge::new(Port::new(a, 0), Port::new(id, 0)));
                if c.output == Some(a) {
                    c.output = Some(id);
                }
            }
            check_graph(c, id, CMD)?;
        }
        Ok(id)
    })?;
    Ok(json!({ "node": id }))
}

fn remove_nodes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.removeNode";
    let ids: Vec<NodeId> = match p.get("nodes").or_else(|| p.get("node")) {
        Some(Value::Array(a)) => a.iter().map(|v| v.as_u64().ok_or_else(|| bad(CMD, "`nodes` must be node ids"))).collect::<Result<_>>()?,
        Some(v) => vec![v.as_u64().ok_or_else(|| bad(CMD, "`nodes` must be node ids"))?],
        None => return Err(bad(CMD, "need `nodes`")),
    };
    if ids.is_empty() || ids.len() > dag::MAX_NODES {
        return Err(bad(CMD, "need 1 to 256 `nodes`"));
    }
    edit_comp(s, p, CMD, "Delete Node", None, |c| {
        for id in &ids {
            if c.node(*id).is_none() {
                return Err(bad(CMD, format!("no node {id}")));
            }
        }
        for id in ids {
            // a node with one input and consumers is bridged: its consumers read its input
            let feed = c.edges.iter().find(|e| e.to == Port::new(id, 0)).map(|e| e.from);
            if let Some(src) = feed {
                for e in c.edges.iter_mut().filter(|e| e.from.node == id) {
                    e.from = src;
                }
            }
            c.edges.retain(|e| e.from.node != id && e.to.node != id);
            c.nodes.retain(|n| n.id != id);
            if c.output == Some(id) {
                c.output = None;
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn connect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.connect";
    let (from, to) = (node_p(p, "from", CMD)?, node_p(p, "to", CMD)?);
    let (fp, tp) = (port_p(p, "fromPort", CMD)?, port_p(p, "toPort", CMD)?);
    edit_comp(s, p, CMD, "Connect", None, |c| {
        let f = c.node(from).ok_or_else(|| bad(CMD, format!("no node {from}")))?;
        if fp >= dag::DagNode::outputs(f) {
            return Err(bad(CMD, format!("node {from} has no output {fp}")));
        }
        let t = c.node(to).ok_or_else(|| bad(CMD, format!("no node {to}")))?;
        if tp >= t.kind.inputs() {
            return Err(bad(CMD, format!("node {to} has no input {tp}")));
        }
        // the input's old connection is replaced
        c.edges.retain(|e| e.to != Port::new(to, tp));
        if c.edges.len() >= dag::MAX_EDGES {
            return Err(bad(CMD, format!("a comp holds at most {} connections", dag::MAX_EDGES)));
        }
        c.edges.push(Edge::new(Port::new(from, fp), Port::new(to, tp)));
        check_graph(c, to, CMD)
    })?;
    Ok(Value::Null)
}

fn disconnect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.disconnect";
    let to = node_p(p, "to", CMD)?;
    let tp = port_p(p, "toPort", CMD)?;
    edit_comp(s, p, CMD, "Disconnect", None, |c| {
        let before = c.edges.len();
        c.edges.retain(|e| e.to != Port::new(to, tp));
        if c.edges.len() == before {
            return Err(bad(CMD, format!("input {tp} of node {to} isn't connected")));
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Where a node keeps parameter `key` and what value it wants: the grade node's (Color), the text
/// layer's (Text) or the node's own (every other kind, [`comp::params`]).
enum Slot {
    Grade(NodeParamKind),
    Layer(ParamValue, ParamKind),
    Own(ParamValue, ParamKind),
}

fn slot(n: &CompNode, key: &str, cmd: &str) -> Result<Slot> {
    let missing = || bad(cmd, format!("a {} node has no parameter `{key}` (see comp.params)", n.kind.key()));
    match &n.kind {
        CompKind::Color { .. } => {
            if key.starts_with("tnr.") {
                return Err(missing());
            }
            node_param(key).map(|d| Slot::Grade(d.kind)).ok_or_else(missing)
        }
        CompKind::Text { layer } => layer.def().and_then(|d| d.param(key)).map(|d| Slot::Layer(d.default.clone(), d.kind.clone())).ok_or_else(missing),
        // C4.3: a group's parameters are its exposed ones (comp.get lists them in `groupParams`)
        CompKind::Group { .. } => comp::group::param_def(n, key).map(|d| Slot::Own(d.default, d.kind)).ok_or_else(missing),
        kind => comp::params(kind).into_iter().find(|d| d.id == key).map(|d| Slot::Own(d.default, d.kind)).ok_or_else(missing),
    }
}

/// A JSON value for a schema parameter: typed like its default, numbers finite and within the hard
/// range, choices within their options.
fn schema_value(default: &ParamValue, kind: &ParamKind, v: &Value) -> Option<ParamValue> {
    let pv = json_to_param(default, v)?;
    Some(match (pv, kind) {
        (ParamValue::Float(f), ParamKind::Float { min, max, .. }) if f.is_finite() => ParamValue::Float(f.clamp(*min, *max)),
        (ParamValue::Float(f), _) if f.is_finite() => ParamValue::Float(f),
        (ParamValue::Float(_), _) => return None,
        (ParamValue::Choice(c), ParamKind::Choice(opts)) if (c as usize) < opts.len() => ParamValue::Choice(c),
        (ParamValue::Choice(_), _) => return None,
        (ParamValue::Vec2(p), _) if p.x.is_finite() && p.y.is_finite() => ParamValue::Vec2(p),
        (ParamValue::Vec2(_), _) => return None,
        (ParamValue::Color(c), _) if c.iter().all(|x| x.is_finite()) => ParamValue::Color(c.map(|x| x.clamp(0.0, 1.0))),
        (ParamValue::Color(_), _) => return None,
        (ParamValue::Text(t), _) => ParamValue::Text(t.chars().take(comp::MAX_TEXT).collect()),
        (other, _) => other,
    })
}

fn convert(sl: &Slot, v: &Value) -> Option<ParamValue> {
    match sl {
        Slot::Grade(kind) => crate::grade::value_for(*kind, v),
        Slot::Layer(d, k) | Slot::Own(d, k) => schema_value(d, k, v),
    }
}

/// The parameter map a slot lives in.
fn params_of<'a>(n: &'a mut CompNode, sl: &Slot) -> &'a mut std::collections::BTreeMap<String, Param> {
    match (&mut n.kind, sl) {
        (CompKind::Color { node }, Slot::Grade(_)) => &mut node.params,
        (CompKind::Text { layer }, Slot::Layer(..)) => &mut layer.params,
        _ => &mut n.params,
    }
}

fn default_of(sl: &Slot) -> ParamValue {
    match sl {
        Slot::Grade(kind) => crate::grade::identity(*kind),
        Slot::Layer(d, _) | Slot::Own(d, _) => d.clone(),
    }
}

/// Most keyframes `comp.setParam` accepts in one list.
const MAX_KEYFRAMES: usize = 10_000;

fn set_param(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.setParam";
    let node = node_p(p, "node", CMD)?;
    let key = str_p(p, "param").ok_or_else(|| bad(CMD, "need `param`"))?.to_string();
    let time = ticks_p(p, "time", CMD)?.unwrap_or(Tick::ZERO);
    let reset = bool_p(p, "reset").unwrap_or(false);
    let (comp_id, c) = comp_at(s, p, CMD)?;
    let n = c.node(node).ok_or_else(|| bad(CMD, format!("no node {node}")))?;
    let sl = slot(n, &key, CMD)?;
    let wrong = |what: &str| bad(CMD, format!("`{key}` wants {what}"));
    // the new state of the parameter: reset, a keyframe list, or a value at `time`
    enum Change {
        Reset,
        Keys(Vec<filmcraft_project::Keyframe>),
        Value(ParamValue),
    }
    let change = if reset {
        Change::Reset
    } else if let Some(list) = p.get("keyframes") {
        let a = list.as_array().filter(|a| a.len() <= MAX_KEYFRAMES).ok_or_else(|| wrong("`keyframes` as a list of {time, value}"))?;
        let mut keys = Vec::with_capacity(a.len());
        for k in a {
            let t = k.get("time").and_then(Value::as_i64).ok_or_else(|| wrong("whole-tick keyframe times"))?;
            let v = k.get("value").and_then(|v| convert(&sl, v)).ok_or_else(|| wrong("keyframe values of its type"))?;
            keys.push(filmcraft_project::Keyframe::new(Tick(t), v));
        }
        keys.sort_by_key(|k| k.time);
        keys.dedup_by_key(|k| k.time);
        Change::Keys(keys)
    } else {
        let raw = p.get("value").ok_or_else(|| bad(CMD, "need `value`, `keyframes` or `reset`"))?;
        Change::Value(convert(&sl, raw).ok_or_else(|| wrong("a value of its type (see comp.params)"))?)
    };
    let merge = format!("comp.setParam/{}/{node}/{key}", comp_id.0);
    edit_comp(s, p, CMD, "Change Node Parameter", Some(merge), |c| {
        let n = node_mut(c, node, CMD)?;
        let map = params_of(n, &sl);
        match change {
            Change::Reset => {
                map.remove(&key);
            }
            Change::Keys(keys) => {
                let first = keys.first().map(|k| k.value.clone()).unwrap_or_else(|| default_of(&sl));
                let prm = map.entry(key.clone()).or_insert_with(|| Param::new(first.clone()));
                prm.value = first;
                prm.keyframes = keys;
            }
            Change::Value(v) => match map.get_mut(&key) {
                Some(prm) => prm.set_at(time, v),
                None => {
                    map.insert(key.clone(), Param::new(v));
                }
            },
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn toggle_animation(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.toggleAnimation";
    let node = node_p(p, "node", CMD)?;
    let key = str_p(p, "param").ok_or_else(|| bad(CMD, "need `param`"))?.to_string();
    let time = ticks_p(p, "time", CMD)?.unwrap_or(Tick::ZERO);
    let n = comp_at(s, p, CMD)?.1.node(node).ok_or_else(|| bad(CMD, format!("no node {node}")))?;
    let sl = slot(n, &key, CMD)?;
    if matches!(sl, Slot::Grade(NodeParamKind::Curve) | Slot::Own(_, ParamKind::Curve { .. }) | Slot::Layer(_, ParamKind::Curve { .. })) {
        return Err(bad(CMD, "curves are not keyframed"));
    }
    let animated = edit_comp(s, p, CMD, "Toggle Animation", None, |c| {
        let n = node_mut(c, node, CMD)?;
        let map = params_of(n, &sl);
        let prm = map.entry(key.clone()).or_insert_with(|| Param::new(default_of(&sl)));
        prm.toggle_animation(time);
        Ok(prm.is_animated())
    })?;
    Ok(json!({ "animated": animated }))
}

fn set_source(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.setSource";
    let node = node_p(p, "node", CMD)?;
    let src = source_p(s, p, CMD)?;
    let offset = ticks_p(p, "offset", CMD)?;
    if src.is_none() && offset.is_none() {
        return Err(bad(CMD, "need `item`, `track` or `offset`"));
    }
    edit_comp(s, p, CMD, "Set Media In Source", None, |c| {
        let n = node_mut(c, node, CMD)?;
        let CompKind::MediaIn { source, offset: off } = &mut n.kind else { return Err(bad(CMD, format!("node {node} is not a Media In"))) };
        if let Some(v) = src {
            *source = v;
        }
        if let Some(o) = offset {
            *off = o;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn bypass(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.bypass";
    let node = node_p(p, "node", CMD)?;
    let on = bool_p(p, "on");
    let now = edit_comp(s, p, CMD, "Bypass Node", None, |c| {
        let n = node_mut(c, node, CMD)?;
        n.bypass = on.unwrap_or(!n.bypass);
        Ok(n.bypass)
    })?;
    Ok(json!({ "bypass": now }))
}

fn label_node(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.labelNode";
    let node = node_p(p, "node", CMD)?;
    let label: String = str_p(p, "label").ok_or_else(|| bad(CMD, "need `label`"))?.chars().take(MAX_LABEL).collect();
    edit_comp(s, p, CMD, "Label Node", None, |c| {
        node_mut(c, node, CMD)?.label = label;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn move_node(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.moveNode";
    let node = node_p(p, "node", CMD)?;
    let pos = pos_p(p, CMD)?.ok_or_else(|| bad(CMD, "need `pos`"))?;
    let comp_id = target(s, p, CMD)?;
    edit_comp(s, p, CMD, "Move Node", Some(format!("comp.moveNode/{}/{node}", comp_id.0)), |c| {
        node_mut(c, node, CMD)?.pos = pos;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_output(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.setOutput";
    let node = node_p(p, "node", CMD)?;
    edit_comp(s, p, CMD, "Set Media Out", None, |c| {
        node_mut(c, node, CMD)?;
        c.output = Some(node);
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_size(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.setSize";
    top_level_only(p, CMD, "Comp Settings")?;
    let width = checked_u32_p(p, "width", CMD)?;
    let height = checked_u32_p(p, "height", CMD)?;
    for v in [width, height].into_iter().flatten() {
        if v == 0 || v > comp::MAX_SIDE {
            return Err(bad(CMD, format!("width and height must be 1..={}", comp::MAX_SIDE)));
        }
    }
    let rate = fps_p(p, CMD)?;
    let duration = ticks_p(p, "duration", CMD)?;
    if duration.is_some_and(|d| d <= Tick::ZERO) {
        return Err(bad(CMD, "duration must be positive"));
    }
    edit_comp(s, p, CMD, "Comp Settings", None, |c| {
        c.width = width.unwrap_or(c.width);
        c.height = height.unwrap_or(c.height);
        c.rate = rate.unwrap_or(c.rate);
        c.duration = duration.unwrap_or(c.duration);
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_masks(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.setMasks";
    let node = node_p(p, "node", CMD)?;
    let raw = p.get("masks").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `masks` (a list of masks)"))?;
    if raw.len() > filmcraft_project::grade::MAX_WINDOWS {
        return Err(bad(CMD, format!("a Mask node holds at most {} masks", filmcraft_project::grade::MAX_WINDOWS)));
    }
    let list: Vec<filmcraft_project::Mask> =
        raw.iter().map(|m| serde_json::from_value(m.clone()).map_err(|e| bad(CMD, format!("not a mask: {e}")))).collect::<Result<_>>()?;
    // one undo step per drag on the monitor: the overlay sends the same `gesture` while it drags
    let merge = match p.get("gesture").and_then(Value::as_u64) {
        Some(g) => Some(format!("comp.setMasks/{}/{node}/{g}", target(s, p, CMD)?.0)),
        None => None,
    };
    edit_comp(s, p, CMD, "Set Masks", merge, |c| {
        let n = node_mut(c, node, CMD)?;
        let CompKind::Mask { masks } = &mut n.kind else { return Err(bad(CMD, format!("node {node} is not a Mask node"))) };
        *masks = list;
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Places the comp `comp` (a new one in the active sequence's settings without it) on the timeline
/// through `timeline.place` (default: V1 at the playhead), as one undo step.
fn new_clip(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.newClip";
    let depth = s.history.undo.len();
    let comp = match p.get("comp") {
        Some(_) => target(s, p, CMD)?.0,
        None => new(s, &json!({}))?["comp"].as_u64().ok_or_else(|| bad(CMD, "no comp made"))?,
    };
    let mut place = json!({ "item": comp, "track": p.get("track").cloned().unwrap_or(json!("V1")), "audio": false });
    match (p.get("time"), p.get("seconds")) {
        (Some(t), _) => place["time"] = t.clone(),
        (None, Some(sec)) => place["seconds"] = sec.clone(),
        (None, None) => place["time"] = json!(s.playhead().0),
    }
    let placed = s.execute("timeline.place", place);
    // one undo step for the new comp and its clip (or nothing at all when placing failed)
    if s.history.undo.len() > depth.saturating_add(1) {
        s.history.undo.truncate(depth.saturating_add(1));
        if let Some(last) = s.history.undo.last_mut() {
            last.0 = "New Comp Clip".to_string();
        }
    }
    match placed {
        Ok(r) => Ok(json!({ "comp": comp, "clip": r.get("clip").cloned().unwrap_or(r) })),
        Err(e) => {
            if p.get("comp").is_none() && s.history.undo.len() > depth {
                // drop the comp made for the failed clip (and leave nothing to redo)
                if s.execute("edit.undo", json!({})).is_ok() {
                    s.history.redo.pop();
                }
            }
            Err(e)
        }
    }
}

/// One clip `comp.convertToCompClip` takes: its track, the clip, its source size.
struct Converted {
    track: usize,
    clip: filmcraft_project::TrackItem,
    size: (u32, u32),
}

/// Why `it` can't go into a comp yet (C1 is strict, DESIGN-C1 §9; relaxed in C4), else `None`.
fn convert_blocker(s: &Session, q: &filmcraft_project::Sequence, it: &filmcraft_project::TrackItem) -> Option<String> {
    let name = &it.name;
    let pi = s.project.item(it.item)?;
    if !matches!(pi.kind, ItemKind::Media(_) | ItemKind::Sequence(_) | ItemKind::Comp(_)) || !pi.has_video() {
        return Some(format!("`{name}` is not a media clip, sequence or comp"));
    }
    if !it.enabled {
        return Some(format!("`{name}` is disabled"));
    }
    if it.speed != 1.0 || it.reverse || it.frame_hold.is_some() {
        return Some(format!("`{name}` has a speed change, reverse or frame hold"));
    }
    if it.scale_to_frame && s.project.source_size(it.item).is_none() {
        return Some(format!("`{name}` has no known size"));
    }
    if it.grade.is_some() || it.temporal_nr.is_some() || it.multicam.is_some() {
        return Some(format!("`{name}` has a grade, noise reduction or multi-camera angle"));
    }
    let seq_par = filmcraft_project::sane_par(q.settings.par);
    if filmcraft_render::source_par(&s.project, it.item).map_or(seq_par, filmcraft_project::sane_par) != seq_par {
        return Some(format!("`{name}` has other pixels than the sequence"));
    }
    let linked = it.link.is_some_and(|l| q.all_tracks().any(|t| t.items.iter().any(|o| o.id != it.id && o.link == Some(l))));
    if linked {
        return Some(format!("`{name}` is linked to sound (unlink it first)"));
    }
    if q.all_tracks().any(|t| t.transitions.iter().any(|x| x.from == Some(it.id) || x.to == Some(it.id))) {
        return Some(format!("`{name}` has a transition"));
    }
    for e in &it.effects {
        let animated = e.params.values().any(Param::is_animated);
        match e.effect.as_str() {
            "motion" | "opacity" if e.enabled && (animated || !e.masks.is_empty()) => {
                return Some(format!("`{name}` has animated or masked {}", e.effect));
            }
            "time_remap" if e.enabled && (animated || e.f64_at("speed", Tick::ZERO) != 100.0) => {
                return Some(format!("`{name}` has time remapping"));
            }
            "motion" | "opacity" | "time_remap" => {}
            other => return Some(format!("`{name}` has the effect `{}`", e.def().map_or(other, |d| d.name))),
        }
    }
    None
}

/// The clip's static Motion as a Transform node's parameters in a comp of the sequence's size
/// whose Media In fits the source into the frame (`fit` from `crates/render/src/comp/project.rs`):
/// comp pixel = offset + f × source pixel, so the anchor moves into comp pixels and the scale is
/// divided by f. The result shows the source where [`filmcraft_render::motion_matrix`] put it.
fn transform_params(it: &filmcraft_project::TrackItem, (w, h): (u32, u32), (sw, sh): (u32, u32)) -> Vec<(&'static str, ParamValue)> {
    use filmcraft_geom::Vec2;
    let (w, h, sw, sh) = (f64::from(w), f64::from(h), f64::from(sw.max(1)), f64::from(sh.max(1)));
    let f = (w / sw).min(h / sh);
    let (ox, oy) = ((w - sw * f) / 2.0, (h - sh * f) / 2.0);
    let (mut pos, mut anchor, mut scale, mut scale_w, mut uniform, mut rot) =
        (Vec2::new(w / 2.0, h / 2.0), Vec2::new(sw / 2.0, sh / 2.0), 100.0, 100.0, true, 0.0);
    if let Some(m) = it.effect("motion").filter(|m| m.enabled) {
        let p = m.vec2_at("position", Tick::ZERO);
        if !p.x.is_nan() && !p.y.is_nan() && m.param("position").is_some() {
            pos = p;
        }
        let a = m.vec2_at("anchor", Tick::ZERO);
        if !a.x.is_nan() && !a.y.is_nan() && m.param("anchor").is_some() {
            anchor = a;
        }
        scale = m.f64_at("scale", Tick::ZERO);
        uniform = m.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
        scale_w = if uniform { scale } else { m.f64_at("scale_width", Tick::ZERO) };
        rot = m.f64_at("rotation", Tick::ZERO);
    }
    // Scale to Frame Size fits the source in the frame: the same f the Media In already applied
    let k = if it.scale_to_frame { 1.0 } else { 1.0 / f };
    let finite = |v: f64, d: f64| if v.is_finite() { v } else { d };
    vec![
        ("position", ParamValue::Vec2(pos)),
        ("anchor", ParamValue::Vec2(Vec2::new(ox + anchor.x * f, oy + anchor.y * f))),
        ("scale", ParamValue::Float(finite(scale * k, 100.0))),
        ("scale_width", ParamValue::Float(finite(scale_w * k, 100.0))),
        ("uniform_scale", ParamValue::Bool(uniform)),
        ("rotation", ParamValue::Float(finite(rot, 0.0))),
    ]
}

/// Converts stacked video clips into one comp clip (DESIGN-C1 §5, §9): a Media In per clip (bottom
/// track first), each one's Motion folded into a Transform, its Opacity into the Merge that lays it
/// over the ones below, the Merges chained into Media Out. The comp clip replaces them on the
/// lowest track. Strict for C1: the clips must cover the same time on different tracks and have no
/// effects, grades, speed changes, transitions, linked sound or animated Motion / Opacity; a
/// refusal names the first clip in the way and changes nothing.
fn convert_to_comp_clip(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.convertToCompClip";
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let ids = crate::commands::clips_p(s, p);
    if ids.is_empty() {
        return Err(bad(CMD, "select the clips to convert"));
    }
    let mut clips: Vec<Converted> = Vec::new();
    for id in &ids {
        let Some((tid, it)) = q.find_item(*id) else { return Err(bad(CMD, format!("no clip {}", id.0))) };
        let Some(track) = q.video_tracks.iter().position(|t| t.id == tid) else {
            return Err(bad(CMD, format!("`{}` is a sound clip: a comp holds pictures only", it.name)));
        };
        if q.video_tracks.get(track).is_some_and(|t| t.locked) {
            return Err(bad(CMD, format!("`{}` is on a locked track", it.name)));
        }
        if let Some(why) = convert_blocker(s, &q, it) {
            return Err(bad(CMD, format!("{why}; flatten or remove it first (Convert to Comp Clip takes plain stacked clips for now)")));
        }
        let size = s.project.source_size(it.item).ok_or_else(|| bad(CMD, format!("`{}` has no known size", it.name)))?;
        if clips.iter().any(|c| c.clip.id == it.id) {
            continue;
        }
        clips.push(Converted { track, clip: it.clone(), size });
    }
    clips.sort_by_key(|c| c.track);
    if clips.windows(2).any(|w| w.first().map(|c| c.track) == w.get(1).map(|c| c.track)) {
        return Err(bad(CMD, "the clips must be stacked (one per track)"));
    }
    let Some(base) = clips.first() else { return Err(bad(CMD, "select the clips to convert")) };
    let (start, duration, lowest) = (base.clip.start, base.clip.duration, base.track);
    if clips.iter().any(|c| c.clip.start != start || c.clip.duration != duration) {
        return Err(bad(CMD, "the clips must start and end at the same time"));
    }
    let base_opacity = base.clip.effect("opacity").filter(|o| o.enabled);
    if base_opacity.is_some_and(|o| o.f64_at("opacity", Tick::ZERO) != 100.0 || o.param("blend").is_some_and(|b| b.value != ParamValue::Choice(0))) {
        return Err(bad(CMD, format!("`{}` (the lowest clip) must be at 100 % Normal opacity", base.clip.name)));
    }
    if clips.len() > (dag::MAX_NODES.saturating_sub(1)) / 3 {
        return Err(bad(CMD, "too many clips for one comp"));
    }
    let (w, h, rate) = (q.settings.width, q.settings.height, q.settings.frame_rate);
    let name = str_p(p, "name").filter(|n| !n.trim().is_empty()).unwrap_or("Comp").chars().take(256).collect::<String>();
    // the graph: Media In → Transform per clip, Merges chained bottom-up, last → Media Out (node 1)
    let mut c = Comp::new(w, h, rate, duration);
    c.color = q.settings.color;
    let mut below: Option<NodeId> = None;
    for (n, cv) in clips.iter().enumerate() {
        let x = n as f32 * 200.0;
        let mi = c.next_id();
        let mut m = CompNode::new(mi, CompKind::MediaIn { source: MediaInSource::Item { item: cv.clip.item }, offset: cv.clip.source_in });
        m.label = cv.clip.name.chars().take(MAX_LABEL).collect();
        m.pos = [x, 0.0];
        c.nodes.push(m);
        let tr = c.next_id();
        let mut t = CompNode::new(tr, CompKind::Transform);
        for (k, v) in transform_params(&cv.clip, (w, h), cv.size) {
            t.params.insert(k.to_string(), Param::new(v));
        }
        t.pos = [x, 120.0];
        c.nodes.push(t);
        c.edges.push(Edge::new(Port::new(mi, 0), Port::new(tr, 0)));
        below = Some(match below {
            None => tr,
            Some(bg) => {
                let mg = c.next_id();
                let mut node = CompNode::new(mg, CompKind::Merge);
                if let Some(o) = cv.clip.effect("opacity").filter(|o| o.enabled) {
                    for k in ["opacity", "blend"] {
                        if let Some(v) = o.param(k) {
                            node.params.insert(k.to_string(), v.clone());
                        }
                    }
                }
                node.pos = [x, 240.0];
                c.nodes.push(node);
                c.edges.push(Edge::new(Port::new(bg, 0), Port::new(mg, 0)));
                c.edges.push(Edge::new(Port::new(tr, 0), Port::new(mg, 1)));
                mg
            }
        });
    }
    if let Some(last) = below {
        c.edges.push(Edge::new(Port::new(last, 0), Port::new(1, 0)));
        if let Some(out) = c.node_mut(1) {
            out.pos = [(clips.len() as f32) * 200.0, 240.0];
        }
    }
    check_graph(&c, 1, CMD)?;
    c.sanitize();
    let removed: Vec<filmcraft_project::ClipId> = clips.iter().map(|c| c.clip.id).collect();
    let (comp, clip) = s.edit("Convert to Comp Clip", |pr, st| {
        let comp = pr.add_item(&name, Label::Iris, ItemKind::Comp(Arc::new(c)), None);
        let clip = pr
            .make_track_item(comp, filmcraft_project::TrackKind::Video, start, filmcraft_time::TimeRange::new(Tick::ZERO, duration), rate)
            .ok_or_else(|| bad(CMD, "cannot place the comp"))?;
        let id = clip.id;
        let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        filmcraft_edit::delete_items(seq, &removed);
        let t = seq.video_tracks.get_mut(lowest).ok_or(EngineError::NoSequence)?;
        t.items.push(clip);
        t.sort();
        st.selection = vec![id];
        Ok((comp, id))
    })?;
    Ok(json!({ "comp": comp.0, "clip": clip.0 }))
}
