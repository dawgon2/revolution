//! Comp programs on the GPU (fork C1 step 7b, ADR 0007): the passes of a
//! [`filmcraft_render::comp::Program`] run on comp-sized slot textures with the renderer's own
//! layer draw and effect stage. A slot takes part in a draw as an ordinary linear RGBA layer: its
//! texture is registered in the upload cache under a placeholder frame (no pixels), so the layer
//! and effect paths sample it without an upload. The CPU [`execute_cpu`] is the oracle.
//!
//! [`execute_cpu`]: filmcraft_render::comp::execute_cpu

use std::sync::Arc;

use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_geom::Affine;
use filmcraft_render::Blend;
use filmcraft_render::comp::{Pass, Program};
use filmcraft_render::gpufx::FxOp;
use filmcraft_render::plan::{FramePlan, LayerFx, PlanLayer};
use filmcraft_render::track::Homography;

use filmcraft_render::paint::PlannedStroke;
use filmcraft_time::Tick;
use wgpu::util::DeviceExt;

use crate::comp_paint::{self, CompKernels};
use crate::comp_warp::{self, WarpKernels};
use crate::{GpuCompositor, Uploaded};

/// Most GPU memory the comps' slot textures may take together (bytes, every slot set held); a comp
/// that would go above it runs on the CPU. Provisional: 2 GB holds a 6.2K comp's three live
/// pictures and the scratch slot four times over.
pub const COMP_VRAM_BUDGET: u64 = 2 << 30;

/// Top-level draws without a comp after which the comps' textures are let go.
pub(crate) const COMP_IDLE_FRAMES: u32 = 120;

/// Most comp layers one frame runs on the GPU (the rest run on the CPU).
const MAX_COMP_SETS: usize = 8;

/// Comp pictures whose frames are remembered (a few static nodes per comp, a few comps).
const COMP_IMAGES_KEPT: usize = 8;

/// Bytes per slot texel (`Rgba16Float`).
const SLOT_TEXEL_BYTES: u64 = 8;

/// A comp-sized texture and the placeholder frame it is drawn through.
pub(crate) struct Slot {
    pub(crate) target: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    frame: Arc<VideoFrame>,
}

impl Slot {
    /// GPU memory its texture takes (0 before the first draw).
    fn bytes(&self) -> u64 {
        self.target.as_ref().map_or(0, |(_, _, (w, h))| u64::from(*w).saturating_mul(u64::from(*h)).saturating_mul(SLOT_TEXEL_BYTES))
    }

    fn new(w: u32, h: u32) -> Slot {
        let frame = VideoFrame { width: w, height: h, data: PixelData::RgbaF32(Arc::new(Vec::new())), ..VideoFrame::rgba_f32(1, 1, vec![0.0; 4]) };
        Slot { target: None, frame: Arc::new(frame) }
    }
}

impl GpuCompositor {
    /// Runs `p` on the GPU; its result is a placeholder frame registered in the upload cache
    /// (draw it as a layer right away). None when the comp doesn't fit [`COMP_VRAM_BUDGET`] or the
    /// device's texture size, or has no passes (the caller then runs it on the CPU).
    pub(crate) fn run_comp(&mut self, p: &Program, set: usize) -> Option<Arc<VideoFrame>> {
        // each comp layer of a frame has its own slots: a second comp must not overwrite the
        // first one's result before the frame is drawn
        if set >= MAX_COMP_SETS {
            return None;
        }
        while self.comp_sets.len() <= set {
            self.comp_sets.push(Vec::new());
        }
        // run outside a frame (tests, timings): the comp is its own frame, so its slot draws are
        // not counted as comp-free top-level draws (120 of them let every comp texture go,
        // the active slot set included, mid-program)
        let slots = self.comp_sets.get_mut(set)?;
        std::mem::swap(&mut self.comp_slots, slots);
        let top = !self.fx_frame_open;
        if top {
            self.fx_frame_open = true;
            if let Some(f) = self.fx.as_mut() {
                f.begin_frame();
            }
        }
        let out = self.run_comp_in_slots(p);
        if let Some(slots) = self.comp_sets.get_mut(set) {
            std::mem::swap(&mut self.comp_slots, slots);
        }
        if top {
            self.comp_frame_done(true);
            self.fx_frame_open = false;
            if let Some(f) = self.fx.as_mut() {
                f.end_frame();
            }
        }
        out
    }

    /// After a top-level draw: a comp-free stretch of [`COMP_IDLE_FRAMES`] draws lets the comps'
    /// textures go (`had_comps` restarts the count).
    pub(crate) fn comp_frame_done(&mut self, had_comps: bool) {
        if had_comps {
            self.comp_idle = 0;
            return;
        }
        self.comp_idle = self.comp_idle.saturating_add(1);
        if self.comp_idle == COMP_IDLE_FRAMES {
            self.comp_sets.clear();
            self.comp_slots.clear();
            self.comp_output = None;
            self.comp_backdrop = None;
            self.comp_coverage = None;
            self.comp_warp_levels.clear();
        }
    }

    /// Registers set `set`'s output slot `out` in the upload cache again (right before the frame
    /// draws the comp's result: later comps' uploads may have evicted it).
    pub(crate) fn refresh_comp_output(&mut self, set: usize, out: usize) {
        let Some(slots) = self.comp_sets.get_mut(set) else { return };
        std::mem::swap(&mut self.comp_slots, slots);
        self.register_slot(out);
        if let Some(slots) = self.comp_sets.get_mut(set) {
            std::mem::swap(&mut self.comp_slots, slots);
        }
    }

    fn run_comp_in_slots(&mut self, p: &Program) -> Option<Arc<VideoFrame>> {
        let w = u32::try_from(p.size.0).ok()?.max(1);
        let h = u32::try_from(p.size.1).ok()?.max(1);
        if w.max(h) > self.device.limits().max_texture_dimension_2d || p.passes.is_empty() {
            return None;
        }
        // effect ops the GPU stage can't run (or no stage on this device): the CPU runs the comp,
        // as it does a timeline layer's effects
        let fx_ok = |ops: &[FxOp]| self.fx.is_some() && ops.iter().all(FxOp::gpu_ok);
        if p.passes.iter().any(|pass| matches!(pass, Pass::Fx { ops, .. } if !fx_ok(ops))) {
            return None;
        }
        // projective draws (Tracker / Planar Tracker corner pins, C3.8): without the warp kernels
        // the CPU runs such a comp (affine tracker motion is an ordinary Draw)
        let warps = p.passes.iter().any(|pass| matches!(pass, Pass::Warp { .. }));
        if warps && self.warp_kernels().is_none() {
            return None;
        }
        // the Roto kernel has no CPU fallback inside the GPU program
        if p.passes.iter().any(|pass| matches!(pass, Pass::Coverage { .. })) && self.comp_kernels().is_none() {
            return None;
        }
        // a comp clip inside a Media In track would need these slots while they are in use
        if p.passes.iter().any(|pass| matches!(pass, Pass::Layers { layers, .. } if layers.iter().any(|l| l.comp.is_some()))) {
            return None;
        }
        // a picture of another size than the comp is stored as it is by the CPU oracle (the GPU
        // slots are all comp-sized): the CPU runs such a comp
        if p.passes.iter().any(|pass| matches!(pass, Pass::Image { image, .. } if (image.w, image.h) != p.size)) {
            return None;
        }
        // the program's slots and one scratch slot
        let n = usize::from(p.textures).saturating_add(1);
        let mut bytes = (n as u64).saturating_mul(u64::from(w)).saturating_mul(u64::from(h)).saturating_mul(SLOT_TEXEL_BYTES);
        // (and the f32 roto coverage texture)
        if p.passes.iter().any(|pass| matches!(pass, Pass::Coverage { .. })) {
            bytes = bytes.saturating_add(u64::from(w).saturating_mul(u64::from(h)).saturating_mul(4));
        }
        // (and the warps' halved source levels, at most)
        if warps {
            bytes = bytes.saturating_add(comp_warp::levels_bytes(w, h));
        }
        // the other comps' slot sets are held too (this one's are swapped out of `comp_sets`)
        let mut held = self.comp_sets.iter().flatten().map(Slot::bytes).fold(0u64, u64::saturating_add);
        // (a coverage texture kept from an earlier comp while this one needs none)
        if !p.passes.iter().any(|pass| matches!(pass, Pass::Coverage { .. }))
            && let Some((_, _, (cw, ch))) = &self.comp_coverage
        {
            held = held.saturating_add(u64::from(*cw).saturating_mul(u64::from(*ch)).saturating_mul(4));
        }
        // (halved levels kept from an earlier comp while this one has no warp)
        if !warps {
            held = held.saturating_add(self.comp_warp_levels_bytes());
        }
        if bytes.saturating_add(held) > self.comp_vram_budget {
            log::warn!("a comp of {w}x{h} with {n} pictures doesn't fit the GPU budget; it renders on the CPU");
            return None;
        }
        // reuse the slots when the size matches (a slot whose texture was never written is empty)
        if self.comp_slots.len() != n || self.comp_slots.first().is_some_and(|s| (s.frame.width, s.frame.height) != (w, h)) {
            self.comp_slots = (0..n).map(|_| Slot::new(w, h)).collect();
        }
        let mut written = vec![false; n];
        let scratch = n - 1;
        for pass in &p.passes {
            match pass {
                Pass::Image { dst, image } => {
                    // (same size as the comp: checked above)
                    let frame = self.image_frame(image);
                    self.slot_draw(usize::from(*dst), vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)], true, (w, h));
                    mark(&mut written, usize::from(*dst));
                }
                Pass::Layers { dst, layers } => {
                    self.slot_draw(usize::from(*dst), layers.to_vec(), true, (w, h));
                    mark(&mut written, usize::from(*dst));
                }
                Pass::Fill { dst, color } => {
                    let layers = if *color == [0.0; 4] {
                        Vec::new()
                    } else {
                        let px = Arc::new(VideoFrame::rgba_f32(1, 1, color.to_vec()));
                        vec![PlanLayer::new(px, Affine::scale(f64::from(w), f64::from(h)), 1.0, Blend::Normal)]
                    };
                    self.slot_draw(usize::from(*dst), layers, true, (w, h));
                    mark(&mut written, usize::from(*dst));
                }
                Pass::Copy { src, dst } => self.slot_layer_pass(
                    &mut written,
                    scratch,
                    usize::from(*src),
                    usize::from(*dst),
                    Affine::IDENTITY,
                    1.0,
                    Blend::Normal,
                    true,
                    false,
                    None,
                    (w, h),
                ),
                Pass::Draw { src, dst, xform, hq } => {
                    self.slot_layer_pass(&mut written, scratch, usize::from(*src), usize::from(*dst), *xform, 1.0, Blend::Normal, true, *hq, None, (w, h));
                }
                Pass::Over { src, dst, mode, opacity } => {
                    // (CPU: nothing happens unless both pictures exist)
                    let (s, d) = (usize::from(*src), usize::from(*dst));
                    if written.get(s).copied().unwrap_or(false) && written.get(d).copied().unwrap_or(false) {
                        self.slot_layer_pass(&mut written, scratch, s, d, Affine::IDENTITY, *opacity, *mode, false, false, None, (w, h));
                    }
                }
                // C3.6: the dab kernel; past its limits (or on a GPU error) the slot is read back,
                // painted on the CPU and uploaded again (a failed read back: the CPU runs the comp)
                Pass::Paint { tex, reveal, clones, paint, at, frame, px_scale } => {
                    let t = usize::from(*tex);
                    if written.get(t).copied().unwrap_or(false) {
                        let rv = reveal.map(usize::from).filter(|r| written.get(*r).copied().unwrap_or(false));
                        // C3.10: the clone sources that hold a picture (as the CPU oracle's)
                        let cl: Vec<(Tick, usize)> =
                            clones.iter().map(|(dt, c)| (*dt, usize::from(*c))).filter(|(_, c)| written.get(*c).copied().unwrap_or(false)).collect();
                        let times: Vec<Tick> = cl.iter().map(|c| c.0).collect();
                        let strokes = filmcraft_render::paint::plan(paint, *at, *frame, *px_scale, w as usize, h as usize, rv.is_some(), &times);
                        // (no stroke: the slot as it is; the CPU oracle's apply would also zero
                        // non-finite channels, which no GPU pass does for a picture left alone)
                        if strokes.is_empty() {
                            continue;
                        }
                        let clone_slots: Vec<usize> = cl.iter().map(|c| c.1).collect();
                        if self.gpu_paint(t, rv, &clone_slots, scratch, &strokes, (w, h)) {
                            self.comp_paint_gpu = self.comp_paint_gpu.saturating_add(1);
                            continue;
                        }
                        self.comp_paint_cpu = self.comp_paint_cpu.saturating_add(1);
                        let src = self.read_slot(t)?;
                        let rv = match rv {
                            Some(r) => Some(self.read_slot(r)?),
                            None => None,
                        };
                        let mut cpx = Vec::with_capacity(cl.len());
                        for (dt, c) in &cl {
                            cpx.push((*dt, self.read_slot(*c)?));
                        }
                        let cpx: Vec<(Tick, &[f32])> = cpx.iter().map(|(dt, v)| (*dt, v.as_slice())).collect();
                        let px = filmcraft_render::paint::apply(paint, *at, *frame, *px_scale, &src, &cpx, rv.as_deref(), w as usize, h as usize);
                        if px.len() == src.len() {
                            let frame = Arc::new(VideoFrame::rgba_f32(w, h, px));
                            self.slot_draw(t, vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)], true, (w, h));
                        }
                    }
                }
                // C3.8: the warp kernel (a GPU error: the CPU runs the comp)
                Pass::Warp { src, dst, h: hm } => {
                    let (s, d) = (usize::from(*src), usize::from(*dst));
                    if written.get(s).copied().unwrap_or(false) {
                        if !self.gpu_warp(s, d, scratch, hm, (w, h)) {
                            return None;
                        }
                    } else {
                        // (CPU: a missing source draws transparency)
                        self.slot_draw(d, Vec::new(), true, (w, h));
                    }
                    mark(&mut written, d);
                }
                // C3.6: the f32 coverage uploaded, one draw (a GPU error: the CPU runs the comp)
                Pass::Coverage { tex, coverage, matte } => {
                    let t = usize::from(*tex);
                    let n = (w as usize).saturating_mul(h as usize);
                    if coverage.len() != n {
                        if *matte {
                            let px = Arc::new(VideoFrame::rgba_f32(1, 1, vec![0.0, 0.0, 0.0, 1.0]));
                            self.slot_draw(t, vec![PlanLayer::new(px, Affine::scale(f64::from(w), f64::from(h)), 1.0, Blend::Normal)], true, (w, h));
                            mark(&mut written, t);
                        }
                    } else if *matte || written.get(t).copied().unwrap_or(false) {
                        if !self.gpu_coverage(t, scratch, coverage, *matte, (w, h)) {
                            return None;
                        }
                        mark(&mut written, t);
                    }
                }
                Pass::Fx { tex, ops } => {
                    let t = usize::from(*tex);
                    if written.get(t).copied().unwrap_or(false) {
                        let fx = LayerFx { size: (w, h), decimation: 1, ops: ops.clone() };
                        self.slot_layer_pass(&mut written, scratch, t, t, Affine::IDENTITY, 1.0, Blend::Normal, true, false, Some(fx), (w, h));
                    }
                }
            }
        }
        let out = usize::from(p.out);
        if !written.get(out).copied().unwrap_or(false) {
            // nothing reached the output: a transparent picture
            self.slot_draw(out, Vec::new(), true, (w, h));
        }
        self.register_slot(out)
    }

    /// A comp picture as a frame, the same frame for the same picture (static nodes come from the
    /// comp's cache as the same `Arc`), so the upload cache keeps its texture across frames.
    fn image_frame(&mut self, image: &Arc<filmcraft_render::image::Image>) -> Arc<VideoFrame> {
        if let Some((_, f)) = self.comp_images.iter().find(|(i, _)| Arc::ptr_eq(i, image)) {
            return f.clone();
        }
        let f = Arc::new(VideoFrame::rgba_f32(image.w as u32, image.h as u32, image.px.clone()));
        self.comp_images.push((image.clone(), f.clone()));
        if self.comp_images.len() > COMP_IMAGES_KEPT {
            self.comp_images.remove(0);
        }
        f
    }

    /// `src` drawn into `dst` as one layer (`clear`: onto transparency, else over what `dst`
    /// holds). When they are the same slot the result goes to the scratch slot and the two swap.
    #[allow(clippy::too_many_arguments)]
    fn slot_layer_pass(
        &mut self,
        written: &mut [bool],
        scratch: usize,
        src: usize,
        dst: usize,
        m: Affine,
        opacity: f32,
        blend: Blend,
        clear: bool,
        hq: bool,
        fx: Option<LayerFx>,
        size: (u32, u32),
    ) {
        let layers = match (written.get(src).copied().unwrap_or(false), self.register_slot(src)) {
            (true, Some(frame)) => vec![PlanLayer { frame, matrix: m, opacity, blend, fx: fx.map(Arc::new), comp: None }],
            _ => Vec::new(),
        };
        let target = if src == dst { scratch } else { dst };
        if target != dst && !clear {
            // over itself: start the scratch slot from the destination
            if let Some(frame) = self.register_slot(dst) {
                self.slot_draw(target, vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)], true, size);
            }
        }
        let was = self.high_quality;
        self.high_quality = was || hq;
        self.slot_draw(target, layers, clear, size);
        self.high_quality = was;
        if target != dst {
            // the scratch slot holds the result: swap the textures (each keeps its placeholder)
            let a = self.comp_slots.get_mut(target).map(|s| s.target.take());
            let b = self.comp_slots.get_mut(dst).map(|s| s.target.take());
            if let (Some(a), Some(b)) = (a, b) {
                if let Some(s) = self.comp_slots.get_mut(target) {
                    s.target = b;
                }
                if let Some(s) = self.comp_slots.get_mut(dst) {
                    s.target = a;
                }
            }
        }
        mark(written, dst);
    }

    /// Renders `layers` into slot `i` (created on first use) through the accumulator path.
    fn slot_draw(&mut self, i: usize, layers: Vec<PlanLayer>, clear: bool, (w, h): (u32, u32)) {
        let Some(slot) = self.comp_slots.get_mut(i) else { return };
        let mut target = slot.target.take();
        std::mem::swap(&mut self.accum, &mut target);
        // the comp's own output / backdrop: the frame's keep their size (no reallocation per frame)
        std::mem::swap(&mut self.output, &mut self.comp_output);
        std::mem::swap(&mut self.backdrop, &mut self.comp_backdrop);
        let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers, output: None };
        self.render_plan(&plan, None, clear, false);
        std::mem::swap(&mut self.backdrop, &mut self.comp_backdrop);
        std::mem::swap(&mut self.output, &mut self.comp_output);
        std::mem::swap(&mut self.accum, &mut target);
        if let Some(slot) = self.comp_slots.get_mut(i) {
            slot.target = target;
        }
    }

    /// The Paint / Roto kernels (built on first use; None when the device can't run them).
    fn comp_kernels(&self) -> Option<&CompKernels> {
        let fx = self.fx.is_some();
        self.comp_kernels.get_or_init(|| if fx { CompKernels::new(&self.device, crate::ACCUM_FORMAT) } else { None }).as_ref()
    }

    /// The warp kernels (built on first use; None when they can't be built).
    fn warp_kernels(&self) -> Option<&WarpKernels> {
        self.comp_warp_kernels.get_or_init(|| WarpKernels::new(&self.device, crate::ACCUM_FORMAT)).as_ref()
    }

    /// GPU memory the kept halved warp levels take.
    fn comp_warp_levels_bytes(&self) -> u64 {
        self.comp_warp_levels
            .iter()
            .flatten()
            .map(|(_, _, (w, h))| u64::from(*w).saturating_mul(u64::from(*h)).saturating_mul(comp_warp::LEVEL_TEXEL_BYTES))
            .fold(0, u64::saturating_add)
    }

    /// A projective warp (C3.8, see [`crate::comp_warp`]): slot `src` drawn through `hm` into
    /// `dst` on transparency (through `scratch` and a swap when they are the same slot). False
    /// on a GPU error or without kernels (the caller then runs the comp on the CPU).
    fn gpu_warp(&mut self, src: usize, dst: usize, scratch: usize, hm: &Homography, (w, h): (u32, u32)) -> bool {
        let Some(plan) = comp_warp::levels(w, h, hm) else {
            // (CPU: a singular or non-finite map draws nothing)
            self.slot_draw(dst, Vec::new(), true, (w, h));
            return true;
        };
        let Some(k) = self.warp_kernels() else { return false };
        let (warp, half, bgl) = (k.warp.clone(), k.half.clone(), k.bgl.clone());
        let Some(src_view) = self.comp_slots.get(src).and_then(|s| s.target.as_ref()).filter(|t| t.2 == (w, h)).map(|t| t.1.clone()) else {
            return false;
        };
        let into = if src == dst { scratch } else { dst };
        if into >= self.comp_slots.len() || into == src {
            return false;
        }
        let device = self.device.clone();
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let val = device.push_error_scope(wgpu::ErrorFilter::Validation);
        // the halved levels (kept between frames while their sizes repeat)
        if self.comp_warp_levels.len() < plan.sizes.len() {
            self.comp_warp_levels.resize_with(plan.sizes.len(), || None);
        }
        let mut views = vec![src_view];
        for (slot, &(lw, lh)) in self.comp_warp_levels.iter_mut().zip(&plan.sizes) {
            views.push(Self::target(&device, slot, lw, lh, comp_warp::LEVEL_FORMAT, wgpu::TextureUsages::empty()));
        }
        let bind = |view: &wgpu::TextureView, params: &[u8]| {
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("comp-warp-params"),
                contents: params,
                usage: wgpu::BufferUsages::UNIFORM,
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("comp-warp"),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                ],
            })
        };
        let mut enc = device.create_command_encoder(&Default::default());
        let unused = comp_warp::params(&Homography::IDENTITY, (0, 0));
        for pair in views.windows(2) {
            let [from, to] = pair else { continue };
            let bg = bind(from, &unused);
            let mut pass = crate::accum_pass(&mut enc, to, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(&half);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        let last = plan.sizes.last().copied().unwrap_or((w, h));
        let target = self.take_target(into, (w, h));
        let ok = match (target.as_ref(), views.last()) {
            (Some((_, view, _)), Some(from)) if views.len() == plan.sizes.len().saturating_add(1) => {
                let bg = bind(from, &comp_warp::params(&plan.inv, last));
                {
                    let mut pass = crate::accum_pass(&mut enc, view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                    pass.set_pipeline(&warp);
                    pass.set_bind_group(0, &bg, &[]);
                    pass.draw(0..3, 0..1);
                }
                self.queue.submit([enc.finish()]);
                true
            }
            _ => false,
        };
        let errors = [pollster::block_on(val.pop()), pollster::block_on(oom.pop())];
        if let Some(e) = errors.iter().flatten().next() {
            // (any of the textures may be the failed allocation: made again on their next use)
            log::warn!("the warp kernel failed, the comp renders on the CPU: {e}");
            drop(target);
            self.comp_warp_levels.clear();
            return false;
        }
        self.put_target(into, target);
        if !ok {
            return false;
        }
        if into != dst {
            // the scratch slot holds the result: swap the textures (each keeps its placeholder)
            let a = self.comp_slots.get_mut(into).and_then(|s| s.target.take());
            let b = self.comp_slots.get_mut(dst).and_then(|s| s.target.take());
            self.put_target(dst, a);
            self.put_target(into, b);
        }
        self.comp_warp_gpu = self.comp_warp_gpu.saturating_add(1);
        true
    }

    /// Slot `i`'s texture (created comp-sized when it has none yet), taken out of the slot.
    fn take_target(&mut self, i: usize, (w, h): (u32, u32)) -> Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))> {
        let slot = self.comp_slots.get_mut(i)?;
        let mut t = slot.target.take();
        Self::target(&self.device, &mut t, w, h, crate::ACCUM_FORMAT, wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST);
        t
    }

    fn put_target(&mut self, i: usize, t: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>) {
        if let Some(slot) = self.comp_slots.get_mut(i) {
            slot.target = t;
        }
    }

    /// The strokes painted into slot `t` by the dab kernel (C3.6, see [`crate::comp_paint`]): the
    /// whole frame drawn into `scratch` from `t`, then the two swap; `clones` the slots of the
    /// strokes' clone sources 1.. (C3.10, at most [`filmcraft_render::paint::MAX_CLONE_TIMES`]).
    /// False (slot
    /// unchanged) when the kernel can't run this pass: no kernels, too much work, a buffer over
    /// the device's limits or a GPU error.
    #[allow(clippy::too_many_arguments)]
    fn gpu_paint(&mut self, t: usize, reveal: Option<usize>, clones: &[usize], scratch: usize, strokes: &[PlannedStroke], (w, h): (u32, u32)) -> bool {
        let limits = self.device.limits();
        let max = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        let Some(data) = comp_paint::pack(strokes, w as usize, h as usize, max, self.comp_paint_max_entries) else { return false };
        let Some(k) = self.comp_kernels() else { return false };
        if scratch >= self.comp_slots.len() || scratch == t {
            return false;
        }
        let Some((_, src_view, size)) = self.comp_slots.get(t).and_then(|s| s.target.as_ref()).filter(|s| s.2 == (w, h)) else { return false };
        let reveal_view = match reveal.and_then(|r| self.comp_slots.get(r)?.target.as_ref()) {
            Some((_, v, s)) if *s == (w, h) => v.clone(),
            Some(_) => return false,
            None => self.dummy.clone(),
        };
        if clones.len() > comp_paint::CLONE_SOURCES {
            return false;
        }
        let mut clone_views = Vec::with_capacity(comp_paint::CLONE_SOURCES);
        for c in clones {
            match self.comp_slots.get(*c).and_then(|s| s.target.as_ref()) {
                Some((_, v, s)) if *s == (w, h) => clone_views.push(v.clone()),
                _ => return false,
            }
        }
        clone_views.resize(comp_paint::CLONE_SOURCES, self.dummy.clone());
        let (src_view, size) = (src_view.clone(), *size);
        let device = self.device.clone();
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let val = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let buf = |label: &str, bytes: &[u8], usage: wgpu::BufferUsages| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: if bytes.is_empty() { &[0; 16] } else { bytes },
                usage,
            })
        };
        let mut params = Vec::with_capacity(16);
        for v in [w, h, comp_paint::TILE as u32, data.tiles_x] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        let storage = wgpu::BufferUsages::STORAGE;
        let bufs = [
            buf("comp-paint-params", &params, wgpu::BufferUsages::UNIFORM),
            buf("comp-paint-dabs", &data.dabs, storage),
            buf("comp-paint-strokes", &data.strokes, storage),
            buf("comp-paint-tiles", bytemuck::cast_slice(&data.tiles), storage),
            buf("comp-paint-index", bytemuck::cast_slice(&data.index), storage),
        ];
        fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry { binding, resource }
        }
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("comp-paint"),
            layout: &k.paint_bgl,
            entries: &[
                entry(0, bufs[0].as_entire_binding()),
                entry(1, wgpu::BindingResource::TextureView(&src_view)),
                entry(2, wgpu::BindingResource::TextureView(&reveal_view)),
                entry(3, bufs[1].as_entire_binding()),
                entry(4, bufs[2].as_entire_binding()),
                entry(5, bufs[3].as_entire_binding()),
                entry(6, bufs[4].as_entire_binding()),
                entry(10, wgpu::BindingResource::TextureView(&clone_views[0])),
                entry(11, wgpu::BindingResource::TextureView(&clone_views[1])),
                entry(12, wgpu::BindingResource::TextureView(&clone_views[2])),
                entry(13, wgpu::BindingResource::TextureView(&clone_views[3])),
            ],
        });
        let pipeline = k.paint.clone();
        let target = self.take_target(scratch, (w, h));
        let ok = if let Some((_, dst_view, _)) = target.as_ref().filter(|t| t.2 == size) {
            let mut enc = device.create_command_encoder(&Default::default());
            {
                let mut pass = crate::accum_pass(&mut enc, dst_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..3, 0..1);
            }
            self.queue.submit([enc.finish()]);
            true
        } else {
            false
        };
        let errors = [pollster::block_on(val.pop()), pollster::block_on(oom.pop())];
        if let Some(e) = errors.iter().flatten().next() {
            // (the scratch texture may be the failed allocation: made again on its next use)
            log::warn!("the paint kernel failed, painting on the CPU: {e}");
            drop(target);
            return false;
        }
        if !ok {
            self.put_target(scratch, target);
            return false;
        }
        // the scratch slot holds the result: swap the textures (each keeps its placeholder)
        let old = self.comp_slots.get_mut(t).and_then(|s| s.target.take());
        self.put_target(t, target);
        self.put_target(scratch, old);
        true
    }

    /// A Roto coverage pass on slot `t` (C3.6): the coverage uploaded as `R32Float`, then the Matte
    /// `(c, c, c, 1)` drawn into `t`, or `t · c` into `scratch` and the two swapped. False on a GPU
    /// error or without kernels (`t` unchanged).
    fn gpu_coverage(&mut self, t: usize, scratch: usize, coverage: &[f32], matte: bool, (w, h): (u32, u32)) -> bool {
        if self.comp_kernels().is_none() {
            return false;
        }
        let device = self.device.clone();
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let val = device.push_error_scope(wgpu::ErrorFilter::Validation);
        if self.comp_coverage.as_ref().is_none_or(|c| c.2 != (w, h)) {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("comp-coverage"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            self.comp_coverage = Some((tex, view, (w, h)));
        }
        let Some((cov_tex, cov_view, _)) = self.comp_coverage.as_ref() else { return false };
        let (cov_tex, cov_view) = (cov_tex.clone(), cov_view.clone());
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &cov_tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(coverage),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w.saturating_mul(4)), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.uploaded_bytes = self.uploaded_bytes.saturating_add((coverage.len() as u64).saturating_mul(4));
        let src_view = if matte {
            self.dummy.clone()
        } else {
            match self.comp_slots.get(t).and_then(|s| s.target.as_ref()) {
                Some((_, v, s)) if *s == (w, h) => v.clone(),
                _ => return false,
            }
        };
        let Some(k) = self.comp_kernels() else { return false };
        let (pipeline, bgl) = (k.cover.clone(), k.cover_bgl.clone());
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("comp-cover-params"),
            contents: &[u32::from(matte).to_le_bytes(), [0; 4], [0; 4], [0; 4]].concat(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("comp-cover"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 7, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::TextureView(&src_view) },
                wgpu::BindGroupEntry { binding: 9, resource: wgpu::BindingResource::TextureView(&cov_view) },
            ],
        });
        let into = if matte { t } else { scratch };
        if !matte && (scratch == t || scratch >= self.comp_slots.len()) {
            return false;
        }
        let target = self.take_target(into, (w, h));
        let ok = if let Some((_, view, _)) = target.as_ref() {
            let mut enc = device.create_command_encoder(&Default::default());
            {
                let mut pass = crate::accum_pass(&mut enc, view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..3, 0..1);
            }
            self.queue.submit([enc.finish()]);
            true
        } else {
            false
        };
        let errors = [pollster::block_on(val.pop()), pollster::block_on(oom.pop())];
        if let Some(e) = errors.iter().flatten().next() {
            // (either texture may be the failed allocation: made again on their next use)
            log::warn!("the roto kernel failed, the comp renders on the CPU: {e}");
            drop(target);
            self.comp_coverage = None;
            return false;
        }
        self.put_target(into, target);
        if !ok {
            return false;
        }
        if !matte {
            let a = self.comp_slots.get_mut(scratch).and_then(|s| s.target.take());
            let b = self.comp_slots.get_mut(t).and_then(|s| s.target.take());
            self.put_target(t, a);
            self.put_target(scratch, b);
        }
        true
    }

    /// Slot `i`'s picture read back as premultiplied linear RGBA f32 (C3.5b Paint passes). None
    /// for a slot without a texture or a failed read-back.
    fn read_slot(&self, i: usize) -> Option<Vec<f32>> {
        let (tex, _, (w, h)) = self.comp_slots.get(i)?.target.as_ref()?;
        let size = (*w, *h);
        let mut enc = self.device.create_command_encoder(&Default::default());
        let bands = crate::record_readback(&self.device, &mut enc, tex, size, SLOT_TEXEL_BYTES as u32);
        self.queue.submit([enc.finish()]);
        let bytes = crate::map_readback(&self.device, &bands?, size, SLOT_TEXEL_BYTES as u32)?;
        Some(bytes.as_chunks::<2>().0.iter().map(|b| crate::f16_to_f32(u16::from_le_bytes(*b))).collect())
    }

    /// Registers slot `i`'s texture in the upload cache under its placeholder frame (again before
    /// every use: the cache evicts old entries). None for a slot without a texture.
    fn register_slot(&mut self, i: usize) -> Option<Arc<VideoFrame>> {
        let slot = self.comp_slots.get(i)?;
        let (_, view, (w, h)) = slot.target.as_ref()?;
        let frame = slot.frame.clone();
        let PixelData::RgbaF32(px) = &frame.data else { return None };
        let key = (Arc::as_ptr(px) as *const u8 as usize, *w, *h);
        self.clock += 1;
        let up = Uploaded {
            views: [view.clone(), self.dummy.clone(), self.dummy.clone(), self.dummy.clone()],
            kind: 1,
            code_scale: 1.0,
            texel_scale: 1.0,
            alpha_scale: 0.0,
            chroma: (*w, *h),
            last_used: self.clock,
            _pixels: frame.data.clone(),
        };
        self.uploads.insert(key, up);
        Some(frame)
    }
}

fn mark(written: &mut [bool], i: usize) {
    if let Some(w) = written.get_mut(i) {
        *w = true;
    }
}
