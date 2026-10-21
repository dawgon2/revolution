//! Branched colour grades on the GPU (fork C2 lane 3, `docs/fork/DESIGN-C2.md` §2–3): the passes
//! of a [`GradeProgram`] run inside a layer's effect stage, on the stage's own pooled working
//! textures (no allocation per frame: the job asks the pool for the graph's slots with the rest).
//! A program slot maps to whichever working texture holds it now: a pass writes a free texture and
//! the slot's old one goes back to the free list, so no pass reads and writes one texture.
//!
//! `Fx` runs the node's effect steps through the ordinary router ([`FxStage::route`]); `Matte`
//! keys with the existing key chain (`chain_key`) and box blur; `Over` is the layer blend of
//! `composite.wgsl`; the other passes are `graph.wgsl`. [`execute_graph_cpu`] is the oracle. What
//! the stage can't run ([`GradeProgram::gpu_ok`], a shader that doesn't build, the VRAM budget)
//! renders the clip on the CPU instead (`GpuCompositor::render_plan`'s CPU fallback).
//!
//! [`execute_graph_cpu`]: filmcraft_render::grade::execute_graph_cpu

use filmcraft_render::gpufx::FxOp;
use filmcraft_render::grade::{GradePass, GradeProgram, KeyMode};

use super::{CHAIN_KEY, Dispatch, FxStage, Pipe, RouteCx, Step, Work, boxes, fused_steps, mask_program, merge_chains, step, steps, textures_for};

const G_COPY: u32 = 1;
const G_FILL: u32 = 2;
const G_MATTE: u32 = 3;
const G_KEY_IN: u32 = 4;
const G_LIMIT: u32 = 5;
const G_MIX: u32 = 6;
const G_OVER: u32 = 7;
const G_CHANNEL: u32 = 8;
const G_COMBINE_INIT: u32 = 9;
const G_COMBINE_SET: u32 = 10;
const G_KEY_MIX: u32 = 11;

/// Matte sources (`graph.wgsl` `G_MATTE`, i1.x).
const MATTE_ONE: u32 = 0;
const MATTE_CLAMP: u32 = 1;
const MATTE_RAW: u32 = 2;

/// Scratch textures a pass may need beyond the program's slots (a matte's key and blur, a mixer's
/// running sum, an effect's ping-pong).
const MIN_SCRATCH: usize = 3;

/// The graph shader's pipeline and bind group layout.
pub(crate) struct GraphPipe {
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

/// The text of `src` from `begin` up to (not including) `end`.
fn section<'a>(src: &'a str, begin: &str, end: &str) -> Option<&'a str> {
    let a = src.find(begin)?;
    let rest = src.get(a..)?;
    let b = rest.find(end)?;
    rest.get(..b)
}

/// `graph.wgsl` with the layer blend helpers of `composite.wgsl` and the window coverage of
/// `colorops.wgsl` (None: a section marker moved; the graph then renders on the CPU).
pub(crate) fn module_source() -> Option<String> {
    let composite = include_str!("../composite.wgsl");
    let colorops = include_str!("../colorops.wgsl");
    let parts = [
        include_str!("graph.wgsl"),
        section(composite, "fn srgb_to_linear(", "fn pq_eotf(")?,
        section(composite, "// ---- Dissolve:", "// Normal and Dissolve")?,
        section(composite, "// ---- the other blend modes", "// Effect sources are axis-aligned")?,
        section(colorops, "// ---- mask coverage", "// A masked effect's mix")?,
    ];
    Some(parts.join("\n"))
}

/// Builds the graph pipeline for working textures of `format` (None: it failed; reported here,
/// not as an uncaptured device error).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn build(device: &wgpu::Device, format: wgpu::TextureFormat) -> Option<GraphPipe> {
    let Some(src) = module_source() else {
        log::warn!("grade graph shader sections not found; branched grades render on the CPU");
        return None;
    };
    let src = super::wgsl_for(&src, format);
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let tex = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("fx-graph"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
            tex(1),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format, view_dimension: wgpu::TextureViewDimension::D2 },
                count: None,
            },
            tex(3),
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
            tex(5),
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("fx-graph"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("filmcraft-fx-graph"), source: wgpu::ShaderSource::Wgsl(src.into()) });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("fx-graph"),
        layout: Some(&pl),
        module: &module,
        entry_point: Some("graph_px"),
        compilation_options: Default::default(),
        cache: None,
    });
    if let Some(e) = pollster::block_on(scope.pop()) {
        log::warn!("grade graph shader failed to build; branched grades render on the CPU: {e}");
        return None;
    }
    Some(GraphPipe { bgl, pipeline })
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn build(_device: &wgpu::Device, _format: wgpu::TextureFormat) -> Option<GraphPipe> {
    None
}

/// Whether `ops` hold a grade graph (at the top or under an effect mask).
pub(crate) fn has_graph(ops: &[FxOp]) -> bool {
    ops.iter().any(|o| match o {
        FxOp::GradeGraph(_) => true,
        FxOp::Masked { ops, .. } => has_graph(ops),
        _ => false,
    })
}

/// The passes of a node's effect steps (as the layer's own ops are lowered).
fn lower(ops: &[FxOp], fuse: bool) -> Vec<Step> {
    if fuse { merge_chains(ops.iter().flat_map(fused_steps).collect()) } else { ops.iter().flat_map(steps).collect() }
}

/// Working textures the graph steps among `all` need at once (0: none): the program's slots, the
/// layer's picture until the program takes it, scratch, and the picture a masked effect keeps
/// (a graph under an effect mask).
pub(super) fn textures_needed(all: &[Step], fuse: bool) -> usize {
    let kept = usize::from(all.iter().any(|s| s.save || s.combine));
    all.iter()
        .filter_map(|s| s.graph.as_deref())
        .map(|p| {
            let fx = p.passes.iter().filter_map(|q| if let GradePass::Fx { ops, .. } = q { Some(textures_for(&lower(ops, fuse)) - 1) } else { None });
            usize::from(p.textures).saturating_add(fx.fold(MIN_SCRATCH, usize::max)).saturating_add(1).saturating_add(kept)
        })
        .max()
        .unwrap_or(0)
}

/// Which working texture holds each program slot, and the free ones.
struct Slots {
    map: Vec<Option<usize>>,
    free: Vec<usize>,
}

impl Slots {
    fn get(&self, slot: u16) -> Option<usize> {
        self.map.get(usize::from(slot)).copied().flatten()
    }

    fn mapped(&self, k: usize) -> bool {
        self.map.contains(&Some(k))
    }

    fn take(&mut self) -> Option<usize> {
        self.free.pop()
    }

    /// `k` back to the free list (unless a slot holds it).
    fn release(&mut self, k: usize) {
        if !self.mapped(k) && !self.free.contains(&k) {
            self.free.push(k);
        }
    }

    /// Slot `slot` is now in texture `k` (an out-of-range slot: `k` is let go, as the CPU ignores
    /// it); its old texture is let go.
    fn set(&mut self, slot: u16, k: usize) {
        let Some(m) = self.map.get_mut(usize::from(slot)) else {
            self.release(k);
            return;
        };
        let old = m.replace(k);
        if let Some(o) = old
            && o != k
        {
            self.release(o);
        }
    }
}

/// A matte's last pass: its key from `from` (`mode`), inverted, times the windows of `program`.
fn matte_finish(mode: u32, from: &wgpu::TextureView, invert: bool, program: Option<Vec<f32>>) -> GPass<'_> {
    let windows = program.is_some();
    GPass { i1: [mode, u32::from(invert), u32::from(windows), 0], src: Some(from), program, ..GPass::new(G_MATTE) }
}

/// One graph-shader pass.
struct GPass<'v> {
    code: u32,
    i1: [u32; 4],
    p: [f32; 4],
    src: Option<&'v wgpu::TextureView>,
    aux: Option<&'v wgpu::TextureView>,
    aux2: Option<&'v wgpu::TextureView>,
    program: Option<Vec<f32>>,
}

impl<'v> GPass<'v> {
    fn new(code: u32) -> Self {
        GPass { code, i1: [0; 4], p: [0.0; 4], src: None, aux: None, aux2: None, program: None }
    }
}

impl FxStage {
    /// Builds the graph pipeline once (later calls: whether it built).
    pub(crate) fn ensure_graph(&mut self, device: &wgpu::Device) -> bool {
        if self.graph.is_none() {
            self.graph = Some(build(device, self.format));
        }
        matches!(self.graph, Some(Some(_)))
    }

    /// Whether the stage runs the grade graphs among `fx`'s ops (true when there are none): the
    /// pipeline built ([`ensure_graph`](Self::ensure_graph) first) and the working textures fit
    /// the budget. A layer for which this is false renders its effects on the CPU.
    pub(crate) fn graphs_fit(&self, device: &wgpu::Device, fx: &filmcraft_render::plan::LayerFx) -> bool {
        if !has_graph(&fx.ops) {
            return true;
        }
        if !matches!(self.graph, Some(Some(_))) {
            return false;
        }
        let max = device.limits().max_texture_dimension_2d;
        let fuse = self.fuse && self.box3.is_some();
        let (all, _) = super::plan_steps(fx, &[], max, 0, fuse);
        let n = textures_for(&all).max(textures_needed(&all, fuse));
        // (as `job` at temporal radius 0: the main chain's textures and the graph's beyond them)
        let (w, h) = (fx.size.0.max(1), fx.size.1.max(1));
        let bytes = u64::from(w).saturating_mul(u64::from(h)).saturating_mul(super::texel_bytes(self.format)).saturating_mul(n as u64);
        bytes <= self.budget
    }

    /// Routes the grade program `p` starting from the layer's picture in texture `start`, with
    /// the job's textures `cx.pool` except `keep` (a masked effect's saved image: never written);
    /// the texture holding the result (None: the job can't run; the layer then renders on the
    /// CPU).
    pub(super) fn route_graph(&self, cx: &RouteCx<'_>, p: &GradeProgram, start: usize, keep: &[usize], work: &mut Vec<Work>) -> Option<usize> {
        let gp = self.graph.as_ref()?.as_ref()?;
        let n = usize::from(p.textures.min(filmcraft_render::comp::MAX_LIVE_TEXTURES));
        let mut sl = Slots { map: vec![None; n], free: cx.pool.iter().copied().filter(|k| *k != start && !keep.contains(k)).rev().collect() };
        let mut source_taken = false;
        let view = |k: usize| cx.views.get(k);
        for pass in &p.passes {
            match pass {
                GradePass::Source { dst } => {
                    if source_taken {
                        return None;
                    }
                    source_taken = true;
                    if keep.contains(&start) {
                        // the kept image itself: the program works on a copy
                        let t = sl.take()?;
                        let g = GPass { src: Some(view(start)?), ..GPass::new(G_COPY) };
                        work.push(self.graph_dispatch(cx, gp, &g, view(t)?));
                        sl.set(*dst, t);
                    } else {
                        sl.set(*dst, start);
                    }
                }
                GradePass::Fill { dst, color } => {
                    let t = sl.take()?;
                    let mut g = GPass::new(G_FILL);
                    g.p = *color;
                    work.push(self.graph_dispatch(cx, gp, &g, view(t)?));
                    sl.set(*dst, t);
                }
                GradePass::Copy { src, dst } => {
                    let t = sl.take()?;
                    let g = match sl.get(*src) {
                        Some(s) => GPass { src: Some(view(s)?), ..GPass::new(G_COPY) },
                        None => GPass::new(G_FILL),
                    };
                    work.push(self.graph_dispatch(cx, gp, &g, view(t)?));
                    sl.set(*dst, t);
                }
                GradePass::Fx { tex, ops } => {
                    let Some(s) = sl.get(*tex) else { continue };
                    let st = lower(ops, cx.fuse);
                    if st.is_empty() {
                        continue;
                    }
                    let mut slots = vec![s];
                    for _ in 1..textures_for(&st) {
                        slots.push(sl.take()?);
                    }
                    let r = self.route(cx, &st, &slots, s, work)?;
                    sl.set(*tex, r);
                    for k in slots {
                        sl.release(k);
                    }
                }
                GradePass::Matte { src, dst, key, blur, invert, masks } => {
                    let Some(s) = sl.get(*src) else { continue };
                    let prog = mask_program(masks);
                    let windows = prog.len() > 1;
                    let finish = |mode: u32, from| matte_finish(mode, from, *invert, windows.then(|| prog.clone()));
                    let out = match (key, blur.iter().any(|r| *r > 0)) {
                        (None, false) => {
                            let t = sl.take()?;
                            work.push(self.graph_dispatch(cx, gp, &finish(MATTE_ONE, view(s)?), view(t)?));
                            t
                        }
                        (key, blurred) => {
                            // the raw key (or everywhere) into a, then clamped / blurred
                            let a = sl.take()?;
                            match key {
                                Some(k) => {
                                    let (kp, kc) = crate::colorops::program_and_codes(k)?;
                                    let mut ks = step(CHAIN_KEY, [kp.len() as u32, 0, 0, 0], &[]);
                                    ks.program = Some(kp);
                                    ks.codes = kc;
                                    work.push(Work::Compute(self.dispatch(cx, &ks, view(s)?, view(a)?, cx.dummy)));
                                }
                                None => {
                                    let g = GPass { p: [1.0; 4], ..GPass::new(G_FILL) };
                                    work.push(self.graph_dispatch(cx, gp, &g, view(a)?));
                                }
                            }
                            let b = sl.take()?;
                            if blurred {
                                // clamped (`finite_key`), then the box passes (rows, then columns)
                                let g = GPass { i1: [MATTE_CLAMP, 0, 0, 0], src: Some(view(a)?), ..GPass::new(G_MATTE) };
                                work.push(self.graph_dispatch(cx, gp, &g, view(b)?));
                                let mut bs = Vec::new();
                                boxes(blur, false, true, &mut bs);
                                boxes(blur, true, true, &mut bs);
                                let r = self.route(cx, &bs, &[b, a], b, work)?;
                                let o = if r == a { b } else { a };
                                work.push(self.graph_dispatch(cx, gp, &finish(MATTE_RAW, view(r)?), view(o)?));
                                sl.release(r);
                                o
                            } else {
                                work.push(self.graph_dispatch(cx, gp, &finish(MATTE_CLAMP, view(a)?), view(b)?));
                                sl.release(a);
                                b
                            }
                        }
                    };
                    sl.set(*dst, out);
                }
                GradePass::KeyIn { key, dst, invert, gain, offset } => {
                    let (Some(k), Some(d)) = (sl.get(*key), sl.get(*dst)) else { continue };
                    let t = sl.take()?;
                    let g = GPass {
                        i1: [u32::from(*invert), 0, 0, 0],
                        p: [*gain, *offset, 0.0, 0.0],
                        src: Some(view(d)?),
                        aux: Some(view(k)?),
                        ..GPass::new(G_KEY_IN)
                    };
                    work.push(self.graph_dispatch(cx, gp, &g, view(t)?));
                    sl.set(*dst, t);
                }
                GradePass::Limit { orig, dst, matte } => {
                    let (Some(o), Some(d), Some(m)) = (sl.get(*orig), sl.get(*dst), sl.get(*matte)) else { continue };
                    let t = sl.take()?;
                    let g = GPass { src: Some(view(d)?), aux: Some(view(o)?), aux2: Some(view(m)?), ..GPass::new(G_LIMIT) };
                    work.push(self.graph_dispatch(cx, gp, &g, view(t)?));
                    sl.set(*dst, t);
                }
                GradePass::Mix { srcs, dst } => {
                    let present: Vec<(usize, f32)> = srcs.iter().filter_map(|(s, wt)| sl.get(*s).map(|k| (k, *wt))).collect();
                    let share = 1.0 / present.len().max(1) as f32;
                    let acc = self.accumulate(
                        cx,
                        gp,
                        &mut sl,
                        &present,
                        |(k, wt), first| GPass { i1: [u32::from(first), 0, 0, 0], p: [*wt, share, 0.0, 0.0], src: view(*k), ..GPass::new(G_MIX) },
                        [0.0; 4],
                        work,
                    )?;
                    sl.set(*dst, acc);
                }
                GradePass::Over { src, dst, mode, opacity } => {
                    let (Some(s), Some(d)) = (sl.get(*src), sl.get(*dst)) else { continue };
                    let t = sl.take()?;
                    let g = GPass { i1: [mode.index(), 0, 0, 0], p: [*opacity, 0.0, 0.0, 0.0], src: Some(view(s)?), aux: Some(view(d)?), ..GPass::new(G_OVER) };
                    work.push(self.graph_dispatch(cx, gp, &g, view(t)?));
                    sl.set(*dst, t);
                }
                GradePass::Channel { src, dst, which } => {
                    let Some(s) = sl.get(*src) else { continue };
                    let t = sl.take()?;
                    let g = GPass { i1: [u32::from(*which), 0, 0, 0], src: Some(view(s)?), ..GPass::new(G_CHANNEL) };
                    work.push(self.graph_dispatch(cx, gp, &g, view(t)?));
                    sl.set(*dst, t);
                }
                GradePass::Combine { srcs, dst } => {
                    let first = srcs.iter().flatten().find_map(|s| sl.get(*s));
                    let mut t = sl.take()?;
                    let g = GPass { i1: [u32::from(first.is_some()), 0, 0, 0], src: first.and_then(view), ..GPass::new(G_COMBINE_INIT) };
                    work.push(self.graph_dispatch(cx, gp, &g, view(t)?));
                    for (c, s) in srcs.iter().enumerate() {
                        let Some(s) = s.and_then(|s| sl.get(s)) else { continue };
                        let next = sl.take()?;
                        let g = GPass { i1: [c as u32, 0, 0, 0], src: Some(view(s)?), aux: Some(view(t)?), ..GPass::new(G_COMBINE_SET) };
                        work.push(self.graph_dispatch(cx, gp, &g, view(next)?));
                        sl.release(t);
                        t = next;
                    }
                    sl.set(*dst, t);
                }
                GradePass::KeyMix { srcs, dst } => {
                    let present: Vec<(usize, KeyMode, bool)> = srcs.iter().filter_map(|(s, m, inv)| sl.get(*s).map(|k| (k, *m, *inv))).collect();
                    let acc = self.accumulate(
                        cx,
                        gp,
                        &mut sl,
                        &present,
                        |(k, mode, inv), first| {
                            let mode = match mode {
                                KeyMode::Add => 0,
                                KeyMode::Subtract => 1,
                                KeyMode::Intersect => 2,
                            };
                            GPass { i1: [u32::from(first), mode, u32::from(*inv), 0], src: view(*k), ..GPass::new(G_KEY_MIX) }
                        },
                        [0.0, 0.0, 0.0, 1.0],
                        work,
                    )?;
                    sl.set(*dst, acc);
                }
            }
        }
        sl.get(p.out)
    }

    /// A running pass over `items` (each reads the sum so far in `aux`; the first none): the
    /// texture holding the result, or `empty` filled in when there are no items.
    #[allow(clippy::too_many_arguments)]
    fn accumulate<'v, T>(
        &self,
        cx: &RouteCx<'v>,
        gp: &GraphPipe,
        sl: &mut Slots,
        items: &[T],
        pass: impl Fn(&T, bool) -> GPass<'v>,
        empty: [f32; 4],
        work: &mut Vec<Work>,
    ) -> Option<usize> {
        let mut acc: Option<usize> = None;
        for it in items {
            let t = sl.take()?;
            let mut g = pass(it, acc.is_none());
            // (every item reads a picture)
            g.src?;
            g.aux = match acc {
                Some(a) => Some(cx.views.get(a)?),
                None => None,
            };
            work.push(self.graph_dispatch(cx, gp, &g, cx.views.get(t)?));
            if let Some(a) = acc {
                sl.release(a);
            }
            acc = Some(t);
        }
        match acc {
            Some(a) => Some(a),
            None => {
                let t = sl.take()?;
                let g = GPass { p: empty, ..GPass::new(G_FILL) };
                work.push(self.graph_dispatch(cx, gp, &g, cx.views.get(t)?));
                Some(t)
            }
        }
    }

    /// One graph-shader pass into `dst` (unbound inputs read the dummy texture).
    fn graph_dispatch(&self, cx: &RouteCx<'_>, gp: &GraphPipe, g: &GPass<'_>, dst: &wgpu::TextureView) -> Work {
        let (device, queue, (w, h)) = (cx.device, cx.queue, cx.size);
        let mut bytes = Vec::with_capacity(128);
        for v in [g.code, w, h, 0].iter().chain(&g.i1) {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for k in 0..24 {
            bytes.extend_from_slice(&g.p.get(k).copied().unwrap_or(0.0).to_le_bytes());
        }
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-graph-u"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buf, 0, &bytes);
        let program = g.program.as_ref().map(|p| {
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fx-graph-program"),
                size: (p.len().max(4) * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bytes: Vec<u8> = p.iter().flat_map(|v| v.to_le_bytes()).collect();
            queue.write_buffer(&b, 0, &bytes);
            b
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx-graph"),
            layout: &gp.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(g.src.unwrap_or(cx.dummy)) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(dst) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(g.aux.unwrap_or(cx.dummy)) },
                wgpu::BindGroupEntry { binding: 4, resource: program.as_ref().unwrap_or(&self.no_program).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(g.aux2.unwrap_or(cx.dummy)) },
            ],
        });
        Work::Compute(Dispatch {
            pipe: Pipe::Graph,
            bg,
            bg1: None,
            groups: (w.div_ceil(16), h.div_ceil(16)),
            special: Some(gp.pipeline.clone()),
            user: None,
            slot: None,
        })
    }
}
