//! The wgpu compositor.
//!
//! Executes a [`FramePlan`](filmcraft_render::plan::FramePlan) on the GPU: every layer's source is
//! uploaded as textures (Y/Cb/Cr planes stay YUV — conversion to linear RGB happens per pixel in
//! the shader), drawn as a transformed quad with premultiplied "over" blending into a Rgba16Float
//! accumulator, then resolved over black into an sRGB `Rgba8UnormSrgb` texture that the UI
//! registers as a native texture. Uploads are cached by pixel-buffer identity, so a paused frame or
//! a still costs nothing.
//!
//! Every blend mode of [`filmcraft_render::Blend`] runs here. Normal and Dissolve (whose per-pixel
//! pattern is the CPU's 64-bit hash, reproduced in WGSL) use fixed-function "over" blending. The
//! other 25 modes need the colour under the layer: before such a layer is drawn, the accumulator
//! region under its quad is copied into a backdrop texture, and the fragment shader composites
//! with the CPU reference's math (`filmcraft_render::blend::composite`: sRGB-encoded straight
//! colour, W3C formulas, back to linear) and writes the result. Only those layers pay the copy.
//!
//! A layer carrying standard effects ([`filmcraft_render::plan::LayerFx`]) goes through the
//! effect stage ([`fx`]) first: its source is drawn into an `Rgba32Float` working image, the
//! effects run as compute passes in WGSL with the CPU reference's math
//! (`filmcraft_render::gpufx::FxOp::apply`), and the result is drawn like any RGBA layer. Layers
//! without effects take the single draw above.
//!
//! Layers that need converting before upload (linear f32 RGBA from CPU-rendered layers, 16-bit
//! YUV such as ProRes) are converted to half floats by [`prepare`], which frame workers run off the
//! UI thread; [`GpuCompositor::composite_prepared`] then only copies bytes into textures.
//!
//! The CPU plan executor (`filmcraft_render::plan::execute_cpu`) is the oracle; tests compare.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;

use filmcraft_color::{Matrix, Range, Transfer};
use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_render::Blend;
use filmcraft_render::plan::{FramePlan, PlanLayer, PlanStep};

pub mod adapter_pick;
pub mod colorops;
mod comp;
pub mod export_renderer;
pub mod fx;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub mod interop;
pub mod lut;
pub mod mask;
pub mod scopes;
#[cfg(target_os = "linux")]
pub mod shared_picture;
mod transition;
#[cfg(target_os = "linux")]
pub mod unpack422;
mod yuv;
pub use export_renderer::ExportRenderer;
pub use lut::GpuLut;
pub use mask::GpuMask;
pub use yuv::YuvReadback;

/// Output texture format: gamma-encoded RGBA8 (what egui expects of native textures); the resolve
/// shader applies the sRGB encoding.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

struct Uploaded {
    /// Y, Cb, Cr (or the RGBA texture) and the alpha plane (the dummy texture when there is none).
    views: [wgpu::TextureView; 4],
    kind: u32,
    code_scale: f32,
    /// Multiplier from a loaded Y'CbCr texel to its code (see [`SrcInfo::texel_scale`]).
    texel_scale: f32,
    /// Multiplier from the alpha texture's sample to alpha 0..1; 0 when the frame has no alpha plane.
    alpha_scale: f32,
    chroma: (u32, u32),
    last_used: u64,
    /// The uploaded pixel buffers, kept alive while cached: the cache key is the buffer address,
    /// and a freed buffer's address can be reused by a different frame (stale texture).
    _pixels: PixelData,
}

pub struct GpuCompositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layer_pipeline: wgpu::RenderPipeline,
    /// Blend modes that read the destination: `fs_blend`, no fixed-function blending.
    blend_pipeline: wgpu::RenderPipeline,
    final_pipeline: wgpu::RenderPipeline,
    layer_bgl: wgpu::BindGroupLayout,
    blend_bgl: wgpu::BindGroupLayout,
    final_bgl: wgpu::BindGroupLayout,
    accum: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    /// Copy of the accumulator under a blend-mode layer (allocated on first use).
    backdrop: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    output: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    /// Staging buffers of the accumulator read-back, one per band of rows (each within the
    /// device's largest buffer): (buffer, size in bytes).
    accum_readback_buf: Vec<(wgpu::Buffer, u64)>,
    uploads: HashMap<(usize, u32, u32), Uploaded>,
    clock: u64,
    dummy: wgpu::TextureView,
    /// The effect stage (None on devices without compute shaders or storage textures: layers
    /// with effects are then rendered on the CPU here).
    fx: Option<fx::FxStage>,
    /// Maximum Render Quality: minified layers are resampled through a Lanczos-3 filter.
    high_quality: bool,
    /// Scope samples of the output (compute devices only).
    scope_sampler: Option<scopes::ScopeSampler>,
    /// Export frames as Y'CbCr from the accumulator (compute devices only).
    yuv: Option<yuv::YuvPass>,
    /// The output transform of colour-managed sequences (with the effect stage).
    out_pass: Option<colorops::OutputPass>,
    /// Where the output pass writes (then copied back into the accumulator).
    post: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    transition: transition::PushStage,
    transition_inputs: [Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>; 2],
    /// The slot textures of comp programs (fork C1 step 7b), kept while comps of one size play.
    comp_slots: Vec<comp::Slot>,
    /// One slot set per comp layer of a frame (the active one is swapped into `comp_slots`).
    comp_sets: Vec<Vec<comp::Slot>>,
    /// A top-level [`render_plan`](Self::render_plan) has begun the effect pool's frame.
    fx_frame_open: bool,
    /// Bytes all comp slot sets may hold together ([`comp::COMP_VRAM_BUDGET`]; lower in tests).
    comp_vram_budget: u64,
    /// Top-level draws in a row without a comp ([`comp::COMP_IDLE_FRAMES`] frees the comp textures).
    comp_idle: u32,
    /// The output and backdrop textures comp slot draws use (swapped in like the slot's
    /// accumulator), so the frame's own keep their size when comp and frame sizes differ.
    comp_output: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    comp_backdrop: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    /// Recent comp pictures and their frames (static nodes keep their texture across frames).
    comp_images: Vec<(Arc<filmcraft_render::image::Image>, Arc<VideoFrame>)>,
    pub gpu_transitions: u64,
    pub cpu_transitions: u64,
    /// Total bytes uploaded (stats).
    pub uploaded_bytes: u64,
    /// The device has `TEXTURE_FORMAT_16BIT_NORM`: 9–16-bit planes upload as they are
    /// (`R16Unorm` / `Rg16Unorm`) instead of being converted to half floats on the CPU.
    unorm16: bool,
    /// Imports hardware-decoded pictures in place (Linux, a device from `interop::create_device`).
    #[cfg(target_os = "linux")]
    importer: Option<interop::Importer>,
    /// NVDEC's shared memory made for another device on this GPU, imported here (fork 8.6c: the
    /// export renderer), by allocation, until the allocation is freed.
    #[cfg(target_os = "linux")]
    shared_imports: HashMap<u64, (wgpu::Buffer, std::sync::Weak<()>)>,
    /// Splits imported packed 4:2:2 pictures into VRAM planes (made on first use; inner None:
    /// the device cannot, they are read back).
    #[cfg(target_os = "linux")]
    unpack422: Option<Option<unpack422::Unpack422>>,
    /// The NV12 / P010 plane textures [`GpuCompositor::write_yuv420_into_buffer`] renders before
    /// copying them into an encoder's buffer: luma, chroma, size, 10-bit.
    nv_planes: Option<(wgpu::Texture, wgpu::Texture, (u32, u32), bool)>,
    /// GPU pictures drawn in place / read back to memory first (stats).
    pub gpu_frames_in_place: u64,
    pub gpu_frames_read_back: u64,
}

/// Whether the last compositor made has 16-bit normalized textures: [`prepare`] (run on frame
/// workers, away from the device) then leaves 9–16-bit planes alone. Only a hint for speed: a
/// compositor ignores half-float planes it was handed when it uploads 16-bit ones as they are,
/// and converts planes it was not handed.
static UNORM16: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// User shader effects (ADR 0006) submitted to the GPU and not finished yet, with a count each.
static USER_IN_FLIGHT: std::sync::Mutex<Vec<((String, u64), u32)>> = std::sync::Mutex::new(Vec::new());
/// Called with the user effects that were on the GPU when a device was lost.
pub type UserEffectLostHook = Box<dyn Fn(&[(String, u64)]) + Send + Sync>;
static USER_LOST_HOOK: std::sync::Mutex<Option<UserEffectLostHook>> = std::sync::Mutex::new(None);

/// Most distinct user effects tracked in flight.
const MAX_USER_IN_FLIGHT: usize = 1024;

/// Count `keys` in (`add`) or out of flight.
fn user_in_flight(keys: &[(String, u64)], add: bool) {
    let mut v = USER_IN_FLIGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for k in keys {
        match (v.iter().position(|(e, _)| e == k), add) {
            (Some(i), true) => {
                if let Some((_, n)) = v.get_mut(i) {
                    *n = n.saturating_add(1);
                }
            }
            (Some(i), false) => {
                if let Some((_, n)) = v.get_mut(i) {
                    *n = n.saturating_sub(1);
                }
            }
            (None, true) if v.len() < MAX_USER_IN_FLIGHT => v.push((k.clone(), 1)),
            _ => {}
        }
    }
    v.retain(|(_, n)| *n > 0);
}

/// The user effects submitted to the GPU and not finished yet (id, source hash).
pub fn user_effects_in_flight() -> Vec<(String, u64)> {
    USER_IN_FLIGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().map(|(k, _)| k.clone()).collect()
}

/// Set what happens to the user effects in flight when a device is lost (the app quarantines
/// them: `filmcraft_engine::user_effects::quarantine`).
pub fn set_user_effect_lost_hook(hook: UserEffectLostHook) {
    *USER_LOST_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
}

/// A device was lost: hand the user effects in flight to the hook. Call it from the device's
/// lost callback.
pub fn notify_device_lost() {
    let keys = user_effects_in_flight();
    if keys.is_empty() {
        return;
    }
    log::error!("the GPU stopped while user shader effects ran: {keys:?}");
    if let Some(h) = USER_LOST_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
        h(&keys);
    }
}

/// The device features a [`GpuCompositor`] uses when the adapter has them: request these when
/// creating the device (`adapter.features() & filmcraft_gpu::wanted_features()`).
pub fn wanted_features() -> wgpu::Features {
    // (timestamps: the slow-pass rule of user shader effects, ADR 0006)
    wgpu::Features::TEXTURE_FORMAT_16BIT_NORM | wgpu::Features::TIMESTAMP_QUERY
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// f32 → IEEE half (round to nearest even), no dependency needed.
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - e);
        let round = (m >> 12) & 1;
        return sign | (((m >> 13) + round) as u16);
    }
    let m = mant >> 13;
    let round_bits = mant & 0x1fff;
    let mut h = (sign as u32) | ((e as u32) << 10) | m;
    if round_bits > 0x1000 || (round_bits == 0x1000 && (m & 1) == 1) {
        h += 1;
    }
    h as u16
}

/// IEEE half → f32 (the inverse of [`f32_to_f16`]), for reading `Rgba16Float` textures back.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x03ff) as u32;
    let bits = match exp {
        0 => {
            if mant == 0 {
                sign
            } else {
                // Half subnormal: value = mant × 2⁻²⁴ = (1 + frac) × 2^(b−24) with b the mantissa's
                // highest set bit — renormalise into an f32.
                let b = 31 - mant.leading_zeros();
                let frac = (mant ^ (1 << b)) << (23 - b);
                sign | ((103 + b) << 23) | frac
            }
        }
        0x1f => sign | 0x7f80_0000 | (mant << 13),
        _ => sign | ((exp + 112) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

/// [`f16_to_f32`] with a branch-light path for normal numbers and zeros (the bulk of read-back
/// data); subnormals, infinities and NaNs take the general path. Same result for every input.
#[inline(always)]
pub fn f16_to_f32_fast(h: u16) -> f32 {
    let u = u32::from(h);
    let sign = (u & 0x8000) << 16;
    let abs = u & 0x7fff;
    if (0x0400..0x7c00).contains(&abs) {
        // rebias the exponent (127 - 15 = 112) and widen the mantissa
        f32::from_bits(sign | ((abs + (112 << 10)) << 13))
    } else if abs == 0 {
        f32::from_bits(sign)
    } else {
        f16_to_f32(h)
    }
}

/// Half-float texel data for one frame (one byte vector per plane), converted off the UI thread.
/// 8-bit frames need no conversion and have none.
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    planes: Vec<Vec<u8>>,
}

impl Prepared {
    pub fn bytes(&self) -> usize {
        self.planes.iter().map(Vec::len).sum()
    }
}

/// [`Prepared`] data for every layer of a plan (index-aligned with its layers), or for the
/// fallback image of a [`FramePlan::Image`].
#[derive(Clone, Debug, Default)]
pub struct PreparedPlan {
    layers: Vec<Option<Prepared>>,
    image: Option<Prepared>,
    children: Vec<PreparedPlan>,
}

impl PreparedPlan {
    pub fn bytes(&self) -> usize {
        self.layers.iter().flatten().chain(&self.image).map(Prepared::bytes).sum::<usize>() + self.children.iter().map(Self::bytes).sum::<usize>()
    }
}

/// Values per parallel chunk when converting.
const CHUNK: usize = 1 << 15;

fn f32_to_f16_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; v.len() * 2];
    out.par_chunks_mut(CHUNK * 2).zip(v.par_chunks(CHUNK)).for_each(|(o, s)| {
        for (o, x) in o.as_chunks_mut::<2>().0.iter_mut().zip(s) {
            o.copy_from_slice(&f32_to_f16(*x).to_le_bytes());
        }
    });
    out
}

/// Half floats of `code / 2^bits` for every 16-bit code (built once per bit depth).
fn code_table(bits: u32) -> &'static [[u8; 2]] {
    static TABLES: [std::sync::OnceLock<Vec<[u8; 2]>>; 17] = [const { std::sync::OnceLock::new() }; 17];
    let bits = bits.min(16);
    TABLES[bits as usize].get_or_init(|| {
        let scale = (1u32 << bits) as f32;
        (0..=u16::MAX as u32).map(|c| f32_to_f16(c as f32 / scale).to_le_bytes()).collect()
    })
}

/// `code / 2^bits` as half floats, through a per-code table (same values as converting each sample).
fn codes_to_f16_bytes(v: &[u16], bits: u32) -> Vec<u8> {
    let table = code_table(bits);
    let mut out = vec![0u8; v.len() * 2];
    out.par_chunks_mut(CHUNK * 2).zip(v.par_chunks(CHUNK)).for_each(|(o, s)| {
        for (o, c) in o.as_chunks_mut::<2>().0.iter_mut().zip(s) {
            o.copy_from_slice(&table[*c as usize]);
        }
    });
    out
}

/// Convert a frame's texels for upload (None when it uploads as it is).
pub fn prepare_frame(f: &VideoFrame) -> Option<Prepared> {
    prepare_frame_for(f, UNORM16.load(std::sync::atomic::Ordering::Relaxed))
}

/// [`prepare_frame`] for a compositor with (`unorm16`) or without 16-bit normalized textures.
fn prepare_frame_for(f: &VideoFrame, unorm16: bool) -> Option<Prepared> {
    match &f.data {
        PixelData::RgbaF32(d) => Some(Prepared { planes: vec![f32_to_f16_bytes(d)] }),
        PixelData::Yuv16 { .. } | PixelData::Yuv16Bi { .. } if unorm16 => None,
        PixelData::Yuv16 { planes, bits, alpha, .. } => {
            Some(Prepared { planes: planes.iter().chain(alpha.as_ref()).map(|p| codes_to_f16_bytes(p, *bits)).collect() })
        }
        // MSB-aligned samples: sample / 65536 = code / 2^bits, exact in a half float
        PixelData::Yuv16Bi { y, uv, .. } => Some(Prepared { planes: vec![codes_to_f16_bytes(y, 16), codes_to_f16_bytes(uv, 16)] }),
        PixelData::Rgba8(_) | PixelData::Yuv8 { .. } | PixelData::Gpu(_) => None,
    }
}

/// Convert every layer of a plan for upload. Thread-safe and GPU-free: run it on a worker.
pub fn prepare(plan: &FramePlan) -> PreparedPlan {
    match plan {
        FramePlan::Layers { layers, .. } => prepare_layers(layers),
        FramePlan::Image(img) => PreparedPlan { image: Some(Prepared { planes: vec![f32_to_f16_bytes(&img.px)] }), ..Default::default() },
        FramePlan::Composite { steps, .. } => PreparedPlan {
            children: steps
                .iter()
                .flat_map(|s| match s {
                    PlanStep::Layers(layers) => vec![prepare_layers(layers)],
                    PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| prepare_layers(ls)).collect(),
                    // (nothing to upload; `composite_steps` takes no child for it either)
                    PlanStep::Output(_) => Vec::new(),
                })
                .collect(),
            ..Default::default()
        },
    }
}

/// Whether a plan carries an output transform (a colour-managed sequence, fork ADR 0003), which
/// needs the compute stage.
fn plan_has_output(plan: &FramePlan) -> bool {
    match plan {
        FramePlan::Layers { output, .. } => output.is_some(),
        FramePlan::Composite { steps, .. } => steps.iter().any(|s| matches!(s, PlanStep::Output(_))),
        FramePlan::Image(_) => false,
    }
}

fn prepare_layers(layers: &[PlanLayer]) -> PreparedPlan {
    // (a comp layer's frame has no pixels: its program runs when it is drawn)
    PreparedPlan { layers: layers.iter().map(|l| if l.comp.is_some() { None } else { prepare_frame(&l.frame) }).collect(), ..Default::default() }
}

#[cfg(test)]
thread_local! {
    /// Render targets created on this thread (tests check that targets are reused).
    static TARGETS_MADE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl GpuCompositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::with_fx_format(device, queue, fx::fx_format_for(device.adapter_info().device_type))
    }

    /// A compositor for export frames: the effect stage always works in full precision
    /// (`Rgba32Float`) on every GPU, whatever the device rule or `FILMCRAFT_FX_FORMAT` would pick
    /// for preview (owner, FORK-PLAN §1a.64: half floats are for preview / playback only, so an
    /// export never mixes precisions between chunks rendered on different GPUs).
    pub fn for_export(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::with_fx_format(device, queue, fx::FX_FORMAT)
    }

    /// Whether the effect stage works in half floats (preview on integrated GPUs; never for
    /// [`for_export`](Self::for_export)).
    pub fn effects_half_precision(&self) -> bool {
        self.fx.as_ref().is_some_and(|f| f.format() == fx::FX_FORMAT_HALF)
    }

    /// [`new`](Self::new) with the effect stage's working textures in `fx_format` (tests: the
    /// half-float stage integrated GPUs get, on any adapter).
    pub(crate) fn with_fx_format(device: &wgpu::Device, queue: &wgpu::Queue, fx_format: wgpu::TextureFormat) -> Self {
        let unorm16 = device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        UNORM16.store(unorm16, std::sync::atomic::Ordering::Relaxed);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
        });
        let layer_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                tex_entry(5),
            ],
        });
        let blend_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend-layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                tex_entry(4),
                tex_entry(5),
            ],
        });
        let final_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("final"),
            entries: &[wgpu::BindGroupLayoutEntry { binding: 0, ..tex_entry(0) }],
        });
        let pl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("layer"), bind_group_layouts: &[Some(&layer_bgl)], immediate_size: 0 });
        let layer_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layer"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: ACCUM_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let bpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blend-layer"),
            bind_group_layouts: &[Some(&blend_bgl)],
            immediate_size: 0,
        });
        let blend_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blend-layer"),
            layout: Some(&bpl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blend"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: ACCUM_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let fpl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("final"), bind_group_layouts: &[Some(&final_bgl)], immediate_size: 0 });
        let final_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("final"),
            layout: Some(&fpl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_full"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: OUTPUT_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dummy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy = dummy_tex.create_view(&Default::default());
        let fx = fx::FxStage::supported(device).then(|| fx::FxStage::with_format(device, &shader, &layer_bgl, fx_format));
        let out_pass = fx.is_some().then(|| colorops::OutputPass::new(device));
        let scope_sampler = fx.is_some().then(|| scopes::ScopeSampler::new(device));
        let yuv = fx.is_some().then(|| yuv::YuvPass::new(device));
        Self {
            device: device.clone(),
            queue: queue.clone(),
            layer_pipeline,
            blend_pipeline,
            final_pipeline,
            layer_bgl,
            blend_bgl,
            final_bgl,
            accum: None,
            backdrop: None,
            output: None,
            accum_readback_buf: Vec::new(),
            uploads: HashMap::new(),
            clock: 0,
            dummy,
            fx,
            high_quality: false,
            scope_sampler,
            yuv,
            out_pass,
            post: None,
            transition: transition::PushStage::new(device),
            transition_inputs: [None, None],
            comp_slots: Vec::new(),
            comp_sets: Vec::new(),
            fx_frame_open: false,
            comp_vram_budget: comp::COMP_VRAM_BUDGET,
            comp_idle: 0,
            comp_output: None,
            comp_backdrop: None,
            comp_images: Vec::new(),
            gpu_transitions: 0,
            cpu_transitions: 0,
            uploaded_bytes: 0,
            unorm16,
            #[cfg(target_os = "linux")]
            importer: interop::Importer::new(device),
            #[cfg(target_os = "linux")]
            shared_imports: HashMap::new(),
            #[cfg(target_os = "linux")]
            unpack422: None,
            nv_planes: None,
            gpu_frames_in_place: 0,
            gpu_frames_read_back: 0,
        }
    }

    fn target(
        device: &wgpu::Device,
        slot: &mut Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
        extra: wgpu::TextureUsages,
    ) -> wgpu::TextureView {
        if let Some(s) = slot.as_ref().filter(|s| s.2 == (w, h)) {
            return s.1.clone();
        }
        #[cfg(test)]
        TARGETS_MADE.with(|n| n.set(n.get().saturating_add(1)));
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filmcraft-target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | extra,
            view_formats: &[],
        });
        let v = t.create_view(&Default::default());
        *slot = Some((t, v.clone(), (w, h)));
        v
    }

    fn plane_texture(&mut self, w: u32, h: u32, format: wgpu::TextureFormat, bytes: &[u8], bpp: u32) -> wgpu::TextureView {
        let t = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filmcraft-plane"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bpp), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.uploaded_bytes += bytes.len() as u64;
        t.create_view(&Default::default())
    }

    /// Upload (or reuse) the textures of a frame, using `prep` when it was converted beforehand.
    fn upload(&mut self, f: &VideoFrame, prep: Option<&Prepared>) -> (usize, u32, u32) {
        let id = match &f.data {
            PixelData::Rgba8(d) => Arc::as_ptr(d) as *const u8 as usize,
            PixelData::RgbaF32(d) => Arc::as_ptr(d) as *const u8 as usize,
            PixelData::Yuv8 { planes, .. } => Arc::as_ptr(&planes[0]) as *const u8 as usize,
            PixelData::Yuv16 { planes, .. } => Arc::as_ptr(&planes[0]) as *const u8 as usize,
            PixelData::Yuv16Bi { y, .. } => Arc::as_ptr(y) as *const u8 as usize,
            PixelData::Gpu(s) => Arc::as_ptr(s) as *const u8 as usize,
        };
        let key = (id, f.width, f.height);
        self.clock += 1;
        if let Some(u) = self.uploads.get_mut(&key) {
            u.last_used = self.clock;
            return key;
        }
        let (w, h) = (f.width, f.height);
        let d0 = self.dummy.clone();
        let dummy = || d0.clone();
        let up = match &f.data {
            PixelData::Gpu(s) => {
                let (bits, texel) = match s.format() {
                    filmcraft_frame::GpuFormat::P010 | filmcraft_frame::GpuFormat::Y210 => (10u32, 65535.0 / 64.0),
                    filmcraft_frame::GpuFormat::Nv12 => (8, 255.0),
                };
                // 4:2:2 chroma is full height
                let chroma_size = if s.format() == filmcraft_frame::GpuFormat::Y210 { (w.div_ceil(2), h) } else { (w.div_ceil(2), h.div_ceil(2)) };
                #[cfg(target_os = "linux")]
                let imported = if s.crop_origin() != (0, 0) {
                    None
                } else if s.format() == filmcraft_frame::GpuFormat::Y210 {
                    self.import_422(s, (w, h))
                } else if let Some(planes) = self.copy_shared(s, (w, h)) {
                    // NVDEC's picture in memory shared with CUDA on this GPU (fork 8.6c)
                    Some(planes)
                } else {
                    self.importer.as_mut().and_then(|i| i.import(s.as_ref()).map(|(a, b)| (a.clone(), b.clone())))
                };
                #[cfg(not(target_os = "linux"))]
                let imported: Option<(wgpu::TextureView, wgpu::TextureView)> = None;
                match imported {
                    Some((y, uv)) => {
                        self.gpu_frames_in_place += 1;
                        Uploaded {
                            views: [y, uv, dummy(), dummy()],
                            kind: 3,
                            code_scale: (1u32 << bits) as f32,
                            texel_scale: texel,
                            alpha_scale: 0.0,
                            chroma: chroma_size,
                            last_used: self.clock,
                            _pixels: f.data.clone(),
                        }
                    }
                    None => {
                        // read back and upload as a biplanar / planar frame
                        self.gpu_frames_read_back += 1;
                        let data = s.download(w, h).unwrap_or_else(|e| {
                            log::warn!("reading a decoded picture back failed: {e}");
                            let c = (w as usize).div_ceil(2) * (h as usize).div_ceil(2);
                            PixelData::Yuv8 {
                                planes: [Arc::new(vec![16; (w * h) as usize]), Arc::new(vec![128; c]), Arc::new(vec![128; c])],
                                chroma: filmcraft_frame::Chroma::C420,
                                alpha: None,
                            }
                        });
                        let tmp = VideoFrame { data, ..f.clone() };
                        let k = self.upload(&tmp, None);
                        match self.uploads.remove(&k) {
                            Some(mut u) => {
                                // keep the surface alive with the textures, as for other frames
                                u._pixels = f.data.clone();
                                u
                            }
                            None => return key,
                        }
                    }
                }
            }
            PixelData::Rgba8(d) => {
                let v = self.plane_texture(w, h, wgpu::TextureFormat::Rgba8UnormSrgb, d, 4);
                Uploaded {
                    views: [v, dummy(), dummy(), dummy()],
                    kind: 0,
                    code_scale: 1.0,
                    texel_scale: 1.0,
                    alpha_scale: 0.0,
                    chroma: (w, h),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
                }
            }
            PixelData::RgbaF32(d) => {
                let owned;
                let half = match prep.and_then(|p| p.planes.first()) {
                    Some(p) => p,
                    None => {
                        owned = f32_to_f16_bytes(d);
                        &owned
                    }
                };
                // too few texels (a comp slot's placeholder whose registration was evicted, or a
                // short buffer): transparent instead of an invalid upload
                let need = (w as usize).saturating_mul(h as usize).saturating_mul(8);
                let v = if half.len() < need { dummy() } else { self.plane_texture(w, h, wgpu::TextureFormat::Rgba16Float, half, 8) };
                Uploaded {
                    views: [v, dummy(), dummy(), dummy()],
                    kind: 1,
                    code_scale: 1.0,
                    texel_scale: 1.0,
                    alpha_scale: 0.0,
                    chroma: (w, h),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
                }
            }
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let y = self.plane_texture(w, h, wgpu::TextureFormat::R8Unorm, &planes[0], 1);
                let u = self.plane_texture(cw, ch, wgpu::TextureFormat::R8Unorm, &planes[1], 1);
                let v = self.plane_texture(cw, ch, wgpu::TextureFormat::R8Unorm, &planes[2], 1);
                let a = alpha.as_ref().map(|a| self.plane_texture(w, h, wgpu::TextureFormat::R8Unorm, a, 1));
                let alpha_scale = if a.is_some() { 1.0 } else { 0.0 };
                Uploaded {
                    views: [y, u, v, a.unwrap_or_else(dummy)],
                    kind: 2,
                    code_scale: 256.0,
                    texel_scale: 255.0,
                    alpha_scale,
                    chroma: (cw, ch),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let bits = (*bits).clamp(1, 16);
                let scale = (1u32 << bits) as f32;
                // codes in the low bits: R16Unorm loads code / 65535, half floats code / 2^bits
                let (views, a, texel_scale) = if self.unorm16 {
                    let f = wgpu::TextureFormat::R16Unorm;
                    let y = self.plane_texture(w, h, f, bytemuck::cast_slice(&planes[0]), 2);
                    let u = self.plane_texture(cw, ch, f, bytemuck::cast_slice(&planes[1]), 2);
                    let v = self.plane_texture(cw, ch, f, bytemuck::cast_slice(&planes[2]), 2);
                    let a = alpha.as_ref().map(|a| self.plane_texture(w, h, f, bytemuck::cast_slice(a), 2));
                    ([y, u, v], a, 65535.0)
                } else {
                    let owned;
                    let p = match prep.filter(|p| p.planes.len() == 3 + alpha.is_some() as usize) {
                        Some(p) => p,
                        None => {
                            owned = Prepared { planes: planes.iter().chain(alpha.as_ref()).map(|p| codes_to_f16_bytes(p, bits)).collect() };
                            &owned
                        }
                    };
                    let f = wgpu::TextureFormat::R16Float;
                    let y = self.plane_texture(w, h, f, &p.planes[0], 2);
                    let u = self.plane_texture(cw, ch, f, &p.planes[1], 2);
                    let v = self.plane_texture(cw, ch, f, &p.planes[2], 2);
                    let a = alpha.as_ref().and_then(|_| p.planes.get(3)).map(|b| self.plane_texture(w, h, f, b, 2));
                    ([y, u, v], a, scale)
                };
                // (full-resolution alpha plane: its codes span 0..2^bits - 1, hence the scale; the
                // shader multiplies its texel by alpha_scale: texel × texel_scale / (2^bits - 1))
                let alpha_scale = if a.is_some() { texel_scale / (scale - 1.0) } else { 0.0 };
                let [y, u, v] = views;
                Uploaded {
                    views: [y, u, v, a.unwrap_or_else(dummy)],
                    kind: 2,
                    code_scale: scale,
                    texel_scale,
                    alpha_scale,
                    chroma: (cw, ch),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
                }
            }
            PixelData::Yuv16Bi { y, uv, chroma, bits } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let bits = (*bits).clamp(1, 16);
                let scale = (1u32 << bits) as f32;
                // MSB-aligned: R16Unorm loads (code << (16 - bits)) / 65535, half floats
                // (code << (16 - bits)) / 65536
                let lsb = (1u32 << (16 - bits)) as f32;
                let (yv, uvv, texel_scale) = if self.unorm16 {
                    let yv = self.plane_texture(w, h, wgpu::TextureFormat::R16Unorm, bytemuck::cast_slice(y), 2);
                    let uvv = self.plane_texture(cw, ch, wgpu::TextureFormat::Rg16Unorm, bytemuck::cast_slice(uv), 4);
                    (yv, uvv, 65535.0 / lsb)
                } else {
                    let owned;
                    let p = match prep.filter(|p| p.planes.len() == 2) {
                        Some(p) => p,
                        None => {
                            owned = Prepared { planes: vec![codes_to_f16_bytes(y, 16), codes_to_f16_bytes(uv, 16)] };
                            &owned
                        }
                    };
                    let yv = self.plane_texture(w, h, wgpu::TextureFormat::R16Float, &p.planes[0], 2);
                    let uvv = self.plane_texture(cw, ch, wgpu::TextureFormat::Rg16Float, &p.planes[1], 4);
                    (yv, uvv, 65536.0 / lsb)
                };
                Uploaded {
                    views: [yv, uvv, dummy(), dummy()],
                    kind: 3,
                    code_scale: scale,
                    texel_scale,
                    alpha_scale: 0.0,
                    chroma: (cw, ch),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
                }
            }
        };
        self.uploads.insert(key, up);
        // keep the most recent uploads (enough for several stacked layers + playback lookahead)
        if self.uploads.len() > 24 {
            let mut v: Vec<(u64, (usize, u32, u32))> = self.uploads.iter().map(|(k, u)| (u.last_used, *k)).collect();
            v.sort_unstable();
            for (_, k) in v.into_iter().take(self.uploads.len() - 24) {
                self.uploads.remove(&k);
            }
        }
        key
    }

    /// A picture NVDEC copied into memory shared with CUDA on this same GPU (fork 8.6c /
    /// XPS item 18: [`shared_picture::SharedPicture`]): its planes copied into new textures on
    /// the GPU, no CPU in the way. None (the other paths take it): another kind of surface,
    /// another device, a layout wgpu can't copy as it lies, or 10 bits without 16-bit textures.
    #[cfg(target_os = "linux")]
    fn copy_shared(&mut self, s: &Arc<dyn filmcraft_frame::GpuSurface>, (w, h): (u32, u32)) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        let sp = s.as_any()?.downcast_ref::<shared_picture::SharedPicture>()?;
        let ten = sp.format == filmcraft_frame::GpuFormat::P010;
        if !sp.copyable() || (ten && !self.unorm16) {
            return None;
        }
        // made for this device, or another device of the same GPU (the window's, when this is the
        // export renderer): its memory imported here once per allocation
        let buffer = if interop::same_device(&sp.device, &self.device) {
            sp.buffer.clone()
        } else {
            let m = sp.memory.as_ref()?;
            self.shared_imports.retain(|_, (_, alive)| alive.strong_count() > 0);
            match self.shared_imports.get(&m.key) {
                Some((b, _)) => b.clone(),
                None if self.shared_imports.len() < 64 => {
                    let fd = m.fd.try_clone().ok()?;
                    let b = match interop::import_cuda_shared(&self.device, fd, m.size, m.dedicated) {
                        Ok(b) => b,
                        Err(e) => {
                            log::warn!("importing NVDEC's shared memory into this device failed ({e}); reading pictures back");
                            return None;
                        }
                    };
                    self.shared_imports.insert(m.key, (b.clone(), m.alive.clone()));
                    b
                }
                None => return None,
            }
        };
        // the visible picture (top-left of the coded one: NVDEC's 1920 × 1088 for 1080p)
        if w == 0 || h == 0 || w > sp.coded.0 || h > sp.coded.1 {
            return None;
        }
        let (fy, fc) =
            if ten { (wgpu::TextureFormat::R16Unorm, wgpu::TextureFormat::Rg16Unorm) } else { (wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm) };
        let plane = |label: &'static str, width: u32, height: u32, format| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let max = self.device.limits().max_texture_dimension_2d;
        if w > max || h > max {
            return None;
        }
        let (luma, chroma) = (plane("NVDEC luma", w, h, fy), plane("NVDEC chroma", w.div_ceil(2), h.div_ceil(2), fc));
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("NVDEC planes") });
        for (texture, offset, rows) in [(&luma, 0, h), (&chroma, sp.chroma_offset, h.div_ceil(2))] {
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset, bytes_per_row: Some(sp.pitch), rows_per_image: Some(rows) },
                },
                texture.as_image_copy(),
                texture.size(),
            );
        }
        self.queue.submit([enc.finish()]);
        Some((luma.create_view(&Default::default()), chroma.create_view(&Default::default())))
    }

    /// A packed 4:2:2 GPU picture (Intel's linear copy, fork 6.1b) imported and unpacked once
    /// into VRAM planes (luma, interleaved chroma), or None (read it back instead). The picture
    /// is kept alive until the unpacking has finished on the GPU, so its memory is never reused
    /// by the decoder while being read.
    #[cfg(target_os = "linux")]
    fn import_422(&mut self, s: &Arc<dyn filmcraft_frame::GpuSurface>, size: (u32, u32)) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        if self.unpack422.is_none() {
            self.unpack422 = Some(unpack422::Unpack422::new(&self.device));
        }
        // any refusal tells the decoders to stop handing such pictures over (process-wide)
        let refuse = |why: &str| {
            filmcraft_frame::refuse_packed_422(why);
            None
        };
        let Some(unpack) = self.unpack422.as_mut().and_then(Option::as_mut) else {
            return refuse("no 16-bit normalized textures");
        };
        let Some(importer) = self.importer.as_mut() else { return refuse("no DMA-BUF import") };
        let Some((packed, _)) = importer.import(s.as_ref()) else { return refuse("the import failed") };
        let Some(out) = unpack.run(&self.device, &self.queue, packed, size) else {
            return refuse("unpacking failed (size beyond the device's limits)");
        };
        let hold = s.clone();
        self.queue.on_submitted_work_done(move || drop(hold));
        Some((out.luma, out.chroma))
    }

    /// The source description of an uploaded frame.
    fn src_info(&self, f: &VideoFrame, key: (usize, u32, u32)) -> Option<SrcInfo> {
        let up = self.uploads.get(&key)?;
        Some(SrcInfo {
            kind: up.kind,
            code_scale: up.code_scale,
            texel_scale: up.texel_scale,
            alpha: up.alpha_scale,
            chroma: up.chroma,
            size: (f.width, f.height),
            color: f.color,
        })
    }

    /// Maximum Render Quality on or off (the Program monitor sets it from the sequence while
    /// paused; playback keeps the cheaper supersampling).
    pub fn set_high_quality(&mut self, on: bool) {
        self.high_quality = on;
    }

    /// How a layer's frame is drawn onto its effect working image: box-decimated by the plan's
    /// `decimation` (as the CPU decodes it), resampled further when the working image is smaller
    /// still (fork Phase 5.3: converted media graded at the size drawn), with Maximum Render
    /// Quality's filter like any scaled layer.
    fn fx_source_uniforms(&self, src: &SrcInfo, frame: &VideoFrame, fx: &filmcraft_render::plan::LayerFx) -> [f32; 28] {
        let n = fx.decimation.max(1) as f64;
        let (dw, dh) = ((frame.width as f64 / n).floor().max(1.0), (frame.height as f64 / n).floor().max(1.0));
        let (fw, fh) = (fx.size.0.max(1) as f64, fx.size.1.max(1) as f64);
        let out = (fx.size.0.max(1), fx.size.1.max(1));
        if fw < dw || fh < dh {
            let m = filmcraft_geom::Affine::scale(fw / frame.width.max(1) as f64, fh / frame.height.max(1) as f64);
            self.with_quality(Self::uniforms(src, &m, 1.0, Blend::Normal, out))
        } else {
            Self::uniforms(src, &filmcraft_geom::Affine::scale(1.0 / n, 1.0 / n), 1.0, Blend::Normal, out)
        }
    }

    /// The bind group drawing `frame` onto the effect working image of `fx` (as the layer's own
    /// source draw). None when the frame could not be uploaded.
    fn fx_source_bg(&mut self, frame: &VideoFrame, fx: &filmcraft_render::plan::LayerFx) -> Option<wgpu::BindGroup> {
        let key = self.upload(frame, None);
        let src = self.src_info(frame, key)?;
        let views = self.uploads.get(&key)?.views.clone();
        let su = self.fx_source_uniforms(&src, frame, fx);
        let sbuf = self.uniform_buffer(&su);
        Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx-source"),
            layout: &self.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sbuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&views[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[3]) },
            ],
        }))
    }

    /// For each op of `fx`: the source bind groups of a temporal op's selected neighbours
    /// (`tnr::select` order; None for one that is not a source frame), empty for other ops.
    fn fx_neighbour_sources(&mut self, fx: &filmcraft_render::plan::LayerFx) -> Vec<Vec<Option<wgpu::BindGroup>>> {
        use filmcraft_render::gpufx::{FxOp, NeighbourPicture};
        let mut out = Vec::with_capacity(fx.ops.len());
        for op in &fx.ops {
            let mut v = Vec::new();
            if let FxOp::TemporalDenoise { radius, inputs: Some(inp), .. } = op {
                for i in filmcraft_render::tnr::select(inp, *radius) {
                    v.push(match inp.neighbours.get(i).map(|n| &n.picture) {
                        Some(NeighbourPicture::Frame { frame, .. }) => self.fx_source_bg(frame, fx),
                        _ => None,
                    });
                }
            }
            out.push(v);
        }
        out
    }

    /// A layer's uniforms with the resample quality: Lanczos-3 when minifying with Maximum Render
    /// Quality (`p3.w`; `p2.w` is the footprint).
    fn with_quality(&self, mut un: [f32; 28]) -> [f32; 28] {
        if self.high_quality && un[23] > filmcraft_render::image::HQ_MIN_FOOTPRINT {
            un[27] = 1.0;
        }
        un
    }

    fn uniforms(src: &SrcInfo, m: &filmcraft_geom::Affine, opacity: f32, blend: Blend, out: (u32, u32)) -> [f32; 28] {
        let (kr, kb) = src.color.matrix.kr_kb();
        // code units relative to 8-bit (2^(bits - 8)), as `filmcraft_color::normalize_y` / `_c`
        let bits_scale = src.code_scale / 256.0;
        let (yo, ys, co, cs) = match (src.color.range, src.kind) {
            (Range::Limited, 2 | 3) => (16.0 * bits_scale, 219.0 * bits_scale, 128.0 * bits_scale, 224.0 * bits_scale),
            (Range::Full, 2 | 3) => (0.0, src.code_scale - 1.0, src.code_scale / 2.0, src.code_scale - 1.0),
            _ => (0.0, 1.0, 0.0, 1.0),
        };
        let transfer = match src.color.transfer {
            Transfer::Linear => 1.0,
            Transfer::Pq => 2.0,
            Transfer::Hlg => 3.0,
            _ => 0.0,
        };
        // source pixels per output pixel → supersampling taps
        let sx = (m.a * m.a + m.b * m.b).sqrt();
        let sy = (m.c * m.c + m.d * m.d).sqrt();
        let footprint = (1.0 / sx.min(sy).max(1e-6)) as f32;
        let taps = if footprint > 1.25 { footprint.ceil().min(4.0) } else { 1.0 };
        let _ = Matrix::Bt709;
        [
            m.a as f32,
            m.b as f32,
            m.c as f32,
            m.d as f32,
            m.e as f32,
            m.f as f32,
            out.0 as f32,
            out.1 as f32,
            src.size.0 as f32,
            src.size.1 as f32,
            src.chroma.0 as f32,
            src.chroma.1 as f32,
            opacity,
            src.kind as f32,
            taps,
            transfer,
            yo,
            ys,
            co,
            cs,
            kr,
            kb,
            src.code_scale,
            footprint.max(1.0),
            blend.index() as f32,
            src.alpha,
            src.texel_scale,
            0.0,
        ]
    }

    fn uniform_buffer(&self, u: &[f32; 28]) -> wgpu::Buffer {
        let bytes: Vec<u8> = u.iter().flat_map(|v| v.to_le_bytes()).collect();
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer-u"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buf, 0, &bytes);
        buf
    }

    /// Composite a plan; returns the output view (sRGB, over black) and its size.
    pub fn composite(&mut self, plan: &FramePlan) -> (wgpu::TextureView, (u32, u32)) {
        self.composite_prepared(plan, None)
    }

    /// [`composite`](Self::composite) with texel conversions already done by [`prepare`] (the
    /// result is identical; only the upload work on this thread differs).
    pub fn composite_prepared(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>) -> (wgpu::TextureView, (u32, u32)) {
        // a colour-managed sequence's output transform needs the compute stage: without it, the
        // whole frame is made on the CPU
        if self.out_pass.is_none() && plan_has_output(plan) {
            return self.composite_prepared(&FramePlan::Image(filmcraft_render::plan::execute_cpu(plan)), None);
        }
        self.render_plan(plan, prep, true, true)
    }

    /// Draw `plan` into the accumulator (cleared first when `clear`, else over what is there) and,
    /// when `resolve`, resolve it over black into the output texture (returned; otherwise the
    /// accumulator's view). A [`FramePlan::Layers`] with an output transform ([`OutputStage`])
    /// runs it at its place (fork ADR 0003).
    fn render_plan(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>, clear: bool, resolve: bool) -> (wgpu::TextureView, (u32, u32)) {
        // one effect-pool frame per top-level draw: a comp's slot draws (nested `render_plan`s)
        // share it, so neither the comp-size nor the frame-size pooled textures are dropped and
        // made again every frame
        let top = !self.fx_frame_open;
        if top {
            self.fx_frame_open = true;
            if let Some(f) = self.fx.as_mut() {
                f.begin_frame();
            }
        }
        let out = self.render_plan_in_frame(plan, prep, clear, resolve);
        if top {
            // (a composite of steps may hold comps in its layers: counted as a comp frame)
            let had_comps = match plan {
                FramePlan::Layers { layers, .. } => layers.iter().any(|l| l.comp.is_some()),
                FramePlan::Composite { .. } => true,
                FramePlan::Image(_) => false,
            };
            self.comp_frame_done(had_comps);
            self.fx_frame_open = false;
            if let Some(f) = self.fx.as_mut() {
                f.end_frame();
            }
        }
        out
    }

    fn render_plan_in_frame(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>, clear: bool, resolve: bool) -> (wgpu::TextureView, (u32, u32)) {
        // comp layers: their programs run first, on comp-sized slots (C1 step 7b); a comp that
        // can't run here stays a comp layer and is rendered on the CPU below
        let comps_run;
        let plan = match plan {
            FramePlan::Layers { width, height, layers, output } if layers.iter().any(|l| l.comp.is_some()) => {
                let mut set = 0usize;
                let mut ran_sets = Vec::new();
                let layers = layers
                    .iter()
                    .map(|l| {
                        let ran = l.comp.as_ref().and_then(|p| {
                            set += 1;
                            let frame = self.run_comp(p, set - 1)?;
                            ran_sets.push((set - 1, usize::from(p.out)));
                            Some(frame)
                        });
                        match ran {
                            Some(frame) => PlanLayer { frame, comp: None, ..l.clone() },
                            None => l.clone(),
                        }
                    })
                    .collect();
                // a later comp's uploads may have pushed an earlier comp's result out of the
                // upload cache (it would draw transparent): register every result again now
                for (s, out) in ran_sets {
                    self.refresh_comp_output(s, out);
                }
                // sets of comps this frame doesn't have any more are let go (no set is swapped
                // out here: comps never run inside another comp's draw)
                self.comp_sets.truncate(set);
                comps_run = FramePlan::Layers { width: *width, height: *height, layers, output: output.clone() };
                &comps_run
            }
            _ => plan,
        };
        let output = match plan {
            FramePlan::Layers { output, .. } => output.clone(),
            FramePlan::Image(_) | FramePlan::Composite { .. } => None,
        };
        let owned;
        let (w, h, layers): (u32, u32, &[PlanLayer]) = match plan {
            FramePlan::Layers { width, height, layers, .. } => (*width as u32, *height as u32, layers.as_slice()),
            FramePlan::Image(img) => {
                let frame = match prep.and_then(|p| p.image.as_ref()) {
                    // The texels come from `prep`; the frame only carries size and colour.
                    Some(_) => VideoFrame {
                        width: img.w as u32,
                        height: img.h as u32,
                        data: PixelData::RgbaF32(Arc::new(Vec::new())),
                        ..VideoFrame::rgba_f32(1, 1, vec![0.0; 4])
                    },
                    None => VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px.clone()),
                };
                owned =
                    [PlanLayer { frame: Arc::new(frame), matrix: filmcraft_geom::Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, comp: None }];
                (img.w as u32, img.h as u32, &owned[..])
            }
            FramePlan::Composite { width, height, steps } => return self.composite_steps(*width, *height, steps, prep),
        };
        let (w, h) = (w.max(1), h.max(1));
        let accum_view = Self::target(&self.device, &mut self.accum, w, h, ACCUM_FORMAT, wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST);
        let post_view = output
            .as_ref()
            .map(|_| Self::target(&self.device, &mut self.post, w, h, ACCUM_FORMAT, wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC));
        let backdrop_view = if layers.iter().any(|l| l.blend.reads_destination()) {
            Some(Self::target(&self.device, &mut self.backdrop, w, h, ACCUM_FORMAT, wgpu::TextureUsages::COPY_DST))
        } else {
            None
        };
        let out_view = Self::target(&self.device, &mut self.output, w, h, OUTPUT_FORMAT, wgpu::TextureUsages::COPY_SRC);
        // Layers whose effects the GPU stage can't run (no stage on this device, a working image
        // larger than a texture, or a grade graph whose shader didn't build or whose textures don't
        // fit the budget) are rendered on the CPU here and drawn as plain RGBA layers.
        let max_side = self.device.limits().max_texture_dimension_2d;
        if layers.iter().any(|l| l.fx.as_ref().is_some_and(|f| fx::graph::has_graph(&f.ops)))
            && let Some(f) = self.fx.as_mut()
        {
            f.ensure_graph(&self.device);
        }
        let cpu_fx: Vec<Option<PlanLayer>> = layers
            .iter()
            .map(|l| {
                // a comp program the GPU couldn't run (too large) runs on the CPU here
                if l.comp.is_none() {
                    let lfx = l.fx.as_ref()?;
                    if self.fx.as_ref().is_some_and(|f| f.graphs_fit(&self.device, lfx)) && lfx.size.0.max(lfx.size.1) <= max_side {
                        return None;
                    }
                }
                let img = l.cpu_image();
                if img.w.max(img.h) > max_side as usize {
                    // larger than a texture: placed on the CPU, drawn as a canvas-size picture
                    let placed = img.transformed(w as usize, h as usize, &l.matrix);
                    return Some(PlanLayer::new(Arc::new(VideoFrame::rgba_f32(w, h, placed.px)), filmcraft_geom::Affine::IDENTITY, l.opacity, l.blend));
                }
                Some(PlanLayer::new(Arc::new(VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px)), l.matrix, l.opacity, l.blend))
            })
            .collect();
        let resolved: Vec<&PlanLayer> = layers.iter().zip(&cpu_fx).map(|(l, c)| c.as_ref().unwrap_or(l)).collect();
        let layers = resolved;
        let keys: Vec<(usize, u32, u32)> = layers
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let p = prep.and_then(|p| if matches!(plan, FramePlan::Image(_)) { p.image.as_ref() } else { p.layers.get(i).and_then(Option::as_ref) });
                // (texels converted beforehand belong to the plan's own frame)
                let p = if cpu_fx.get(i).is_some_and(Option::is_some) { None } else { p };
                self.upload(&l.frame, p)
            })
            .collect();
        // (bind group, None for a fixed-function layer or Some(region of the accumulator to copy
        // into the backdrop) for a layer that reads the destination; Some(None): off the output)
        let mut bind_groups = Vec::with_capacity(layers.len());
        // the effect stage of each layer with effects (recorded right before its draw)
        let mut jobs: Vec<Option<fx::FxJob>> = Vec::with_capacity(layers.len());
        for (l, k) in layers.iter().zip(&keys) {
            // (every frame was just uploaded; a missing upload draws transparent texels)
            let mut src = self.src_info(&l.frame, *k).unwrap_or(SrcInfo {
                kind: 1,
                code_scale: 1.0,
                texel_scale: 1.0,
                alpha: 0.0,
                chroma: (1, 1),
                size: (l.frame.width, l.frame.height),
                color: l.frame.color,
            });
            let mut views = self.uploads.get(k).map_or_else(|| std::array::from_fn(|_| self.dummy.clone()), |up| up.views.clone());
            let job = match &l.fx {
                Some(lfx) => {
                    // the source drawn onto the working image (box-decimated by n, as the CPU decodes)
                    let sbg = self.fx_source_bg(&l.frame, lfx);
                    let nbrs = self.fx_neighbour_sources(lfx);
                    match (self.fx.as_mut(), sbg) {
                        (Some(f), Some(sbg)) => f.job(&self.device, &self.queue, lfx, sbg, &nbrs, &self.dummy),
                        _ => None,
                    }
                }
                None => None,
            };
            // a grade graph the stage couldn't route after all (fork C2): the layer's effects on
            // the CPU, drawn as an RGBA picture, never the layer without its grade
            let mut on_cpu = false;
            if job.is_none() && l.fx.as_ref().is_some_and(|f| fx::graph::has_graph(&f.ops)) {
                let img = l.cpu_image();
                if img.w.max(img.h) <= max_side as usize {
                    let frame = Arc::new(VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px));
                    let key = self.upload(&frame, None);
                    if let (Some(info), Some(up)) = (self.src_info(&frame, key), self.uploads.get(&key)) {
                        log::warn!("a branched grade couldn't run on the GPU; the clip rendered on the CPU");
                        (src, views, on_cpu) = (info, up.views.clone(), true);
                    }
                }
            }
            // what the layer draws: the effect result (linear premultiplied RGBA) or the frame
            let (u, tex) = match &job {
                Some(j) => {
                    let s = SrcInfo { kind: 1, code_scale: 1.0, texel_scale: 1.0, alpha: 0.0, chroma: l.size(), size: l.size(), color: l.frame.color };
                    (
                        self.with_quality(Self::uniforms(&s, &l.matrix, l.opacity, l.blend, (w, h))),
                        [j.result.clone(), self.dummy.clone(), self.dummy.clone(), self.dummy.clone()],
                    )
                }
                None => {
                    // (a layer with effects only gets here if its job failed, which the CPU
                    // fallback above rules out; it is then drawn without them, where the working
                    // image would be)
                    let m = match &l.fx {
                        Some(_) if on_cpu => l.matrix,
                        Some(lfx) => {
                            let n = 1.0 / lfx.decimation.max(1) as f64;
                            l.matrix.then_apply(&filmcraft_geom::Affine::scale(n, n))
                        }
                        None => l.matrix,
                    };
                    (self.with_quality(Self::uniforms(&src, &m, l.opacity, l.blend, (w, h))), views)
                }
            };
            jobs.push(job);
            let buf = self.uniform_buffer(&u);
            let mut entries = vec![
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tex[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&tex[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&tex[3]) },
            ];
            let (layout, region) = match (&backdrop_view, l.blend.reads_destination()) {
                (Some(b), true) => {
                    entries.push(wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(b) });
                    (&self.blend_bgl, Some(quad_bounds(l, w, h)))
                }
                _ => (&self.layer_bgl, None),
            };
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("layer"), layout, entries: &entries });
            bind_groups.push((bg, region));
        }
        let final_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("final"),
            layout: &self.final_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&accum_view) }],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-composite") });
        // Runs of fixed-function layers draw in one pass. A layer that reads the destination
        // ends it, copies the accumulator under its quad into the backdrop and draws on its own.
        let accum_tex = self.accum.as_ref().map(|a| a.0.clone());
        let backdrop_tex = self.backdrop.as_ref().map(|b| b.0.clone());
        let post_tex = self.post.as_ref().map(|p| p.0.clone());
        let mut cleared = !clear;
        let mut converted = output.is_none();
        let mut i = 0;
        while i < bind_groups.len() || !cleared {
            // the output transform, before the first display-referred layer (captions)
            if let (false, true, Some(o)) = (converted, cleared, output.as_ref())
                && o.at <= i
            {
                converted = true;
                self.record_output(&mut enc, &accum_view, post_view.as_ref(), (accum_tex.as_ref(), post_tex.as_ref()), (w, h), &o.ops);
            }
            // a layer's effects run right before it is drawn (pooled working textures are reused
            // by the next layer with effects)
            if let (Some(Some(job)), Some(f)) = (jobs.get(i), self.fx.as_ref()) {
                f.record(&mut enc, job);
            }
            if let Some((bg, Some(region))) = bind_groups.get(i) {
                i += 1;
                let (Some(rect), Some(src), Some(dst)) = (region, &accum_tex, &backdrop_tex) else { continue };
                if !cleared {
                    accum_pass(&mut enc, &accum_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                    cleared = true;
                }
                let origin = wgpu::Origin3d { x: rect.0, y: rect.1, z: 0 };
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo { texture: src, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    wgpu::TexelCopyTextureInfo { texture: dst, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    wgpu::Extent3d { width: rect.2, height: rect.3, depth_or_array_layers: 1 },
                );
                let mut pass = accum_pass(&mut enc, &accum_view, wgpu::LoadOp::Load);
                pass.set_pipeline(&self.blend_pipeline);
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
                continue;
            }
            // a run of fixed-function layers, up to the next one that reads the destination or
            // has effects to run first
            let breaks = |j: &usize| bind_groups.get(*j).is_some_and(|(_, r)| r.is_some()) || jobs.get(*j).is_some_and(Option::is_some);
            let end = (i + 1..bind_groups.len()).find(breaks).unwrap_or(bind_groups.len()).max(i);
            let load = if cleared { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) };
            let mut pass = accum_pass(&mut enc, &accum_view, load);
            cleared = true;
            pass.set_pipeline(&self.layer_pipeline);
            for (bg, _) in bind_groups.get(i..end).unwrap_or_default() {
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
            }
            i = end;
        }
        if let (false, Some(o)) = (converted, output.as_ref()) {
            self.record_output(&mut enc, &accum_view, post_view.as_ref(), (accum_tex.as_ref(), post_tex.as_ref()), (w, h), &o.ops);
        }
        if resolve {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("resolve"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.final_pipeline);
            pass.set_bind_group(0, &final_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        if let Some(f) = self.fx.as_ref() {
            for job in jobs.iter().flatten() {
                f.after_submit(&self.queue, job);
            }
        }
        (if resolve { out_view } else { accum_view }, (w, h))
    }

    fn composite_steps(&mut self, width: usize, height: usize, steps: &[PlanStep], prep: Option<&PreparedPlan>) -> (wgpu::TextureView, (u32, u32)) {
        if steps.iter().any(|s| matches!(s, PlanStep::Transition { effect, .. } if effect.effect != "push")) {
            self.cpu_transitions += 1;
            let plan = FramePlan::Composite { width, height, steps: steps.to_vec() };
            return self.composite_prepared(&FramePlan::Image(filmcraft_render::plan::execute_cpu(&plan)), None);
        }
        let empty = FramePlan::Layers { width, height, layers: Vec::new(), output: None };
        let (canvas, dims) = self.render_plan(&empty, None, true, false);
        let mut children = prep.map(|p| p.children.iter());
        for step in steps {
            match step {
                PlanStep::Layers(layers) => {
                    let p = children.as_mut().and_then(Iterator::next);
                    self.render_plan(&FramePlan::Layers { width, height, layers: layers.clone(), output: None }, p, false, false);
                }
                PlanStep::Transition { inputs, effect, progress, .. } => {
                    let mut views = [self.dummy.clone(), self.dummy.clone()];
                    for (index, layers) in inputs.iter().enumerate() {
                        let p = children.as_mut().and_then(Iterator::next);
                        // Isolate each clip; queued draws keep the parent accumulator and blend order.
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        let (view, _) = self.render_plan(&FramePlan::Layers { width, height, layers: layers.clone(), output: None }, p, true, false);
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        views[index] = view;
                    }
                    self.transition.draw(&self.device, &self.queue, &canvas, [&views[0], &views[1]], dims, effect, *progress);
                    self.gpu_transitions += 1;
                }
                // the output transform of a colour-managed sequence on the canvas so far (fork
                // ADR 0003): an empty layer run whose output stage comes first
                PlanStep::Output(ops) => {
                    let stage = filmcraft_render::plan::OutputStage { ops: ops.clone(), at: 0 };
                    self.render_plan(&FramePlan::Layers { width, height, layers: Vec::new(), output: Some(stage) }, None, false, false);
                }
            }
        }
        self.render_plan(&empty, None, false, true)
    }

    /// Export rendering: `plan` composited into the linear float accumulator and read back as
    /// premultiplied RGBA f32 (the sRGB resolve is skipped). None for a plan the export path does
    /// not run on the GPU (transitions: the caller renders those on the CPU) or a failed read-back.
    pub fn render_export_prepared(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>) -> Option<(u32, u32, Vec<f32>)> {
        // (an output transform without the compute stage would be skipped silently: CPU instead)
        if matches!(plan, FramePlan::Composite { .. }) || (self.out_pass.is_none() && plan_has_output(plan)) {
            return None;
        }
        let t = std::time::Instant::now();
        self.render_plan(plan, prep, true, false);
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-composite-export") });
        let size = self.copy_accumulator(&mut enc)?;
        self.queue.submit([enc.finish()]);
        export_renderer::timing::add_submit(t.elapsed());
        self.map_accumulator(size)
    }

    /// The output transform on the accumulator: into `post`, then copied back.
    fn record_output(
        &self,
        enc: &mut wgpu::CommandEncoder,
        accum: &wgpu::TextureView,
        post: Option<&wgpu::TextureView>,
        (accum_tex, post_tex): (Option<&wgpu::Texture>, Option<&wgpu::Texture>),
        (w, h): (u32, u32),
        ops: &[filmcraft_color::ops::ColorOp],
    ) {
        let (Some(pass), Some(post), Some(src), Some(dst)) = (self.out_pass.as_ref(), post, post_tex, accum_tex) else { return };
        if pass.record(&self.device, &self.queue, enc, accum, post, (w, h), ops).is_none() {
            log::warn!("output transform of {} ops too large for the GPU program; shown unconverted", ops.len());
            return;
        }
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo { texture: src, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyTextureInfo { texture: dst, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    /// Messages about user shader effects turned off by the slow-pass rule (ADR 0006) since the
    /// last call (timings arrive when the device is next polled).
    pub fn take_user_effect_messages(&self) -> Vec<String> {
        self.fx.as_ref().map(fx::FxStage::take_user_messages).unwrap_or_default()
    }

    /// The slow-pass rule's limit: a user pass slower than `ms_at_1080p` (scaled by pixel count)
    /// on [`fx::USER_SLOW_FRAMES`] frames in a row is turned off ([`fx::USER_SLOW_MS_1080P`] by
    /// default).
    pub fn set_user_effect_slow_limit(&self, ms_at_1080p: f64) {
        if let Some(f) = self.fx.as_ref() {
            f.set_user_slow_limit(ms_at_1080p);
        }
    }

    /// Run the effect stage of a layer alone: `frame` decoded into its working image and `fx`
    /// applied, read back as linear premultiplied RGBA f32 (what `LayerFx` ops produce on the
    /// CPU from the same frame). None when the working image does not fit in a texture.
    pub fn effect_image(&mut self, frame: &VideoFrame, fx: &filmcraft_render::plan::LayerFx) -> Option<(u32, u32, Vec<f32>)> {
        let (w, h) = (fx.size.0.max(1), fx.size.1.max(1));
        let sbg = self.fx_source_bg(frame, fx)?;
        let nbrs = self.fx_neighbour_sources(fx);
        let stage = self.fx.as_mut()?;
        stage.begin_frame();
        let job = stage.job(&self.device, &self.queue, fx, sbg, &nbrs, &self.dummy)?;
        let half = stage.format() == fx::FX_FORMAT_HALF;
        let bpp = if half { 8 } else { 16 };
        let mut enc = self.device.create_command_encoder(&Default::default());
        stage.record(&mut enc, &job);
        let bands = record_readback(&self.device, &mut enc, &job.result_texture, (w, h), bpp);
        self.queue.submit([enc.finish()]);
        stage.after_submit(&self.queue, &job);
        stage.end_frame();
        let bytes = map_readback(&self.device, &bands?, (w, h), bpp)?;
        let out = if half {
            bytes.as_chunks::<2>().0.iter().map(|b| f16_to_f32(u16::from_le_bytes(*b))).collect()
        } else {
            bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
        };
        Some((w, h, out))
    }

    /// Set the GPU memory one layer's effect working textures may take (bytes; default
    /// [`fx::FX_VRAM_BUDGET`] or `FILMCRAFT_FX_VRAM_MB`). Above it, temporal noise reduction
    /// degrades to ±1 frame, then is skipped.
    pub fn set_fx_vram_budget(&mut self, bytes: u64) {
        if let Some(f) = self.fx.as_mut() {
            f.set_budget(bytes);
        }
    }

    /// Start reading back the scope samples of the last composited picture (at most `max_w` ×
    /// `max_h`, picked as `filmcraft_scopes::Signal` picks them), without waiting. None without
    /// an output or on devices without compute shaders.
    pub fn request_scope_samples(&self, max_w: u32, max_h: u32) -> Option<scopes::ScopeReadback> {
        let (_, view, size) = self.output.as_ref()?;
        self.scope_sampler.as_ref()?.request(&self.device, &self.queue, view, *size, (max_w, max_h))
    }

    /// The last composited picture as planar Y'CbCr 4:2:0 (`yuv420p10le` when `ten_bit`, else
    /// `yuv420p`), limited range, matrix (Kr, Kb), computed on the GPU from the float
    /// accumulator (fork Phase 5.1: export without a CPU render). Blocks until read back. None
    /// without a picture, on devices without compute shaders, or for buffers the device can't hold.
    pub fn read_yuv420(&self, kr: f32, kb: f32, ten_bit: bool) -> Option<Vec<u8>> {
        let (_, view, size) = self.accum.as_ref()?;
        self.yuv.as_ref()?.run(&self.device, &self.queue, view, *size, kr, kb, ten_bit, false)
    }

    /// As [`read_yuv420`](Self::read_yuv420) but 4:2:2 (chroma averaged over horizontal pairs,
    /// full height): planar `yuv422p10le` / `yuv422p` (fork X422: the 4:2:2 HEVC master without
    /// a CPU conversion).
    pub fn read_yuv422(&self, kr: f32, kb: f32, ten_bit: bool) -> Option<Vec<u8>> {
        let (_, view, size) = self.accum.as_ref()?;
        self.yuv.as_ref()?.run(&self.device, &self.queue, view, *size, kr, kb, ten_bit, true)
    }

    /// As [`read_yuv420`](Self::read_yuv420) / [`read_yuv422`](Self::read_yuv422) (`chroma_422`)
    /// without waiting: [`YuvReadback::wait`] for the bytes, outside whatever lock guards this
    /// compositor (fork X422: the next frame composites meanwhile).
    pub fn submit_yuv(&self, kr: f32, kb: f32, ten_bit: bool, chroma_422: bool) -> Option<YuvReadback> {
        let (_, view, size) = self.accum.as_ref()?;
        self.yuv.as_ref()?.submit(&self.device, &self.queue, view, *size, kr, kb, ten_bit, chroma_422)
    }

    /// The last composited picture converted as for [`read_yuv420`](Self::read_yuv420) and
    /// written straight into a video encoder's input surface (fork Phase 5.2c, zero-copy export):
    /// `dmabuf` is the surface's memory (luma and interleaved chroma layers, P010 when `ten_bit`,
    /// else NV12, at least the picture's size; `writable`: exported for writing), `id` / `alive`
    /// its identity for the import cache.
    /// Does not wait: the encoder must wait for the returned submission before reading the
    /// surface. None: no picture, no import support, or the driver cannot render into this
    /// memory (upload the read-back instead).
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    pub fn write_yuv420_into(
        &mut self,
        id: u64,
        alive: std::sync::Weak<()>,
        dmabuf: &filmcraft_frame::DmaBuf,
        writable: bool,
        kr: f32,
        kb: f32,
        ten_bit: bool,
    ) -> Option<wgpu::SubmissionIndex> {
        let (_, view, size) = self.accum.as_ref()?;
        let format = if ten_bit { filmcraft_frame::GpuFormat::P010 } else { filmcraft_frame::GpuFormat::Nv12 };
        // a device that cannot render these formats reports it here (to the caller, who reads
        // back instead), not as an uncaptured error that would end GPU rendering altogether
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (luma, chroma) = self.importer.as_mut()?.import_target(id, alive, dmabuf, format, writable)?;
        let targets = yuv::YuvTargets { luma, chroma, size: (dmabuf.width, dmabuf.height) };
        let sub = self.yuv.as_ref()?.run_into(&self.device, &self.queue, view, *size, kr, kb, ten_bit, &targets);
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("writing an export frame into the encoder's surface: {e}");
            return None;
        }
        sub
    }

    /// The last composited picture converted as for [`read_yuv420`](Self::read_yuv420), as NV12
    /// (P010 when `ten_bit`, 10 bits at the top of each 16), into `out` on this device: the luma
    /// plane's rows `pitch` bytes apart from offset 0, the interleaved chroma right after it at
    /// `pitch × height` (fork 8.6d D2: memory shared with CUDA that NVENC encodes in place, no
    /// read-back). `pitch` must be a multiple of 256 and hold a row. Does not wait: the encoder
    /// waits for the returned submission. None: no picture, a bad layout, or the device refused.
    pub fn write_yuv420_into_buffer(&mut self, out: &wgpu::Buffer, pitch: u32, kr: f32, kb: f32, ten_bit: bool) -> Option<wgpu::SubmissionIndex> {
        let (_, view, size) = self.accum.as_ref()?;
        let (w, h) = *size;
        if !nv_buffer_layout_ok((w, h), pitch, ten_bit, out.size()) {
            return None;
        }
        use wgpu::TextureFormat as F;
        let (fy, fc) = if ten_bit { (F::R16Unorm, F::Rg16Unorm) } else { (F::R8Unorm, F::Rg8Unorm) };
        if self.nv_planes.as_ref().is_none_or(|p| p.2 != (w, h) || p.3 != ten_bit) {
            let make = |label, (pw, ph), format| {
                self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            };
            self.nv_planes = Some((make("nv-luma", (w, h), fy), make("nv-chroma", (w / 2, h / 2), fc), (w, h), ten_bit));
        }
        let (luma, chroma, _, _) = self.nv_planes.as_ref()?;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (lv, cv) = (luma.create_view(&Default::default()), chroma.create_view(&Default::default()));
        let targets = yuv::YuvTargets { luma: &lv, chroma: &cv, size: (w, h) };
        self.yuv.as_ref()?.run_into(&self.device, &self.queue, view, (w, h), kr, kb, ten_bit, &targets)?;
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("nv-into-buffer") });
        for (texture, offset, rows) in [(luma, 0u64, h), (chroma, u64::from(pitch) * u64::from(h), h / 2)] {
            enc.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: out,
                    layout: wgpu::TexelCopyBufferLayout { offset, bytes_per_row: Some(pitch), rows_per_image: Some(rows) },
                },
                texture.size(),
            );
        }
        let sub = self.queue.submit([enc.finish()]);
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("writing an export frame into the encoder's buffer: {e}");
            return None;
        }
        Some(sub)
    }

    /// Block until `submission` has finished on the GPU (false: the device was lost).
    pub fn wait_for(device: &wgpu::Device, submission: wgpu::SubmissionIndex) -> bool {
        device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None }).is_ok()
    }

    /// The device the compositor renders with.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// A device for rendering without a window (exports): the high-performance adapter, the
    /// compositor's features, textures up to 8192 px, with DMA-BUF import of hardware-decoded
    /// frames on Linux (unless `FILMCRAFT_NO_GPU_IMPORT` is set).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        Self::headless_device_on(None)
    }

    /// [`headless_device`](Self::headless_device) on the GPU with PCI ids `pci` (vendor,
    /// device; fork 6E.3: one export compositor per GPU); `None`: the GPU Settings ▸ Playback ▸
    /// Graphics device picks ([`adapter_pick`]), else the high-performance adapter.
    /// No adapter with those ids: `None`. (Two identical cards: the first one.)
    #[cfg(not(target_arch = "wasm32"))]
    pub fn headless_device_on(pci: Option<(u32, u32)>) -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = match pci {
            // Settings ▸ Playback ▸ Graphics device (fork XPS a), else the high-performance one
            None => match adapter_pick::choose(&instance, wgpu::Backends::all()) {
                Some(a) => a,
                None => {
                    let opts = wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() };
                    pollster::block_on(instance.request_adapter(&opts)).ok()?
                }
            },
            Some((vendor, device)) => pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).into_iter().find(|a| {
                let i = a.get_info();
                i.vendor == vendor && i.device == device
            })?,
        };
        let desc = wgpu::DeviceDescriptor {
            label: Some("filmcraft export device"),
            // adapter-specific format features: `R16Unorm` / `Rg16Unorm` render targets (P010
            // written straight into the encoder's surfaces, fork Phase 5.2c)
            required_features: adapter.features() & (wanted_features() | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES),
            // (a 6.2K frame's 4:2:0 planes are ~156 MB as one u32 per sample)
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: 8192,
                max_storage_buffer_binding_size: adapter.limits().max_storage_buffer_binding_size,
                max_buffer_size: adapter.limits().max_buffer_size,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        };
        #[cfg(target_os = "linux")]
        if std::env::var_os("FILMCRAFT_NO_GPU_IMPORT").is_none()
            && let Some(Ok(d)) = interop::create_device(&adapter, &desc)
        {
            return Some(d);
        }
        pollster::block_on(adapter.request_device(&desc)).ok()
    }

    /// Read the **linear float accumulator** back as premultiplied RGBA f32 (export rendering):
    /// the display texture this crate's [`read_output`](Self::read_output) resolves is sRGB 8-bit
    /// and would clip the working-space data HDR exports need. The accumulator must have been
    /// composited first ([`composite`](Self::composite) / [`composite_prepared`](Self::composite_prepared)).
    pub fn read_accumulator(&mut self) -> Option<(u32, u32, Vec<f32>)> {
        let mut enc = self.device.create_command_encoder(&Default::default());
        let size = self.copy_accumulator(&mut enc)?;
        self.queue.submit([enc.finish()]);
        self.map_accumulator(size)
    }

    /// Record a copy of the accumulator into the staging buffers (one per band of rows that fits
    /// the device's largest buffer; grown when too small, reused otherwise); returns its width,
    /// height, row pitch in bytes and rows per band.
    fn copy_accumulator(&mut self, enc: &mut wgpu::CommandEncoder) -> Option<(u32, u32, u32, u32)> {
        let (_, _, (w, h)) = self.accum.as_ref()?;
        let (w, h) = (*w, *h);
        let (row, per) = readback_bands(&self.device, w, h, 8)?;
        let n = h.div_ceil(per) as usize;
        let needed = u64::from(row) * u64::from(per);
        self.accum_readback_buf.truncate(n);
        for i in 0..n {
            if self.accum_readback_buf.get(i).is_none_or(|(_, size)| *size < needed) {
                let b = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("accum-readback"),
                    size: needed,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                match self.accum_readback_buf.get_mut(i) {
                    Some(slot) => *slot = (b, needed),
                    None => self.accum_readback_buf.push((b, needed)),
                }
            }
        }
        let (tex, _, _) = self.accum.as_ref()?;
        for (i, (buf, _)) in self.accum_readback_buf.iter().enumerate() {
            let y0 = per.checked_mul(i as u32)?;
            let rows = per.min(h.saturating_sub(y0));
            if rows == 0 {
                break;
            }
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: y0, z: 0 }, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyBufferInfo {
                    buffer: buf,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(rows) },
                },
                wgpu::Extent3d { width: w, height: rows, depth_or_array_layers: 1 },
            );
        }
        Some((w, h, row, per))
    }

    /// Wait for the staging buffer filled by [`copy_accumulator`](Self::copy_accumulator) and
    /// convert its half floats to f32.
    fn map_accumulator(&self, (w, h, row, per): (u32, u32, u32, u32)) -> Option<(u32, u32, Vec<f32>)> {
        if w == 0 || h == 0 || per == 0 {
            return None;
        }
        let t = std::time::Instant::now();
        let n = (h.div_ceil(per) as usize).min(self.accum_readback_buf.len());
        let bufs = self.accum_readback_buf.get(..n)?;
        let len = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
        let mut rx = Vec::with_capacity(n);
        for (buf, _) in bufs {
            let (tx, r) = std::sync::mpsc::channel();
            buf.slice(..).map_async(wgpu::MapMode::Read, move |res| {
                let _ = tx.send(res);
            });
            rx.push(r);
        }
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        // (only buffers whose mapping succeeded are unmapped below; nothing returns early while
        // any is mapped, so the reused staging buffers are never left mapped)
        let ok: Vec<bool> = rx.iter().map(|r| matches!(r.recv(), Ok(Ok(())))).collect();
        let mapped = ok.iter().all(|m| *m);
        export_renderer::timing::add_map_wait(t.elapsed());
        let t = std::time::Instant::now();
        let (width, row, per) = (w as usize, row as usize, per as usize);
        let mut out = filmcraft_frame::pool::take_f32_overwritten(len);
        let mut converted = mapped;
        if mapped {
            let datas: Vec<_> = bufs.iter().filter_map(|(b, _)| b.slice(..).get_mapped_range().ok()).collect();
            // every row must be there: the pooled output holds stale values until written
            converted = datas.len() == n
                && out.chunks_exact_mut(width * 4).enumerate().all(|(y, dst)| {
                    let (band, r) = (y / per, y % per);
                    let Some(src) = datas.get(band).and_then(|d| d.get(r * row..r * row + width * 8)) else { return false };
                    for (d, s) in dst.iter_mut().zip(src.as_chunks::<2>().0) {
                        *d = f16_to_f32_fast(u16::from_le_bytes(*s));
                    }
                    true
                });
        }
        for ((buf, _), m) in bufs.iter().zip(&ok) {
            if *m {
                buf.unmap();
            }
        }
        export_renderer::timing::add_convert(t.elapsed());
        converted.then_some((w, h, out))
    }

    /// Read the output back as RGBA8 (tests / screenshots / thumbnails).
    pub fn read_output(&self) -> Option<(u32, u32, Vec<u8>)> {
        let (tex, _, (w, h)) = self.output.as_ref()?;
        let (w, h) = (*w, *h);
        let mut enc = self.device.create_command_encoder(&Default::default());
        let bands = record_readback(&self.device, &mut enc, tex, (w, h), 4);
        self.queue.submit([enc.finish()]);
        let out = map_readback(&self.device, &bands?, (w, h), 4)?;
        Some((w, h, out))
    }
}

/// Row pitch (bytes, 256-aligned) of a `w`-texel row of `bpp` bytes per texel, and the rows per
/// read-back band so that one band fits the device's largest buffer. None when even one row
/// does not fit (or the numbers overflow).
fn readback_bands(device: &wgpu::Device, w: u32, h: u32, bpp: u32) -> Option<(u32, u32)> {
    let row = u64::from(w).checked_mul(u64::from(bpp))?.div_ceil(256).checked_mul(256)?;
    let max = device.limits().max_buffer_size;
    if row == 0 || row > max || h == 0 {
        return None;
    }
    let rows = (max / row).min(u64::from(h)).max(1);
    Some((u32::try_from(row).ok()?, u32::try_from(rows).ok()?))
}

/// One read-back band: a mappable buffer holding `rows` rows of pitch `row`.
struct Band {
    buf: wgpu::Buffer,
    rows: u32,
    row: u32,
}

/// Record copies of `tex` (`w`×`h`, `bpp` bytes per texel) into mappable buffers, one per band
/// of rows, each within the device's largest buffer size.
fn record_readback(device: &wgpu::Device, enc: &mut wgpu::CommandEncoder, tex: &wgpu::Texture, (w, h): (u32, u32), bpp: u32) -> Option<Vec<Band>> {
    let (row, per) = readback_bands(device, w, h, bpp)?;
    let mut bands = Vec::new();
    let mut y0 = 0u32;
    while y0 < h {
        let rows = per.min(h - y0);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(row) * u64::from(rows),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: y0, z: 0 }, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(rows) } },
            wgpu::Extent3d { width: w, height: rows, depth_or_array_layers: 1 },
        );
        bands.push(Band { buf, rows, row });
        y0 += rows;
    }
    Some(bands)
}

/// Wait for the bands of [`record_readback`] (submitted) and return the rows tightly packed.
fn map_readback(device: &wgpu::Device, bands: &[Band], (w, h): (u32, u32), bpp: u32) -> Option<Vec<u8>> {
    let width = (w as usize).checked_mul(bpp as usize)?;
    let mut rx = Vec::with_capacity(bands.len());
    for b in bands {
        let (tx, r) = std::sync::mpsc::channel();
        b.buf.slice(..).map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        rx.push(r);
    }
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let mut ok = true;
    for r in &rx {
        ok &= matches!(r.recv(), Ok(Ok(())));
    }
    if !ok {
        return None;
    }
    // (one-shot buffers: dropped mapped, which frees them)
    let mut out = Vec::with_capacity(width.checked_mul(h as usize)?);
    for b in bands {
        let data = b.buf.slice(..).get_mapped_range().ok()?;
        for y in 0..b.rows as usize {
            let start = y.checked_mul(b.row as usize)?;
            out.extend_from_slice(data.get(start..start.checked_add(width)?)?);
        }
    }
    (out.len() == width * h as usize).then_some(out)
}

/// Shader source: kind (0 RGBA8 sRGB, 1 linear premultiplied RGBA, 2 YUV), sampling scales, dimensions, color.
struct SrcInfo {
    kind: u32,
    /// 2^bits of a Y'CbCr source (256 for 8-bit; code units for the range offsets).
    code_scale: f32,
    /// Loaded texel × this = code (255 for R8Unorm, 65535 for R16Unorm, 2^bits for half floats
    /// of code / 2^bits, less for MSB-aligned planes).
    texel_scale: f32,
    /// Alpha texture sample → alpha (0: the frame has no alpha plane).
    alpha: f32,
    chroma: (u32, u32),
    size: (u32, u32),
    color: filmcraft_color::ColorInfo,
}

/// A render pass drawing into the accumulator.
fn accum_pass<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("layers"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// The output pixels a layer's quad can touch, as (x, y, width, height) clamped to the `w`×`h`
/// output with a one-pixel margin; None when it touches none. A non-finite matrix covers it all.
fn quad_bounds(l: &PlanLayer, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let m = &l.matrix;
    let (fw, fh) = (l.size().0 as f64, l.size().1 as f64);
    let pts = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)].map(|(x, y)| (m.a * x + m.c * y + m.e, m.b * x + m.d * y + m.f));
    if pts.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return Some((0, 0, w, h));
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in pts {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    // in range before the cast (`as` saturates anyway; NaN is excluded above)
    let px = |v: f64, hi: u32| v.max(0.0).min(hi as f64) as u32;
    let (x0, y0) = (px(x0.floor() - 1.0, w), px(y0.floor() - 1.0, h));
    let (x1, y1) = (px(x1.ceil() + 1.0, w), px(y1.ceil() + 1.0, h));
    (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
}

#[cfg(test)]
mod blend_tests;
#[cfg(test)]
mod colorops_tests;
#[cfg(test)]
mod comp_tests;
#[cfg(test)]
mod fx_tests;
#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod user_fx_tests;

/// Whether an NV12 / P010 picture of `size` fits `bytes` at `pitch` (fork 8.6d D2): even sizes,
/// a pitch that is a multiple of wgpu's 256-byte row alignment and holds a row, luma and chroma
/// inside the buffer.
pub fn nv_buffer_layout_ok((w, h): (u32, u32), pitch: u32, ten_bit: bool, bytes: u64) -> bool {
    let row = u64::from(w) * if ten_bit { 2 } else { 1 };
    w > 0
        && h > 0
        && w.is_multiple_of(2)
        && h.is_multiple_of(2)
        && pitch.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        && u64::from(pitch) >= row
        && u64::from(pitch).checked_mul(u64::from(h) + u64::from(h / 2)).is_some_and(|need| need <= bytes)
}
