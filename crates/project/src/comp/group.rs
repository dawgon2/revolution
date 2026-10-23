//! Node groups / macros (fork C4.3, ADR 0009 §2): a [`CompKind::Group`] node holds an inner comp
//! of the same size and rate. Its `MediaIn { GroupInput { index } }` nodes read the group node's
//! inputs, its output is the group's output, and [`ExposedParam`]s map parameters of the group
//! node (kept in its own `params`) onto inner nodes' parameters, which they override when the
//! group is evaluated ([`inner_with_overrides`]).
//!
//! Bounds (hostile project files and commands): groups nest at most [`MAX_GROUP_DEPTH`] deep (a
//! deeper group is emptied), the inner nodes of every level count toward [`dag::MAX_NODES`]
//! together, a group exposes at most [`MAX_EXPOSED`] parameters, each pointing at an existing
//! inner node and parameter, and has at most [`MAX_GROUP_INPUTS`] inputs.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{Comp, CompKind, CompNode, MAX_LABEL, MediaInSource, params};
use crate::dag::{self, DagError, Edge, NodeId, Port};
use crate::effect::ParamDef;
use crate::keyframe::Param;

/// Deepest groups nest (a group inside a group … this many levels).
pub const MAX_GROUP_DEPTH: usize = 4;
/// Most parameters one group exposes.
pub const MAX_EXPOSED: usize = 64;
/// Most inputs one group has.
pub const MAX_GROUP_INPUTS: u32 = 8;

/// One parameter of a group node: `id` (its key in the group node's `params`, and what
/// `comp.setParam` names) drives parameter `param` of inner node `node`. For an inner Group node,
/// `param` is one of that group's own exposed ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExposedParam {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    pub node: NodeId,
    pub param: String,
}

/// A group's input ports: one past the highest Group Input index of its inner comp (its own
/// level only; nested groups read their own inputs), at most [`MAX_GROUP_INPUTS`].
pub fn input_count(inner: &Comp) -> u8 {
    let top = inner
        .nodes
        .iter()
        .filter_map(|n| match n.kind {
            CompKind::MediaIn { source: MediaInSource::GroupInput { index }, .. } => Some(index.min(MAX_GROUP_INPUTS - 1).saturating_add(1)),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    u8::try_from(top).unwrap_or(0)
}

/// Brings the groups of `c` (a comp `depth` groups deep) within their bounds: Group Input indices
/// below [`MAX_GROUP_INPUTS`]; each group's inside the outer size, rate and colour, sanitized
/// (its nodes taken from `budget`), emptied when it would nest deeper than [`MAX_GROUP_DEPTH`];
/// exposed parameters capped, unique, pointing at existing inner parameters; the group node's
/// params only its exposed ones.
pub(super) fn sanitize_groups(c: &mut Comp, depth: usize, budget: &mut usize) {
    let (w, h, rate, duration, color) = (c.width, c.height, c.rate, c.duration, c.color);
    for n in &mut c.nodes {
        if let CompKind::MediaIn { source: MediaInSource::GroupInput { index }, .. } = &mut n.kind {
            *index = (*index).min(MAX_GROUP_INPUTS - 1);
        }
        let CompKind::Group { comp: inner, exposed } = &mut n.kind else { continue };
        inner.width = w;
        inner.height = h;
        inner.rate = rate;
        inner.duration = duration;
        inner.color = color;
        if depth.saturating_add(1) > MAX_GROUP_DEPTH {
            inner.nodes = Vec::new();
            inner.edges = Vec::new();
            inner.output = None;
            exposed.clear();
            n.params.clear();
            continue;
        }
        inner.sanitize_at(depth.saturating_add(1), budget);
        exposed.truncate(MAX_EXPOSED);
        let mut seen = BTreeSet::new();
        exposed.retain_mut(|e| {
            e.id = e.id.chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect();
            e.label = e.label.chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect();
            !e.id.is_empty() && seen.insert(e.id.clone()) && resolve_def(inner, e.node, &e.param, 0).is_some()
        });
        // only exposed ids, each holding values of its inner parameter's type
        let defs: BTreeMap<String, ParamDef> = exposed.iter().filter_map(|e| resolve_def(inner, e.node, &e.param, 0).map(|d| (e.id.clone(), d))).collect();
        n.params.retain(|k, p| {
            defs.get(k).is_some_and(|d| {
                let want = std::mem::discriminant(&d.default);
                std::mem::discriminant(&p.value) == want && p.keyframes.iter().all(|k| std::mem::discriminant(&k.value) == want)
            })
        });
    }
}

/// How many groups deep `c` nests (0: no group), counted at most [`MAX_GROUP_DEPTH`] + 1 deep.
pub fn nesting(c: &Comp) -> usize {
    fn walk(c: &Comp, depth: usize) -> usize {
        if depth > MAX_GROUP_DEPTH {
            return depth;
        }
        c.nodes
            .iter()
            .filter_map(|n| match &n.kind {
                CompKind::Group { comp, .. } => Some(walk(comp, depth.saturating_add(1))),
                _ => None,
            })
            .max()
            .unwrap_or(depth)
    }
    walk(c, 0)
}

/// The schema of parameter `param` of inner node `node` of `c` (through nested groups' exposed
/// parameters, at most [`MAX_GROUP_DEPTH`] levels), its default the node's current static value.
/// Only schema parameters ([`params`]) can be exposed (not a Color node's grade or a Text layer's).
fn resolve_def(c: &Comp, node: NodeId, param: &str, depth: usize) -> Option<ParamDef> {
    if depth > MAX_GROUP_DEPTH {
        return None;
    }
    let n = c.node(node)?;
    let mut def = match &n.kind {
        CompKind::Group { comp, exposed } => {
            let e = exposed.iter().find(|e| e.id == param)?;
            resolve_def(comp, e.node, &e.param, depth.saturating_add(1))?
        }
        kind => params(kind).into_iter().find(|d| d.id == param)?,
    };
    if let Some(p) = n.params.get(param) {
        def.default = p.value.clone();
    }
    Some(def)
}

/// A group node's parameters: each exposed parameter with the schema of the inner parameter it
/// drives (default: the inner parameter's current value). Empty for other nodes.
pub fn exposed_params(n: &CompNode) -> Vec<(ExposedParam, ParamDef)> {
    let CompKind::Group { comp, exposed } = &n.kind else { return Vec::new() };
    exposed.iter().filter_map(|e| resolve_def(comp, e.node, &e.param, 0).map(|d| (e.clone(), d))).collect()
}

/// The schema of a group node's parameter `key` (an exposed id).
pub fn param_def(n: &CompNode, key: &str) -> Option<ParamDef> {
    let CompKind::Group { comp, exposed } = &n.kind else { return None };
    let e = exposed.iter().find(|e| e.id == key)?;
    resolve_def(comp, e.node, &e.param, 0)
}

/// A group node's inner comp as it is evaluated: its exposed parameters that the group node sets
/// override the inner nodes' (borrowed when none does). None for other nodes.
pub fn inner_with_overrides(n: &CompNode) -> Option<Cow<'_, Comp>> {
    let CompKind::Group { comp, exposed } = &n.kind else { return None };
    let set: Vec<(&ExposedParam, &Param)> = exposed.iter().filter_map(|e| n.params.get(&e.id).map(|p| (e, p))).collect();
    if set.is_empty() {
        return Some(Cow::Borrowed(&**comp));
    }
    let mut inner = (**comp).clone();
    for (e, p) in set {
        if let Some(target) = inner.node_mut(e.node) {
            target.params.insert(e.param.clone(), p.clone());
        }
    }
    Some(Cow::Owned(inner))
}

impl Comp {
    /// Every node, groups' inner nodes included (at most [`MAX_GROUP_DEPTH`] levels down).
    pub fn all_nodes(&self) -> Vec<&CompNode> {
        fn walk<'a>(c: &'a Comp, depth: usize, out: &mut Vec<&'a CompNode>) {
            for n in &c.nodes {
                out.push(n);
                if let CompKind::Group { comp, .. } = &n.kind
                    && depth < MAX_GROUP_DEPTH
                {
                    walk(comp, depth.saturating_add(1), out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, 0, &mut out);
        out
    }

    /// [`Comp::all_nodes`] mutably (the same nodes, for remapping item ids on paste).
    pub fn for_each_node_mut(&mut self, f: &mut dyn FnMut(&mut CompNode)) {
        fn walk(c: &mut Comp, depth: usize, f: &mut dyn FnMut(&mut CompNode)) {
            for n in &mut c.nodes {
                f(n);
                if let CompKind::Group { comp, .. } = &mut n.kind
                    && depth < MAX_GROUP_DEPTH
                {
                    walk(comp, depth.saturating_add(1), f);
                }
            }
        }
        walk(self, 0, f);
    }
}

/// Why `path` (a group node id, then nested group ids) can't address a group's inside of a comp
/// (fork C4.8): empty, deeper than [`MAX_GROUP_DEPTH`], or through a node that isn't there or
/// isn't a group. In plain words.
fn path_error(c: &Comp, path: &[NodeId]) -> Option<String> {
    if path.is_empty() {
        return Some("the group path is empty; leave it out to edit the comp's own nodes".into());
    }
    if path.len() > MAX_GROUP_DEPTH {
        return Some(format!("groups nest at most {MAX_GROUP_DEPTH} deep, so a group path has at most {MAX_GROUP_DEPTH} node ids (it has {})", path.len()));
    }
    let mut cur = c;
    let mut at = String::from("in the comp");
    for id in path {
        let Some(n) = cur.node(*id) else { return Some(format!("there is no node {id} {at}")) };
        let CompKind::Group { comp, .. } = &n.kind else { return Some(format!("node {id} {at} is a {} node, not a group", n.kind.key())) };
        at = format!("inside group {id}");
        cur = comp;
    }
    None
}

/// The inside of the group at `path` of `c` (outer group id, then nested group ids, at most
/// [`MAX_GROUP_DEPTH`]); the reason in plain words when the path leads nowhere.
pub fn inner<'a>(c: &'a Comp, path: &[NodeId]) -> Result<&'a Comp, String> {
    if let Some(e) = path_error(c, path) {
        return Err(e);
    }
    let mut cur = c;
    for id in path {
        match cur.node(*id).map(|n| &n.kind) {
            Some(CompKind::Group { comp, .. }) => cur = comp,
            _ => return Err(format!("node {id} is not a group")),
        }
    }
    Ok(cur)
}

/// [`inner`] mutably: the comp the group-path edits of the comp commands work on.
pub fn inner_mut<'a>(c: &'a mut Comp, path: &[NodeId]) -> Result<&'a mut Comp, String> {
    if let Some(e) = path_error(c, path) {
        return Err(e);
    }
    let mut cur = c;
    for id in path {
        let Some(n) = cur.node_mut(*id) else { return Err(format!("there is no node {id}")) };
        match &mut n.kind {
            CompKind::Group { comp, .. } => cur = &mut **comp,
            _ => return Err(format!("node {id} is not a group")),
        }
    }
    Ok(cur)
}

/// A copy of `c` whose result is node `node` of the group at `path` (empty: of `c` itself) as it
/// is evaluated in place (LO.4: what a Tracker or Roto node inside a group analyses): at each
/// level along the path the comp's output is the next group (that group not bypassed), and the
/// innermost level's output is `node`, so its Group Inputs read the group's inputs from the
/// enclosing level and the groups' exposed parameters apply. The reason in plain words when the
/// path or node leads nowhere.
pub fn view_of(c: &Comp, path: &[NodeId], node: NodeId) -> Result<Comp, String> {
    if !path.is_empty()
        && let Some(e) = path_error(c, path)
    {
        return Err(e);
    }
    let mut view = c.clone();
    let mut cur = &mut view;
    for id in path {
        cur.output = Some(*id);
        let Some(n) = cur.node_mut(*id) else { return Err(format!("there is no node {id}")) };
        n.bypass = false;
        match &mut n.kind {
            CompKind::Group { comp, .. } => cur = &mut **comp,
            _ => return Err(format!("node {id} is not a group")),
        }
    }
    if cur.node(node).is_none() {
        return Err(format!("no node {node}"));
    }
    cur.output = Some(node);
    Ok(view)
}

/// After an edit inside the group at `path`: connections into group inputs that no longer exist
/// (a Group Input removed inside) are dropped at every level along the path.
pub fn prune_group_inputs(c: &mut Comp, path: &[NodeId]) {
    let mut cur = c;
    for id in path.iter().take(MAX_GROUP_DEPTH) {
        let ins = cur.node(*id).map_or(0, |n| n.kind.inputs());
        cur.edges.retain(|e| e.to.node != *id || e.to.index < ins);
        let Some(n) = cur.node_mut(*id) else { return };
        let CompKind::Group { comp, .. } = &mut n.kind else { return };
        cur = &mut **comp;
    }
}

/// Where the nodes of a new group go: the Group node's id.
fn place(nodes: &[&CompNode]) -> [f32; 2] {
    let k = nodes.len().max(1) as f32;
    let sum = nodes.iter().fold([0.0f32, 0.0], |a, n| [a[0] + n.pos[0], a[1] + n.pos[1]]);
    [sum[0] / k, sum[1] / k]
}

/// Puts nodes `ids` of `c` into one new Group node (labelled `label`) and returns its id: edges
/// into the selection become the group's inputs (one per distinct source, in input order), the
/// one node the selection's outside consumers read becomes its output. Refused, with the reason in
/// plain words and `c` unchanged, for an empty selection, unknown nodes, Media Out / the comp's
/// output, a selection that feeds nothing outside it, has more than one output or nodes that
/// don't lead to it, more than [`MAX_GROUP_INPUTS`] inputs, a loop through an outside node, or
/// too many nodes.
pub fn group_nodes(c: &mut Comp, ids: &[NodeId], label: &str) -> Result<NodeId, String> {
    let sel: BTreeSet<NodeId> = ids.iter().copied().collect();
    if sel.is_empty() {
        return Err("select the nodes to group".into());
    }
    for id in &sel {
        let n = c.node(*id).ok_or_else(|| format!("no node {id}"))?;
        if matches!(n.kind, CompKind::MediaOut) || c.output == Some(*id) {
            return Err("Media Out (the comp's result) can't go into a group; leave it out of the selection".into());
        }
        if matches!(n.kind, CompKind::MediaIn { source: MediaInSource::GroupInput { .. }, .. }) {
            return Err("a Group Input reads its own group's input, so it can't go into a group inside it; leave it out of the selection".into());
        }
    }
    // the selection's output: the one node its outside consumers read
    let exits: Vec<Edge> = c.edges.iter().filter(|e| sel.contains(&e.from.node) && !sel.contains(&e.to.node)).copied().collect();
    let outs: BTreeSet<Port> = exits.iter().map(|e| e.from).collect();
    let out = match outs.len() {
        0 => return Err("the selected nodes don't feed anything outside them, so a group of them would show nothing; connect them first".into()),
        1 => outs.iter().next().copied().ok_or("no output")?,
        _ => {
            let names: Vec<String> = outs.iter().map(|p| p.node.to_string()).collect();
            return Err(format!(
                "nodes {} all feed nodes outside the selection; a group has one output, so select the nodes that flow into one",
                names.join(", ")
            ));
        }
    };
    // every selected node must lead to the output inside the selection
    let mut reach = BTreeSet::from([out.node]);
    let mut stack = vec![out.node];
    while let Some(x) = stack.pop() {
        for e in c.edges.iter().filter(|e| e.to.node == x && sel.contains(&e.from.node)) {
            if reach.insert(e.from.node) {
                stack.push(e.from.node);
            }
        }
    }
    if let Some(stray) = sel.iter().find(|id| !reach.contains(id)) {
        return Err(format!("node {stray} doesn't lead to node {} inside the selection; a group's nodes must flow into its output", out.node));
    }
    // the inputs: each distinct outside source, in the order of the inputs it feeds
    let mut entries: Vec<Edge> = c.edges.iter().filter(|e| !sel.contains(&e.from.node) && sel.contains(&e.to.node)).copied().collect();
    entries.sort_by_key(|e| e.to);
    let mut sources: Vec<Port> = Vec::new();
    for e in &entries {
        if !sources.contains(&e.from) {
            sources.push(e.from);
        }
    }
    // the new group around the deepest selected group: within MAX_GROUP_DEPTH
    let deepest = c
        .nodes
        .iter()
        .filter(|n| sel.contains(&n.id))
        .filter_map(|n| match &n.kind {
            CompKind::Group { comp, .. } => Some(nesting(comp).saturating_add(1)),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    if deepest.saturating_add(1) > MAX_GROUP_DEPTH {
        return Err(format!("groups nest at most {MAX_GROUP_DEPTH} deep, and the selection already holds groups {deepest} deep"));
    }
    if sources.len() > MAX_GROUP_INPUTS as usize {
        return Err(format!("the selection reads {} pictures from outside it; a group has at most {MAX_GROUP_INPUTS} inputs", sources.len()));
    }
    // inputs + Media Out + the group node itself: within the node cap
    let total = c.all_nodes().len().saturating_add(sources.len()).saturating_add(2);
    if total > dag::MAX_NODES {
        return Err(format!("a comp holds at most {} nodes (groups' insides included)", dag::MAX_NODES));
    }

    let picked: Vec<&CompNode> = c.nodes.iter().filter(|n| sel.contains(&n.id)).collect();
    let pos = place(&picked);
    let (min_x, max_x) = picked.iter().fold((f32::MAX, f32::MIN), |(a, b), n| (a.min(n.pos[0]), b.max(n.pos[0])));
    let mut inner = Comp { nodes: picked.into_iter().cloned().collect(), edges: Vec::new(), output: None, ..Comp::new(c.width, c.height, c.rate, c.duration) };
    inner.color = c.color;
    inner.edges = c.edges.iter().filter(|e| sel.contains(&e.from.node) && sel.contains(&e.to.node)).copied().collect();
    let mut next = inner.next_id();
    for (i, src) in sources.iter().enumerate() {
        let id = next;
        next = next.saturating_add(1);
        let mut gi = CompNode::new(id, CompKind::MediaIn { source: MediaInSource::GroupInput { index: i as u32 }, offset: filmcraft_time::Tick::ZERO });
        gi.label = format!("Input {}", i.saturating_add(1));
        gi.pos = [min_x - 200.0, i as f32 * 80.0];
        inner.nodes.push(gi);
        for e in entries.iter().filter(|e| e.from == *src) {
            inner.edges.push(Edge::new(Port::new(id, 0), e.to));
        }
    }
    let mo = next;
    inner.nodes.push(CompNode { pos: [max_x + 200.0, 0.0], ..CompNode::new(mo, CompKind::MediaOut) });
    inner.edges.push(Edge::new(out, Port::new(mo, 0)));
    inner.output = Some(mo);

    let gid = c.next_id();
    let mut g = CompNode::new(gid, CompKind::Group { comp: Box::new(inner), exposed: Vec::new() });
    g.label = if label.trim().is_empty() { "Group".into() } else { label.chars().take(MAX_LABEL).collect() };
    g.pos = pos;
    let mut next_c = c.clone();
    next_c.nodes.retain(|n| !sel.contains(&n.id));
    next_c.edges.retain(|e| !sel.contains(&e.from.node) && !sel.contains(&e.to.node));
    next_c.nodes.push(g);
    for (i, src) in sources.iter().enumerate() {
        next_c.edges.push(Edge::new(*src, Port::new(gid, i as u8)));
    }
    for e in &exits {
        next_c.edges.push(Edge::new(Port::new(gid, 0), e.to));
    }
    for at in std::iter::once(gid).chain(next_c.output) {
        match dag::validate(&next_c.nodes, &next_c.edges, at) {
            Ok(_) => {}
            Err(DagError::Cycle(_)) => {
                return Err("the selection feeds a node outside it that feeds the selection back; a group of it would make a loop".into());
            }
            Err(e) => return Err(format!("the graph can't be grouped: {e}")),
        }
    }
    *c = next_c;
    Ok(gid)
}

/// Replaces Group node `id` of `c` with its inner nodes (exposed values the group sets written
/// into them, so the picture doesn't change): Group Inputs replaced by what fed the group's
/// inputs, the group's consumers reading what its inner Media Out showed. Inner node ids are kept
/// where the outer comp doesn't use them, else renumbered. Returns the ids of the nodes put back.
pub fn ungroup(c: &mut Comp, id: NodeId) -> Result<Vec<NodeId>, String> {
    let g = c.node(id).ok_or_else(|| format!("no node {id}"))?;
    let inner = inner_with_overrides(g).ok_or_else(|| format!("node {id} is not a group"))?.into_owned();
    let mut next_c = c.clone();
    next_c.nodes.retain(|n| n.id != id);
    // outer sources of each group input, the group's consumers
    let feeds: BTreeMap<u8, Port> = c.edges.iter().filter(|e| e.to.node == id).map(|e| (e.to.index, e.from)).collect();
    let consumers: Vec<Port> = c.edges.iter().filter(|e| e.from.node == id).map(|e| e.to).collect();
    next_c.edges.retain(|e| e.from.node != id && e.to.node != id);
    // what the inner comp shows: its Media Out's input, else its output node itself
    let result: Option<Port> = match inner.output.and_then(|o| inner.node(o)) {
        Some(n) if matches!(n.kind, CompKind::MediaOut) => inner.edges.iter().find(|e| e.to == Port::new(n.id, 0)).map(|e| e.from),
        Some(n) => Some(Port::new(n.id, 0)),
        None => None,
    };
    let dropped = |n: &CompNode| {
        matches!(n.kind, CompKind::MediaIn { source: MediaInSource::GroupInput { .. }, .. })
            || (inner.output == Some(n.id) && matches!(n.kind, CompKind::MediaOut))
    };
    // inner ids → outer ids (fresh ones above every outer and inner id; the node count can only
    // shrink: the group node, its inputs and its Media Out go)
    let mut map: BTreeMap<NodeId, NodeId> = BTreeMap::new();
    let mut next = next_c.next_id().max(id.saturating_add(1)).max(inner.next_id());
    for n in inner.nodes.iter().filter(|n| !dropped(n)) {
        let new = if next_c.node(n.id).is_none() && n.id != id && !map.values().any(|v| *v == n.id) {
            n.id
        } else {
            let v = next;
            next = next.saturating_add(1);
            v
        };
        map.insert(n.id, new);
    }
    let group_input = |node: NodeId| match inner.node(node).map(|n| &n.kind) {
        Some(CompKind::MediaIn { source: MediaInSource::GroupInput { index }, .. }) => Some(u8::try_from(*index).unwrap_or(u8::MAX)),
        _ => None,
    };
    // an inner port's outer replacement (None: nothing there)
    let outer = |p: Port| -> Option<Port> {
        match group_input(p.node) {
            Some(i) => feeds.get(&i).copied(),
            None => map.get(&p.node).map(|n| Port::new(*n, p.index)),
        }
    };
    for n in inner.nodes.iter().filter(|n| !dropped(n)) {
        let mut n = n.clone();
        n.id = map.get(&n.id).copied().unwrap_or(n.id);
        next_c.nodes.push(n);
    }
    for e in &inner.edges {
        let Some(to) = map.get(&e.to.node) else { continue };
        if let Some(from) = outer(e.from) {
            next_c.edges.push(Edge::new(from, Port::new(*to, e.to.index)));
        }
    }
    if let Some(r) = result.and_then(outer) {
        for to in consumers {
            next_c.edges.push(Edge::new(r, to));
        }
        if c.output == Some(id) {
            next_c.output = Some(r.node);
        }
    } else if c.output == Some(id) {
        next_c.output = None;
    }
    if next_c.edges.len() > dag::MAX_EDGES {
        return Err(format!("ungrouping would need more than {} connections", dag::MAX_EDGES));
    }
    *c = next_c;
    Ok(map.into_values().collect())
}

/// Exposes parameter `param` of inner node `inner` on group node `group` of `c` (labelled
/// `label`): returns its id (the parameter's name, made unique).
pub fn expose(c: &mut Comp, group: NodeId, inner: NodeId, param: &str, label: &str) -> Result<String, String> {
    let n = c.node_mut(group).ok_or_else(|| format!("no node {group}"))?;
    let CompKind::Group { comp, exposed } = &mut n.kind else { return Err(format!("node {group} is not a group")) };
    let target = comp.node(inner).ok_or_else(|| format!("the group has no node {inner} inside it"))?;
    let def = resolve_def(comp, inner, param, 0).ok_or_else(|| {
        format!(
            "a {} node inside the group has no parameter `{param}` that can be exposed (see comp.params; Color and Text node settings can't be exposed yet)",
            target.kind.key()
        )
    })?;
    if exposed.iter().any(|e| e.node == inner && e.param == param) {
        return Err(format!("`{param}` of node {inner} is already exposed"));
    }
    if exposed.len() >= MAX_EXPOSED {
        return Err(format!("a group exposes at most {MAX_EXPOSED} parameters"));
    }
    let base: String = param.chars().take(MAX_LABEL.saturating_sub(4)).collect();
    let id = (1..=MAX_EXPOSED + 1)
        .map(|k| if k == 1 { base.clone() } else { format!("{base}_{k}") })
        .find(|id| !exposed.iter().any(|e| &e.id == id))
        .ok_or("no free parameter id")?;
    let label = if label.trim().is_empty() { def.label.to_string() } else { label.chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect() };
    exposed.push(ExposedParam { id: id.clone(), label, node: inner, param: param.to_string() });
    Ok(id)
}

/// Removes exposed parameter `id` from group node `group`; a value the group set for it is written
/// into the inner parameter, so the picture doesn't change.
pub fn unexpose(c: &mut Comp, group: NodeId, id: &str) -> Result<(), String> {
    let n = c.node_mut(group).ok_or_else(|| format!("no node {group}"))?;
    let CompKind::Group { comp, exposed } = &mut n.kind else { return Err(format!("node {group} is not a group")) };
    let at = exposed.iter().position(|e| e.id == id).ok_or_else(|| format!("the group has no exposed parameter `{id}`"))?;
    let e = exposed.remove(at);
    let value = n.params.remove(id);
    if let Some(v) = value
        && let Some(t) = comp.node_mut(e.node)
    {
        t.params.insert(e.param, v);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
