//! Comp evaluation (fork C1, ADR 0007): a validated comp graph becomes a [`Program`] of passes on
//! comp-sized textures, which the CPU runs here ([`execute_cpu`], the oracle and export path) and
//! the GPU runs with the same passes (C1 step 7). Every pass is existing maths: blend modes
//! ([`crate::blend`]), the Motion matrix ([`Affine::motion`]), effect ops ([`FxOp`]: keys, blur,
//! masks, one grade node through [`crate::grade::compile_fx`]). The one new picture is the
//! gradient generator.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock, PoisonError};

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::comp::{Comp, CompKind, CompNode, MediaInSource, RotoOutput};
use filmcraft_project::dag::{DagError, NodeId, Port};
use filmcraft_project::grade::Grade;
use filmcraft_project::{EffectInstance, ParamValue, Project, find_effect};
use filmcraft_time::Tick;

use crate::blend::{self, Blend};
use crate::effects::{FxCtx, dec};
use crate::gpufx::{self, FxOp};
use crate::image::Image;

/// Most comp-sized textures a program may hold at once.
pub const MAX_LIVE_TEXTURES: u16 = 8;
/// Longest side of a generated gradient; larger comps upscale it (a gradient upscales exactly).
pub const GRADIENT_MAX_SIDE: usize = 2048;
/// How deep clone-from-another-frame sources nest (C3.10): a Paint node's input rendered at
/// another time may itself hold Paint nodes cloning from other times this many levels down;
/// deeper ones clone from the same time.
pub const MAX_CLONE_DEPTH: u8 = 2;
/// Most input sub-graphs one compile renders at other times for clone sources (C3.10, all
/// levels together) for a comp on the timeline, a quarter of that per level a comp is nested in
/// another comp or sequence ([`CompSources::nest_depth`]); Clone strokes past it clone from the
/// same time.
pub const MAX_CLONE_COMPILES: u32 = 16;

/// A texture slot of a program (comp-sized, premultiplied linear light).
pub type Tex = u16;

/// Why a comp can't be evaluated (it then renders transparent).
#[derive(Debug, Clone, PartialEq)]
pub enum CompError {
    Graph(DagError),
    TooManyTextures,
    Empty,
}

impl std::fmt::Display for CompError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompError::Graph(e) => e.fmt(f),
            CompError::TooManyTextures => write!(f, "the comp needs more than {MAX_LIVE_TEXTURES} pictures at once"),
            CompError::Empty => write!(f, "the comp is empty"),
        }
    }
}

impl std::error::Error for CompError {}

impl From<DagError> for CompError {
    fn from(e: DagError) -> Self {
        CompError::Graph(e)
    }
}

/// One step of a program.
#[derive(Clone, Debug)]
pub enum Pass {
    /// `dst` = a picture (Media In, Text, Gradient), already at the program size.
    Image { dst: Tex, image: Arc<Image> },
    /// `dst` = these frame-plan layers drawn on transparency (a Media In reading a track: its
    /// decoded or hardware frames, effects and Motion as on the timeline; C1 step 7d).
    Layers { dst: Tex, layers: Arc<[crate::plan::PlanLayer]> },
    /// `dst` = one premultiplied colour everywhere (Solid; transparent for a missing input).
    Fill { dst: Tex, color: [f32; 4] },
    /// `dst` = `src` (before an in-place pass on a picture another node still needs).
    Copy { src: Tex, dst: Tex },
    /// Effect ops on `tex` in place (Key, Mask, Blur, Color).
    Fx { tex: Tex, ops: Vec<FxOp> },
    /// `dst` = `src` drawn through `xform` (source pixels → destination pixels) on transparency.
    Draw { src: Tex, dst: Tex, xform: Affine, hq: bool },
    /// `src` composited over `dst` (Merge).
    Over { src: Tex, dst: Tex, mode: Blend, opacity: f32 },
    /// Paint strokes (C3.5b) on `tex` in place ([`crate::paint::apply`]: `tex` is also the clone
    /// and erase source, `reveal` the Reveal tools' source, `clones` the node's input rendered at
    /// other times for Clone strokes with a clone time offset, C3.10: (offset, slot), at most
    /// [`crate::paint::MAX_CLONE_TIMES`]). `paint` holds only the strokes active at `at`. The GPU
    /// lays the same dabs ([`crate::paint::plan`]) in a WGSL kernel (C3.6).
    Paint { tex: Tex, reveal: Option<Tex>, clones: Vec<(Tick, Tex)>, paint: Arc<filmcraft_project::paint::Paint>, at: Tick, frame: Tick, px_scale: f32 },
    /// `dst` = `src` drawn through the projective map `h` (source pixels → destination pixels) on
    /// transparency (Tracker / Planar Tracker corner pins; the GPU runs the same inverse map and
    /// bilinear rule in a kernel, C3.8).
    Warp { src: Tex, dst: Tex, h: crate::track::Homography },
    /// A Roto node's coverage (C3.6; `w·h` f32 values in 0..1, row-major, see [`Coverage`]):
    /// `matte` sets `tex` to `(c, c, c, 1)` (the Matte output), otherwise `tex` is multiplied by
    /// `c` in place (the Alpha output). A coverage of another length than the program's pixels
    /// leaves `tex` unchanged (matte: black).
    Coverage { tex: Tex, coverage: Arc<Coverage>, matte: bool },
}

/// A Roto node's coverage in a [`Program`]: the prepared (flattened) shapes, rasterised on the
/// CPU by [`crate::roto::rasterize`] the first time the CPU needs the values (LO.5: the GPU
/// measures the same edges in its own kernel and only asks for the raster when it can't), or a
/// raster given as it is.
#[derive(Debug)]
pub struct Coverage {
    roto: Option<crate::roto::PreparedRoto>,
    raster: OnceLock<Option<Arc<[f32]>>>,
}

impl Coverage {
    /// The coverage of `prepared` (`prepared.w × prepared.h` values once rasterised).
    pub fn from_roto(prepared: crate::roto::PreparedRoto) -> Coverage {
        Coverage { roto: Some(prepared), raster: OnceLock::new() }
    }

    /// The prepared shapes (None for a raster given as it is).
    pub fn roto(&self) -> Option<&crate::roto::PreparedRoto> {
        self.roto.as_ref()
    }

    /// The f32 values (rasterised on first use; None when the frame is empty or too large).
    pub fn raster(&self) -> Option<&Arc<[f32]>> {
        self.raster.get_or_init(|| self.roto.as_ref().and_then(|p| roto_coverage(p, p.w, p.h))).as_ref()
    }

    /// The memory it holds now.
    pub fn byte_size(&self) -> usize {
        let pts = self.roto.as_ref().map_or(0, |p| p.shapes.iter().map(|s| s.pts.len().saturating_mul(32)).fold(0, usize::saturating_add));
        let raster = self.raster.get().and_then(Option::as_ref).map_or(0, |r| r.len().saturating_mul(4));
        pts.saturating_add(raster)
    }
}

impl From<Arc<[f32]>> for Coverage {
    fn from(raster: Arc<[f32]>) -> Coverage {
        Coverage { roto: None, raster: OnceLock::from(Some(raster)) }
    }
}

/// A comp ready to run: passes on `textures` slots of `size`, the result in `out`.
#[derive(Clone, Debug)]
pub struct Program {
    pub size: (usize, usize),
    /// Peak number of slots used at once.
    pub textures: u16,
    pub passes: Vec<Pass>,
    pub out: Tex,
}

impl Program {
    /// The memory its pictures hold (the CPU images it carries; slots are allocated when it runs).
    pub fn byte_size(&self) -> usize {
        let size = |p: &Pass| match p {
            Pass::Image { image, .. } => image.px.len().saturating_mul(4),
            Pass::Layers { layers, .. } => layers.iter().map(|l| l.frame.byte_size()).fold(0, usize::saturating_add),
            Pass::Coverage { coverage, .. } => coverage.byte_size(),
            _ => 0,
        };
        self.passes.iter().map(size).fold(0, usize::saturating_add)
    }
}

/// Pictures a comp reads from outside itself.
pub trait CompSources {
    /// A Media In node's picture at comp time `t` (the node's offset already added), fitted into a
    /// `w`×`h` canvas (transparent outside). None: nothing there (offline, outside the clip).
    fn media_in(&self, source: &MediaInSource, t: Tick, w: usize, h: usize) -> Option<Image>;
    /// The same picture as frame-plan layers placed on a `w`×`h` canvas, when the source can give
    /// them (the GPU then draws the frames itself); None: use [`media_in`](Self::media_in).
    fn media_in_layers(&self, _source: &MediaInSource, _t: Tick, _w: usize, _h: usize) -> Option<Vec<crate::plan::PlanLayer>> {
        None
    }
    /// How many comps or sequences deep this comp is nested (0: drawn by the timeline itself).
    /// Clone-from-another-frame sources (C3.10) shrink with it, so that comps nested inside comps
    /// can't multiply each other's renders.
    fn nest_depth(&self) -> u32 {
        0
    }
    /// A text node's layer rasterised on a transparent `w`×`h` canvas (`px_scale` image pixels per
    /// comp pixel), exactly as a graphic clip draws it (layer rasters are cached by
    /// `graphic_clip`). None: nothing drawn.
    fn text(&self, layer: &EffectInstance, t: Tick, w: usize, h: usize, px_scale: f32) -> Option<Image> {
        let s = if px_scale.is_finite() && px_scale > 0.0 { px_scale } else { 1.0 };
        let canvas = (((w as f32 / s).round() as u32).max(1), ((h as f32 / s).round() as u32).max(1));
        let mut img = Image::new(w, h);
        crate::graphic_clip::render_graphic_layers(std::slice::from_ref(layer), t, canvas, &Affine::scale(f64::from(s), f64::from(s)), &mut img);
        Some(img)
    }
}

/// No outside pictures (tests, comps without Media In).
pub struct NoSources;

impl CompSources for NoSources {
    fn media_in(&self, _: &MediaInSource, _: Tick, _: usize, _: usize) -> Option<Image> {
        None
    }
}

/// Where a comp is evaluated.
#[derive(Clone, Copy)]
pub struct CompCtx<'a> {
    /// Comp time.
    pub t: Tick,
    /// The program's pixel size (the comp size × render scale; at least 1×1).
    pub size: (usize, usize),
    /// The project (LUT library of Color nodes).
    pub project: Option<&'a Project>,
    /// Maximum Render Quality for Transform draws.
    pub high_quality: bool,
}

/// Texture slots handed out by liveness: a node's slot is reused once its last consumer ran.
#[derive(Clone)]
struct Slots {
    free: Vec<Tex>,
    next: Tex,
    peak: u16,
}

impl Slots {
    fn take(&mut self) -> Result<Tex, CompError> {
        if let Some(t) = self.free.pop() {
            return Ok(t);
        }
        if self.next >= MAX_LIVE_TEXTURES {
            return Err(CompError::TooManyTextures);
        }
        let t = self.next;
        self.next += 1;
        self.peak = self.peak.max(self.next);
        Ok(t)
    }
    /// Slots that can still be taken.
    fn available(&self) -> usize {
        self.free.len().saturating_add(usize::from(MAX_LIVE_TEXTURES.saturating_sub(self.next)))
    }
    fn give(&mut self, t: Tex) {
        if !self.free.contains(&t) {
            self.free.push(t);
        }
    }
}

/// Compiles `comp` at `ctx.t` into a program of `ctx.size` (validating the graph first).
pub fn compile(comp: &Comp, ctx: CompCtx, sources: &dyn CompSources) -> Result<Program, CompError> {
    compile_inner(comp, comp, ctx, sources, &Prerendered::default(), &mut CloneBudget::new(sources))
}

/// What a compile may still spend on clone-from-another-frame sources (C3.10).
struct CloneBudget {
    depth: u8,
    compiles: u32,
}

impl CloneBudget {
    /// [`MAX_CLONE_COMPILES`] for a comp on the timeline, a quarter of that per level it is nested
    /// (16, 4, 1, then none): each nested comp's renders are multiplied by its parents' clone
    /// sources, so this keeps the whole tree's work bounded.
    fn new(sources: &dyn CompSources) -> Self {
        let compiles = MAX_CLONE_COMPILES.checked_shr(sources.nest_depth().saturating_mul(2)).unwrap_or(0);
        CloneBudget { depth: 0, compiles }
    }
}

/// Static-subgraph pictures a compile takes instead of their nodes: `images` stand in for their
/// node, the `skip` nodes are only read by those (so they emit nothing).
#[derive(Default)]
struct Prerendered {
    images: BTreeMap<NodeId, Arc<Image>>,
    skip: BTreeSet<NodeId>,
}

/// `scope` is the whole comp `comp` is part of (itself, or the comp a subgraph was cut from):
/// expressions read their references there.
fn compile_inner(
    comp: &Comp,
    scope: &Comp,
    ctx: CompCtx,
    sources: &dyn CompSources,
    pre: &Prerendered,
    budget: &mut CloneBudget,
) -> Result<Program, CompError> {
    compile_program(comp, scope, ctx, sources, pre, budget, Group { inputs: &[], depth: 0, frame: None })
}

/// [`compile_inner`] of a comp inside `group` (C4.3).
fn compile_program(
    comp: &Comp,
    scope: &Comp,
    ctx: CompCtx,
    sources: &dyn CompSources,
    pre: &Prerendered,
    budget: &mut CloneBudget,
    group: Group,
) -> Result<Program, CompError> {
    let (w, h) = (ctx.size.0.max(1), ctx.size.1.max(1));
    let mut slots = Slots { free: Vec::new(), next: 0, peak: 0 };
    let mut passes = Vec::new();
    let out = compile_into(comp, scope, ctx, sources, pre, budget, group, &mut slots, &mut passes)?;
    Ok(Program { size: (w, h), textures: slots.peak, passes, out })
}

/// Where a comp being compiled sits (C4.3): the slots holding its Group node's inputs (empty
/// outside a group), how many groups deep it is, and the parent comp's expressions at this time
/// (an inner expression's `node()` names the group's own nodes first, then the parent's).
#[derive(Clone, Copy)]
struct Group<'a> {
    inputs: &'a [Option<Tex>],
    depth: usize,
    frame: Option<&'a filmcraft_project::comp::ExprFrame<'a>>,
}

/// Appends the passes of `comp` to `passes` in slots of `slots` and returns the slot holding its
/// result (every other slot it took given back). A group's inside is compiled into its parent's
/// program this way, so the GPU runs groups with the same passes.
#[allow(clippy::too_many_arguments)]
fn compile_into(
    comp: &Comp,
    scope: &Comp,
    ctx: CompCtx,
    sources: &dyn CompSources,
    pre: &Prerendered,
    budget: &mut CloneBudget,
    group: Group,
    slots: &mut Slots,
    passes: &mut Vec<Pass>,
) -> Result<Tex, CompError> {
    let order = comp.validate()?;
    let (w, h) = (ctx.size.0.max(1), ctx.size.1.max(1));
    let px_scale = (w as f64 / f64::from(comp.width.max(1))) as f32;
    // C4.2: nodes with expressions are read as copies with those parameters evaluated at `t`
    // against the whole comp, all through one frame (each parameter evaluated once; parses come
    // from expr::parse_cached) (ADR 0009 §1.6)
    // (C4.3: inside a group, through the parent's frame for names the group doesn't have)
    let frame = scope.expr_frame_in(ctx.t, group.frame);
    // (LO.4: a Color node's grade and a Text node's layer settings too: `param_store`)
    let evaluated: BTreeMap<NodeId, CompNode> = if scope.nodes.iter().any(|n| n.param_store().values().any(|p| p.expression.is_some())) {
        scope.nodes.iter().filter_map(|n| Some((n.id, frame.node(n.id)?))).collect()
    } else {
        BTreeMap::new()
    };
    let nodes: BTreeMap<NodeId, &CompNode> = comp.nodes.iter().map(|n| (n.id, evaluated.get(&n.id).unwrap_or(n))).collect();
    let last_use = order.last_use();
    let mut result: BTreeMap<NodeId, Tex> = BTreeMap::new();
    let mut out = None;

    for (pos, step) in order.steps.iter().enumerate() {
        let Some(node) = nodes.get(&step.node) else { continue };
        let input = |i: usize| -> Option<Tex> { step.inputs.get(i).copied().flatten().and_then(|p: Port| result.get(&p.node).copied()) };
        // A slot that this step may overwrite: input `i`'s own slot when nothing later reads it,
        // otherwise a copy (a missing input gives a cleared slot).
        let writable = |i: usize, slots: &mut Slots, passes: &mut Vec<Pass>| -> Result<Tex, CompError> {
            let src_node = step.inputs.get(i).copied().flatten().map(|p| p.node);
            match (input(i), src_node) {
                (Some(src), Some(n)) if last_use.get(&n).copied() == Some(pos) && !reads_twice(&step.inputs, n) => Ok(src),
                (Some(src), _) => {
                    let dst = slots.take()?;
                    passes.push(Pass::Copy { src, dst });
                    Ok(dst)
                }
                (None, _) => {
                    let dst = slots.take()?;
                    passes.push(Pass::Fill { dst, color: [0.0; 4] });
                    Ok(dst)
                }
            }
        };
        let t = ctx.t;
        if pre.skip.contains(&node.id) {
            continue;
        }
        let tex = if let Some(image) = pre.images.get(&node.id) {
            let dst = slots.take()?;
            passes.push(Pass::Image { dst, image: Arc::clone(image) });
            dst
        } else if node.bypass {
            if node.kind.inputs() == 0 {
                let dst = slots.take()?;
                passes.push(Pass::Fill { dst, color: [0.0; 4] });
                dst
            } else {
                writable(0, slots, passes)?
            }
        } else {
            match &node.kind {
                // C4.3: a group's input (a copy: the parent still owns its slot)
                CompKind::MediaIn { source: MediaInSource::GroupInput { index }, .. } => {
                    let dst = slots.take()?;
                    match usize::try_from(*index).ok().and_then(|i| group.inputs.get(i).copied().flatten()) {
                        Some(src) => passes.push(Pass::Copy { src, dst }),
                        None => passes.push(Pass::Fill { dst, color: [0.0; 4] }),
                    }
                    dst
                }
                CompKind::MediaIn { source, offset } => {
                    let dst = slots.take()?;
                    let at = Tick(t.0.saturating_add(offset.0));
                    if let Some(layers) = sources.media_in_layers(source, at, w, h) {
                        passes.push(Pass::Layers { dst, layers: layers.into() });
                    } else {
                        match sources.media_in(source, at, w, h).filter(|i| (i.w, i.h) == (w, h)) {
                            Some(image) => passes.push(Pass::Image { dst, image: Arc::new(image) }),
                            None => passes.push(Pass::Fill { dst, color: [0.0; 4] }),
                        }
                    }
                    dst
                }
                CompKind::Text { layer } => {
                    let dst = slots.take()?;
                    match sources.text(layer, t, w, h, px_scale).filter(|i| (i.w, i.h) == (w, h)) {
                        Some(image) => passes.push(Pass::Image { dst, image: Arc::new(image) }),
                        None => passes.push(Pass::Fill { dst, color: [0.0; 4] }),
                    }
                    dst
                }
                CompKind::Solid => {
                    let dst = slots.take()?;
                    passes.push(Pass::Fill { dst, color: premul(color(node, "color", t)) });
                    dst
                }
                CompKind::Gradient => {
                    let dst = slots.take()?;
                    passes.push(Pass::Image { dst, image: Arc::new(gradient(node, t, w, h)) });
                    dst
                }
                CompKind::MediaOut => writable(0, slots, passes)?,
                CompKind::Merge => {
                    let dst = writable(0, slots, passes)?;
                    if let Some(src) = input(1) {
                        let opacity = (num(node, "opacity", t, 100.0) / 100.0).clamp(0.0, 1.0) as f32;
                        let mode = Blend::from_index(choice(node, "blend", t));
                        passes.push(Pass::Over { src, dst, mode, opacity });
                    }
                    dst
                }
                CompKind::Transform => {
                    let dst = slots.take()?;
                    match input(0) {
                        Some(src) => passes.push(Pass::Draw { src, dst, xform: transform(node, t, (w, h), px_scale), hq: ctx.high_quality }),
                        None => passes.push(Pass::Fill { dst, color: [0.0; 4] }),
                    }
                    dst
                }
                // C3.5b: roto coverage (rasterised by whichever path runs it, LO.5), paint strokes
                CompKind::Roto { roto, output } => {
                    // C3.7: shapes linked to a Tracker / Planar Tracker move with it
                    let size = (f64::from(comp.width.max(1)), f64::from(comp.height.max(1)));
                    let roto = track::linked_roto(roto, |id| nodes.get(&id).copied().map(|n| &n.kind), comp.rate, t, size);
                    let prepared = crate::roto::prepare(&roto, t, w, h, px_scale);
                    // (rasterised when a path needs the values: LO.5, the GPU measures the edges)
                    let fits = w.checked_mul(h).is_some_and(|n| n > 0 && n <= crate::roto::MAX_PIXELS);
                    match output {
                        RotoOutput::Matte => {
                            let dst = slots.take()?;
                            if fits {
                                passes.push(Pass::Coverage { tex: dst, coverage: Arc::new(Coverage::from_roto(prepared)), matte: true });
                            } else {
                                passes.push(Pass::Fill { dst, color: [0.0, 0.0, 0.0, 1.0] });
                            }
                            dst
                        }
                        RotoOutput::Alpha => {
                            let tex = writable(0, slots, passes)?;
                            if !prepared.shapes.is_empty() && fits {
                                passes.push(Pass::Coverage { tex, coverage: Arc::new(Coverage::from_roto(prepared)), matte: false });
                            }
                            tex
                        }
                    }
                }
                CompKind::Paint { paint } => {
                    let frame = comp.rate.frame_duration();
                    let strokes: Vec<_> = paint.active_at(t, frame).cloned().collect();
                    // C3.10: the input rendered at each clone time offset, into slots of its own
                    let mut clones = Vec::new();
                    if !strokes.is_empty()
                        && budget.depth < MAX_CLONE_DEPTH
                        && let Some(src) = step.inputs.first().copied().flatten().map(|p| p.node)
                    {
                        for dt in crate::paint::clone_times(paint, t, frame).0 {
                            if budget.compiles == 0 {
                                break;
                            }
                            budget.compiles -= 1;
                            let at = CompCtx { t: Tick(t.0.saturating_add(dt.0)), ..ctx };
                            if let Some(slot) = shifted_input(comp, scope, src, at, sources, pre, budget, group.depth, slots, passes) {
                                clones.push((dt, slot));
                            }
                        }
                    }
                    let tex = writable(0, slots, passes)?;
                    for (_, c) in &clones {
                        slots.give(*c);
                    }
                    if !strokes.is_empty() {
                        let paint = Arc::new(filmcraft_project::paint::Paint { strokes });
                        passes.push(Pass::Paint { tex, reveal: input(1), clones, paint, at: t, frame, px_scale });
                    }
                    tex
                }
                CompKind::Tracker { .. } | CompKind::PlanarTracker { .. } => {
                    let fg = input(1);
                    let size = (f64::from(comp.width.max(1)), f64::from(comp.height.max(1)));
                    let plan = track::plan(&node.kind, comp.rate, t, size, fg.is_some());
                    let warp = plan.map_or(track::TrackWarp::Affine(Affine::IDENTITY), |p| p.warp.scaled(f64::from(px_scale)));
                    let draw = |src: Tex, dst: Tex| match warp {
                        track::TrackWarp::Affine(xform) => Pass::Draw { src, dst, xform, hq: ctx.high_quality },
                        track::TrackWarp::Projective(h) => Pass::Warp { src, dst, h },
                    };
                    match (plan.is_some_and(|p| p.foreground), fg) {
                        // the foreground moved, then merged over input 0
                        (true, Some(src)) => {
                            let dst = writable(0, slots, passes)?;
                            let moved = slots.take()?;
                            passes.push(draw(src, moved));
                            passes.push(Pass::Over { src: moved, dst, mode: Blend::Normal, opacity: 1.0 });
                            slots.give(moved);
                            dst
                        }
                        _ => {
                            let dst = slots.take()?;
                            match input(0) {
                                Some(src) => passes.push(draw(src, dst)),
                                None => passes.push(Pass::Fill { dst, color: [0.0; 4] }),
                            }
                            dst
                        }
                    }
                }
                // C4.3: the inside compiled into this program, reading this node's inputs, its
                // exposed parameters overriding the inner ones; too deep or not valid: transparent
                CompKind::Group { .. } => {
                    let ins: Vec<Option<Tex>> = (0..usize::from(node.kind.inputs())).map(input).collect();
                    let inner = filmcraft_project::comp::group::inner_with_overrides(node);
                    let compiled = match &inner {
                        Some(inner) if group.depth < filmcraft_project::comp::MAX_GROUP_DEPTH => {
                            let at = Group { inputs: &ins, depth: group.depth.saturating_add(1), frame: Some(&frame) };
                            // (an inside that fails part way leaves no passes and holds no slots)
                            let (kept, held) = (passes.len(), slots.clone());
                            match compile_into(inner, inner, ctx, sources, &Prerendered::default(), budget, at, slots, passes) {
                                Err(CompError::TooManyTextures) => return Err(CompError::TooManyTextures),
                                Ok(tex) => Some(tex),
                                Err(_) => {
                                    passes.truncate(kept);
                                    *slots = held;
                                    None
                                }
                            }
                        }
                        _ => None,
                    };
                    match compiled {
                        Some(tex) => tex,
                        None => {
                            let dst = slots.take()?;
                            passes.push(Pass::Fill { dst, color: [0.0; 4] });
                            dst
                        }
                    }
                }
                CompKind::Key { .. } | CompKind::Mask { .. } | CompKind::Blur | CompKind::Color { .. } => {
                    let tex = writable(0, slots, passes)?;
                    let ops = node_ops(node, &ctx, comp.color.working, w, h, px_scale);
                    if !ops.is_empty() {
                        passes.push(Pass::Fx { tex, ops });
                    }
                    tex
                }
            }
        };
        result.insert(node.id, tex);
        out = Some(tex);
        // inputs whose last consumer was this step free their slots (unless this node took it)
        for p in step.inputs.iter().flatten() {
            if last_use.get(&p.node).copied() == Some(pos)
                && let Some(&s) = result.get(&p.node)
                && s != tex
            {
                slots.give(s);
            }
        }
    }
    out.ok_or(CompError::Empty)
}

/// `node`'s picture (with everything it reads) compiled at `ctx` (another time, C3.10) and its
/// passes appended to `passes` in free slots of `slots`: the slot holding it, the others given
/// back. None (nothing appended, no slot held) when it can't be: a graph error, or not enough
/// free slots.
#[allow(clippy::too_many_arguments)]
fn shifted_input(
    comp: &Comp,
    scope: &Comp,
    node: NodeId,
    ctx: CompCtx,
    sources: &dyn CompSources,
    pre: &Prerendered,
    budget: &mut CloneBudget,
    group_depth: usize,
    slots: &mut Slots,
    passes: &mut Vec<Pass>,
) -> Option<Tex> {
    // (no room for even one more slot beside the Paint step's own: don't render it at all)
    if slots.available() < 2 {
        return None;
    }
    // the node and everything upstream of it
    let mut sub = BTreeSet::from([node]);
    let mut stack = vec![node];
    while let Some(x) = stack.pop() {
        for e in comp.edges.iter().filter(|e| e.to.node == x) {
            if sub.insert(e.from.node) {
                stack.push(e.from.node);
            }
        }
    }
    // (C4.3: a group's inputs exist only at the group's own time: clone from the same time)
    if comp.nodes.iter().any(|n| sub.contains(&n.id) && matches!(n.kind, CompKind::MediaIn { source: MediaInSource::GroupInput { .. }, .. })) {
        return None;
    }
    let part = Comp {
        width: comp.width,
        height: comp.height,
        rate: comp.rate,
        duration: comp.duration,
        nodes: comp.nodes.iter().filter(|n| sub.contains(&n.id)).cloned().collect(),
        edges: comp.edges.iter().filter(|e| sub.contains(&e.from.node) && sub.contains(&e.to.node)).cloned().collect(),
        output: Some(node),
        color: comp.color,
    };
    budget.depth = budget.depth.saturating_add(1);
    // (another time: the parent's frame is at this one, so a group's subgraph reads only its own)
    let program = compile_program(&part, scope, ctx, sources, pre, budget, Group { inputs: &[], depth: group_depth, frame: None });
    budget.depth = budget.depth.saturating_sub(1);
    let program = program.ok()?;
    // (one slot stays free for the Paint step's own copy of its input)
    if slots.available() <= usize::from(program.textures) {
        return None;
    }
    let mut map: Vec<Tex> = Vec::with_capacity(usize::from(program.textures));
    for _ in 0..program.textures {
        match slots.take() {
            Ok(s) => map.push(s),
            Err(_) => {
                map.into_iter().for_each(|s| slots.give(s));
                return None;
            }
        }
    }
    let remapped: Option<Vec<Pass>> = program.passes.into_iter().map(|p| remap_pass(p, &map)).collect();
    let (Some(out), Some(remapped)) = (map.get(usize::from(program.out)).copied(), remapped) else {
        map.into_iter().for_each(|s| slots.give(s));
        return None;
    };
    passes.extend(remapped);
    map.into_iter().filter(|s| *s != out).for_each(|s| slots.give(s));
    Some(out)
}

/// `p` with its slots renamed through `map` (slot `i` becomes `map[i]`); None for a slot outside it.
fn remap_pass(p: Pass, map: &[Tex]) -> Option<Pass> {
    let m = |t: Tex| map.get(usize::from(t)).copied();
    Some(match p {
        Pass::Image { dst, image } => Pass::Image { dst: m(dst)?, image },
        Pass::Layers { dst, layers } => Pass::Layers { dst: m(dst)?, layers },
        Pass::Fill { dst, color } => Pass::Fill { dst: m(dst)?, color },
        Pass::Copy { src, dst } => Pass::Copy { src: m(src)?, dst: m(dst)? },
        Pass::Fx { tex, ops } => Pass::Fx { tex: m(tex)?, ops },
        Pass::Draw { src, dst, xform, hq } => Pass::Draw { src: m(src)?, dst: m(dst)?, xform, hq },
        Pass::Over { src, dst, mode, opacity } => Pass::Over { src: m(src)?, dst: m(dst)?, mode, opacity },
        Pass::Paint { tex, reveal, clones, paint, at, frame, px_scale } => {
            let reveal = match reveal {
                Some(r) => Some(m(r)?),
                None => None,
            };
            let clones = clones.into_iter().map(|(dt, c)| Some((dt, m(c)?))).collect::<Option<Vec<_>>>()?;
            Pass::Paint { tex: m(tex)?, reveal, clones, paint, at, frame, px_scale }
        }
        Pass::Warp { src, dst, h } => Pass::Warp { src: m(src)?, dst: m(dst)?, h },
        Pass::Coverage { tex, coverage, matte } => Pass::Coverage { tex: m(tex)?, coverage, matte },
    })
}

/// Whether `node` feeds more than one input port of the same step (a Merge of a picture with
/// itself): its slot must not be overwritten in place then.
fn reads_twice(inputs: &[Option<Port>], node: NodeId) -> bool {
    inputs.iter().flatten().filter(|p| p.node == node).count() > 1
}

/// Runs a program on the CPU.
pub fn execute_cpu(p: &Program) -> Image {
    let (w, h) = p.size;
    let mut tex: Vec<Option<Image>> = vec![None; usize::from(p.textures)];
    let blank = || Image::new(w, h);
    for pass in &p.passes {
        match pass {
            Pass::Image { dst, image } => put(&mut tex, *dst, (**image).clone()),
            Pass::Layers { dst, layers } => {
                let mut img = blank();
                crate::plan::execute_layers(&mut img, layers);
                put(&mut tex, *dst, img);
            }
            Pass::Fill { dst, color } => put(&mut tex, *dst, if *color == [0.0; 4] { blank() } else { Image::filled(w, h, *color) }),
            Pass::Copy { src, dst } => {
                let img = get(&tex, *src).cloned().unwrap_or_else(blank);
                put(&mut tex, *dst, img);
            }
            Pass::Fx { tex: t, ops } => {
                if let Some(img) = tex.get_mut(usize::from(*t)).and_then(Option::as_mut) {
                    gpufx::apply_ops(ops, img);
                }
            }
            Pass::Draw { src, dst, xform, hq } => {
                let img = match get(&tex, *src) {
                    Some(s) if *hq => s.transformed_hq(w, h, xform),
                    Some(s) => s.transformed(w, h, xform),
                    None => blank(),
                };
                put(&mut tex, *dst, img);
            }
            Pass::Paint { tex: t, reveal, clones, paint, at, frame, px_scale } => {
                let Some(img) = tex.get_mut(usize::from(*t)).and_then(Option::take) else { continue };
                let same = |r: &&Image| (r.w, r.h) == (img.w, img.h);
                let rv = reveal.and_then(|r| get(&tex, r)).filter(same).map(|r| r.px.as_slice());
                let cl: Vec<(Tick, &[f32])> = clones.iter().filter_map(|(dt, c)| get(&tex, *c).filter(same).map(|i| (*dt, i.px.as_slice()))).collect();
                let px = crate::paint::apply(paint, *at, *frame, *px_scale, &img.px, &cl, rv, img.w, img.h);
                let img = if px.len() == img.px.len() { Image { px, ..img } } else { img };
                put(&mut tex, *t, img);
            }
            Pass::Warp { src, dst, h: hm } => {
                let img = match get(&tex, *src) {
                    Some(s) => track::warp_projective(s, w, h, hm),
                    None => blank(),
                };
                put(&mut tex, *dst, img);
            }
            Pass::Coverage { tex: t, coverage, matte } => {
                let n = w.saturating_mul(h);
                let coverage: &[f32] = coverage.raster().map_or(&[], |c| &c[..]);
                if *matte {
                    let img = if coverage.len() == n {
                        Image { w, h, px: coverage.iter().flat_map(|c| [*c, *c, *c, 1.0]).collect() }
                    } else {
                        Image::filled(w, h, [0.0, 0.0, 0.0, 1.0])
                    };
                    put(&mut tex, *t, img);
                } else if let Some(img) = tex.get_mut(usize::from(*t)).and_then(Option::as_mut)
                    && coverage.len() == img.w.saturating_mul(img.h)
                {
                    for (p, c) in img.px.as_chunks_mut::<4>().0.iter_mut().zip(coverage.iter()) {
                        p.iter_mut().for_each(|v| *v *= c);
                    }
                }
            }
            Pass::Over { src, dst, mode, opacity } => {
                let Some(s) = get(&tex, *src).cloned() else { continue };
                if let Some(d) = tex.get_mut(usize::from(*dst)).and_then(Option::as_mut)
                    && (d.w, d.h) == (s.w, s.h)
                {
                    blend::composite(d, &s, *opacity, *mode);
                }
            }
        }
    }
    tex.get_mut(usize::from(p.out)).and_then(Option::take).unwrap_or_else(blank)
}

fn get(tex: &[Option<Image>], t: Tex) -> Option<&Image> {
    tex.get(usize::from(t)).and_then(Option::as_ref)
}

fn put(tex: &mut [Option<Image>], t: Tex, img: Image) {
    if let Some(slot) = tex.get_mut(usize::from(t)) {
        *slot = Some(img);
    }
}

/// Renders `comp` at `ctx` on the CPU; a comp that can't be evaluated is transparent.
pub fn render_cpu(comp: &Comp, ctx: CompCtx, sources: &dyn CompSources) -> Image {
    match compile_cached(comp, ctx, sources) {
        Ok(p) => execute_cpu(&p),
        Err(e) => {
            log::debug!("comp not rendered: {e}");
            Image::new(ctx.size.0.max(1), ctx.size.1.max(1))
        }
    }
}

/// Bytes the static-subgraph cache may hold (DESIGN-C1 §3.2).
pub const STATIC_CACHE_BYTES: usize = 256 * 1024 * 1024;

/// The static-subgraph cache: pictures of subgraphs with no Media In / Color node and no keyframe
/// (titles, masks and blurs over solids or gradients), by everything that decides their pixels.
/// Least recently used out first.
#[derive(Default)]
struct StaticCache {
    map: HashMap<String, (Arc<Image>, u64)>,
    bytes: usize,
    clock: u64,
}

fn image_bytes(img: &Image) -> usize {
    img.w.saturating_mul(img.h).saturating_mul(16)
}

impl StaticCache {
    fn get(&mut self, key: &str) -> Option<Arc<Image>> {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        self.map.get_mut(key).map(|(img, used)| {
            *used = clock;
            Arc::clone(img)
        })
    }
    fn put(&mut self, key: String, img: Arc<Image>, cap: usize) {
        let size = image_bytes(&img);
        if size > cap {
            return;
        }
        if let Some((old, _)) = self.map.remove(&key) {
            self.bytes = self.bytes.saturating_sub(image_bytes(&old));
        }
        while self.bytes.saturating_add(size) > cap {
            let Some(oldest) = self.map.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) else { break };
            if let Some((old, _)) = self.map.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(image_bytes(&old));
            }
        }
        self.clock = self.clock.wrapping_add(1);
        self.bytes = self.bytes.saturating_add(size);
        self.map.insert(key, (img, self.clock));
    }
}

fn static_cache() -> &'static Mutex<StaticCache> {
    static CACHE: OnceLock<Mutex<StaticCache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// Whether a node's own picture never changes with time or footage: not a Media In, not a Color
/// node (its LUTs live outside the comp), no keyframe anywhere in it (masks, text layer) and no
/// expression on time or another node.
fn static_kind(comp: &Comp, n: &CompNode) -> bool {
    static_kind_in(comp, n, 0)
}

/// [`static_kind`] `depth` groups down. A Group is static when every node inside is (its Group
/// Inputs read the group's own inputs, which [`static_tops`] checks like any node's; LO.4), at
/// most [`filmcraft_project::comp::MAX_GROUP_DEPTH`] deep.
fn static_kind_in(comp: &Comp, n: &CompNode, depth: usize) -> bool {
    // (Paint: strokes show by time without a keyframe; a roto shape following a tracker moves
    // with the tracker's keyframes, which are not in this node)
    if let CompKind::Roto { roto, .. } = &n.kind
        && roto.shapes.iter().any(|s| s.track_link.is_some())
    {
        return false;
    }
    let own = serde_json::to_string(n).is_ok_and(|j| !j.contains("\"keyframes\"")) && comp.node_expressions_static(n.id);
    if let CompKind::Group { comp: inner, .. } = &n.kind {
        let group_input = |m: &CompNode| matches!(m.kind, CompKind::MediaIn { source: filmcraft_project::comp::MediaInSource::GroupInput { .. }, .. });
        return own && depth < filmcraft_project::comp::MAX_GROUP_DEPTH && inner.nodes.iter().all(|m| group_input(m) || static_kind_in(inner, m, depth + 1));
    }
    // (C4.2: an expression on time or on another node changes the picture without changing this node)
    !matches!(n.kind, CompKind::MediaIn { .. } | CompKind::Color { .. } | CompKind::Paint { .. }) && own
}

/// The static subgraphs worth caching, by their top node: each top node is static (its whole
/// subgraph is) and is read by a node that isn't, or is the output; only subgraphs holding a
/// Text, Blur, Key or Mask node (layout or a spatial pass; a lone Solid costs less than a copy).
/// Returns each top node with its subgraph (the nodes it depends on, itself included).
fn static_tops(comp: &Comp, order: &filmcraft_project::dag::Order) -> Vec<(NodeId, BTreeSet<NodeId>)> {
    let nodes: BTreeMap<NodeId, &CompNode> = comp.nodes.iter().map(|n| (n.id, n)).collect();
    let mut fixed: BTreeMap<NodeId, bool> = BTreeMap::new();
    for step in &order.steps {
        let own = nodes.get(&step.node).is_some_and(|n| static_kind(comp, n));
        let ins = step.inputs.iter().flatten().all(|p| fixed.get(&p.node).copied().unwrap_or(false));
        fixed.insert(step.node, own && ins);
    }
    let is_fixed = |n: NodeId| fixed.get(&n).copied().unwrap_or(false);
    let mut tops = Vec::new();
    for step in &order.steps {
        let n = step.node;
        if !is_fixed(n) {
            continue;
        }
        let read_by_dynamic = order.steps.iter().any(|s| !is_fixed(s.node) && s.inputs.iter().flatten().any(|p| p.node == n));
        if !(read_by_dynamic || comp.output == Some(n)) {
            continue;
        }
        // the subgraph: n and everything it reads, through the order's input lists
        let mut sub = BTreeSet::from([n]);
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            for p in order.steps.iter().filter(|s| s.node == x).flat_map(|s| s.inputs.iter().flatten()) {
                if sub.insert(p.node) {
                    stack.push(p.node);
                }
            }
        }
        let worth = sub.iter().filter_map(|id| nodes.get(id)).any(|x| {
            !x.bypass
                && matches!(
                    x.kind,
                    CompKind::Text { .. } | CompKind::Blur | CompKind::Key { .. } | CompKind::Mask { .. } | CompKind::Roto { .. } | CompKind::Group { .. }
                )
        });
        if worth {
            tops.push((n, sub));
        }
    }
    tops
}

/// [`compile`] with the static-subgraph cache (DESIGN-C1 §3.2): each static subgraph worth caching
/// is rendered once per state and size into [`STATIC_CACHE_BYTES`] and comes back as one
/// picture; the rest of the graph compiles as usual. Same pixels as [`compile`].
pub fn compile_cached(comp: &Comp, ctx: CompCtx, sources: &dyn CompSources) -> Result<Program, CompError> {
    let order = comp.validate()?;
    let tops = static_tops(comp, &order);
    if tops.is_empty() {
        return compile(comp, ctx, sources);
    }
    let (w, h) = (ctx.size.0.max(1), ctx.size.1.max(1));
    let mut pre = Prerendered::default();
    for (top, sub) in &tops {
        let mut part = comp.clone();
        part.nodes.retain(|n| sub.contains(&n.id));
        part.edges.retain(|e| sub.contains(&e.from.node) && sub.contains(&e.to.node));
        part.output = Some(*top);
        let state = serde_json::to_string(&(&part.nodes, &part.edges, top, &part.color)).unwrap_or_default();
        let key = format!("{w}x{h}|{}|{state}", ctx.high_quality);
        let hit = static_cache().lock().unwrap_or_else(PoisonError::into_inner).get(&key);
        let image = match hit {
            Some(img) => img,
            None => {
                let img = Arc::new(execute_cpu(&compile_inner(&part, comp, ctx, sources, &Prerendered::default(), &mut CloneBudget::new(sources))?));
                static_cache().lock().unwrap_or_else(PoisonError::into_inner).put(key, Arc::clone(&img), STATIC_CACHE_BYTES);
                img
            }
        };
        pre.images.insert(*top, image);
        pre.skip.extend(sub.iter().filter(|n| *n != top));
    }
    // a node in a cached subgraph that something compiled still reads is compiled too
    loop {
        let needed: Vec<NodeId> = order
            .steps
            .iter()
            .filter(|s| !pre.skip.contains(&s.node) && !pre.images.contains_key(&s.node))
            .flat_map(|s| s.inputs.iter().flatten().map(|p| p.node))
            .filter(|n| pre.skip.contains(n))
            .collect();
        if needed.is_empty() {
            break;
        }
        for n in needed {
            pre.skip.remove(&n);
        }
    }
    for (top, _) in &tops {
        pre.skip.remove(top);
    }
    compile_inner(comp, comp, ctx, sources, &pre, &mut CloneBudget::new(sources))
}

fn num(n: &CompNode, key: &str, t: Tick, default: f64) -> f64 {
    n.value_at(key, t).and_then(|v| v.as_f64()).filter(|v| v.is_finite()).unwrap_or(default)
}

fn choice(n: &CompNode, key: &str, t: Tick) -> u32 {
    match n.value_at(key, t) {
        Some(ParamValue::Choice(c)) => c,
        _ => 0,
    }
}

fn point(n: &CompNode, key: &str, t: Tick) -> Option<Vec2> {
    n.value_at(key, t).and_then(|v| v.as_vec2()).filter(|p| p.x.is_finite() && p.y.is_finite())
}

/// A colour parameter (sRGB-encoded, straight alpha, as every colour control) in linear light.
fn color(n: &CompNode, key: &str, t: Tick) -> [f32; 4] {
    let c = n.value_at(key, t).and_then(|v| v.as_color()).unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let [r, g, b] = dec([c[0], c[1], c[2]]);
    let a = if c[3].is_finite() { c[3].clamp(0.0, 1.0) } else { 1.0 };
    [r, g, b, a]
}

fn premul(c: [f32; 4]) -> [f32; 4] {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}

/// The Transform node's matrix: Motion's parameters in comp pixels (position / anchor "auto" =
/// the frame centre), scaled to the program size.
fn transform(n: &CompNode, t: Tick, (w, h): (usize, usize), px_scale: f32) -> Affine {
    let s = f64::from(px_scale);
    let centre = Vec2::new(w as f64 / 2.0, h as f64 / 2.0);
    let pos = point(n, "position", t).map_or(centre, |p| Vec2::new(p.x * s, p.y * s));
    let anchor = point(n, "anchor", t).map_or(centre, |p| Vec2::new(p.x * s, p.y * s));
    let scale = num(n, "scale", t, 100.0) / 100.0;
    let uniform = n.value_at("uniform_scale", t).and_then(|v| v.as_bool()).unwrap_or(true);
    let sx = if uniform { scale } else { num(n, "scale_width", t, 100.0) / 100.0 };
    Affine::motion(pos, Vec2::new(sx, scale), num(n, "rotation", t, 0.0), anchor)
}

/// The effect ops of a Key / Mask / Blur / Color node for a `w`×`h` image.
fn node_ops(n: &CompNode, ctx: &CompCtx, working: filmcraft_color::WorkingSpace, w: usize, h: usize, px_scale: f32) -> Vec<FxOp> {
    let cx = FxCtx { t: ctx.t, px_scale, seconds: 0.0, timecode: "", clip_name: "", project: ctx.project, env: None, working };
    let with_params = |effect: &str| -> Option<EffectInstance> {
        let mut e = find_effect(effect)?.instance();
        for (k, v) in &n.params {
            e.params.insert(k.clone(), v.clone());
        }
        Some(e)
    };
    match &n.kind {
        CompKind::Key { effect } if filmcraft_project::comp::KEY_EFFECTS.contains(&effect.as_str()) => {
            with_params(effect).and_then(|e| FxOp::eval_all(&e, &cx, w, h)).unwrap_or_default()
        }
        CompKind::Blur => with_params("gaussian_blur").and_then(|e| FxOp::eval_all(&e, &cx, w, h)).unwrap_or_default(),
        CompKind::Mask { masks } if !masks.is_empty() => {
            vec![FxOp::OpacityMask { masks: crate::mask::prepare(masks, ctx.t, px_scale).into() }]
        }
        CompKind::Color { node } => {
            let grade = Grade { nodes: vec![node.clone()], ..Default::default() };
            let gctx = crate::grade::GradeCtx { px_scale, seq_time: ctx.t, clip: 0, project: ctx.project, mattes: None };
            crate::grade::compile_fx(&grade, ctx.t, working, gctx, w, h)
        }
        _ => Vec::new(),
    }
}

/// A Roto node's coverage as a `w × h` f32 buffer (clamped to 0..1; None: a frame too large to
/// rasterise, e.g. over [`crate::roto::MAX_PIXELS`]).
fn roto_coverage(prepared: &crate::roto::PreparedRoto, w: usize, h: usize) -> Option<Arc<[f32]>> {
    let mut cov = crate::roto::rasterize(prepared, w, h);
    if cov.len() != w.checked_mul(h)? || cov.is_empty() {
        return None;
    }
    cov.iter_mut().for_each(|c| *c = if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 });
    Some(cov.into())
}

/// The Gradient node's picture: linear (along start → end) or radial (around start, reaching end),
/// mixed in linear light, generated at most [`GRADIENT_MAX_SIDE`] on its long side and upscaled.
fn gradient(n: &CompNode, t: Tick, w: usize, h: usize) -> Image {
    let long = w.max(h).max(1);
    let k = if long > GRADIENT_MAX_SIDE { GRADIENT_MAX_SIDE as f64 / long as f64 } else { 1.0 };
    let (gw, gh) = (((w as f64 * k).round() as usize).max(1), ((h as f64 * k).round() as usize).max(1));
    let start = point(n, "start", t).unwrap_or(Vec2::new(0.0, 0.5));
    let end = point(n, "end", t).unwrap_or(Vec2::new(1.0, 0.5));
    let (a, b) = (premul(color(n, "color_a", t)), premul(color(n, "color_b", t)));
    let radial = choice(n, "shape", t) == 1;
    let (sx, sy) = (start.x * gw as f64, start.y * gh as f64);
    let (dx, dy) = (end.x * gw as f64 - sx, end.y * gh as f64 - sy);
    let len2 = dx * dx + dy * dy;
    let mut img = Image::new(gw, gh);
    for (i, px) in img.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i % gw) as f64 + 0.5 - sx, (i / gw) as f64 + 0.5 - sy);
        let f = if len2 <= f64::EPSILON {
            1.0
        } else if radial {
            ((x * x + y * y) / len2).sqrt()
        } else {
            (x * dx + y * dy) / len2
        };
        let f = f.clamp(0.0, 1.0) as f32;
        for (c, (&ca, &cb)) in px.iter_mut().zip(a.iter().zip(b.iter())) {
            *c = ca + (cb - ca) * f;
        }
    }
    if (gw, gh) == (w, h) { img } else { img.transformed(w, h, &Affine::scale(w as f64 / gw as f64, h as f64 / gh as f64)) }
}

pub(crate) mod project;
pub mod track;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod group_tests;
