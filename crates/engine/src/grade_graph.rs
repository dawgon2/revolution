//! Grade graph commands (fork C2, `docs/fork/DESIGN-C2.md` §4): branch a clip's grade with
//! parallel, layer, outside, splitter / combiner and key-mixer nodes, connect and disconnect ports
//! and pick the output node. Every edit is one undo step; an edit that would leave a graph that
//! can't render ([`Grade::graph_order`] fails) is refused with the reason.
//!
//! The first branching edit writes the serial chain out as explicit edges
//! ([`Grade::make_explicit`]) in the same undo step; an edit that leaves a plain chain drops them
//! again ([`Grade::simplify`]). While an edit runs, the output node is pinned
//! ([`Grade::output`]) so a node left unconnected never makes the output ambiguous; afterwards
//! the pin is dropped again when the output is the graph's only sink anyway.
//!
//! Ports (DESIGN-C2 §1): a correction node takes RGB on input 0 and a key on input 1, and gives
//! RGB on output 0 and its key on output 1; the grade's input is the virtual node
//! [`SOURCE`] (id 0, one output).

use std::collections::BTreeSet;

use filmcraft_project::dag::{self, Edge, NodeId, Port};
use filmcraft_project::grade::{Grade, GradeNode, MAX_MIX_INPUTS, MAX_NODES, NodeKind, SOURCE, clean_pos};
use filmcraft_project::{ClipId, Param, ParamValue};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad};
use crate::grade::{current_for, edit_grade, edit_grade_merged, is_video, node_arg, pick};
use crate::{Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_grade, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("grade.addParallel", "Add Parallel Node", r#"{"clip":id?,"node":id?}"#, |s, p| add_branch(s, p, Branch::Parallel)),
        spec("grade.addLayer", "Add Layer Node", r#"{"clip":id?,"node":id?}"#, |s, p| add_branch(s, p, Branch::Layer)),
        spec("grade.addOutside", "Add Outside Node", r#"{"clip":id?,"node":id?}"#, |s, p| add_branch(s, p, Branch::Outside)),
        spec("grade.addSplitter", "Add Splitter / Combiner Nodes", r#"{"clip":id?,"node":id?}"#, |s, p| add_branch(s, p, Branch::Splitter)),
        spec("grade.addKeyMixer", "Add Key Mixer", r#"{"clip":id?,"node":id?}"#, |s, p| add_branch(s, p, Branch::KeyMixer)),
        spec("grade.connect", "Connect Nodes", r#"{"clip":id?,"from":node|0,"fromPort":n?,"to":node,"toPort":n?}"#, connect),
        spec("grade.disconnect", "Disconnect Nodes", r#"{"clip":id?,"to":node,"toPort":n?}"#, disconnect),
        spec("grade.setOutput", "Set Grade Output", r#"{"clip":id?,"node":id}"#, set_output),
        spec("grade.moveNodes", "Move Nodes on the Canvas", r#"{"clip":id?,"nodes":[{"node":id|0,"x":num,"y":num}],"gesture":n?}"#, move_nodes),
        spec("grade.resetLayout", "Reset Layout", r#"{"clip":id?}"#, reset_layout),
    ]
}

/// Enabled when a selected (or named) video clip has a grade with at least one node.
fn has_grade(s: &Session) -> std::result::Result<(), String> {
    let q = s.active_sequence().ok_or_else(|| "no sequence is open".to_string())?;
    let graded = |c: ClipId| is_video(q, c) && q.find_item(c).and_then(|(_, it)| it.grade.as_ref()).is_some_and(|g| !g.nodes.is_empty());
    if s.state.selection.iter().any(|c| graded(*c)) { Ok(()) } else { Err("select a video clip with grade nodes".into()) }
}

// ---------- graph helpers (also used by the serial `grade.*` commands on a branched grade) ----------

/// Starts a graph edit: writes the serial chain out as edges and pins the current output node.
pub(crate) fn begin(g: &mut Grade) {
    g.make_explicit();
    if g.output.is_none()
        && let Ok(o) = g.output_node()
        && o != SOURCE
    {
        g.output = Some(o);
    }
}

/// Ends a graph edit: refuses a graph that can't render, drops the output pin when the output is
/// the only sink anyway, and drops the edges when the graph is a plain chain again.
pub(crate) fn finish(g: &mut Grade, cmd: &str) -> Result<()> {
    if let Err(e) = g.graph_order() {
        return Err(bad(cmd, format!("refused: the grade's nodes would not render ({e})")));
    }
    tidy(g);
    Ok(())
}

/// Drops a redundant output pin and, for a plain chain, the edges.
fn tidy(g: &mut Grade) {
    if let Some(o) = g.output.take()
        && g.output_node() != Ok(o)
    {
        g.output = Some(o);
    }
    g.simplify();
}

/// The output port feeding input `port` of `node`.
fn input_of(g: &Grade, node: NodeId, port: u8) -> Option<Port> {
    g.edges.iter().find(|e| e.to == Port::new(node, port)).map(|e| e.from)
}

/// Every connection leaving `old` leaves `new` instead.
fn reroute_from(g: &mut Grade, old: Port, new: Port) {
    for e in g.edges.iter_mut().filter(|e| e.from == old) {
        e.from = new;
    }
}

fn push_edge(g: &mut Grade, from: Port, to: Port, cmd: &str) -> Result<()> {
    if g.edges.len() >= dag::MAX_EDGES {
        return Err(bad(cmd, format!("a grade holds at most {} connections", dag::MAX_EDGES)));
    }
    g.edges.push(Edge::new(from, to));
    Ok(())
}

/// A fresh node of `kind` placed after `after` in the node list (the list order only matters for
/// a serial grade and for layout ties).
fn new_node(g: &mut Grade, kind: NodeKind, label: &str, after: NodeId, cmd: &str) -> Result<NodeId> {
    if g.nodes.len() >= MAX_NODES {
        return Err(bad(cmd, format!("a grade holds at most {MAX_NODES} nodes")));
    }
    let id = g.next_id();
    if id == SOURCE || g.node(id).is_some() {
        return Err(bad(cmd, "no free node id"));
    }
    let mut n = GradeNode::new(id);
    n.kind = kind;
    n.label = label.to_string();
    let at = g.nodes.iter().position(|n| n.id == after).map_or(g.nodes.len(), |i| i.saturating_add(1));
    g.nodes.insert(at.min(g.nodes.len()), n);
    Ok(id)
}

/// Whether a port carries a key (a matte) rather than a picture: a correction node's port 1 and
/// every port of a key mixer; everything else (the grade's input included) carries RGB.
pub(crate) fn is_key_port(g: &Grade, node: NodeId, port: u8) -> bool {
    match g.node(node).map(|n| n.kind) {
        Some(NodeKind::Correction) => port == 1,
        Some(NodeKind::KeyMixer) => true,
        _ => false,
    }
}

/// Refuses a serial add anchored on a node whose output is a key (a key mixer).
fn picture_anchor(g: &Grade, anchor: NodeId, cmd: &str) -> Result<()> {
    if is_key_port(g, anchor, 0) {
        return Err(bad(cmd, format!("node {anchor} gives a key, not a picture: add the node elsewhere")));
    }
    Ok(())
}

/// Wires a just-inserted node `y` in after `anchor`'s RGB output (`None`: after the output node,
/// or the grade's input when there is none): what fed from there now feeds from `y`.
pub(crate) fn wire_after(g: &mut Grade, anchor: Option<NodeId>, y: NodeId, cmd: &str) -> Result<()> {
    let a = anchor.or(g.output).unwrap_or(SOURCE);
    picture_anchor(g, a, cmd)?;
    let port = Port::new(a, 0);
    reroute_from(g, port, Port::new(y, 0));
    push_edge(g, port, Port::new(y, 0), cmd)?;
    if g.output == Some(a) || (a == SOURCE && g.output.is_none()) {
        g.output = Some(y);
    }
    Ok(())
}

/// Wires a just-inserted node `y` in before `anchor`'s RGB input (`None`: straight after the
/// grade's input).
pub(crate) fn wire_before(g: &mut Grade, anchor: Option<NodeId>, y: NodeId, cmd: &str) -> Result<()> {
    match anchor {
        Some(x) => {
            picture_anchor(g, x, cmd)?;
            for e in g.edges.iter_mut().filter(|e| e.to == Port::new(x, 0)) {
                e.to = Port::new(y, 0);
            }
            push_edge(g, Port::new(y, 0), Port::new(x, 0), cmd)
        }
        None => wire_after(g, Some(SOURCE), y, cmd),
    }
}

/// The nodes deleting `x` takes with it: a splitter or combiner takes its partner and the channel
/// nodes between them (those fed only by the splitter and feeding only the combiner), so the pair
/// never goes half; any other node goes alone.
fn deleted_with(g: &Grade, x: NodeId) -> Vec<NodeId> {
    let outs = |n: NodeId| g.edges.iter().filter(move |e| e.from.node == n).map(|e| e.to.node);
    let ins = |n: NodeId| g.edges.iter().filter(move |e| e.to.node == n).map(|e| e.from.node);
    let is = |n: NodeId, k: NodeKind| g.node(n).is_some_and(|n| n.kind == k);
    let (s, c) = match g.node(x).map(|n| n.kind) {
        Some(NodeKind::Splitter) => (Some(x), outs(x).flat_map(|m| std::iter::once(m).chain(outs(m))).find(|m| is(*m, NodeKind::Combiner))),
        Some(NodeKind::Combiner) => (ins(x).flat_map(|m| std::iter::once(m).chain(ins(m))).find(|m| is(*m, NodeKind::Splitter)), Some(x)),
        _ => return vec![x],
    };
    let (Some(s), Some(c)) = (s, c) else { return vec![x] };
    let mut gone = vec![s, c];
    for m in outs(s) {
        if m != c && !gone.contains(&m) && ins(m).all(|i| i == s) && outs(m).all(|o| o == c) {
            gone.push(m);
        }
    }
    gone
}

/// Unhooks node `x` (and what goes with it, [`deleted_with`]) from a branched grade before
/// deletion: what the group's output fed is fed from the group's first connected input of the
/// same kind (picture or key) instead, and every other connection of the group goes. The output
/// moves upstream with it. Returns the ids to delete.
pub(crate) fn unhook(g: &mut Grade, x: NodeId) -> Vec<NodeId> {
    let gone = deleted_with(g, x);
    // the group's entry (the splitter, else x) and exit (the combiner, else x)
    let kind_in = |k: NodeKind| gone.iter().copied().find(|n| g.node(*n).is_some_and(|n| n.kind == k));
    let entry = kind_in(NodeKind::Splitter).unwrap_or(x);
    let exit = kind_in(NodeKind::Combiner).unwrap_or(x);
    let out_key = is_key_port(g, exit, 0);
    let up = g
        .edges
        .iter()
        .filter(|e| e.to.node == entry && is_key_port(g, entry, e.to.index) == out_key && !gone.contains(&e.from.node))
        .min_by_key(|e| e.to.index)
        .map(|e| e.from);
    let downstream: Vec<Port> = g.edges.iter().filter(|e| e.from == Port::new(exit, 0) && !gone.contains(&e.to.node)).map(|e| e.to).collect();
    g.edges.retain(|e| !gone.contains(&e.from.node) && !gone.contains(&e.to.node));
    if let Some(up) = up {
        g.edges.extend(downstream.into_iter().map(|to| Edge::new(up, to)));
    }
    if g.output.is_some_and(|o| gone.contains(&o)) {
        g.output = up.map(|p| p.node).filter(|n| *n != SOURCE);
    }
    gone
}

/// Makes `g` render only up to node `id`'s output (a measurement of one node). A serial grade
/// keeps the nodes up to it; a graph keeps every node and makes `id` the output.
pub(crate) fn probe_through(g: &mut Grade, id: NodeId) {
    if g.is_graph() {
        g.output = Some(id);
    } else if let Some(pos) = g.nodes.iter().position(|n| n.id == id) {
        g.nodes.truncate(pos.saturating_add(1));
    }
}

/// Makes `g` render node `id`'s input picture: the nodes before it (serial) or the node feeding
/// its RGB input (graph); no grade at all when that is the grade's input.
pub(crate) fn probe_before(g: &mut Grade, id: NodeId) {
    if g.is_graph() {
        match input_of(g, id, 0) {
            Some(p) if p.node != SOURCE && g.node(p.node).is_some() => g.output = Some(p.node),
            _ => probe_ungraded(g),
        }
    } else if let Some(pos) = g.nodes.iter().position(|n| n.id == id) {
        g.nodes.truncate(pos);
    }
}

/// Makes `g` an empty grade (the picture before any node).
pub(crate) fn probe_ungraded(g: &mut Grade) {
    g.nodes.clear();
    g.edges.clear();
    g.output = None;
}

/// Appends `src`'s nodes (and connections) after `dst`'s output with fresh ids, for a branched
/// grade on either side. Nodes past [`MAX_NODES`] are dropped with their connections (the output
/// is then the last copied node along the picture's path). Returns false, leaving `dst`
/// unchanged, when the result couldn't render.
pub(crate) fn append_graph(dst: &mut Grade, src: &Grade) -> bool {
    let mut out = dst.clone();
    begin(&mut out);
    let tail = out.output.unwrap_or(SOURCE);
    let mut map: Vec<(NodeId, NodeId)> = Vec::new();
    for node in &src.nodes {
        if out.nodes.len() >= MAX_NODES {
            break;
        }
        let id = out.next_id();
        if id == SOURCE || out.node(id).is_some() {
            break;
        }
        let mut copy = node.clone();
        copy.id = id;
        out.nodes.push(copy);
        map.push((node.id, id));
    }
    let new_id = |old: NodeId| if old == SOURCE { Some(tail) } else { map.iter().find(|(o, _)| *o == old).map(|(_, n)| *n) };
    for e in src.effective_edges() {
        let (Some(f), Some(t)) = (new_id(e.from.node), new_id(e.to.node)) else { continue };
        // the source's input is the destination's output picture (its RGB port)
        let fi = if e.from.node == SOURCE { 0 } else { e.from.index };
        if out.edges.len() < dag::MAX_EDGES {
            out.edges.push(Edge::new(Port::new(f, fi), Port::new(t, e.to.index)));
        }
    }
    match src.output_node().ok().and_then(new_id) {
        Some(n) => out.output = Some(n).filter(|n| *n != SOURCE).or(out.output),
        None => {
            // the source's output wasn't copied: follow the picture from the old end through the
            // copied nodes as far as it goes
            let copied: Vec<NodeId> = map.iter().map(|(_, n)| *n).collect();
            let mut end = tail;
            for _ in 0..copied.len() {
                let Some(next) = out.edges.iter().find(|e| e.from == Port::new(end, 0) && copied.contains(&e.to.node)).map(|e| e.to.node) else { break };
                end = next;
            }
            if end != SOURCE {
                out.output = Some(end);
            }
        }
    }
    if out.graph_order().is_err() {
        return false;
    }
    tidy(&mut out);
    *dst = out;
    true
}

// ---------- commands ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Branch {
    Parallel,
    Layer,
    Outside,
    Splitter,
    KeyMixer,
}

impl Branch {
    fn cmd(self) -> &'static str {
        match self {
            Branch::Parallel => "grade.addParallel",
            Branch::Layer => "grade.addLayer",
            Branch::Outside => "grade.addOutside",
            Branch::Splitter => "grade.addSplitter",
            Branch::KeyMixer => "grade.addKeyMixer",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Branch::Parallel => "Add Parallel Node",
            Branch::Layer => "Add Layer Node",
            Branch::Outside => "Add Outside Node",
            Branch::Splitter => "Add Splitter / Combiner Nodes",
            Branch::KeyMixer => "Add Key Mixer",
        }
    }
    /// How many nodes the edit adds at most.
    fn adds(self) -> usize {
        match self {
            Branch::Parallel | Branch::Layer => 2,
            Branch::Outside | Branch::KeyMixer => 1,
            Branch::Splitter => 5,
        }
    }
}

fn add_branch(s: &mut Session, p: &Value, b: Branch) -> Result<Value> {
    let cmd = b.cmd();
    let want = node_arg(p, cmd)?;
    let (clip, current) = current_for(s, p, cmd)?;
    let (select, added) = edit_grade(s, p, cmd, b.label(), false, |g| {
        let x = pick(g, want, current, cmd)?;
        let kind = g.node(x).map(|n| n.kind).ok_or_else(|| bad(cmd, format!("no node {x}")))?;
        let picture = matches!(kind, NodeKind::Correction | NodeKind::ParallelMixer | NodeKind::LayerMixer | NodeKind::Combiner);
        if b == Branch::Splitter && !picture {
            return Err(bad(cmd, format!("node {x} gives no RGB picture to split")));
        }
        if b != Branch::Splitter && kind != NodeKind::Correction {
            return Err(bad(cmd, format!("node {x} is not a correction node")));
        }
        if g.nodes.len().saturating_add(b.adds()) > MAX_NODES {
            return Err(bad(cmd, format!("a grade holds at most {MAX_NODES} nodes")));
        }
        begin(g);
        let added = match b {
            Branch::Parallel => mixer(g, x, NodeKind::ParallelMixer, cmd)?,
            Branch::Layer => mixer(g, x, NodeKind::LayerMixer, cmd)?,
            Branch::Outside => outside(g, x, cmd)?,
            Branch::Splitter => splitter(g, x, cmd)?,
            Branch::KeyMixer => {
                let k = new_node(g, NodeKind::KeyMixer, "", x, cmd)?;
                push_edge(g, Port::new(x, 1), Port::new(k, 0), cmd)?;
                vec![k]
            }
        };
        finish(g, cmd)?;
        // (a grade with a hand-made layout: the new nodes go next to `x`, fork C2b)
        g.place_new(&added, Some(x));
        let select = match b {
            // the new correction node (a splitter: the red channel's node)
            Branch::Splitter => added.get(1).copied(),
            _ => added.first().copied(),
        };
        Ok((select, added))
    })?;
    if let Some(node) = select {
        s.state.grade_node = Some(crate::GradeSel { clip, node });
    }
    Ok(json!({ "node": select, "nodes": added }))
}

/// Parallel / layer: a new correction node beside `x`, fed from `x`'s input, both into a mixer of
/// `kind` (a new one, or the one `x` already feeds alone). Returns the new nodes, correction first.
fn mixer(g: &mut Grade, x: NodeId, kind: NodeKind, cmd: &str) -> Result<Vec<NodeId>> {
    let src = input_of(g, x, 0).ok_or_else(|| bad(cmd, format!("node {x} has no input to share (connect its input first)")))?;
    let outs: Vec<Port> = g.edges.iter().filter(|e| e.from == Port::new(x, 0)).map(|e| e.to).collect();
    // x already feeds a mixer of this kind (and nothing else): the new node joins that mixer
    if let [only] = outs.as_slice()
        && g.node(only.node).is_some_and(|m| m.kind == kind)
    {
        let m = only.node;
        let used: BTreeSet<u8> = g.edges.iter().filter(|e| e.to.node == m).map(|e| e.to.index).collect();
        let port = (0..MAX_MIX_INPUTS).find(|i| !used.contains(i)).ok_or_else(|| bad(cmd, format!("a mixer takes at most {MAX_MIX_INPUTS} inputs")))?;
        let last_in = g.edges.iter().filter(|e| e.to.node == m).filter_map(|e| (e.from.node != SOURCE).then_some(e.from.node)).next_back().unwrap_or(x);
        let y = new_node(g, NodeKind::Correction, "", last_in, cmd)?;
        push_edge(g, src, Port::new(y, 0), cmd)?;
        push_edge(g, Port::new(y, 0), Port::new(m, port), cmd)?;
        return Ok(vec![y]);
    }
    let y = new_node(g, NodeKind::Correction, "", x, cmd)?;
    let m = new_node(g, kind, "", y, cmd)?;
    reroute_from(g, Port::new(x, 0), Port::new(m, 0));
    if g.output == Some(x) {
        g.output = Some(m);
    }
    push_edge(g, src, Port::new(y, 0), cmd)?;
    push_edge(g, Port::new(x, 0), Port::new(m, 0), cmd)?;
    push_edge(g, Port::new(y, 0), Port::new(m, 1), cmd)?;
    Ok(vec![y, m])
}

/// Outside: a correction node after `x` whose key input is `x`'s key output, inverted.
fn outside(g: &mut Grade, x: NodeId, cmd: &str) -> Result<Vec<NodeId>> {
    let y = new_node(g, NodeKind::Correction, "", x, cmd)?;
    if let Some(n) = g.node_mut(y) {
        n.params.insert("key.invert".into(), Param::new(ParamValue::Float(1.0)));
    }
    wire_after(g, Some(x), y, cmd)?;
    push_edge(g, Port::new(x, 1), Port::new(y, 1), cmd)?;
    Ok(vec![y])
}

/// Splitter / combiner after `x`, with one correction node per channel between them. Returns
/// splitter, red, green, blue, combiner.
fn splitter(g: &mut Grade, x: NodeId, cmd: &str) -> Result<Vec<NodeId>> {
    let sp = new_node(g, NodeKind::Splitter, "", x, cmd)?;
    let mut prev = sp;
    let mut chans = Vec::new();
    for label in ["Red", "Green", "Blue"] {
        prev = new_node(g, NodeKind::Correction, label, prev, cmd)?;
        chans.push(prev);
    }
    let co = new_node(g, NodeKind::Combiner, "", prev, cmd)?;
    reroute_from(g, Port::new(x, 0), Port::new(co, 0));
    if g.output == Some(x) {
        g.output = Some(co);
    }
    push_edge(g, Port::new(x, 0), Port::new(sp, 0), cmd)?;
    for (i, c) in (0u8..).zip(chans.iter().copied()) {
        push_edge(g, Port::new(sp, i), Port::new(c, 0), cmd)?;
        push_edge(g, Port::new(c, 0), Port::new(co, i), cmd)?;
    }
    let mut all = vec![sp];
    all.extend(chans);
    all.push(co);
    Ok(all)
}

/// A node id parameter (0 is the grade's input).
fn id_p(p: &Value, k: &str, cmd: &str) -> Result<NodeId> {
    p.get(k).and_then(Value::as_u64).ok_or_else(|| bad(cmd, format!("need `{k}` (a node id; 0 is the grade's input)")))
}

/// A port parameter (default 0).
fn port_p(p: &Value, k: &str, cmd: &str) -> Result<u8> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v.as_u64().and_then(|n| u8::try_from(n).ok()).ok_or_else(|| bad(cmd, format!("`{k}` must be a port number (0…255)"))),
    }
}

/// Output / input port counts of `id` (the grade's input: one output, no input).
fn ports(g: &Grade, id: NodeId) -> Option<(u8, u8)> {
    if id == SOURCE {
        return Some((1, 0));
    }
    g.node(id).map(|n| (n.kind.outputs(), n.kind.inputs()))
}

fn connect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.connect";
    let (from, to) = (id_p(p, "from", CMD)?, id_p(p, "to", CMD)?);
    let (fp, tp) = (port_p(p, "fromPort", CMD)?, port_p(p, "toPort", CMD)?);
    edit_grade(s, p, CMD, "Connect Nodes", false, |g| {
        let (outs, _) = ports(g, from).ok_or_else(|| bad(CMD, format!("no node {from}")))?;
        if fp >= outs {
            return Err(bad(CMD, format!("node {from} has no output {fp}")));
        }
        if to == SOURCE {
            return Err(bad(CMD, "the grade's input takes no connections"));
        }
        let (_, ins) = ports(g, to).ok_or_else(|| bad(CMD, format!("no node {to}")))?;
        if tp >= ins {
            return Err(bad(CMD, format!("node {to} has no input {tp}")));
        }
        if from == to {
            return Err(bad(CMD, "a node can't feed itself"));
        }
        let kind = |key: bool| if key { "a key" } else { "a picture" };
        let (fk, tk) = (is_key_port(g, from, fp), is_key_port(g, to, tp));
        if fk != tk {
            return Err(bad(CMD, format!("output {fp} of node {from} gives {}, but input {tp} of node {to} takes {}", kind(fk), kind(tk))));
        }
        begin(g);
        // the input's old connection is replaced
        g.edges.retain(|e| e.to != Port::new(to, tp));
        push_edge(g, Port::new(from, fp), Port::new(to, tp), CMD)?;
        finish(g, CMD)
    })?;
    Ok(Value::Null)
}

fn disconnect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.disconnect";
    let to = id_p(p, "to", CMD)?;
    let tp = port_p(p, "toPort", CMD)?;
    edit_grade(s, p, CMD, "Disconnect Nodes", false, |g| {
        if !g.effective_edges().iter().any(|e| e.to == Port::new(to, tp)) {
            return Err(bad(CMD, format!("input {tp} of node {to} isn't connected")));
        }
        begin(g);
        g.edges.retain(|e| e.to != Port::new(to, tp));
        // (no edges at all would read as the serial chain again)
        if g.edges.is_empty() && !g.nodes.is_empty() {
            return Err(bad(CMD, "that is the grade's only connection; delete the node instead"));
        }
        finish(g, CMD)
    })?;
    Ok(Value::Null)
}

fn set_output(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.setOutput";
    let node = id_p(p, "node", CMD)?;
    edit_grade(s, p, CMD, "Set Grade Output", false, |g| {
        let kind = g.node(node).map(|n| n.kind).ok_or_else(|| bad(CMD, format!("no node {node}")))?;
        if matches!(kind, NodeKind::KeyMixer | NodeKind::Splitter) {
            return Err(bad(CMD, format!("node {node} gives no RGB picture")));
        }
        begin(g);
        g.output = Some(node);
        finish(g, CMD)
    })?;
    Ok(Value::Null)
}

/// Refuses an edit the serial commands can't express on a branched grade.
pub(crate) fn serial_only(g: &Grade, cmd: &str) -> Result<()> {
    if g.is_graph() { Err(bad(cmd, "the grade is branched: connect nodes on the node canvas instead")) } else { Ok(()) }
}

/// `grade.moveNodes` (fork C2b): stores where nodes sit on the node canvas (`nodes`: node, x, y in
/// canvas units; node 0 is the grade's input). Non-finite coordinates are refused, huge ones
/// clamped. The calls of one drag share one undo step: same `gesture` number (the canvas sends a
/// new one per drag); without one every call is its own undo step.
fn move_nodes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.moveNodes";
    let list = p.get("nodes").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `nodes` ([{\"node\": id, \"x\": num, \"y\": num}])"))?;
    if list.is_empty() || list.len() > MAX_NODES.saturating_add(1) {
        return Err(bad(CMD, format!("`nodes` takes 1…{} entries", MAX_NODES.saturating_add(1))));
    }
    let mut moves: Vec<(NodeId, [f32; 2])> = Vec::with_capacity(list.len());
    for v in list {
        let node = v.get("node").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "each entry needs `node` (a node id; 0 is the grade's input)"))?;
        let coord = |k: &str| v.get(k).and_then(Value::as_f64);
        let (Some(x), Some(y)) = (coord("x"), coord("y")) else {
            return Err(bad(CMD, format!("node {node} needs finite `x` and `y`")));
        };
        let pos = clean_pos(x, y).ok_or_else(|| bad(CMD, format!("node {node} needs finite `x` and `y`")))?;
        moves.retain(|(n, _)| *n != node);
        moves.push((node, pos));
    }
    let merge = match p.get("gesture") {
        None | Some(Value::Null) => None,
        Some(v) => Some(format!("moveNodes/g{}", v.as_u64().ok_or_else(|| bad(CMD, "`gesture` must be a whole number"))?)),
    };
    edit_grade_merged(s, p, CMD, "Move Nodes", false, merge.as_deref(), |g| {
        for (node, pos) in &moves {
            if !g.set_pos(*node, Some(*pos)) {
                return Err(bad(CMD, format!("no node {node}")));
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

/// `grade.resetLayout` (fork C2b): forgets every stored node position, so the canvas lays the
/// grade out automatically again. One undo step; refused when nothing was placed by hand.
fn reset_layout(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.resetLayout";
    edit_grade(s, p, CMD, "Reset Layout", false, |g| if g.clear_positions() { Ok(()) } else { Err(bad(CMD, "the nodes are already laid out automatically")) })?;
    Ok(Value::Null)
}
