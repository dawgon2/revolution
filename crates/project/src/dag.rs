//! A generic node graph (directed acyclic graph) shared by comps (C1) and grades (C2).
//!
//! The graph is plain data: nodes that implement [`DagNode`] and a list of [`Edge`]s from an
//! output port to an input port. [`validate`] checks a graph from a project file (hostile input:
//! counts capped, unknown nodes, bad ports, duplicate inputs and cycles reported, never a panic)
//! and returns the evaluation order of the nodes the output depends on. See ADR 0007
//! (`docs/adr/0007-node-graph.md`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// A node's id: stable and unique within one graph (new nodes take max + 1).
pub type NodeId = u64;

/// Most nodes a graph may hold.
pub const MAX_NODES: usize = 256;
/// Most edges a graph may hold.
pub const MAX_EDGES: usize = 1024;
/// Longest chain of nodes (sources count as depth 1): 64 nodes after a source, so a full serial
/// grade (its virtual input + `grade::MAX_NODES` nodes, fork C2) fits.
pub const MAX_DEPTH: usize = 65;

/// One port of a node: an output port when it is an edge's `from`, an input port as its `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Port {
    pub node: NodeId,
    #[serde(default)]
    pub index: u8,
}

impl Port {
    pub fn new(node: NodeId, index: u8) -> Self {
        Self { node, index }
    }
}

/// A connection from an output port to an input port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub from: Port,
    pub to: Port,
}

impl Edge {
    pub fn new(from: Port, to: Port) -> Self {
        Self { from, to }
    }
}

/// What a graph needs to know about its nodes to be checked, sorted and scheduled.
pub trait DagNode {
    fn id(&self) -> NodeId;
    /// Number of input ports (Merge 2, Media Out 1, Media In 0 …).
    fn inputs(&self) -> u8;
    /// Number of output ports (1 for every C1 node).
    fn outputs(&self) -> u8;
    /// A bypassed node passes its input 0 through (a bypassed source is transparent).
    fn bypassed(&self) -> bool;
}

/// Why a graph can't be evaluated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DagError {
    /// The nodes that are in a cycle or depend on one (sorted by id).
    #[error("the graph has a cycle through nodes {0:?}")]
    Cycle(Vec<NodeId>),
    #[error("too many nodes (at most {MAX_NODES})")]
    TooManyNodes,
    #[error("too many connections (at most {MAX_EDGES})")]
    TooManyEdges,
    #[error("the graph is too deep (at most {MAX_DEPTH} nodes in a chain)")]
    TooDeep,
    #[error("unknown node {0}")]
    UnknownNode(NodeId),
    #[error("node {0} appears twice")]
    DuplicateNode(NodeId),
    #[error("node {} has no port {}", .0.node, .0.index)]
    BadPort(Port),
    #[error("input {} of node {} has two connections", .0.index, .0.node)]
    DuplicateInput(Port),
    #[error("the output node doesn't exist")]
    NoOutput,
}

/// One node to evaluate and where each of its inputs comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub node: NodeId,
    /// One entry per input port: the output port feeding it, `None` when unconnected.
    pub inputs: Vec<Option<Port>>,
}

/// The evaluation order of a valid graph: only the nodes the output depends on, every node after
/// all of its inputs, the output last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub steps: Vec<Step>,
}

impl Order {
    /// The node ids in evaluation order.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.steps.iter().map(|s| s.node)
    }

    /// How many steps after its own step each node's result is still needed (its last consumer's
    /// position), by node id: for texture liveness. The output's entry is its own position.
    pub fn last_use(&self) -> BTreeMap<NodeId, usize> {
        let mut last = BTreeMap::new();
        for (i, s) in self.steps.iter().enumerate() {
            last.insert(s.node, i);
            for p in s.inputs.iter().flatten() {
                last.insert(p.node, i);
            }
        }
        last
    }
}

/// Checks a graph and returns the evaluation order of the nodes `output` depends on.
///
/// Rules: counts within [`MAX_NODES`] / [`MAX_EDGES`]; node ids unique; every edge between known
/// nodes and existing ports; at most one edge per input port; no cycle anywhere in the graph (also
/// among nodes the output doesn't use, so the canvas can show it); no chain deeper than
/// [`MAX_DEPTH`]. Unconnected inputs are allowed. Work is bounded by the caps.
pub fn validate<N: DagNode>(nodes: &[N], edges: &[Edge], output: NodeId) -> Result<Order, DagError> {
    if nodes.len() > MAX_NODES {
        return Err(DagError::TooManyNodes);
    }
    if edges.len() > MAX_EDGES {
        return Err(DagError::TooManyEdges);
    }
    let mut index: BTreeMap<NodeId, usize> = BTreeMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if index.insert(n.id(), i).is_some() {
            return Err(DagError::DuplicateNode(n.id()));
        }
    }
    // Input slots per node, filled from the edges.
    let mut inputs: Vec<Vec<Option<Port>>> = nodes.iter().map(|n| vec![None; usize::from(n.inputs())]).collect();
    // Consumers per node (by node index), for Kahn's algorithm.
    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for e in edges {
        let from = *index.get(&e.from.node).ok_or(DagError::UnknownNode(e.from.node))?;
        let to = *index.get(&e.to.node).ok_or(DagError::UnknownNode(e.to.node))?;
        let from_node = nodes.get(from).ok_or(DagError::UnknownNode(e.from.node))?;
        if e.from.index >= from_node.outputs() {
            return Err(DagError::BadPort(e.from));
        }
        let slot = inputs.get_mut(to).and_then(|v| v.get_mut(usize::from(e.to.index))).ok_or(DagError::BadPort(e.to))?;
        if slot.is_some() {
            return Err(DagError::DuplicateInput(e.to));
        }
        *slot = Some(e.from);
        if let Some(c) = consumers.get_mut(from) {
            c.push(to);
        }
    }
    let out = *index.get(&output).ok_or(DagError::NoOutput)?;

    // Kahn's algorithm over the whole graph, smallest id first so the order is deterministic.
    let mut pending: Vec<usize> = inputs.iter().map(|v| v.iter().flatten().count()).collect();
    let mut ready: BTreeSet<(NodeId, usize)> = BTreeSet::new();
    for (i, n) in nodes.iter().enumerate() {
        if pending.get(i) == Some(&0) {
            ready.insert((n.id(), i));
        }
    }
    let mut depth = vec![0usize; nodes.len()];
    let mut sorted: Vec<usize> = Vec::with_capacity(nodes.len());
    while let Some((_, i)) = ready.pop_first() {
        let d = inputs
            .get(i)
            .map(|v| v.iter().flatten().filter_map(|p| index.get(&p.node).and_then(|&j| depth.get(j))).max().copied().unwrap_or(0))
            .unwrap_or(0)
            .saturating_add(1);
        if d > MAX_DEPTH {
            return Err(DagError::TooDeep);
        }
        if let Some(slot) = depth.get_mut(i) {
            *slot = d;
        }
        sorted.push(i);
        for &c in consumers.get(i).map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(p) = pending.get_mut(c) {
                *p = p.saturating_sub(1);
                if *p == 0
                    && let Some(n) = nodes.get(c)
                {
                    ready.insert((n.id(), c));
                }
            }
        }
    }
    if sorted.len() < nodes.len() {
        let done: BTreeSet<usize> = sorted.iter().copied().collect();
        let mut stuck: Vec<NodeId> = nodes.iter().enumerate().filter(|(i, _)| !done.contains(i)).map(|(_, n)| n.id()).collect();
        stuck.sort_unstable();
        return Err(DagError::Cycle(stuck));
    }

    // Keep only what the output depends on (walk its inputs back; bounded by the node count).
    let mut needed = vec![false; nodes.len()];
    let mut stack = vec![out];
    while let Some(i) = stack.pop() {
        match needed.get_mut(i) {
            Some(seen) if !*seen => *seen = true,
            _ => continue,
        }
        for p in inputs.get(i).map(Vec::as_slice).unwrap_or(&[]).iter().flatten() {
            if let Some(&j) = index.get(&p.node) {
                stack.push(j);
            }
        }
    }
    let steps = sorted
        .into_iter()
        .filter(|&i| needed.get(i) == Some(&true))
        .filter_map(|i| Some(Step { node: nodes.get(i)?.id(), inputs: inputs.get(i)?.clone() }))
        .collect();
    Ok(Order { steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct N {
        id: NodeId,
        ins: u8,
        outs: u8,
    }

    impl DagNode for N {
        fn id(&self) -> NodeId {
            self.id
        }
        fn inputs(&self) -> u8 {
            self.ins
        }
        fn outputs(&self) -> u8 {
            self.outs
        }
        fn bypassed(&self) -> bool {
            false
        }
    }

    fn n(id: NodeId, ins: u8) -> N {
        N { id, ins, outs: 1 }
    }

    fn e(from: NodeId, to: NodeId, port: u8) -> Edge {
        Edge::new(Port::new(from, 0), Port::new(to, port))
    }

    fn ids(o: &Order) -> Vec<NodeId> {
        o.nodes().collect()
    }

    /// Media In 1, Media In 2 → Merge 3 → Out 4.
    fn merge_graph() -> (Vec<N>, Vec<Edge>) {
        (vec![n(4, 1), n(3, 2), n(2, 0), n(1, 0)], vec![e(1, 3, 0), e(2, 3, 1), e(3, 4, 0)])
    }

    #[test]
    fn topological_order_puts_inputs_first_and_output_last() {
        let (nodes, edges) = merge_graph();
        let o = validate(&nodes, &edges, 4).unwrap();
        assert_eq!(ids(&o), vec![1, 2, 3, 4]);
        let merge = o.steps.get(2).unwrap();
        assert_eq!(merge.inputs, vec![Some(Port::new(1, 0)), Some(Port::new(2, 0))]);
    }

    #[test]
    fn order_is_deterministic_by_id() {
        let (mut nodes, edges) = merge_graph();
        nodes.reverse();
        assert_eq!(ids(&validate(&nodes, &edges, 4).unwrap()), vec![1, 2, 3, 4]);
    }

    #[test]
    fn unreachable_nodes_are_pruned() {
        let (mut nodes, mut edges) = merge_graph();
        nodes.push(n(9, 0));
        nodes.push(n(10, 1));
        edges.push(e(9, 10, 0));
        assert_eq!(ids(&validate(&nodes, &edges, 4).unwrap()), vec![1, 2, 3, 4]);
        // The output may be any node: only its ancestors are kept.
        assert_eq!(ids(&validate(&nodes, &edges, 10).unwrap()), vec![9, 10]);
    }

    #[test]
    fn dangling_inputs_are_allowed() {
        let nodes = vec![n(1, 0), n(2, 2), n(3, 1)];
        let edges = vec![e(1, 2, 0), e(2, 3, 0)];
        let o = validate(&nodes, &edges, 3).unwrap();
        assert_eq!(o.steps.get(1).unwrap().inputs, vec![Some(Port::new(1, 0)), None]);
        // An output with nothing connected is a single step.
        let o = validate(&[n(5, 1)], &[], 5).unwrap();
        assert_eq!(o.steps, vec![Step { node: 5, inputs: vec![None] }]);
    }

    #[test]
    fn cycles_are_reported_even_off_the_output_path() {
        let nodes = vec![n(1, 1), n(2, 1), n(3, 1)];
        let edges = vec![e(1, 2, 0), e(2, 1, 0), e(2, 3, 0)];
        assert_eq!(validate(&nodes, &edges, 3), Err(DagError::Cycle(vec![1, 2, 3])));
        // A self-loop.
        assert_eq!(validate(&[n(7, 1)], &[e(7, 7, 0)], 7), Err(DagError::Cycle(vec![7])));
        // A cycle not feeding the output.
        let nodes = vec![n(1, 0), n(2, 1), n(3, 1), n(4, 1)];
        let edges = vec![e(1, 2, 0), e(3, 4, 0), e(4, 3, 0)];
        assert_eq!(validate(&nodes, &edges, 2), Err(DagError::Cycle(vec![3, 4])));
    }

    #[test]
    fn duplicate_input_is_refused() {
        let nodes = vec![n(1, 0), n(2, 0), n(3, 1)];
        let edges = vec![e(1, 3, 0), e(2, 3, 0)];
        assert_eq!(validate(&nodes, &edges, 3), Err(DagError::DuplicateInput(Port::new(3, 0))));
    }

    #[test]
    fn bad_ports_and_unknown_nodes_are_refused() {
        let nodes = vec![n(1, 0), n(2, 1)];
        assert_eq!(validate(&nodes, &[e(1, 2, 1)], 2), Err(DagError::BadPort(Port::new(2, 1))));
        let from_bad = Edge::new(Port::new(1, 3), Port::new(2, 0));
        assert_eq!(validate(&nodes, &[from_bad], 2), Err(DagError::BadPort(Port::new(1, 3))));
        assert_eq!(validate(&nodes, &[e(8, 2, 0)], 2), Err(DagError::UnknownNode(8)));
        assert_eq!(validate(&nodes, &[e(1, 8, 0)], 2), Err(DagError::UnknownNode(8)));
        assert_eq!(validate(&nodes, &[], 99), Err(DagError::NoOutput));
        assert_eq!(validate::<N>(&[], &[], 0), Err(DagError::NoOutput));
        // A node with no output ports can't feed anything.
        let sink = vec![N { id: 1, ins: 0, outs: 0 }, n(2, 1)];
        assert_eq!(validate(&sink, &[e(1, 2, 0)], 2), Err(DagError::BadPort(Port::new(1, 0))));
    }

    #[test]
    fn duplicate_node_ids_are_refused() {
        assert_eq!(validate(&[n(1, 0), n(1, 1)], &[], 1), Err(DagError::DuplicateNode(1)));
    }

    #[test]
    fn caps_hold() {
        let many: Vec<N> = (0..=MAX_NODES as u64).map(|i| n(i, 0)).collect();
        assert_eq!(validate(&many, &[], 0), Err(DagError::TooManyNodes));
        let edges = vec![e(1, 2, 0); MAX_EDGES + 1];
        assert_eq!(validate(&[n(1, 0), n(2, 1)], &edges, 2), Err(DagError::TooManyEdges));

        // A chain of exactly MAX_DEPTH nodes is fine, one more is too deep.
        let chain = |len: u64| -> (Vec<N>, Vec<Edge>) {
            let nodes = (0..len).map(|i| n(i, u8::from(i > 0))).collect();
            let edges = (1..len).map(|i| e(i - 1, i, 0)).collect();
            (nodes, edges)
        };
        let (nodes, edges) = chain(MAX_DEPTH as u64);
        assert_eq!(validate(&nodes, &edges, MAX_DEPTH as u64 - 1).unwrap().steps.len(), MAX_DEPTH);
        let (nodes, edges) = chain(MAX_DEPTH as u64 + 1);
        assert_eq!(validate(&nodes, &edges, MAX_DEPTH as u64), Err(DagError::TooDeep));
    }

    #[test]
    fn fan_out_is_allowed_and_last_use_counts_every_consumer() {
        // 1 feeds both inputs of a Merge (2) and a second chain (3) merged later (4).
        let nodes = vec![n(1, 0), n(2, 2), n(3, 1), n(4, 2)];
        let edges = vec![e(1, 2, 0), e(1, 2, 1), e(1, 3, 0), e(2, 4, 0), e(3, 4, 1)];
        let o = validate(&nodes, &edges, 4).unwrap();
        assert_eq!(ids(&o), vec![1, 2, 3, 4]);
        let last = o.last_use();
        assert_eq!(last.get(&1), Some(&2));
        assert_eq!(last.get(&2), Some(&3));
        assert_eq!(last.get(&4), Some(&3));
    }

    #[test]
    fn hostile_random_graphs_never_panic() {
        // A small deterministic generator: random ids (with repeats), port counts and edges.
        let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..2000 {
            let count = (next() % 40) as usize;
            let nodes: Vec<N> = (0..count).map(|_| N { id: next() % 48, ins: (next() % 4) as u8, outs: (next() % 3) as u8 }).collect();
            let edges: Vec<Edge> =
                (0..(next() % 80)).map(|_| Edge::new(Port::new(next() % 50, (next() % 4) as u8), Port::new(next() % 50, (next() % 4) as u8))).collect();
            if let Ok(o) = validate(&nodes, &edges, next() % 50) {
                // Every input of a step comes from an earlier step.
                let mut seen = BTreeSet::new();
                for st in &o.steps {
                    for p in st.inputs.iter().flatten() {
                        assert!(seen.contains(&p.node));
                    }
                    seen.insert(st.node);
                }
            }
        }
    }

    #[test]
    fn edges_round_trip_through_serde() {
        let edge = e(3, 4, 1);
        let json = serde_json::to_string(&edge).unwrap();
        assert_eq!(serde_json::from_str::<Edge>(&json).unwrap(), edge);
        // A missing port index is port 0.
        let short: Edge = serde_json::from_str(r#"{"from":{"node":1},"to":{"node":2}}"#).unwrap();
        assert_eq!(short, e(1, 2, 0));
    }
}
