//! Saved comp templates (fork C4.4, ADR 0009 §3): a piece of a comp's node graph kept as a file
//! (`<data dir>/templates/comp/<name>.fctemplate`) and inserted into any comp later.
//!
//! A template holds `{ version, name, nodes, edges }`: node ids renumbered from 1, only the
//! connections between the saved nodes, and no project item references (Media In nodes that read
//! a project item are dropped with their connections; Media Out is never saved). A template file
//! is hostile input: [`CompTemplate::parse`] caps its size, checks the version, refuses damaged
//! graphs and brings the nodes within the same caps as a comp from a project file
//! ([`Comp::sanitize`]).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::comp::{Comp, CompKind, CompNode, MAX_GROUP_DEPTH, MediaInSource};
use crate::dag::{self, DagError, Edge, NodeId, Port};

/// The template format version this build writes and reads.
pub const VERSION: u32 = 1;
/// Largest template file read.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Longest template name in characters.
pub const MAX_NAME: usize = 64;
/// The template file extension.
pub const EXTENSION: &str = "fctemplate";

/// A saved piece of a comp graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompTemplate {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<CompNode>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

/// Why a template can't be made, read or inserted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("select at least one node to save as a template")]
    NoSelection,
    #[error("the comp has no node {0}")]
    UnknownNode(NodeId),
    #[error("nothing to save: Media In nodes that read project items and Media Out are not kept in templates")]
    NothingLeft,
    #[error("the template file is too large ({0} bytes; at most 4 MiB)")]
    TooLarge(usize),
    #[error("the template was made by a newer version of FilmCraft (format {0}; this build reads up to {VERSION})")]
    NewerVersion(u32),
    #[error("the template file has an unknown format version ({0})")]
    BadVersion(u32),
    #[error("the template file can't be read: {0}")]
    Parse(String),
    #[error("the template has {0} nodes (at most {max})", max = dag::MAX_NODES)]
    TooManyNodes(usize),
    #[error("the template has {0} connections (at most {max})", max = dag::MAX_EDGES)]
    TooManyEdges(usize),
    #[error("the template's graph is damaged: {0}")]
    BadGraph(String),
    #[error("the template has a node that refers to a project item or a Media Out node, which templates can't hold")]
    ItemReference,
    #[error("the template has no nodes")]
    Empty,
    #[error("inserting {adding} nodes would give the comp {total} (at most {max})", max = dag::MAX_NODES)]
    CompFull { adding: usize, total: usize },
    #[error("inserting the template's connections would give the comp {total} (at most {max})", max = dag::MAX_EDGES)]
    CompEdgesFull { total: usize },
    #[error("the insert position must be two finite numbers")]
    BadPosition,
    #[error("the comp has no free node ids left")]
    NoIds,
}

/// A template made from a selection, and what was left out of it.
#[derive(Clone, Debug, PartialEq)]
pub struct FromSelection {
    pub template: CompTemplate,
    /// Media In nodes that read a project item (dropped with their connections).
    pub dropped_media_in: usize,
    /// Media Out nodes (never saved).
    pub dropped_media_out: usize,
    /// Expression references that couldn't be rewritten, in words.
    pub warnings: Vec<String>,
}

/// A template added to a comp: the new top-level ids and what the reference rewrite warns about.
#[derive(Clone, Debug, PartialEq)]
pub struct Inserted {
    pub nodes: Vec<NodeId>,
    pub warnings: Vec<String>,
}

/// Does this node refer to the project (a Media In reading an item) or is it the comp's output?
fn is_item_media_in(n: &CompNode) -> bool {
    matches!(n.kind, CompKind::MediaIn { source: MediaInSource::Item { .. }, .. })
}

/// How many nodes `nodes` hold, groups' inner nodes included (at most [`MAX_GROUP_DEPTH`] levels
/// down, as [`Comp::all_nodes`] counts them).
fn count_all(nodes: &[CompNode]) -> usize {
    fn walk(nodes: &[CompNode], depth: usize) -> usize {
        nodes
            .iter()
            .map(|n| match &n.kind {
                CompKind::Group { comp, .. } if depth < MAX_GROUP_DEPTH => walk(&comp.nodes, depth.saturating_add(1)).saturating_add(1),
                _ => 1,
            })
            .fold(0, usize::saturating_add)
    }
    walk(nodes, 0)
}

/// Does any node, inside groups too, read a project item?
fn has_item_ref(nodes: &[CompNode]) -> bool {
    fn walk(nodes: &[CompNode], depth: usize) -> bool {
        nodes.iter().any(|n| match &n.kind {
            CompKind::Group { comp, .. } => depth < MAX_GROUP_DEPTH && walk(&comp.nodes, depth.saturating_add(1)),
            _ => is_item_media_in(n),
        })
    }
    walk(nodes, 0)
}

/// Removes the Media In nodes that read a project item from inside group node `n` (nested groups
/// too): their inner connections, the group's output if it was one, and the exposed parameters
/// (and the group's values for them) that pointed at them. Returns how many were removed.
fn strip_inner_items(n: &mut CompNode, depth: usize) -> usize {
    let CompKind::Group { comp, exposed } = &mut n.kind else { return 0 };
    if depth >= MAX_GROUP_DEPTH {
        return 0;
    }
    let nested = comp.nodes.iter_mut().map(|x| strip_inner_items(x, depth.saturating_add(1))).fold(0, usize::saturating_add);
    let gone: BTreeSet<NodeId> = comp.nodes.iter().filter(|x| is_item_media_in(x)).map(|x| x.id).collect();
    if !gone.is_empty() {
        comp.nodes.retain(|x| !gone.contains(&x.id));
        comp.edges.retain(|e| !gone.contains(&e.from.node) && !gone.contains(&e.to.node));
        if comp.output.is_some_and(|o| gone.contains(&o)) {
            comp.output = None;
        }
        exposed.retain(|e| !gone.contains(&e.node));
        let ids: BTreeSet<&str> = exposed.iter().map(|e| e.id.as_str()).collect();
        n.params.retain(|k, _| ids.contains(k.as_str()));
    }
    nested.saturating_add(gone.len())
}

/// A template name within the caps: control characters removed, whitespace trimmed, at most
/// [`MAX_NAME`] characters. (File names are made safe separately, by the engine.)
pub fn clean_name(name: &str) -> String {
    let s: String = name.chars().filter(|c| !c.is_control()).collect();
    s.trim().chars().take(MAX_NAME).collect::<String>().trim().to_string()
}

impl CompTemplate {
    /// The nodes `selection` of `comp` (and the connections between them) as a template named
    /// `name`. Ids are renumbered from 1 in the comp's node order; Media In nodes reading a
    /// project item and Media Out nodes are dropped with their connections (counted in the result).
    pub fn from_selection(comp: &Comp, selection: &[NodeId], name: &str) -> Result<FromSelection, TemplateError> {
        if selection.is_empty() {
            return Err(TemplateError::NoSelection);
        }
        let wanted: BTreeSet<NodeId> = selection.iter().copied().collect();
        for id in &wanted {
            if comp.node(*id).is_none() {
                return Err(TemplateError::UnknownNode(*id));
            }
        }
        let (mut dropped_media_in, mut dropped_media_out) = (0usize, 0usize);
        let mut map: BTreeMap<NodeId, NodeId> = BTreeMap::new();
        let mut nodes = Vec::new();
        for n in comp.nodes.iter().filter(|n| wanted.contains(&n.id)) {
            if is_item_media_in(n) {
                dropped_media_in = dropped_media_in.saturating_add(1);
                continue;
            }
            if matches!(n.kind, CompKind::MediaOut) {
                dropped_media_out = dropped_media_out.saturating_add(1);
                continue;
            }
            let id = NodeId::try_from(nodes.len()).map_err(|_| TemplateError::NoIds)?.saturating_add(1);
            map.insert(n.id, id);
            let mut copy = CompNode { id, ..n.clone() };
            // groups' inner ids are their own (scoped to the group), so only the top level is
            // renumbered; their insides lose what reads project items
            dropped_media_in = dropped_media_in.saturating_add(strip_inner_items(&mut copy, 0));
            nodes.push(copy);
        }
        if nodes.is_empty() {
            return Err(TemplateError::NothingLeft);
        }
        let kept = count_all(&nodes);
        if kept > dag::MAX_NODES {
            return Err(TemplateError::TooManyNodes(kept));
        }
        // references resolve as they did in this comp: kept for copied nodes by label, renumbered
        // by id, and anything outside the selection made unresolvable
        let mut warnings = Vec::new();
        remap_expressions(&mut nodes, &mut warnings, &mut |r| match comp.find_node(r) {
            Some(old) => match map.get(&old) {
                Some(_) if comp.node(old).is_some_and(|n| !n.label.is_empty() && n.label.eq_ignore_ascii_case(r.trim())) => None,
                Some(new) => Some(new.to_string()),
                None => Some(format!("{OUTSIDE_PREFIX}{old}")),
            },
            None => None,
        });
        let edges: Vec<Edge> = comp
            .edges
            .iter()
            .filter_map(|e| {
                let from = *map.get(&e.from.node)?;
                let to = *map.get(&e.to.node)?;
                Some(Edge::new(Port::new(from, e.from.index), Port::new(to, e.to.index)))
            })
            .take(dag::MAX_EDGES)
            .collect();
        let template = CompTemplate { version: VERSION, name: clean_name(name), nodes, edges };
        Ok(FromSelection { template, dropped_media_in, dropped_media_out, warnings })
    }

    /// The template as file bytes (pretty JSON).
    pub fn to_bytes(&self) -> Result<Vec<u8>, TemplateError> {
        serde_json::to_vec_pretty(self).map_err(|e| TemplateError::Parse(e.to_string()))
    }

    /// Reads a template file (hostile): at most [`MAX_BYTES`], a known version, at most
    /// [`dag::MAX_NODES`] nodes / [`dag::MAX_EDGES`] connections, a valid graph (unique ids, known
    /// nodes and ports, one connection per input, no cycle, not too deep), no project item
    /// references or Media Out; then brought within the comp caps (labels, parameters, counts).
    pub fn parse(bytes: &[u8]) -> Result<CompTemplate, TemplateError> {
        if bytes.len() > MAX_BYTES {
            return Err(TemplateError::TooLarge(bytes.len()));
        }
        // (serde_json bounds nesting depth itself, so deeply nested input is an error, not a stack
        // overflow)
        let mut t: CompTemplate = serde_json::from_slice(bytes).map_err(|e| TemplateError::Parse(e.to_string()))?;
        if t.version > VERSION {
            return Err(TemplateError::NewerVersion(t.version));
        }
        if t.version == 0 {
            return Err(TemplateError::BadVersion(t.version));
        }
        let all = count_all(&t.nodes);
        if all > dag::MAX_NODES {
            return Err(TemplateError::TooManyNodes(all));
        }
        if t.edges.len() > dag::MAX_EDGES {
            return Err(TemplateError::TooManyEdges(t.edges.len()));
        }
        let first = t.nodes.first().map(|n| n.id).ok_or(TemplateError::Empty)?;
        // (a group's inner Media Out is its output, so only the top level refuses one)
        if has_item_ref(&t.nodes) || t.nodes.iter().any(|n| matches!(n.kind, CompKind::MediaOut)) {
            return Err(TemplateError::ItemReference);
        }
        match dag::validate(&t.nodes, &t.edges, first) {
            Ok(_) | Err(DagError::NoOutput) => {}
            Err(e) => return Err(TemplateError::BadGraph(e.to_string())),
        }
        // the same caps as a comp from a project file
        let mut tmp =
            Comp { nodes: std::mem::take(&mut t.nodes), edges: std::mem::take(&mut t.edges), ..Comp::new(1, 1, Default::default(), Default::default()) };
        tmp.sanitize();
        t.nodes = tmp.nodes;
        for n in &mut t.nodes {
            n.pos = n.pos.map(clamp_pos);
        }
        t.edges = tmp.edges;
        t.name = clean_name(&t.name);
        Ok(t)
    }

    /// Adds the template's nodes and connections to `comp` with fresh ids (after the comp's
    /// highest), placed so the template's top-left node position lands at `at`. Returns the new
    /// ids in the template's node order. Refused (the comp unchanged) when the comp would have more
    /// than [`dag::MAX_NODES`] nodes (groups' inner nodes included) or [`dag::MAX_EDGES`]
    /// connections. Only top-level ids are renumbered: a group's inner ids are scoped to its own
    /// inner comp. Call [`Comp::sanitize`] afterwards (as every comp edit does) so inserted groups
    /// take the comp's size and rate.
    pub fn insert_into(&self, comp: &mut Comp, at: [f32; 2]) -> Result<Vec<NodeId>, TemplateError> {
        self.insert(comp, at).map(|i| i.nodes)
    }

    /// [`CompTemplate::insert_into`] with the reference rewrite's warnings: an id reference whose
    /// new id is also a label in `comp` (a label wins, so it would read that node instead), and
    /// expressions too long to rewrite (kept as they were).
    pub fn insert(&self, comp: &mut Comp, at: [f32; 2]) -> Result<Inserted, TemplateError> {
        if !at.iter().all(|v| v.is_finite()) {
            return Err(TemplateError::BadPosition);
        }
        if self.nodes.is_empty() {
            return Err(TemplateError::Empty);
        }
        // every level counts toward the cap (groups' inner nodes too)
        let adding = count_all(&self.nodes);
        let total = count_all(&comp.nodes).saturating_add(adding);
        if total > dag::MAX_NODES {
            return Err(TemplateError::CompFull { adding, total });
        }
        let total_edges = comp.edges.len().saturating_add(self.edges.len());
        if total_edges > dag::MAX_EDGES {
            return Err(TemplateError::CompEdgesFull { total: total_edges });
        }
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let min_x = self.nodes.iter().map(|n| finite(n.pos[0])).fold(f32::INFINITY, f32::min);
        let min_y = self.nodes.iter().map(|n| finite(n.pos[1])).fold(f32::INFINITY, f32::min);
        let base = comp.next_id();
        let mut map: BTreeMap<NodeId, NodeId> = BTreeMap::new();
        let mut added = Vec::with_capacity(self.nodes.len());
        let mut new_nodes = Vec::with_capacity(self.nodes.len());
        for (i, n) in self.nodes.iter().enumerate() {
            let id = NodeId::try_from(i).ok().and_then(|i| base.checked_add(i)).filter(|&id| id != NodeId::MAX).ok_or(TemplateError::NoIds)?;
            if map.insert(n.id, id).is_some() {
                return Err(TemplateError::BadGraph(DagError::DuplicateNode(n.id).to_string()));
            }
            let pos = [clamp_pos(at[0] + (finite(n.pos[0]) - min_x)), clamp_pos(at[1] + (finite(n.pos[1]) - min_y))];
            new_nodes.push(CompNode { id, pos, ..n.clone() });
            added.push(id);
        }
        let mut new_edges = Vec::with_capacity(self.edges.len());
        for e in &self.edges {
            let from = *map.get(&e.from.node).ok_or_else(|| TemplateError::BadGraph(DagError::UnknownNode(e.from.node).to_string()))?;
            let to = *map.get(&e.to.node).ok_or_else(|| TemplateError::BadGraph(DagError::UnknownNode(e.to.node).to_string()))?;
            new_edges.push(Edge::new(Port::new(from, e.from.index), Port::new(to, e.to.index)));
        }
        // label or id: decided by the template's own nodes, as the template was saved
        let own = Comp { nodes: self.nodes.clone(), ..Comp::new(1, 1, Default::default(), Default::default()) };
        let target_labels = label_set(&comp.nodes);
        let mut warnings = Vec::new();
        let mut clashes = BTreeSet::new();
        remap_expressions(&mut new_nodes, &mut warnings, &mut |r| {
            let old = own.find_node(r)?;
            if own.node(old).is_some_and(|n| !n.label.is_empty() && n.label.eq_ignore_ascii_case(r.trim())) {
                return None;
            }
            let new = map.get(&old)?.to_string();
            if target_labels.contains(&new.to_lowercase()) {
                clashes.insert(new.clone());
            }
            Some(new)
        });
        for id in clashes {
            warnings.push(format!("a node in this comp is labelled \u{201c}{id}\u{201d}, so expressions that refer to node {id} read that node instead; rename it or the reference"));
        }
        comp.nodes.extend(new_nodes);
        comp.edges.extend(new_edges);
        Ok(Inserted { nodes: added, warnings })
    }
}

/// The node labels, lower case (an expression's `node("…")` names a label before an id).
fn label_set<'a>(nodes: impl IntoIterator<Item = &'a CompNode>) -> BTreeSet<String> {
    nodes.into_iter().filter(|n| !n.label.is_empty()).map(|n| n.label.to_lowercase()).collect()
}

/// What a saved template's `node("<id>")` reference to a node outside the selection becomes: a
/// name no node has, so it reports "no node called `outside:<id>`" instead of reading whatever
/// node gets that id in another comp.
pub const OUTSIDE_PREFIX: &str = "outside:";

/// Rewrites the `node(…)` references of the nodes' parameter expressions: `decide` gets each
/// reference (its label or id text) and returns what it becomes (None: unchanged). The whole
/// argument token is replaced by a double-quoted string, so `node(3)` and `node('3')` both work.
/// A source that doesn't parse is left; one that would grow past [`crate::expr::MAX_SOURCE`] is
/// kept as it was, with a warning.
fn remap_expressions(nodes: &mut [CompNode], warnings: &mut Vec<String>, decide: &mut dyn FnMut(&str) -> Option<String>) {
    for n in nodes {
        let name = n.display_name();
        for (key, p) in n.params.iter_mut() {
            let Some(src) = &p.expression else { continue };
            let out = remap_expression(src, decide);
            if out.len() > crate::expr::MAX_SOURCE {
                warnings
                    .push(format!("the expression of {name} \u{203a} {key} would be too long after renumbering its node references; it was kept as it was"));
                continue;
            }
            p.expression = Some(out);
        }
    }
}

fn remap_expression(src: &str, decide: &mut dyn FnMut(&str) -> Option<String>) -> String {
    use crate::expr::ExprKind;
    let Ok(e) = crate::expr::parse(src) else { return src.to_string() };
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    e.any(&mut |x| {
        if let ExprKind::Param { node: Some(r), node_span, .. } = &x.kind
            && let Some(new) = decide(r)
            // (replacements are ids or `outside:<id>`: no quotes inside)
            && !new.contains(['"', '\\'])
        {
            edits.push((node_span.start, node_span.end, format!("\"{new}\"")));
        }
        false
    });
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = src.to_string();
    for (start, end, new) in edits {
        if start <= end && end <= out.len() && out.is_char_boundary(start) && out.is_char_boundary(end) {
            out.replace_range(start..end, &new);
        }
    }
    out
}

/// Canvas positions stay finite and within a sane range.
fn clamp_pos(v: f32) -> f32 {
    if v.is_finite() { v.clamp(-1.0e7, 1.0e7) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ItemId;
    use filmcraft_time::{FrameRate, Tick};

    fn edge(from: NodeId, to: NodeId, port: u8) -> Edge {
        Edge::new(Port::new(from, 0), Port::new(to, port))
    }

    /// Out(1) ← Color(6) ← Merge(5) ← [Solid(4), Key(3) ← MediaIn item(2)]; Layer MediaIn(7) → Blur(8).
    fn sample() -> Comp {
        let mut c = Comp::new(1920, 1080, FrameRate { num: 24, den: 1 }, Tick(1_000));
        c.nodes.push(CompNode::new(2, CompKind::MediaIn { source: MediaInSource::Item { item: ItemId(7) }, offset: Tick::ZERO }));
        c.nodes.push(CompNode { pos: [100.0, 50.0], ..CompNode::new(3, CompKind::from_key("key").unwrap()) });
        c.nodes.push(CompNode { pos: [100.0, 200.0], ..CompNode::new(4, CompKind::Solid) });
        c.nodes.push(CompNode { pos: [300.0, 120.0], label: "mix".into(), ..CompNode::new(5, CompKind::Merge) });
        c.nodes.push(CompNode { pos: [500.0, 120.0], ..CompNode::new(6, CompKind::from_key("color").unwrap()) });
        c.nodes.push(CompNode::new(7, CompKind::MediaIn { source: MediaInSource::Layer { track: 2 }, offset: Tick::ZERO }));
        c.nodes.push(CompNode::new(8, CompKind::Blur));
        c.edges = vec![edge(2, 3, 0), edge(4, 5, 0), edge(3, 5, 1), edge(5, 6, 0), edge(6, 1, 0), edge(7, 8, 0)];
        c
    }

    #[test]
    fn selection_renumbers_and_keeps_internal_edges_only() {
        let c = sample();
        let r = CompTemplate::from_selection(&c, &[6, 5, 4, 3, 2, 1, 5], "  My\u{7}  Look ").unwrap();
        assert_eq!((r.dropped_media_in, r.dropped_media_out), (1, 1));
        let t = r.template;
        assert_eq!(t.version, VERSION);
        assert_eq!(t.name, "My  Look");
        // comp order 3, 4, 5, 6 → 1, 2, 3, 4
        assert_eq!(t.nodes.iter().map(|n| (n.id, n.kind.key())).collect::<Vec<_>>(), vec![(1, "key"), (2, "solid"), (3, "merge"), (4, "color")]);
        assert_eq!(t.nodes[2].label, "mix");
        // MediaIn→Key and Color→Out are gone; the rest is renumbered
        assert_eq!(t.edges, vec![edge(2, 3, 0), edge(1, 3, 1), edge(3, 4, 0)]);
        // layer Media In nodes are kept (they don't refer to project items)
        let t = CompTemplate::from_selection(&c, &[7, 8], "l").unwrap().template;
        assert_eq!(t.edges, vec![edge(1, 2, 0)]);
    }

    #[test]
    fn selection_refusals() {
        let c = sample();
        assert_eq!(CompTemplate::from_selection(&c, &[], "x"), Err(TemplateError::NoSelection));
        assert_eq!(CompTemplate::from_selection(&c, &[3, 99], "x"), Err(TemplateError::UnknownNode(99)));
        assert_eq!(CompTemplate::from_selection(&c, &[1, 2], "x"), Err(TemplateError::NothingLeft));
    }

    #[test]
    fn round_trip_and_insert() {
        let c = sample();
        let t = CompTemplate::from_selection(&c, &[3, 4, 5, 6], "Look").unwrap().template;
        let back = CompTemplate::parse(&t.to_bytes().unwrap()).unwrap();
        assert_eq!(back, t);
        let mut target = sample();
        let ids = back.insert_into(&mut target, [1000.0, -20.0]).unwrap();
        assert_eq!(ids, vec![9, 10, 11, 12]);
        let n = |id| target.node(id).unwrap();
        // top-left (100, 50) lands at (1000, -20)
        assert_eq!(n(9).pos, [1000.0, -20.0]);
        assert_eq!(n(10).pos, [1000.0, 130.0]);
        assert_eq!(n(11).pos, [1200.0, 50.0]);
        assert_eq!(n(11).label, "mix");
        assert!(target.edges.contains(&edge(10, 11, 0)) && target.edges.contains(&edge(9, 11, 1)) && target.edges.contains(&edge(11, 12, 0)));
        // still a valid graph; wire the inserted Color to the output and it evaluates like the original
        target.edges.retain(|e| e.to.node != 1);
        target.edges.push(edge(12, 1, 0));
        let order: Vec<NodeId> = target.validate().unwrap().nodes().collect();
        assert_eq!(order, vec![9, 10, 11, 12, 1]);
    }

    #[test]
    fn insert_respects_the_node_cap() {
        let c = sample();
        let t = CompTemplate::from_selection(&c, &[3, 4, 5, 6], "Look").unwrap().template;
        let mut full = sample();
        let mut next = full.next_id();
        while full.nodes.len() < dag::MAX_NODES - 2 {
            full.nodes.push(CompNode::new(next, CompKind::Solid));
            next += 1;
        }
        let before = full.clone();
        assert!(matches!(t.insert_into(&mut full, [0.0, 0.0]), Err(TemplateError::CompFull { .. })));
        assert_eq!(full, before);
        assert_eq!(t.insert_into(&mut full, [f32::NAN, 0.0]), Err(TemplateError::BadPosition));
        // ids near the top of the range: refused, not wrapped
        let mut high = sample();
        high.nodes.push(CompNode::new(NodeId::MAX - 2, CompKind::Solid));
        assert_eq!(t.insert_into(&mut high, [0.0, 0.0]), Err(TemplateError::NoIds));
    }

    #[test]
    fn hostile_files_are_refused_without_panicking() {
        // too large
        let big = vec![b' '; MAX_BYTES + 1024 * 1024];
        assert!(matches!(CompTemplate::parse(&big), Err(TemplateError::TooLarge(_))));
        // versions
        assert_eq!(CompTemplate::parse(br#"{"version":2,"name":"x","nodes":[{"id":1,"kind":{"type":"blur"}}]}"#), Err(TemplateError::NewerVersion(2)));
        assert_eq!(CompTemplate::parse(br#"{"version":0,"name":"x","nodes":[{"id":1,"kind":{"type":"blur"}}]}"#), Err(TemplateError::BadVersion(0)));
        // 100k nodes (fits in 4 MiB)
        let mut s = String::from(r#"{"version":1,"name":"x","nodes":["#);
        for i in 0..100_000 {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(r#"{{"id":{i},"kind":{{"type":"blur"}}}}"#));
        }
        s.push_str("]}");
        assert!(s.len() <= MAX_BYTES);
        assert_eq!(CompTemplate::parse(s.as_bytes()), Err(TemplateError::TooManyNodes(100_000)));
        // bad edges
        for json in [
            r#"{"version":1,"name":"x","nodes":[{"id":1,"kind":{"type":"blur"}}],"edges":[{"from":{"node":1},"to":{"node":9}}]}"#,
            r#"{"version":1,"name":"x","nodes":[{"id":1,"kind":{"type":"blur"}},{"id":2,"kind":{"type":"blur"}}],"edges":[{"from":{"node":1},"to":{"node":2,"index":7}}]}"#,
            r#"{"version":1,"name":"x","nodes":[{"id":1,"kind":{"type":"blur"}},{"id":2,"kind":{"type":"blur"}}],"edges":[{"from":{"node":1},"to":{"node":2}},{"from":{"node":2},"to":{"node":1}}]}"#,
            r#"{"version":1,"name":"x","nodes":[{"id":1,"kind":{"type":"blur"}},{"id":1,"kind":{"type":"solid"}}]}"#,
        ] {
            assert!(matches!(CompTemplate::parse(json.as_bytes()), Err(TemplateError::BadGraph(_))), "{json}");
        }
        // item references, Media Out, empty
        assert_eq!(
            CompTemplate::parse(br#"{"version":1,"name":"x","nodes":[{"id":1,"kind":{"type":"mediaIn","source":{"kind":"item","item":3}}}]}"#),
            Err(TemplateError::ItemReference)
        );
        assert_eq!(CompTemplate::parse(br#"{"version":1,"name":"x","nodes":[{"id":1,"kind":{"type":"mediaOut"}}]}"#), Err(TemplateError::ItemReference));
        assert_eq!(CompTemplate::parse(br#"{"version":1,"name":"x"}"#), Err(TemplateError::Empty));
        // garbage and deeply nested JSON
        for bytes in [&b"\xff\xfe garbage"[..], b"", b"null", b"[1,2,3]", br#"{"version":"one"}"#] {
            assert!(matches!(CompTemplate::parse(bytes), Err(TemplateError::Parse(_))));
        }
        let deep = format!("{}{}", "[".repeat(200_000), "]".repeat(200_000));
        assert!(matches!(CompTemplate::parse(deep.as_bytes()), Err(TemplateError::Parse(_))));
        let deep =
            format!(r#"{{"version":1,"name":"x","nodes":[{{"id":1,"kind":{{"type":"blur"}},"label":{}1{}}}]}}"#, "[".repeat(100_000), "]".repeat(100_000));
        assert!(matches!(CompTemplate::parse(deep.as_bytes()), Err(TemplateError::Parse(_))));
    }

    /// C4.3 groups: an inner Media In that reads an item is stripped (and counted), a file with
    /// one inside a group is refused, and inner nodes count toward the caps.
    #[test]
    fn groups_lose_inner_item_references_and_count_toward_the_caps() {
        let mut c = sample();
        let g = crate::comp::group::group_nodes(&mut c, &[2, 3], "Keyed").unwrap();
        let group = c.node(g).unwrap().clone();
        let CompKind::Group { comp: inner, .. } = &group.kind else { panic!("not a group") };
        assert!(has_item_ref(&inner.nodes));
        let inner_count = inner.nodes.len();
        // a file holding the group as it is: refused
        let raw = CompTemplate { version: VERSION, name: "raw".into(), nodes: vec![CompNode { id: 1, ..group.clone() }], edges: Vec::new() };
        assert_eq!(CompTemplate::parse(&raw.to_bytes().unwrap()), Err(TemplateError::ItemReference));
        // saved from the comp: stripped with its inner connections
        let r = CompTemplate::from_selection(&c, &[g], "Keyed").unwrap();
        assert_eq!(r.dropped_media_in, 1);
        let CompKind::Group { comp: saved, .. } = &r.template.nodes[0].kind else { panic!("not a group") };
        assert!(!has_item_ref(&saved.nodes));
        assert_eq!(saved.nodes.len(), inner_count - 1);
        assert!(saved.edges.iter().all(|e| saved.node(e.from.node).is_some() && saved.node(e.to.node).is_some()));
        assert_eq!(count_all(&r.template.nodes), inner_count);
        let back = CompTemplate::parse(&r.template.to_bytes().unwrap()).unwrap();
        assert_eq!(count_all(&back.nodes), inner_count);
        // inner ids stay as they are; only the group node takes a fresh top-level id
        let mut target = sample();
        let ids = back.insert_into(&mut target, [0.0, 0.0]).unwrap();
        target.sanitize();
        let CompKind::Group { comp: placed, .. } = &target.node(ids[0]).unwrap().kind else { panic!("not a group") };
        assert_eq!(placed.nodes.iter().map(|n| n.id).collect::<Vec<_>>(), saved.nodes.iter().map(|n| n.id).collect::<Vec<_>>());
        assert_eq!((placed.width, placed.height), (target.width, target.height));
        // the cap counts inner nodes: a comp with room for the group node alone is refused
        let mut full = sample();
        let mut next = full.next_id();
        while count_all(&full.nodes) < dag::MAX_NODES - 1 {
            full.nodes.push(CompNode::new(next, CompKind::Solid));
            next += 1;
        }
        assert!(matches!(back.insert_into(&mut full, [0.0, 0.0]), Err(TemplateError::CompFull { .. })));
    }

    /// C4.2 expressions survive save, file and insert, and `node("<id>")` follows the renumbering.
    #[test]
    fn expressions_round_trip_and_follow_renumbered_ids() {
        use crate::keyframe::{Param, ParamValue};
        let mut c = sample();
        c.node_mut(7).unwrap().label = "Plate".into();
        let src = r#"node(3).x + node('mix').opacity + node( "2" ).y + self.opacity + node("6").x + node("plate").x"#;
        let param = Param { expression: Some(src.into()), ..Param::new(ParamValue::Float(50.0)) };
        c.node_mut(5).unwrap().params.insert("opacity".into(), param);
        let r = CompTemplate::from_selection(&c, &[3, 4, 5, 6], "Expr").unwrap();
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let t = r.template;
        let saved = t.nodes[2].params["opacity"].expression.clone().unwrap();
        // 3 → 1 (unquoted too), 6 → 4; the label of a copied node stays; the id and the label
        // outside the selection (2, "plate" = 7) can't resolve any more
        assert_eq!(saved, r#"node("1").x + node('mix').opacity + node( "outside:2" ).y + self.opacity + node("4").x + node("outside:7").x"#);
        let back = CompTemplate::parse(&t.to_bytes().unwrap()).unwrap();
        assert_eq!(back, t);
        let mut target = sample();
        let ins = back.insert(&mut target, [0.0, 0.0]).unwrap();
        assert_eq!(ins.nodes, vec![9, 10, 11, 12]);
        assert!(ins.warnings.is_empty(), "{:?}", ins.warnings);
        let inserted = target.node(11).unwrap().params["opacity"].expression.clone().unwrap();
        assert_eq!(inserted, r#"node("9").x + node('mix').opacity + node( "outside:2" ).y + self.opacity + node("12").x + node("outside:7").x"#);
        // a target node labelled like a new id: inserted, with a warning
        let mut target = sample();
        target.node_mut(8).unwrap().label = "9".into();
        let ins = back.insert(&mut target, [0.0, 0.0]).unwrap();
        assert_eq!(ins.warnings.len(), 1, "{:?}", ins.warnings);
        assert!(ins.warnings[0].contains("labelled \u{201c}9\u{201d}"), "{:?}", ins.warnings);
        // the template's own labels decide label-or-id, not the target's: "mix" stays a label
        assert!(target.node(11).unwrap().params["opacity"].expression.as_deref().is_some_and(|s| s.contains("node('mix')")));
        // a rewrite that would pass the source cap keeps the original, with a warning
        let mut c = sample();
        let long = format!("{}0{}", "node(3).x+".repeat(30), " ".repeat(crate::expr::MAX_SOURCE - 301));
        assert!(long.len() <= crate::expr::MAX_SOURCE && crate::expr::parse(&long).is_ok());
        c.node_mut(5).unwrap().params.insert("opacity".into(), Param { expression: Some(long.clone()), ..Param::new(ParamValue::Float(1.0)) });
        let r = CompTemplate::from_selection(&c, &[3, 5], "Long").unwrap();
        assert_eq!(r.template.nodes[1].params["opacity"].expression.as_deref(), Some(long.as_str()));
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("too long"), "{:?}", r.warnings);
        // a source that doesn't parse is kept as it is
        let mut c = sample();
        c.node_mut(5).unwrap().params.insert("opacity".into(), Param { expression: Some("node(\"3\".x +".into()), ..Param::new(ParamValue::Float(1.0)) });
        let t = CompTemplate::from_selection(&c, &[3, 5], "Bad").unwrap().template;
        assert_eq!(t.nodes[1].params["opacity"].expression.as_deref(), Some("node(\"3\".x +"));
    }

    #[test]
    fn extreme_positions_are_clamped_and_keep_their_layout() {
        let json = br#"{"version":1,"name":"x","nodes":[
            {"id":1,"kind":{"type":"blur"},"pos":[-5e6,0]},
            {"id":2,"kind":{"type":"blur"},"pos":[5e6,100]},
            {"id":3,"kind":{"type":"blur"},"pos":[1e30,-1e30]}]}"#;
        let t = CompTemplate::parse(json).unwrap();
        assert_eq!(t.nodes[2].pos, [1.0e7, -1.0e7]);
        let mut c = sample();
        let ids = t.insert_into(&mut c, [0.0, 0.0]).unwrap();
        let pos = |i: usize| c.node(ids[i]).unwrap().pos;
        // top-left is (-5e6, -1e7): the first two keep their spacing
        assert_eq!(pos(0), [0.0, 1.0e7]);
        assert_eq!(pos(1), [1.0e7, 1.0e7 + 100.0].map(|v: f32| v.clamp(-1.0e7, 1.0e7)));
        assert_eq!(pos(1)[0] - pos(0)[0], 1.0e7);
        assert!(c.nodes.iter().all(|n| n.pos.iter().all(|v| v.is_finite() && v.abs() <= 1.0e7)));
        // within range the layout is exact
        let t = CompTemplate::parse(
            br#"{"version":1,"name":"x","nodes":[{"id":1,"kind":{"type":"blur"},"pos":[9e6,0]},{"id":2,"kind":{"type":"blur"},"pos":[9.5e6,40]}]}"#,
        )
        .unwrap();
        let mut c = sample();
        let ids = t.insert_into(&mut c, [10.0, 20.0]).unwrap();
        assert_eq!(c.node(ids[0]).unwrap().pos, [10.0, 20.0]);
        assert_eq!(c.node(ids[1]).unwrap().pos, [500_010.0, 60.0]);
    }

    #[test]
    fn parse_holds_the_comp_caps() {
        let label = "x".repeat(10_000);
        let name = "n".repeat(500);
        let json = format!(r#"{{"version":1,"name":"{name}","nodes":[{{"id":5,"kind":{{"type":"blur"}},"label":"{label}","pos":[1e30,2]}}]}}"#);
        let t = CompTemplate::parse(json.as_bytes()).unwrap();
        assert_eq!(t.name.chars().count(), MAX_NAME);
        assert_eq!(t.nodes[0].label.chars().count(), crate::comp::MAX_LABEL);
        // far-off positions are clamped on insert
        let mut c = sample();
        let ids = t.insert_into(&mut c, [0.0, 0.0]).unwrap();
        assert!(c.node(ids[0]).unwrap().pos.iter().all(|v| v.is_finite()));
    }
}
