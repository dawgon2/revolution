//! Frame plans for the GPU compositor.
//!
//! [`plan_frame`] resolves what is visible at a time into a list of layers the GPU can draw
//! directly: a decoded source frame (YUV planes or RGBA), the matrix from source pixels to output
//! pixels, an opacity and a blend mode (all 27 of [`Blend`] are composited by the GPU). A media
//! clip whose enabled standard effects all have a GPU implementation ([`crate::gpufx`]) carries
//! them as a [`LayerFx`]: their parameters evaluated at the frame's time, run by the GPU on the
//! clip's working image before Motion places it, in the CPU's order (effects → Motion → Opacity /
//! blend). Anything the shaders don't cover yet — other standard effects, masks, adjustment
//! layers, nested sequences, transitions other than dissolves, dips and Push — is rendered on the CPU
//! for that layer (or the whole frame) and handed over as a pre-composited image, so the GPU path
//! is always exact with respect to the CPU reference.
//!
//! A Push transition is planned as ordered [`PlanStep`] operations on isolated clip inputs: each
//! side is drawn into its own image, the two are pushed together, and the result is composited (Normal) over
//! the canvas of the lower tracks, which is preserved for the blends above it. [`execute_cpu`]
//! runs the same steps on the CPU and is the reference.
//!
//! A colour-managed sequence (HDR, wide gamut, DaVinci Wide Gamut; fork ADR 0003) composites in
//! its working space and converts for the monitor at the end: an [`OutputStage`] on a
//! [`FramePlan::Layers`], or a [`PlanStep::Output`] step among composite steps.

use std::sync::Arc;

use filmcraft_frame::VideoFrame;
use filmcraft_geom::Affine;
use filmcraft_project::{ItemId, ItemKind, Project, Sequence, TrackItem};
use filmcraft_time::Tick;

use crate::gpufx::FxOp;
use crate::{Blend, RenderOptions, SourceProvider, motion_matrix, output_size};

/// One layer for the GPU, bottom to top.
#[derive(Clone)]
pub struct PlanLayer {
    pub frame: Arc<VideoFrame>,
    /// Maps frame pixels (0..w, 0..h) to output pixels — or, with [`fx`](Self::fx), working-image
    /// pixels (0..fx.size).
    pub matrix: Affine,
    pub opacity: f32,
    /// How the layer combines with what is under it ([`crate::blend::composite`]).
    pub blend: Blend,
    /// Standard effects to run on the frame before it is placed (None: draw the frame directly).
    pub fx: Option<Arc<LayerFx>>,
    /// A comp clip (fork C1 step 7): the picture is this program's result, not `frame` (which
    /// only carries its size, with no pixels). Comp layers have no `fx`.
    pub comp: Option<Arc<crate::comp::Program>>,
}

/// The GPU effect stage of a layer: the frame is decoded into a working image of `size` (the
/// frame box-decimated by `decimation`, as the CPU decodes it, and resampled to `size` when that
/// is smaller still), `ops` run on it in order, and the result is placed with the layer's matrix,
/// opacity and blend mode.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerFx {
    pub size: (u32, u32),
    pub decimation: u32,
    pub ops: Vec<FxOp>,
}

impl std::fmt::Debug for PlanLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanLayer")
            .field("frame", &(self.frame.width, self.frame.height))
            .field("matrix", &self.matrix)
            .field("opacity", &self.opacity)
            .field("blend", &self.blend)
            .field("fx", &self.fx.as_ref().map(|x| x.ops.len()))
            .field("comp", &self.comp.as_ref().map(|p| p.passes.len()))
            .finish()
    }
}

impl PlanLayer {
    /// A layer drawing `frame` directly.
    pub fn new(frame: Arc<VideoFrame>, matrix: Affine, opacity: f32, blend: Blend) -> Self {
        Self { frame, matrix, opacity, blend, fx: None, comp: None }
    }

    /// A layer drawing the result of a comp program (its pixels are `program.size`).
    pub fn comp(program: Arc<crate::comp::Program>, matrix: Affine, opacity: f32, blend: Blend) -> Self {
        let (w, h) = program.size;
        let w = u32::try_from(w).unwrap_or(u32::MAX).max(1);
        let h = u32::try_from(h).unwrap_or(u32::MAX).max(1);
        let frame =
            VideoFrame { width: w, height: h, data: filmcraft_frame::PixelData::RgbaF32(Arc::new(Vec::new())), ..VideoFrame::rgba_f32(1, 1, vec![0.0; 4]) };
        Self { frame: Arc::new(frame), matrix, opacity, blend, fx: None, comp: Some(program) }
    }

    /// The layer's picture on the CPU before placement: the comp's result, the frame with its
    /// effects, or the frame.
    pub fn cpu_image(&self) -> crate::Image {
        match (&self.comp, &self.fx) {
            (Some(p), _) => crate::comp::execute_cpu(p),
            (None, Some(fx)) => effect_image(&self.frame, fx),
            (None, None) => crate::Image { w: self.frame.width as usize, h: self.frame.height as usize, px: self.frame.to_linear_f32() },
        }
    }

    /// Size of the picture the matrix places: the working image with effects, else the frame.
    pub fn size(&self) -> (u32, u32) {
        match &self.fx {
            Some(fx) => fx.size,
            None => (self.frame.width, self.frame.height),
        }
    }
}

/// The output transform of a colour-managed sequence (fork ADR 0003): after the layers before
/// `at` are composited (in the working space), `ops` convert the picture for the monitor; the
/// layers from `at` on (captions) are drawn onto the converted picture.
#[derive(Clone, Debug)]
pub struct OutputStage {
    pub ops: Arc<[filmcraft_color::ops::ColorOp]>,
    pub at: usize,
}

#[derive(Clone)]
pub enum FramePlan {
    /// Draw these layers over black (and convert for the monitor, see [`OutputStage`]).
    Layers { width: usize, height: usize, layers: Vec<PlanLayer>, output: Option<OutputStage> },
    /// Ordered layer runs and two-input operations; each step composites onto the same canvas.
    Composite { width: usize, height: usize, steps: Vec<PlanStep> },
    /// The CPU produced the final image (fallback).
    Image(crate::Image),
}

#[derive(Clone)]
pub enum PlanStep {
    Layers(Vec<PlanLayer>),
    /// Inputs are isolated clip canvases; transition result composites Normal over lower tracks.
    Transition {
        inputs: [Vec<PlanLayer>; 2],
        effect: filmcraft_project::EffectInstance,
        progress: f32,
        scale: f32,
    },
    /// The output transform of a colour-managed sequence on the canvas so far (the
    /// [`OutputStage`] of a plan made of steps, fork ADR 0003); the steps after it (captions)
    /// draw onto the converted picture.
    Output(Arc<[filmcraft_color::ops::ColorOp]>),
}

/// The steps of a plan being built, with the output transform once it is marked.
#[derive(Default)]
struct PlanStack {
    steps: Vec<PlanStep>,
    /// The output stage of a plan that is one layer run ([`FramePlan::Layers`]); a plan of
    /// several steps carries it as a [`PlanStep::Output`] instead.
    output: Option<OutputStage>,
}

impl PlanStack {
    fn push(&mut self, layer: PlanLayer) {
        if let Some(PlanStep::Layers(layers)) = self.steps.last_mut() {
            layers.push(layer);
        } else {
            self.steps.push(PlanStep::Layers(vec![layer]));
        }
    }

    /// Mark the output transform here: everything pushed so far is converted, what follows
    /// (captions) is drawn onto the converted picture.
    fn mark_output(&mut self, ops: Arc<[filmcraft_color::ops::ColorOp]>) {
        match self.steps.as_slice() {
            [] => self.output = Some(OutputStage { ops, at: 0 }),
            [PlanStep::Layers(layers)] => self.output = Some(OutputStage { ops, at: layers.len() }),
            _ => self.steps.push(PlanStep::Output(ops)),
        }
    }

    fn into_layers(self, width: usize, height: usize) -> Vec<PlanLayer> {
        match self.finish(width, height) {
            FramePlan::Layers { layers, output: None, .. } => layers,
            plan => vec![PlanLayer::new(cpu_frame(execute_cpu(&plan)), Affine::IDENTITY, 1.0, Blend::Normal)],
        }
    }

    fn finish(mut self, width: usize, height: usize) -> FramePlan {
        if self.steps.len() <= 1 && self.steps.first().is_none_or(|s| matches!(s, PlanStep::Layers(_))) {
            let layers = match self.steps.pop() {
                Some(PlanStep::Layers(layers)) => layers,
                _ => Vec::new(),
            };
            FramePlan::Layers { width, height, layers, output: self.output }
        } else {
            // (the output stage was marked while the plan was one layer run and a transition
            // step followed, which `plan_frame` never does: keep it where it was, as a step)
            if let Some(o) = self.output.take() {
                let mut insert = 0;
                if let Some(PlanStep::Layers(layers)) = self.steps.first_mut() {
                    let rest = layers.split_off(o.at.min(layers.len()));
                    if !rest.is_empty() {
                        self.steps.insert(1, PlanStep::Layers(rest));
                    }
                    insert = 1;
                }
                self.steps.insert(insert, PlanStep::Output(o.ops));
            }
            FramePlan::Composite { width, height, steps: self.steps }
        }
    }
}

impl FramePlan {
    /// Source storage retained by the plan, before GPU preparation.
    pub fn source_bytes(&self) -> usize {
        let layers = |ls: &[PlanLayer]| ls.iter().map(|l| l.frame.byte_size() + l.comp.as_ref().map_or(0, |p| p.byte_size())).sum::<usize>();
        match self {
            Self::Image(img) => img.px.len() * size_of::<f32>(),
            Self::Layers { layers: ls, .. } => layers(ls),
            Self::Composite { steps, .. } => steps
                .iter()
                .map(|s| match s {
                    PlanStep::Layers(ls) => layers(ls),
                    PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| layers(ls)).sum(),
                    PlanStep::Output(_) => 0,
                })
                .sum(),
        }
    }

    /// Largest output/source texture side required by the plan.
    pub fn max_side(&self) -> usize {
        let layer_side = |ls: &[PlanLayer]| ls.iter().map(|l| l.frame.width.max(l.frame.height) as usize).max().unwrap_or(0);
        match self {
            Self::Image(img) => img.w.max(img.h),
            Self::Layers { width, height, layers, .. } => (*width).max(*height).max(layer_side(layers)),
            Self::Composite { width, height, steps } => steps
                .iter()
                .map(|s| match s {
                    PlanStep::Layers(ls) => layer_side(ls),
                    PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| layer_side(ls)).max().unwrap_or(0),
                    PlanStep::Output(_) => 0,
                })
                .fold((*width).max(*height), usize::max),
        }
    }
}

/// Whether `frame` is Y'CbCr (planar, biplanar or a GPU surface): its input transform can run on
/// the GPU after the source draw.
fn frame_is_yuv(frame: &VideoFrame) -> bool {
    matches!(
        frame.data,
        filmcraft_frame::PixelData::Yuv8 { .. }
            | filmcraft_frame::PixelData::Yuv16 { .. }
            | filmcraft_frame::PixelData::Yuv16Bi { .. }
            | filmcraft_frame::PixelData::Gpu(_)
    )
}

/// The same frame read as its decoder-normalised signal: Y'CbCr → R'G'B' with no transfer
/// function (the input transform's table decodes it). Shares the pixel data.
fn signal_frame(frame: &Arc<VideoFrame>) -> Arc<VideoFrame> {
    let mut f = VideoFrame::clone(frame);
    f.color.transfer = filmcraft_color::Transfer::Linear;
    Arc::new(f)
}

fn cpu_frame(img: crate::Image) -> Arc<VideoFrame> {
    Arc::new(VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px))
}

fn simple_transition(id: &str) -> bool {
    matches!(id, "cross_dissolve" | "dip_to_black" | "dip_to_white")
}

/// The standard effects of a media clip if the GPU can draw it (any blend mode, every enabled
/// standard effect with a GPU implementation, masked or not; empty without effects or
/// with effects off). None: the clip is rendered on the CPU.
fn gpu_chain<'a>(project: &Project, item: &'a TrackItem, opts: RenderOptions) -> Option<Vec<&'a filmcraft_project::EffectInstance>> {
    let is_media = project.item(item.item).is_some_and(|p| matches!(p.kind, ItemKind::Media(_) | ItemKind::Subclip { .. }));
    if !is_media {
        return None;
    }
    if !opts.effects {
        return Some(Vec::new());
    }
    let mut chain = Vec::new();
    for e in item.effects.iter().filter(|e| e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e)) {
        if !e.enabled {
            continue;
        }
        // (masked effects too: `FxOp::Masked`)
        if !crate::gpufx::gpu_capable(&e.effect) {
            return None;
        }
        chain.push(e);
    }
    Some(chain)
}

/// Plan the frame at timeline `t`.
pub fn plan_frame(project: &Project, seq_id: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> FramePlan {
    let Some(seq) = project.sequence(seq_id) else { return FramePlan::Image(crate::Image::new(1, 1)) };
    let (w, h) = output_size(seq, opts.scale);
    if whole_frame_on_cpu(project, seq, t, opts) {
        return FramePlan::Image(crate::render_sequence(project, seq_id, t, opts, sources));
    }
    let mut layers = PlanStack::default();
    push_tracks(project, seq, t, opts, sources, 0, &mut layers);
    // Colour-managed sequences (HDR, wide gamut, DaVinci Wide Gamut) composite in their working
    // space and convert for the monitor at the end (`OutputStage`, fork ADR 0003).
    let pipe = seq.settings.color;
    if !pipe.is_plain() {
        let ops = crate::colorman::display_transform(&pipe).ops();
        if !ops.is_empty() {
            layers.mark_output(ops.into());
        }
    }
    if opts.captions {
        for o in crate::caption_overlays(seq, t, w, h) {
            layers.push(PlanLayer::new(
                Arc::new(VideoFrame::rgba_f32(o.w as u32, o.h as u32, o.px)),
                Affine::translate(o.x as f64, o.y as f64),
                1.0,
                Blend::Normal,
            ));
        }
    }
    layers.finish(w, h)
}

/// Whether [`plan_frame`] hands the frame at `t` back as one CPU image ([`FramePlan::Image`])
/// with the default [`RenderOptions`] (a monitor or an SDR export: the working space is converted
/// for display). A GPU renderer gains nothing on such a frame, so a caller with only a few of
/// them (the export's pool) can render it on the CPU without taking one.
pub fn is_cpu_frame(project: &Project, seq_id: ItemId, t: Tick) -> bool {
    project.sequence(seq_id).is_none_or(|seq| whole_frame_on_cpu(project, seq, t, RenderOptions::default()))
}

/// The frames [`plan_frame`] does not plan: a colour-managed sequence asked for its working
/// space itself (`working_output`: HDR exports, scopes) or nested composites and converts on the
/// CPU (the monitor's picture stays on the GPU with an [`OutputStage`], fork ADR 0003), and so
/// does a frame with an adjustment layer or a complex transition anywhere at `t` (whole-frame
/// fallback).
fn whole_frame_on_cpu(project: &Project, seq: &Sequence, t: Tick, opts: RenderOptions) -> bool {
    (!seq.settings.color.is_plain() && (opts.working_output || opts.depth > 0)) || !layered_at(project, seq, t)
}

/// Whether the frame of `seq` at `t` can be planned as layers: no adjustment layer and no
/// transition the compositor cannot mix itself is showing.
fn layered_at(project: &Project, seq: &Sequence, t: Tick) -> bool {
    for tr in &seq.video_tracks {
        if !tr.enabled {
            continue;
        }
        if let Some(trn) = tr.transitions.iter().find(|x| x.range().contains(t))
            && !simple_transition(&trn.effect.effect)
            && trn.effect.effect != "push"
        {
            return false;
        }
        if let Some(it) = tr.item_at(t)
            && project.item(it.item).is_some_and(|p| matches!(p.kind, ItemKind::AdjustmentLayer { .. }))
        {
            return false;
        }
    }
    true
}

/// Push the layers of every video track of `seq` at `t`, bottom track first (`seq` must be
/// [`layered_at`] `t`). `nest` counts the nested sequences already followed to reach `seq`.
fn push_tracks(project: &Project, seq: &Sequence, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider, nest: u32, layers: &mut PlanStack) {
    let (w, h) = output_size(seq, opts.scale);
    for tr in &seq.video_tracks {
        if !tr.enabled {
            continue;
        }
        if let Some(trn) = tr.transitions.iter().find(|x| x.range().contains(t)) {
            // These dissolves are symmetric: Reverse (play B→A backwards) renders the same frames.
            let p = trn.progress(t) as f32;
            let a = trn.from.and_then(|id| tr.item(id)).filter(|i| i.enabled);
            let b = trn.to.and_then(|id| tr.item(id)).filter(|i| i.enabled);
            match trn.effect.effect.as_str() {
                "push" => {
                    let mut inputs = [Vec::new(), Vec::new()];
                    for (slot, item) in inputs.iter_mut().zip([a, b]) {
                        if let Some(item) = item {
                            let mut input = PlanStack::default();
                            push_item(project, seq, item, t, opts, sources, 1.0, Some(Blend::Normal), nest, &mut input);
                            *slot = input.into_layers(w, h);
                        }
                    }
                    let progress = if trn.reverse {
                        inputs.swap(0, 1);
                        1.0 - p
                    } else {
                        p
                    };
                    layers.steps.push(PlanStep::Transition { inputs, effect: trn.effect.clone(), progress, scale: opts.scale });
                }
                "dip_to_black" | "dip_to_white" => {
                    let col = if trn.effect.effect == "dip_to_black" { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] };
                    layers.push(PlanLayer::new(Arc::new(VideoFrame::rgba_f32(1, 1, col.to_vec())), Affine::scale(w as f64, h as f64), 1.0, Blend::Normal));
                    let (it, k) = if p < 0.5 { (a, 1.0 - p * 2.0) } else { (b, (p - 0.5) * 2.0) };
                    if let Some(it) = it {
                        push_item(project, seq, it, t, opts, sources, k, Some(Blend::Normal), nest, layers);
                    }
                }
                _ => {
                    // cross dissolve: A at full, B over it at p (premultiplied over == linear mix when A is opaque)
                    if let Some(it) = a {
                        push_item(project, seq, it, t, opts, sources, 1.0 - if b.is_none() { p } else { 0.0 }, Some(Blend::Normal), nest, layers);
                    }
                    if let Some(it) = b {
                        push_item(project, seq, it, t, opts, sources, p, Some(Blend::Normal), nest, layers);
                    }
                }
            }
            continue;
        }
        let Some(item) = tr.item_at(t) else { continue };
        if !item.enabled {
            continue;
        }
        push_item(project, seq, item, t, opts, sources, 1.0, None, nest, layers);
    }
}

/// Whether a nested sequence can be drawn by pushing its own layers into the plan of the sequence
/// it is in, instead of being rendered to an image on the CPU first. Compositing its layers one
/// by one over what is below gives the same picture as compositing them together first only when
/// they all blend Normal, and the frame must be one that plans as layers in the same colour
/// pipeline.
fn nest_is_plain(project: &Project, seq: &Sequence, nested: &Sequence, ft: Tick) -> bool {
    nested.settings.color == seq.settings.color
        && (nested.settings.width, nested.settings.height) == (seq.settings.width, seq.settings.height)
        && layered_at(project, nested, ft)
        && !nested.video_tracks.iter().any(|tr| tr.enabled && tr.transitions.iter().any(|t| t.range().contains(ft) && !simple_transition(&t.effect.effect)))
        && nested.video_tracks.iter().filter(|tr| tr.enabled).all(|tr| {
            tr.transitions.iter().any(|x| x.range().contains(ft))
                || tr.item_at(ft).filter(|i| i.enabled).is_none_or(|i| crate::opacity_blend(i, i.effect_time_at(ft)).1 == Blend::Normal)
        })
}

/// Push the layer(s) of `item`. `blend` overrides the item's own blend mode: inside a transition
/// the CPU reference mixes the clips and composites the result Normal, ignoring their modes.
/// `nest` counts the multi-camera clips already followed to reach `item` (see
/// [`crate::MAX_NEST_DEPTH`]).
#[allow(clippy::too_many_arguments)]
fn push_item(
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
    extra_opacity: f32,
    blend: Option<Blend>,
    nest: u32,
    out: &mut PlanStack,
) {
    // frame time (`ft`) vs. effect time (`mt`): they differ inside a frame hold without Hold Filters
    let ft = item.source_time_at(t);
    let mt = item.effect_time_at(t);
    // (cheap: no operations built; the grade is compiled once, below, when it renders)
    let graded_item = crate::grade::changes_picture(project, item, mt, opts.temporal);
    let (op, own) = crate::opacity_blend(item, mt);
    let bl = blend.unwrap_or(own);
    // A multi-camera clip that only shows its angle (no effects, untransformed, same frame size)
    // draws the angle's clip directly: no CPU pass over the nested sequence.
    if let Some(ItemKind::Sequence(nested)) = project.item(item.item).map(|p| &p.kind)
        && nest < crate::MAX_NEST_DEPTH
        && let Some(angle) = item.multicam_angle(nested)
        && !(opts.effects && (item.has_standard_effects() || graded_item))
        && (nested.settings.width, nested.settings.height) == (seq.settings.width, seq.settings.height)
        && near_identity(&motion_matrix(seq, item, (nested.settings.width, nested.settings.height), Some(nested.settings.par), mt))
        && let Some(tr) = nested.angle_video_track_index(angle).and_then(|i| nested.video_tracks.get(i))
        && !tr.transitions.iter().any(|x| x.range().contains(ft))
        && tr.item_at(ft).is_none_or(|i| crate::opacity_blend(i, i.effect_time_at(ft)).1 != Blend::Dissolve)
    {
        // Inside the nested sequence the angle's clip is composited onto an empty canvas, where
        // every mode but Dissolve is Normal; the multicam clip's own mode then applies to it.
        if let Some(inner) = tr.item_at(ft).filter(|i| i.enabled) {
            push_item(project, nested, inner, ft, opts, sources, extra_opacity * op, Some(bl), nest + 1, out);
        }
        return;
    }
    // A nested sequence that is only shown (no effects, untransformed, fully opaque, blending
    // Normal) contributes its own layers: the compositor draws them like the clips of this
    // sequence, with no CPU pass over the nested sequence. Its captions are part of its picture.
    if let Some(ItemKind::Sequence(nested)) = project.item(item.item).map(|p| &p.kind)
        && nest < crate::MAX_NEST_DEPTH
        && item.multicam_angle(nested).is_none()
        && !(opts.effects && (item.has_standard_effects() || graded_item))
        && !item.has_opacity_masks()
        && bl == Blend::Normal
        && extra_opacity * op >= 1.0 - 1e-6
        && near_identity(&motion_matrix(seq, item, (nested.settings.width, nested.settings.height), Some(nested.settings.par), mt))
        && nest_is_plain(project, seq, nested, ft)
    {
        push_tracks(project, nested, ft, opts, sources, nest + 1, out);
        let (w, h) = output_size(nested, opts.scale);
        for o in crate::caption_overlays(nested, ft, w, h) {
            out.push(PlanLayer::new(
                Arc::new(VideoFrame::rgba_f32(o.w as u32, o.h as u32, o.px)),
                Affine::translate(o.x as f64, o.y as f64),
                1.0,
                Blend::Normal,
            ));
        }
        return;
    }
    // Graphic clips without standard effects: the layers are rasterised (cached) into one tight
    // image the GPU places as a layer.
    if !(opts.effects && (item.has_standard_effects() || graded_item))
        && !item.has_opacity_masks()
        && project.item(item.item).is_some_and(|p| matches!(p.kind, ItemKind::Graphic { .. }))
    {
        let Some(size) = crate::source_size(project, item.item) else { return };
        let (w, h) = output_size(seq, opts.scale);
        let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion_matrix(seq, item, size, None, mt));
        if let Some((img, x, y)) = crate::graphic_clip::render_graphic_tight(item, mt, size, &m, w, h) {
            out.push(PlanLayer::new(cpu_frame(img), Affine::translate(x as f64, y as f64), op * extra_opacity, bl));
        }
        return;
    }
    if let Some(chain) = gpu_chain(project, item, opts) {
        let Some(src) = sources.source(item.item) else { return };
        let Some(size) = crate::source_size(project, item.item) else { return };
        let motion = motion_matrix(seq, item, size, crate::source_par(project, item.item), mt);
        let lin = ((motion.a * motion.a + motion.b * motion.b).sqrt()).max((motion.c * motion.c + motion.d * motion.d).sqrt());
        let want = (lin * opts.scale as f64).clamp(1.0 / 64.0, 1.0) as f32;
        let clock = src.frame_clock();
        let time = crate::video_source_time(item, t, &clock);
        let Ok(frame) = crate::clip_frame(src.as_ref(), item, time, want) else { return };
        let cs = crate::colorman::source_space(project, item.item, &frame);
        // blended in-between frames (Frame Blending / Optical Flow on speed-changed clips) are
        // made on the CPU (below)
        let single = crate::interpolation_blend(item, t, &clock).is_none();
        let managed = crate::colorman::needs_management(&seq.settings.color, cs, &frame);
        // Log / HDR / wide-gamut Y'CbCr media: the GPU gets the decoded signal and converts it
        // with the input transform as its first colour operations (fork ADR 0003); RGB stills
        // keep the CPU conversion.
        let input = (single && managed && frame_is_yuv(&frame)).then(|| {
            let pipe = &seq.settings.color;
            // (the content peak rounded as `colorman::input_transform` does)
            let peak = crate::colorman::source_peak_nits(project, item.item).map(|p| f64::from(p.round() as u32));
            let ops = filmcraft_color::transform::input_ops(cs, frame.color.range, pipe, peak);
            FxOp::ColorChain(ops.into())
        });
        let plain = single && !managed;
        let opacity_masked = item.has_opacity_masks();
        // the colour grade: after the input transform, before the effect stack (fork ADR 0004)
        let grades = opts.effects && graded_item;
        if plain && (!chain.is_empty() || opacity_masked || grades) || input.is_some() {
            // GPU effect stage: the working image the CPU would decode (`base_layer`), the
            // effects evaluated for it, placed as `item_layer` places it
            // (a converted frame is decoded at full size: averaging its signal before the
            // input transform would not be averaging light)
            let n = if input.is_some() { 1 } else { crate::decimation(frame.width as f32, size.0 as f32 * want) };
            let (mut lw, mut lh) = ((frame.width as usize / n).max(1), (frame.height as usize / n).max(1));
            // ...but a converted frame drawn smaller is graded at the size drawn (fork Phase 5.3:
            // a 6.2K clip in a 4K export or a monitor), like Resolve's timeline-resolution
            // processing: the signal is resampled before the input transform, which differs from
            // resampling light only at edges between very different levels.
            // `FILMCRAFT_FULL_SIZE_GRADE` keeps the full size.
            let s = (size.0 as f32 * want / frame.width.max(1) as f32).min(1.0);
            if input.is_some() && s < 0.95 && std::env::var_os("FILMCRAFT_FULL_SIZE_GRADE").is_none() {
                lw = ((frame.width as f32 * s).round() as usize).max(1);
                lh = ((frame.height as f32 * s).round() as usize).max(1);
            }
            let px_scale = lw as f32 / size.0.max(1) as f32;
            let tc = "";
            let cx = crate::effects::FxCtx {
                t: mt,
                px_scale,
                seconds: (t - item.start).seconds(),
                timecode: tc,
                clip_name: &item.name,
                project: Some(project),
                env: None,
                working: seq.settings.color.working,
            };
            let mut ops: Option<Vec<FxOp>> = Some(input.clone().into_iter().collect());
            // (grain sizes scale with the working image: `px_scale`)
            let mut graded = if grades {
                // Magic Mask mattes (fork G3.2): tracked in the sequence frame, warped into the layer
                let to_seq = motion.then_apply(&Affine::scale(1.0 / px_scale as f64, 1.0 / px_scale as f64));
                let mattes = crate::matte::layer_mattes(project, seq, item, mt, opts.mattes, &to_seq, lw, lh);
                crate::grade::item_fx(project, item, mt, t, seq.settings.color.working, px_scale, lw, lh, mattes.as_ref())
            } else {
                Vec::new()
            };
            // temporal noise reduction (fork 6.7): the neighbouring source frames, drawn like this
            // one (its signal for an input transform), and their flows; or the op removed
            let to_signal = input.is_some();
            let prep = |f: Arc<VideoFrame>| if to_signal { signal_frame(&f) } else { f };
            crate::temporal::attach_frames(&mut graded, project, seq, item, t, opts, &src, &frame, want, n as u32, (lw, lh), &prep);
            if !graded.is_empty() {
                match ops.as_mut() {
                    Some(v) if graded.iter().all(FxOp::gpu_ok) => v.extend(graded),
                    Some(_) => {
                        // (e.g. node LUTs too large together for the GPU program: CPU every frame)
                        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                        if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            log::warn!("a clip's colour grade doesn't fit the GPU program; it renders on the CPU");
                        }
                        ops = None;
                    }
                    None => {}
                }
            }
            for e in &chain {
                let masks = crate::mask::prepare(&e.masks, mt, px_scale);
                let more =
                    FxOp::eval_all(e, &cx, lw, lh).map(|more| if masks.is_empty() { more } else { vec![FxOp::Masked { ops: more, masks: masks.into() }] });
                match (ops.as_mut(), more) {
                    (Some(v), Some(more)) if more.iter().all(FxOp::gpu_ok) => v.extend(more),
                    _ => ops = None,
                }
            }
            if opacity_masked && let Some(op) = item.effect("opacity").filter(|e| e.enabled && !e.masks.is_empty()) {
                let masks = crate::mask::prepare(&op.masks, mt, px_scale);
                let m = FxOp::OpacityMask { masks: masks.into() };
                match ops.as_mut() {
                    Some(v) if m.gpu_ok() => v.push(m),
                    _ => ops = None,
                }
            }
            // (an input transform followed by Lumetri: one pass)
            if let Some(ops) = ops.map(crate::gpufx::fuse_color_chains) {
                // a converted frame is handed over as its signal (the transfer applied by the
                // input transform, not by the source draw)
                let frame = if input.is_some() { signal_frame(&frame) } else { frame };
                let m = Affine::scale(opts.scale as f64, opts.scale as f64)
                    .then_apply(&motion)
                    .then_apply(&Affine::scale(1.0 / px_scale as f64, 1.0 / px_scale as f64));
                let fx = LayerFx { size: (lw as u32, lh as u32), decimation: n as u32, ops };
                out.push(PlanLayer { frame, matrix: m, opacity: op * extra_opacity, blend: bl, fx: Some(Arc::new(fx)), comp: None });
                return;
            }
        } else if plain {
            // Draft playback at reduced resolution: hand over planes box-filtered to the size
            // drawn instead of the full picture (a quarter at 1/2, a sixteenth at 1/4 of the
            // upload and sampling). The mean is taken over Y'CbCr codes, not linear light as
            // the shader's supersampling does, so this stays limited to the opt-in draft mode.
            let n = if filmcraft_media::cancel::draft() { crate::decimation(frame.width as f32, size.0 as f32 * want) } else { 1 };
            let frame = match frame.box_decimated(n) {
                Some(small) => Arc::new(small),
                None => frame,
            };
            let px_scale = frame.width as f64 / size.0.max(1) as f64;
            let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion).then_apply(&Affine::scale(1.0 / px_scale, 1.0 / px_scale));
            out.push(PlanLayer::new(frame, m, op * extra_opacity, bl));
            return;
        }
    }
    // a comp clip without effects of its own: its program, placed by Motion (C1 step 7)
    if let Some(l) = crate::comp_plan_layer(project, seq, item, t, opts, sources) {
        out.push(PlanLayer { opacity: l.opacity * extra_opacity, blend: bl, ..l });
        return;
    }
    // CPU-rendered layer (standard effects): drawn by the GPU as a pre-rendered canvas image.
    let tc = filmcraft_time::format_time(t, seq.settings.frame_rate, seq.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48_000);
    if let Some((img, op2, _)) = crate::item_layer(project, seq, item, t, opts, sources, &tc) {
        out.push(PlanLayer::new(cpu_frame(img), Affine::IDENTITY, op2 * extra_opacity, bl));
    }
}

/// Track `track` of `seq` alone at `t` as frame-plan layers (a comp's Media In, C1 step 7d);
/// None when it needs more than one layer run (a transition) and must render on the CPU.
pub(crate) fn track_layers(
    project: &Project,
    seq: &Sequence,
    track: usize,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
) -> Option<Vec<PlanLayer>> {
    let tr = seq.video_tracks.get(track)?;
    if !tr.enabled {
        return Some(Vec::new());
    }
    if tr.transitions.iter().any(|x| x.range().contains(t)) {
        return None;
    }
    let mut out = PlanStack::default();
    if let Some(item) = tr.item_at(t).filter(|i| i.enabled) {
        push_item(project, seq, item, t, opts, sources, 1.0, None, opts.depth, &mut out);
    }
    match (out.output, out.steps.as_slice()) {
        (None, []) => Some(Vec::new()),
        (None, [PlanStep::Layers(l)]) => Some(l.clone()),
        _ => None,
    }
}

/// A decoded frame of media `item` as one layer fitted into a `w`×`h` canvas (centred, as
/// `comp::project::fit` places the CPU picture): a comp's Media In of a media item or subclip.
/// Plain frames draw directly; log / HDR / wide-gamut Y'CbCr frames get the input transform as
/// the layer's colour ops (as a timeline clip does, fork ADR 0003). None: an RGB frame that needs
/// management (the CPU converts it).
pub(crate) fn media_frame_layer(
    project: &Project,
    item: ItemId,
    frame: Arc<VideoFrame>,
    pipe: &filmcraft_color::ColorPipeline,
    w: usize,
    h: usize,
) -> Option<PlanLayer> {
    let (fw, fh) = (f64::from(frame.width.max(1)), f64::from(frame.height.max(1)));
    let fit = |iw: f64, ih: f64| {
        let s = (w as f64 / iw).min(h as f64 / ih);
        (s.is_finite() && s > 0.0).then(|| Affine::translate((w as f64 - iw * s) / 2.0, (h as f64 - ih * s) / 2.0).then_apply(&Affine::scale(s, s)))
    };
    let cs = crate::colorman::source_space(project, item, &frame);
    if !crate::colorman::needs_management(pipe, cs, &frame) {
        let m = fit(fw, fh)?;
        return Some(PlanLayer::new(frame, m, 1.0, Blend::Normal));
    }
    if !frame_is_yuv(&frame) {
        return None;
    }
    let peak = crate::colorman::source_peak_nits(project, item).map(|p| f64::from(p.round() as u32));
    let ops = filmcraft_color::transform::input_ops(cs, frame.color.range, pipe, peak);
    // graded at the size drawn, as a timeline clip is (fork Phase 5.3)
    let s = ((w as f64 / fw).min(h as f64 / fh)).min(1.0);
    let (lw, lh) = if s.is_finite() && s > 0.0 && s < 0.95 { ((fw * s).round().max(1.0), (fh * s).round().max(1.0)) } else { (fw, fh) };
    let m = fit(lw, lh)?;
    let fx = LayerFx { size: (lw as u32, lh as u32), decimation: 1, ops: vec![FxOp::ColorChain(ops.into())] };
    Some(PlanLayer { frame: signal_frame(&frame), matrix: m, opacity: 1.0, blend: Blend::Normal, fx: Some(Arc::new(fx)), comp: None })
}

fn near_identity(m: &Affine) -> bool {
    (m.a - 1.0).abs() < 1e-9 && (m.d - 1.0).abs() < 1e-9 && m.b.abs() < 1e-9 && m.c.abs() < 1e-9 && m.e.abs() < 1e-6 && m.f.abs() < 1e-6
}

/// A layer's working image with its effects applied, on the CPU (the reference for the GPU
/// effect stage).
pub fn effect_image(frame: &VideoFrame, fx: &LayerFx) -> crate::Image {
    let mut img = crate::gpufx::working_image(frame, fx.decimation, (fx.size.0 as usize, fx.size.1 as usize));
    // (temporal ops replay the earlier ops on their source-frame neighbours, as the GPU does)
    crate::gpufx::apply_ops(&fx.ops, &mut img);
    img
}

/// Execute a plan on the CPU (reference for the GPU compositor).
pub fn execute_cpu(plan: &FramePlan) -> crate::Image {
    match plan {
        FramePlan::Image(img) => img.clone(),
        FramePlan::Composite { width, height, steps } => {
            let mut canvas = crate::Image::new(*width, *height);
            for step in steps {
                match step {
                    PlanStep::Layers(layers) => execute_layers(&mut canvas, layers),
                    PlanStep::Transition { inputs, effect, progress, scale } => {
                        let [a, b] = inputs.each_ref().map(|layers| {
                            let mut input = crate::Image::new(*width, *height);
                            execute_layers(&mut input, layers);
                            input
                        });
                        let mixed = crate::transitions::apply_scaled(effect, &a, &b, *progress, *scale);
                        crate::blend::composite(&mut canvas, &mixed, 1.0, Blend::Normal);
                    }
                    PlanStep::Output(ops) => canvas.map_rgb(|c, _, _| filmcraft_color::ops::apply_chain(ops, c)),
                }
            }
            canvas
        }
        FramePlan::Layers { width, height, layers, output } => {
            let mut canvas = crate::Image::new(*width, *height);
            match output {
                None => execute_layers(&mut canvas, layers),
                Some(o) => {
                    let at = o.at.min(layers.len());
                    execute_layers(&mut canvas, layers.get(..at).unwrap_or_default());
                    canvas.map_rgb(|c, _, _| filmcraft_color::ops::apply_chain(&o.ops, c));
                    execute_layers(&mut canvas, layers.get(at..).unwrap_or_default());
                }
            }
            canvas
        }
    }
}

pub(crate) fn execute_layers(canvas: &mut crate::Image, layers: &[PlanLayer]) {
    let (width, height) = (canvas.w, canvas.h);
    for l in layers {
        let src = l.cpu_image();
        let placed = if l.matrix == Affine::scale(width as f64, height as f64) && src.w == 1 && src.h == 1 {
            crate::Image::filled(width, height, src.get(0, 0))
        } else {
            src.transformed(width, height, &l.matrix)
        };
        crate::blend::composite(canvas, &placed, l.opacity, l.blend);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattening_an_operation_preserves_its_picture() {
        let frame = Arc::new(VideoFrame::rgba_f32(1, 1, vec![0.25, 0.0, 0.0, 0.5]));
        let steps = vec![PlanStep::Transition {
            inputs: [vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)], vec![]],
            effect: filmcraft_project::find_effect("push").unwrap().instance(),
            progress: 0.0,
            scale: 1.0,
        }];
        let expected = execute_cpu(&FramePlan::Composite { width: 1, height: 1, steps: steps.clone() });
        let layers = PlanStack { steps, output: None }.into_layers(1, 1);
        let got = execute_cpu(&FramePlan::Layers { width: 1, height: 1, layers, output: None });
        assert_eq!(got.px, expected.px);
        assert!(got.px.iter().any(|v| *v != 0.0));
    }

    /// An output transform marked on one layer run stays an [`OutputStage`]; marked after a
    /// transition step it becomes a [`PlanStep::Output`], and both convert the same pixels.
    #[test]
    fn the_output_stage_is_kept_as_a_stage_or_a_step() {
        let ops: Arc<[filmcraft_color::ops::ColorOp]> =
            vec![filmcraft_color::ops::ColorOp::Matrix { m: [[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]], offset: [0.0; 3] }].into();
        let layer = || PlanLayer::new(Arc::new(VideoFrame::rgba_f32(1, 1, vec![1.0, 0.5, 0.0, 1.0])), Affine::IDENTITY, 1.0, Blend::Normal);
        let mut one = PlanStack::default();
        one.push(layer());
        one.mark_output(ops.clone());
        one.push(layer());
        let FramePlan::Layers { output: Some(o), layers, .. } = one.finish(1, 1) else { panic!("one layer run") };
        assert_eq!((o.at, layers.len()), (1, 2));
        let mut steps = PlanStack::default();
        steps.push(layer());
        steps.steps.push(PlanStep::Transition {
            inputs: [vec![layer()], vec![]],
            effect: filmcraft_project::find_effect("push").unwrap().instance(),
            progress: 1.0,
            scale: 1.0,
        });
        steps.mark_output(ops);
        steps.push(layer());
        let FramePlan::Composite { steps, .. } = steps.finish(1, 1) else { panic!("steps") };
        assert!(matches!(steps.as_slice(), [PlanStep::Layers(_), PlanStep::Transition { .. }, PlanStep::Output(_), PlanStep::Layers(_)]));
        let got = execute_cpu(&FramePlan::Composite { width: 1, height: 1, steps });
        // the last layer (unconverted, opaque) covers the converted canvas
        assert!((got.px[0] - 1.0).abs() < 1e-6);
    }
}
