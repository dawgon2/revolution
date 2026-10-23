//! A clip's colour grade compiled to colour operations (fork ADR 0004).
//!
//! The grade runs between the input transform and the effect stack, on the same grading signal
//! as Lumetri ([`GradeSpace`]: DaVinci Intermediate in a DWG / DI sequence). Each node's
//! parameters become existing [`ColorOp`]s (film grain included: [`ColorOp::Grain`], a
//! positional operation like the vignette); nothing here is new grading maths. Identity
//! parameters emit nothing, and a grade that changes nothing compiles to an empty chain, so it
//! renders bit-exactly like no grade at all.
//!
//! **Node graphs (fork C2, `docs/fork/DESIGN-C2.md` §2).** A grade without edges, or whose graph
//! is a plain chain, compiles exactly as the serial grade always did. A branched grade compiles to
//! one [`FxOp::GradeGraph`]: a [`GradeProgram`] of passes on texture slots (`graph.rs`).

use std::sync::Arc;

use filmcraft_color::ops::{ColorOp, GRAIN_MAX_SIZE, Table, Tf};
use filmcraft_color::{GradeSpace, WorkingSpace};
use filmcraft_project::TrackItem;
use filmcraft_project::grade::{Grade, GradeNode, MAX_WINDOWS, NodeKind, Qualifier};

use crate::gpufx::FxOp;
use filmcraft_time::Tick;

mod graph;
#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod hdr_tests;

pub use graph::{GradePass, GradeProgram, GradeProgramError, KeyMode, execute_graph_cpu};

/// Luma weights of the saturation control (Lumetri's).
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// HDR White of the grading signal in HDR working spaces (Lumetri's default).
const HDR_WHITE_NITS: f32 = 1000.0;

/// Samples of a compiled tone curve (Lumetri's).
const CURVE_SAMPLES: usize = 1024;

/// Where a grade renders, for its positional operations (film grain).
#[derive(Clone, Copy, Debug)]
pub struct GradeCtx<'a> {
    /// Image pixels per full-resolution pixel. "Full resolution" is the clip's source pixels
    /// (`px_scale` of the effect stage: the working image's width over the source width, also
    /// when a large source is graded at the size drawn, `plan.rs`; `FILMCRAFT_FULL_SIZE_GRADE`
    /// keeps the source size, scale 1). For an adjustment layer: sequence pixels × render scale.
    pub px_scale: f32,
    /// Sequence time of the frame (freeze frames and holds still get a new grain every frame).
    pub seq_time: Tick,
    /// The track item (clip id): two stacked clips of one source don't share a grain pattern.
    pub clip: u64,
    /// The project, for node LUTs from its LUT library (`lib:` references; None: built-ins only).
    pub project: Option<&'a filmcraft_project::Project>,
    /// Magic Mask mattes for this frame by node id (fork G3; refined, any size: stretched over
    /// the frame). A node with one is limited to it (and to its windows, if any).
    pub mattes: Option<&'a NodeMattes>,
}

/// This frame's Magic Mask mattes by grade node id.
#[derive(Clone, Debug, Default)]
pub struct NodeMattes(pub std::collections::BTreeMap<u64, std::sync::Arc<crate::matte::BitmapMatte>>);

impl Default for GradeCtx<'_> {
    fn default() -> Self {
        Self { px_scale: 1.0, seq_time: Tick(0), clip: 0, project: None, mattes: None }
    }
}

/// `item`'s grade at clip time `t` (sequence time `seq_t`) as steps on working linear light, for
/// a `w`×`h` image of `px_scale` pixels per source pixel (empty: no grade, bypassed, or nothing
/// that changes the picture). See [`compile_fx`]. `mattes`: this frame's Magic Mask mattes in
/// the `w`×`h` image ([`crate::matte::layer_mattes`]); a node with one is limited to it.
#[allow(clippy::too_many_arguments)]
pub fn item_fx(
    project: &filmcraft_project::Project,
    item: &TrackItem,
    t: Tick,
    seq_t: Tick,
    working: WorkingSpace,
    px_scale: f32,
    w: usize,
    h: usize,
    mattes: Option<&NodeMattes>,
) -> Vec<FxOp> {
    let ctx = GradeCtx { px_scale, seq_time: seq_t, clip: item.id.0, project: Some(project), mattes };
    let mut out = item.grade.as_ref().map(|g| compile_fx(g, t, working, ctx, w, h)).unwrap_or_default();
    // the clip's temporal NR (fork 6.7b): before everything, on the whole picture (where v1 put
    // the first active node's); renders without temporal access strip it. Unlike v1 it survives a
    // grade dropped for a non-finite step (only hostile values do that), and it never runs on a
    // solid generator (v1 ran it there for nothing)
    if is_media(project, item)
        && let Some(op) = temporal(item, t)
    {
        out.insert(0, op);
    }
    out
}

/// True when `item`'s grade changes the picture at clip time `t` (an empty, identity, bypassed or
/// disabled grade doesn't, so the clip keeps every shortcut it has without one: a title stays
/// vector-sharp, a nested sequence keeps drawing its own layers). Cheap: reads the parameters,
/// builds no operations or curve tables. (A hostile grade whose chain [`compile_at`] drops as
/// non-finite still counts: the clip then takes the effect path with an empty grade.)
///
/// The clip's temporal noise reduction counts only in a render with temporal access (`temporal`,
/// [`crate::RenderOptions::temporal`]) and on a media / subclip item: elsewhere the op is left out,
/// so a clip with only temporal NR keeps its plain fast paths (playback, graphics, nests).
pub fn changes_picture(project: &filmcraft_project::Project, item: &TrackItem, t: Tick, temporal_access: bool) -> bool {
    item.grade.as_ref().is_some_and(|g| grade_changes(g, t, Some(project))) || (temporal_access && is_media(project, item) && temporal(item, t).is_some())
}

/// Whether `item` shows media (a media clip or a subclip): the items temporal NR runs on.
pub fn is_media(project: &filmcraft_project::Project, item: &TrackItem) -> bool {
    takes_temporal_nr(project, item.item)
}

/// Whether project item `id` takes a clip temporal NR (fork 6.7b; the one rule the renderer and
/// the `tnr.*` commands share): a media item that isn't a solid generator (Color Matte, Bars…;
/// demo footage counts as media), or a subclip of one (one level, never recursing).
pub fn takes_temporal_nr(project: &filmcraft_project::Project, id: filmcraft_project::ItemId) -> bool {
    use filmcraft_project::{ItemKind, MediaRef};
    let footage = |kind: &ItemKind| match kind {
        ItemKind::Media(m) => !matches!(&m.media, MediaRef::Generator(g) if !matches!(g, filmcraft_media::Generator::Demo(_))),
        _ => false,
    };
    match project.item(id).map(|p| &p.kind) {
        Some(ItemKind::Subclip { parent, .. }) => project.item(*parent).is_some_and(|p| footage(&p.kind)),
        Some(kind) => footage(kind),
        None => false,
    }
}

/// The clip's temporal noise reduction (fork 6.7b, `TrackItem::temporal_nr`) at clip time `t`
/// (None: absent, switched off or both strengths 0; the op's `inputs` are None: the renderer
/// attaches the neighbouring frames, `crate::temporal`). Every value is clamped by the schema
/// (NaN / mistyped: identity); `frames` is rounded to 1 or 2.
pub fn temporal(item: &TrackItem, t: Tick) -> Option<FxOp> {
    let n = item.temporal_nr.as_ref().filter(|n| n.enabled)?;
    let (luma, chroma) = (n.float_at("luma", t), n.float_at("chroma", t));
    let radius = n.float_at("frames", t).round().clamp(1.0, f32::from(crate::tnr::MAX_RADIUS)) as u8;
    (luma > 0.0 || chroma > 0.0).then(|| FxOp::TemporalDenoise { luma, chroma, radius, motion: n.float_at("motion", t), inputs: None })
}

/// Whether `item` asks for temporal noise reduction at clip time `t` (see [`temporal`]).
pub fn uses_temporal(item: &TrackItem, t: Tick) -> bool {
    item.enabled && temporal(item, t).is_some()
}

/// Whether `item` asks for temporal noise reduction anywhere: at the clip's start or at any
/// keyframe of its strengths (the render bar marks such a clip for rendering: playback never
/// shows temporal NR, the render cache does).
pub fn ever_temporal(project: &filmcraft_project::Project, item: &TrackItem) -> bool {
    if !is_media(project, item) || !item.enabled {
        return false;
    }
    let Some(n) = item.temporal_nr.as_ref() else { return false };
    let mut times = vec![item.effect_time_at(item.start)];
    for k in ["luma", "chroma"] {
        if let Some(p) = n.params.get(k) {
            times.extend(p.keyframes.iter().map(|kf| kf.time));
        }
    }
    times.into_iter().any(|t| temporal(item, t).is_some())
}

/// Whether the frame of sequence `seq_id` at `t` shows a media clip with temporal noise
/// reduction (also inside nested sequences, as deep as they render). The Program monitor asks for
/// a temporal render of its paused frame only then, so other frames keep one cache entry.
pub fn temporal_at(project: &filmcraft_project::Project, seq_id: filmcraft_project::ItemId, t: Tick) -> bool {
    fn walk(project: &filmcraft_project::Project, seq: &filmcraft_project::Sequence, t: Tick, depth: u32) -> bool {
        seq.video_tracks.iter().filter(|tr| tr.enabled).any(|tr| {
            tr.items.iter().filter(|it| it.enabled && it.start <= t && t < it.end()).any(|it| match project.item(it.item).map(|p| &p.kind) {
                Some(filmcraft_project::ItemKind::Media(_) | filmcraft_project::ItemKind::Subclip { .. }) => {
                    is_media(project, it) && uses_temporal(it, it.effect_time_at(t))
                }
                Some(filmcraft_project::ItemKind::Sequence(nested)) if depth < crate::MAX_NEST_DEPTH => walk(project, nested, it.source_time_at(t), depth + 1),
                _ => false,
            })
        })
    }
    project.sequence(seq_id).is_some_and(|seq| walk(project, seq, t, 0))
}

/// Remove every [`FxOp::TemporalDenoise`] from `ops` (also inside masked steps): a render without
/// temporal access (playback, scrubbing, adjustment layers, nested / graphic clips).
pub fn strip_temporal(ops: &mut Vec<FxOp>) {
    ops.retain(|o| !matches!(o, FxOp::TemporalDenoise { .. }));
    for o in ops.iter_mut() {
        if let FxOp::Masked { ops: inner, .. } | FxOp::MaskedBy { ops: inner, .. } = o {
            strip_temporal(inner);
        }
    }
}

/// Whether `g` changes the picture at `t`: a node of a serial grade (or a chain) does, and a
/// branched graph that isn't bypassed always counts (its mixers change the picture by themselves).
fn grade_changes(g: &Grade, t: Tick, project: Option<&filmcraft_project::Project>) -> bool {
    match shape(g) {
        Shape::Serial(nodes) => nodes.into_iter().any(|n| node_changes(n, t, project)),
        // (a graph refused for its slots only shows up when compiled; it then renders ungraded)
        Shape::Graph(_) => true,
        Shape::Broken(_) => false,
    }
}

/// Whether [`node_ops`] emits anything for `n` at `t` (the same conditions).
fn node_changes(n: &GradeNode, t: Tick, project: Option<&filmcraft_project::Project>) -> bool {
    let q = n.qualifier_at(t);
    if q.is_some_and(|q| q.show > 0) || n.float_at("nr.luma", t) > 0.0 || n.float_at("nr.chroma", t) > 0.0 {
        return true;
    }
    n.float_at("temperature", t) != 0.0
        || n.float_at("tint", t) != 0.0
        || ["lift", "gamma", "gain", "offset"].iter().any(|k| {
            let w = n.wheel_at(k, t);
            [w[0] + w[3], w[1] + w[3], w[2] + w[3]].iter().any(|&v| v != 0.0)
        })
        || n.float_at("contrast", t) != 1.0
        || n.float_at("saturation", t) != 1.0
        || CURVES.iter().any(|(k, _)| n.curve(k).is_some())
        || n.float_at("grain.strength", t) > 0.0
        || n.float_at("hl.rolloff", t) > 0.0
        || n.hdr_at(t).is_some()
        || n.warper.as_ref().is_some_and(|w| !w.is_identity())
        || despill_op(n, t).is_some()
        || (!n.lut.is_empty() && n.float_at("lut.mix", t) > 0.0 && crate::luts::resolve(project, &n.lut).is_some())
}

const CURVES: [(&str, [bool; 3]); 4] =
    [("curve.master", [true; 3]), ("curve.r", [true, false, false]), ("curve.g", [false, true, false]), ("curve.b", [false, false, true])];

/// [`compile_at`] for a full-resolution image.
pub fn compile(grade: &Grade, t: Tick, working: WorkingSpace) -> Vec<ColorOp> {
    compile_at(grade, t, working, GradeCtx::default())
}

/// `grade` at clip time `t` as a chain on working linear light: the grading signal's encode, every
/// enabled node's operations in order, the decode. Empty when no node changes the picture.
/// `ctx.px_scale` scales positional operations (grain size), so a reduced-resolution preview
/// shows the grain the full-size render has.
///
/// Film grain is a positional colour operation ([`ColorOp::Grain`]), so a node's grain sits at the
/// node's place in the chain, on the grading signal (later nodes grade the grain too, as in a
/// node grader), and the whole grade stays one fused per-pixel pass.
///
/// A qualified node's matte is unblurred here (the per-pixel form) and windows are ignored;
/// [`compile_fx`] blurs the matte and applies the windows.
///
/// A branched grade (fork C2) has no per-pixel form: it compiles to nothing here unless its graph
/// is a plain chain; [`compile_fx`] runs it.
pub fn compile_at(grade: &Grade, t: Tick, working: WorkingSpace, ctx: GradeCtx) -> Vec<ColorOp> {
    let gs = GradeSpace::new(working, HDR_WHITE_NITS);
    let Shape::Serial(nodes) = shape(grade) else { return Vec::new() };
    let body: Vec<ColorOp> = parts(nodes, t, &gs, ctx).into_iter().flat_map(|(_, p)| p.into_signal_ops()).collect();
    if body.is_empty() {
        return body;
    }
    let mut ops = gs.encode_ops();
    ops.extend(body);
    ops.extend(gs.decode_ops());
    // (every value is clamped to its schema range, but a chain that still isn't finite is dropped:
    // a grade never poisons the picture)
    if ops.iter().all(ColorOp::is_finite) { ops } else { Vec::new() }
}

/// `grade` at clip time `t` as image steps on working linear light for a `w`×`h` image: runs of
/// per-pixel nodes as one fused [`FxOp::ColorChain`] (exactly [`compile_at`]'s chain when no
/// qualifier blurs its matte), a qualified node with a blurred matte as an
/// [`FxOp::HslSecondary`] (key on the node's input, blur, the node's correction mixed in by the
/// matte), a node with windows as an [`FxOp::Masked`] around its steps (the window coverage times
/// the qualifier's matte). Empty when no node changes the picture or a step isn't finite.
///
/// A branched grade (fork C2) whose graph isn't a plain chain compiles to one
/// [`FxOp::GradeGraph`]; a graph that can't render (a cycle, two candidate outputs, more than
/// [`crate::comp::MAX_LIVE_TEXTURES`] pictures at once, a step that isn't finite) to nothing.
pub fn compile_fx(grade: &Grade, t: Tick, working: WorkingSpace, ctx: GradeCtx, w: usize, h: usize) -> Vec<FxOp> {
    let order = match shape(grade) {
        Shape::Serial(nodes) => {
            let gs = GradeSpace::new(working, HDR_WHITE_NITS);
            let out = serial_steps(parts(nodes, t, &gs, ctx), t, &gs, ctx, w, h, false);
            return if out.iter().all(step_finite) { out } else { Vec::new() };
        }
        Shape::Graph(order) => order,
        Shape::Broken(e) => {
            log::debug!("grade graph not rendered: {e}");
            return Vec::new();
        }
    };
    match graph::compile(grade, &order, t, working, ctx, w, h) {
        Ok(p) => vec![FxOp::GradeGraph(Arc::new(p))],
        Err(e) => {
            log::debug!("grade graph not rendered: {e}");
            Vec::new()
        }
    }
}

/// How a grade compiles (its graph checked once).
enum Shape<'a> {
    /// A serial chain: these nodes in order (a grade without edges: its active correction nodes;
    /// a node of another kind, only in a hostile file, is skipped as the identity; a bypassed
    /// grade: none; a graph that is a plain chain of correction nodes).
    Serial(Vec<&'a GradeNode>),
    /// A real graph, in evaluation order.
    Graph(filmcraft_project::dag::Order),
    /// A graph that can't render (the grade renders ungraded).
    Broken(filmcraft_project::grade::GraphError),
}

fn shape(grade: &Grade) -> Shape<'_> {
    // (a grade without edges naming an output, only in a hostile file, stops at that node, as
    // its graph does)
    if grade.bypass || (!grade.is_graph() && grade.output.is_none()) {
        return Shape::Serial(grade.active_nodes().filter(|n| n.kind == NodeKind::Correction).collect());
    }
    match graph::plan(grade) {
        Ok(graph::Plan::Chain(nodes)) => Shape::Serial(nodes),
        Ok(graph::Plan::Graph(order)) => Shape::Graph(order),
        Err(e) => Shape::Broken(e),
    }
}

/// The image steps of `parts` in order (see [`compile_fx`]). `signal`: the steps run on the
/// grading signal (a graph's correction node, fork C2), so they don't encode / decode it
/// themselves, and noise reduction (which reads working linear light) is wrapped in a decode /
/// encode; otherwise on working linear light, exactly as a serial grade always compiled.
#[allow(clippy::too_many_arguments)]
fn serial_steps(parts: Vec<(&GradeNode, Part)>, t: Tick, gs: &GradeSpace, ctx: GradeCtx, w: usize, h: usize, signal: bool) -> Vec<FxOp> {
    let px_scale = if ctx.px_scale.is_finite() && ctx.px_scale > 0.0 { ctx.px_scale } else { 1.0 };
    let wrap = |body: Vec<ColorOp>| {
        if signal {
            return body;
        }
        let mut ops = gs.encode_ops();
        ops.extend(body);
        ops.extend(gs.decode_ops());
        ops
    };
    let mut out = Vec::new();
    let mut run: Vec<ColorOp> = Vec::new();
    let keyed = |key: ColorOp, body: Vec<ColorOp>, sigma: f32| {
        let blur = crate::effects::gaussian_boxes(w, h, sigma, sigma).0;
        let mut correct = vec![ColorOp::Stash];
        correct.extend(body);
        FxOp::HslSecondary { key: wrap(vec![key]).into(), median_r: 0, denoise: 0.0, blur, correct: wrap(correct).into() }
    };
    for (n, part) in parts {
        let nr = denoise(n, t, px_scale);
        let empty = matches!(&part, Part::Signal(ops) if ops.is_empty());
        if empty && nr.is_none() {
            continue;
        }
        let windows = n.windows.get(..n.windows.len().min(MAX_WINDOWS)).unwrap_or_default();
        let masks = if windows.is_empty() { Vec::new() } else { crate::mask::prepare(windows, t, px_scale) };
        let matte = ctx.mattes.and_then(|m| m.0.get(&n.id)).filter(|m| m.is_valid()).cloned();
        if masks.is_empty() && matte.is_none() && nr.is_none() && !matches!(part, Part::Keyed { .. }) {
            run.extend(part.into_signal_ops());
            continue;
        }
        if !run.is_empty() {
            out.push(FxOp::ColorChain(wrap(std::mem::take(&mut run)).into()));
        }
        // noise reduction first: it cleans the node's input, which its qualifier then keys
        let mut steps: Vec<FxOp> = Vec::new();
        if let Some(nr) = nr {
            if signal {
                steps.push(FxOp::ColorChain(gs.decode_ops().into()));
                steps.push(nr);
                steps.push(FxOp::ColorChain(gs.encode_ops().into()));
            } else {
                steps.push(nr);
            }
        }
        if !empty {
            steps.push(match part {
                Part::Keyed { key, body, sigma } => keyed(key, body, sigma),
                part => FxOp::ColorChain(wrap(part.into_signal_ops()).into()),
            });
        }
        let steps = if masks.is_empty() { steps } else { vec![FxOp::Masked { ops: steps, masks: masks.into() }] };
        match matte {
            // windows inside the matte: the node changes only where both allow it
            Some(matte) => out.push(FxOp::MaskedBy { ops: steps, matte }),
            None => out.extend(steps),
        }
    }
    if !run.is_empty() {
        out.push(FxOp::ColorChain(wrap(run).into()));
    }
    out
}

/// Every number of a compiled grade step is finite.
fn step_finite(o: &FxOp) -> bool {
    let finite = |ops: &[ColorOp]| ops.iter().all(ColorOp::is_finite);
    match o {
        FxOp::ColorChain(ops) => finite(ops),
        FxOp::HslSecondary { key, correct, .. } => finite(key) && finite(correct),
        FxOp::Masked { ops, .. } | FxOp::MaskedBy { ops, .. } => ops.iter().all(step_finite),
        FxOp::GradeGraph(p) => p.is_finite(),
        _ => true,
    }
}

/// A node's noise reduction at clip time `t` for an image of `px_scale` pixels per source pixel
/// (None: both strengths 0).
fn denoise(n: &GradeNode, t: Tick, px_scale: f32) -> Option<FxOp> {
    let (luma, chroma) = (n.float_at("nr.luma", t), n.float_at("nr.chroma", t));
    (luma > 0.0 || chroma > 0.0).then(|| FxOp::Denoise { luma, chroma, sigma: n.float_at("nr.size", t) * px_scale })
}

/// One node's share of a compiled grade, on the grading signal.
enum Part {
    /// Per-pixel operations.
    Signal(Vec<ColorOp>),
    /// A qualified node whose matte is blurred (σ in image pixels): `key` sets the mask from the
    /// node's input; `body` ends with the [`ColorOp::MixStash`] (and the highlight view).
    Keyed { key: ColorOp, body: Vec<ColorOp>, sigma: f32 },
}

impl Part {
    /// The per-pixel form (a blurred matte unblurred).
    fn into_signal_ops(self) -> Vec<ColorOp> {
        match self {
            Part::Signal(ops) => ops,
            Part::Keyed { key, body, .. } => {
                let mut ops = vec![ColorOp::Stash, key];
                ops.extend(body);
                ops
            }
        }
    }
}

/// The parts of every active node, in order, up to and including the first node that shows its
/// qualifier (the highlight / matte view replaces the picture, so later nodes don't run).
fn parts<'a>(nodes: Vec<&'a GradeNode>, t: Tick, gs: &GradeSpace, ctx: GradeCtx) -> Vec<(&'a GradeNode, Part)> {
    let px_scale = if ctx.px_scale.is_finite() && ctx.px_scale > 0.0 { ctx.px_scale } else { 1.0 };
    let ctx = GradeCtx { px_scale, ..ctx };
    let mut out = Vec::new();
    for n in nodes {
        let body = node_ops(n, t, gs, &ctx);
        // (empty parts stay: the node may still reduce noise, `compile_fx`)
        let Some(q) = n.qualifier_at(t) else {
            out.push((n, Part::Signal(body)));
            continue;
        };
        let show = q.show.min(2);
        if body.is_empty() && show == 0 {
            out.push((n, Part::Signal(body)));
            continue;
        }
        let hdr = gs.is_hdr();
        let mut body = body;
        body.push(ColorOp::MixStash { invert: q.invert });
        if show > 0 {
            let show_mask = if show == 1 { 1 } else { 3 };
            body.push(ColorOp::HslCorrect { hue_shift: 0.0, sat: 1.0, temp: 0.0, tint: 0.0, show_mask, hdr });
        }
        let key = key_op(n, &q, hdr);
        // (Lumetri's HSL Secondary blur: σ up to 20 full-resolution pixels)
        let sigma = q.blur * QUAL_MAX_BLUR * px_scale;
        if sigma >= 0.3 {
            out.push((n, Part::Keyed { key, body, sigma }));
        } else {
            let mut ops = vec![ColorOp::Stash, key];
            ops.extend(body);
            out.push((n, Part::Signal(ops)));
        }
        if show > 0 {
            break;
        }
    }
    out
}

/// A working linear-light colour as the qualifier sees it: HSL (0…1) of the grading signal (the
/// grade picker, `grade.pickQualifier`).
pub fn qualifier_hsl(working: WorkingSpace, rgb: [f32; 3]) -> [f32; 3] {
    let gs = GradeSpace::new(working, HDR_WHITE_NITS);
    let v = filmcraft_color::ops::apply_chain(&gs.encode_ops(), rgb);
    let u = filmcraft_color::ops::to_unit(v, gs.is_hdr());
    let h = filmcraft_color::rgb_to_hsl(u[0], u[1], u[2]);
    h.map(|x| if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 })
}

/// The matte blur at `qual.blur` 1, in full-resolution pixels (σ; Lumetri's HSL Secondary).
const QUAL_MAX_BLUR: f32 = 20.0;

/// The key of node `n`'s qualifier `q`: its 3D qualifier's when that is the node's key (fork
/// G4.3), else the HSL ranges. Ranges reaching an end of 0…1 have no soft edge there (a full range
/// selects everything, black and white included).
fn key_op(n: &GradeNode, q: &Qualifier, hdr: bool) -> ColorOp {
    if let Some(k) = n.qual3d_active() {
        return key3d_op(k, hdr);
    }
    let soft = q.soft.max(1e-3);
    ColorOp::HslKey {
        hue: q.hue,
        range: q.hue_width.max(1e-3),
        sat_min: if q.sat[0] <= 0.0 { -1.0 } else { q.sat[0] },
        sat_max: q.sat[1],
        luma_min: if q.lum[0] <= 0.0 { -1.0 } else { q.lum[0] },
        luma_max: if q.lum[1] >= 1.0 { 2.0 } else { q.lum[1] },
        soft,
        hdr,
    }
}

/// The key of a 3D qualifier (fork G4.3): its clusters as spheres in the opponent space.
fn key3d_op(q: &filmcraft_project::qual3d::Qual3d, hdr: bool) -> ColorOp {
    let spheres = |v: &[filmcraft_project::qual3d::Cluster]| v.iter().map(|c| [c.c[0], c.c[1], c.c[2], c.r]).collect::<Vec<_>>();
    ColorOp::Key3d(Arc::new(filmcraft_color::key3d::Key3d::new(&spheres(&q.include), &spheres(&q.exclude), q.effective_softness(), hdr)))
}

/// The despill of a node whose qualifier keys with a 3D qualifier with despill (fork G4.3): the
/// picked colour's chroma pulled out, as the node's first correction (so it acts inside the
/// node's matte and the node's other corrections grade the despilled colour).
fn despill_op(n: &GradeNode, t: Tick) -> Option<ColorOp> {
    n.qualifier_at(t)?;
    let q = n.qual3d_active()?;
    let amount = q.effective_despill();
    if amount <= 0.0 {
        return None;
    }
    let dir = filmcraft_color::key3d::despill_dir(q.key_chroma())?;
    Some(ColorOp::Despill { dir, amount })
}

/// A working linear-light colour as the 3D qualifier sees it: its opponent coordinates of the
/// grading signal brought into 0…1 (`filmcraft_color::key3d`; the stroke sampler,
/// `grade.qual3d.addStroke`). None for a colour that isn't finite.
pub fn qual3d_point(working: WorkingSpace, rgb: [f32; 3]) -> Option<[f32; 3]> {
    let gs = GradeSpace::new(working, HDR_WHITE_NITS);
    let v = filmcraft_color::ops::apply_chain(&gs.encode_ops(), rgb);
    if !v.iter().all(|x| x.is_finite()) {
        return None;
    }
    let p = filmcraft_color::key3d::opponent(filmcraft_color::ops::to_unit(v, gs.is_hdr()));
    p.iter().all(|x| x.is_finite()).then_some(p)
}

/// One node's operations on the grading signal, in order: a 3D qualifier's despill
/// ([`despill_op`]), temperature / tint (in linear light), lift / gamma / gain / offset, contrast
/// around the pivot, saturation, the HDR zone wheels ([`hdr_zone_ops`]), the colour warper
/// ([`warp_ops`]), curves, the LUT, highlight roll-off, film grain.
fn node_ops(n: &GradeNode, t: Tick, gs: &GradeSpace, ctx: &GradeCtx) -> Vec<ColorOp> {
    let mut ops = Vec::new();
    // 3D qualifier despill (G4.3): first, inside the node's matte
    ops.extend(despill_op(n, t));
    // white balance in linear light (Lumetri's gains)
    let (temp, tint) = (n.float_at("temperature", t), n.float_at("tint", t));
    if temp != 0.0 || tint != 0.0 {
        let g = filmcraft_color::wb::gains(temp, tint);
        ops.extend(gs.decode_ops());
        ops.push(ColorOp::Matrix { m: [[g[0], 0.0, 0.0], [0.0, g[1], 0.0], [0.0, 0.0, g[2]]], offset: [0.0; 3] });
        ops.extend(gs.encode_ops());
    }
    // wheels: offsets around identity, the master ring added to every channel
    let wheel = |key: &str| {
        let w = n.wheel_at(key, t);
        [w[0] + w[3], w[1] + w[3], w[2] + w[3]]
    };
    let (lift, gamma, gain, offset) = (wheel("lift"), wheel("gamma"), wheel("gain"), wheel("offset"));
    if [lift, gamma, gain, offset].iter().flatten().any(|&v| v != 0.0) {
        ops.push(ColorOp::Lggo {
            lift,
            // up brightens the mids: v^(1/γ) with γ = 2^wheel (½ … 2 for one wheel's range)
            gamma: gamma.map(|g| g.exp2()),
            gain: gain.map(|g| (1.0 + g).max(0.0)),
            offset,
        });
    }
    let (contrast, pivot) = (n.float_at("contrast", t), n.float_at("pivot", t));
    if contrast != 1.0 {
        let k = contrast;
        ops.push(ColorOp::Matrix { m: [[k, 0.0, 0.0], [0.0, k, 0.0], [0.0, 0.0, k]], offset: [pivot * (1.0 - k); 3] });
    }
    let sat = n.float_at("saturation", t);
    if sat != 1.0 {
        ops.push(ColorOp::Saturation { sat, vibrance: 0.0, weights: LUMA });
    }
    ops.extend(hdr_zone_ops(n, t, gs));
    // colour warper (G4.1): after the primaries, before the curves
    ops.extend(warp_ops(n));
    let hdr = gs.is_hdr();
    for (key, channels) in CURVES {
        if let Some(points) = n.curve(key) {
            let table = Table { lo: 0.0, hi: 1.0, values: crate::effects::curve_lut(&points, CURVE_SAMPLES).into() };
            ops.push(ColorOp::Curve { table, channels, hdr });
        }
    }
    if !n.lut.is_empty()
        && let Some(lut) = crate::luts::resolve(ctx.project, &n.lut)
    {
        let mix = n.float_at("lut.mix", t);
        if mix > 0.0 {
            ops.push(ColorOp::Lut { lut, intensity: mix, hdr });
        }
    }
    let rolloff = n.float_at("hl.rolloff", t);
    if rolloff > 0.0 {
        // on a display-referred signal (sRGB-encoded working linear, diffuse white 1), whatever
        // the grading signal: the knee means the same in every working space
        ops.extend(gs.decode_ops());
        ops.push(ColorOp::Encode(Tf::Srgb));
        ops.push(ColorOp::Table(soft_clip(n.float_at("hl.knee", t), rolloff)));
        ops.push(ColorOp::Decode(Tf::Srgb));
        ops.extend(gs.encode_ops());
    }
    let strength = n.float_at("grain.strength", t);
    if strength > 0.0 {
        let softness = n.float_at("grain.softness", t);
        let (size, strength) = grain_cell(n.float_at("grain.size", t), ctx.px_scale, strength, softness);
        let seed = grain_seed(&[t.0 as u64, ctx.seq_time.0 as u64, ctx.clip, n.id]);
        ops.push(ColorOp::Grain { strength, inv_size: 1.0 / size, softness, seed, weights: LUMA });
    }
    ops
}

/// Luminance of mid grey in working linear light: the HDR zone wheels' stops count from here
/// (18 % in every working space: scene reflectance in DWG, display-referred linear otherwise).
pub const HDR_ZONE_PIVOT: f32 = 0.18;

/// A node's HDR zone wheels (fork G4.2) as operations on the grading signal: the signal decoded
/// to working linear light, the [`ColorOp::ZoneWheels`], encoded back (empty when every wheel is
/// the identity). They sit right after the node's primaries (lift / gamma / gain / offset,
/// contrast, saturation) and before its colour warper and curves, as Resolve's HDR palette
/// grades the same node.
///
/// The colour model: luminance with the working gamut's weights (DWG in a DaVinci working space,
/// BT.2020 for PQ / HLG, BT.709 otherwise), stops from [`HDR_ZONE_PIVOT`]; each wheel's balance
/// becomes a luminance-neutral shift (its offsets minus their luminance), so it tints without
/// brightening; exposure is a gain in stops.
fn hdr_zone_ops(n: &GradeNode, t: Tick, gs: &GradeSpace) -> Vec<ColorOp> {
    use filmcraft_color::ops::{Zone, ZoneAdjust, ZoneWheels};
    let Some(h) = n.hdr_at(t) else { return Vec::new() };
    let weights = hdr_zone_weights(gs);
    let adjust = |z: &filmcraft_project::grade::HdrZone| {
        let b = z.balance;
        let y = weights[0] * b[0] + weights[1] * b[1] + weights[2] * b[2];
        ZoneAdjust { exposure: z.exposure, sat: z.sat, shift: b.map(|v| v - y) }
    };
    let zones = h.zones.map(|z| Zone { edge: z.range, falloff: z.falloff, adjust: adjust(&z) });
    let op = ColorOp::ZoneWheels(Arc::new(ZoneWheels { weights, pivot: HDR_ZONE_PIVOT, global: adjust(&h.global), zones }));
    // SDR: an unclamped sRGB round trip (the curve continues past 1 and linearly below 0), so
    // turning a wheel on doesn't clip the primaries' overshoot before the curves; the other
    // grading spaces don't clip mid-chain anyway
    let (decode, encode) = match gs {
        GradeSpace::Sdr => (vec![ColorOp::Decode(Tf::Srgb)], vec![ColorOp::Encode(Tf::Srgb)]),
        _ => (gs.decode_ops(), gs.encode_ops()),
    };
    let mut ops = decode;
    ops.push(op);
    ops.extend(encode);
    ops
}

/// The luminance weights of the working gamut of grading space `gs` (they sum to 1).
fn hdr_zone_weights(gs: &GradeSpace) -> [f32; 3] {
    let gamut = match gs {
        GradeSpace::DaVinciIntermediate => filmcraft_color::Gamut::DaVinciWide,
        GradeSpace::Pq { .. } | GradeSpace::Hlg { .. } => filmcraft_color::Gamut::Bt2020,
        GradeSpace::Sdr => filmcraft_color::Gamut::Bt709,
    };
    gamut.luma().map(|v| v as f32)
}

/// A node's colour warper (fork G4.1) as ops on the grading signal: the hue-sat mesh, then the
/// chroma-luma mesh, each only when it moves something (an untouched warper is no op at all, so
/// it is bit exact). Not keyframed: the meshes are static.
fn warp_ops(n: &GradeNode) -> Vec<ColorOp> {
    use filmcraft_color::warp::{MeshSpec, WarpSpace, compile};
    use filmcraft_project::warper::WarpMode;
    let Some(w) = n.warper.as_ref().filter(|w| !w.is_identity()) else {
        return Vec::new();
    };
    [(WarpMode::HueSat, WarpSpace::HueSat), (WarpMode::ChromaLuma, WarpSpace::ChromaLuma)]
        .into_iter()
        .filter_map(|(mode, space)| {
            let mesh = w.mesh(mode);
            let moved = mesh.moves(mode);
            let pinned: Vec<u32> = mesh.points.iter().filter(|p| p.pinned).map(|p| p.index).collect();
            let spec = MeshSpec {
                space,
                cols: mesh.cols,
                rows: mesh.rows,
                moved: &moved,
                pinned: &pinned,
                range: w.effective_range(),
                strength: w.effective_strength(),
                preserve_luma: w.preserve_luma,
                weights: LUMA,
            };
            compile(&spec).map(|op| ColorOp::Warp2D(std::sync::Arc::new(op)))
        })
        .collect()
}

/// The highlight soft clip (G2.5) on a display-referred signal (white 1): the identity up to `knee`; above it
/// `knee + (1 − knee)·tanh((v − knee) / (1 − knee))`, which leaves the knee with slope 1 and
/// approaches 1 instead of passing it; mixed in by `amount`. Sampled over −0.5…2 (beyond: the
/// table's linear extension, slope 1 below and ≈ 1 − amount above).
fn soft_clip(knee: f32, amount: f32) -> Table {
    let k = f64::from(if knee.is_finite() { knee.clamp(0.05, 0.99) } else { 0.75 });
    let a = f64::from(if amount.is_finite() { amount.clamp(0.0, 1.0) } else { 0.0 });
    Table::sample(-0.5, 2.0, SOFT_CLIP_SAMPLES, |v| {
        let f = if v <= k { v } else { k + (1.0 - k) * ((v - k) / (1.0 - k)).tanh() };
        v + (f - v) * a
    })
}

/// Samples of the soft-clip table (≈ 1 / 800 of the signal range apart).
const SOFT_CLIP_SAMPLES: usize = 2048;

/// The grain cell size in image pixels and the strength for a `size` (source pixels) grain drawn
/// at `px_scale`. Cells smaller than a pixel are drawn 1 px and attenuated by the cell size: a
/// pixel of a downscaled render averages (1 / cell)² independent cells, whose mean has
/// 1 / √((1 / cell)²) = cell times their standard deviation. So a ¼-resolution preview shows the
/// grain strength of the full-size render box-downscaled by 4.
///
/// Soft grain correlates neighbouring pixels, so its average keeps more of its strength: over
/// many cells, by [`soft_gain`]`(softness)` (≈ 1.45 at softness ½). The gain is blended in by
/// 1 − cell² (none at 1 px cells, all of it for many cells per pixel).
fn grain_cell(size: f32, px_scale: f32, strength: f32, softness: f32) -> (f32, f32) {
    let cell = (size * px_scale).min(GRAIN_MAX_SIZE);
    if cell >= 1.0 {
        return (cell, strength);
    }
    // (finite, ≥ 0.5 × the smallest scale: never 0 or NaN here)
    let cell = cell.clamp(0.0, 1.0);
    let gain = 1.0 + (soft_gain(softness) - 1.0) * (1.0 - cell * cell);
    (1.0, strength * cell * gain)
}

/// How much more of its standard deviation soft grain keeps than hard grain when averaged over
/// many cells: per axis, every cell value contributes ∫₀¹ df / N(f) with the normalised
/// interpolation's per-axis norm N(f) = √((1 − a)² + a²), a = softness · smoothstep(f)
/// (`filmcraft_color::ops::grain_noise`); the two axes multiply.
fn soft_gain(softness: f32) -> f32 {
    let s = if softness.is_finite() { softness.clamp(0.0, 1.0) } else { 0.0 };
    const STEPS: usize = 64;
    let per_axis = (0..STEPS)
        .map(|i| {
            let f = (i as f32 + 0.5) / STEPS as f32;
            let a = s * f * f * (3.0 - 2.0 * f);
            1.0 / ((1.0 - a) * (1.0 - a) + a * a).sqrt().max(0.5)
        })
        .sum::<f32>()
        / STEPS as f32;
    per_axis * per_axis
}

/// The grain pattern's seed: a hash of the clip time and the sequence time (one pattern per
/// frame, the same every time that frame renders, new every frame also in a hold), the clip and
/// the node (stacked clips and two grain nodes don't repeat each other).
fn grain_seed(parts: &[u64]) -> u32 {
    let mut x = 0u64;
    for p in parts {
        // splitmix64 finaliser over each part
        x = (x ^ p).wrapping_add(0x9e37_79b9_7f4a_7c15);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
    }
    (x ^ (x >> 32)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::grade::MAX_NODES;
    use filmcraft_project::{Param, ParamValue};

    fn node(params: &[(&str, ParamValue)]) -> GradeNode {
        let mut n = GradeNode::new(1);
        for (k, v) in params {
            n.params.insert((*k).into(), Param::new(v.clone()));
        }
        n
    }

    const SPACES: [WorkingSpace; 2] = [WorkingSpace::Rec709, WorkingSpace::DaVinciWide];

    #[test]
    fn identity_grades_compile_to_nothing() {
        let identity = node(&[
            ("lift", ParamValue::Color([0.0; 4])),
            ("contrast", ParamValue::Float(1.0)),
            ("pivot", ParamValue::Float(0.2)),
            ("saturation", ParamValue::Float(1.0)),
            ("curve.master", ParamValue::Curve(vec![[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]])),
            ("unknown.key", ParamValue::Float(9.0)),
        ]);
        for ws in SPACES {
            assert!(compile(&Grade::default(), Tick(0), ws).is_empty());
            assert!(compile(&Grade::with_node(), Tick(0), ws).is_empty());
            assert!(compile(&Grade { nodes: vec![identity.clone(); 3], bypass: false, ..Default::default() }, Tick(0), ws).is_empty());
        }
    }

    #[test]
    fn bypassed_and_disabled_nodes_compile_to_nothing() {
        let mut lifted = node(&[("lift", ParamValue::Color([0.1, 0.0, 0.0, 0.0]))]);
        let mut g = Grade { nodes: vec![lifted.clone()], bypass: true, ..Default::default() };
        assert!(compile(&g, Tick(0), WorkingSpace::DaVinciWide).is_empty());
        lifted.enabled = false;
        g = Grade { nodes: vec![lifted], bypass: false, ..Default::default() };
        assert!(compile(&g, Tick(0), WorkingSpace::DaVinciWide).is_empty());
    }

    #[test]
    fn a_node_compiles_to_the_signal_ops_between_encode_and_decode() {
        let n = node(&[
            ("lift", ParamValue::Color([0.1, 0.0, 0.0, 0.0])),
            ("gamma", ParamValue::Color([0.0, 1.0, 0.0, 0.0])),
            ("gain", ParamValue::Color([0.0, 0.0, 0.5, 0.0])),
            ("offset", ParamValue::Color([0.0, 0.0, 0.0, 0.02])),
            ("contrast", ParamValue::Float(1.2)),
            ("pivot", ParamValue::Float(0.4)),
            ("saturation", ParamValue::Float(0.5)),
            ("curve.g", ParamValue::Curve(vec![[0.0, 0.1], [1.0, 1.0]])),
        ]);
        let gs = GradeSpace::new(WorkingSpace::DaVinciWide, HDR_WHITE_NITS);
        let ops = compile(&Grade { nodes: vec![n], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide);
        let enc = gs.encode_ops();
        let dec = gs.decode_ops();
        assert_eq!(ops[..enc.len()], enc[..]);
        assert_eq!(ops[ops.len() - dec.len()..], dec[..]);
        let body = &ops[enc.len()..ops.len() - dec.len()];
        assert_eq!(body[0], ColorOp::Lggo { lift: [0.1, 0.0, 0.0], gamma: [1.0, 2.0, 1.0], gain: [1.0, 1.0, 1.5], offset: [0.02; 3] });
        let k = 1.2f32;
        assert_eq!(body[1], ColorOp::Matrix { m: [[k, 0.0, 0.0], [0.0, k, 0.0], [0.0, 0.0, k]], offset: [0.4 * (1.0 - k); 3] });
        assert_eq!(body[2], ColorOp::Saturation { sat: 0.5, vibrance: 0.0, weights: LUMA });
        assert!(matches!(&body[3], ColorOp::Curve { channels: [false, true, false], hdr: false, .. }));
        assert_eq!(body.len(), 4);
    }

    /// The compiled chain on a pixel against the node's controls computed by hand (the oracle).
    #[test]
    fn compiled_chain_matches_a_hand_computed_pixel() {
        let n = node(&[
            ("lift", ParamValue::Color([0.05, 0.0, 0.0, 0.0])),
            ("gain", ParamValue::Color([0.0, 0.2, 0.0, 0.0])),
            ("contrast", ParamValue::Float(1.5)),
            ("pivot", ParamValue::Float(0.5)),
        ]);
        // Rec. 709 working space: the grading signal is sRGB-encoded display values
        let ops = compile(&Grade { nodes: vec![n], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::Rec709);
        let lin = [0.18f32, 0.18, 0.18];
        let out = filmcraft_color::ops::apply_chain(&ops, lin);
        let srgb_enc = |x: f32| if x <= 0.0031308 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
        let srgb_dec = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        let v = srgb_enc(0.18);
        let (lift, gain) = ([0.05f32, 0.0, 0.0], [1.0f32, 1.2, 1.0]);
        for c in 0..3 {
            let a = lift[c] + (gain[c] - lift[c]) * v;
            let s = (a - 0.5) * 1.5 + 0.5;
            let want = srgb_dec(s.clamp(0.0, 1.0));
            assert!((out[c] - want).abs() < 1e-4, "channel {c}: {} vs {want}", out[c]);
        }
    }

    #[test]
    fn keyframed_wheels_interpolate_over_clip_time() {
        let mut n = GradeNode::new(1);
        let mut p = Param::new(ParamValue::Color([0.0; 4]));
        p.put_keyframe(Tick(0), ParamValue::Color([0.0; 4]));
        p.put_keyframe(Tick(100), ParamValue::Color([0.0, 0.0, 0.0, 0.2]));
        n.params.insert("offset".into(), p);
        let g = Grade { nodes: vec![n], bypass: false, ..Default::default() };
        assert!(compile(&g, Tick(0), WorkingSpace::Rec709).is_empty());
        let mid = compile(&g, Tick(50), WorkingSpace::Rec709);
        assert!(mid.iter().any(|o| matches!(o, ColorOp::Lggo { offset, .. } if (offset[0] - 0.1).abs() < 1e-6)));
    }

    #[test]
    fn temperature_runs_in_linear_light() {
        let n = node(&[("temperature", ParamValue::Float(50.0))]);
        let gs = GradeSpace::new(WorkingSpace::DaVinciWide, HDR_WHITE_NITS);
        let ops = compile(&Grade { nodes: vec![n], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide);
        let mut want = gs.encode_ops();
        want.extend(gs.decode_ops());
        want.push(ColorOp::Matrix { m: [[1.175, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.825]], offset: [0.0; 3] });
        want.extend(gs.encode_ops());
        want.extend(gs.decode_ops());
        assert_eq!(ops, want);
    }

    #[test]
    fn hostile_grades_never_produce_non_finite_ops() {
        let mut nodes = Vec::new();
        for i in 0..10_000u64 {
            let mut n = GradeNode::new(i);
            n.params.insert("gain".into(), Param::new(ParamValue::Color([f32::MAX, f32::NAN, -f32::MAX, f32::INFINITY])));
            n.params.insert("gamma".into(), Param::new(ParamValue::Color([-1e30, 1e30, 0.0, 0.0])));
            n.params.insert("contrast".into(), Param::new(ParamValue::Float(f64::INFINITY)));
            n.params.insert("pivot".into(), Param::new(ParamValue::Float(-1e300)));
            n.params.insert("curve.master".into(), Param::new(ParamValue::Curve(vec![[0.5, f32::NAN]; 10_000])));
            nodes.push(n);
        }
        let g = Grade { nodes, ..Default::default() };
        for ws in [WorkingSpace::Rec709, WorkingSpace::DaVinciWide, WorkingSpace::Rec2100Pq, WorkingSpace::Rec2100Hlg] {
            let ops = compile(&g, Tick(i64::MAX), ws);
            assert!(!ops.is_empty() && ops.iter().all(ColorOp::is_finite));
            let lggo = ops.iter().filter(|o| matches!(o, ColorOp::Lggo { .. })).count();
            assert_eq!(lggo, MAX_NODES);
            let out = filmcraft_color::ops::apply_chain(&ops, [0.5, 0.2, 0.1]);
            assert!(out.iter().all(|v| !v.is_nan()), "{out:?}");
        }
    }

    fn with_id(mut n: GradeNode, id: u64) -> GradeNode {
        n.id = id;
        n
    }

    fn grain_node(id: u64, strength: f64) -> GradeNode {
        let n = node(&[("grain.strength", ParamValue::Float(strength)), ("grain.size", ParamValue::Float(2.0)), ("grain.softness", ParamValue::Float(0.25))]);
        with_id(n, id)
    }

    /// A 48×48 grey patch through `ops` (positions matter for grain).
    fn patch(ops: &[ColorOp], level: f32) -> Vec<[f32; 3]> {
        (0..48 * 48).map(|i| filmcraft_color::ops::apply_chain_at(ops, [level; 3], [(i % 48) as f32, (i / 48) as f32])).collect()
    }

    #[test]
    fn zero_grain_strength_compiles_to_nothing() {
        let n = node(&[("grain.strength", ParamValue::Float(0.0)), ("grain.size", ParamValue::Float(3.0)), ("grain.softness", ParamValue::Float(1.0))]);
        for ws in SPACES {
            assert!(compile(&Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() }, Tick(5), ws).is_empty());
        }
    }

    #[test]
    fn grain_sits_at_its_nodes_place_in_the_chain() {
        let lift = node(&[("lift", ParamValue::Color([0.1, 0.0, 0.0, 0.0]))]);
        let sat = with_id(node(&[("saturation", ParamValue::Float(0.5))]), 3);
        let g = Grade { nodes: vec![lift, grain_node(2, 0.5), sat], bypass: false, ..Default::default() };
        let gs = GradeSpace::new(WorkingSpace::DaVinciWide, HDR_WHITE_NITS);
        let ops = compile_at(&g, Tick(77), WorkingSpace::DaVinciWide, GradeCtx { px_scale: 0.5, ..GradeCtx::default() });
        let enc = gs.encode_ops().len();
        let body = &ops[enc..ops.len() - gs.decode_ops().len()];
        assert!(matches!(body[0], ColorOp::Lggo { .. }));
        // grain size 2 full-resolution pixels at half resolution: 1 pixel cells
        assert!(matches!(body[1], ColorOp::Grain { strength: 0.5, inv_size: 1.0, softness: 0.25, .. }), "{:?}", body[1]);
        assert!(matches!(body[2], ColorOp::Saturation { .. }));
        assert_eq!(body.len(), 3);
    }

    #[test]
    fn grain_is_the_same_for_a_frame_and_new_every_frame() {
        let g = Grade { nodes: vec![grain_node(1, 0.6)], bypass: false, ..Default::default() };
        for ws in SPACES {
            let (a, b, c) = (compile(&g, Tick(1000), ws), compile(&g, Tick(1000), ws), compile(&g, Tick(2000), ws));
            assert_eq!(patch(&a, 0.18), patch(&b, 0.18));
            let (pa, pc) = (patch(&a, 0.18), patch(&c, 0.18));
            let differ = pa.iter().zip(&pc).filter(|(x, y)| x != y).count();
            assert!(differ > pa.len() / 2, "{differ}");
        }
        // two grain nodes on the same frame draw different patterns
        let two = |id| compile(&Grade { nodes: vec![grain_node(id, 0.6)], bypass: false, ..Default::default() }, Tick(1000), WorkingSpace::Rec709);
        assert_ne!(patch(&two(1), 0.18), patch(&two(2), 0.18));
    }

    #[test]
    fn grain_is_weaker_in_shadows_and_highlights() {
        let g = Grade { nodes: vec![grain_node(1, 1.0)], bypass: false, ..Default::default() };
        // Rec. 709: the grading signal is sRGB; 0, mid grey (sRGB 0.5) and 1 in linear light
        let ops = compile(&g, Tick(0), WorkingSpace::Rec709);
        // (measured on the signal the grain was added to)
        let enc = |x: f32| if x <= 0.0031308 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
        let mad = |level: f32| patch(&ops, level).iter().map(|p| (enc(p[1]) - enc(level)).abs()).sum::<f32>() / 2304.0;
        let (black, mid, white) = (mad(0.0), mad(0.214), mad(1.0));
        assert!(mid > 0.01, "{mid}");
        assert!(black < mid * 0.2 && white < mid * 0.2, "{black} {mid} {white}");
    }

    #[test]
    fn hostile_grain_params_and_scales_stay_finite() {
        let mut n = GradeNode::new(u64::MAX);
        n.params.insert("grain.strength".into(), Param::new(ParamValue::Float(1e300)));
        n.params.insert("grain.size".into(), Param::new(ParamValue::Float(-1e300)));
        n.params.insert("grain.softness".into(), Param::new(ParamValue::Float(f64::NAN)));
        let g = Grade { nodes: vec![n], bypass: false, ..Default::default() };
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY, 1e-30, 1e30] {
            for t in [Tick(i64::MIN), Tick(-1), Tick(i64::MAX)] {
                let ops = compile_at(&g, t, WorkingSpace::Rec2100Pq, GradeCtx { px_scale: scale, seq_time: t, clip: u64::MAX, project: None, mattes: None });
                assert!(!ops.is_empty() && ops.iter().all(ColorOp::is_finite));
                let out = filmcraft_color::ops::apply_chain_at(&ops, [0.5, 0.2, 0.1], [1e6, -3.0]);
                assert!(out.iter().all(|v| v.is_finite()), "{out:?}");
            }
        }
    }

    fn std_dev(v: &[f32]) -> f32 {
        let n = v.len().max(1) as f32;
        let mean = v.iter().sum::<f32>() / n;
        (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n).sqrt()
    }

    /// Green channel of a flat `level` patch of `w`² pixels through `ops`.
    fn flat(ops: &[ColorOp], w: usize, level: f32) -> Vec<f32> {
        (0..w * w).map(|i| filmcraft_color::ops::apply_chain_at(ops, [level; 3], [(i % w) as f32, (i / w) as f32])[1]).collect()
    }

    /// A ¼-resolution preview shows about the grain strength of the full-size render
    /// box-downscaled by 4 (cells below a pixel are attenuated, not drawn at full amplitude).
    #[test]
    fn preview_grain_matches_a_downscaled_full_render() {
        for (size, softness) in [(1.0, 0.0), (1.0, 0.5), (1.0, 1.0), (2.0, 0.0), (2.0, 0.5), (0.5, 0.0), (4.0, 0.5)] {
            let n =
                node(&[("grain.strength", ParamValue::Float(1.0)), ("grain.size", ParamValue::Float(size)), ("grain.softness", ParamValue::Float(softness))]);
            let g = Grade { nodes: vec![n], bypass: false, ..Default::default() };
            let at = |px_scale| compile_at(&g, Tick(9), WorkingSpace::Rec709, GradeCtx { px_scale, seq_time: Tick(9), clip: 4, project: None, mattes: None });
            let (w, k) = (256usize, 4usize);
            let full = flat(&at(1.0), w, 0.214);
            let small = w / k;
            let down: Vec<f32> = (0..small * small)
                .map(|i| {
                    let (x, y) = (i % small, i / small);
                    (0..k * k).map(|j| full.get((y * k + j / k) * w + x * k + j % k).copied().unwrap_or(0.0)).sum::<f32>() / (k * k) as f32
                })
                .collect();
            let preview = flat(&at(0.25), small, 0.214);
            let (a, b) = (std_dev(&down), std_dev(&preview));
            eprintln!("size {size} softness {softness}: downscaled {a:.5}, preview {b:.5}");
            assert!(b > 0.0 && (b / a - 1.0).abs() < 0.25, "size {size} softness {softness}: downscaled {a}, preview {b}");
        }
    }

    #[test]
    fn grain_seed_mixes_sequence_time_and_clip() {
        let g = Grade { nodes: vec![grain_node(1, 0.6)], bypass: false, ..Default::default() };
        let ops = |seq: i64, clip: u64| {
            compile_at(&g, Tick(500), WorkingSpace::Rec709, GradeCtx { px_scale: 1.0, seq_time: Tick(seq), clip, project: None, mattes: None })
        };
        assert_eq!(patch(&ops(10, 1), 0.18), patch(&ops(10, 1), 0.18));
        // a hold (same clip time, later sequence time) and a stacked clip of the same source
        assert_ne!(patch(&ops(10, 1), 0.18), patch(&ops(11, 1), 0.18));
        assert_ne!(patch(&ops(10, 1), 0.18), patch(&ops(10, 2), 0.18));
    }

    /// `changes_picture`'s table-free test agrees with the compiled chain.
    #[test]
    fn node_changes_agrees_with_node_ops() {
        let cases: Vec<(&str, ParamValue)> = vec![
            ("lift", ParamValue::Color([0.0; 4])),
            ("lift", ParamValue::Color([0.1, 0.0, 0.0, -0.1])),
            ("gamma", ParamValue::Color([0.0, 0.0, 0.0, 0.2])),
            ("contrast", ParamValue::Float(1.0)),
            ("contrast", ParamValue::Float(1.1)),
            ("pivot", ParamValue::Float(0.1)),
            ("saturation", ParamValue::Float(0.9)),
            ("temperature", ParamValue::Float(0.0)),
            ("tint", ParamValue::Float(-3.0)),
            ("curve.b", ParamValue::Curve(vec![[0.0, 0.0], [1.0, 1.0]])),
            ("curve.b", ParamValue::Curve(vec![[0.0, 0.1], [1.0, 1.0]])),
            ("grain.strength", ParamValue::Float(0.0)),
            ("grain.strength", ParamValue::Float(0.01)),
            ("grain.size", ParamValue::Float(3.0)),
            ("qual.on", ParamValue::Float(1.0)),
            ("qual.show", ParamValue::Float(1.0)),
        ];
        for (k, v) in cases {
            let n = node(&[(k, v.clone())]);
            let compiled = !compile(&Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide).is_empty();
            assert_eq!(node_changes(&n, Tick(0), None), compiled, "{k} {v:?}");
        }
    }

    /// A node lifting red, qualified on `hue` (0…1) with the given extras.
    fn qualified(extra: &[(&str, ParamValue)]) -> GradeNode {
        let mut ps = vec![
            ("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5])),
            ("qual.on", ParamValue::Float(1.0)),
            ("qual.hue", ParamValue::Float(0.0)),
            ("qual.hueWidth", ParamValue::Float(0.1)),
            ("qual.satLow", ParamValue::Float(0.3)),
            ("qual.soft", ParamValue::Float(0.02)),
        ];
        ps.extend(extra.iter().cloned());
        node(&ps)
    }

    const RED: [f32; 3] = [0.3, 0.02, 0.02];
    const BLUE: [f32; 3] = [0.02, 0.03, 0.3];
    const GREY: [f32; 3] = [0.18; 3];

    #[test]
    fn a_qualifier_limits_the_node_to_its_key() {
        for ws in SPACES {
            let plain =
                compile(&Grade { nodes: vec![node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5]))])], bypass: false, ..Default::default() }, Tick(0), ws);
            let ops = compile(&Grade { nodes: vec![qualified(&[])], bypass: false, ..Default::default() }, Tick(0), ws);
            let (a, b) = (filmcraft_color::ops::apply_chain(&ops, RED), filmcraft_color::ops::apply_chain(&plain, RED));
            assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5), "{ws:?}: {a:?} vs {b:?}");
            for c in [BLUE, GREY] {
                let out = filmcraft_color::ops::apply_chain(&ops, c);
                assert!(out.iter().zip(&c).all(|(x, y)| (x - y).abs() < 1e-5), "{ws:?}: {c:?} → {out:?}");
            }
            // inverted: everything but the red
            let inv = compile(&Grade { nodes: vec![qualified(&[("qual.invert", ParamValue::Float(1.0))])], bypass: false, ..Default::default() }, Tick(0), ws);
            let r = filmcraft_color::ops::apply_chain(&inv, RED);
            assert!(r.iter().zip(&RED).all(|(x, y)| (x - y).abs() < 1e-5), "{r:?}");
            let g = filmcraft_color::ops::apply_chain(&inv, GREY);
            let gp = filmcraft_color::ops::apply_chain(&plain, GREY);
            assert!(g.iter().zip(&gp).all(|(x, y)| (x - y).abs() < 1e-5), "{g:?} vs {gp:?}");
        }
    }

    #[test]
    fn a_full_range_qualifier_selects_everything() {
        let mut n = qualified(&[]);
        n.params.insert("qual.hueWidth".into(), Param::new(ParamValue::Float(1.0)));
        n.params.insert("qual.satLow".into(), Param::new(ParamValue::Float(0.0)));
        let plain = compile(
            &Grade { nodes: vec![node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5]))])], bypass: false, ..Default::default() },
            Tick(0),
            WorkingSpace::DaVinciWide,
        );
        let ops = compile(&Grade { nodes: vec![n], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide);
        for c in [RED, BLUE, GREY, [0.0; 3], [1.0; 3], [20.0, 0.0, 5.0]] {
            let (a, b) = (filmcraft_color::ops::apply_chain(&ops, c), filmcraft_color::ops::apply_chain(&plain, c));
            assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= 1e-5 * y.abs().max(1.0)), "{c:?}: {a:?} vs {b:?}");
        }
    }

    #[test]
    fn sat_high_excludes_saturated_colours() {
        let n = qualified(&[("qual.hueWidth", ParamValue::Float(1.0)), ("qual.satLow", ParamValue::Float(0.0)), ("qual.satHigh", ParamValue::Float(0.3))]);
        let ops = compile(&Grade { nodes: vec![n], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::Rec709);
        let out = filmcraft_color::ops::apply_chain(&ops, RED);
        assert!(out.iter().zip(&RED).all(|(x, y)| (x - y).abs() < 1e-5), "{out:?}");
        let grey = filmcraft_color::ops::apply_chain(&ops, GREY);
        assert!(grey[1] > GREY[1] + 0.01, "{grey:?}");
    }

    #[test]
    fn a_qualifier_alone_changes_nothing_and_highlight_stops_the_chain() {
        let only = node(&[("qual.on", ParamValue::Float(1.0)), ("qual.hue", ParamValue::Float(0.3))]);
        let g = Grade { nodes: vec![only.clone()], bypass: false, ..Default::default() };
        assert!(compile(&g, Tick(0), WorkingSpace::DaVinciWide).is_empty());
        assert!(!changes_picture_of(&g));
        let mut shown = qualified(&[("qual.show", ParamValue::Float(2.0))]);
        shown.id = 1;
        let later = with_id(node(&[("saturation", ParamValue::Float(0.0))]), 2);
        let g = Grade { nodes: vec![shown, later], bypass: false, ..Default::default() };
        assert!(changes_picture_of(&g));
        let ops = compile(&g, Tick(0), WorkingSpace::Rec709);
        assert!(!ops.iter().any(|o| matches!(o, ColorOp::Saturation { .. })), "later nodes don't run under the matte view");
        // the matte: white on the red, black on the blue
        let (r, b) = (filmcraft_color::ops::apply_chain(&ops, RED), filmcraft_color::ops::apply_chain(&ops, BLUE));
        assert!(r.iter().all(|v| (v - 1.0).abs() < 1e-4) && b.iter().all(|v| v.abs() < 1e-4), "{r:?} {b:?}");
    }

    fn changes_picture_of(g: &Grade) -> bool {
        g.active_nodes().any(|n| node_changes(n, Tick(0), None))
    }

    #[test]
    fn compile_fx_fuses_unblurred_nodes_and_keys_blurred_ones() {
        let lift = node(&[("lift", ParamValue::Color([0.1, 0.0, 0.0, 0.0]))]);
        let g = Grade { nodes: vec![lift.clone(), with_id(qualified(&[]), 2)], bypass: false, ..Default::default() };
        let fx = compile_fx(&g, Tick(0), WorkingSpace::DaVinciWide, GradeCtx::default(), 64, 32);
        let want = compile(&g, Tick(0), WorkingSpace::DaVinciWide);
        assert!(matches!(fx.as_slice(), [FxOp::ColorChain(ops)] if ops[..] == want[..]), "{fx:?}");
        let blurred = with_id(qualified(&[("qual.blur", ParamValue::Float(0.5))]), 2);
        let g = Grade { nodes: vec![lift, blurred, with_id(node(&[("saturation", ParamValue::Float(0.5))]), 3)], bypass: false, ..Default::default() };
        let fx = compile_fx(&g, Tick(0), WorkingSpace::DaVinciWide, GradeCtx::default(), 64, 32);
        assert!(matches!(fx.as_slice(), [FxOp::ColorChain(_), FxOp::HslSecondary { blur, .. }, FxOp::ColorChain(_)] if !blur.is_empty()), "{fx:?}");
        assert!(fx.iter().all(FxOp::gpu_ok));
        // the blurred node's correction lands on the red and nowhere else (flat images)
        let flat = |c: [f32; 3]| {
            let mut img = crate::Image::new(8, 8);
            img.px.chunks_mut(4).for_each(|p| p.copy_from_slice(&[c[0], c[1], c[2], 1.0]));
            let only = Grade { nodes: vec![with_id(qualified(&[("qual.blur", ParamValue::Float(0.5))]), 2)], bypass: false, ..Default::default() };
            for op in compile_fx(&only, Tick(0), WorkingSpace::Rec709, GradeCtx::default(), 8, 8) {
                op.apply(&mut img);
            }
            [img.px[0], img.px[1], img.px[2]]
        };
        let b = flat(BLUE);
        assert!(b.iter().zip(&BLUE).all(|(x, y)| (x - y).abs() < 1e-4), "{b:?}");
        let r = flat(RED);
        let plain = compile(
            &Grade { nodes: vec![node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5]))])], bypass: false, ..Default::default() },
            Tick(0),
            WorkingSpace::Rec709,
        );
        let want = filmcraft_color::ops::apply_chain(&plain, RED);
        assert!(r.iter().zip(&want).all(|(x, y)| (x - y).abs() < 1e-3), "{r:?} vs {want:?}");
    }

    /// A 32×16 grey image through the grade's FX steps.
    fn run_fx(g: &Grade, c: [f32; 3]) -> crate::Image {
        let mut img = crate::Image::new(32, 16);
        img.px.chunks_mut(4).for_each(|p| p.copy_from_slice(&[c[0], c[1], c[2], 1.0]));
        for op in compile_fx(g, Tick(0), WorkingSpace::Rec709, GradeCtx::default(), 32, 16) {
            op.apply(&mut img);
        }
        img
    }

    fn px(img: &crate::Image, x: usize, y: usize) -> [f32; 3] {
        let i = (y * img.w + x) * 4;
        [img.px[i], img.px[i + 1], img.px[i + 2]]
    }

    #[test]
    fn a_window_limits_the_node_to_its_shape() {
        use filmcraft_project::{Mask, MaskPath};
        let mut n = node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5]))]);
        // a rectangle over the left half (source pixels; feather 0)
        let mut m = Mask::new("w", MaskPath::rect(0.0, 0.0, 16.0, 16.0));
        m.feather = Param::new(ParamValue::Float(0.0));
        n.windows.push(m);
        let g = Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() };
        let fx = compile_fx(&g, Tick(0), WorkingSpace::Rec709, GradeCtx::default(), 32, 16);
        assert!(matches!(fx.as_slice(), [FxOp::Masked { ops, .. }] if matches!(ops.as_slice(), [FxOp::ColorChain(_)])), "{fx:?}");
        assert!(fx.iter().all(FxOp::gpu_ok));
        let img = run_fx(&g, GREY);
        let (inside, outside) = (px(&img, 4, 8), px(&img, 28, 8));
        assert!(inside[1] > GREY[1] + 0.01, "{inside:?}");
        assert!(outside.iter().zip(&GREY).all(|(a, b)| (a - b).abs() < 1e-5), "{outside:?}");
        // inverted window: the other half
        n.windows[0].inverted = true;
        let img = run_fx(&Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() }, GREY);
        assert!(px(&img, 28, 8)[1] > GREY[1] + 0.01 && (px(&img, 4, 8)[1] - GREY[1]).abs() < 1e-5);
        // a window and a qualifier: only where both select (grey isn't red)
        n.windows[0].inverted = false;
        let mut q = qualified(&[]);
        q.windows = n.windows.clone();
        let img = run_fx(&Grade { nodes: vec![q.clone()], bypass: false, ..Default::default() }, GREY);
        assert!((px(&img, 4, 8)[1] - GREY[1]).abs() < 1e-5);
        let img = run_fx(&Grade { nodes: vec![q], bypass: false, ..Default::default() }, RED);
        assert!(px(&img, 4, 8)[0] > RED[0] + 0.01 && (px(&img, 28, 8)[0] - RED[0]).abs() < 1e-5);
        // windows don't enter the per-pixel chain, and a node with only windows changes nothing
        let mut empty = GradeNode::new(1);
        empty.windows = n.windows.clone();
        assert!(
            compile_fx(&Grade { nodes: vec![empty], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::Rec709, GradeCtx::default(), 32, 16)
                .is_empty()
        );
    }

    #[test]
    fn noise_reduction_runs_at_the_nodes_input_and_inside_its_windows() {
        use filmcraft_project::{Mask, MaskPath};
        let nr = node(&[("nr.luma", ParamValue::Float(0.5)), ("nr.chroma", ParamValue::Float(0.25))]);
        let g = Grade { nodes: vec![nr.clone()], bypass: false, ..Default::default() };
        assert!(changes_picture_of(&g));
        let ctx = GradeCtx { px_scale: 0.5, ..GradeCtx::default() };
        let fx = compile_fx(&g, Tick(0), WorkingSpace::DaVinciWide, ctx, 32, 16);
        assert!(matches!(fx.as_slice(), [FxOp::Denoise { luma: 0.5, chroma: 0.25, sigma: 0.75 }]), "{fx:?}");
        // (the per-pixel chain has no noise reduction)
        assert!(compile(&g, Tick(0), WorkingSpace::DaVinciWide).is_empty());
        // NR, then the node's correction, inside the node's window; the nodes around it fuse
        let mut n = nr;
        n.params.insert("gain".into(), Param::new(ParamValue::Color([0.0, 0.0, 0.0, 0.5])));
        n.windows.push(Mask::new("w", MaskPath::rect(0.0, 0.0, 16.0, 16.0)));
        let lift = |id| with_id(node(&[("lift", ParamValue::Color([0.1, 0.0, 0.0, 0.0]))]), id);
        let g = Grade { nodes: vec![lift(1), with_id(n, 2), lift(3)], bypass: false, ..Default::default() };
        let fx = compile_fx(&g, Tick(0), WorkingSpace::DaVinciWide, GradeCtx::default(), 32, 16);
        assert!(
            matches!(fx.as_slice(), [FxOp::ColorChain(_), FxOp::Masked { ops, .. }, FxOp::ColorChain(_)] if matches!(ops.as_slice(), [FxOp::Denoise { .. }, FxOp::ColorChain(_)])),
            "{fx:?}"
        );
        assert!(fx.iter().all(FxOp::gpu_ok));
        // zero strengths: nothing
        let off = node(&[("nr.luma", ParamValue::Float(0.0)), ("nr.size", ParamValue::Float(3.0))]);
        assert!(
            compile_fx(&Grade { nodes: vec![off.clone()], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide, GradeCtx::default(), 8, 8)
                .is_empty()
        );
        assert!(!changes_picture_of(&Grade { nodes: vec![off], bypass: false, ..Default::default() }));
    }

    #[test]
    fn soft_clip_rolls_highlights_off_and_leaves_the_rest() {
        let tab = soft_clip(0.75, 1.0);
        for v in [-0.3f32, 0.0, 0.2, 0.5, 0.74] {
            assert!((tab.eval(v) - v).abs() < 1e-3, "{v}: {}", tab.eval(v));
        }
        // monotonic, below 1 over the sampled range, slope ≈ 1 at the knee
        let mut last = f32::MIN;
        for i in 0..=250 {
            let v = 0.75 + i as f32 * 0.005;
            let o = tab.eval(v);
            assert!(o >= last && o < 1.0, "{v}: {o}");
            last = o;
        }
        assert!(((tab.eval(0.76) - tab.eval(0.75)) / 0.01 - 1.0).abs() < 0.05);
        // half the amount: half way between
        let half = soft_clip(0.75, 0.5);
        assert!((half.eval(1.2) - (1.2 + tab.eval(1.2)) / 2.0).abs() < 1e-3);
        // a node: the op sits after the curves, and 0 compiles to nothing
        let n = node(&[("hl.rolloff", ParamValue::Float(0.6)), ("hl.knee", ParamValue::Float(0.8))]);
        let ops = compile(&Grade { nodes: vec![n], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide);
        assert!(ops.iter().any(|o| matches!(o, ColorOp::Table(_))));
        let out = filmcraft_color::ops::apply_chain(&ops, [50.0, 0.18, 0.0]);
        let plain = filmcraft_color::ops::apply_chain(&ops, [0.18; 3]);
        // (full roll-off at 0.6 of 0.8: everything ends at or below diffuse white)
        assert!(out[0] < 50.0 && out.iter().all(|v| v.is_finite()), "{out:?}");
        let full = node(&[("hl.rolloff", ParamValue::Float(1.0)), ("hl.knee", ParamValue::Float(0.8))]);
        for ws in SPACES {
            let ops = compile(&Grade { nodes: vec![full.clone()], bypass: false, ..Default::default() }, Tick(0), ws);
            let o = filmcraft_color::ops::apply_chain(&ops, [8.0, 1.5, 0.3]);
            assert!(o[0] <= 1.0 + 1e-3 && o[1] <= 1.0 + 1e-3 && o[1] > 0.6, "{ws:?}: {o:?}");
        }
        assert!(plain.iter().all(|v| (v - 0.18).abs() < 1e-4), "mid grey untouched: {plain:?}");
        let off = node(&[("hl.rolloff", ParamValue::Float(0.0)), ("hl.knee", ParamValue::Float(0.6))]);
        assert!(compile(&Grade { nodes: vec![off], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide).is_empty());
        for (k, a) in [(f32::NAN, f32::NAN), (-5.0, 9.0), (1.0, 1.0)] {
            let t = soft_clip(k, a);
            assert!(t.values.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn a_node_lut_applies_after_the_corrections_and_mixes() {
        let mut n = node(&[("lut.mix", ParamValue::Float(0.5))]);
        n.lut = "builtin:look-unknown-for-sure".into();
        // an unknown reference applies nothing (but the clip still takes the effect path)
        assert!(compile(&Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::Rec709).is_empty());
        let Some(b) = crate::luts::builtins().iter().find(|b| b.id.starts_with("look-")) else { return };
        n.lut = format!("builtin:{}", b.id);
        n.params.insert("saturation".into(), Param::new(ParamValue::Float(0.5)));
        let ops = compile(&Grade { nodes: vec![n.clone()], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::Rec709);
        let sat = ops.iter().position(|o| matches!(o, ColorOp::Saturation { .. }));
        let lut = ops.iter().position(|o| matches!(o, ColorOp::Lut { intensity, .. } if (*intensity - 0.5).abs() < 1e-6));
        assert!(sat.is_some() && lut.is_some() && sat < lut, "{ops:?}");
        n.params.insert("lut.mix".into(), Param::new(ParamValue::Float(0.0)));
        assert!(
            !compile(&Grade { nodes: vec![n], bypass: false, ..Default::default() }, Tick(0), WorkingSpace::Rec709)
                .iter()
                .any(|o| matches!(o, ColorOp::Lut { .. }))
        );
    }

    #[test]
    fn hostile_windows_never_panic() {
        use filmcraft_geom::Vec2;
        use filmcraft_project::{Mask, MaskPath};
        let mut n = node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5]))]);
        for i in 0..1000 {
            let mut m = Mask::new("w", MaskPath::polygon(&[Vec2::new(f64::NAN, 0.0), Vec2::new(1e300, -1e300), Vec2::new(f64::INFINITY, i as f64)]));
            m.feather = Param::new(ParamValue::Float(f64::INFINITY));
            m.expansion = Param::new(ParamValue::Float(-1e300));
            n.windows.push(m);
        }
        n.windows.push(Mask::new("empty", MaskPath { vertices: Vec::new(), closed: true }));
        let g = Grade { nodes: vec![n], bypass: false, ..Default::default() };
        for scale in [0.0, f32::NAN, 1e-30, 1e30] {
            let fx = compile_fx(&g, Tick(0), WorkingSpace::DaVinciWide, GradeCtx { px_scale: scale, ..GradeCtx::default() }, 8, 8);
            if let Some(FxOp::Masked { masks, .. }) = fx.first() {
                assert!(masks.len() <= MAX_WINDOWS);
            }
            let mut img = crate::Image::new(8, 8);
            for op in fx {
                op.apply(&mut img);
            }
            assert!(img.px.iter().all(|v| v.is_finite()));
        }
    }

    /// Fork G3: a node with a Magic Mask matte changes only inside it; other nodes, and a
    /// matte for a node that isn't there, change nothing about the compile.
    #[test]
    fn a_magic_mask_matte_limits_only_its_node() {
        use crate::matte::BitmapMatte;
        use std::sync::Arc;
        let n = node(&[("offset", ParamValue::Color([0.1, 0.1, 0.1, 0.1]))]);
        let g = Grade { nodes: vec![n], bypass: false, ..Default::default() };
        // left half of the frame inside
        let left = Arc::new(BitmapMatte::new(2, 1, vec![255, 0]).unwrap());
        let run = |mattes: Option<&NodeMattes>| {
            let fx = compile_fx(&g, Tick(0), WorkingSpace::Rec709, GradeCtx { mattes, ..GradeCtx::default() }, 8, 2);
            let mut img = crate::Image::new(8, 2);
            img.px.iter_mut().enumerate().for_each(|(i, v)| *v = if i % 4 == 3 { 1.0 } else { 0.5 });
            for op in &fx {
                op.apply(&mut img);
            }
            (fx, img)
        };
        let (plain, whole) = run(None);
        assert!(!plain.iter().any(|o| matches!(o, FxOp::MaskedBy { .. })));
        let other = NodeMattes([(99, left.clone())].into_iter().collect());
        assert_eq!(run(Some(&other)).1.px, whole.px, "a matte for another node changes nothing");
        let mine = NodeMattes([(1, left)].into_iter().collect());
        let (fx, img) = run(Some(&mine));
        assert!(matches!(fx.first(), Some(FxOp::MaskedBy { .. })));
        let px = |im: &crate::Image, x: usize| im.px[x * 4];
        assert!((px(&img, 0) - px(&whole, 0)).abs() < 1e-6, "inside: graded");
        assert!((px(&img, 7) - 0.5).abs() < 1e-6, "outside: untouched");
        assert!((px(&whole, 0) - 0.5).abs() > 1e-3, "the node does change the picture");
        // an invalid matte (fields are public) is ignored, not trusted
        let bad = NodeMattes([(1, Arc::new(BitmapMatte { width: 4, height: 4, alpha: vec![0; 3] }))].into_iter().collect());
        assert_eq!(run(Some(&bad)).1.px, whole.px);
    }

    #[test]
    fn hostile_qualifiers_stay_finite() {
        let mut n = qualified(&[]);
        for k in
            ["qual.hue", "qual.hueWidth", "qual.satLow", "qual.satHigh", "qual.lumLow", "qual.lumHigh", "qual.soft", "qual.blur", "qual.show", "qual.invert"]
        {
            n.params.insert(k.into(), Param::new(ParamValue::Float(f64::NAN)));
        }
        let mut m = qualified(&[]);
        for (i, k) in ["qual.hue", "qual.hueWidth", "qual.satLow", "qual.satHigh", "qual.lumLow", "qual.lumHigh", "qual.soft", "qual.blur", "qual.show"]
            .iter()
            .enumerate()
        {
            m.params.insert((*k).into(), Param::new(ParamValue::Float(if i % 2 == 0 { 1e300 } else { -1e300 })));
        }
        for n in [n, m] {
            let g = Grade { nodes: vec![n], bypass: false, ..Default::default() };
            for ws in [WorkingSpace::Rec709, WorkingSpace::DaVinciWide, WorkingSpace::Rec2100Pq] {
                for op in compile_fx(&g, Tick(0), ws, GradeCtx { px_scale: f32::NAN, ..GradeCtx::default() }, 0, 0) {
                    assert!(op.gpu_ok(), "{op:?}");
                }
                let ops = compile(&g, Tick(0), ws);
                let out = filmcraft_color::ops::apply_chain(&ops, [0.5, 0.2, 0.1]);
                assert!(out.iter().all(|v| v.is_finite()), "{out:?}");
            }
        }
    }

    #[test]
    fn half_resolution_pixels_land_in_the_full_resolution_cells() {
        use filmcraft_color::ops::grain_noise;
        // size 2 at full resolution, 1 at half: half-res pixel x covers full-res 2x and 2x + 1
        for y in 0..20u32 {
            for x in 0..20u32 {
                let half = grain_noise([x as f32, y as f32], 1.0, 0.0, 77);
                for (fx, fy) in [(2 * x, 2 * y), (2 * x + 1, 2 * y), (2 * x, 2 * y + 1), (2 * x + 1, 2 * y + 1)] {
                    assert_eq!(grain_noise([fx as f32, fy as f32], 0.5, 0.0, 77), half, "{x} {y}");
                }
            }
        }
    }

    fn warped(f: impl FnOnce(&mut filmcraft_project::warper::Warper)) -> GradeNode {
        let mut n = GradeNode::new(1);
        let mut w = filmcraft_project::warper::Warper::default();
        f(&mut w);
        n.warper = Some(w);
        n
    }

    #[test]
    fn an_untouched_warper_compiles_to_nothing() {
        use filmcraft_project::warper::WarpMode;
        let pinned_only = warped(|w| {
            w.hue_sat.set_point(WarpMode::HueSat, 3, w.hue_sat.original(WarpMode::HueSat, 3).unwrap(), Some(true));
        });
        let weak = warped(|w| {
            w.strength = 0.0;
            w.hue_sat.set_point(WarpMode::HueSat, 3, [0.5, 0.5], None);
        });
        for n in [warped(|_| {}), pinned_only, weak] {
            let g = Grade { nodes: vec![n.clone()], ..Default::default() };
            assert!(compile(&g, Tick(0), WorkingSpace::DaVinciWide).is_empty());
            assert!(!node_changes(&n, Tick(0), None));
        }
    }

    #[test]
    fn the_warper_sits_after_saturation_and_before_the_curves() {
        use filmcraft_project::warper::WarpMode;
        let mut n = warped(|w| {
            w.hue_sat.set_point(WarpMode::HueSat, 8, [0.1, 0.5], None);
            w.chroma_luma.set_point(WarpMode::ChromaLuma, 22, [0.5, 0.6], None);
        });
        n.params.insert("saturation".into(), Param::new(ParamValue::Float(1.2)));
        n.params.insert("curve.master".into(), Param::new(ParamValue::Curve(vec![[0.0, 0.1], [1.0, 1.0]])));
        assert!(node_changes(&n, Tick(0), None));
        let ops = compile(&Grade { nodes: vec![n], ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide);
        let kinds: Vec<&str> = ops
            .iter()
            .filter_map(|o| match o {
                ColorOp::Saturation { .. } => Some("sat"),
                ColorOp::Warp2D(w) if w.space() == filmcraft_color::warp::WarpSpace::HueSat => Some("hs"),
                ColorOp::Warp2D(_) => Some("cl"),
                ColorOp::Curve { .. } => Some("curve"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["sat", "hs", "cl", "curve"]);
        // a red of saturation ½ (the moved hue-sat point) turns towards hue 0.1
        let n = warped(|w| {
            w.hue_sat.set_point(WarpMode::HueSat, 8, [0.1, 0.5], None);
        });
        let ops = warp_ops(&n);
        let red = filmcraft_color::warp::hsv_to_rgb(0.0, 0.5, 0.6);
        let o = filmcraft_color::ops::apply_chain(&ops, red);
        assert!(o[1] > red[1] + 0.05 && (o[0] - red[0]).abs() < 1e-4, "{red:?} → {o:?}");
    }

    /// Fork G4.3: a node lifting the gain, keyed by a 3D qualifier picked on `picked`.
    fn picked3d(ws: WorkingSpace, picked: &[[f32; 3]], f: impl FnOnce(&mut filmcraft_project::qual3d::Qual3d)) -> GradeNode {
        let mut n = node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5])), ("qual.on", ParamValue::Float(1.0))]);
        let mut q = filmcraft_project::qual3d::Qual3d::default();
        let pts: Vec<[f32; 3]> = picked.iter().filter_map(|c| qual3d_point(ws, *c)).collect();
        q.add_samples(&pts, false);
        f(&mut q);
        n.qual3d = Some(q);
        n
    }

    fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn a_3d_qualifier_limits_the_node_to_the_picked_colours() {
        for ws in SPACES {
            let one = |n: GradeNode| compile(&Grade { nodes: vec![n], ..Default::default() }, Tick(0), ws);
            let plain = one(node(&[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5]))]));
            let ops = one(picked3d(ws, &[RED], |_| {}));
            assert!(ops.iter().any(|o| matches!(o, ColorOp::Key3d(_))) && !ops.iter().any(|o| matches!(o, ColorOp::HslKey { .. })));
            let ap = filmcraft_color::ops::apply_chain;
            assert!(close3(ap(&ops, RED), ap(&plain, RED)), "{ws:?}");
            for c in [BLUE, GREY] {
                assert!(close3(ap(&ops, c), c), "{ws:?}: {c:?}");
            }
            // inverted: everything but the red
            let mut inv = picked3d(ws, &[RED], |_| {});
            inv.params.insert("qual.invert".into(), Param::new(ParamValue::Float(1.0)));
            let inv = one(inv);
            assert!(close3(ap(&inv, RED), RED) && close3(ap(&inv, GREY), ap(&plain, GREY)));
            // an exclude stroke on the red takes it out again; no picked colour selects nothing
            let ex = one(picked3d(ws, &[RED, BLUE], |q| {
                q.add_samples(&[qual3d_point(ws, RED).unwrap()], true);
            }));
            assert!(close3(ap(&ex, RED), RED) && close3(ap(&ex, BLUE), ap(&plain, BLUE)));
            let none = one(picked3d(ws, &[], |_| {}));
            assert!(close3(ap(&none, RED), RED));
        }
    }

    #[test]
    fn the_hsl_mode_ignores_the_3d_samples_and_qual_off_ignores_both() {
        let hsl = qualified(&[]);
        let mut with = hsl.clone();
        let mut q = filmcraft_project::qual3d::Qual3d { mode: filmcraft_project::qual3d::QualMode::Hsl, despill: 1.0, ..Default::default() };
        q.add_samples(&[[0.1, 0.1, 0.1]], false);
        with.qual3d = Some(q);
        let c = |n: GradeNode| compile(&Grade { nodes: vec![n], ..Default::default() }, Tick(0), WorkingSpace::DaVinciWide);
        assert_eq!(c(hsl), c(with));
        let mut off = picked3d(WorkingSpace::DaVinciWide, &[RED], |q| q.despill = 1.0);
        off.params.remove("qual.on");
        off.params.remove("gain");
        assert!(c(off.clone()).is_empty());
        assert!(!node_changes(&off, Tick(0), None));
    }

    #[test]
    fn despill_pulls_the_picked_chroma_out_inside_the_matte() {
        let ws = WorkingSpace::Rec709;
        // despill alone (no other correction) still changes the picture
        let mut n = picked3d(ws, &[RED], |q| q.despill = 1.0);
        n.params.remove("gain");
        assert!(node_changes(&n, Tick(0), None));
        let ops = compile(&Grade { nodes: vec![n], ..Default::default() }, Tick(0), ws);
        let ap = filmcraft_color::ops::apply_chain;
        let r = ap(&ops, RED);
        let spread = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
        assert!(spread(r) < spread(RED) * 0.5, "{r:?}");
        assert!(close3(ap(&ops, BLUE), BLUE) && close3(ap(&ops, GREY), GREY), "outside the matte: untouched");
        // despill is the node's first correction
        let n = picked3d(ws, &[RED], |q| q.despill = 0.5);
        let ops = compile(&Grade { nodes: vec![n], ..Default::default() }, Tick(0), ws);
        let i = ops.iter().position(|o| matches!(o, ColorOp::Despill { .. })).unwrap();
        let j = ops.iter().position(|o| matches!(o, ColorOp::Lggo { .. })).unwrap();
        let k = ops.iter().position(|o| matches!(o, ColorOp::Key3d(_))).unwrap();
        assert!(k < i && i < j, "{ops:?}");
    }

    #[test]
    fn a_blurred_3d_key_is_an_hsl_secondary_step_and_hostile_pixels_stay_safe() {
        let ws = WorkingSpace::DaVinciWide;
        let mut n = picked3d(ws, &[RED], |_| {});
        n.params.insert("qual.blur".into(), Param::new(ParamValue::Float(0.5)));
        let g = Grade { nodes: vec![n], ..Default::default() };
        let steps = compile_fx(&g, Tick(0), ws, GradeCtx::default(), 16, 8);
        assert!(steps.iter().any(|s| matches!(s, FxOp::HslSecondary { key, .. } if key.iter().any(|o| matches!(o, ColorOp::Key3d(_))))), "{steps:?}");
        let mut img = crate::image::Image::new(16, 8);
        for (i, p) in img.px.chunks_mut(4).enumerate() {
            let c = match i % 4 {
                0 => RED,
                1 => [f32::NAN, 0.1, 0.1],
                2 => [f32::INFINITY, 0.0, 0.0],
                _ => GREY,
            };
            p.copy_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
        for s in &steps {
            s.apply(&mut img);
        }
        for (i, p) in img.px.chunks(4).enumerate() {
            if i % 4 == 3 {
                assert!(p.iter().all(|v| v.is_finite()), "a hostile neighbour doesn't spread: {p:?}");
            }
        }
        assert!(qual3d_point(ws, [f32::NAN, 0.0, 0.0]).is_none());
    }
}
