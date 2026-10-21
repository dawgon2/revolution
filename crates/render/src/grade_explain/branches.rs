//! Plain-language notes on a branched grade (fork C2, G2.10–11): what the parallel, layer,
//! splitter / combiner, key-mixer and outside nodes do with the pictures and selections that
//! reach them. These are read from the graph and the mixers' settings, not measured, so they are
//! exact and cost no rendering. A serial grade has none.
//!
//! Fork AI2 item 2: the notes are built from [`BranchFacts`] (serde, so they can go into the
//! explainer model's prompt), and [`check_branch_explanation`] guards a model-written note: it is
//! accepted only when it is one of the note's fixed phrasings ([`BranchFacts::phrasings`]) with
//! the real names and values. Anything else fails, and the caller shows
//! [`BranchFacts::sentence`] instead (false accepts are the serious bug, as in `check`).

use filmcraft_project::dag::{Edge, NodeId};
use filmcraft_project::effect::BLEND_MODES;
use filmcraft_project::grade::{Grade, GradeNode, MAX_MIX_INPUTS, MAX_NODES, NodeKind, SOURCE};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

use super::check::{FactMismatch, check_shape};

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
fn poss(name: &str) -> String {
    if name.starts_with('\'') { format!("the {name} node's") } else { format!("{name}'s") }
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

/// One layer of a layer mixer: `look` sits on top of `below`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerFact {
    /// The node whose look is the upper layer.
    pub look: String,
    /// The node right below it, or "the layers below" from the third layer on.
    pub below: String,
    /// Opacity in percent, 0–100.
    pub opacity: i64,
    /// The blend mode's name as the mixer shows it ("Normal", "Soft Light", …).
    pub blend: String,
}

/// How a key mixer combines one selection with the ones before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyOp {
    /// The first selection.
    Start,
    Add,
    Subtract,
    Intersect,
}

/// One selection a key mixer reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyFact {
    pub op: KeyOp,
    /// Everything outside the selection (the input is inverted).
    pub outside: bool,
    /// The node whose selection it is.
    pub from: String,
}

/// What one branching or selecting node does, read from the graph (the facts behind one note).
/// Names are the ones the notes use: `'Label'`, `node N` or `the original picture`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BranchFacts {
    /// A parallel mixer: `inputs` blended by `shares` (percent of the total gain, same order).
    /// Fewer than two inputs: it passes its picture through.
    Parallel { node: String, inputs: Vec<String>, shares: Vec<i64>, same_image: bool },
    /// A layer mixer, from the second input up. Empty: fewer than two inputs, passes through.
    Layer { node: String, layers: Vec<LayerFact> },
    /// A splitter: which nodes get its red, green and blue channel.
    Splitter { node: String, red: Vec<String>, green: Vec<String>, blue: Vec<String> },
    /// A combiner: (channel, source) for each connected channel, red / green / blue order.
    Combiner { node: String, channels: Vec<(String, String)> },
    /// A key mixer's selections in input order. Empty: no selections connected.
    KeyMixer { node: String, keys: Vec<KeyFact> },
    /// A correction node limited by another node: `what` is the picture or selection it reads.
    Limited { node: String, outside: bool, what: String },
}

/// The facts of every node that branches or selects, in node order. Empty for a serial grade.
pub fn branch_facts(g: &Grade, t: Tick) -> Vec<BranchFacts> {
    branch_facts_by_node(g, t).into_iter().map(|(_, f)| f).collect()
}

/// [`branch_facts`] with the id of the node each note is about.
pub fn branch_facts_by_node(g: &Grade, t: Tick) -> Vec<(NodeId, BranchFacts)> {
    if !g.is_graph() {
        return Vec::new();
    }
    let edges = g.effective_edges();
    let mut out = Vec::new();
    for n in g.nodes.iter().take(MAX_NODES) {
        let me = name(g, n.id);
        let f = match n.kind {
            NodeKind::Correction => limited_facts(g, n, &edges, me, t),
            NodeKind::ParallelMixer => Some(parallel_facts(g, n, &edges, me, t)),
            NodeKind::LayerMixer => Some(layer_facts(g, n, &edges, me, t)),
            NodeKind::Splitter => Some(splitter_facts(g, n, &edges, me)),
            NodeKind::Combiner => Some(combiner_facts(g, n, &edges, me)),
            NodeKind::KeyMixer => Some(key_mixer_facts(g, n, &edges, me, t)),
        };
        out.extend(f.map(|f| (n.id, f)));
    }
    out
}

/// One sentence per node that branches or selects, in node order. Empty for a serial grade.
pub fn explain_branches(g: &Grade, t: Tick) -> Vec<String> {
    branch_facts(g, t).iter().map(BranchFacts::sentence).collect()
}

fn parallel_facts(g: &Grade, n: &GradeNode, edges: &[Edge], node: String, t: Tick) -> BranchFacts {
    let ins = rgb_feeds(edges, n.id);
    if ins.len() < 2 {
        return BranchFacts::Parallel { node, inputs: ins.iter().map(|(_, f)| name(g, *f)).collect(), shares: Vec::new(), same_image: false };
    }
    let weights: Vec<f32> = ins.iter().map(|(i, _)| n.float_at(&format!("mix.gain.{i}"), t).max(0.0)).collect();
    let total: f32 = weights.iter().sum();
    let inputs: Vec<String> = ins.iter().map(|(_, f)| name(g, *f)).collect();
    let shares: Vec<i64> = weights.iter().map(|w| if total > 0.0 { percent(w / total) } else { 0 }).collect();
    // branches fed by the same picture work on the same image
    let sources: Vec<Option<NodeId>> = ins.iter().map(|(_, f)| feeds(edges, *f).into_iter().find(|(i, _, p)| *i == 0 && *p != 1).map(|(_, s, _)| s)).collect();
    let same_image = sources.first().is_some_and(|f| f.is_some() && sources.iter().all(|x| x == f));
    BranchFacts::Parallel { node, inputs, shares, same_image }
}

fn layer_facts(g: &Grade, n: &GradeNode, edges: &[Edge], node: String, t: Tick) -> BranchFacts {
    let ins = rgb_feeds(edges, n.id);
    if ins.len() < 2 {
        return BranchFacts::Layer { node, layers: Vec::new() };
    }
    let mut layers = Vec::new();
    for (k, (i, f)) in ins.iter().enumerate().skip(1) {
        let below = match ins.get(k.saturating_sub(1)) {
            Some((_, b)) if k == 1 => name(g, *b),
            _ => "the layers below".to_string(),
        };
        let opacity = percent(n.float_at(&format!("layer.opacity.{i}"), t).clamp(0.0, 1.0));
        layers.push(LayerFact { look: name(g, *f), below, opacity, blend: blend_name(n, *i, t) });
    }
    BranchFacts::Layer { node, layers }
}

fn splitter_facts(g: &Grade, n: &GradeNode, edges: &[Edge], node: String) -> BranchFacts {
    let to = |i: u8| -> Vec<String> { edges.iter().filter(|e| e.from.node == n.id && e.from.index == i).map(|e| name(g, e.to.node)).collect() };
    BranchFacts::Splitter { node, red: to(0), green: to(1), blue: to(2) }
}

fn combiner_facts(g: &Grade, n: &GradeNode, edges: &[Edge], node: String) -> BranchFacts {
    let by_port = feeds(edges, n.id);
    let mut channels = Vec::new();
    for (c, i) in ["red", "green", "blue"].iter().zip(0u8..) {
        if let Some((_, f, _)) = by_port.iter().find(|(p, _, _)| *p == i) {
            channels.push(((*c).to_string(), name(g, *f)));
        }
    }
    BranchFacts::Combiner { node, channels }
}

fn key_mixer_facts(g: &Grade, n: &GradeNode, edges: &[Edge], node: String, t: Tick) -> BranchFacts {
    let keys = feeds(edges, n.id)
        .into_iter()
        .filter(|(i, _, p)| *i < MAX_MIX_INPUTS && *p == 1)
        .enumerate()
        .map(|(k, (i, f, _))| {
            let op = if k == 0 {
                KeyOp::Start
            } else {
                match n.float_at(&format!("kmix.mode.{i}"), t).round() as i64 {
                    1 => KeyOp::Subtract,
                    2 => KeyOp::Intersect,
                    _ => KeyOp::Add,
                }
            };
            KeyFact { op, outside: n.float_at(&format!("kmix.invert.{i}"), t) >= 0.5, from: name(g, f) }
        })
        .collect();
    BranchFacts::KeyMixer { node, keys }
}

/// A correction node whose key input is connected: an outside node (inverted) or one limited by
/// another node's selection.
fn limited_facts(g: &Grade, n: &GradeNode, edges: &[Edge], node: String, t: Tick) -> Option<BranchFacts> {
    let (_, from, from_port) = feeds(edges, n.id).into_iter().find(|(i, _, _)| *i == 1)?;
    let src = name(g, from);
    let what = if g.node(from).is_some_and(|x| x.kind == NodeKind::KeyMixer) {
        format!("the combined selection of {src}")
    } else if from_port == 1 {
        format!("{} selection", poss(&src))
    } else {
        format!("the picture of {src}")
    };
    Some(BranchFacts::Limited { node, outside: n.float_at("key.invert", t) >= 0.5, what })
}

/// The fixed phrasings of each kind of note. Variant 0 is the template note
/// ([`BranchFacts::sentence`]); the training data uses all of them, the checker accepts exactly
/// these (after `normalize`).
const VARIANTS: usize = 2;

impl BranchFacts {
    /// The node the note is about.
    pub fn node(&self) -> &str {
        match self {
            Self::Parallel { node, .. }
            | Self::Layer { node, .. }
            | Self::Splitter { node, .. }
            | Self::Combiner { node, .. }
            | Self::KeyMixer { node, .. }
            | Self::Limited { node, .. } => node,
        }
    }

    /// The template note, always true (what the Explain panel shows without a model).
    pub fn sentence(&self) -> String {
        self.phrase(0)
    }

    /// Every accepted phrasing of this note, the template first.
    pub fn phrasings(&self) -> Vec<String> {
        let mut v: Vec<String> = (0..VARIANTS).map(|k| self.phrase(k)).collect();
        v.dedup();
        v
    }

    fn phrase(&self, k: usize) -> String {
        let s = match self {
            Self::Parallel { node, inputs, shares, same_image } => {
                if shares.len() < 2 {
                    match k {
                        0 => format!("{node} is a parallel mixer with fewer than two inputs, so it passes its picture through."),
                        _ => format!("{node} has fewer than two inputs, so this parallel mixer passes its picture through."),
                    }
                } else {
                    let lead = if *same_image { "each work on the same image" } else { "each give a picture" };
                    let share: Vec<String> = shares.iter().map(i64::to_string).collect();
                    match k {
                        0 => format!("{node}: {} {lead}; their results are blended {}.", join_and(inputs), share.join(" / ")),
                        _ => {
                            let each: Vec<String> = inputs.iter().zip(shares).map(|(i, s)| format!("{s} % of {i}")).collect();
                            format!("{node}: {} {lead}, and it blends {}.", join_and(inputs), join_and(&each))
                        }
                    }
                }
            }
            Self::Layer { node, layers } => {
                if layers.is_empty() {
                    match k {
                        0 => format!("{node} is a layer mixer with fewer than two inputs, so it passes its picture through."),
                        _ => format!("{node} has fewer than two inputs, so this layer mixer passes its picture through."),
                    }
                } else {
                    let parts: Vec<String> = layers
                        .iter()
                        .map(|l| match k {
                            0 => format!("{} look sits on top of {} at {} % opacity, {}", poss(&l.look), l.below, l.opacity, l.blend),
                            _ => format!("{} look is laid over {} in {} mode at {} % opacity", poss(&l.look), l.below, l.blend, l.opacity),
                        })
                        .collect();
                    format!("{node}: {}.", parts.join("; "))
                }
            }
            Self::Splitter { node, red, green, blue } => {
                let outs: Vec<String> = [("red", red), ("green", green), ("blue", blue)]
                    .iter()
                    .map(|(c, to)| if to.is_empty() { format!("{c} goes nowhere") } else { format!("{c} goes to {}", join_and(to)) })
                    .collect();
                match k {
                    0 => format!("{node} splits the picture into its red, green and blue channels, each a grey picture: {}.", outs.join("; ")),
                    _ => format!("{node} turns the picture into three grey pictures, one per channel: {}.", outs.join("; ")),
                }
            }
            Self::Combiner { node, channels } => {
                if channels.is_empty() {
                    return capitalize(&format!("{node} puts three channels back into one colour picture, but none are connected."));
                }
                let parts: Vec<String> = channels.iter().map(|(c, f)| format!("{c} from {f}")).collect();
                match k {
                    0 => format!("{node} puts the channels back into one colour picture ({}).", parts.join(", ")),
                    _ => format!("{node} rebuilds one colour picture from its channels: {}.", join_and(&parts)),
                }
            }
            Self::KeyMixer { node, keys } => {
                if keys.is_empty() {
                    return capitalize(&format!("{node} is a key mixer with no selections connected, so it selects nothing."));
                }
                let parts: Vec<String> = keys
                    .iter()
                    .map(|kf| {
                        let verb = match kf.op {
                            KeyOp::Start => "starts with",
                            KeyOp::Add => "adds",
                            KeyOp::Subtract => "takes away",
                            KeyOp::Intersect => "keeps only what overlaps",
                        };
                        let outside = if kf.outside { "everything outside " } else { "" };
                        format!("{verb} {outside}{} selection", poss(&kf.from))
                    })
                    .collect();
                match k {
                    0 => format!("{node} builds one selection: it {}.", join_and(&parts)),
                    _ => format!("{node} combines selections into one: it {}.", join_and(&parts)),
                }
            }
            Self::Limited { node, outside, what } => match (k, outside) {
                (0, true) => format!("{node}: works on everything outside {what}."),
                (0, false) => format!("{node}: works only inside {what}."),
                (_, true) => format!("{node} changes only what lies outside {what}."),
                (_, false) => format!("{node} changes only what lies inside {what}."),
            },
        };
        capitalize(&s)
    }
}

/// Lower case, one space between words, US "color" as "colour", "60%" as "60 %", no final full
/// stop: the form phrasings are compared in.
fn normalize(text: &str) -> String {
    let mut s = String::with_capacity(text.len().saturating_add(8));
    let mut prev: Option<char> = None;
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if c.is_whitespace() {
            if prev != Some(' ') {
                s.push(' ');
            }
            prev = Some(' ');
            continue;
        }
        if c == '%' && prev.is_some_and(|p| p.is_ascii_digit()) {
            s.push(' ');
        }
        s.push(c);
        prev = Some(c);
    }
    let s = s.replace("color", "colour").replace('\u{2019}', "'");
    s.trim_end_matches('.').trim().to_string()
}

/// The whole numbers written in `text` (digits only; "1e9" gives 1 and 9, both checked).
fn numbers_in(text: &str) -> Vec<i64> {
    let mut v = Vec::new();
    let mut cur = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            if cur.len() < 12 {
                cur.push(c);
            }
        } else if !cur.is_empty() {
            v.push(cur.parse::<i64>().unwrap_or(i64::MAX));
            cur.clear();
        }
    }
    v
}

/// The node names in a normalized text: quoted labels and "node N".
fn names_in(text: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('\'') {
        let after = rest.get(i.saturating_add(1)..).unwrap_or("");
        // an apostrophe of a possessive ("node 2's") is not a quote
        if after.starts_with("s ") || after.is_empty() {
            rest = after;
            continue;
        }
        match after.find('\'') {
            Some(j) => {
                v.push(after.get(..j).unwrap_or("").to_string());
                rest = after.get(j.saturating_add(1)..).unwrap_or("");
            }
            None => break,
        }
    }
    let words: Vec<&str> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    for w in words.windows(2) {
        if let [a, b] = w
            && *a == "node"
            && b.chars().all(|c| c.is_ascii_digit())
        {
            v.push(format!("node {b}"));
        }
    }
    v
}

/// Check that `text` truthfully explains the branching node of `facts`: it must be one of
/// [`BranchFacts::phrasings`] (see [`check_phrasings`]). Err gives the first reason it is not;
/// then show [`BranchFacts::sentence`]. The text is hostile (any UTF-8, any length).
pub fn check_branch_explanation(facts: &BranchFacts, text: &str) -> Result<(), FactMismatch> {
    check_phrasings(&facts.phrasings(), text)
}

/// The check for notes with a fixed set of true phrasings (branch notes, comp-graph notes):
/// `text` must equal one of `phrasings` (the template first) up to case, spacing, "color" /
/// "colour", "60%" / "60 %" and a final full stop, and pass [`check_shape`]. Err says why not: a
/// number or a node name that none of the phrasings has, else the first word that differs from
/// the template. Hostile text (any UTF-8, any length) only fails.
pub fn check_phrasings(phrasings: &[String], text: &str) -> Result<(), FactMismatch> {
    check_shape(text)?;
    let got = normalize(text);
    let accepted: Vec<String> = phrasings.iter().map(|p| normalize(p)).collect();
    if accepted.contains(&got) {
        return Ok(());
    }
    let known: Vec<i64> = phrasings.iter().flat_map(|p| numbers_in(p)).collect();
    if let Some(n) = numbers_in(&got).into_iter().find(|n| !known.contains(n)) {
        return Err(FactMismatch::Unsupported(format!("the number {n}")));
    }
    let names: Vec<String> = accepted.iter().flat_map(|p| names_in(p)).collect();
    if let Some(n) = names_in(&got).into_iter().find(|n| !names.contains(n)) {
        return Err(FactMismatch::Label(format!("'{}' is not a node this note is about", n.chars().take(40).collect::<String>())));
    }
    let want = accepted.first().cloned().unwrap_or_default();
    let (at, word) =
        got.split(' ').zip(want.split(' ').chain(std::iter::repeat(""))).enumerate().find(|(_, (a, b))| a != b).map_or((0, ""), |(i, (a, _))| (i, a));
    Err(FactMismatch::Ungrammatical(at, word.chars().take(40).collect()))
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

    /// A grade with every kind of branching node (the C2 lesson shapes in one graph).
    fn every_kind() -> Grade {
        let mut g = Grade::default();
        node(&mut g, 1, NodeKind::Correction, "Skin");
        node(&mut g, 2, NodeKind::Correction, "");
        node(&mut g, 3, NodeKind::ParallelMixer, "");
        node(&mut g, 4, NodeKind::LayerMixer, "Look");
        node(&mut g, 5, NodeKind::KeyMixer, "");
        node(&mut g, 6, NodeKind::Correction, "");
        g.edges = vec![
            e(SOURCE, 0, 1, 0),
            e(SOURCE, 0, 2, 0),
            e(1, 0, 3, 0),
            e(2, 0, 3, 1),
            e(3, 0, 4, 0),
            e(2, 0, 4, 1),
            e(1, 1, 5, 0),
            e(2, 1, 5, 1),
            e(4, 0, 6, 0),
            e(5, 0, 6, 1),
        ];
        set(&mut g, 3, "mix.gain.0", 1.5);
        set(&mut g, 3, "mix.gain.1", 0.5);
        set(&mut g, 4, "layer.opacity.1", 0.4);
        set(&mut g, 4, "layer.mode.1", 13.0);
        set(&mut g, 5, "kmix.mode.1", 2.0);
        set(&mut g, 6, "key.invert", 1.0);
        g
    }

    #[test]
    fn notes_are_built_from_the_facts() {
        let g = every_kind();
        let facts = branch_facts(&g, Tick(0));
        assert_eq!(facts.iter().map(BranchFacts::sentence).collect::<Vec<_>>(), explain_branches(&g, Tick(0)));
        assert!(matches!(&facts[0], BranchFacts::Parallel { shares, same_image: true, .. } if shares == &[75, 25]));
        assert!(matches!(&facts[1], BranchFacts::Layer { layers, .. } if layers[0].opacity == 40 && layers[0].blend == "Soft Light"));
        assert!(matches!(&facts[2], BranchFacts::KeyMixer { keys, .. } if keys[1].op == KeyOp::Intersect));
        assert!(matches!(&facts[3], BranchFacts::Limited { outside: true, .. }));
        // the facts go into the model's prompt as JSON and come back unchanged
        let json = serde_json::to_string(&facts).unwrap_or_default();
        assert!(json.contains("\"kind\":\"parallel\""));
        assert_eq!(serde_json::from_str::<Vec<BranchFacts>>(&json).ok(), Some(facts));
    }

    #[test]
    fn every_phrasing_passes_the_checker() {
        let g = every_kind();
        for f in branch_facts(&g, Tick(0)) {
            let p = f.phrasings();
            assert!(p.len() >= 2, "{p:?}");
            for s in &p {
                assert_eq!(check_branch_explanation(&f, s), Ok(()), "{s}");
                assert_eq!(check_branch_explanation(&f, &s.to_uppercase().replace("COLOUR", "COLOR")), Ok(()), "{s}");
                assert_eq!(check_branch_explanation(&f, s.trim_end_matches('.')), Ok(()), "{s}");
            }
        }
    }

    #[test]
    fn invented_values_and_nodes_are_rejected() {
        let g = every_kind();
        let facts = branch_facts(&g, Tick(0));
        let par = &facts[0];
        let s = par.sentence();
        assert!(s.ends_with("blended 75 / 25."), "{s}");
        assert!(matches!(check_branch_explanation(par, &s.replace("75 / 25", "60 / 40")), Err(FactMismatch::Unsupported(_))));
        // swapped shares use only real numbers but say the wrong thing
        assert!(check_branch_explanation(par, &s.replace("75 / 25", "25 / 75")).is_err());
        assert!(matches!(check_branch_explanation(par, &s.replace("'Skin'", "'Sky'")), Err(FactMismatch::Label(_))));
        let layer = &facts[1];
        let s = layer.sentence();
        assert!(check_branch_explanation(layer, &s.replace("Soft Light", "Multiply")).is_err());
        assert!(check_branch_explanation(layer, &s.replace("40 %", "41 %")).is_err());
        let key = &facts[2];
        assert!(check_branch_explanation(key, &key.sentence().replace("keeps only what overlaps", "adds")).is_err());
        let lim = &facts[3];
        assert!(check_branch_explanation(lim, &lim.sentence().replace("outside", "inside")).is_err());
        // a true note with an extra claim
        assert!(check_branch_explanation(lim, &format!("{} It also warms the skin.", lim.sentence())).is_err());
    }

    #[test]
    fn hostile_text_is_rejected_without_panicking() {
        let g = every_kind();
        let facts = branch_facts(&g, Tick(0));
        let long = "a".repeat(1 << 20);
        let digits = "9".repeat(5000);
        for f in &facts {
            for t in ["", " ", "NaN", "1e999 %", "½ / ½", "''''", "'", "node", "node 99999999999999999999", "\u{0}\u{202e}", long.as_str(), digits.as_str()] {
                assert!(check_branch_explanation(f, t).is_err(), "{t:.40}");
            }
        }
    }

    #[test]
    fn passthrough_and_empty_mixers_have_true_phrasings() {
        let mut g = Grade::default();
        node(&mut g, 1, NodeKind::ParallelMixer, "");
        node(&mut g, 2, NodeKind::LayerMixer, "");
        node(&mut g, 3, NodeKind::KeyMixer, "");
        node(&mut g, 4, NodeKind::Combiner, "");
        node(&mut g, 5, NodeKind::Splitter, "");
        g.edges = vec![e(SOURCE, 0, 1, 0), e(1, 0, 2, 0), e(2, 0, 5, 0)];
        for f in branch_facts(&g, Tick(0)) {
            for s in f.phrasings() {
                assert_eq!(check_branch_explanation(&f, &s), Ok(()), "{s}");
            }
        }
    }
}
