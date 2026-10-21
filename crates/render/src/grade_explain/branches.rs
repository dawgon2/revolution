//! Plain-language notes on a branched grade (fork C2, G2.10–11): what the parallel, layer,
//! splitter / combiner, key-mixer and outside nodes do with the pictures and selections that
//! reach them. These are read from the graph and the mixers' settings, not measured, so they are
//! exact and cost no rendering. A serial grade has none.

use filmcraft_project::dag::{Edge, NodeId};
use filmcraft_project::effect::BLEND_MODES;
use filmcraft_project::grade::{Grade, GradeNode, MAX_MIX_INPUTS, MAX_NODES, NodeKind, SOURCE};
use filmcraft_time::Tick;

/// How a node is named in the sentences: its name, or "node N" by position (1-based, the same
/// numbers the Nodes strip shows).
fn name(g: &Grade, id: NodeId) -> String {
    if id == SOURCE {
        return "the original picture".to_string();
    }
    match g.nodes.iter().take(MAX_NODES).enumerate().find(|(_, n)| n.id == id) {
        Some((_, n)) if !n.label.is_empty() => format!("'{}'", n.label),
        Some((pos, _)) => format!("node {}", pos.saturating_add(1)),
        None => "an unknown node".to_string(),
    }
}

/// A node's name as a possessive: "node 3's", "the 'Skin' node's".
fn poss(g: &Grade, id: NodeId) -> String {
    match name(g, id) {
        n if n.starts_with('\'') => format!("the {n} node's"),
        n => format!("{n}'s"),
    }
}

/// The sentence with its first letter in capitals ("node 3: …" starts it).
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Connections into `node`: (its input index, the source node, the source's output index).
fn feeds(edges: &[Edge], node: NodeId) -> Vec<(u8, NodeId, u8)> {
    let mut v: Vec<(u8, NodeId, u8)> = edges.iter().filter(|e| e.to.node == node).map(|e| (e.to.index, e.from.node, e.from.index)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// A mixer's picture inputs: (input index, source node).
fn rgb_feeds(edges: &[Edge], node: NodeId) -> Vec<(u8, NodeId)> {
    feeds(edges, node).into_iter().filter(|(i, _, from_port)| *i < MAX_MIX_INPUTS && *from_port != 1).map(|(i, f, _)| (i, f)).collect()
}

fn blend_name(n: &GradeNode, i: u8, t: Tick) -> String {
    let m = n.float_at(&format!("layer.mode.{i}"), t).round();
    let m = if m.is_finite() && m >= 0.0 { m as usize } else { 0 };
    BLEND_MODES.get(m).copied().unwrap_or("Normal").to_string()
}

fn percent(v: f32) -> i64 {
    if v.is_finite() { (f64::from(v) * 100.0).round() as i64 } else { 0 }
}

/// One sentence per node that branches or selects, in node order. Empty for a serial grade.
pub fn explain_branches(g: &Grade, t: Tick) -> Vec<String> {
    if !g.is_graph() {
        return Vec::new();
    }
    let edges = g.effective_edges();
    let mut out = Vec::new();
    for n in g.nodes.iter().take(MAX_NODES) {
        let me = name(g, n.id);
        let s = match n.kind {
            NodeKind::Correction => correction_note(g, n, &edges, &me, t),
            NodeKind::ParallelMixer => Some(parallel_note(g, n, &edges, &me, t)),
            NodeKind::LayerMixer => Some(layer_note(g, n, &edges, &me, t)),
            NodeKind::Splitter => Some(splitter_note(g, n, &edges, &me)),
            NodeKind::Combiner => Some(combiner_note(g, n, &edges, &me)),
            NodeKind::KeyMixer => Some(key_mixer_note(g, n, &edges, &me, t)),
        };
        out.extend(s.map(|t| capitalize(&t)));
    }
    out
}

fn parallel_note(g: &Grade, n: &GradeNode, edges: &[Edge], me: &str, t: Tick) -> String {
    let ins = rgb_feeds(edges, n.id);
    if ins.len() < 2 {
        return format!("{me} is a parallel mixer with fewer than two inputs, so it passes its picture through.");
    }
    let weights: Vec<f32> = ins.iter().map(|(i, _)| n.float_at(&format!("mix.gain.{i}"), t).max(0.0)).collect();
    let total: f32 = weights.iter().sum();
    let names: Vec<String> = ins.iter().map(|(_, f)| name(g, *f)).collect();
    let share: Vec<String> = weights.iter().map(|w| if total > 0.0 { percent(w / total).to_string() } else { "0".into() }).collect();
    // branches fed by the same picture work on the same image
    let sources: Vec<Option<NodeId>> = ins.iter().map(|(_, f)| feeds(edges, *f).into_iter().find(|(i, _, p)| *i == 0 && *p != 1).map(|(_, s, _)| s)).collect();
    let same = sources.first().is_some_and(|f| f.is_some() && sources.iter().all(|x| x == f));
    let lead = if same { "each work on the same image" } else { "each give a picture" };
    format!("{me}: {} {lead}; their results are blended {}.", join_and(&names), share.join(" / "))
}

fn layer_note(g: &Grade, n: &GradeNode, edges: &[Edge], me: &str, t: Tick) -> String {
    let ins = rgb_feeds(edges, n.id);
    if ins.len() < 2 {
        return format!("{me} is a layer mixer with fewer than two inputs, so it passes its picture through.");
    }
    let mut parts = Vec::new();
    for (k, (i, f)) in ins.iter().enumerate().skip(1) {
        let below = match ins.get(k.saturating_sub(1)) {
            Some((_, b)) if k == 1 => name(g, *b),
            _ => "the layers below".to_string(),
        };
        let op = percent(n.float_at(&format!("layer.opacity.{i}"), t).clamp(0.0, 1.0));
        parts.push(format!("{} look sits on top of {below} at {op} % opacity, {}", poss(g, *f), blend_name(n, *i, t)));
    }
    format!("{me}: {}.", parts.join("; "))
}

fn splitter_note(g: &Grade, n: &GradeNode, edges: &[Edge], me: &str) -> String {
    let outs: Vec<String> = ["red", "green", "blue"]
        .iter()
        .zip(0u8..)
        .map(|(c, i)| {
            let to: Vec<String> = edges.iter().filter(|e| e.from.node == n.id && e.from.index == i).map(|e| name(g, e.to.node)).collect();
            if to.is_empty() { format!("{c} goes nowhere") } else { format!("{c} goes to {}", join_and(&to)) }
        })
        .collect();
    format!("{me} splits the picture into its red, green and blue channels, each a grey picture: {}.", outs.join("; "))
}

fn combiner_note(g: &Grade, n: &GradeNode, edges: &[Edge], me: &str) -> String {
    let by_port = feeds(edges, n.id);
    let mut parts = Vec::new();
    for (c, i) in ["red", "green", "blue"].iter().zip(0u8..) {
        if let Some((_, f, _)) = by_port.iter().find(|(p, _, _)| *p == i) {
            parts.push(format!("{c} from {}", name(g, *f)));
        }
    }
    if parts.is_empty() {
        return format!("{me} puts three channels back into one colour picture, but none are connected.");
    }
    format!("{me} puts the channels back into one colour picture ({}).", parts.join(", "))
}

fn key_mixer_note(g: &Grade, n: &GradeNode, edges: &[Edge], me: &str, t: Tick) -> String {
    let keys: Vec<(u8, NodeId)> = feeds(edges, n.id).into_iter().filter(|(i, _, p)| *i < MAX_MIX_INPUTS && *p == 1).map(|(i, f, _)| (i, f)).collect();
    if keys.is_empty() {
        return format!("{me} is a key mixer with no selections connected, so it selects nothing.");
    }
    let mut parts = Vec::new();
    for (k, (i, f)) in keys.iter().enumerate() {
        let outside = if n.float_at(&format!("kmix.invert.{i}"), t) >= 0.5 { "everything outside " } else { "" };
        let verb = if k == 0 {
            "starts with"
        } else {
            match n.float_at(&format!("kmix.mode.{i}"), t).round() as i64 {
                1 => "takes away",
                2 => "keeps only what overlaps",
                _ => "adds",
            }
        };
        parts.push(format!("{verb} {outside}{} selection", poss(g, *f)));
    }
    format!("{me} builds one selection: it {}.", join_and(&parts))
}

/// A correction node whose key input is connected: an outside node (inverted) or one limited by
/// another node's selection.
fn correction_note(g: &Grade, n: &GradeNode, edges: &[Edge], me: &str, t: Tick) -> Option<String> {
    let (_, from, from_port) = feeds(edges, n.id).into_iter().find(|(i, _, _)| *i == 1)?;
    let src = name(g, from);
    let what = if g.node(from).is_some_and(|x| x.kind == NodeKind::KeyMixer) {
        format!("the combined selection of {src}")
    } else if from_port == 1 {
        format!("{} selection", poss(g, from))
    } else {
        format!("the picture of {src}")
    };
    if n.float_at("key.invert", t) >= 0.5 {
        Some(format!("{me}: works on everything outside {what}."))
    } else {
        Some(format!("{me}: works only inside {what}."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::dag::Port;
    use filmcraft_project::keyframe::{Param, ParamValue};

    fn set(g: &mut Grade, id: u64, k: &str, v: f64) {
        if let Some(n) = g.node_mut(id) {
            n.params.insert(k.into(), Param::new(ParamValue::Float(v)));
        }
    }

    fn node(g: &mut Grade, id: u64, kind: NodeKind, label: &str) {
        let mut n = GradeNode::new(id);
        n.kind = kind;
        n.label = label.into();
        g.nodes.push(n);
    }

    fn e(a: u64, ap: u8, b: u64, bp: u8) -> Edge {
        Edge::new(Port::new(a, ap), Port::new(b, bp))
    }

    #[test]
    fn serial_has_no_notes() {
        let mut g = Grade::with_node();
        g.nodes.push(GradeNode::new(2));
        assert!(explain_branches(&g, Tick(0)).is_empty());
    }

    #[test]
    fn parallel_is_blended_evenly_and_by_gain() {
        let mut g = Grade::default();
        node(&mut g, 1, NodeKind::Correction, "Skin");
        node(&mut g, 2, NodeKind::Correction, "Background");
        node(&mut g, 3, NodeKind::ParallelMixer, "");
        g.edges = vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)];
        let t = explain_branches(&g, Tick(0));
        assert_eq!(t, ["Node 3: 'Skin' and 'Background' each work on the same image; their results are blended 50 / 50."]);
        set(&mut g, 3, "mix.gain.0", 1.5);
        set(&mut g, 3, "mix.gain.1", 0.5);
        assert!(explain_branches(&g, Tick(0)).iter().any(|s| s.ends_with("blended 75 / 25.")));
    }

    #[test]
    fn layer_names_mode_and_opacity() {
        let mut g = Grade::default();
        node(&mut g, 1, NodeKind::Correction, "");
        node(&mut g, 2, NodeKind::Correction, "");
        node(&mut g, 3, NodeKind::LayerMixer, "");
        g.edges = vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)];
        set(&mut g, 3, "layer.opacity.1", 0.6);
        set(&mut g, 3, "layer.mode.1", 13.0);
        assert_eq!(explain_branches(&g, Tick(0)), ["Node 3: node 2's look sits on top of node 1 at 60 % opacity, Soft Light."]);
    }

    #[test]
    fn outside_splitter_combiner_and_key_mixer() {
        let mut g = Grade::default();
        node(&mut g, 1, NodeKind::Correction, "Sky");
        node(&mut g, 2, NodeKind::Correction, "");
        node(&mut g, 3, NodeKind::Splitter, "");
        node(&mut g, 4, NodeKind::Combiner, "");
        node(&mut g, 5, NodeKind::KeyMixer, "");
        g.edges =
            vec![e(SOURCE, 0, 1, 0), e(1, 0, 2, 0), e(1, 1, 2, 1), e(2, 0, 3, 0), e(3, 0, 4, 0), e(3, 1, 4, 1), e(3, 2, 4, 2), e(1, 1, 5, 0), e(2, 1, 5, 1)];
        set(&mut g, 2, "key.invert", 1.0);
        set(&mut g, 5, "kmix.mode.1", 1.0);
        let t = explain_branches(&g, Tick(0));
        assert_eq!(t[0], "Node 2: works on everything outside the 'Sky' node's selection.");
        assert!(t[1].starts_with("Node 3 splits the picture into its red, green and blue channels") && t[1].contains("red goes to node 4"));
        assert_eq!(t[2], "Node 4 puts the channels back into one colour picture (red from node 3, green from node 3, blue from node 3).");
        assert_eq!(t[3], "Node 5 builds one selection: it starts with the 'Sky' node's selection and takes away node 2's selection.");
    }

    #[test]
    fn hostile_graphs_do_not_panic() {
        let mut g = Grade::default();
        node(&mut g, 1, NodeKind::ParallelMixer, "");
        node(&mut g, 2, NodeKind::LayerMixer, "");
        node(&mut g, 3, NodeKind::KeyMixer, "");
        g.edges = vec![e(9, 0, 1, 0), e(9, 0, 1, 200), e(1, 0, 2, 0), e(2, 0, 2, 1)];
        set(&mut g, 2, "layer.mode.1", f64::NAN);
        assert_eq!(explain_branches(&g, Tick(0)).len(), 3);
    }
}
