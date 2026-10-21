//! Colour grade commands (`grade.*`, fork ADR 0004, GRADE-PLAN G1): edit a clip's serial node
//! grade. Every edit is one undo step through [`Session::edit_sequence`].
//!
//! Commands address a clip by `clip` (default: the first selected video clip) and a node by its
//! stable `node` id (never an index). `grade.addNode` creates the grade when the clip has none.
//!
//! The *current node* (`EditorState::grade_node`: a clip and a node, set by `grade.selectNode`,
//! by adding a node and by the UI) is what a command falls back to when `node` is omitted, so
//! keyboard shortcuts work without parameters. It only applies to its own clip, and only while
//! that clip's grade still holds the node. With no current node, a grade with exactly one node
//! uses that node; otherwise the command is refused.

use filmcraft_project::grade::{Grade, GradeNode, MAX_NODES, NodeParamKind, node_param};
use filmcraft_project::{ClipId, Param, ParamValue, Sequence};

use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, clip_p, has_seq, str_p, time_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: true }
}
fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

/// A command in the Clip > Grade menu.
fn menu_spec(id: &'static str, label: &'static str, params: &'static str, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &["Clip", "Grade"], shortcut: None, params, enabled: has_seq, run, journal }
}

/// The most characters a node label keeps.
const MAX_LABEL: usize = 64;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        query("grade.get", "Get Grade", r#"{"clip":id?}"#, get),
        query("grade.params", "List Grade Node Parameters", "{}", params),
        spec("grade.addNode", "Add Serial Node", r#"{"clip":id?,"after":node?,"label":str?}"#, |s, p| add(s, p, false)),
        spec("grade.insertNode", "Insert Node Before", r#"{"clip":id?,"before":node?,"label":str?}"#, |s, p| add(s, p, true)),
        query("grade.selectNode", "Select Node", r#"{"clip":id?,"node":id?}"#, select),
        spec("grade.removeNode", "Delete Node", r#"{"clip":id?,"node":id?,"nodes":[id]?}"#, remove),
        spec("grade.moveNode", "Move Node", r#"{"clip":id?,"node":id,"to":n}"#, move_node),
        spec("grade.toggleNode", "Enable / Disable Node", r#"{"clip":id?,"node":id?,"nodes":[id]?,"on":bool?}"#, toggle),
        spec("grade.labelNode", "Label Node", r#"{"clip":id?,"node":id?,"label":str}"#, label),
        spec(
            "grade.setParam",
            "Set Node Parameter",
            r#"{"clip":id?,"node":id?,"param":str,"value":num|[r,g,b,master?]|[[x,y]],"time":ticks?,"merge":str?}"#,
            set_param,
        ),
        spec("grade.wb.pick", "Pick White Balance", r#"{"clip":id?,"node":id?,"x":0..1,"y":0..1,"x2":0..1?,"y2":0..1?,"time":ticks?}"#, |s, p| {
            pick_white_balance(s, p, false)
        }),
        spec("grade.wb.auto", "Auto White Balance", r#"{"clip":id?,"node":id?,"time":ticks?}"#, |s, p| pick_white_balance(s, p, true)),
        spec("grade.pickQualifier", "Pick Qualifier Colour", r#"{"clip":id?,"node":id?,"x":0..1,"y":0..1,"mode":"set"|"add"?,"time":ticks?}"#, pick_qualifier),
        spec("grade.setLut", "Set Node LUT", r#"{"clip":id?,"node":id?,"lut":"builtin:<id>"|"lib:<id>"|""?,"path":str?}"#, set_lut),
        spec("grade.toggleAnimation", "Toggle Node Parameter Animation", r#"{"clip":id?,"node":id?,"param":str,"time":ticks?}"#, toggle_animation),
        spec("grade.resetNode", "Reset Node", r#"{"clip":id?,"node":id?,"param":str?}"#, reset),
        spec("grade.bypass", "Bypass Grade", r#"{"clip":id?,"on":bool?}"#, bypass),
        spec("grade.clear", "Remove Grade", r#"{"clip":id?}"#, clear),
        menu_spec("grade.copy", "Copy Grade", r#"{"clip":id?}"#, copy, true),
        menu_spec("grade.paste", "Paste Grade", r#"{"clips":[id]?}"#, paste, true),
        menu_spec("grade.append", "Append Grade", r#"{"clips":[id]?}"#, append, true),
        menu_spec("grade.applyFrom", "Apply Grade From Clip", r#"{"from":id,"clips":[id]?}"#, apply_from, true),
    ]
}

/// The clip a command addresses: `clip`, else the first selected clip on a video track.
pub(crate) fn target(s: &Session, p: &Value, cmd: &str) -> Result<ClipId> {
    if p.get("clip").is_some() {
        return clip_p(p, "clip").ok_or_else(|| bad(cmd, "`clip` must be a clip id"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    s.state.selection.iter().copied().find(|c| is_video(q, *c)).ok_or_else(|| bad(cmd, "need `clip` (or a selected video clip)"))
}

pub(crate) fn is_video(q: &Sequence, c: ClipId) -> bool {
    q.video_tracks.iter().any(|t| t.item(c).is_some())
}

/// A non-negative integer parameter (negative or fractional values are refused, not wrapped).
fn uint(p: &Value, k: &str) -> Option<u64> {
    p.get(k).and_then(Value::as_u64)
}

fn node_id(p: &Value, k: &str, cmd: &str) -> Result<u64> {
    uint(p, k).ok_or_else(|| bad(cmd, format!("need `{k}` (a node id)")))
}

/// The explicit `node` argument: None when absent, an error when present but not a node id.
pub(crate) fn node_arg(p: &Value, cmd: &str) -> Result<Option<u64>> {
    match p.get("node") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| bad(cmd, "`node` must be a node id")),
    }
}

/// The node a command acts on: the explicit one, else the current node (already checked to be on
/// this clip), else the grade's only node.
pub(crate) fn pick(g: &Grade, want: Option<u64>, current: Option<u64>, cmd: &str) -> Result<u64> {
    if let Some(w) = want {
        return Ok(w);
    }
    if let Some(c) = current.filter(|c| g.nodes.iter().any(|n| n.id == *c)) {
        return Ok(c);
    }
    match g.nodes.as_slice() {
        [only] => Ok(only.id),
        _ => Err(bad(cmd, "no current node: pass `node` or call grade.selectNode")),
    }
}

/// The addressed clip and the current node when it belongs to that clip.
pub(crate) fn current_for(s: &Session, p: &Value, cmd: &str) -> Result<(ClipId, Option<u64>)> {
    let c = target(s, p, cmd)?;
    Ok((c, s.state.grade_node.filter(|sel| sel.clip == c).map(|sel| sel.node)))
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.selectNode";
    let Some(node) = node_arg(p, CMD)? else {
        s.state.grade_node = None;
        return Ok(json!({ "clip": Value::Null, "node": Value::Null }));
    };
    let c = target(s, p, CMD)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(c).ok_or_else(|| bad(CMD, "no such clip"))?;
    if !it.grade.as_ref().is_some_and(|g| g.nodes.iter().any(|n| n.id == node)) {
        return Err(bad(CMD, format!("the clip's grade has no node {node}")));
    }
    s.state.grade_node = Some(crate::GradeSel { clip: c, node });
    Ok(json!({ "clip": c.0, "node": node }))
}

/// Run `f` on the clip's grade (created empty when missing and `create`) as one undo step.
pub(crate) fn edit_grade<R>(s: &mut Session, p: &Value, cmd: &'static str, label: &str, create: bool, f: impl FnOnce(&mut Grade) -> Result<R>) -> Result<R> {
    edit_grade_merged(s, p, cmd, label, create, None, f)
}

/// [`edit_grade`] that merges consecutive edits with the same `merge` key into one undo step.
pub(crate) fn edit_grade_merged<R>(
    s: &mut Session,
    p: &Value,
    cmd: &'static str,
    label: &str,
    create: bool,
    merge: Option<&str>,
    f: impl FnOnce(&mut Grade) -> Result<R>,
) -> Result<R> {
    let c = target(s, p, cmd)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let body = move |pr: &mut filmcraft_project::Project, _: &mut crate::EditorState| -> Result<R> {
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        if !is_video(q, c) {
            return Err(bad(cmd, "a grade needs a clip on a video track"));
        }
        let (_, it) = q.find_item_mut(c).ok_or(filmcraft_edit::EditError::NoItem(c))?;
        if it.grade.is_none() && create {
            it.grade = Some(Grade::default());
        }
        let g = it.grade.as_mut().ok_or_else(|| bad(cmd, "the clip has no grade"))?;
        f(g)
    };
    let r = match merge {
        Some(k) => s.edit_merged(label, &format!("grade:{}:{k}", c.0), body),
        None => s.edit(label, body),
    };
    // (a removed node takes its selected window with it)
    s.fix_state();
    r
}

fn node_mut<'a>(g: &'a mut Grade, id: u64, cmd: &str) -> Result<&'a mut GradeNode> {
    g.node_mut(id).ok_or_else(|| bad(cmd, format!("no node {id}")))
}

fn clean_label(p: &Value) -> String {
    str_p(p, "label").unwrap_or_default().chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect()
}

fn get(s: &mut Session, p: &Value) -> Result<Value> {
    let c = target(s, p, "grade.get")?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(c).ok_or_else(|| bad("grade.get", "no such clip"))?;
    Ok(json!({ "clip": c.0, "grade": it.grade }))
}

pub(crate) fn params(_: &mut Session, _: &Value) -> Result<Value> {
    let list: Vec<Value> = filmcraft_project::grade::NODE_PARAMS
        .iter()
        // (then the graph parameters of keys and mixers, fork C2)
        .chain(filmcraft_project::grade::GRAPH_PARAMS)
        .map(|p| match p.kind {
            NodeParamKind::Float { identity, min, max } => {
                json!({"key": p.key, "label": p.label, "kind": "float", "identity": identity, "min": min, "max": max})
            }
            NodeParamKind::Wheel { range } => json!({"key": p.key, "label": p.label, "kind": "wheel", "range": range}),
            NodeParamKind::Curve => json!({"key": p.key, "label": p.label, "kind": "curve"}),
        })
        .collect();
    Ok(json!({ "params": list, "maxNodes": MAX_NODES }))
}

fn add(s: &mut Session, p: &Value, before: bool) -> Result<Value> {
    let cmd = if before { "grade.insertNode" } else { "grade.addNode" };
    let key = if before { "before" } else { "after" };
    // (null = no anchor: the node strip's "+ Serial" sends the current node, null when none)
    if p.get(key).is_some_and(|v| !v.is_null() && v.as_u64().is_none()) {
        return Err(bad(cmd, format!("`{key}` must be a node id")));
    }
    let anchor = uint(p, key);
    let (clip, current) = current_for(s, p, cmd)?;
    let label = clean_label(p);
    let id = edit_grade(s, p, cmd, if before { "Insert Node" } else { "Add Serial Node" }, true, |g| {
        if g.nodes.len() >= MAX_NODES {
            return Err(bad(cmd, format!("a grade holds at most {MAX_NODES} nodes")));
        }
        // no explicit anchor: the current node when this grade holds it
        let anchor = anchor.or_else(|| current.filter(|c| g.nodes.iter().any(|n| n.id == *c)));
        let at = match anchor {
            Some(a) => {
                let i = g.nodes.iter().position(|n| n.id == a).ok_or_else(|| bad(cmd, format!("no node {a}")))?;
                if before { i } else { i + 1 }
            }
            None if before => 0,
            None => g.nodes.len(),
        };
        let id = g.next_id();
        if id == 0 {
            return Err(bad(cmd, "no free node id"));
        }
        let mut n = GradeNode::new(id);
        n.label = label;
        // a branched grade (fork C2): the node is wired in after / before its anchor
        let branched = g.is_graph();
        if branched {
            crate::grade_graph::begin(g);
        }
        g.nodes.insert(at.min(g.nodes.len()), n);
        if branched {
            if before {
                crate::grade_graph::wire_before(g, anchor, id, cmd)?;
            } else {
                crate::grade_graph::wire_after(g, anchor, id, cmd)?;
            }
            crate::grade_graph::finish(g, cmd)?;
        }
        // (a grade with a hand-made layout: the node goes next to its anchor, fork C2b)
        g.place_new(&[id], anchor);
        Ok(id)
    })?;
    s.state.grade_node = Some(crate::GradeSel { clip, node: id });
    Ok(json!({ "node": id }))
}

/// The `nodes` list of a command that takes several nodes at once (the node canvas's selection):
/// None when absent, an error when not a list of node ids or longer than a grade can hold.
fn nodes_arg(p: &Value, cmd: &str) -> Result<Option<Vec<u64>>> {
    match p.get("nodes") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if a.len() <= MAX_NODES.saturating_mul(4) => {
            let mut ids: Vec<u64> = a.iter().map(|v| v.as_u64().ok_or_else(|| bad(cmd, "`nodes` must be node ids"))).collect::<Result<_>>()?;
            ids.dedup();
            if ids.is_empty() {
                return Err(bad(cmd, "`nodes` is empty"));
            }
            Ok(Some(ids))
        }
        Some(_) => Err(bad(cmd, "`nodes` must be a list of node ids")),
    }
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.removeNode";
    let want = node_arg(p, CMD)?;
    let many = nodes_arg(p, CMD)?;
    let (clip, current) = current_for(s, p, CMD)?;
    let (gone, neighbour) = edit_grade(s, p, CMD, "Delete Node", false, |g| {
        let ids = match many {
            Some(ids) => ids,
            None => vec![pick(g, want, current, CMD)?],
        };
        let first = ids.iter().filter_map(|id| g.nodes.iter().position(|n| n.id == *id)).min().unwrap_or(0);
        // a branched grade (fork C2): what a node fed is fed from its input (a splitter or a
        // combiner takes its pair with it); refused when the rest could not render
        let branched = g.is_graph();
        if branched {
            crate::grade_graph::begin(g);
        }
        let mut gone: Vec<u64> = Vec::new();
        for id in ids {
            if gone.contains(&id) {
                continue;
            }
            if g.node(id).is_none() {
                return Err(bad(CMD, format!("no node {id}")));
            }
            if branched {
                gone.extend(crate::grade_graph::unhook(g, id));
            } else {
                gone.push(id);
            }
            g.nodes.retain(|n| !gone.contains(&n.id));
        }
        if branched {
            crate::grade_graph::finish(g, CMD)?;
        }
        // the previous node, else the next one
        let neighbour = g.nodes.get(first.wrapping_sub(1)).or_else(|| g.nodes.get(first)).map(|n| n.id);
        Ok((gone, neighbour))
    })?;
    if current.is_some_and(|c| gone.contains(&c)) {
        s.state.grade_node = neighbour.map(|node| crate::GradeSel { clip, node });
    }
    Ok(json!({ "removed": gone }))
}

fn move_node(s: &mut Session, p: &Value) -> Result<Value> {
    let id = node_id(p, "node", "grade.moveNode")?;
    let to = uint(p, "to").ok_or_else(|| bad("grade.moveNode", "need `to` (a position)"))?;
    edit_grade(s, p, "grade.moveNode", "Move Node", false, |g| {
        crate::grade_graph::serial_only(g, "grade.moveNode")?;
        let i = g.nodes.iter().position(|n| n.id == id).ok_or_else(|| bad("grade.moveNode", format!("no node {id}")))?;
        let to = usize::try_from(to).unwrap_or(usize::MAX).min(g.nodes.len().saturating_sub(1));
        if to != i {
            let n = g.nodes.remove(i);
            g.nodes.insert(to, n);
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn toggle(s: &mut Session, p: &Value) -> Result<Value> {
    let want = node_arg(p, "grade.toggleNode")?;
    let current = current_for(s, p, "grade.toggleNode")?.1;
    let many = nodes_arg(p, "grade.toggleNode")?;
    let on = bool_p(p, "on");
    let now = edit_grade(s, p, "grade.toggleNode", "Enable / Disable Node", false, |g| {
        let ids = match many {
            Some(ids) => ids,
            None => vec![pick(g, want, current, "grade.toggleNode")?],
        };
        // several nodes (one undo step): each flips, unless `on` says
        let mut last = true;
        for id in ids {
            let n = node_mut(g, id, "grade.toggleNode")?;
            n.enabled = on.unwrap_or(!n.enabled);
            last = n.enabled;
        }
        Ok(last)
    })?;
    Ok(json!({ "enabled": now }))
}

fn label(s: &mut Session, p: &Value) -> Result<Value> {
    let want = node_arg(p, "grade.labelNode")?;
    let current = current_for(s, p, "grade.labelNode")?.1;
    if str_p(p, "label").is_none() {
        return Err(bad("grade.labelNode", "need `label`"));
    }
    let text = clean_label(p);
    edit_grade(s, p, "grade.labelNode", "Label Node", false, |g| {
        let id = pick(g, want, current, "grade.labelNode")?;
        node_mut(g, id, "grade.labelNode")?.label = text;
        Ok(())
    })?;
    Ok(Value::Null)
}

/// A JSON value as the node parameter `kind` holds it, with every number finite and in range.
pub(crate) fn value_for(kind: NodeParamKind, v: &Value) -> Option<ParamValue> {
    let finite = |x: &Value| x.as_f64().filter(|f| f.is_finite());
    Some(match kind {
        NodeParamKind::Float { min, max, .. } => ParamValue::Float(finite(v)?.clamp(f64::from(min), f64::from(max))),
        NodeParamKind::Wheel { range } => {
            let a = v.as_array().filter(|a| (3..=4).contains(&a.len()))?;
            let mut c = [0.0f32; 4];
            for (slot, x) in c.iter_mut().zip(a) {
                *slot = (finite(x)? as f32).clamp(-range, range);
            }
            ParamValue::Color(c)
        }
        NodeParamKind::Curve => {
            let a = v.as_array()?;
            if a.len() > filmcraft_project::grade::MAX_CURVE_POINTS {
                return None;
            }
            let mut pts = Vec::with_capacity(a.len());
            for pt in a {
                let xy = pt.as_array().filter(|xy| xy.len() == 2)?;
                pts.push([(finite(xy.first()?)? as f32).clamp(0.0, 1.0), (finite(xy.get(1)?)? as f32).clamp(0.0, 1.0)]);
            }
            pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
            ParamValue::Curve(pts)
        }
    })
}

/// The identity value of a node parameter.
pub(crate) fn identity(kind: NodeParamKind) -> ParamValue {
    match kind {
        NodeParamKind::Float { identity, .. } => ParamValue::Float(f64::from(identity)),
        NodeParamKind::Wheel { .. } => ParamValue::Color([0.0; 4]),
        NodeParamKind::Curve => ParamValue::Curve(vec![[0.0, 0.0], [1.0, 1.0]]),
    }
}

/// The clip time of sequence time `tl` (clamped into the clip) at which the renderer evaluates
/// the grade (`effect_time_at`, so frame holds match), where keyframes live.
pub(crate) fn media_time(s: &Session, p: &Value, c: ClipId) -> Tick {
    let tl = time_p(s, p, "").unwrap_or_else(|| s.playhead());
    s.active_sequence()
        .and_then(|q| q.find_item(c))
        .map(|(_, it)| it.effect_time_at(tl.clamp(it.start, (it.end() - Tick(1)).max(it.start))))
        .unwrap_or(Tick::ZERO)
}

fn set_param(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.setParam";
    let want = node_arg(p, CMD)?;
    let current = current_for(s, p, CMD)?.1;
    let key = str_p(p, "param").ok_or_else(|| bad(CMD, "need `param`"))?;
    if key.starts_with("tnr.") {
        return Err(bad(CMD, "temporal NR is a clip setting since schema v17: use `tnr.set`"));
    }
    let def = node_param(key).ok_or_else(|| bad(CMD, format!("no node parameter `{key}` (see grade.params)")))?;
    let raw = p.get("value").ok_or_else(|| bad(CMD, "need `value`"))?;
    let v = value_for(def.kind, raw).ok_or_else(|| bad(CMD, format!("`{key}` wants {}", kind_hint(def.kind))))?;
    let c = target(s, p, CMD)?;
    let mt = media_time(s, p, c);
    let merge = str_p(p, "merge").map(str::to_string);
    edit_grade_merged(s, p, CMD, "Change Node Parameter", false, merge.as_deref(), |g| {
        let id = pick(g, want, current, CMD)?;
        let n = node_mut(g, id, CMD)?;
        match n.params.get_mut(def.key) {
            // curves are not keyframed (their points are the static value)
            Some(prm) if !matches!(def.kind, NodeParamKind::Curve) => prm.set_at(mt, v),
            _ => {
                n.params.insert(def.key.to_string(), Param::new(v));
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Qualifier picker (G2.1): sample the node's input at `x`, `y` (0…1 of the sequence frame) and
/// set the node's qualifier around it (`mode` "set", the default), or widen it to include the
/// sample ("add"). The input is the clip through its input transform and the nodes before this
/// one (no effects). One undo step.
/// The picture at node `id`'s input (`p`'s `node`, else the current one) of the target clip at
/// `p`'s `time` (else the playhead): the nodes before it, no effects after the grade, as working
/// linear light at a quarter size. Returns the clip, the node id and the working space.
fn node_input(s: &mut Session, p: &Value, cmd: &'static str) -> Result<(ClipId, u64, filmcraft_render::Image, filmcraft_color::WorkingSpace)> {
    let want = node_arg(p, cmd)?;
    let (c, current) = current_for(s, p, cmd)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let working = q.settings.color.working;
    let (_, it) = q.find_item(c).ok_or_else(|| bad(cmd, "no such clip"))?;
    if s.project.item(it.item).is_some_and(|pi| matches!(pi.kind, filmcraft_project::ItemKind::AdjustmentLayer { .. })) {
        // (its input is the composite below it, which `render_clip` doesn't draw)
        return Err(bad(cmd, "the pickers don't work on adjustment layers yet: set the values by hand"));
    }
    let g = it.grade.as_ref().ok_or_else(|| bad(cmd, "the clip has no grade"))?;
    let id = pick(g, want, current, cmd)?;
    if g.node(id).is_none() {
        return Err(bad(cmd, format!("no node {id}")));
    }
    let tl = time_p(s, p, "").unwrap_or_else(|| s.playhead()).clamp(it.start, (it.end() - Tick(1)).max(it.start));
    // the node's input: the nodes before it, no effects after the grade
    let mut probe = (*s.project).clone();
    if let Some((_, pi)) = probe.sequence_mut(seq_id).and_then(|q| q.find_item_mut(c)) {
        if let Some(pg) = pi.grade.as_mut() {
            // (a graph keeps its nodes and makes the node feeding this one the output)
            crate::grade_graph::probe_before(pg, id);
        }
        for e in pi.effects.iter_mut().filter(|e| !e.def().is_some_and(|d| d.intrinsic)) {
            e.enabled = false;
        }
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, working_output: true, ..Default::default() };
    let img = filmcraft_render::render_clip(&probe, seq_id, c, tl, opts, &provider).ok_or_else(|| bad(cmd, "the clip has no picture"))?;
    Ok((c, id, img, working))
}

/// What a white-balance pick judges clipping on (G2c): the clip's picture through its input
/// transform alone (no grade, no effects, so a blown highlight counts as blown whatever earlier
/// nodes or an Input LUT did to it) and where its source clips ([`ClipLevel`]: the source's
/// top code value carried through the input transform, so DaVinci Wide Gamut and HDR sequences
/// catch clipping too). Clips without media (nested sequences, graphics) fall back to the picture
/// being sampled and the working space's own clip level.
///
/// [`ClipLevel`]: filmcraft_color::wb::ClipLevel
pub(crate) struct WbClip {
    /// The source picture; `None`: the sampled picture is it (or the fallback applies).
    source: Option<filmcraft_render::Image>,
    level: filmcraft_color::wb::ClipLevel,
}

/// The timeline time a picker samples clip `c` at: `p`'s `time` (else the playhead), inside the clip.
pub(crate) fn wb_time(s: &Session, p: &Value, c: ClipId) -> Tick {
    let t = time_p(s, p, "").unwrap_or_else(|| s.playhead());
    match s.active_sequence().and_then(|q| q.find_item(c)) {
        Some((_, it)) => t.clamp(it.start, (it.end() - Tick(1)).max(it.start)),
        None => t,
    }
}

/// [`WbClip`] for clip `c` of sequence `seq_id` at timeline `t`, rendered like the sample
/// (`RenderOptions` of the pickers). `sampled_is_source`: the sampled picture already is the
/// clip through its input transform alone (a node picker on the first node), nothing to render.
/// `keep_before`: the effects before this index stay on (Lumetri's sample keeps them, and one may
/// move the picture: Transform, Crop…), so the same point lands on the same pixels; 0 for none.
pub(crate) fn wb_clip(
    s: &mut Session,
    seq_id: filmcraft_project::ItemId,
    c: ClipId,
    t: Tick,
    sampled_is_source: bool,
    keep_before: usize,
    cmd: &str,
) -> Result<WbClip> {
    use filmcraft_color::wb::ClipLevel;
    let q = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
    let pipe = q.settings.color;
    let (_, it) = q.find_item(c).ok_or_else(|| bad(cmd, "no such clip"))?;
    let item = it.item;
    let fallback = WbClip { source: None, level: ClipLevel::working(wb_clip_level(pipe.working)) };
    let Some((_, media, _)) = s.project.resolve_media(item) else { return Ok(fallback) };
    let Some(v) = media.info.video.as_ref() else { return Ok(fallback) };
    let cs = media.interpret.color_space.unwrap_or_else(|| v.detected_color_space());
    // (stills and generators decode to RGB, full range, as the renderer treats them)
    let range = if media.info.kind == filmcraft_media::MediaKind::Movie { v.color.range } else { filmcraft_color::Range::Full };
    let peak = filmcraft_render::colorman::source_peak_nits(&s.project, item).filter(|p| p.is_finite() && *p > 0.0).map(|p| f64::from(p.round()));
    let level = ClipLevel::for_source(cs, range, &pipe, peak);
    if level.is_none() || sampled_is_source {
        return Ok(WbClip { source: None, level });
    }
    let mut probe = (*s.project).clone();
    if let Some((_, pi)) = probe.sequence_mut(seq_id).and_then(|q| q.find_item_mut(c)) {
        pi.grade = None;
        for (_, e) in pi.effects.iter_mut().enumerate().filter(|(i, e)| *i >= keep_before && !e.def().is_some_and(|d| d.intrinsic)) {
            e.enabled = false;
        }
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, working_output: true, ..Default::default() };
    let source = filmcraft_render::render_clip(&probe, seq_id, c, t, opts, &provider).ok_or_else(|| bad(cmd, "the clip has no picture"))?;
    Ok(WbClip { source: Some(source), level })
}

/// Solve Temperature / Tint for `p`'s point (`x`, `y`; a 5 × 5 px box at full size) or box (to
/// `x2`, `y2`) of `img` (working linear light at the white balance's input), or for Auto
/// balance's neutral estimate. A clipped spot is refused and Auto leaves clipped pixels out,
/// judged on `clip`.
pub(crate) fn wb_solve(
    img: &filmcraft_render::Image,
    clip: &WbClip,
    working: filmcraft_color::WorkingSpace,
    p: &Value,
    auto: bool,
    cmd: &str,
) -> Result<filmcraft_color::wb::WbSolve> {
    use filmcraft_color::wb::{self, ClipLevel};
    // the picture clipping is judged on, at the sample's size
    let (src, level) = match &clip.source {
        Some(src) if (src.w, src.h) == (img.w, img.h) => (src, clip.level),
        Some(_) => (img, ClipLevel::working(wb_clip_level(working))),
        None => (img, clip.level),
    };
    let rgb = if auto {
        let luma = filmcraft_color::ColorPipeline { working, ..Default::default() }.working_gamut().luma().map(|v| v as f32);
        let mut masked = img.px.clone();
        if !level.is_none() {
            for (m, sp) in masked.as_chunks_mut::<4>().0.iter_mut().zip(src.px.as_chunks::<4>().0) {
                let a = sp[3];
                if a.is_finite() && a > 1e-4 && level.is_clipped([sp[0] / a, sp[1] / a, sp[2] / a]) {
                    m[3] = 0.0;
                }
            }
        }
        wb::auto_neutral(&masked, img.w, img.h, luma, f32::INFINITY)
            .ok_or_else(|| bad(cmd, "not enough of the frame is grey or near-grey for Auto balance: pick something neutral instead"))?
    } else {
        let coord = |k: &str| p.get(k).and_then(Value::as_f64).map(|v| v as f32);
        let (Some(x), Some(y)) = (coord("x"), coord("y")) else {
            return Err(bad(cmd, "need `x` and `y` (0…1 of the frame)"));
        };
        let (x2, y2) = (coord("x2").unwrap_or(x), coord("y2").unwrap_or(y));
        // (the picture is a quarter size: 5 px of the frame are 2 here)
        let avg = |im: &filmcraft_render::Image| wb::box_average(&im.px, im.w, im.h, [x, y], [x2, y2], 2);
        let rgb = avg(img).ok_or_else(|| bad(cmd, wb::WbReject::Invalid.message()))?;
        let clipped = avg(src).is_some_and(|c| level.is_clipped(c));
        return wb::solve_judged(rgb, clipped).map_err(|e| bad(cmd, e.message()));
    };
    wb::solve(rgb, f32::INFINITY).map_err(|e| bad(cmd, e.message()))
}

/// The clip level of working linear light when the source isn't known: 1.0 in a
/// display-referred SDR space, none in scene-referred or HDR spaces (values above 1 are real
/// there). Media clips use their source's clip level instead ([`wb_clip`]).
pub(crate) fn wb_clip_level(working: filmcraft_color::WorkingSpace) -> f32 {
    if working.is_hdr() || working.is_scene_referred() { f32::INFINITY } else { 1.0 }
}

/// `grade.wb.pick` / `grade.wb.auto`: solve the node's Temperature and Tint so the picked spot
/// (or the frame's neutral estimate) becomes neutral, as one undo step. Sampling the node's input
/// makes picking twice give the same values.
fn pick_white_balance(s: &mut Session, p: &Value, auto: bool) -> Result<Value> {
    let cmd = if auto { "grade.wb.auto" } else { "grade.wb.pick" };
    // (the point is checked before rendering)
    if !auto {
        wb_point_check(p, cmd)?;
    }
    let (c, id, img, working) = node_input(s, p, cmd)?;
    // (on the first node the sample is the clip through its input transform alone)
    let first =
        s.active_sequence().and_then(|q| q.find_item(c)).and_then(|(_, it)| it.grade.as_ref()).is_some_and(|g| g.nodes.first().is_some_and(|n| n.id == id));
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let t = wb_time(s, p, c);
    let clip = wb_clip(s, seq_id, c, t, first, 0, cmd)?;
    let sol = wb_solve(&img, &clip, working, p, auto, cmd)?;
    let mt = media_time(s, p, c);
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.insert("clip".into(), json!(c.0));
    }
    edit_grade(s, &q, cmd, if auto { "Auto White Balance" } else { "Pick White Balance" }, false, |g| {
        let n = node_mut(g, id, cmd)?;
        for (k, v) in [("temperature", sol.temperature), ("tint", sol.tint)] {
            let v = ParamValue::Float(f64::from(v));
            match n.params.get_mut(k) {
                Some(prm) => prm.set_at(mt, v),
                None => {
                    n.params.insert(k.to_string(), Param::new(v));
                }
            }
        }
        Ok(())
    })?;
    Ok(wb_result(json!({"clip": c.0, "node": id}), sol))
}

/// Refuse a missing, non-numeric or out-of-frame point (or box corner) before anything is
/// rendered.
pub(crate) fn wb_point_check(p: &Value, cmd: &str) -> Result<()> {
    let v = |k: &str| p.get(k).filter(|v| !v.is_null()).map(Value::as_f64);
    let (Some(Some(_)), Some(Some(_))) = (v("x"), v("y")) else {
        return Err(bad(cmd, "need `x` and `y` (0…1 of the frame)"));
    };
    for k in ["x", "y", "x2", "y2"] {
        match v(k) {
            None => {}
            Some(Some(c)) if (0.0..=1.0).contains(&c) => {}
            Some(_) => return Err(bad(cmd, format!("`{k}` is outside the frame: pick a point inside the picture (0…1)"))),
        }
    }
    Ok(())
}

/// The command result: the solved values, and a note when the cast was beyond the sliders.
pub(crate) fn wb_result(mut base: Value, sol: filmcraft_color::wb::WbSolve) -> Value {
    if let Some(o) = base.as_object_mut() {
        o.insert("temperature".into(), json!(sol.temperature));
        o.insert("tint".into(), json!(sol.tint));
        o.insert("clamped".into(), json!(sol.clamped));
        if sol.clamped {
            o.insert("message".into(), json!("the colour cast is stronger than Temperature / Tint can fully remove: the sliders are at their limit"));
        }
    }
    base
}

fn pick_qualifier(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.pickQualifier";
    let coord = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0) as f32);
    let (Some(x), Some(y)) = (coord("x"), coord("y")) else {
        return Err(bad(CMD, "need `x` and `y` (0…1 of the frame)"));
    };
    let add = match str_p(p, "mode") {
        None | Some("set") => false,
        Some("add") => true,
        Some(m) => return Err(bad(CMD, format!("unknown mode `{m}` (set or add)"))),
    };
    let (c, id, img, working) = node_input(s, p, CMD)?;
    let rgb = sample(&img, x, y).ok_or_else(|| bad(CMD, "the clip has no picture there"))?;
    let [h, sat, l] = filmcraft_render::grade::qualifier_hsl(working, rgb);
    let mt = media_time(s, p, c);
    let set = edit_grade(s, p, CMD, "Pick Qualifier", false, |g| {
        let mut set = Vec::new();
        let n = node_mut(g, id, CMD)?;
        let was_on = n.float_at("qual.on", mt) >= 0.5;
        let vals: Vec<(&str, f32)> = if add && was_on {
            let f = |k| n.float_at(k, mt);
            // (a nearly grey sample has no meaningful hue: the hue band stays)
            let (hue, width) = if sat < 0.05 { (f("qual.hue"), f("qual.hueWidth")) } else { widen_hue(f("qual.hue"), f("qual.hueWidth"), h) };
            vec![
                ("qual.hue", hue),
                ("qual.hueWidth", width),
                ("qual.satLow", f("qual.satLow").min(sat - PICK_SAT)),
                ("qual.satHigh", f("qual.satHigh").max(sat + PICK_SAT)),
                ("qual.lumLow", f("qual.lumLow").min(l - PICK_LUM)),
                ("qual.lumHigh", f("qual.lumHigh").max(l + PICK_LUM)),
            ]
        } else {
            // a nearly grey sample has no meaningful hue: select every hue
            let width = if sat < 0.05 { 1.0 } else { PICK_HUE };
            vec![
                ("qual.on", 1.0),
                ("qual.hue", h),
                ("qual.hueWidth", width),
                ("qual.satLow", sat - PICK_SAT),
                ("qual.satHigh", sat + PICK_SAT),
                ("qual.lumLow", l - PICK_LUM),
                ("qual.lumHigh", l + PICK_LUM),
            ]
        };
        for (k, v) in vals {
            let Some(NodeParamKind::Float { min, max, .. }) = node_param(k).map(|d| d.kind) else { continue };
            let v = ParamValue::Float(f64::from(v.clamp(min, max)));
            match n.params.get_mut(k) {
                Some(prm) => prm.set_at(mt, v.clone()),
                None => {
                    n.params.insert(k.to_string(), Param::new(v.clone()));
                }
            }
            set.push((k, v));
        }
        Ok(set)
    })?;
    let params: serde_json::Map<String, Value> = set
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                match v {
                    ParamValue::Float(f) => json!(f),
                    _ => Value::Null,
                },
            )
        })
        .collect();
    Ok(json!({ "clip": c.0, "node": id, "sample": { "hue": h, "sat": sat, "luma": l }, "params": params }))
}

/// The node's LUT (G2.6): a reference (`lut`, as Lumetri's: `builtin:<id>`, `lib:<id>`; empty or
/// absent: none) or a `.cube` / `.3dl` file (`path`: imported into the project's LUT library
/// first, as its own "Import LUT" undo step, after the node is checked). Setting the reference
/// is one undo step.
fn set_lut(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.setLut";
    let want = node_arg(p, CMD)?;
    let (c, current) = current_for(s, p, CMD)?;
    // (check the node before importing anything, so a refused call leaves no library entry)
    {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        if !is_video(q, c) {
            return Err(bad(CMD, "a grade needs a clip on a video track"));
        }
        let g = q.find_item(c).and_then(|(_, it)| it.grade.as_ref()).ok_or_else(|| bad(CMD, "the clip has no grade"))?;
        let id = pick(g, want, current, CMD)?;
        g.node(id).ok_or_else(|| bad(CMD, format!("no node {id}")))?;
    }
    let r = match (str_p(p, "path"), str_p(p, "lut")) {
        (Some(path), _) => {
            let path = path.to_string();
            format!("lib:{}", crate::color::import_lut(s, &path, None)?.0)
        }
        (None, Some(r)) => r.to_string(),
        (None, None) => String::new(),
    };
    if !r.is_empty() && filmcraft_render::luts::resolve(Some(&s.project), &r).is_none() {
        return Err(bad(CMD, format!("unknown LUT `{r}`")));
    }
    let lut = r.clone();
    let id = edit_grade(s, p, CMD, "Set Node LUT", false, move |g| {
        let id = pick(g, want, current, CMD)?;
        node_mut(g, id, CMD)?.lut = lut;
        Ok(id)
    })?;
    Ok(json!({"node": id, "lut": r, "name": filmcraft_render::luts::label(Some(&s.project), &r)}))
}

/// Half-widths the picker selects around a sample (hue: the full width).
const PICK_HUE: f32 = 0.08;
const PICK_SAT: f32 = 0.2;
const PICK_LUM: f32 = 0.15;

/// The hue band (centre, full width, both 0…1, wrapping) widened to include `h` with a margin.
fn widen_hue(centre: f32, width: f32, h: f32) -> (f32, f32) {
    // signed distance from the centre to h around the circle, −½…½
    let d = (h - centre + 0.5).rem_euclid(1.0) - 0.5;
    let half = width / 2.0;
    if d.abs() + PICK_HUE / 4.0 <= half {
        return (centre, width);
    }
    // move the far edge out to h (plus a margin), keep the near edge
    let (lo, hi) = (centre - half, centre + half);
    let (lo, hi) = if d > 0.0 { (lo, centre + d + PICK_HUE / 4.0) } else { (centre + d - PICK_HUE / 4.0, hi) };
    let width = (hi - lo).min(1.0);
    (((lo + hi) / 2.0).rem_euclid(1.0), width)
}

/// The mean straight colour of the 3×3 pixels around `x`, `y` (0…1) of a premultiplied image;
/// None where the image is transparent.
fn sample(img: &filmcraft_render::Image, x: f32, y: f32) -> Option<[f32; 3]> {
    if img.w == 0 || img.h == 0 {
        return None;
    }
    let cx = ((x * img.w as f32) as usize).min(img.w - 1);
    let cy = ((y * img.h as f32) as usize).min(img.h - 1);
    let mut acc = [0.0f32; 4];
    for yy in cy.saturating_sub(1)..=(cy + 1).min(img.h - 1) {
        for xx in cx.saturating_sub(1)..=(cx + 1).min(img.w - 1) {
            let i = (yy * img.w + xx) * 4;
            if let Some(px) = img.px.get(i..i + 4) {
                for (a, v) in acc.iter_mut().zip(px) {
                    *a += if v.is_finite() { *v } else { 0.0 };
                }
            }
        }
    }
    if acc[3] <= 1e-4 {
        return None;
    }
    Some([acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3]])
}

pub(crate) fn kind_hint(kind: NodeParamKind) -> &'static str {
    match kind {
        NodeParamKind::Float { .. } => "a finite number",
        NodeParamKind::Wheel { .. } => "[r, g, b] or [r, g, b, master] (finite numbers)",
        NodeParamKind::Curve => "a list of [x, y] points in 0…1 (at most 64)",
    }
}

fn toggle_animation(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.toggleAnimation";
    let want = node_arg(p, CMD)?;
    let current = current_for(s, p, CMD)?.1;
    let key = str_p(p, "param").ok_or_else(|| bad(CMD, "need `param`"))?;
    let def = node_param(key).ok_or_else(|| bad(CMD, format!("no node parameter `{key}`")))?;
    if matches!(def.kind, NodeParamKind::Curve) {
        return Err(bad(CMD, "curves are not keyframed"));
    }
    let c = target(s, p, CMD)?;
    let mt = media_time(s, p, c);
    let animated = edit_grade(s, p, CMD, "Toggle Animation", false, |g| {
        let id = pick(g, want, current, CMD)?;
        let n = node_mut(g, id, CMD)?;
        let prm = n.params.entry(def.key.to_string()).or_insert_with(|| Param::new(identity(def.kind)));
        prm.toggle_animation(mt);
        Ok(prm.is_animated())
    })?;
    Ok(json!({ "animated": animated }))
}

fn reset(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.resetNode";
    let want = node_arg(p, CMD)?;
    let current = current_for(s, p, CMD)?.1;
    let key = match str_p(p, "param") {
        Some(k) => Some(node_param(k).ok_or_else(|| bad(CMD, format!("no node parameter `{k}`")))?.key),
        None => None,
    };
    edit_grade(s, p, CMD, "Reset Node", false, |g| {
        let id = pick(g, want, current, CMD)?;
        let n = node_mut(g, id, CMD)?;
        match key {
            Some(k) => {
                n.params.remove(k);
            }
            None => n.params.clear(),
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn bypass(s: &mut Session, p: &Value) -> Result<Value> {
    let on = bool_p(p, "on");
    let now = edit_grade(s, p, "grade.bypass", "Bypass Grade", false, |g| {
        g.bypass = on.unwrap_or(!g.bypass);
        Ok(g.bypass)
    })?;
    Ok(json!({ "bypass": now }))
}

fn clear(s: &mut Session, p: &Value) -> Result<Value> {
    let c = target(s, p, "grade.clear")?;
    s.edit_sequence("Remove Grade", |q, _, _| {
        let (_, it) = q.find_item_mut(c).ok_or(filmcraft_edit::EditError::NoItem(c))?;
        if it.grade.take().is_none() {
            return Err(bad("grade.clear", "the clip has no grade"));
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------------------------
// Copy / paste / append / apply (GRADE-PLAN G1.4)
// ---------------------------------------------------------------------------------------------

/// The most clips one grade command touches.
const MAX_TARGETS: usize = 10_000;

/// The video clips `clips` names (every one must be a clip on a video track), else the selected
/// video clips; `skip` is left out. Empty is an error.
pub(crate) fn grade_targets(s: &Session, p: &Value, cmd: &str, skip: Option<ClipId>) -> Result<Vec<ClipId>> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut out: Vec<ClipId> = Vec::new();
    let mut push = |c: ClipId| {
        if Some(c) != skip && !out.contains(&c) && out.len() < MAX_TARGETS {
            out.push(c);
        }
    };
    match p.get("clips") {
        None | Some(Value::Null) => s.state.selection.iter().copied().filter(|c| is_video(q, *c)).for_each(&mut push),
        Some(Value::Array(a)) => {
            for v in a {
                let c = v.as_u64().map(ClipId).ok_or_else(|| bad(cmd, "`clips` must be clip ids"))?;
                if !is_video(q, c) {
                    return Err(bad(cmd, format!("clip {} is not a clip on a video track", c.0)));
                }
                push(c);
            }
        }
        Some(_) => return Err(bad(cmd, "`clips` must be a list of clip ids")),
    }
    if out.is_empty() {
        return Err(bad(cmd, "need `clips` (or selected video clips)"));
    }
    Ok(out)
}

/// Run `f` on the grade slot of every target as one undo step; returns how many clips changed.
pub(crate) fn edit_grades(s: &mut Session, targets: &[ClipId], label: &str, f: impl Fn(&mut Option<Grade>)) -> Result<usize> {
    let r = s.edit_sequence(label, |q, _, _| {
        let mut n = 0;
        for c in targets {
            let (_, it) = q.find_item_mut(*c).ok_or(filmcraft_edit::EditError::NoItem(*c))?;
            let before = it.grade.clone();
            f(&mut it.grade);
            if it.grade != before {
                n += 1;
            }
        }
        Ok(n)
    });
    s.fix_state();
    r
}

fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    let c = target(s, p, "grade.copy")?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(c).ok_or_else(|| bad("grade.copy", "no such clip"))?;
    let g = it.grade.clone().ok_or_else(|| bad("grade.copy", "the clip has no grade"))?;
    let nodes = g.nodes.len();
    s.state.grade_clipboard = Some(g);
    Ok(json!({ "clip": c.0, "nodes": nodes }))
}

fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    let g = s.state.grade_clipboard.clone().ok_or_else(|| bad("grade.paste", "copy a grade first (grade.copy)"))?;
    let targets = grade_targets(s, p, "grade.paste", None)?;
    let n = edit_grades(s, &targets, "Paste Grade", |slot| *slot = Some(g.clone()))?;
    Ok(json!({ "clips": n }))
}

/// Append `src`'s nodes after `dst`'s with fresh ids; nodes beyond [`MAX_NODES`] are dropped.
/// Returns false (and leaves `dst` alone) when a branched result couldn't render.
fn append_nodes(dst: &mut Grade, src: &Grade) -> bool {
    let old: Vec<u64> = dst.nodes.iter().map(|n| n.id).collect();
    // (the appended nodes go to the right of the grade they join, fork C2b)
    let right = dst.layout().iter().map(|(_, p)| p[0]).fold(f32::MIN, f32::max);
    let ok = append_nodes_unplaced(dst, src);
    if ok {
        let new: Vec<u64> = dst.nodes.iter().map(|n| n.id).filter(|id| !old.contains(id)).collect();
        let src_left = src.nodes.iter().filter_map(|n| src.stored_pos(n.id)).map(|p| p[0]).fold(f32::MAX, f32::min);
        if src_left < f32::MAX && !old.is_empty() {
            dst.offset_positions(&new, [right + filmcraft_project::grade::LAYOUT_COLUMN - src_left, 0.0]);
        }
        // a hand-made layout receiving nodes without positions: they go after its output
        let unplaced: Vec<u64> = new.iter().copied().filter(|id| dst.stored_pos(*id).is_none()).collect();
        if !unplaced.is_empty() {
            let anchor = dst.output_node().ok().filter(|o| old.contains(o));
            dst.place_new(&unplaced, anchor);
        }
    }
    ok
}

fn append_nodes_unplaced(dst: &mut Grade, src: &Grade) -> bool {
    // a branched grade on either side (fork C2): the connections come along
    if dst.is_graph() || src.is_graph() {
        return crate::grade_graph::append_graph(dst, src);
    }
    for node in &src.nodes {
        if dst.nodes.len() >= MAX_NODES {
            break;
        }
        let id = dst.next_id();
        if id == 0 {
            break;
        }
        let mut copy = node.clone();
        copy.id = id;
        dst.nodes.push(copy);
    }
    true
}

fn append(s: &mut Session, p: &Value) -> Result<Value> {
    let g = s.state.grade_clipboard.clone().ok_or_else(|| bad("grade.append", "copy a grade first (grade.copy)"))?;
    let targets = grade_targets(s, p, "grade.append", None)?;
    let full = s
        .active_sequence()
        .is_some_and(|q| targets.iter().all(|c| q.find_item(*c).and_then(|(_, it)| it.grade.as_ref()).is_some_and(|d| d.nodes.len() >= MAX_NODES)));
    if full && !g.nodes.is_empty() {
        return Err(bad("grade.append", format!("a grade holds at most {MAX_NODES} nodes")));
    }
    // a branched append that couldn't render on some clip is refused before anything changes
    if let Some(q) = s.active_sequence() {
        for c in &targets {
            let mut trial = q.find_item(*c).and_then(|(_, it)| it.grade.clone()).unwrap_or_default();
            if !append_nodes(&mut trial, &g) {
                return Err(bad("grade.append", format!("appended to clip {}, the grade's nodes would not render (too many nodes or connections)", c.0)));
            }
        }
    }
    let n = edit_grades(s, &targets, "Append Grade", |slot| {
        append_nodes(slot.get_or_insert_with(Grade::default), &g);
    })?;
    Ok(json!({ "clips": n }))
}

fn apply_from(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.applyFrom";
    let from = p.get("from").and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad(CMD, "need `from` (a clip id)"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(from).ok_or_else(|| bad(CMD, "no such clip"))?;
    if !is_video(q, from) {
        return Err(bad(CMD, "`from` must be a clip on a video track"));
    }
    let g = it.grade.clone().ok_or_else(|| bad(CMD, "the source clip has no grade"))?;
    let targets = grade_targets(s, p, CMD, Some(from))?;
    let n = edit_grades(s, &targets, "Apply Grade", |slot| *slot = Some(g.clone()))?;
    Ok(json!({ "clips": n }))
}

#[cfg(test)]
mod pick_tests {
    use super::widen_hue;

    fn contains(centre: f32, width: f32, h: f32) -> bool {
        let d = (h - centre + 0.5).rem_euclid(1.0) - 0.5;
        d.abs() <= width / 2.0 + 1e-5
    }

    #[test]
    fn widen_hue_wraps_keeps_and_caps() {
        // inside the band: unchanged
        assert_eq!(widen_hue(0.3, 0.2, 0.32), (0.3, 0.2));
        // across 0 / 1, both directions
        for (c, w, h) in [(0.95, 0.1, 0.05), (0.05, 0.1, 0.9), (0.5, 0.0, 0.7), (0.2, 0.05, 0.1)] {
            let (nc, nw) = widen_hue(c, w, h);
            assert!((0.0..1.0).contains(&nc) && nw <= 1.0 && nw >= w, "{c} {w} {h} → {nc} {nw}");
            assert!(contains(nc, nw, h), "{c} {w} {h} → {nc} {nw} misses h");
            // the old band's near edge is kept
            assert!(contains(nc, nw, c), "{c} {w} {h} → {nc} {nw} misses the old centre");
        }
        // never wider than every hue
        assert!(widen_hue(0.0, 0.9, 0.5).1 <= 1.0);
    }
}
