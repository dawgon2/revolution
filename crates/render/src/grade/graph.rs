//! A branched grade as passes on texture slots (fork C2, `docs/fork/DESIGN-C2.md` §2): the grade's
//! [`GradeProgram`], mirroring C1's [`crate::comp::Program`]. Slots are handed out by liveness
//! ([`Order::last_use`]) and capped at [`MAX_LIVE_TEXTURES`] (a graph needing more is refused and
//! renders ungraded). Every pass is existing maths: a correction node's own steps are the serial
//! grade's ([`super::compile_fx`] for that node, limited by its Magic Mask matte as there), its
//! matte is its qualifier's key times its windows' coverage times its Magic Mask matte (fork G3),
//! the layer mixer is the Merge blend ([`crate::blend`]).
//!
//! The program encodes the grade's input to the grading signal once at its start and decodes its
//! result once at its end, as the serial chain does, so branches mix on the signal a colourist
//! grades (Resolve also mixes there). [`execute_graph_cpu`] is the CPU oracle (and the CPU
//! renderer: [`FxOp::GradeGraph`]'s `apply`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use filmcraft_color::ops::ColorOp;
use filmcraft_color::{GradeSpace, WorkingSpace};
use filmcraft_project::dag::{NodeId, Order, Port, Step};
use filmcraft_project::grade::{Grade, GradeNode, GraphError, MAX_MIX_INPUTS, MAX_NODES, MAX_WINDOWS, NodeKind, SOURCE};
use filmcraft_time::Tick;
use rayon::prelude::*;

use super::{GradeCtx, HDR_WHITE_NITS, QUAL_MAX_BLUR, key_op, parts, serial_steps, step_finite};
use crate::blend::{self, Blend};
use crate::comp::{MAX_LIVE_TEXTURES, Tex};
use crate::gpufx::{self, FxOp};
use crate::image::Image;
use crate::mask::FlatMask;
use crate::matte::BitmapMatte;

/// How a key mixer input combines with the inputs before it (`kmix.mode.<i>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMode {
    /// Union: the keys add (clamped to 1).
    Add,
    /// This key is taken away.
    Subtract,
    /// Only where both select (product).
    Intersect,
}

impl KeyMode {
    /// From the parameter value: 0 add, 1 subtract, 2 intersect (anything else: add).
    fn from_value(v: f32) -> Self {
        match v.round() as i32 {
            1 => KeyMode::Subtract,
            2 => KeyMode::Intersect,
            _ => KeyMode::Add,
        }
    }
}

/// One step of a grade program. Pictures are on the grading signal (premultiplied); keys are grey
/// pictures (the key in R, G and B, alpha 1).
#[derive(Clone, Debug, PartialEq)]
pub enum GradePass {
    /// `dst` = the grade's input picture.
    Source { dst: Tex },
    /// `dst` = one premultiplied colour everywhere (an unconnected input: transparent).
    Fill { dst: Tex, color: [f32; 4] },
    /// `dst` = `src` (before an in-place pass on a picture another node still needs).
    Copy { src: Tex, dst: Tex },
    /// A correction node's own steps on `tex` in place (the serial grade's steps for that node,
    /// on the grading signal).
    Fx { tex: Tex, ops: Vec<FxOp> },
    /// `dst` = the key of `src`: `key` (an HSL key; None: everything) with its matte blurred by box
    /// passes of radii `blur` (rows, then columns), inverted, times the coverage of `masks` (the
    /// node's windows; empty: everywhere), times `bitmap` (the node's Magic Mask matte, fork G3;
    /// None: everywhere). A node's key output.
    Matte { src: Tex, dst: Tex, key: Option<Arc<[ColorOp]>>, blur: Vec<u32>, invert: bool, masks: Arc<[FlatMask]>, bitmap: Option<Arc<BitmapMatte>> },
    /// The key `dst` times an incoming key, inverted (when `invert`), × `gain` + `offset`,
    /// clamped to 0…1: a correction node's key input (Resolve's outside node inverts it).
    KeyIn { key: Tex, dst: Tex, invert: bool, gain: f32, offset: f32 },
    /// `dst` (a corrected copy of `orig`) mixed with `orig` by the key `matte`.
    Limit { orig: Tex, dst: Tex, matte: Tex },
    /// `dst` = the weighted sum of `srcs` (parallel mixer); alpha their mean.
    Mix { srcs: Vec<(Tex, f32)>, dst: Tex },
    /// `src` composited over `dst` (layer mixer: C1's Merge).
    Over { src: Tex, dst: Tex, mode: Blend, opacity: f32 },
    /// `dst` = channel `which` (0 R, 1 G, 2 B) of `src` as a grey picture (splitter).
    Channel { src: Tex, dst: Tex, which: u8 },
    /// `dst` = R of `srcs[0]`, G of `srcs[1]`, B of `srcs[2]` (missing: 0), alpha of the first
    /// present one (combiner).
    Combine { srcs: [Option<Tex>; 3], dst: Tex },
    /// `dst` = the keys of `srcs` combined in order (each inverted first when its flag is set; the
    /// first sets the key, the next ones combine by their mode).
    KeyMix { srcs: Vec<(Tex, KeyMode, bool)>, dst: Tex },
}

/// A branched grade ready to run: passes on `textures` slots of `size`, the result in `out`
/// (working linear light again).
#[derive(Clone, Debug, PartialEq)]
pub struct GradeProgram {
    pub size: (usize, usize),
    /// Peak number of slots used at once.
    pub textures: u16,
    pub passes: Vec<GradePass>,
    pub out: Tex,
}

impl GradeProgram {
    /// Every number of every pass is finite (a program that isn't renders as no grade).
    pub fn is_finite(&self) -> bool {
        let fin = |v: &[f32]| v.iter().all(|x| x.is_finite());
        self.passes.iter().all(|p| match p {
            GradePass::Fx { ops, .. } => ops.iter().all(step_finite),
            GradePass::Fill { color, .. } => fin(color),
            GradePass::Matte { key, .. } => key.as_ref().is_none_or(|k| k.iter().all(ColorOp::is_finite)),
            GradePass::KeyIn { gain, offset, .. } => fin(&[*gain, *offset]),
            GradePass::Mix { srcs, .. } => srcs.iter().all(|(_, w)| w.is_finite()),
            GradePass::Over { opacity, .. } => opacity.is_finite(),
            _ => true,
        })
    }

    /// Whether the GPU effect stage runs this program (`crates/gpu` `fx/graph.rs`): finite, within
    /// [`MAX_LIVE_TEXTURES`], one [`GradePass::Source`] first, every node's steps GPU-capable
    /// (no nested graph or temporal op), keys and windows within the shader's limits. Anything
    /// else renders its clip on the CPU ([`execute_graph_cpu`]).
    pub fn gpu_ok(&self) -> bool {
        use filmcraft_color::ops::{MAX_GPU_PROGRAM, gpu_size};
        self.is_finite()
            && self.textures <= MAX_LIVE_TEXTURES
            && matches!(self.passes.first(), Some(GradePass::Source { .. }))
            && self.passes.iter().filter(|p| matches!(p, GradePass::Source { .. })).count() == 1
            && self.passes.iter().all(|p| match p {
                GradePass::Fx { ops, .. } => ops.iter().all(|o| !matches!(o, FxOp::GradeGraph(_) | FxOp::TemporalDenoise { .. }) && o.gpu_ok()),
                GradePass::Matte { key, blur, masks, .. } => {
                    key.as_ref().is_none_or(|k| k.iter().all(ColorOp::is_finite) && gpu_size(k) <= MAX_GPU_PROGRAM)
                        && blur.len() <= MAX_MATTE_BOXES
                        && gpufx::masks_ok(masks)
                }
                GradePass::Mix { srcs, .. } => srcs.len() <= usize::from(MAX_MIX_INPUTS),
                GradePass::KeyMix { srcs, .. } => srcs.len() <= usize::from(MAX_MIX_INPUTS),
                _ => true,
            })
    }
}

/// Most box passes per axis a matte's blur takes on the GPU (`gaussian_boxes` makes three).
pub const MAX_MATTE_BOXES: usize = 8;

/// Why a grade's graph doesn't render (it then renders ungraded).
#[derive(Debug, Clone, PartialEq)]
pub enum GradeProgramError {
    Graph(GraphError),
    TooManyTextures,
    NotFinite,
}

impl std::fmt::Display for GradeProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GradeProgramError::Graph(e) => e.fmt(f),
            GradeProgramError::TooManyTextures => write!(f, "the grade needs more than {MAX_LIVE_TEXTURES} pictures at once"),
            GradeProgramError::NotFinite => write!(f, "a grade setting isn't a finite number"),
        }
    }
}

impl std::error::Error for GradeProgramError {}

/// How a grade with edges compiles.
pub(super) enum Plan<'a> {
    /// A plain chain of correction nodes: the enabled ones in order (the serial grade's steps).
    Chain(Vec<&'a GradeNode>),
    /// A real graph, in evaluation order.
    Graph(Order),
}

/// The grade's graph checked ([`Grade::graph_order`]) and classified.
pub(super) fn plan(grade: &Grade) -> Result<Plan<'_>, GraphError> {
    let order = grade.graph_order()?;
    Ok(match chain(grade, &order) {
        Some(nodes) => Plan::Chain(nodes),
        None => Plan::Graph(order),
    })
}

/// The enabled nodes of `order` when it is the input → a → b → … chain of correction nodes, each
/// fed only on its picture input by the one before.
fn chain<'a>(grade: &'a Grade, order: &Order) -> Option<Vec<&'a GradeNode>> {
    let nodes: BTreeMap<NodeId, &GradeNode> = grade.nodes.iter().take(MAX_NODES).map(|n| (n.id, n)).collect();
    let mut steps = order.steps.iter();
    if steps.next().map(|s| s.node) != Some(SOURCE) {
        return None;
    }
    let mut prev = SOURCE;
    let mut out = Vec::new();
    for s in steps {
        let n = nodes.get(&s.node)?;
        let fed = s.inputs.iter().enumerate().all(|(i, p)| if i == 0 { *p == Some(Port::new(prev, 0)) } else { p.is_none() });
        if n.kind != NodeKind::Correction || !fed {
            return None;
        }
        if n.enabled {
            out.push(*n);
        }
        prev = n.id;
    }
    Some(out)
}

/// Texture slots handed out by liveness: a node's slots are reused once its last consumer ran.
struct Slots {
    free: Vec<Tex>,
    next: Tex,
    peak: u16,
}

impl Slots {
    fn take(&mut self) -> Result<Tex, GradeProgramError> {
        if let Some(t) = self.free.pop() {
            return Ok(t);
        }
        if self.next >= MAX_LIVE_TEXTURES {
            return Err(GradeProgramError::TooManyTextures);
        }
        let t = self.next;
        self.next += 1;
        self.peak = self.peak.max(self.next);
        Ok(t)
    }
    fn give(&mut self, t: Tex) {
        if !self.free.contains(&t) {
            self.free.push(t);
        }
    }
}

/// A program being built: its passes, slots and the slot of every node output made so far.
struct Builder {
    slots: Slots,
    passes: Vec<GradePass>,
    result: BTreeMap<Port, Tex>,
}

impl Builder {
    /// The slot feeding input `i` of `step` (None: unconnected).
    fn input(&self, step: &Step, i: usize) -> Option<Tex> {
        step.inputs.get(i).copied().flatten().and_then(|p| self.result.get(&p).copied())
    }

    fn fill(&mut self, color: [f32; 4]) -> Result<Tex, GradeProgramError> {
        let dst = self.slots.take()?;
        self.passes.push(GradePass::Fill { dst, color });
        Ok(dst)
    }

    /// A new slot holding input `i` (transparent when unconnected).
    fn copy_of(&mut self, step: &Step, i: usize) -> Result<Tex, GradeProgramError> {
        match self.input(step, i) {
            Some(src) => {
                let dst = self.slots.take()?;
                self.passes.push(GradePass::Copy { src, dst });
                Ok(dst)
            }
            None => self.fill([0.0; 4]),
        }
    }

    /// A slot this step may overwrite holding input `i`: the input's own slot when nothing later
    /// reads its node and this step reads it once, otherwise a copy.
    fn writable(&mut self, step: &Step, i: usize, pos: usize, last_use: &BTreeMap<NodeId, usize>) -> Result<Tex, GradeProgramError> {
        let port = step.inputs.get(i).copied().flatten();
        match (self.input(step, i), port) {
            (Some(src), Some(p)) if last_use.get(&p.node).copied() == Some(pos) && step.inputs.iter().flatten().filter(|q| **q == p).count() == 1 => Ok(src),
            _ => self.copy_of(step, i),
        }
    }
}

/// `grade` (a real graph: `order` from [`plan`]) at clip time `t` as a program for a `w`×`h`
/// image (see [`super::compile_fx`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn compile(
    grade: &Grade,
    order: &Order,
    t: Tick,
    working: WorkingSpace,
    ctx: GradeCtx,
    w: usize,
    h: usize,
) -> Result<GradeProgram, GradeProgramError> {
    let output = grade.output_node().map_err(GradeProgramError::Graph)?;
    let gs = GradeSpace::new(working, HDR_WHITE_NITS);
    let px_scale = if ctx.px_scale.is_finite() && ctx.px_scale > 0.0 { ctx.px_scale } else { 1.0 };
    let ctx = GradeCtx { px_scale, ..ctx };
    let nodes: BTreeMap<NodeId, &GradeNode> = grade.nodes.iter().take(MAX_NODES).map(|n| (n.id, n)).collect();
    // the outputs some step reads, and the result: only those are made
    let mut wanted: BTreeSet<Port> = order.steps.iter().flat_map(|s| s.inputs.iter().flatten().copied()).collect();
    wanted.insert(Port::new(output, 0));
    let last_use = order.last_use();
    let mut b = Builder { slots: Slots { free: Vec::new(), next: 0, peak: 0 }, passes: Vec::new(), result: BTreeMap::new() };
    let cx = NodeCx { t, gs: &gs, ctx, w, h };

    for (pos, step) in order.steps.iter().enumerate() {
        let made = if step.node == SOURCE {
            let dst = b.slots.take()?;
            b.passes.push(GradePass::Source { dst });
            b.passes.push(GradePass::Fx { tex: dst, ops: vec![FxOp::ColorChain(gs.encode_ops().into())] });
            vec![(Port::new(SOURCE, 0), dst)]
        } else {
            let Some(node) = nodes.get(&step.node) else { continue };
            let want = |i: u8| wanted.contains(&Port::new(node.id, i));
            let mut made = Vec::new();
            if !node.enabled {
                // a bypassed node passes its picture through on every picture output (a key
                // output is white: everything)
                for i in (0..node.kind.outputs()).rev().filter(|&i| want(i)) {
                    let tex = match (node.kind, i) {
                        (NodeKind::Correction, 1) => b.fill([1.0; 4])?,
                        (_, 0) => b.writable(step, 0, pos, &last_use)?,
                        _ => b.copy_of(step, 0)?,
                    };
                    made.push((Port::new(node.id, i), tex));
                }
            } else {
                match node.kind {
                    NodeKind::Correction => correction(&mut b, node, step, pos, &last_use, &cx, want(0), want(1), &mut made)?,
                    NodeKind::ParallelMixer => {
                        let srcs = connected(&b, step);
                        let tex = if srcs.is_empty() {
                            b.fill([0.0; 4])?
                        } else {
                            let srcs = mix_weights(node, &srcs, t);
                            let dst = b.slots.take()?;
                            b.passes.push(GradePass::Mix { srcs, dst });
                            dst
                        };
                        made.push((Port::new(node.id, 0), tex));
                    }
                    NodeKind::LayerMixer => {
                        let srcs = connected(&b, step);
                        let tex = match srcs.split_first() {
                            None => b.fill([0.0; 4])?,
                            Some((&(base, _), above)) => {
                                let dst = b.writable(step, usize::from(base), pos, &last_use)?;
                                for &(i, src) in above {
                                    let mode = Blend::from_index(node.float_at(&format!("layer.mode.{i}"), t).round() as u32);
                                    let opacity = node.float_at(&format!("layer.opacity.{i}"), t);
                                    b.passes.push(GradePass::Over { src, dst, mode, opacity });
                                }
                                dst
                            }
                        };
                        made.push((Port::new(node.id, 0), tex));
                    }
                    NodeKind::Splitter => {
                        for which in (0..3u8).filter(|&i| want(i)) {
                            let tex = match b.input(step, 0) {
                                Some(src) => {
                                    let dst = b.slots.take()?;
                                    b.passes.push(GradePass::Channel { src, dst, which });
                                    dst
                                }
                                None => b.fill([0.0; 4])?,
                            };
                            made.push((Port::new(node.id, which), tex));
                        }
                    }
                    NodeKind::Combiner => {
                        let srcs = [b.input(step, 0), b.input(step, 1), b.input(step, 2)];
                        let dst = b.slots.take()?;
                        b.passes.push(GradePass::Combine { srcs, dst });
                        made.push((Port::new(node.id, 0), dst));
                    }
                    NodeKind::KeyMixer => {
                        let srcs: Vec<(Tex, KeyMode, bool)> = connected(&b, step)
                            .into_iter()
                            .map(|(i, tex)| {
                                (tex, KeyMode::from_value(node.float_at(&format!("kmix.mode.{i}"), t)), node.float_at(&format!("kmix.invert.{i}"), t) >= 0.5)
                            })
                            .collect();
                        let dst = b.slots.take()?;
                        b.passes.push(GradePass::KeyMix { srcs, dst });
                        made.push((Port::new(node.id, 0), dst));
                    }
                }
            }
            made
        };
        let outs: BTreeSet<Tex> = made.iter().map(|(_, t)| *t).collect();
        // inputs whose last consumer was this step free their slots (unless this node took one)
        let done: BTreeSet<NodeId> = step.inputs.iter().flatten().map(|p| p.node).filter(|n| last_use.get(n).copied() == Some(pos)).collect();
        let freed: Vec<Port> = b.result.keys().filter(|p| done.contains(&p.node)).copied().collect();
        for p in freed {
            if let Some(s) = b.result.remove(&p)
                && !outs.contains(&s)
            {
                b.slots.give(s);
            }
        }
        b.result.extend(made);
    }
    let out = b.result.get(&Port::new(output, 0)).copied().ok_or(GradeProgramError::Graph(GraphError::Dag(filmcraft_project::dag::DagError::NoOutput)))?;
    b.passes.push(GradePass::Fx { tex: out, ops: vec![FxOp::ColorChain(gs.decode_ops().into())] });
    let p = GradeProgram { size: (w, h), textures: b.slots.peak, passes: b.passes, out };
    if p.is_finite() { Ok(p) } else { Err(GradeProgramError::NotFinite) }
}

/// What a node's steps are compiled for.
struct NodeCx<'a> {
    t: Tick,
    gs: &'a GradeSpace,
    ctx: GradeCtx<'a>,
    w: usize,
    h: usize,
}

/// The connected inputs of a mixer step, by input index.
fn connected(b: &Builder, step: &Step) -> Vec<(u8, Tex)> {
    (0..MAX_MIX_INPUTS).filter_map(|i| b.input(step, usize::from(i)).map(|tex| (i, tex))).collect()
}

/// A parallel mixer's inputs with their weights: each input's `mix.gain.<i>` (missing: 1) over the
/// sum of the connected inputs' gains, so the weights add up to 1 (all equal by default; an equal
/// share when the gains sum to 0).
fn mix_weights(node: &GradeNode, srcs: &[(u8, Tex)], t: Tick) -> Vec<(Tex, f32)> {
    let gains: Vec<f32> = srcs.iter().map(|(i, _)| node.float_at(&format!("mix.gain.{i}"), t)).collect();
    let sum: f32 = gains.iter().sum();
    let share = 1.0 / srcs.len().max(1) as f32;
    srcs.iter().zip(gains).map(|(&(_, tex), g)| (tex, if sum.is_finite() && sum > 0.0 { g / sum } else { share })).collect()
}

/// A correction node: its picture output (port 0; its own steps, or limited by its key input) and
/// its key output (port 1: qualifier × windows, times the key input).
#[allow(clippy::too_many_arguments)]
fn correction(
    b: &mut Builder,
    node: &GradeNode,
    step: &Step,
    pos: usize,
    last_use: &BTreeMap<NodeId, usize>,
    cx: &NodeCx,
    want_rgb: bool,
    want_key: bool,
    made: &mut Vec<(Port, Tex)>,
) -> Result<(), GradeProgramError> {
    let t = cx.t;
    let key_in = b.input(step, 1);
    // an unconnected picture input reads transparent
    let (src, temp) = match b.input(step, 0) {
        Some(src) => (src, false),
        None => (b.fill([0.0; 4])?, true),
    };
    // the key reads the node's input after its noise reduction, as the serial grade keys it
    let needs_matte = want_key || (want_rgb && key_in.is_some());
    let key_src = match super::denoise(node, t, cx.ctx.px_scale) {
        Some(nr) if needs_matte => {
            let k = b.slots.take()?;
            b.passes.push(GradePass::Copy { src, dst: k });
            let ops = vec![FxOp::ColorChain(cx.gs.decode_ops().into()), nr, FxOp::ColorChain(cx.gs.encode_ops().into())];
            b.passes.push(GradePass::Fx { tex: k, ops });
            Some(k)
        }
        _ => None,
    };
    let matte = |b: &mut Builder| -> Result<Tex, GradeProgramError> {
        let dst = b.slots.take()?;
        let (key, blur, invert, masks) = matte_of(node, cx);
        let bitmap = bitmap_of(node, cx);
        b.passes.push(GradePass::Matte { src: key_src.unwrap_or(src), dst, key, blur, invert, masks, bitmap });
        if let Some(key) = key_in {
            let (invert, gain, offset) = (node.float_at("key.invert", t) >= 0.5, node.float_at("key.gain", t), node.float_at("key.offset", t));
            b.passes.push(GradePass::KeyIn { key, dst, invert, gain, offset });
        }
        Ok(dst)
    };
    // (the key reads the node's input, so it is made before the picture is corrected in place)
    let key_out = if want_key { Some(matte(b)?) } else { None };
    if want_rgb {
        let tex = match key_in {
            None => {
                let tex = if temp { src } else { b.writable(step, 0, pos, last_use)? };
                let ops = serial_steps(parts(vec![node], t, cx.gs, cx.ctx), t, cx.gs, cx.ctx, cx.w, cx.h, true);
                if !ops.is_empty() {
                    b.passes.push(GradePass::Fx { tex, ops });
                }
                tex
            }
            Some(_) => {
                // the node's correction everywhere, then mixed in by its own key (qualifier ×
                // windows × Magic Mask matte) times the key in
                let tex = b.slots.take()?;
                b.passes.push(GradePass::Copy { src, dst: tex });
                let mut free = node.clone();
                free.windows.clear();
                free.params.remove("qual.on");
                let ctx = GradeCtx { mattes: None, ..cx.ctx };
                let ops = serial_steps(parts(vec![&free], t, cx.gs, ctx), t, cx.gs, ctx, cx.w, cx.h, true);
                if !ops.is_empty() {
                    b.passes.push(GradePass::Fx { tex, ops });
                }
                let m = match key_out {
                    Some(m) => m,
                    None => matte(b)?,
                };
                b.passes.push(GradePass::Limit { orig: src, dst: tex, matte: m });
                if key_out.is_none() {
                    b.slots.give(m);
                }
                tex
            }
        };
        made.push((Port::new(node.id, 0), tex));
    }
    if let Some(m) = key_out {
        made.push((Port::new(node.id, 1), m));
    }
    if temp && !made.iter().any(|(_, t)| *t == src) {
        b.slots.give(src);
    }
    if let Some(k) = key_src {
        b.slots.give(k);
    }
    Ok(())
}

/// A node's matte at `cx.t`: its qualifier's key (None: off) with the blur radii and invert the
/// node's own correction uses, and its windows.
fn matte_of(n: &GradeNode, cx: &NodeCx) -> (Option<Arc<[ColorOp]>>, Vec<u32>, bool, Arc<[FlatMask]>) {
    let px_scale = cx.ctx.px_scale;
    let windows = n.windows.get(..n.windows.len().min(MAX_WINDOWS)).unwrap_or_default();
    let masks: Arc<[FlatMask]> = if windows.is_empty() { Arc::from(Vec::new()) } else { crate::mask::prepare(windows, cx.t, px_scale).into() };
    let Some(q) = n.qualifier_at(cx.t) else { return (None, Vec::new(), false, masks) };
    let sigma = q.blur * QUAL_MAX_BLUR * px_scale;
    let blur = if sigma >= 0.3 { crate::effects::gaussian_boxes(cx.w, cx.h, sigma, sigma).0 } else { Vec::new() };
    (Some(Arc::from(vec![key_op(n, &q, cx.gs.is_hdr(), cx.t)])), blur, q.invert, masks)
}

/// A node's Magic Mask matte for this frame (fork G3; None: none, or an invalid one, which the
/// serial grade ignores too).
fn bitmap_of(n: &GradeNode, cx: &NodeCx) -> Option<Arc<BitmapMatte>> {
    cx.ctx.mattes.and_then(|m| m.0.get(&n.id)).filter(|m| m.is_valid()).cloned()
}

/// Runs a grade program on `input` (working linear light) on the CPU: the oracle of the GPU passes
/// and the CPU renderer's [`FxOp::GradeGraph`]. A program for another size still runs (on the
/// input's size); an empty or malformed input comes back unchanged.
pub fn execute_graph_cpu(p: &GradeProgram, input: &Image) -> Image {
    let (w, h) = (input.w, input.h);
    if w == 0 || h == 0 || input.px.len() != w.saturating_mul(h).saturating_mul(4) {
        return input.clone();
    }
    let mut tex: Vec<Option<Image>> = vec![None; usize::from(p.textures.min(MAX_LIVE_TEXTURES))];
    let blank = || Image::new(w, h);
    for pass in &p.passes {
        match pass {
            GradePass::Source { dst } => put(&mut tex, *dst, input.clone()),
            GradePass::Fill { dst, color } => put(&mut tex, *dst, if *color == [0.0; 4] { blank() } else { Image::filled(w, h, *color) }),
            GradePass::Copy { src, dst } => {
                let img = get(&tex, *src).cloned().unwrap_or_else(blank);
                put(&mut tex, *dst, img);
            }
            GradePass::Fx { tex: t, ops } => {
                if let Some(img) = tex.get_mut(usize::from(*t)).and_then(Option::as_mut) {
                    gpufx::apply_ops(ops, img);
                }
            }
            GradePass::Matte { src, dst, key, blur, invert, masks, bitmap } => {
                let Some(s) = get(&tex, *src) else { continue };
                let mut m: Vec<f32> =
                    s.px.par_chunks(4)
                        .enumerate()
                        .map(|(i, px)| match (key, px) {
                            (None, _) => 1.0,
                            (Some(k), [r, g, b, a]) if *a > 1e-6 => {
                                let xy = [(i % w) as f32, (i / w) as f32];
                                filmcraft_color::ops::apply_chain_px(k, [r / a, g / a, b / a], xy, 1.0).1
                            }
                            _ => 0.0,
                        })
                        .collect();
                m.iter_mut().for_each(|v| *v = finite_key(*v));
                if !blur.is_empty() {
                    let radii: Vec<usize> = blur.iter().map(|r| *r as usize).collect();
                    crate::effects::blur_plane_radii(&mut m, w, h, &radii);
                }
                if *invert {
                    m.iter_mut().for_each(|v| *v = 1.0 - *v);
                }
                if let Some(cov) = crate::mask::coverage(masks, w, h) {
                    m.iter_mut().zip(cov).for_each(|(v, c)| *v *= c);
                }
                if let Some(cov) = bitmap.as_deref().and_then(|b| crate::matte::coverage(b, w, h)) {
                    m.iter_mut().zip(cov).for_each(|(v, c)| *v *= c);
                }
                put(&mut tex, *dst, grey(w, h, &m));
            }
            GradePass::KeyIn { key, dst, invert, gain, offset } => {
                let Some(k) = get(&tex, *key).cloned() else { continue };
                let Some(d) = tex.get_mut(usize::from(*dst)).and_then(Option::as_mut) else { continue };
                for ([r, g, b, _], [k, ..]) in d.px.as_chunks_mut::<4>().0.iter_mut().zip(k.px.as_chunks::<4>().0) {
                    let k = finite_key(*k);
                    let k = if *invert { 1.0 - k } else { k };
                    let f = (k * gain + offset).clamp(0.0, 1.0);
                    for v in [r, g, b] {
                        *v *= f;
                    }
                }
            }
            GradePass::Limit { orig, dst, matte } => {
                let (Some(o), Some(m)) = (get(&tex, *orig).cloned(), get(&tex, *matte).cloned()) else { continue };
                let Some(d) = tex.get_mut(usize::from(*dst)).and_then(Option::as_mut) else { continue };
                if (o.w, o.h, m.w, m.h) != (d.w, d.h, d.w, d.h) {
                    continue;
                }
                d.px.par_chunks_mut(4).zip(o.px.par_chunks(4)).zip(m.px.par_chunks(4)).for_each(|((dp, op), mp)| {
                    let m = finite_key(mp.first().copied().unwrap_or(0.0));
                    for (v, o) in dp.iter_mut().zip(op) {
                        *v = o + (*v - o) * m;
                    }
                });
            }
            GradePass::Mix { srcs, dst } => {
                let mut img = blank();
                let present: Vec<(&Image, f32)> = srcs.iter().filter_map(|(s, wt)| get(&tex, *s).map(|i| (i, *wt))).collect();
                let share = 1.0 / present.len().max(1) as f32;
                for (s, wt) in present {
                    img.px.par_chunks_mut(4).zip(s.px.par_chunks(4)).for_each(|(dp, sp)| {
                        for (k, (v, s)) in dp.iter_mut().zip(sp).enumerate() {
                            *v += s * if k == 3 { share } else { wt };
                        }
                    });
                }
                put(&mut tex, *dst, img);
            }
            GradePass::Over { src, dst, mode, opacity } => {
                let Some(s) = get(&tex, *src).cloned() else { continue };
                if let Some(d) = tex.get_mut(usize::from(*dst)).and_then(Option::as_mut)
                    && (d.w, d.h) == (s.w, s.h)
                {
                    blend::composite(d, &s, *opacity, *mode);
                }
            }
            GradePass::Channel { src, dst, which } => {
                let Some(s) = get(&tex, *src) else { continue };
                let px = s.px.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, a]| {
                    let v = match which {
                        0 => r,
                        1 => g,
                        _ => b,
                    };
                    [v, v, v, a]
                });
                let img = Image { w, h, px: px.collect() };
                put(&mut tex, *dst, img);
            }
            GradePass::Combine { srcs, dst } => {
                let mut img = blank();
                if let Some(first) = srcs.iter().flatten().find_map(|s| get(&tex, *s)) {
                    for (dp, sp) in img.px.as_chunks_mut::<4>().0.iter_mut().zip(first.px.as_chunks::<4>().0) {
                        dp[3] = sp[3];
                    }
                }
                for (c, s) in srcs.iter().enumerate() {
                    let Some(s) = s.and_then(|s| get(&tex, s)) else { continue };
                    for (dp, sp) in img.px.as_chunks_mut::<4>().0.iter_mut().zip(s.px.as_chunks::<4>().0) {
                        if let (Some(d), Some(s)) = (dp.get_mut(c), sp.get(c)) {
                            *d = *s;
                        }
                    }
                }
                put(&mut tex, *dst, img);
            }
            GradePass::KeyMix { srcs, dst } => {
                let mut acc = vec![0.0f32; w.saturating_mul(h)];
                let mut first = true;
                for (s, mode, invert) in srcs {
                    let Some(s) = get(&tex, *s) else { continue };
                    for (a, [k, ..]) in acc.iter_mut().zip(s.px.as_chunks::<4>().0) {
                        let k = finite_key(*k);
                        let k = if *invert { 1.0 - k } else { k };
                        *a = if first {
                            k
                        } else {
                            match mode {
                                KeyMode::Add => (*a + k).min(1.0),
                                KeyMode::Subtract => (*a - k).max(0.0),
                                KeyMode::Intersect => *a * k,
                            }
                        };
                    }
                    first = false;
                }
                put(&mut tex, *dst, grey(w, h, &acc));
            }
        }
    }
    tex.get_mut(usize::from(p.out)).and_then(Option::take).unwrap_or_else(|| input.clone())
}

/// A key value clamped to 0…1; a non-finite one (a NaN picture fed into a key input) is 0.
fn finite_key(k: f32) -> f32 {
    if k.is_finite() { k.clamp(0.0, 1.0) } else { 0.0 }
}

/// A key as a grey picture (alpha 1).
fn grey(w: usize, h: usize, m: &[f32]) -> Image {
    Image { w, h, px: m.iter().flat_map(|&v| [v, v, v, 1.0]).collect() }
}

fn get(tex: &[Option<Image>], t: Tex) -> Option<&Image> {
    tex.get(usize::from(t)).and_then(Option::as_ref)
}

fn put(tex: &mut [Option<Image>], t: Tex, img: Image) {
    if let Some(slot) = tex.get_mut(usize::from(t)) {
        *slot = Some(img);
    }
}
