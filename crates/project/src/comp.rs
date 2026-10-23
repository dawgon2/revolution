//! Compositions (C1): a node graph rendered as one picture, used on the timeline like a nested
//! sequence. The graph is a [`crate::dag`] graph; every node maps to maths that already exists
//! (blend modes, the Motion matrix, key / mask / blur effects, one grade node). See ADR 0007
//! (`docs/adr/0007-node-graph.md`) and `docs/fork/DESIGN-C1.md`.

use std::collections::BTreeMap;

use filmcraft_geom::Vec2;
use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};

use crate::ItemId;
use crate::dag::{self, DagError, DagNode, Edge, NodeId, Order};
use crate::effect::{EffectInstance, ParamDef, ch, col, find_effect, pt};
use crate::grade::{GradeNode, MAX_WINDOWS};
use crate::keyframe::{Param, ParamValue};
use crate::mask::{Mask, MaskPath, TrackMethod};
use crate::paint::Paint;
use crate::roto::Roto;

/// Largest comp side in pixels.
pub const MAX_SIDE: u32 = 16_384;
/// Largest comp area in pixels.
pub const MAX_PIXELS: u64 = 256 * 1024 * 1024;
/// Most parameters a node keeps.
pub const MAX_PARAMS: usize = 64;
/// Longest node label in characters.
pub const MAX_LABEL: usize = 64;
/// Longest text value in characters (text nodes).
pub const MAX_TEXT: usize = 4_096;
/// The key effects a Key node can use.
pub const KEY_EFFECTS: &[&str] = &["ultra_key", "luma_key"];

/// A composition: a node graph rendered as one picture of `width × height` at `rate`.
/// A project item (`ItemKind::Comp`), so one comp can be on the timeline many times.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comp {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub rate: FrameRate,
    #[serde(default)]
    pub duration: Tick,
    #[serde(default)]
    pub nodes: Vec<CompNode>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// The Media Out node (None: the comp is transparent).
    #[serde(default)]
    pub output: Option<NodeId>,
    /// Working / grading space of the Color nodes: the sequence's when the comp was made.
    #[serde(default = "default_color")]
    pub color: filmcraft_color::ColorPipeline,
}

fn default_color() -> filmcraft_color::ColorPipeline {
    filmcraft_color::ColorPipeline::REC709
}

/// One node of a comp.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompNode {
    pub id: NodeId,
    pub kind: CompKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bypass: bool,
    /// Keyframable parameters by key ([`params`] is the schema; missing = its default).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Param>,
    /// Canvas position (UI only, saved so the layout survives).
    #[serde(default)]
    pub pos: [f32; 2],
}

/// Where a Media In node's picture comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MediaInSource {
    /// A project item (media, subclip, nested sequence or another comp).
    Item { item: ItemId },
    /// Video track `track` of the sequence the comp clip sits in, rendered alone at the comp
    /// clip's time.
    Layer { track: u32 },
    /// Input `index` of the Group node this comp is the inside of (C4.3, [`group`]); transparent
    /// in a comp that isn't a group's inside, or when that input isn't connected.
    GroupInput { index: u32 },
}

/// What a Roto node outputs (C3.5b).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RotoOutput {
    /// The input with its alpha (and colour, premultiplied) multiplied by the shapes' coverage.
    #[default]
    Alpha,
    /// The coverage itself as an opaque white-on-black picture (the input is not read).
    Matte,
}

impl RotoOutput {
    pub const ALL: [RotoOutput; 2] = [RotoOutput::Alpha, RotoOutput::Matte];
    pub fn label(self) -> &'static str {
        match self {
            RotoOutput::Alpha => "Alpha",
            RotoOutput::Matte => "Matte",
        }
    }
    /// By key (`alpha` / `matte`, any case).
    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        Self::ALL.into_iter().find(|o| o.label().to_ascii_lowercase() == s)
    }
}

/// What a node does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum CompKind {
    /// A picture from the project or the timeline, `offset` later than comp time.
    MediaIn {
        source: MediaInSource,
        #[serde(default)]
        offset: Tick,
    },
    /// The comp's result (pass-through).
    MediaOut,
    /// Input 0 = background, input 1 = foreground; params `opacity`, `blend` (as Opacity).
    Merge,
    /// Motion's parameters: position, scale, scale width, uniform scale, rotation, anchor.
    Transform,
    /// A key effect (one of [`KEY_EFFECTS`]); params are that effect's.
    Key { effect: String },
    /// Alpha × the coverage of these masks (each with its own mode, feather and invert).
    Mask {
        #[serde(default)]
        masks: Vec<Mask>,
    },
    /// Gaussian Blur's parameters.
    Blur,
    /// One grade node (its parameters live in `node.params`, not the comp node's).
    Color { node: GradeNode },
    /// A graphic text layer (the text effect instance a graphic clip holds).
    Text { layer: Box<EffectInstance> },
    /// A flat colour; param `color`.
    Solid,
    /// Linear or radial between two colours; params `start`, `end` (0..1 of the frame),
    /// `color_a`, `color_b`, `shape`.
    Gradient,
    // -- C3.5b: roto and paint (crate::roto / crate::paint; coordinates in comp pixels)
    /// Animated roto shapes combined into one matte: cuts the input (`output` Alpha) or shows the
    /// matte (Matte). No enabled shape: Alpha passes the input through, Matte is black.
    Roto {
        #[serde(default)]
        roto: Roto,
        #[serde(default)]
        output: RotoOutput,
    },
    /// Paint strokes over input 0; input 1 (optional) is the Reveal tools' source.
    Paint {
        #[serde(default)]
        paint: Paint,
    },
    /// Point tracker (fork C3.5a): `points` tracked through input 0 (`comp.track.analyse`), and
    /// the motion they give applied as `operation` says. Input 0 = the picture (and what is
    /// analysed); input 1 = an optional foreground: when connected, Match Move / Corner Pin
    /// move it and merge it over input 0 (Stabilise always moves input 0).
    Tracker {
        #[serde(default)]
        points: Vec<TrackerPoint>,
        #[serde(default)]
        operation: TrackOperation,
        /// Stabilise only: frames of camera path smoothing (0 = lock the points in place).
        #[serde(default)]
        smooth_radius: u32,
        /// The comp time the solves are relative to (match move: where the foreground is placed;
        /// stabilise: the frame the picture is locked to).
        #[serde(default)]
        reference: Tick,
        #[serde(default)]
        method: TrackMethod,
        /// Corner Pin only (LO.5): the indices into `points` of the four points the corners go to
        /// (top left, top right, bottom right, bottom left). Empty: the first four analysed points.
        /// Anything but four distinct indices of existing points loads (and renders) as empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        corner_points: Vec<u32>,
    },
    /// Planar tracker (fork C3.5a): `region` (drawn at `reference`, comp pixels) tracked as a
    /// plane through input 0; `quad` (at `reference`) follows it as the keyframed `corners`.
    /// Input 0 = background (and what is analysed), input 1 = the insert (Corner Pin / Match Move).
    PlanarTracker {
        #[serde(default)]
        region: MaskPath,
        /// The corner-pin quad at `reference`, clockwise from the top left (comp pixels). All
        /// zero: the region's bounding box.
        #[serde(default)]
        quad: [Vec2; 4],
        #[serde(default)]
        reference: Tick,
        /// The analysed quad corners (four keyframed points, clockwise from the top left).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        corners: Vec<Param>,
        /// Confidence (0..1) of each analysed frame.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        confidence: Vec<(Tick, f32)>,
        #[serde(default = "corner_pin_op")]
        operation: TrackOperation,
    },
    /// A node group / macro (C4.3, ADR 0009 §2, [`group`]): an inner comp of the same size and rate
    /// whose `MediaIn { GroupInput { index } }` nodes read this node's inputs and whose output is
    /// this node's output. `exposed` maps parameters of this node (in its own `params`) onto inner
    /// nodes' parameters, which they override when the group is evaluated.
    Group {
        comp: Box<Comp>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exposed: Vec<ExposedParam>,
    },
}

pub mod group;
pub use group::{ExposedParam, MAX_EXPOSED, MAX_GROUP_DEPTH, MAX_GROUP_INPUTS};

/// What a Tracker or Planar Tracker node does with what it tracked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackOperation {
    /// The foreground (else the picture) follows the tracked motion from the reference frame.
    #[default]
    MatchMove,
    /// The picture moves so the tracked points (plane) stay where they are at the reference.
    Stabilise,
    /// The foreground's (else the picture's) corners pinned to four tracked points (Tracker:
    /// points 1-4, clockwise from the top left) or to the plane's corners (Planar Tracker).
    CornerPin,
}

impl TrackOperation {
    pub const ALL: [TrackOperation; 3] = [TrackOperation::MatchMove, TrackOperation::Stabilise, TrackOperation::CornerPin];
    /// The key commands use (`matchMove`, `stabilise`, `cornerPin`; `stabilize` is accepted).
    pub fn key(self) -> &'static str {
        match self {
            TrackOperation::MatchMove => "matchMove",
            TrackOperation::Stabilise => "stabilise",
            TrackOperation::CornerPin => "cornerPin",
        }
    }
    pub fn from_key(k: &str) -> Option<Self> {
        if k.eq_ignore_ascii_case("stabilize") {
            return Some(TrackOperation::Stabilise);
        }
        Self::ALL.into_iter().find(|o| o.key().eq_ignore_ascii_case(k))
    }
    pub fn label(self) -> &'static str {
        match self {
            TrackOperation::MatchMove => "Match Move",
            TrackOperation::Stabilise => "Stabilise",
            TrackOperation::CornerPin => "Corner Pin",
        }
    }
}

fn corner_pin_op() -> TrackOperation {
    TrackOperation::CornerPin
}

/// Most points a Tracker node holds.
pub const MAX_TRACK_POINTS: usize = 64;
/// Most keyframes (analysed frames) one track keeps.
pub const MAX_TRACK_KEYS: usize = 200_000;
/// Most vertices of a Planar Tracker region.
pub const MAX_REGION_POINTS: usize = 1024;
/// Largest Stabilise smoothing radius (frames).
pub const MAX_SMOOTH_RADIUS: u32 = 1000;
/// Pattern half-size range (comp pixels).
pub const PATTERN_HALF: (f64, f64) = (3.0, 64.0);
/// Search half-size range (comp pixels per frame).
pub const SEARCH_HALF: (f64, f64) = (1.0, 256.0);

/// One point of a Tracker node: a pattern box tracked inside a search box.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackerPoint {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Pattern box half-size (comp pixels).
    #[serde(default = "default_pattern")]
    pub pattern: Vec2,
    /// How far the pattern may move per frame (comp pixels each way).
    #[serde(default = "default_search")]
    pub search: Vec2,
    /// The point's path (comp pixels; a `Vec2` parameter). Its static value is where it was
    /// placed; analysis writes one keyframe per tracked frame.
    #[serde(default = "origin_param")]
    pub track: Param,
    /// Confidence (0..1) of each analysed frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confidence: Vec<(Tick, f32)>,
}

/// A Tracker's chosen Corner Pin points (LO.5) as indices, when they are four distinct indices
/// below `points`; None otherwise (then the first four analysed points are used).
pub fn valid_corner_points(corner_points: &[u32], points: usize) -> Option<[usize; 4]> {
    let [a, b, c, d] = corner_points else { return None };
    let four = [*a, *b, *c, *d].map(|i| usize::try_from(i).unwrap_or(usize::MAX));
    let distinct = (0..4).all(|i| (i + 1..4).all(|j| four.get(i) != four.get(j)));
    (distinct && four.iter().all(|&i| i < points)).then_some(four)
}

fn default_pattern() -> Vec2 {
    Vec2::new(16.0, 16.0)
}

fn default_search() -> Vec2 {
    Vec2::new(24.0, 24.0)
}

fn origin_param() -> Param {
    Param::new(ParamValue::Vec2(Vec2::ZERO))
}

impl TrackerPoint {
    /// A point placed at `at` (comp pixels) with the default boxes.
    pub fn at(name: impl Into<String>, at: Vec2) -> Self {
        Self { name: name.into(), pattern: default_pattern(), search: default_search(), track: Param::new(ParamValue::Vec2(at)), confidence: Vec::new() }
    }

    fn sanitize(&mut self) {
        if self.name.chars().count() > MAX_LABEL {
            self.name = self.name.chars().take(MAX_LABEL).collect();
        }
        let clamp = |v: &mut Vec2, (lo, hi): (f64, f64), d: Vec2| {
            *v = if v.x.is_finite() && v.y.is_finite() { Vec2::new(v.x.clamp(lo, hi), v.y.clamp(lo, hi)) } else { d };
        };
        clamp(&mut self.pattern, PATTERN_HALF, default_pattern());
        clamp(&mut self.search, SEARCH_HALF, default_search());
        sanitize_track(&mut self.track);
        self.confidence.truncate(MAX_TRACK_KEYS);
    }
}

/// A track parameter within its caps: a `Vec2` (else the origin), at most [`MAX_TRACK_KEYS`]
/// keyframes, those that aren't points dropped.
fn sanitize_track(p: &mut Param) {
    if !matches!(p.value, ParamValue::Vec2(_)) {
        p.value = ParamValue::Vec2(Vec2::ZERO);
    }
    p.keyframes.truncate(MAX_TRACK_KEYS);
    p.keyframes.retain(|k| matches!(k.value, ParamValue::Vec2(_)));
}

/// Keeps a comp time within a day either way, so adding a frame to it can't overflow.
fn clamp_day(t: &mut Tick) {
    let day = filmcraft_time::TICKS_PER_SECOND.saturating_mul(86_400);
    t.0 = t.0.clamp(-day, day);
}

/// Where a new comp's Media Out node sits on the canvas (x; canvas units).
pub const MEDIA_OUT_X: f32 = 640.0;

/// Every kind's key, as `comp.addNode` / `comp.params` name it.
pub const KINDS: &[&str] =
    &["mediaIn", "mediaOut", "merge", "transform", "key", "mask", "blur", "color", "text", "solid", "gradient", "roto", "paint", "tracker", "planarTracker"];

impl CompKind {
    /// The kind's key (one of [`KINDS`]).
    pub fn key(&self) -> &'static str {
        match self {
            CompKind::MediaIn { .. } => "mediaIn",
            CompKind::MediaOut => "mediaOut",
            CompKind::Merge => "merge",
            CompKind::Transform => "transform",
            CompKind::Key { .. } => "key",
            CompKind::Mask { .. } => "mask",
            CompKind::Blur => "blur",
            CompKind::Color { .. } => "color",
            CompKind::Text { .. } => "text",
            CompKind::Solid => "solid",
            CompKind::Gradient => "gradient",
            CompKind::Roto { .. } => "roto",
            CompKind::Paint { .. } => "paint",
            CompKind::Tracker { .. } => "tracker",
            CompKind::PlanarTracker { .. } => "planarTracker",
            CompKind::Group { .. } => "group",
        }
    }

    /// A new node of kind `key` with default contents (Media In: track 0; Key: Ultra Key; Text: an
    /// default text layer). None for an unknown key.
    pub fn from_key(key: &str) -> Option<CompKind> {
        Some(match key {
            "mediaIn" => CompKind::MediaIn { source: MediaInSource::Layer { track: 0 }, offset: Tick::ZERO },
            "mediaOut" => CompKind::MediaOut,
            "merge" => CompKind::Merge,
            "transform" => CompKind::Transform,
            "key" => CompKind::Key { effect: "ultra_key".into() },
            "mask" => CompKind::Mask { masks: Vec::new() },
            "blur" => CompKind::Blur,
            "color" => CompKind::Color { node: GradeNode::new(1) },
            "text" => CompKind::Text { layer: Box::new(find_effect(crate::graphic::TEXT_LAYER)?.instance()) },
            "solid" => CompKind::Solid,
            "gradient" => CompKind::Gradient,
            "roto" => CompKind::Roto { roto: Roto::default(), output: RotoOutput::Alpha },
            "paint" => CompKind::Paint { paint: Paint::default() },
            "tracker" => CompKind::Tracker {
                points: Vec::new(),
                operation: TrackOperation::MatchMove,
                smooth_radius: 0,
                reference: Tick::ZERO,
                method: TrackMethod::default(),
                corner_points: Vec::new(),
            },
            "planarTracker" => CompKind::PlanarTracker {
                region: MaskPath::default(),
                quad: [Vec2::ZERO; 4],
                reference: Tick::ZERO,
                corners: Vec::new(),
                confidence: Vec::new(),
                operation: TrackOperation::CornerPin,
            },
            _ => return None,
        })
    }

    /// Number of input ports.
    pub fn inputs(&self) -> u8 {
        match self {
            CompKind::MediaIn { .. } | CompKind::Text { .. } | CompKind::Solid | CompKind::Gradient => 0,
            CompKind::Merge | CompKind::Paint { .. } | CompKind::Tracker { .. } | CompKind::PlanarTracker { .. } => 2,
            CompKind::Roto { .. } => 1,
            CompKind::MediaOut | CompKind::Transform | CompKind::Key { .. } | CompKind::Mask { .. } | CompKind::Blur | CompKind::Color { .. } => 1,
            CompKind::Group { comp, .. } => group::input_count(comp),
        }
    }
}

impl DagNode for CompNode {
    fn id(&self) -> NodeId {
        self.id
    }
    fn inputs(&self) -> u8 {
        self.kind.inputs()
    }
    fn outputs(&self) -> u8 {
        1
    }
    fn bypassed(&self) -> bool {
        self.bypass
    }
}

/// The parameter schema of a node kind (Color nodes use `grade::NODE_PARAMS`, Text nodes their
/// layer's own parameters: both return an empty list here).
pub fn params(kind: &CompKind) -> Vec<ParamDef> {
    let from = |id: &str, skip: &[&str]| -> Vec<ParamDef> {
        find_effect(id).map(|d| d.params.iter().filter(|p| !skip.contains(&p.id)).cloned().collect()).unwrap_or_default()
    };
    match kind {
        CompKind::Merge => from("opacity", &[]),
        CompKind::Transform => from("motion", &["anti_flicker"]),
        CompKind::Key { effect } if KEY_EFFECTS.contains(&effect.as_str()) => from(effect, &[]),
        CompKind::Blur => from("gaussian_blur", &[]),
        CompKind::Solid => vec![col("color", "Color", [1.0, 1.0, 1.0, 1.0])],
        CompKind::Gradient => vec![
            pt("start", "Start", 0.0, 0.5),
            pt("end", "End", 1.0, 0.5),
            col("color_a", "Start Color", [0.0, 0.0, 0.0, 1.0]),
            col("color_b", "End Color", [1.0, 1.0, 1.0, 1.0]),
            ch("shape", "Shape", &["Linear", "Radial"], 0),
        ],
        _ => Vec::new(),
    }
}

impl CompNode {
    pub fn new(id: NodeId, kind: CompKind) -> Self {
        Self { id, kind, label: String::new(), bypass: false, params: BTreeMap::new(), pos: [0.0, 0.0] }
    }

    /// A parameter's value at comp time `t`: the stored one, else the schema default (LO.4: a
    /// Color node's are its grade's, a Text node's its layer's, see [`CompNode::param_store`]).
    pub fn value_at(&self, key: &str, t: Tick) -> Option<ParamValue> {
        match self.param_store().get(key) {
            Some(p) => Some(p.value_at(t)),
            None => self.param_default(key),
        }
    }

    /// Where the node keeps its parameters (LO.4): a Color node's grade node's settings, a Text
    /// node's layer settings, every other node's own `params`. Expressions, `value_at` and
    /// [`ExprFrame`] read parameters here.
    pub fn param_store(&self) -> &BTreeMap<String, Param> {
        match &self.kind {
            CompKind::Color { node } => &node.params,
            CompKind::Text { layer } => &layer.params,
            _ => &self.params,
        }
    }

    /// [`CompNode::param_store`] mutably.
    pub fn param_store_mut(&mut self) -> &mut BTreeMap<String, Param> {
        match &mut self.kind {
            CompKind::Color { node } => &mut node.params,
            CompKind::Text { layer } => &mut layer.params,
            _ => &mut self.params,
        }
    }

    /// Parameter `key`'s schema default: a grade setting's identity (number or wheel; None for a
    /// curve or an unknown key), a text layer setting's default, else the kind's schema.
    pub fn param_default(&self, key: &str) -> Option<ParamValue> {
        match &self.kind {
            CompKind::Color { .. } => match crate::grade::node_param(key).map(|p| p.kind)? {
                crate::grade::NodeParamKind::Float { identity, .. } => Some(ParamValue::Float(f64::from(identity))),
                crate::grade::NodeParamKind::Wheel { .. } => Some(ParamValue::Color([0.0; 4])),
                crate::grade::NodeParamKind::Curve => None,
            },
            CompKind::Text { layer } => layer.def().and_then(|d| d.param(key)).map(|d| d.default.clone()),
            kind => params(kind).into_iter().find(|d| d.id == key).map(|d| d.default),
        }
    }

    /// How many entries parameter `key` has when it is a menu (an expression's result is kept
    /// within them).
    fn param_choices(&self, key: &str) -> Option<usize> {
        let kind = match &self.kind {
            CompKind::Color { .. } => return None,
            CompKind::Text { layer } => layer.def().and_then(|d| d.param(key)).map(|d| d.kind.clone())?,
            kind => params(kind).into_iter().find(|d| d.id == key).map(|d| d.kind)?,
        };
        match kind {
            crate::effect::ParamKind::Choice(names) => Some(names.len()),
            _ => None,
        }
    }

    fn sanitize(&mut self) {
        if self.params.len() > MAX_PARAMS {
            self.params = std::mem::take(&mut self.params).into_iter().take(MAX_PARAMS).collect();
        }
        for p in self.params.values_mut() {
            cap_text(&mut p.value);
            for k in &mut p.keyframes {
                cap_text(&mut k.value);
            }
            p.cap_expression();
        }
        if self.label.chars().count() > MAX_LABEL {
            self.label = self.label.chars().take(MAX_LABEL).collect();
        }
        for v in &mut self.pos {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        match &mut self.kind {
            CompKind::Mask { masks } => masks.truncate(MAX_WINDOWS),
            CompKind::Color { node } => {
                node.windows.truncate(MAX_WINDOWS);
                if node.params.len() > MAX_PARAMS * 4 {
                    node.params = std::mem::take(&mut node.params).into_iter().take(MAX_PARAMS * 4).collect();
                }
                // (LO.4: grade settings take expressions)
                node.params.values_mut().for_each(Param::cap_expression);
            }
            CompKind::Text { layer } => {
                if layer.params.len() > MAX_PARAMS {
                    layer.params = std::mem::take(&mut layer.params).into_iter().take(MAX_PARAMS).collect();
                }
                for p in layer.params.values_mut() {
                    cap_text(&mut p.value);
                    for k in &mut p.keyframes {
                        cap_text(&mut k.value);
                    }
                    p.cap_expression();
                }
            }
            // offsets within a day either way, so adding them to a time can't overflow
            CompKind::MediaIn { offset, .. } => clamp_day(offset),
            CompKind::Tracker { points, smooth_radius, reference, corner_points, .. } => {
                points.truncate(MAX_TRACK_POINTS);
                points.iter_mut().for_each(TrackerPoint::sanitize);
                *smooth_radius = (*smooth_radius).min(MAX_SMOOTH_RADIUS);
                clamp_day(reference);
                if valid_corner_points(corner_points, points.len()).is_none() {
                    corner_points.clear();
                }
            }
            CompKind::PlanarTracker { region, quad, reference, corners, confidence, .. } => {
                region.vertices.truncate(MAX_REGION_POINTS);
                for v in &mut region.vertices {
                    for p in [&mut v.p, &mut v.t_in, &mut v.t_out] {
                        zero_if_not_finite(p);
                    }
                }
                quad.iter_mut().for_each(zero_if_not_finite);
                corners.truncate(4);
                corners.iter_mut().for_each(sanitize_track);
                confidence.truncate(MAX_TRACK_KEYS);
                clamp_day(reference);
            }
            CompKind::Roto { roto, .. } => roto.sanitize(),
            CompKind::Paint { paint } => paint.sanitize(),
            _ => {}
        }
    }
}

fn zero_if_not_finite(p: &mut Vec2) {
    if !(p.x.is_finite() && p.y.is_finite()) {
        *p = Vec2::ZERO;
    }
}

fn cap_text(v: &mut ParamValue) {
    if let ParamValue::Text(s) = v
        && s.chars().count() > MAX_TEXT
    {
        *s = s.chars().take(MAX_TEXT).collect();
    }
}

impl Comp {
    /// An empty comp with one Media Out node (id 1) as its output.
    pub fn new(width: u32, height: u32, rate: FrameRate, duration: Tick) -> Self {
        let mut c = Self {
            width,
            height,
            rate,
            duration,
            // Media Out on the right of the canvas, where the flow ends (new nodes start at 0, 0)
            nodes: vec![CompNode { pos: [MEDIA_OUT_X, 0.0], ..CompNode::new(1, CompKind::MediaOut) }],
            edges: Vec::new(),
            output: Some(1),
            color: default_color(),
        };
        c.sanitize();
        c
    }

    /// An id no node has yet (max + 1; 1 for an empty comp).
    pub fn next_id(&self) -> NodeId {
        self.nodes.iter().map(|n| n.id).max().unwrap_or(0).saturating_add(1)
    }

    pub fn node(&self, id: NodeId) -> Option<&CompNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut CompNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// The evaluation order of the nodes the output depends on (see [`dag::validate`]).
    pub fn validate(&self) -> Result<Order, DagError> {
        let out = self.output.ok_or(DagError::NoOutput)?;
        dag::validate(&self.nodes, &self.edges, out)
    }

    /// Brings a comp from a file within the caps: sizes clamped (1 ..= [`MAX_SIDE`], area ≤
    /// [`MAX_PIXELS`]), counts truncated, labels / texts shortened, non-finite canvas positions
    /// zeroed, a bad frame rate or negative duration reset. A cycle or a bad edge is left in place
    /// for [`Comp::validate`] to report (the comp then renders transparent).
    pub fn sanitize(&mut self) {
        let mut budget = dag::MAX_NODES;
        self.sanitize_at(0, &mut budget);
    }

    /// [`Comp::sanitize`] of a comp `depth` groups deep, its nodes and its groups' inner nodes
    /// taken from `budget` (C4.3: every level together holds at most [`dag::MAX_NODES`]).
    fn sanitize_at(&mut self, depth: usize, budget: &mut usize) {
        self.width = self.width.clamp(1, MAX_SIDE);
        self.height = self.height.clamp(1, MAX_SIDE);
        let max_h = MAX_PIXELS / u64::from(self.width);
        if u64::from(self.height) > max_h {
            self.height = u32::try_from(max_h).unwrap_or(MAX_SIDE).clamp(1, MAX_SIDE);
        }
        if self.rate.num <= 0 || self.rate.den <= 0 {
            self.rate = FrameRate::default();
        }
        if self.duration.0 < 0 {
            self.duration = Tick::ZERO;
        }
        self.nodes.truncate((*budget).min(dag::MAX_NODES));
        *budget = budget.saturating_sub(self.nodes.len());
        self.edges.truncate(dag::MAX_EDGES);
        for n in &mut self.nodes {
            n.sanitize();
        }
        group::sanitize_groups(self, depth, budget);
    }

    /// The project items Media In nodes read (for self-nesting checks and media management).
    /// (Groups' inner nodes included, C4.3.)
    pub fn sources(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.all_nodes().into_iter().filter_map(|n| match &n.kind {
            CompKind::MediaIn { source: MediaInSource::Item { item }, .. } => Some(*item),
            _ => None,
        })
    }

    /// The comp's frame size as a vector.
    pub fn size(&self) -> Vec2 {
        Vec2::new(f64::from(self.width), f64::from(self.height))
    }
}

// -- C4.2: parameter expressions (ADR 0009 §1)

/// Whether a parameter holding this value type can take an expression (not Text / Curve / Path).
pub fn expression_kind_allowed(v: &ParamValue) -> bool {
    crate::expr::kind_allowed(v)
}

/// Most expression evaluations one top-level parameter read makes (its own and every reference
/// not already known this frame).
pub const MAX_EXPR_EVALS: u32 = 256;

/// The expressions of one comp at one time: results already known (each parameter is evaluated
/// at most once per frame, however many others read it), the references being evaluated (for
/// cycles) and the evaluation budget of the current top-level read. Make one per frame with
/// [`Comp::expr_frame`] and read every parameter through it.
pub struct ExprFrame<'a> {
    comp: &'a Comp,
    t: Tick,
    stack: std::cell::RefCell<Vec<(NodeId, String)>>,
    memo: std::cell::RefCell<BTreeMap<(NodeId, String), ParamValue>>,
    budget: std::cell::Cell<u32>,
    /// The heights of the expressions being evaluated, added up (≤ [`crate::expr::MAX_CHAIN_HEIGHT`]).
    chain: std::cell::Cell<u32>,
    /// C4.3: the frame of the comp holding the group this comp is the inside of: a `node()`
    /// name this comp doesn't have is looked up there (ADR 0009 §2.2).
    parent: Option<&'a ExprFrame<'a>>,
}

/// Resolves `self.` / `node()` for the expression of one node's parameter.
struct NodeResolver<'r, 'a> {
    run: &'r ExprFrame<'a>,
    node: NodeId,
}

impl crate::expr::Resolver for NodeResolver<'_, '_> {
    fn param(&self, node: Option<&str>, key: &str, depth: u32) -> Result<crate::expr::Value, crate::expr::ExprError> {
        use crate::expr::ExprError;
        let id = match node {
            None => self.node,
            Some(r) => match (self.run.comp.find_node(r), self.run.parent) {
                (Some(id), _) => id,
                // C4.3: not in this group: the parent comp's node of that name
                (None, Some(parent)) => return NodeResolver { run: parent, node: self.node }.param(node, key, depth),
                (None, None) => return Err(ExprError::general(format!("no node called `{r}`"))),
            },
        };
        if depth >= crate::expr::MAX_REF_DEPTH {
            return Err(ExprError::general(format!("references are nested more than {} deep", crate::expr::MAX_REF_DEPTH)));
        }
        let name = self.run.comp.node(id).map(CompNode::display_name).unwrap_or_default();
        // (a nested error names the parameter it came from; its column is in that one's source)
        let v = self.run.param(id, key, depth.saturating_add(1)).map_err(|e| ExprError::general(format!("`{name}.{key}`: {}", e.message)))?;
        self.run.comp.expr_value(&v).ok_or_else(|| ExprError::general(format!("`{name}.{key}` is not a number, point or colour")))
    }
}

impl ExprFrame<'_> {
    /// Parameter `key` of node `node` with its expression applied; on failure the keyframed value
    /// and the error (see [`Comp::param_at`]).
    pub fn param_at(&self, node: NodeId, key: &str) -> (ParamValue, Option<crate::expr::ExprError>) {
        use crate::expr::ExprError;
        let Some(n) = self.comp.node(node) else {
            return (ParamValue::Float(0.0), Some(ExprError::general(format!("no node {node}"))));
        };
        let Some(base) = n.value_at(key, self.t) else {
            return (ParamValue::Float(0.0), Some(ExprError::general(format!("`{}` has no parameter `{key}`", n.display_name()))));
        };
        if n.expression(key).is_none() {
            return (base, None);
        }
        self.budget.set(MAX_EXPR_EVALS);
        match self.param(node, key, 0) {
            Ok(v) => (v, None),
            Err(e) => (base, Some(e)),
        }
    }

    /// A copy of node `id` whose expression parameters are replaced by their values (static, no
    /// keyframes, no expression), for a render that reads parameters straight from the node.
    /// None when the node has no expression (read it as it is).
    pub fn node(&self, id: NodeId) -> Option<CompNode> {
        let n = self.comp.node(id)?;
        let keys: Vec<String> = n.param_store().keys().filter(|k| n.expression(k).is_some()).cloned().collect();
        if keys.is_empty() {
            return None;
        }
        let mut out = n.clone();
        for k in keys {
            let (v, _) = self.param_at(id, &k);
            // (LO.4: a Color node's grade setting / a Text node's layer setting where it lives)
            out.param_store_mut().insert(k, Param::new(v));
        }
        Some(out)
    }

    /// Parameter `key` of node `id` with its expression applied (an error if it has none and no
    /// schema default, or its expression fails).
    fn param(&self, id: NodeId, key: &str, depth: u32) -> Result<ParamValue, crate::expr::ExprError> {
        use crate::expr::{Context, ExprError};
        let comp = self.comp;
        let node = comp.node(id).ok_or_else(|| ExprError::general(format!("no node {id}")))?;
        let name = node.display_name();
        let base = node.value_at(key, self.t).ok_or_else(|| ExprError::general(format!("`{name}` has no parameter `{key}`")))?;
        let Some(src) = node.expression(key) else {
            return Ok(base);
        };
        if !expression_kind_allowed(&base) {
            return Err(ExprError::general(format!("`{name}.{key}` can't take an expression")));
        }
        let slot = (id, key.to_string());
        if let Some(v) = self.memo.try_borrow().ok().and_then(|m| m.get(&slot).cloned()) {
            return Ok(v);
        }
        if self.stack.try_borrow().map_or(true, |s| s.contains(&slot)) {
            return Err(ExprError::general(format!("`{name}.{key}` reads itself (a reference cycle)")));
        }
        let left = self.budget.get();
        if left == 0 {
            return Err(ExprError::general("too many parameter references"));
        }
        self.budget.set(left - 1);
        let value = comp.expr_value(&base).ok_or_else(|| ExprError::general(format!("`{name}.{key}` can't take an expression")))?;
        let mut cx = Context::new(self.t, comp.rate, comp.width, comp.height, value);
        cx.depth = depth;
        if let Ok(mut s) = self.stack.try_borrow_mut() {
            s.push(slot.clone());
        }
        let used = self.chain.get();
        let r = crate::expr::parse_cached(src)
            .and_then(|e| {
                let total = used.saturating_add(e.height());
                if total > crate::expr::MAX_CHAIN_HEIGHT {
                    return Err(ExprError::general(format!(
                        "the references and the expressions they read are too deep together (more than {} steps)",
                        crate::expr::MAX_CHAIN_HEIGHT
                    )));
                }
                self.chain.set(total);
                let r = crate::expr::eval(&e, &cx, &NodeResolver { run: self, node: id });
                self.chain.set(used);
                r
            })
            .map_err(|e| e.at(src));
        if let Ok(mut s) = self.stack.try_borrow_mut() {
            s.pop();
        }
        let choices = node.param_choices(key);
        let mut out = crate::expr::to_param(r?, &base, choices).map_err(|e| ExprError::general(format!("`{name}.{key}`: {}", e.message)))?;
        // An "auto" (NaN) point the expression gave back as it read it stays auto; any other
        // result is a plain point (auto read as the comp centre, ADR 0009 §1.5).
        if let (ParamValue::Vec2(b), ParamValue::Vec2(_)) = (&base, &out)
            && (b.x.is_nan() || b.y.is_nan())
            && comp.expr_value(&out) == Some(value)
        {
            out = ParamValue::Vec2(*b);
        }
        if let Ok(mut m) = self.memo.try_borrow_mut() {
            m.insert(slot, out.clone());
        }
        Ok(out)
    }
}

impl CompNode {
    /// The label, else `node <id>` (for messages).
    pub fn display_name(&self) -> String {
        if self.label.is_empty() { format!("node {}", self.id) } else { self.label.clone() }
    }

    /// Parameter `key`'s expression, if it has a non-blank one (LO.4: in
    /// [`CompNode::param_store`]).
    pub fn expression(&self, key: &str) -> Option<&str> {
        self.param_store().get(key).and_then(|p| p.expression.as_deref()).filter(|s| !s.trim().is_empty())
    }
}

impl Comp {
    /// The expressions of this comp at `t`, sharing their results across every read.
    pub fn expr_frame(&self, t: Tick) -> ExprFrame<'_> {
        self.expr_frame_in(t, None)
    }

    /// [`Comp::expr_frame`] of a group's inside (C4.3): names it doesn't have are looked up in
    /// `parent`, the frame of the comp holding the group.
    pub fn expr_frame_in<'a>(&'a self, t: Tick, parent: Option<&'a ExprFrame<'a>>) -> ExprFrame<'a> {
        ExprFrame {
            parent,
            comp: self,
            t,
            stack: Default::default(),
            memo: Default::default(),
            budget: std::cell::Cell::new(MAX_EXPR_EVALS),
            chain: Default::default(),
        }
    }

    /// The node an expression's `node("…")` names: by label (exact, then ignoring case), else by
    /// number.
    pub fn find_node(&self, r: &str) -> Option<NodeId> {
        let r = r.trim();
        if r.is_empty() {
            return None;
        }
        self.nodes
            .iter()
            .find(|n| n.label == r)
            .or_else(|| self.nodes.iter().find(|n| !n.label.is_empty() && n.label.eq_ignore_ascii_case(r)))
            .map(|n| n.id)
            .or_else(|| r.parse::<NodeId>().ok().filter(|id| self.node(*id).is_some()))
    }

    /// A parameter value as an expression sees it. A point's "auto" (NaN) coordinate reads as
    /// the comp centre's: what auto means for every comp node (their inputs are comp-sized).
    fn expr_value(&self, v: &ParamValue) -> Option<crate::expr::Value> {
        if let ParamValue::Vec2(p) = v {
            let (cx, cy) = (f64::from(self.width) / 2.0, f64::from(self.height) / 2.0);
            let x = if p.x.is_finite() { p.x } else { cx };
            let y = if p.y.is_finite() { p.y } else { cy };
            return Some(crate::expr::Value::from_slice(&[x, y]));
        }
        crate::expr::from_param(v)
    }

    /// Parameter `key` of node `node` at comp time `t`: the keyframed value
    /// ([`CompNode::value_at`]) with the parameter's expression applied, if it has one. When the
    /// expression fails (parse, evaluation, a cycle, a missing reference), the keyframed value
    /// and the error. A missing node or parameter gives `Float(0)` and an error. To read several
    /// parameters at one time, share an [`ExprFrame`] ([`Comp::expr_frame`]).
    pub fn param_at(&self, node: NodeId, key: &str, t: Tick) -> (ParamValue, Option<crate::expr::ExprError>) {
        self.expr_frame(t).param_at(node, key)
    }

    /// A copy of node `id` with its expression parameters evaluated at `t` (see
    /// [`ExprFrame::node`]).
    pub fn node_with_expressions_at(&self, id: NodeId, t: Tick) -> Option<CompNode> {
        self.expr_frame(t).node(id)
    }

    /// Every node that has expressions, as [`ExprFrame::node`] copies evaluated at `t` through
    /// one shared frame.
    pub fn nodes_with_expressions_at(&self, t: Tick) -> BTreeMap<NodeId, CompNode> {
        let frame = self.expr_frame(t);
        self.nodes.iter().filter_map(|n| Some((n.id, frame.node(n.id)?))).collect()
    }

    /// Whether parameter `key` of node `node` can change with time: it has keyframes, or its
    /// expression reads `time` / `frame` / `wiggle`, or `value` of an animated parameter, or a
    /// parameter that can change with time (references followed up to
    /// [`crate::expr::MAX_REF_DEPTH`] deep; a cycle or an unparseable expression counts as not
    /// changing, since it renders as the keyframed value).
    pub fn param_is_time_varying(&self, node: NodeId, key: &str) -> bool {
        let mut seen = Vec::new();
        let mut budget = MAX_EXPR_EVALS;
        self.varying(node, key, 0, &mut seen, &mut budget)
    }

    fn varying(&self, id: NodeId, key: &str, depth: u32, seen: &mut Vec<(NodeId, String)>, budget: &mut u32) -> bool {
        use crate::expr::{ExprKind, Func, Name};
        let Some(p) = self.node(id).and_then(|n| n.param_store().get(key)) else { return false };
        if p.is_animated() {
            return true;
        }
        let Some(src) = p.expression.as_deref() else { return false };
        let slot = (id, key.to_string());
        if depth > crate::expr::MAX_REF_DEPTH || seen.contains(&slot) {
            return false;
        }
        // (out of budget: assume it changes, the safe answer for caches)
        if *budget == 0 {
            return true;
        }
        *budget -= 1;
        let Ok(e) = crate::expr::parse_cached(src) else { return false };
        seen.push(slot);
        let r = e.any(&mut |x| match &x.kind {
            ExprKind::Name(Name::Time | Name::Frame) | ExprKind::Call(Func::Wiggle, _) => true,
            ExprKind::Param { node, key, .. } => {
                let target = match node {
                    None => Some(id),
                    Some(r) => self.find_node(r),
                };
                target.is_some_and(|t| self.varying(t, key, depth.saturating_add(1), seen, budget))
            }
            _ => false,
        });
        seen.pop();
        r
    }

    /// Whether node `id`'s expressions (if any) give the same values at every time and depend on
    /// nothing outside the node itself (no other node, nor the comp's rate or size), so a cache
    /// keyed on the node stays right.
    pub fn node_expressions_static(&self, id: NodeId) -> bool {
        use crate::expr::{ExprKind, Name};
        let Some(n) = self.node(id) else { return true };
        n.param_store().iter().filter(|(_, p)| p.expression.is_some()).all(|(k, p)| {
            let reads_outside = p.expression.as_deref().and_then(|s| crate::expr::parse_cached(s).ok()).is_some_and(|e| {
                e.any(&mut |x| matches!(&x.kind, ExprKind::Param { node: Some(_), .. } | ExprKind::Name(Name::Fps | Name::Width | Name::Height)))
            });
            !reads_outside && !self.param_is_time_varying(id, k)
        })
    }
}

#[cfg(test)]
#[path = "comp_expr_tests.rs"]
mod expr_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::Port;

    fn edge(from: NodeId, to: NodeId, port: u8) -> Edge {
        Edge::new(Port::new(from, 0), Port::new(to, port))
    }

    /// Media In (item 7) → Key → Merge over a Solid → Color → Out.
    fn sample() -> Comp {
        let mut c = Comp::new(1920, 1080, FrameRate { num: 24, den: 1 }, Tick(1_000));
        c.nodes.push(CompNode::new(2, CompKind::MediaIn { source: MediaInSource::Item { item: ItemId(7) }, offset: Tick::ZERO }));
        c.nodes.push(CompNode::new(3, CompKind::from_key("key").unwrap()));
        c.nodes.push(CompNode::new(4, CompKind::Solid));
        c.nodes.push(CompNode::new(5, CompKind::Merge));
        c.nodes.push(CompNode::new(6, CompKind::from_key("color").unwrap()));
        c.edges = vec![edge(2, 3, 0), edge(4, 5, 0), edge(3, 5, 1), edge(5, 6, 0), edge(6, 1, 0)];
        c
    }

    #[test]
    fn sample_validates_in_dependency_order() {
        let c = sample();
        let order: Vec<NodeId> = c.validate().unwrap().nodes().collect();
        assert_eq!(order, vec![2, 3, 4, 5, 6, 1]);
        assert_eq!(c.next_id(), 7);
        assert_eq!(c.sources().collect::<Vec<_>>(), vec![ItemId(7)]);
    }

    #[test]
    fn serde_round_trip_and_tags() {
        let c = sample();
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""type":"mediaIn""#), "{json}");
        assert!(json.contains(r#""kind":"item""#), "{json}");
        let back: Comp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn minimal_json_loads_with_defaults() {
        let c: Comp = serde_json::from_str(r#"{"width":640,"height":360}"#).unwrap();
        assert!(c.nodes.is_empty());
        assert_eq!(c.output, None);
        assert_eq!(c.validate(), Err(DagError::NoOutput));
        let n: CompNode = serde_json::from_str(r#"{"id":3,"kind":{"type":"merge"}}"#).unwrap();
        assert_eq!(n.kind, CompKind::Merge);
        assert_eq!(n.pos, [0.0, 0.0]);
    }

    #[test]
    fn every_kind_key_round_trips() {
        for &k in KINDS {
            let kind = CompKind::from_key(k).unwrap();
            assert_eq!(kind.key(), k);
            let json = serde_json::to_string(&CompNode::new(1, kind)).unwrap();
            let back: CompNode = serde_json::from_str(&json).unwrap();
            // Compared as JSON: text layers hold NaN "auto" points, which never compare equal.
            assert_eq!(serde_json::to_string(&back).unwrap(), json, "{k}");
        }
        assert_eq!(CompKind::from_key("nope"), None);
    }

    #[test]
    fn port_counts() {
        assert_eq!(CompKind::Merge.inputs(), 2);
        assert_eq!(CompKind::Solid.inputs(), 0);
        assert_eq!(CompKind::MediaOut.inputs(), 1);
        assert_eq!(CompNode::new(1, CompKind::Blur).outputs(), 1);
    }

    #[test]
    fn param_schemas_come_from_the_effects_they_reuse() {
        let ids = |k: &CompKind| params(k).into_iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids(&CompKind::Merge), vec!["opacity", "blend"]);
        let t = ids(&CompKind::Transform);
        assert!(t.contains(&"position") && t.contains(&"rotation") && !t.contains(&"anti_flicker"));
        assert!(ids(&CompKind::Blur).contains(&"blurriness"));
        assert!(!ids(&CompKind::from_key("key").unwrap()).is_empty());
        assert!(ids(&CompKind::Key { effect: "not_a_key".into() }).is_empty());
        assert_eq!(ids(&CompKind::Gradient), vec!["start", "end", "color_a", "color_b", "shape"]);
        let n = CompNode::new(1, CompKind::Merge);
        assert_eq!(n.value_at("opacity", Tick::ZERO), Some(ParamValue::Float(100.0)));
        assert_eq!(n.value_at("nope", Tick::ZERO), None);
    }

    #[test]
    fn sanitize_holds_the_caps() {
        let mut c = sample();
        c.width = 0;
        c.height = u32::MAX;
        c.rate = FrameRate { num: 0, den: -1 };
        c.duration = Tick(-5);
        let mut n = CompNode::new(50, CompKind::Mask { masks: vec![Mask::linear("m", Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)); MAX_WINDOWS + 5] });
        n.label = "x".repeat(1000);
        n.pos = [f32::NAN, f32::INFINITY];
        for i in 0..(MAX_PARAMS + 10) {
            n.params.insert(format!("p{i}"), Param::new(ParamValue::Text("y".repeat(MAX_TEXT + 10))));
        }
        c.nodes.push(n);
        c.nodes.push(CompNode::new(51, CompKind::MediaIn { source: MediaInSource::Layer { track: 1 }, offset: Tick(i64::MAX) }));
        for i in 0..(dag::MAX_NODES as u64) {
            c.nodes.push(CompNode::new(100 + i, CompKind::Solid));
        }
        c.edges.extend(std::iter::repeat_n(edge(4, 5, 0), dag::MAX_EDGES));
        c.sanitize();
        assert_eq!((c.width, c.height), (1, MAX_SIDE));
        assert!(u64::from(c.width) * u64::from(c.height) <= MAX_PIXELS);
        assert_eq!(c.rate, FrameRate::default());
        assert_eq!(c.duration, Tick::ZERO);
        assert_eq!(c.nodes.len(), dag::MAX_NODES);
        assert_eq!(c.edges.len(), dag::MAX_EDGES);
        let n = c.node(50).unwrap();
        assert_eq!(n.label.chars().count(), MAX_LABEL);
        assert_eq!(n.pos, [0.0, 0.0]);
        assert_eq!(n.params.len(), MAX_PARAMS);
        assert!(n.params.values().all(|p| matches!(&p.value, ParamValue::Text(s) if s.chars().count() == MAX_TEXT)));
        assert!(matches!(&n.kind, CompKind::Mask { masks } if masks.len() == MAX_WINDOWS));
        assert!(matches!(c.node(51).unwrap().kind, CompKind::MediaIn { offset, .. } if offset.0 < i64::MAX / 2));
        // The duplicate edges are left for validate to report, not dropped.
        assert!(matches!(c.validate(), Err(DagError::DuplicateInput(_))));

        let mut wide = Comp::new(MAX_SIDE, MAX_SIDE, FrameRate::default(), Tick::ZERO);
        wide.sanitize();
        assert!(u64::from(wide.width) * u64::from(wide.height) <= MAX_PIXELS);
    }

    /// C3.5b: Roto / Paint nodes load from minimal JSON with defaults, round-trip, and are
    /// brought within the roto / paint caps by `Comp::sanitize`; comps saved before them load
    /// unchanged.
    #[test]
    fn roto_and_paint_nodes_default_round_trip_and_sanitize() {
        use crate::paint::{PaintPoint, PaintStroke, PaintTool};
        use crate::roto::{MAX_SHAPES, RotoKind, RotoShape};
        let n: CompNode = serde_json::from_str(r#"{"id":3,"kind":{"type":"roto"}}"#).unwrap();
        assert_eq!(n.kind, CompKind::Roto { roto: Roto::default(), output: RotoOutput::Alpha });
        assert_eq!(n.kind.inputs(), 1);
        let n: CompNode = serde_json::from_str(r#"{"id":4,"kind":{"type":"paint"}}"#).unwrap();
        assert_eq!(n.kind, CompKind::Paint { paint: Paint::default() });
        assert_eq!(n.kind.inputs(), 2);
        let n: CompNode = serde_json::from_str(r#"{"id":5,"kind":{"type":"roto","output":"matte"}}"#).unwrap();
        assert!(matches!(n.kind, CompKind::Roto { output: RotoOutput::Matte, .. }));
        assert_eq!(RotoOutput::from_name("MATTE"), Some(RotoOutput::Matte));
        assert_eq!(RotoOutput::from_name("x"), None);

        // an old comp (no roto / paint) is the same after a load
        let old = sample();
        let back: Comp = serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
        assert_eq!(back, old);

        let mut c = sample();
        let shape = RotoShape::new("s", RotoKind::Bezier, crate::mask::MaskPath::rect(0.0, 0.0, 10.0, 10.0));
        let roto = Roto { shapes: vec![shape; MAX_SHAPES + 3] };
        c.nodes.push(CompNode::new(20, CompKind::Roto { roto, output: RotoOutput::Matte }));
        let bad = PaintStroke {
            color: [f32::NAN, 2.0, 0.0, 9.0],
            points: vec![PaintPoint::new(f64::NAN, 1.0), PaintPoint::new(1.0, 1.0)],
            ..PaintStroke::new(PaintTool::Color, Vec::new(), crate::paint::Brush::default())
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Comp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        // (NaN doesn't survive JSON: it is written as null, so the hostile stroke is sanitized
        // in memory)
        c.nodes.push(CompNode::new(21, CompKind::Paint { paint: Paint { strokes: vec![bad] } }));
        let mut back = c;
        back.sanitize();
        assert!(matches!(&back.node(20).unwrap().kind, CompKind::Roto { roto, .. } if roto.shapes.len() == MAX_SHAPES));
        let CompKind::Paint { paint } = &back.node(21).unwrap().kind else { panic!("not paint") };
        assert_eq!(paint.strokes[0].points.len(), 1);
        assert_eq!(paint.strokes[0].color[0], 0.0);
        assert_eq!(paint.strokes[0].color[3], 1.0);
    }

    #[test]
    fn trackers_load_with_defaults_and_old_comps_are_unchanged() {
        // a comp saved before the trackers loads and saves exactly as before
        let old = sample();
        let json = serde_json::to_string(&old).unwrap();
        assert!(!json.contains("tracker") && !json.contains("planar"), "{json}");
        assert_eq!(serde_json::from_str::<Comp>(&json).unwrap(), old);
        // minimal tracker nodes take their defaults
        let n: CompNode = serde_json::from_str(r#"{"id":3,"kind":{"type":"tracker"}}"#).unwrap();
        assert_eq!(n.kind, CompKind::from_key("tracker").unwrap());
        assert_eq!(n.kind.inputs(), 2);
        let n: CompNode = serde_json::from_str(r#"{"id":3,"kind":{"type":"planarTracker"}}"#).unwrap();
        assert!(matches!(n.kind, CompKind::PlanarTracker { operation: TrackOperation::CornerPin, .. }));
        let p: TrackerPoint = serde_json::from_str("{}").unwrap();
        assert_eq!((p.pattern, p.search, p.track.value), (default_pattern(), default_search(), ParamValue::Vec2(Vec2::ZERO)));
        // a full tracker round-trips
        let mut pt = TrackerPoint::at("A", Vec2::new(10.0, 20.0));
        pt.track.put_keyframe(Tick(5), ParamValue::Vec2(Vec2::new(11.0, 21.0)));
        pt.confidence.push((Tick(5), 0.9));
        let kind = CompKind::Tracker {
            points: vec![pt],
            operation: TrackOperation::Stabilise,
            smooth_radius: 8,
            reference: Tick(5),
            method: TrackMethod::Position,
            corner_points: Vec::new(),
        };
        let back: CompKind = serde_json::from_str(&serde_json::to_string(&kind).unwrap()).unwrap();
        assert_eq!(back, kind);
        for op in TrackOperation::ALL {
            assert_eq!(TrackOperation::from_key(op.key()), Some(op));
        }
        assert_eq!(TrackOperation::from_key("stabilize"), Some(TrackOperation::Stabilise));
        assert_eq!(TrackOperation::from_key("warp"), None);
    }

    #[test]
    fn tracker_sanitize_holds_the_caps() {
        let mut pt = TrackerPoint::at("x".repeat(500), Vec2::ZERO);
        pt.pattern = Vec2::new(f64::NAN, 1.0);
        pt.search = Vec2::new(1e9, -5.0);
        pt.track.value = ParamValue::Float(3.0);
        pt.track.keyframes = (0..(MAX_TRACK_KEYS + 3) as i64).map(|i| crate::keyframe::Keyframe::new(Tick(i), ParamValue::Vec2(Vec2::ZERO))).collect();
        pt.track.keyframes.push(crate::keyframe::Keyframe::new(Tick(-1), ParamValue::Float(1.0)));
        let mut n = CompNode::new(
            2,
            CompKind::Tracker {
                points: vec![pt; MAX_TRACK_POINTS + 2],
                operation: TrackOperation::MatchMove,
                smooth_radius: u32::MAX,
                reference: Tick(i64::MIN),
                method: TrackMethod::Position,
                corner_points: vec![0, 1, 2, u32::MAX],
            },
        );
        n.sanitize();
        let CompKind::Tracker { points, smooth_radius, reference, corner_points, .. } = &n.kind else { panic!() };
        assert_eq!((points.len(), *smooth_radius), (MAX_TRACK_POINTS, MAX_SMOOTH_RADIUS));
        assert!(corner_points.is_empty(), "an index past the points is dropped");
        assert!(reference.0 > i64::MIN / 2);
        let p = &points[0];
        assert_eq!(p.name.chars().count(), MAX_LABEL);
        assert_eq!(p.pattern, default_pattern());
        assert_eq!(p.search, Vec2::new(SEARCH_HALF.1, SEARCH_HALF.0));
        assert_eq!(p.track.value, ParamValue::Vec2(Vec2::ZERO));
        assert_eq!(p.track.keyframes.len(), MAX_TRACK_KEYS);

        let path = MaskPath { vertices: vec![crate::mask::MaskVertex::corner(Vec2::new(f64::NAN, 0.0)); MAX_REGION_POINTS + 9], closed: true };
        let mut n = CompNode::new(
            3,
            CompKind::PlanarTracker {
                region: path,
                quad: [Vec2::new(f64::INFINITY, 1.0); 4],
                reference: Tick(i64::MAX),
                corners: vec![Param::new(ParamValue::Float(1.0)); 9],
                confidence: vec![(Tick(0), 1.0); MAX_TRACK_KEYS + 1],
                operation: TrackOperation::CornerPin,
            },
        );
        n.sanitize();
        let CompKind::PlanarTracker { region, quad, corners, confidence, .. } = &n.kind else { panic!() };
        assert_eq!(region.vertices.len(), MAX_REGION_POINTS);
        assert!(region.vertices.iter().all(|v| v.p == Vec2::ZERO));
        assert_eq!(*quad, [Vec2::ZERO; 4]);
        assert_eq!(corners.len(), 4);
        assert!(corners.iter().all(|c| c.value == ParamValue::Vec2(Vec2::ZERO)));
        assert_eq!(confidence.len(), MAX_TRACK_KEYS);
    }

    #[test]
    fn hostile_json_never_panics() {
        for json in [
            r#"{"width":-1,"height":2}"#,
            r#"{"width":1,"height":1,"nodes":[{"id":1,"kind":{"type":"merge"},"pos":[1e40,0]}]}"#,
            r#"{"width":1,"height":1,"nodes":[{"id":1,"kind":{"type":"warp"}}]}"#,
            r#"{"width":1,"height":1,"edges":[{"from":{"node":1,"index":300},"to":{"node":1}}]}"#,
            r#"{"width":4,"height":4,"output":1,"nodes":[{"id":1,"kind":{"type":"mediaOut"}}],"edges":[{"from":{"node":1},"to":{"node":1}}]}"#,
        ] {
            if let Ok(mut c) = serde_json::from_str::<Comp>(json) {
                c.sanitize();
                let _ = c.validate();
            }
        }
    }

    /// LO.5: the chosen Corner Pin points are kept only as four distinct existing indices; old
    /// saves (no field) load as empty and save without it.
    #[test]
    fn corner_points_validate_and_old_saves_keep_their_shape() {
        assert_eq!(valid_corner_points(&[3, 0, 1, 2], 4), Some([3, 0, 1, 2]));
        for bad in [&[0, 1, 2][..], &[0, 1, 2, 2], &[0, 1, 2, 4], &[0, 1, 2, 3, 4], &[]] {
            assert_eq!(valid_corner_points(bad, 4), None, "{bad:?}");
        }
        let old = serde_json::json!({"type": "tracker", "points": [], "operation": "cornerPin", "smooth_radius": 0, "reference": 0, "method": "Position"});
        let k: CompKind = serde_json::from_value(old.clone()).unwrap();
        let CompKind::Tracker { corner_points, .. } = &k else { panic!() };
        assert!(corner_points.is_empty());
        assert!(serde_json::to_value(&k).unwrap().get("corner_points").is_none(), "saved without the field");
    }
}
