//! Colour grades: a per-clip list of serial correction nodes (fork ADR 0004).
//!
//! A grade runs on the clip's grading signal after its input transform and before its effect
//! stack. Each node holds keyframable [`Param`]s keyed by name ([`NODE_PARAMS`]); a missing key is
//! the identity, so an empty node changes nothing. Nodes are addressed by a stable id, never by
//! index, so a node graph (parallel / layer nodes, fork C2) adds edges without a format break.
//!
//! **Graph (fork C2, `docs/fork/DESIGN-C2.md` §1).** A grade without [`Grade::edges`] is the serial
//! chain it always was (the grade's input → node 1 → node 2 … → output), and compiles exactly as
//! before. A branched grade stores every connection as a [`dag::Edge`]; the grade's input is the
//! virtual node [`SOURCE`] (id 0, never in `nodes`). [`Grade::graph_order`] checks either form and
//! returns the evaluation order.

use std::collections::BTreeMap;

use crate::dag::{self, DagError, DagNode, Edge, NodeId, Order, Port};
use crate::keyframe::{Param, ParamValue};
use crate::mask::Mask;
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

/// The most nodes a grade compiles; further nodes (only in a damaged or hostile file) are kept
/// but ignored.
pub const MAX_NODES: usize = 64;

// a full serial chain (the input + every node) must be a valid graph
const _: () = assert!(MAX_NODES < dag::MAX_DEPTH && MAX_NODES < dag::MAX_NODES);

/// A clip's colour grade.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Grade {
    /// Serial nodes, applied first to last.
    #[serde(default)]
    pub nodes: Vec<GradeNode>,
    /// Bypass the whole grade (the clip renders as if it had none).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bypass: bool,
    /// Connections of a branched grade (fork C2); empty: the implicit serial chain
    /// ([`Grade::implicit_edges`]). Endpoints are node ids or [`SOURCE`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Edge>,
    /// The node whose RGB output is the grade's result; `None`: the last node (serial) or the one
    /// node whose RGB output feeds nothing (graph).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<NodeId>,
    /// Where the grade's input sits on the node canvas once dragged (fork C2b); `None`: the auto
    /// layout ([`Grade::layout`]). UI only.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_pos")]
    pub input_pos: Option<[f32; 2]>,
}

/// The id of the grade's input picture in [`Grade::edges`] (a virtual node, never in `nodes`).
pub const SOURCE: NodeId = 0;

/// Most inputs a mixer node (parallel, layer, key mixer) takes.
pub const MAX_MIX_INPUTS: u8 = 16;

/// What a node does (fork C2). Everything but [`NodeKind::Correction`] needs a branched grade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    /// A correction node (every node before C2). Inputs 0 = RGB, 1 = key; outputs 0 = RGB, 1 = key
    /// (its qualifier × windows matte). An outside node is a correction node whose key input is
    /// another node's key output, inverted (`key.invert`).
    #[default]
    Correction,
    /// Weighted sum of its RGB inputs (`mix.gain.<i>`).
    ParallelMixer,
    /// Its RGB inputs stacked, higher inputs on top (`layer.mode.<i>` / `layer.opacity.<i>`).
    LayerMixer,
    /// RGB in, three grey pictures out (R, G, B).
    Splitter,
    /// Three grey pictures in (R, G, B), RGB out.
    Combiner,
    /// Combines key inputs (`kmix.mode.<i>`, `kmix.invert.<i>`) into one key.
    KeyMixer,
}

impl NodeKind {
    fn is_correction(&self) -> bool {
        *self == NodeKind::Correction
    }

    /// Input ports of this kind.
    pub fn inputs(self) -> u8 {
        match self {
            NodeKind::Correction => 2,
            NodeKind::ParallelMixer | NodeKind::LayerMixer | NodeKind::KeyMixer => MAX_MIX_INPUTS,
            NodeKind::Splitter => 1,
            NodeKind::Combiner => 3,
        }
    }

    /// Output ports of this kind.
    pub fn outputs(self) -> u8 {
        match self {
            NodeKind::Correction => 2,
            NodeKind::Splitter => 3,
            NodeKind::ParallelMixer | NodeKind::LayerMixer | NodeKind::Combiner | NodeKind::KeyMixer => 1,
        }
    }
}

/// Why a grade's graph can't render (the grade then renders as bypassed and the Color page says
/// why).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error(transparent)]
    Dag(#[from] DagError),
    #[error("several nodes could be the grade's output ({0:?}); connect them or pick one")]
    AmbiguousOutput(Vec<NodeId>),
    #[error("a node uses id 0, which is the grade's input")]
    SourceIdUsed,
}

/// One correction node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradeNode {
    /// Stable id, unique within its grade.
    pub id: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// Parameters by key ([`NODE_PARAMS`]); missing keys are the identity.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Param>,
    /// Windows (G2.2): shapes that limit the node's correction, combined like an effect's masks
    /// (each mask's mode, feather, expansion, opacity, invert; keyframed and trackable). Paths are
    /// in the clip's source pixels. Only the first [`MAX_WINDOWS`] render.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<Mask>,
    /// A LUT applied by the node after its corrections (G2.6): a LUT reference as Lumetri's
    /// (`builtin:<id>`, `lib:<id>` of the project's LUT library; empty: none), mixed in by
    /// `lut.mix`. An unknown reference applies nothing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lut: String,
    /// What the node does (fork C2); correction for every node before C2.
    #[serde(default, skip_serializing_if = "NodeKind::is_correction")]
    pub kind: NodeKind,
    /// Where the node sits on the Color page's node canvas once dragged (fork C2b, canvas units);
    /// `None`: the auto layout places it ([`Grade::layout`]). UI only: rendering ignores it. A
    /// hostile value loads as `None` (not finite) or clamped to ±[`MAX_POS`].
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_pos")]
    pub pos: Option<[f32; 2]>,
}

/// The farthest a stored canvas position may be from the origin (canvas units).
pub const MAX_POS: f32 = 100_000.0;

/// Canvas units between the auto layout's columns and rows ([`Grade::layout`]).
pub const LAYOUT_COLUMN: f32 = 176.0;
pub const LAYOUT_ROW: f32 = 74.0;

/// A canvas position from untrusted numbers: `None` unless both are finite, clamped to
/// ±[`MAX_POS`].
pub fn clean_pos(x: f64, y: f64) -> Option<[f32; 2]> {
    if !(x.is_finite() && y.is_finite()) {
        return None;
    }
    let m = f64::from(MAX_POS);
    Some([x.clamp(-m, m) as f32, y.clamp(-m, m) as f32])
}

/// A stored position that is usable (finite, in range), as [`clean_pos`] would load it.
fn usable(p: Option<[f32; 2]>) -> Option<[f32; 2]> {
    p.and_then(|[x, y]| clean_pos(f64::from(x), f64::from(y)))
}

fn de_pos<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<[f32; 2]>, D::Error> {
    let v: Option<[f64; 2]> = Option::deserialize(d)?;
    Ok(v.and_then(|[x, y]| clean_pos(x, y)))
}

/// The most windows a node renders.
pub const MAX_WINDOWS: usize = 16;

fn enabled_default() -> bool {
    true
}

/// What a node parameter holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeParamKind {
    /// A number with its identity value and range.
    Float { identity: f32, min: f32, max: f32 },
    /// A colour wheel: `Color([r, g, b, master])` offsets around identity (all 0), each in
    /// `-range..=range`.
    Wheel { range: f32 },
    /// A tone curve: `Curve` points in 0…1 (identity: the straight line).
    Curve,
}

/// One entry of the node schema.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeParam {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: NodeParamKind,
}

/// Mid grey (18 % scene reflectance) on the DaVinci Intermediate signal: the default contrast
/// pivot.
pub const DI_MID_GREY: f32 = 0.336;

/// Every node parameter (G0 / G1 primaries and curves; ADR 0004 §2).
pub const NODE_PARAMS: &[NodeParam] = &[
    NodeParam { key: "lift", label: "Lift", kind: NodeParamKind::Wheel { range: 1.0 } },
    NodeParam { key: "gamma", label: "Gamma", kind: NodeParamKind::Wheel { range: 1.0 } },
    NodeParam { key: "gain", label: "Gain", kind: NodeParamKind::Wheel { range: 4.0 } },
    NodeParam { key: "offset", label: "Offset", kind: NodeParamKind::Wheel { range: 1.0 } },
    NodeParam { key: "contrast", label: "Contrast", kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 4.0 } },
    NodeParam { key: "pivot", label: "Pivot", kind: NodeParamKind::Float { identity: DI_MID_GREY, min: 0.0, max: 1.0 } },
    NodeParam { key: "saturation", label: "Saturation", kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 4.0 } },
    NodeParam { key: "temperature", label: "Temperature", kind: NodeParamKind::Float { identity: 0.0, min: -100.0, max: 100.0 } },
    NodeParam { key: "tint", label: "Tint", kind: NodeParamKind::Float { identity: 0.0, min: -100.0, max: 100.0 } },
    NodeParam { key: "curve.master", label: "Master curve", kind: NodeParamKind::Curve },
    NodeParam { key: "curve.r", label: "Red curve", kind: NodeParamKind::Curve },
    NodeParam { key: "curve.g", label: "Green curve", kind: NodeParamKind::Curve },
    NodeParam { key: "curve.b", label: "Blue curve", kind: NodeParamKind::Curve },
    // film grain (G1.6): monochrome, luma weighted, a new pattern every frame
    NodeParam { key: "grain.strength", label: "Grain strength", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    // (grain cell size in full-resolution pixels: the clip's source pixels; see `filmcraft_render::grade::GradeCtx`)
    NodeParam { key: "grain.size", label: "Grain size", kind: NodeParamKind::Float { identity: 1.0, min: 1.0, max: 4.0 } },
    NodeParam { key: "grain.softness", label: "Grain softness", kind: NodeParamKind::Float { identity: 0.5, min: 0.0, max: 1.0 } },
    // HSL qualifier (G2.1): limits the node's correction to a key of its input; ranges on the
    // grading signal as HSL in 0…1; on / invert are switches (≥ ½ is on), show 0 off,
    // 1 highlight (colour over grey), 2 matte
    NodeParam { key: "qual.on", label: "Qualifier", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.hue", label: "Hue centre", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.hueWidth", label: "Hue width", kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.satLow", label: "Saturation low", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.satHigh", label: "Saturation high", kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.lumLow", label: "Luminance low", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.lumHigh", label: "Luminance high", kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.soft", label: "Qualifier softness", kind: NodeParamKind::Float { identity: 0.05, min: 0.001, max: 0.5 } },
    NodeParam { key: "qual.blur", label: "Matte blur", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.invert", label: "Invert qualifier", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "qual.show", label: "Highlight", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 2.0 } },
    // spatial noise reduction (G2.4) at the node's input: luma / chroma strength, filter size in
    // source pixels (σ)
    NodeParam { key: "nr.luma", label: "NR luma", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "nr.chroma", label: "NR chroma", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "nr.size", label: "NR radius", kind: NodeParamKind::Float { identity: 1.5, min: 0.25, max: 3.5 } },
    // (temporal noise reduction is a clip setting since schema v17: `crate::tnr`, fork 6.7b)
    // the node's LUT mix (G2.6)
    NodeParam { key: "lut.mix", label: "LUT mix", kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 1.0 } },
    // highlight soft clip (G2.5): above the knee (on the grading signal) values roll off
    // smoothly towards 1 instead of clipping; amount 0 is off
    NodeParam { key: "hl.rolloff", label: "Highlight roll-off", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
    NodeParam { key: "hl.knee", label: "Roll-off knee", kind: NodeParamKind::Float { identity: 0.75, min: 0.5, max: 0.95 } },
];

/// The graph parameters (fork C2, `docs/fork/DESIGN-C2.md` §1), one entry per mixer input
/// (`<family>.<i>`, `i` < [`MAX_MIX_INPUTS`]).
macro_rules! graph_params {
    ($($i:literal)*) => {
        &[
            // a correction node's key input: inverted (switch), then × gain + offset, clamped to 0…1
            NodeParam { key: "key.invert", label: "Invert key input", kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },
            NodeParam { key: "key.gain", label: "Key input gain", kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 1.0 } },
            NodeParam { key: "key.offset", label: "Key input offset", kind: NodeParamKind::Float { identity: 0.0, min: -1.0, max: 1.0 } },
            // parallel mixer: each input's relative gain; the weights are the gains over their sum
            // across the connected inputs, so all at 1 (the default) is an equal mix
            $(NodeParam { key: concat!("mix.gain.", $i), label: concat!("Input ", $i, " gain"), kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 2.0 } },)*
            // layer mixer: blend mode (index into `effect::BLEND_MODES`) and opacity of each input
            // over the ones below it
            $(NodeParam { key: concat!("layer.mode.", $i), label: concat!("Layer ", $i, " blend mode"), kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 26.0 } },)*
            $(NodeParam { key: concat!("layer.opacity.", $i), label: concat!("Layer ", $i, " opacity"), kind: NodeParamKind::Float { identity: 1.0, min: 0.0, max: 1.0 } },)*
            // key mixer: 0 add, 1 subtract, 2 intersect (with the inputs before it); invert switch
            $(NodeParam { key: concat!("kmix.mode.", $i), label: concat!("Key ", $i, " mode"), kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 2.0 } },)*
            $(NodeParam { key: concat!("kmix.invert.", $i), label: concat!("Invert key ", $i), kind: NodeParamKind::Float { identity: 0.0, min: 0.0, max: 1.0 } },)*
        ]
    };
}

/// Parameters of graph nodes (fork C2): a correction node's key input (`key.*`) and the mixers'
/// per-input settings. Kept apart from [`NODE_PARAMS`] (the correction controls every node shows);
/// [`node_param`] finds both.
pub const GRAPH_PARAMS: &[NodeParam] = graph_params!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15);

// one entry per mixer input
const _: () = assert!(GRAPH_PARAMS.len() == 3 + 5 * MAX_MIX_INPUTS as usize);

/// The schema entry for `key`.
pub fn node_param(key: &str) -> Option<&'static NodeParam> {
    NODE_PARAMS.iter().chain(GRAPH_PARAMS).find(|p| p.key == key)
}

/// The most curve points a node curve keeps when compiled.
pub const MAX_CURVE_POINTS: usize = 64;

impl GradeNode {
    /// A node with every parameter at identity.
    pub fn new(id: u64) -> Self {
        Self {
            id,
            label: String::new(),
            enabled: true,
            params: BTreeMap::new(),
            windows: Vec::new(),
            lut: String::new(),
            kind: NodeKind::Correction,
            pos: None,
        }
    }

    pub fn param(&self, key: &str) -> Option<&Param> {
        self.params.get(key)
    }

    /// A number parameter at clip time `t`, clamped to its schema range; non-finite, missing or
    /// mistyped values give the identity.
    pub fn float_at(&self, key: &str, t: Tick) -> f32 {
        let Some(NodeParamKind::Float { identity, min, max }) = node_param(key).map(|p| p.kind) else {
            return 0.0;
        };
        match self.params.get(key).map(|p| p.value_at(t)) {
            Some(ParamValue::Float(v)) if v.is_finite() => (v as f32).clamp(min, max),
            _ => identity,
        }
    }

    /// A wheel parameter at clip time `t` as `[r, g, b, master]` offsets, each clamped to the
    /// wheel's range; non-finite components are 0.
    pub fn wheel_at(&self, key: &str, t: Tick) -> [f32; 4] {
        let Some(NodeParamKind::Wheel { range }) = node_param(key).map(|p| p.kind) else {
            return [0.0; 4];
        };
        match self.params.get(key).map(|p| p.value_at(t)) {
            Some(ParamValue::Color(c)) => c.map(|v| if v.is_finite() { v.clamp(-range, range) } else { 0.0 }),
            _ => [0.0; 4],
        }
    }

    /// A curve parameter's points (static value), finite, clamped to 0…1 and at most
    /// [`MAX_CURVE_POINTS`]; `None` when missing, mistyped or the straight line.
    pub fn curve(&self, key: &str) -> Option<Vec<[f32; 2]>> {
        let Some(ParamValue::Curve(c)) = self.params.get(key).map(|p| &p.value) else {
            return None;
        };
        let pts: Vec<[f32; 2]> =
            c.iter().filter(|p| p[0].is_finite() && p[1].is_finite()).take(MAX_CURVE_POINTS).map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)]).collect();
        if pts.len() < 2 || pts.iter().all(|p| (p[0] - p[1]).abs() < 1e-4) {
            return None;
        }
        Some(pts)
    }
}

/// A node's HSL qualifier at one time (G2.1): the ranges on the grading signal (HSL, 0…1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Qualifier {
    pub hue: f32,
    /// The full width of the hue band (1: every hue).
    pub hue_width: f32,
    pub sat: [f32; 2],
    pub lum: [f32; 2],
    /// Width of each soft edge.
    pub soft: f32,
    /// Blur of the matte, 0…1 (of the largest radius).
    pub blur: f32,
    pub invert: bool,
    /// 0 off, 1 highlight (colour over grey), 2 the matte.
    pub show: u32,
}

impl GradeNode {
    /// The node's qualifier at clip time `t`, when switched on.
    pub fn qualifier_at(&self, t: Tick) -> Option<Qualifier> {
        if self.float_at("qual.on", t) < 0.5 {
            return None;
        }
        let f = |k| self.float_at(k, t);
        Some(Qualifier {
            hue: f("qual.hue"),
            hue_width: f("qual.hueWidth"),
            sat: [f("qual.satLow"), f("qual.satHigh")],
            lum: [f("qual.lumLow"), f("qual.lumHigh")],
            soft: f("qual.soft"),
            blur: f("qual.blur"),
            invert: f("qual.invert") >= 0.5,
            show: f("qual.show").round() as u32,
        })
    }
}

impl Grade {
    /// A grade with one empty node (id 1).
    pub fn with_node() -> Self {
        Self { nodes: vec![GradeNode::new(1)], ..Default::default() }
    }

    /// The id a new node takes: one more than the largest in use, or (only after a hostile file
    /// used `u64::MAX`) the smallest unused id, so ids stay unique.
    pub fn next_id(&self) -> u64 {
        match self.nodes.iter().map(|n| n.id).max() {
            None => 1,
            Some(max) if max < u64::MAX => max + 1,
            Some(_) => {
                let used: std::collections::BTreeSet<u64> = self.nodes.iter().map(|n| n.id).collect();
                (1..=u64::MAX).find(|i| !used.contains(i)).unwrap_or(0)
            }
        }
    }

    pub fn node(&self, id: u64) -> Option<&GradeNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: u64) -> Option<&mut GradeNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// The nodes that render: none when bypassed, otherwise the enabled ones among the first
    /// [`MAX_NODES`].
    pub fn active_nodes(&self) -> impl Iterator<Item = &GradeNode> {
        self.nodes.iter().take(if self.bypass { 0 } else { MAX_NODES }).filter(|n| n.enabled)
    }

    /// Whether the grade stores explicit connections (a branched grade, fork C2).
    pub fn is_graph(&self) -> bool {
        !self.edges.is_empty()
    }

    /// The serial chain as edges: the input → the first node → … → the last, RGB port to RGB port,
    /// over the first [`MAX_NODES`] nodes.
    pub fn implicit_edges(&self) -> Vec<Edge> {
        let mut edges = Vec::new();
        let mut prev = SOURCE;
        for n in self.nodes.iter().take(MAX_NODES) {
            edges.push(Edge::new(Port::new(prev, 0), Port::new(n.id, 0)));
            prev = n.id;
        }
        edges
    }

    /// The edges that render: the stored ones, or the implicit serial chain.
    pub fn effective_edges(&self) -> Vec<Edge> {
        if self.is_graph() { self.edges.clone() } else { self.implicit_edges() }
    }

    /// The node whose RGB output is the result: [`Grade::output`] when set, the last node of a
    /// serial grade, or the one node of a graph whose RGB output feeds nothing ([`SOURCE`] for a
    /// grade without nodes).
    pub fn output_node(&self) -> Result<NodeId, GraphError> {
        if let Some(id) = self.output {
            return Ok(id);
        }
        let nodes = || self.nodes.iter().take(MAX_NODES);
        if !self.is_graph() {
            return Ok(nodes().next_back().map_or(SOURCE, |n| n.id));
        }
        let fed: std::collections::BTreeSet<NodeId> = self.edges.iter().filter(|e| e.from.index == 0).map(|e| e.from.node).collect();
        // key mixers only make keys, so they are never the picture's output
        let sinks: Vec<NodeId> = nodes().filter(|n| n.kind != NodeKind::KeyMixer && !fed.contains(&n.id)).map(|n| n.id).collect();
        match sinks.as_slice() {
            [] => Ok(SOURCE),
            [one] => Ok(*one),
            many => Err(GraphError::AmbiguousOutput(many.to_vec())),
        }
    }

    /// Checks the grade's graph (serial or branched) and returns the evaluation order, starting
    /// with [`SOURCE`]. Only the first [`MAX_NODES`] nodes take part; an edge to any other node is
    /// an unknown node. Never panics on a hostile file.
    pub fn graph_order(&self) -> Result<Order, GraphError> {
        let nodes: Vec<GraphNode<'_>> = std::iter::once(GraphNode::Source).chain(self.nodes.iter().take(MAX_NODES).map(GraphNode::Node)).collect();
        if self.nodes.iter().take(MAX_NODES).any(|n| n.id == SOURCE) {
            return Err(GraphError::SourceIdUsed);
        }
        let output = self.output_node()?;
        Ok(dag::validate(&nodes, &self.effective_edges(), output)?)
    }

    /// Gives a node with id 0 (only in a damaged or hostile file: 0 is [`SOURCE`]) a fresh id,
    /// with its edges and the output. Returns whether anything changed.
    pub fn renumber_source_id(&mut self) -> bool {
        if !self.nodes.iter().any(|n| n.id == SOURCE) {
            return false;
        }
        let fresh = self.next_id().max(1);
        let fresh = if fresh == SOURCE || self.node(fresh).is_some() {
            let used: std::collections::BTreeSet<u64> = self.nodes.iter().map(|n| n.id).collect();
            (1..=u64::MAX).find(|i| !used.contains(i)).unwrap_or(u64::MAX)
        } else {
            fresh
        };
        let mut renamed = false;
        for n in self.nodes.iter_mut() {
            // only the first id-0 node keeps its edges; any further one is a duplicate either way
            if n.id == SOURCE && !renamed {
                n.id = fresh;
                renamed = true;
            }
        }
        // stored edges written by this fork never name a node 0 (it is the input), so edges keep
        // meaning the input; only an explicit output of 0 meant the node
        if self.output == Some(SOURCE) {
            self.output = Some(fresh);
        }
        true
    }

    /// Turns a serial grade into an explicit graph (the first branching edit does this in the same
    /// undo step): renumbers an id-0 node and writes the chain out as edges. No change to a grade
    /// that already has edges, apart from the renumbering.
    pub fn make_explicit(&mut self) {
        self.renumber_source_id();
        if !self.is_graph() {
            self.edges = self.implicit_edges();
        }
    }

    /// Drops the edges again when the graph is just the serial chain (every node a correction
    /// node, connected input → first → … → last with nothing else, the last the output), so a
    /// grade that was branched and un-branched saves as before. Returns whether it did.
    pub fn simplify(&mut self) -> bool {
        if !self.is_graph() || self.nodes.len() > MAX_NODES || self.nodes.iter().any(|n| n.kind != NodeKind::Correction) {
            return false;
        }
        let mut have = self.edges.clone();
        have.sort_unstable();
        let mut want = self.implicit_edges();
        want.sort_unstable();
        let last = self.nodes.last().map_or(SOURCE, |n| n.id);
        if have != want || self.output.is_some_and(|o| o != last) {
            return false;
        }
        self.edges.clear();
        self.output = None;
        true
    }

    /// Whether any node (or the input) has a stored canvas position (fork C2b).
    pub fn has_positions(&self) -> bool {
        self.input_pos.is_some() || self.nodes.iter().any(|n| n.pos.is_some())
    }

    /// Forgets every stored canvas position (Reset Layout). Returns whether there were any.
    pub fn clear_positions(&mut self) -> bool {
        let had = self.has_positions();
        self.input_pos = None;
        for n in &mut self.nodes {
            n.pos = None;
        }
        had
    }

    /// The stored, usable position of node `id` ([`SOURCE`]: the input).
    pub fn stored_pos(&self, id: NodeId) -> Option<[f32; 2]> {
        if id == SOURCE { usable(self.input_pos) } else { usable(self.node(id).and_then(|n| n.pos)) }
    }

    /// Sets (or clears) the stored position of node `id` ([`SOURCE`]: the input). Returns false
    /// when there is no such node.
    pub fn set_pos(&mut self, id: NodeId, pos: Option<[f32; 2]>) -> bool {
        let pos = usable(pos);
        if id == SOURCE {
            self.input_pos = pos;
            return true;
        }
        match self.node_mut(id) {
            Some(n) => {
                n.pos = pos;
                true
            }
            None => false,
        }
    }

    /// Where each node sits on the node canvas, the grade's input first (fork C2, C2b). A node
    /// with a stored position sits there; the others take the auto layout: their column is the
    /// longest path from the grade's input (a serial grade is one row), nodes sharing a column
    /// stack in list order, moved down past any node already there. Bounded on hostile graphs
    /// (cycles, thousands of nodes): only the first [`MAX_NODES`] nodes are placed.
    pub fn layout(&self) -> Vec<(NodeId, [f32; 2])> {
        let edges = self.effective_edges();
        let nodes: Vec<NodeId> = self.nodes.iter().take(MAX_NODES).map(|n| n.id).collect();
        let mut depth: BTreeMap<NodeId, usize> = nodes.iter().map(|id| (*id, 1)).collect();
        depth.insert(SOURCE, 0);
        for _ in 0..=nodes.len() {
            let mut changed = false;
            for e in &edges {
                let Some(from) = depth.get(&e.from.node).copied() else { continue };
                if e.to.node == SOURCE {
                    continue;
                }
                if let Some(to) = depth.get_mut(&e.to.node) {
                    let want = from.saturating_add(1).min(MAX_NODES.saturating_add(1));
                    if *to < want {
                        *to = want;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let all: Vec<NodeId> = std::iter::once(SOURCE).chain(nodes).collect();
        // the stored positions are taken first, so the auto layout moves around them
        let mut taken: Vec<[f32; 2]> = all.iter().filter_map(|id| self.stored_pos(*id)).collect();
        let mut rows: BTreeMap<usize, usize> = BTreeMap::new();
        all.iter()
            .map(|&id| {
                if let Some(p) = self.stored_pos(id) {
                    return (id, p);
                }
                let col = depth.get(&id).copied().unwrap_or(0);
                let row = rows.entry(col).or_insert(0);
                let mut p = [col as f32 * LAYOUT_COLUMN, *row as f32 * LAYOUT_ROW];
                // (at most one step per placed node: each one blocks at most one row)
                for _ in 0..=taken.len() {
                    if !taken.iter().any(|q| overlaps(p, *q)) {
                        break;
                    }
                    *row = row.saturating_add(1);
                    p[1] = *row as f32 * LAYOUT_ROW;
                }
                *row = row.saturating_add(1);
                taken.push(p);
                (id, p)
            })
            .collect()
    }

    /// Gives just-added nodes `new` a stored position near their anchor once the grade has any
    /// stored position (fork C2b); a grade laid out automatically stays so. The new nodes keep
    /// their arrangement relative to the anchor from the automatic layout (a serial node to its
    /// right, a parallel branch below…), moved down past any node already there. `anchor`
    /// `None` (or gone): the node before the first new one in the list, else the one after.
    pub fn place_new(&mut self, new: &[NodeId], anchor: Option<NodeId>) {
        if self.input_pos.is_none() && !self.nodes.iter().any(|n| n.pos.is_some() && !new.contains(&n.id)) {
            return;
        }
        for id in new {
            self.set_pos(*id, None);
        }
        let first = self.nodes.iter().position(|n| new.contains(&n.id));
        let neighbour = || {
            let i = first?;
            let before = i.checked_sub(1).and_then(|j| self.nodes.get(j)).map(|n| n.id);
            before.or_else(|| self.nodes.iter().skip(i).find(|n| !new.contains(&n.id)).map(|n| n.id))
        };
        let anchor = anchor.filter(|a| *a == SOURCE || self.node(*a).is_some()).or_else(neighbour).unwrap_or(SOURCE);
        let lay = self.layout();
        let mut auto = self.clone();
        auto.clear_positions();
        let auto = auto.layout();
        let at = |l: &[(NodeId, [f32; 2])], id: NodeId| l.iter().find(|(i, _)| *i == id).map(|(_, p)| *p);
        let Some(base) = at(&lay, anchor) else { return };
        let Some(origin) = at(&auto, anchor) else { return };
        let mut taken: Vec<[f32; 2]> = lay.iter().filter(|(id, _)| !new.contains(id)).map(|(_, p)| *p).collect();
        for &id in new {
            let Some(a) = at(&auto, id) else { continue };
            let mut p = [base[0] + (a[0] - origin[0]), base[1] + (a[1] - origin[1])];
            for _ in 0..=taken.len() {
                if !taken.iter().any(|q| overlaps(p, *q)) {
                    break;
                }
                p[1] += LAYOUT_ROW;
            }
            if let Some(p) = clean_pos(f64::from(p[0]), f64::from(p[1])) {
                self.set_pos(id, Some(p));
                taken.push(p);
            }
        }
    }

    /// Moves the stored positions of nodes `ids` by `d` (an appended grade goes to the right of
    /// the one it joins), clamped to ±[`MAX_POS`].
    pub fn offset_positions(&mut self, ids: &[NodeId], d: [f32; 2]) {
        for n in self.nodes.iter_mut().filter(|n| ids.contains(&n.id)) {
            n.pos = usable(n.pos).and_then(|[x, y]| clean_pos(f64::from(x) + f64::from(d[0]), f64::from(y) + f64::from(d[1])));
        }
    }
}

/// Whether two canvas positions are too close for their nodes not to overlap.
fn overlaps(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).abs() < LAYOUT_COLUMN * 0.85 && (a[1] - b[1]).abs() < LAYOUT_ROW * 0.85
}

/// A node of a grade's graph as [`dag::validate`] sees it: the virtual input or a stored node.
#[derive(Clone, Copy, Debug)]
pub enum GraphNode<'a> {
    Source,
    Node(&'a GradeNode),
}

impl DagNode for GraphNode<'_> {
    fn id(&self) -> NodeId {
        match self {
            GraphNode::Source => SOURCE,
            GraphNode::Node(n) => n.id,
        }
    }
    fn inputs(&self) -> u8 {
        match self {
            GraphNode::Source => 0,
            GraphNode::Node(n) => n.kind.inputs(),
        }
    }
    fn outputs(&self) -> u8 {
        match self {
            GraphNode::Source => 1,
            GraphNode::Node(n) => n.kind.outputs(),
        }
    }
    fn bypassed(&self) -> bool {
        match self {
            GraphNode::Source => false,
            GraphNode::Node(n) => !n.enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip_keeps_nodes_params_and_keyframes() {
        let mut g = Grade::with_node();
        let id = g.next_id();
        let mut n = GradeNode::new(id);
        n.label = "Curves".into();
        n.enabled = false;
        n.params.insert("curve.master".into(), Param::new(ParamValue::Curve(vec![[0.0, 0.1], [1.0, 0.9]])));
        let mut lift = Param::new(ParamValue::Color([0.0; 4]));
        lift.put_keyframe(Tick(0), ParamValue::Color([0.1, 0.0, 0.0, 0.0]));
        lift.put_keyframe(Tick(1000), ParamValue::Color([0.0, 0.0, 0.2, 0.05]));
        g.nodes[0].params.insert("lift".into(), lift);
        g.nodes.push(n);
        g.bypass = true;
        let s = serde_json::to_string(&g).unwrap();
        let back: Grade = serde_json::from_str(&s).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.next_id(), 3);
    }

    #[test]
    fn minimal_json_loads_with_defaults_and_unknown_fields_are_ignored() {
        let g: Grade = serde_json::from_str(r#"{"nodes":[{"id":7,"future":{"x":1}}],"alsoFuture":3}"#).unwrap();
        assert_eq!(g.nodes.len(), 1);
        assert!(g.nodes[0].enabled && !g.bypass && g.nodes[0].params.is_empty());
        let empty: Grade = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Grade::default());
    }

    #[test]
    fn hostile_values_are_clamped_or_identity() {
        let mut n = GradeNode::new(1);
        n.params.insert("contrast".into(), Param::new(ParamValue::Float(1e30)));
        n.params.insert("saturation".into(), Param::new(ParamValue::Float(f64::NAN)));
        n.params.insert("pivot".into(), Param::new(ParamValue::Text("x".into())));
        n.params.insert("gain".into(), Param::new(ParamValue::Color([f32::INFINITY, -1e9, f32::NAN, 0.5])));
        n.params.insert("curve.r".into(), Param::new(ParamValue::Curve(vec![[f32::NAN, 0.0], [2.0, -3.0], [0.0, 0.5]])));
        let t = Tick(0);
        assert_eq!(n.float_at("contrast", t), 4.0);
        assert_eq!(n.float_at("saturation", t), 1.0);
        assert_eq!(n.float_at("pivot", t), DI_MID_GREY);
        assert_eq!(n.float_at("no.such.key", t), 0.0);
        assert_eq!(n.wheel_at("gain", t), [0.0, -4.0, 0.0, 0.5]);
        assert_eq!(n.wheel_at("lift", t), [0.0; 4]);
        assert_eq!(n.curve("curve.r"), Some(vec![[1.0, 0.0], [0.0, 0.5]]));
        n.params.insert("curve.g".into(), Param::new(ParamValue::Curve(vec![[0.0, 0.0], [1.0, 1.0]])));
        assert_eq!(n.curve("curve.g"), None);
        let many = (0..10_000).map(|i| [i as f32 / 10_000.0, 0.5]).collect();
        n.params.insert("curve.b".into(), Param::new(ParamValue::Curve(many)));
        assert_eq!(n.curve("curve.b").map(|c| c.len()), Some(MAX_CURVE_POINTS));
    }

    /// Fork 6.7b: temporal NR is a clip setting (`crate::tnr`); `tnr.*` keys left on a node
    /// (a hand-edited file) round-trip but are not node parameters and read as nothing.
    #[test]
    fn node_params_no_longer_list_tnr_keys() {
        assert!(NODE_PARAMS.iter().all(|p| !p.key.starts_with("tnr.")));
        assert!(node_param("tnr.luma").is_none());
        let mut n = GradeNode::new(1);
        n.params.insert("tnr.luma".into(), Param::new(ParamValue::Float(0.7)));
        assert_eq!(n.float_at("tnr.luma", Tick(0)), 0.0);
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(serde_json::from_str::<GradeNode>(&json).unwrap(), n);
    }

    #[test]
    fn grain_keys_default_to_identity_and_clamp() {
        let mut n = GradeNode::new(1);
        let t = Tick(0);
        assert_eq!([n.float_at("grain.strength", t), n.float_at("grain.size", t), n.float_at("grain.softness", t)], [0.0, 1.0, 0.5]);
        n.params.insert("grain.strength".into(), Param::new(ParamValue::Float(1e9)));
        n.params.insert("grain.size".into(), Param::new(ParamValue::Float(-3.0)));
        n.params.insert("grain.softness".into(), Param::new(ParamValue::Float(f64::NAN)));
        assert_eq!([n.float_at("grain.strength", t), n.float_at("grain.size", t), n.float_at("grain.softness", t)], [1.0, 1.0, 0.5]);
    }

    #[test]
    fn qualifier_is_off_in_old_nodes_and_its_values_clamp() {
        // a G1 / v16 node has no qualifier keys
        let mut old = Grade::with_node();
        old.nodes[0].params.insert("lift".into(), Param::new(ParamValue::Color([0.1, 0.0, 0.0, 0.0])));
        let g: Grade = serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
        assert!(g.nodes.len() == 1 && g.nodes.iter().all(|n| n.qualifier_at(Tick(0)).is_none()));
        let mut n = GradeNode::new(1);
        n.params.insert("qual.on".into(), Param::new(ParamValue::Float(1.0)));
        let q = n.qualifier_at(Tick(0)).unwrap();
        assert_eq!((q.hue, q.hue_width, q.sat, q.lum, q.soft, q.blur, q.invert, q.show), (0.0, 1.0, [0.0, 1.0], [0.0, 1.0], 0.05, 0.0, false, 0));
        n.params.insert("qual.show".into(), Param::new(ParamValue::Float(1e300)));
        n.params.insert("qual.soft".into(), Param::new(ParamValue::Float(-5.0)));
        n.params.insert("qual.invert".into(), Param::new(ParamValue::Float(f64::NAN)));
        let q = n.qualifier_at(Tick(0)).unwrap();
        assert_eq!((q.show, q.soft, q.invert), (2, 0.001, false));
        n.params.insert("qual.on".into(), Param::new(ParamValue::Float(0.4)));
        assert!(n.qualifier_at(Tick(0)).is_none());
    }

    #[test]
    fn active_nodes_honour_bypass_enabled_and_the_cap() {
        let mut g = Grade { nodes: (1..=10_000).map(GradeNode::new).collect(), bypass: false, ..Default::default() };
        g.nodes[0].enabled = false;
        assert_eq!(g.active_nodes().count(), MAX_NODES - 1);
        g.bypass = true;
        assert_eq!(g.active_nodes().count(), 0);
        let g = Grade { nodes: vec![GradeNode::new(u64::MAX)], bypass: false, ..Default::default() };
        assert_eq!(g.next_id(), 1);
        let g = Grade { nodes: vec![GradeNode::new(1), GradeNode::new(u64::MAX), GradeNode::new(2)], bypass: false, ..Default::default() };
        assert_eq!(g.next_id(), 3);
    }

    fn e(from: u64, fi: u8, to: u64, ti: u8) -> Edge {
        Edge::new(Port::new(from, fi), Port::new(to, ti))
    }

    fn three() -> Grade {
        Grade { nodes: (1..=3).map(GradeNode::new).collect(), ..Default::default() }
    }

    /// Fork C2: a serial grade (no edges) is the chain input → 1 → 2 → 3, output the last node,
    /// and saves exactly as before (no `edges` / `output` / `kind` keys).
    #[test]
    fn serial_grade_is_the_implicit_chain_and_saves_as_before() {
        let g = three();
        assert!(!g.is_graph());
        assert_eq!(g.implicit_edges(), vec![e(SOURCE, 0, 1, 0), e(1, 0, 2, 0), e(2, 0, 3, 0)]);
        assert_eq!(g.output_node(), Ok(3));
        let order: Vec<u64> = g.graph_order().unwrap().nodes().collect();
        assert_eq!(order, vec![SOURCE, 1, 2, 3]);
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(json, r#"{"nodes":[{"id":1,"enabled":true},{"id":2,"enabled":true},{"id":3,"enabled":true}]}"#);
        assert_eq!(Grade::default().output_node(), Ok(SOURCE));
        assert_eq!(Grade::default().graph_order().unwrap().nodes().collect::<Vec<_>>(), vec![SOURCE]);
    }

    /// Fork C2: a parallel graph (input feeding nodes 1 and 2, both into mixer 3) validates, round
    /// trips through serde and orders every node after its inputs.
    #[test]
    fn parallel_graph_round_trips_and_orders() {
        let mut g = three();
        g.nodes[2].kind = NodeKind::ParallelMixer;
        g.edges = vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)];
        assert_eq!(g.output_node(), Ok(3));
        let order = g.graph_order().unwrap();
        assert_eq!(order.nodes().collect::<Vec<_>>(), vec![SOURCE, 1, 2, 3]);
        assert_eq!(order.steps[3].inputs[..2], [Some(Port::new(1, 0)), Some(Port::new(2, 0))]);
        let back: Grade = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
        assert!(serde_json::to_string(&g).unwrap().contains(r#""kind":"ParallelMixer""#));
        // an outside node: node 2's key input from node 1's key output
        let mut o = three();
        o.make_explicit();
        o.edges.push(e(1, 1, 2, 1));
        assert!(o.graph_order().is_ok());
    }

    /// Fork C2: branching writes the chain out, and un-branching drops it again, so the file is
    /// the same as one never branched.
    #[test]
    fn make_explicit_and_simplify_are_inverse_on_a_chain() {
        let plain = three();
        let mut g = plain.clone();
        g.make_explicit();
        assert!(g.is_graph());
        assert_eq!(g.graph_order().unwrap().nodes().collect::<Vec<_>>(), vec![SOURCE, 1, 2, 3]);
        assert!(g.simplify());
        assert_eq!(g, plain);
        // a real branch is kept
        g.make_explicit();
        g.edges.push(e(1, 1, 2, 1));
        assert!(!g.simplify() && g.is_graph());
        // so is a mixer, or an output that isn't the last node
        let mut m = three();
        m.make_explicit();
        m.nodes[1].kind = NodeKind::LayerMixer;
        assert!(!m.simplify());
        let mut o = three();
        o.make_explicit();
        o.output = Some(2);
        assert!(!o.simplify());
    }

    /// Fork C2 hostile input: bad graphs report an error (the grade then renders bypassed), never
    /// panic: cycles, unknown nodes, bad ports, two outputs, node id 0, nodes past the cap.
    #[test]
    fn hostile_graphs_are_refused_not_panics() {
        let with = |edges: Vec<Edge>| Grade { edges, ..three() };
        assert!(matches!(with(vec![e(1, 0, 2, 0), e(2, 0, 1, 0), e(SOURCE, 0, 3, 0)]).graph_order(), Err(GraphError::Dag(DagError::Cycle(_)))));
        assert!(matches!(Grade { output: Some(3), ..with(vec![e(SOURCE, 0, 99, 0)]) }.graph_order(), Err(GraphError::Dag(DagError::UnknownNode(99)))));
        assert!(matches!(with(vec![e(SOURCE, 1, 1, 0), e(1, 0, 2, 0), e(2, 0, 3, 0)]).graph_order(), Err(GraphError::Dag(DagError::BadPort(_)))));
        assert!(matches!(
            with(vec![e(SOURCE, 0, 1, 0), e(1, 0, 2, 0), e(1, 0, 2, 0), e(2, 0, 3, 0)]).graph_order(),
            Err(GraphError::Dag(DagError::DuplicateInput(_)))
        ));
        assert!(matches!(with(vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0)]).graph_order(), Err(GraphError::AmbiguousOutput(_))));
        assert!(matches!(Grade { output: Some(42), ..three() }.graph_order(), Err(GraphError::Dag(DagError::NoOutput))));
        // node id 0 collides with the input: refused until renumbered
        let mut z = Grade { nodes: vec![GradeNode::new(0), GradeNode::new(5)], ..Default::default() };
        assert_eq!(z.graph_order(), Err(GraphError::SourceIdUsed));
        z.make_explicit();
        assert!(z.nodes.iter().all(|n| n.id != SOURCE));
        assert_eq!(z.graph_order().unwrap().nodes().collect::<Vec<_>>(), vec![SOURCE, 6, 5]);
        assert!(!z.renumber_source_id());
        // a mixer's ports are capped
        let mut m = three();
        m.nodes[2].kind = NodeKind::ParallelMixer;
        m.edges = vec![e(SOURCE, 0, 1, 0), e(1, 0, 3, MAX_MIX_INPUTS), e(SOURCE, 0, 2, 0), e(2, 0, 3, 0)];
        assert!(matches!(m.graph_order(), Err(GraphError::Dag(DagError::BadPort(_)))));
        // thousands of nodes and edges: only the first MAX_NODES take part, the edge cap holds
        let mut big = Grade { nodes: (1..=10_000).map(GradeNode::new).collect(), ..Default::default() };
        assert_eq!(big.graph_order().unwrap().steps.len(), MAX_NODES + 1);
        big.edges = (0..5_000u64).map(|i| e(i, 0, i + 1, 0)).collect();
        assert!(big.graph_order().is_err());
        // garbage kinds / edges in JSON fail the typed load instead of half-loading
        assert!(serde_json::from_str::<Grade>(r#"{"nodes":[{"id":1,"kind":"Teleport"}]}"#).is_err());
        assert!(serde_json::from_str::<Grade>(r#"{"edges":[{"from":{"node":-1},"to":{"node":1}}]}"#).is_err());
    }

    /// Fork C2b: stored canvas positions round-trip, hostile ones load as none or clamped, and a
    /// grade never dragged saves without any `pos` key.
    #[test]
    fn node_positions_round_trip_and_hostile_values_are_cleaned() {
        let mut g = three();
        assert!(!serde_json::to_string(&g).unwrap().contains("pos"));
        g.nodes[1].pos = Some([10.0, -20.0]);
        g.input_pos = Some([-50.0, 0.0]);
        let back: Grade = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
        let h: Grade = serde_json::from_str(r#"{"nodes":[{"id":1,"pos":[1e300,-1e300]},{"id":2,"pos":null}],"inputPos":3,"input_pos":[1e39,2]}"#).unwrap();
        assert_eq!(h.nodes[0].pos, Some([MAX_POS, -MAX_POS]));
        assert_eq!(h.nodes[1].pos, None);
        assert_eq!(h.input_pos, Some([MAX_POS, 2.0]));
        assert!(serde_json::from_str::<Grade>(r#"{"nodes":[{"id":1,"pos":"x"}]}"#).is_err());
        assert_eq!(clean_pos(f64::NAN, 0.0), None);
        assert_eq!(clean_pos(0.0, f64::INFINITY), None);
        // a non-finite value set in code reads as unplaced
        g.nodes[0].pos = Some([f32::NAN, 0.0]);
        assert_eq!(g.stored_pos(1), None);
        assert!(g.clear_positions() && !g.has_positions() && !g.clear_positions());
    }

    /// Fork C2b: the auto layout is unchanged for an undragged grade, keeps stored positions and
    /// moves unplaced nodes off them; new nodes get a position near their anchor.
    #[test]
    fn layout_keeps_stored_positions_and_avoids_them() {
        let mut g = three();
        let auto = g.layout();
        assert_eq!(auto, vec![(0, [0.0, 0.0]), (1, [LAYOUT_COLUMN, 0.0]), (2, [2.0 * LAYOUT_COLUMN, 0.0]), (3, [3.0 * LAYOUT_COLUMN, 0.0])]);
        // node 1 dragged onto node 2's automatic spot: node 2 moves down a row
        g.nodes[0].pos = Some([2.0 * LAYOUT_COLUMN, 0.0]);
        let l = g.layout();
        assert_eq!(l[1], (1, [2.0 * LAYOUT_COLUMN, 0.0]));
        assert_eq!(l[2], (2, [2.0 * LAYOUT_COLUMN, LAYOUT_ROW]));
        // a new node after node 3 (no stored position on 3) sits to its right
        g.nodes.push(GradeNode::new(4));
        g.place_new(&[4], Some(3));
        assert_eq!(g.nodes[3].pos, Some([4.0 * LAYOUT_COLUMN, 0.0]));
        // after node 1 (stored): to its right, below what is already there
        g.nodes.insert(1, GradeNode::new(5));
        g.place_new(&[5], Some(1));
        let p = g.nodes[1].pos.unwrap();
        assert_eq!(p[0], 3.0 * LAYOUT_COLUMN);
        assert!(g.layout().iter().filter(|(id, _)| *id != 5).all(|(_, q)| !overlaps(p, *q)), "{:?}", g.layout());
        // a grade without stored positions stays automatic
        let mut plain = three();
        plain.nodes.push(GradeNode::new(4));
        plain.place_new(&[4], Some(3));
        assert!(!plain.has_positions());
        // thousands of stacked nodes on one spot: bounded, no panic
        let mut big = Grade { nodes: (1..=10_000).map(|i| GradeNode { pos: Some([0.0, 0.0]), ..GradeNode::new(i) }).collect(), ..Default::default() };
        assert_eq!(big.layout().len(), MAX_NODES + 1);
        big.nodes.push(GradeNode::new(20_000));
        big.place_new(&[20_000], None);
        big.offset_positions(&[1, 2], [f32::MAX, f32::NAN]);
        assert_eq!(big.nodes[0].pos, None);
    }

    /// Fork C2: the graph parameters resolve per input, read as identity when missing and clamp.
    #[test]
    fn graph_params_resolve_per_input_and_clamp() {
        let t = Tick(0);
        for k in ["key.invert", "key.gain", "key.offset", "mix.gain.0", "mix.gain.15", "layer.mode.3", "layer.opacity.7", "kmix.mode.0", "kmix.invert.15"] {
            assert!(node_param(k).is_some(), "{k}");
        }
        assert!(node_param("mix.gain.16").is_none() && node_param("mix.gain.").is_none());
        let mut n = GradeNode::new(1);
        assert_eq!([n.float_at("key.gain", t), n.float_at("layer.opacity.2", t), n.float_at("kmix.mode.1", t)], [1.0, 1.0, 0.0]);
        n.params.insert("key.offset".into(), Param::new(ParamValue::Float(-9.0)));
        n.params.insert("layer.mode.1".into(), Param::new(ParamValue::Float(f64::NAN)));
        assert_eq!([n.float_at("key.offset", t), n.float_at("layer.mode.1", t)], [-1.0, 0.0]);
        assert!(NODE_PARAMS.iter().all(|p| GRAPH_PARAMS.iter().all(|g| g.key != p.key)));
    }
}
