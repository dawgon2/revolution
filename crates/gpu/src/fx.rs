//! The GPU effect stage: standard effects of a plan layer ([`LayerFx`]) run in WGSL (`fx.wgsl`).
//!
//! A layer with effects is first drawn from its source (YUV planes or RGBA, through the
//! compositor's own `fs` shader, so decoding is the same as for a plain layer) into a working
//! texture of the size the CPU decodes it at; each [`FxOp`] then runs as compute passes that
//! ping-pong between two working textures (three for Unsharp Mask, which keeps the original);
//! the last one is drawn into the accumulator with the layer's matrix, opacity and blend mode
//! like any RGBA layer. Working textures are `Rgba32Float` (the CPU reference's precision) and
//! pooled per size; layers without effects never touch any of this. Preview may use `Rgba16Float`
//! instead ([`fx_format_for`], fork U0.2.3b: half the bandwidth; opt-in for now, owner question 18);
//! export always works in `Rgba32Float` (`GpuCompositor::for_export`, FORK-PLAN §1a.64).
//!
//! Box blurs (Gaussian Blur, Camera Blur, the blur inside Unsharp / Sharpen) run per pixel for
//! radii up to [`BOX_PER_PIXEL_MAX`] and as a running sum per row / column above, so a huge
//! radius costs O(1) per pixel like on the CPU (radii are capped by `gaussian_boxes`).
//!
//! Temporal noise reduction (`FxOp::TemporalDenoise`, fork 6.7) also draws each neighbouring
//! source frame into a scratch texture, replays the passes of the ops before it there (so the
//! neighbour is in the same state as the layer's image), converts it to (Y, Cb, Cr, α)
//! (`OP_TEMPORAL_INIT`), uploads the flows as `Rg32Float` textures and blends everything in one
//! `temporal_accumulate` pass (accumulation and finish fused: the luma and chroma sums and their
//! two weights are five values, more than one storage texture holds).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_render::gpufx::FxOp;
use filmcraft_render::plan::LayerFx;

pub(crate) mod graph;

/// Working texture format (full precision; [`FX_FORMAT_HALF`] on integrated GPUs).
pub(crate) const FX_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
/// Working texture format on integrated GPUs ([`fx_format_for`]).
pub(crate) const FX_FORMAT_HALF: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Whether preview on integrated GPUs uses [`FX_FORMAT_HALF`]. Off, provisional (owner question 18):
/// the owner's bar for half floats is under one 10-bit code value from f32 (FORK-PLAN §1a.62 /
/// §1a.64); colour ops meet it (~0.3), Sharpen / Unsharp don't (~4, `fx_tests::half_float_preview_*`),
/// and on the Intel UHD 750 the colour chain is ALU-bound, so half floats barely speed preview there.
const HALF_ON_INTEGRATED: bool = false;

/// The working texture format for preview on a device: [`FX_FORMAT_HALF`] on integrated GPUs when
/// [`HALF_ON_INTEGRATED`], [`FX_FORMAT`] otherwise. `FILMCRAFT_FX_FORMAT=f16` / `f32` overrides it
/// (A/B runs). Export never comes here (`GpuCompositor::for_export` is always f32).
pub(crate) fn fx_format_for(device_type: wgpu::DeviceType) -> wgpu::TextureFormat {
    fx_format_pick(device_type, std::env::var("FILMCRAFT_FX_FORMAT").ok().as_deref())
}

fn fx_format_pick(device_type: wgpu::DeviceType, forced: Option<&str>) -> wgpu::TextureFormat {
    match forced.map(str::trim) {
        Some("f16") => FX_FORMAT_HALF,
        Some("f32") => FX_FORMAT,
        _ if HALF_ON_INTEGRATED && device_type == wgpu::DeviceType::IntegratedGpu => FX_FORMAT_HALF,
        _ => FX_FORMAT,
    }
}

/// `src` (a WGSL source writing `rgba32float` storage textures) for working textures of `format`.
fn wgsl_for(src: &str, format: wgpu::TextureFormat) -> String {
    if format == FX_FORMAT_HALF { src.replace("texture_storage_2d<rgba32float, write>", "texture_storage_2d<rgba16float, write>") } else { src.to_string() }
}

/// Bytes per working-texture texel.
fn texel_bytes(format: wgpu::TextureFormat) -> u64 {
    if format == FX_FORMAT_HALF { 8 } else { 16 }
}
/// Box radii up to this many pixels are summed per output pixel; larger ones as a running sum.
pub const BOX_PER_PIXEL_MAX: u32 = 32;

const OP_BRIGHTNESS_CONTRAST: u32 = 1;
const OP_PROC_AMP: u32 = 2;
const OP_TINT: u32 = 3;
const OP_BLACK_WHITE: u32 = 4;
const OP_COLOR_BALANCE: u32 = 5;
const OP_LEAVE_COLOR: u32 = 6;
const OP_CHANGE_TO_COLOR: u32 = 7;
const OP_COLOR_PASS: u32 = 8;
const OP_GAMMA: u32 = 9;
const OP_LEVELS: u32 = 10;
const OP_EXTRACT: u32 = 11;
const OP_INVERT: u32 = 12;
const OP_INVERT_ALPHA: u32 = 13;
const OP_POSTERIZE: u32 = 14;
const OP_ASC_CDL: u32 = 15;
const OP_CHANNEL_MIX: u32 = 16;
const OP_COLOR_REPLACE: u32 = 17;
const OP_ALPHA_ADJUST: u32 = 18;
const OP_VIGNETTE: u32 = 19;
const OP_BOX: u32 = 20;
const OP_DIRECTIONAL: u32 = 22;
const OP_UNSHARP: u32 = 23;
const OP_CROP: u32 = 24;
const OP_RESAMPLE: u32 = 25;
const OP_HFLIP: u32 = 26;
const OP_VFLIP: u32 = 27;
const OP_MIRROR: u32 = 28;
const OP_OFFSET: u32 = 29;
const OP_MEDIAN_MASK: u32 = 30;
const OP_VIDEO_LIMITER: u32 = 31;
const OP_DENOISE: u32 = 32;
const OP_DENOISE_YCC: u32 = 33;
const OP_TEMPORAL_INIT: u32 = 34;
/// Up to three box passes along one axis in one dispatch (`fx.wgsl` `fx_box3`, [`fuse_boxes`]).
const OP_BOX3: u32 = 35;
/// Unsharp Mask with at most one box per axis in one pass (`fx.wgsl`, [`fused_steps`]).
const OP_UNSHARP_DIRECT: u32 = 36;
/// Luma Key (fork Y3.1): a point op that scales the premultiplied pixel by a luma ramp.
const OP_LUMA_KEY: u32 = 37;
/// Ultra Key (fork Y3.1b): the matte into red (a pass before a combine), the choke (reads the
/// blurred matte and the original), and the combine (spill, colour correction, output).
const OP_UKEY_MATTE: u32 = 38;
const OP_UKEY_CHOKE: u32 = 39;
const OP_UKEY: u32 = 40;
/// Color Key (fork Y3.1c): a point op.
const OP_COLOR_KEY: u32 = 41;
/// The largest box radius [`OP_UNSHARP_DIRECT`] takes ((2r + 1)² loads per pixel).
const UNSHARP_DIRECT_MAX: u32 = 3;
/// Pixels per `fx_box3` workgroup (`BOX3_TILE` in `fx.wgsl`).
const BOX3_TILE: u32 = 256;
/// Neighbour slots of the temporal pass (`tnr::MAX_NEIGHBOURS`).
const TEMPORAL_SLOTS: usize = 4;
// (Lumetri is not one op: it is a colour chain, `FxOp::ColorChain`, fork ADR 0003)
/// Step codes of colour-chain passes: plain, key (writes the mask), masked (reads it).
const CHAIN_KEY: u32 = 1;
const CHAIN_MASKED: u32 = 2;
/// Mask pass codes (`colorops.wgsl` `mask_pass`): mix with the saved image, scale.
const MASK_MIX: u32 = 3;
const MASK_SCALE: u32 = 4;
/// The bitmap matte mix (`fx.wgsl` `matte_mix`, [`FxOp::MaskedBy`]); its step carries the matte.
const MATTE_MIX: u32 = 5;
/// The matte's binding in the matte pass's group 1 (`fx.wgsl`; apart from the temporal pass's).
const MATTE_BINDING: u32 = 13;

type Target = (wgpu::Texture, wgpu::TextureView);

pub(crate) struct FxStage {
    px: wgpu::ComputePipeline,
    run: wgpu::ComputePipeline,
    /// Fused box cascades (`fx_box3`; None: the device's workgroup memory is too small).
    box3: Option<wgpu::ComputePipeline>,
    /// A colour-operation chain (`colorops.wgsl`), one pass.
    chain: wgpu::ComputePipeline,
    /// A key chain writing its mask, and a chain correcting `aux` inside the mask in `src`.
    chain_key: wgpu::ComputePipeline,
    chain_masked: wgpu::ComputePipeline,
    /// Mask coverage applied to `src` (mix with the saved image in `aux`, or scale).
    mask: wgpu::ComputePipeline,
    /// Bound as the program of the passes that have none.
    no_program: wgpu::Buffer,
    bgl: wgpu::BindGroupLayout,
    /// Draws a layer's source into a working texture (`composite.wgsl` `fs`, no blending).
    source: wgpu::RenderPipeline,
    /// The temporal blend (`temporal_accumulate`) and its second bind group's layout.
    temporal: wgpu::ComputePipeline,
    tbgl: wgpu::BindGroupLayout,
    /// The bitmap matte mix (`matte_mix`) and its second bind group's layout (the matte, `R8Unorm`).
    matte: wgpu::ComputePipeline,
    mbgl: wgpu::BindGroupLayout,
    pool: HashMap<(u32, u32), Vec<Target>>,
    used: HashSet<(u32, u32)>,
    /// GPU memory one layer's working textures may take ([`FX_VRAM_BUDGET`]).
    budget: u64,
    /// Working texture format ([`fx_format_for`]).
    format: wgpu::TextureFormat,
    /// The chain shader's source for `format` and its pipeline layout, for chains specialised to
    /// their op codes ([`colorops::specialise`](crate::colorops::specialise)).
    chain_src: String,
    pl: wgpu::PipelineLayout,
    /// Specialised chain pipelines by entry and op codes (None: failed to build; the interpreter
    /// runs that chain).
    special: Mutex<HashMap<(Pipe, Vec<u32>), Option<wgpu::ComputePipeline>>>,
    /// The effect shader and per-op `fx_px` pipelines (`PX_OP` set; None: failed to build).
    shader: wgpu::ShaderModule,
    px_special: Mutex<HashMap<u32, Option<wgpu::ComputePipeline>>>,
    /// User shader effects (ADR 0006): their bind group layout (uniform, source, destination),
    /// pipeline layout and pipelines by (id, source hash) (None: failed to build; the pass is
    /// skipped).
    ubgl: wgpu::BindGroupLayout,
    upl: wgpu::PipelineLayout,
    user_pipes: Mutex<HashMap<(String, u64), Option<wgpu::ComputePipeline>>>,
    /// The slow-pass rule (ADR 0006) and whether the device has timestamp queries to apply it.
    watch: Arc<Mutex<UserWatch>>,
    timestamps: bool,
    /// Run every pass on the generic shaders (tests compare the two).
    interpret_only: bool,
    /// Fuse passes ([`fuse_passes`]; tests compare with the unfused passes).
    fuse: bool,
    /// The grade graph pipeline (None: not built yet; Some(None): it failed, graphs render on
    /// the CPU).
    graph: Option<Option<graph::GraphPipe>>,
}

/// Specialised chain pipelines kept at most (the cache is cleared when full).
const MAX_SPECIAL_PIPELINES: usize = 48;

/// Whether colour chains run as shaders specialised to their op codes (`FILMCRAFT_CHAIN_SPECIALISE=0`
/// turns it off: the interpreter, for A/B checks).
pub(crate) fn specialise_chains() -> bool {
    !cfg!(target_arch = "wasm32") && std::env::var("FILMCRAFT_CHAIN_SPECIALISE").map_or(true, |v| v != "0")
}

/// Whether a layer's passes are fused (fork X422 2b, `docs/fork/DESIGN-X422-FUSED.md`; on unless
/// `FILMCRAFT_FX_FUSE=0`, for A/B checks).
pub(crate) fn fuse_passes() -> bool {
    std::env::var("FILMCRAFT_FX_FUSE").map_or(true, |v| v != "0")
}

/// Workgroup memory `fx_box3` needs (two shared rows of `BOX3_TILE + 2 × 3 × BOX_PER_PIXEL_MAX`
/// RGBA f32 texels).
const BOX3_SHARED_BYTES: u32 = 2 * (BOX3_TILE + 6 * BOX_PER_PIXEL_MAX) * 16;

/// Runs of 2–3 adjacent per-pixel box passes along the same axis with the same edge mode become
/// one `fx_box3` pass (same arithmetic: `fx_box3` runs the cascade in workgroup memory). Running-sum
/// passes (radius > [`BOX_PER_PIXEL_MAX`]) and everything else stay as they are.
fn fuse_boxes(steps: Vec<Step>) -> Vec<Step> {
    let fusable = |s: &Step| {
        s.code == OP_BOX && !s.run && s.program.is_none() && !s.combine && !s.save && !s.restore && s.temporal.is_none() && s.i1[0] <= BOX_PER_PIXEL_MAX
    };
    let mut out: Vec<Step> = Vec::with_capacity(steps.len());
    let mut run: Vec<Step> = Vec::new();
    let flush = |run: &mut Vec<Step>, out: &mut Vec<Step>| {
        if let (true, Some(first)) = (run.len() >= 2, run.first()) {
            let mut packed = 0u32;
            for (k, s) in run.iter().enumerate() {
                packed |= s.i1[0].min(BOX_PER_PIXEL_MAX) << (8 * k);
            }
            let mut f = step(OP_BOX3, [packed, first.i1[1], first.i1[2], run.len() as u32], &[]);
            f.unsharp_blur = first.unsharp_blur;
            out.push(f);
            run.clear();
        } else {
            out.append(run);
        }
    };
    for s in steps {
        let joins = fusable(&s) && run.first().is_none_or(|f| f.i1[1] == s.i1[1] && f.i1[2] == s.i1[2] && f.unsharp_blur == s.unsharp_blur) && run.len() < 3;
        if !joins {
            flush(&mut run, &mut out);
        }
        if fusable(&s) {
            run.push(s);
        } else {
            out.push(s);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// The passes of `op` with fusion on: Unsharp Mask with at most one small box per axis (Sharpen at
/// typical sizes) is one pass that sums its blur itself; box cascades become [`fuse_boxes`] passes.
fn fused_steps(op: &FxOp) -> Vec<Step> {
    // HSL Secondary without Denoise or Blur: key and correct in one plain chain
    if let FxOp::HslSecondary { key, median_r, denoise, blur, correct } = op
        && !(*denoise > 0.0 && *median_r > 0)
        && blur.iter().all(|r| *r == 0)
        && let (Some((kp, kc)), Some((cp, cc))) = (crate::colorops::program_and_codes(key), crate::colorops::program_and_codes(correct))
    {
        use crate::colorops::{CO_KEY_BEGIN, CO_KEY_END};
        let mut p = Vec::with_capacity(kp.len() + cp.len() + 2);
        p.push(CO_KEY_BEGIN);
        p.extend(kp);
        p.push(CO_KEY_END);
        p.extend(cp);
        // (a code list is only usable when every op has its code; else the interpreter runs it)
        let codes = if kc.is_empty() || cc.is_empty() { Vec::new() } else { [vec![CO_KEY_BEGIN as u32], kc, vec![CO_KEY_END as u32], cc].concat() };
        if p.len() <= crate::colorops::MAX_PROGRAM {
            let mut s = step(0, [p.len() as u32, 0, 0, 0], &[]);
            s.program = Some(p);
            s.codes = codes;
            return vec![s];
        }
    }
    if let FxOp::Unsharp { rx, ry, amount, threshold } = op {
        let used = |v: &[u32]| v.iter().copied().filter(|r| *r > 0).collect::<Vec<u32>>();
        let (x, y) = (used(rx), used(ry));
        if x.len() <= 1 && y.len() <= 1 && x.iter().chain(&y).all(|r| *r <= UNSHARP_DIRECT_MAX) {
            let (rx, ry) = (x.first().copied().unwrap_or(0), y.first().copied().unwrap_or(0));
            return vec![step(OP_UNSHARP_DIRECT, [rx, ry, 0, 0], &[*amount, *threshold])];
        }
    }
    fuse_boxes(steps(op))
}

/// Whether `s` is a plain colour-chain pass (one program over the image, nothing kept aside).
fn plain_chain(s: &Step) -> bool {
    s.code == 0 && s.program.is_some() && !s.codes.is_empty() && !s.run && !s.combine && !s.unsharp_blur && !s.save && !s.restore && s.temporal.is_none()
}

/// Adjacent plain colour-chain passes become one (the programs run one after the other per pixel;
/// every op's operands are relative to its code, so programs concatenate), within the program and
/// specialisation limits. A chain's result is stored premultiplied and divided again by the next
/// pass, so the merged pass can differ from the two by an ulp.
fn merge_chains(steps: Vec<Step>) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::with_capacity(steps.len());
    for s in steps {
        if let Some(prev) = out.last_mut()
            && plain_chain(prev)
            && plain_chain(&s)
            && let (Some(a), Some(b)) = (prev.program.as_mut(), s.program.as_ref())
            && a.len() + b.len() <= crate::colorops::MAX_PROGRAM
            && prev.codes.len() + s.codes.len() <= crate::colorops::MAX_SPECIALISED_OPS
        {
            a.extend_from_slice(b);
            prev.i1[0] = a.len() as u32;
            prev.codes.extend_from_slice(&s.codes);
            continue;
        }
        out.push(s);
    }
    out
}

/// Which compute pipeline a pass runs.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Pipe {
    Px,
    Run,
    Box3,
    Chain,
    ChainKey,
    ChainMasked,
    Mask,
    Temporal,
    /// The bitmap matte mix (`matte_mix`; group 1 holds the matte).
    Matte,
    /// A user shader effect (its pipeline is the dispatch's `special`).
    User,
    /// A grade graph pass (`fx/graph.wgsl`; its pipeline is the dispatch's `special`).
    Graph,
}

/// One compute pass of a job.
struct Dispatch {
    pipe: Pipe,
    bg: wgpu::BindGroup,
    /// Group 1 (the temporal pass only).
    bg1: Option<wgpu::BindGroup>,
    groups: (u32, u32),
    /// A chain pipeline specialised to the step's op codes, run instead of `pipe`'s.
    special: Option<wgpu::ComputePipeline>,
    /// A user effect's pass: its (id, hash) and its timestamp pair in the job's query set.
    user: Option<(String, u64)>,
    slot: Option<u32>,
}

/// One recorded piece of a job, in order.
enum Work {
    /// Draw a source frame (layer bind group) into a working texture.
    Source {
        bg: wgpu::BindGroup,
        target: wgpu::TextureView,
    },
    Compute(Dispatch),
}

/// A temporal op of a job: its drawable neighbours (source bind group and sanitised flows, in
/// `tnr::select` order), the passes to replay on them, and the constants.
struct TemporalSpec {
    nbrs: Vec<(wgpu::BindGroup, filmcraft_render::flow::FlowField, filmcraft_render::flow::FlowField)>,
    pre: Vec<Step>,
    params: filmcraft_render::tnr::TnrParams,
}

/// The recorded work of one layer's effects (bind groups made; recorded into the frame's encoder
/// right before the layer is drawn, so layers can share pooled textures).
pub(crate) struct FxJob {
    work: Vec<Work>,
    /// The texture holding the result.
    pub(crate) result: wgpu::TextureView,
    pub(crate) result_texture: wgpu::Texture,
    /// The user passes' GPU timestamps (slow-pass rule; None: no user pass or no timestamp
    /// queries on this device).
    timing: Option<UserTiming>,
}

/// User passes timed in one job (each its own compute pass with begin / end timestamps).
struct UserTiming {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// (id, hash) per timestamp pair.
    users: Vec<(String, u64)>,
    pixels: u64,
    /// Nanoseconds per timestamp tick.
    period: f32,
}

/// User passes timed per job at most (later ones run untimed).
const MAX_TIMED_USER_PASSES: usize = 32;

/// A user pass slower than this at 1080p (scaled by pixel count) on [`USER_SLOW_FRAMES`] frames
/// in a row disables the effect for the session (ADR 0006).
pub const USER_SLOW_MS_1080P: f64 = 50.0;
pub const USER_SLOW_FRAMES: u8 = 3;

/// The slow-pass rule's state, shared with the timestamp read-back callbacks.
pub(crate) struct UserWatch {
    slow: HashMap<(String, u64), u8>,
    disabled: HashSet<(String, u64)>,
    messages: Vec<String>,
    limit_ms_1080p: f64,
}

impl UserWatch {
    fn new() -> Self {
        UserWatch { slow: HashMap::new(), disabled: HashSet::new(), messages: Vec::new(), limit_ms_1080p: USER_SLOW_MS_1080P }
    }

    /// One timed pass of `key` over `pixels` took `ms`.
    fn observe(&mut self, key: &(String, u64), ms: f64, pixels: u64) {
        let limit = self.limit_ms_1080p * (pixels as f64 / (1920.0 * 1080.0));
        if !ms.is_finite() || ms <= limit {
            self.slow.remove(key);
            return;
        }
        let n = self.slow.entry(key.clone()).or_insert(0);
        *n = n.saturating_add(1);
        if *n >= USER_SLOW_FRAMES && self.disabled.insert(key.clone()) {
            let msg = format!(
                "the shader effect {} took {ms:.0} ms per frame ({} frames in a row over {limit:.1} ms): turned off until Revolution restarts",
                key.0, USER_SLOW_FRAMES
            );
            log::warn!("{msg}");
            self.messages.push(msg);
        }
    }
}

/// One pass before texture routing: entry point, op code, integer and float parameters.
struct Step {
    run: bool,
    code: u32,
    i1: [u32; 4],
    p: [f32; 24],
    /// Unsharp combine: reads the blurred image and the original.
    combine: bool,
    /// A pass before a combine (Unsharp Mask's blur, HSL Secondary's key and refine): the image
    /// it started from stays untouched until the combine.
    unsharp_blur: bool,
    /// A colour-operation chain's program (`colorops.rs`); the pass runs `colorops.wgsl`.
    program: Option<Vec<f32>>,
    /// The chain's op codes (empty: not a chain, or run it on the interpreter).
    codes: Vec<u32>,
    /// No pass: remember the current image for a masked effect's mix.
    save: bool,
    /// The mask mix: reads the saved image.
    restore: bool,
    /// The temporal blend of `TemporalSpec` n (a combine: src = current YCC, aux = original).
    temporal: Option<usize>,
    /// A user shader effect's pass (its own module and bind group layout).
    user: Option<Box<filmcraft_render::gpufx::UserOp>>,
    /// A branched grade's program (fork C2): routed by [`FxStage::route_graph`].
    graph: Option<Arc<filmcraft_render::grade::GradeProgram>>,
    /// The bitmap matte of a [`MATTE_MIX`] pass (fork G3.2).
    matte: Option<Arc<filmcraft_render::matte::BitmapMatte>>,
}

fn step(code: u32, i1: [u32; 4], p: &[f32]) -> Step {
    let mut q = [0.0; 24];
    for (d, s) in q.iter_mut().zip(p) {
        *d = *s;
    }
    Step {
        run: false,
        code,
        i1,
        p: q,
        combine: false,
        unsharp_blur: false,
        program: None,
        codes: Vec::new(),
        save: false,
        restore: false,
        temporal: None,
        user: None,
        graph: None,
        matte: None,
    }
}

/// Box passes along x (vertical = false) or y for the radii (radius 0 is a no-op on the CPU).
fn boxes(radii: &[u32], vertical: bool, repeat: bool, out: &mut Vec<Step>) {
    for &r in radii.iter().filter(|r| **r > 0) {
        let mut s = step(OP_BOX, [r, repeat as u32, vertical as u32, 0], &[]);
        s.run = r > BOX_PER_PIXEL_MAX;
        out.push(s);
    }
}

/// Active masks as `count, (mode, inverted, feather, expansion, opacity, n, x, y…)…`.
fn mask_program(masks: &[filmcraft_render::mask::FlatMask]) -> Vec<f32> {
    let active: Vec<_> = masks.iter().filter(|m| m.mode.index() != 0).collect();
    let mut p = vec![active.len() as f32];
    for m in active {
        p.extend([m.mode.index() as f32, f32::from(u8::from(m.inverted)), m.feather, m.expansion, m.opacity, m.pts.len() as f32]);
        p.extend(m.pts.iter().flatten());
    }
    p
}

/// The passes of an op (empty: no-op). Unsharp's blur passes come first, then the combine.
fn steps(op: &FxOp) -> Vec<Step> {
    let mut out = Vec::new();
    match op {
        FxOp::BrightnessContrast { br, co } => out.push(step(OP_BRIGHTNESS_CONTRAST, [0; 4], &[*br, *co])),
        FxOp::ProcAmp { br, co, hue, sat } => out.push(step(OP_PROC_AMP, [0; 4], &[*br, *co, *hue, *sat])),
        FxOp::Tint { black, white, amount } => {
            out.push(step(OP_TINT, [0; 4], &[black[0], black[1], black[2], *amount, white[0], white[1], white[2]]));
        }
        FxOp::BlackWhite => out.push(step(OP_BLACK_WHITE, [0; 4], &[])),
        FxOp::ColorBalance { sh, md, hi, preserve } => {
            out.push(step(OP_COLOR_BALANCE, [*preserve as u32, 0, 0, 0], &[sh[0], sh[1], sh[2], 0.0, md[0], md[1], md[2], 0.0, hi[0], hi[1], hi[2]]))
        }
        FxOp::LeaveColor { amount, key_hue, tol, soft } => out.push(step(OP_LEAVE_COLOR, [0; 4], &[*amount, *key_hue, *tol, *soft])),
        FxOp::ChangeToColor { from_hue, to_hue, tol, soft } => out.push(step(OP_CHANGE_TO_COLOR, [0; 4], &[*from_hue, *to_hue, *tol, *soft])),
        FxOp::ColorPass { key, sim, reverse } => out.push(step(OP_COLOR_PASS, [*reverse as u32, 0, 0, 0], &[key[0], key[1], key[2], *sim])),
        FxOp::Gamma { g } => out.push(step(OP_GAMMA, [0; 4], &[*g])),
        FxOp::Levels { ib, iw, ob, ow, g } => out.push(step(OP_LEVELS, [0; 4], &[*ib, *iw, *ob, *ow, *g])),
        FxOp::Extract { lo, hi, soft, invert } => out.push(step(OP_EXTRACT, [*invert as u32, 0, 0, 0], &[*lo, *hi, *soft])),
        FxOp::Invert { channel: 4, blend } => out.push(step(OP_INVERT_ALPHA, [0; 4], &[*blend])),
        FxOp::Invert { channel, blend } => out.push(step(OP_INVERT, [*channel, 0, 0, 0], &[*blend])),
        FxOp::Posterize { n } => out.push(step(OP_POSTERIZE, [0; 4], &[*n])),
        FxOp::Gaussian { rx, ry, repeat } => {
            boxes(rx, false, *repeat, &mut out);
            boxes(ry, true, *repeat, &mut out);
        }
        FxOp::DirectionalBlur { dx, dy, steps } => {
            if *steps >= 2 {
                out.push(step(OP_DIRECTIONAL, [*steps, 0, 0, 0], &[*dx, *dy]));
            }
        }
        FxOp::Unsharp { rx, ry, amount, threshold } => {
            boxes(rx, false, true, &mut out);
            boxes(ry, true, true, &mut out);
            for s in &mut out {
                s.unsharp_blur = true;
            }
            let mut s = step(OP_UNSHARP, [0; 4], &[*amount, *threshold]);
            s.combine = true;
            out.push(s);
        }
        FxOp::Crop { x0, x1, y0, y1, feather } => out.push(step(OP_CROP, [0; 4], &[*x0, *x1, *y0, *y1, *feather])),
        FxOp::Resample(r) => {
            let op = if (r.opacity - 1.0).abs() < 1e-6 { 1.0 } else { r.opacity };
            match &r.inv {
                Some(m) => out.push(step(OP_RESAMPLE, r.rect, &[m.a as f32, m.b as f32, m.c as f32, m.d as f32, m.e as f32, m.f as f32, op, 1.0])),
                None => out.push(step(OP_RESAMPLE, [0; 4], &[0.0; 8])),
            }
        }
        FxOp::HFlip => out.push(step(OP_HFLIP, [0; 4], &[])),
        FxOp::VFlip => out.push(step(OP_VFLIP, [0; 4], &[])),
        FxOp::Mirror { cx, cy, nx, ny } => out.push(step(OP_MIRROR, [0; 4], &[*cx, *cy, *nx, *ny])),
        FxOp::Offset { dx, dy, blend } => out.push(step(OP_OFFSET, [0; 4], &[*dx, *dy, *blend])),
        FxOp::AscCdl { slope: s, offset: o, power: p, sat } => {
            out.push(step(OP_ASC_CDL, [0; 4], &[s[0], s[1], s[2], *sat, o[0], o[1], o[2], 0.0, p[0], p[1], p[2]]));
        }
        FxOp::ChannelMix { m } => {
            let p: Vec<f32> = m.iter().flatten().copied().collect();
            out.push(step(OP_CHANNEL_MIX, [0; 4], &p));
        }
        FxOp::ColorReplace { sim, solid, target: t, replace: r, replace_hsl: h } => {
            out.push(step(OP_COLOR_REPLACE, [*solid as u32, 0, 0, 0], &[t[0], t[1], t[2], *sim, r[0], r[1], r[2], 0.0, h[0], h[1], h[2]]))
        }
        FxOp::AlphaAdjust { opacity, ignore, invert, mask_only } => {
            out.push(step(OP_ALPHA_ADJUST, [*ignore as u32, *invert as u32, *mask_only as u32, 0], &[*opacity]));
        }
        FxOp::LumaKey { th, cut } => out.push(step(OP_LUMA_KEY, [0; 4], &[*th, *cut])),
        FxOp::ColorKey { k, tol, soft } => out.push(step(OP_COLOR_KEY, [0; 4], &[k[0], k[1], k[2], *tol, *soft])),
        FxOp::UltraKey(k) => {
            let [cc_s, cc_h, cc_l] = k.cc.unwrap_or([1.0, 0.0, 1.0]);
            #[rustfmt::skip]
            let p = [
                k.k[0], k.k[1], k.k[2], k.kmag2,
                k.tol, k.gain, k.highlight, k.shadow,
                k.pedestal, k.choke, k.contrast, k.mid,
                k.spill, k.desat, k.range, k.sluma,
                cc_s, cc_h, cc_l,
            ];
            let choke = k.choke > 0.0;
            let blurred = choke || !k.soften_rx.is_empty() || !k.soften_ry.is_empty();
            let i1 = [k.dom.min(2), k.output, k.cc.is_some() as u32, blurred as u32];
            if blurred {
                // the matte (red) through choke and soften; the original stays for the combine
                out.push(step(OP_UKEY_MATTE, i1, &p));
                if choke {
                    boxes(&k.choke_rx, false, false, &mut out);
                    boxes(&k.choke_ry, true, false, &mut out);
                    out.push(step(OP_UKEY_CHOKE, i1, &p));
                }
                boxes(&k.soften_rx, false, false, &mut out);
                boxes(&k.soften_ry, true, false, &mut out);
                for s in &mut out {
                    s.unsharp_blur = true;
                }
            }
            let mut s = step(OP_UKEY, i1, &p);
            s.combine = true;
            out.push(s);
        }
        // (no active mask: the effect applies everywhere, as `mask::coverage` → None)
        FxOp::Masked { ops, masks } if mask_program(masks).len() <= 1 => out.extend(ops.iter().flat_map(steps)),
        FxOp::OpacityMask { masks } if mask_program(masks).len() <= 1 => {}
        FxOp::Masked { ops, masks } => {
            let mut save = step(0, [0; 4], &[]);
            save.save = true;
            out.push(save);
            out.extend(ops.iter().flat_map(steps));
            let prog = mask_program(masks);
            let mut mix = step(MASK_MIX, [prog.len() as u32, 0, 0, 0], &[]);
            mix.program = Some(prog);
            mix.restore = true;
            out.push(mix);
        }
        // (an invalid matte covers nothing: identity, as on the CPU)
        FxOp::MaskedBy { matte, .. } if !matte.is_valid() => {}
        FxOp::MaskedBy { ops, matte } => {
            let mut save = step(0, [0; 4], &[]);
            save.save = true;
            out.push(save);
            out.extend(ops.iter().flat_map(steps));
            let mut mix = step(MATTE_MIX, [0; 4], &[]);
            mix.matte = Some(Arc::clone(matte));
            mix.restore = true;
            out.push(mix);
        }
        FxOp::OpacityMask { masks } => {
            let prog = mask_program(masks);
            let mut s = step(MASK_SCALE, [prog.len() as u32, 0, 0, 0], &[]);
            s.program = Some(prog);
            out.push(s);
        }
        FxOp::HslSecondary { key, median_r, denoise, blur, correct } => {
            let (Some((kp, kc)), Some((cp, cc))) = (crate::colorops::program_and_codes(key), crate::colorops::program_and_codes(correct)) else {
                log::warn!("HSL Secondary chains too large for the GPU program; skipped");
                return out;
            };
            let mut k = step(CHAIN_KEY, [kp.len() as u32, 0, 0, 0], &[]);
            k.program = Some(kp);
            k.codes = kc;
            out.push(k);
            if *denoise > 0.0 && *median_r > 0 {
                out.push(step(OP_MEDIAN_MASK, [*median_r, 0, 0, 0], &[*denoise]));
            }
            boxes(blur, false, true, &mut out);
            boxes(blur, true, true, &mut out);
            for s in &mut out {
                s.unsharp_blur = true;
            }
            let mut c = step(CHAIN_MASKED, [cp.len() as u32, 0, 0, 0], &[]);
            c.program = Some(cp);
            c.codes = cc;
            c.combine = true;
            out.push(c);
        }
        FxOp::ColorChain(ops) => match crate::colorops::program_and_codes(ops) {
            Some((p, codes)) if !p.is_empty() => {
                let mut s = step(0, [p.len() as u32, 0, 0, 0], &[]);
                s.program = Some(p);
                s.codes = codes;
                out.push(s);
            }
            Some(_) => {}
            None => log::warn!("colour chain of {} ops too large for the GPU program; skipped", ops.len()),
        },
        FxOp::Vignette { amount, midpoint, roundness, feather, target } => {
            if amount.abs() >= 1e-5 {
                out.push(step(OP_VIGNETTE, [0; 4], &[*amount, *midpoint, *roundness, *feather, target[0], target[1], target[2]]));
            }
        }
        FxOp::VideoLimiter { max, comp, axis, warn, warning_color } => {
            out.push(step(OP_VIDEO_LIMITER, [*axis, *warn as u32, 0, 0], &[*max, *comp, warning_color[0], warning_color[1], warning_color[2]]));
        }
        FxOp::Denoise { luma, chroma, sigma } => {
            // (both strengths 0: an exact no-op, no pass; the constants are the CPU's.) A pre-pass
            // converts every pixel once to (Y, Cb, Cr, α); the filter reads it and keeps the
            // original (`aux`) for the pixels it leaves unchanged, like Unsharp's combine.
            if let Some(p) = filmcraft_render::denoise::params(*luma, *chroma, *sigma) {
                let mut pre = step(OP_DENOISE_YCC, [0; 4], &[]);
                pre.unsharp_blur = true;
                out.push(pre);
                let flags = [p.radius, u32::from(p.luma_k.is_some()), u32::from(p.chroma_k.is_some()), 0];
                let mut q = vec![p.luma_k.unwrap_or(0.0), p.chroma_k.unwrap_or(0.0), 0.0, 0.0];
                q.extend_from_slice(&p.spatial);
                let mut f = step(OP_DENOISE, flags, &q);
                f.combine = true;
                out.push(f);
            }
        }
        // (built by `FxStage::job`, which has the neighbours' bind groups)
        FxOp::TemporalDenoise { .. } => {}
        // (its passes on the job's textures: `graph.rs`)
        FxOp::GradeGraph(p) => {
            let mut s = step(0, [0; 4], &[]);
            s.graph = Some(Arc::clone(p));
            out.push(s);
        }
        FxOp::User(u) => {
            let mut s = step(0, [0; 4], &[]);
            s.user = Some(u.clone());
            out.push(s);
        }
    }
    out
}

/// Default GPU memory the effect stage's working textures of one layer may take (bytes): above
/// it, temporal noise reduction degrades to ±1 frame, then is skipped. 4 GiB fits ±2 at 6.2K
/// (8 textures of 413 MB). Override with the `FILMCRAFT_FX_VRAM_MB` environment variable or
/// `GpuCompositor::set_fx_vram_budget`.
pub const FX_VRAM_BUDGET: u64 = 4 << 30;

/// [`FX_VRAM_BUDGET`], or `FILMCRAFT_FX_VRAM_MB` (megabytes) when set to a number.
pub fn fx_vram_budget_default() -> u64 {
    std::env::var("FILMCRAFT_FX_VRAM_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()).map_or(FX_VRAM_BUDGET, |mb| mb.saturating_mul(1 << 20))
}

/// The main passes of `fx` and its temporal ops, with temporal radii capped at `cap` (0: the
/// temporal ops are left out). `neighbours[j]` is aligned with `tnr::select` at the op's own
/// radius; capping drops the farther neighbours from that list.
fn plan_steps(fx: &LayerFx, neighbours: &[Vec<Option<wgpu::BindGroup>>], max: u32, cap: u8, fuse: bool) -> (Vec<Step>, Vec<TemporalSpec>) {
    let lower = |op: &FxOp| if fuse { fused_steps(op) } else { steps(op) };
    let mut all: Vec<Step> = Vec::new();
    let mut specs: Vec<TemporalSpec> = Vec::new();
    for (j, op) in fx.ops.iter().enumerate() {
        let FxOp::TemporalDenoise { luma, chroma, radius, motion, inputs } = op else {
            all.extend(lower(op));
            continue;
        };
        let (Some(inp), Some(params)) = (inputs, filmcraft_render::tnr::params(*luma, *chroma, *motion)) else { continue };
        let bgs = neighbours.get(j);
        let nbrs: Vec<_> = filmcraft_render::tnr::select(inp, *radius)
            .iter()
            .enumerate()
            .filter_map(|(slot, &i)| {
                let n = inp.neighbours.get(i)?;
                let bg = bgs?.get(slot)?.as_ref()?;
                if n.offset.unsigned_abs() > cap {
                    return None;
                }
                let (f, b) = (n.flow.sanitized(), n.back.sanitized());
                (f.w as u32 <= max && f.h as u32 <= max).then(|| (bg.clone(), f, b))
            })
            .take(TEMPORAL_SLOTS)
            .collect();
        if nbrs.is_empty() {
            continue;
        }
        let pre: Vec<Step> = fx.ops.get(..j).unwrap_or_default().iter().filter(|o| !matches!(o, FxOp::TemporalDenoise { .. })).flat_map(lower).collect();
        let pre = if fuse { merge_chains(pre) } else { pre };
        specs.push(TemporalSpec { nbrs, pre, params });
        let mut init = step(OP_TEMPORAL_INIT, [0; 4], &[]);
        init.unsharp_blur = true;
        all.push(init);
        let mut acc = step(0, [0; 4], &[]);
        acc.combine = true;
        acc.temporal = Some(specs.len() - 1);
        all.push(acc);
    }
    (if fuse { merge_chains(all) } else { all }, specs)
}

/// Working textures a list of passes needs (ping-pong, Unsharp's original, a masked save).
fn textures_for(steps: &[Step]) -> usize {
    2 + usize::from(steps.iter().any(|s| s.combine)) + usize::from(steps.iter().any(|s| s.save))
}

impl FxStage {
    /// Whether `device` can run the stage: compute shaders with 16×16 workgroups and a storage
    /// texture (WebGL2-class devices can't; they render effects on the CPU).
    pub(crate) fn supported(device: &wgpu::Device) -> bool {
        let l = device.limits();
        l.max_storage_textures_per_shader_stage >= 1
            && l.max_compute_invocations_per_workgroup >= 256
            && l.max_compute_workgroup_size_x >= 64
            && l.max_compute_workgroup_size_y >= 16
            && l.max_compute_workgroups_per_dimension >= 1
    }

    pub(crate) fn with_format(device: &wgpu::Device, composite: &wgpu::ShaderModule, layer_bgl: &wgpu::BindGroupLayout, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-fx"),
            source: wgpu::ShaderSource::Wgsl(wgsl_for(include_str!("fx.wgsl"), format).into()),
        });
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
            label: Some("fx"),
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
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                tex(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("fx"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let ubgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx-user"),
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
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let upl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("fx-user"), bind_group_layouts: &[Some(&ubgl)], immediate_size: 0 });
        let chain_src = wgsl_for(include_str!("colorops.wgsl"), format);
        let chain_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-colorops"),
            source: wgpu::ShaderSource::Wgsl(chain_src.as_str().into()),
        });
        let compute_in = |module: &wgpu::ShaderModule, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("fx"),
                layout: Some(&pl),
                module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let compute = |entry: &str| compute_in(&shader, entry);
        let no_program = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-no-program"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let spl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx-source"),
            bind_group_layouts: &[Some(layer_bgl)],
            immediate_size: 0,
        });
        let source = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fx-source"),
            layout: Some(&spl),
            vertex: wgpu::VertexState { module: composite, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: composite,
                entry_point: Some("fs_fx_source"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        // the temporal pass: group 1 = its constants, the neighbours' (Y, Cb, Cr, α) and their
        // forward / backward flows (`Rg32Float`, read with textureLoad)
        let mut tentries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        }];
        tentries.extend((1..=(3 * TEMPORAL_SLOTS) as u32).map(tex));
        let tbgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fx-temporal"), entries: &tentries });
        let tpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx-temporal"),
            bind_group_layouts: &[Some(&bgl), Some(&tbgl)],
            immediate_size: 0,
        });
        let temporal = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fx-temporal"),
            layout: Some(&tpl),
            module: &shader,
            entry_point: Some("temporal_accumulate"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mbgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fx-matte"), entries: &[tex(MATTE_BINDING)] });
        let mpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx-matte"),
            bind_group_layouts: &[Some(&bgl), Some(&mbgl)],
            immediate_size: 0,
        });
        let matte = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fx-matte"),
            layout: Some(&mpl),
            module: &shader,
            entry_point: Some("matte_mix"),
            compilation_options: Default::default(),
            cache: None,
        });
        let l = device.limits();
        let box3 = (l.max_compute_workgroup_storage_size >= BOX3_SHARED_BYTES && l.max_compute_workgroup_size_x >= BOX3_TILE).then(|| compute("fx_box3"));
        Self {
            px: compute("fx_px"),
            run: compute("fx_run"),
            box3,
            chain: compute_in(&chain_shader, "chain_px"),
            chain_key: compute_in(&chain_shader, "chain_key"),
            chain_masked: compute_in(&chain_shader, "chain_masked"),
            mask: compute_in(&chain_shader, "mask_pass"),
            no_program,
            bgl,
            source,
            temporal,
            tbgl,
            matte,
            mbgl,
            pool: HashMap::new(),
            budget: fx_vram_budget_default(),
            used: HashSet::new(),
            format,
            chain_src,
            pl,
            special: Mutex::new(HashMap::new()),
            shader,
            px_special: Mutex::new(HashMap::new()),
            ubgl,
            upl,
            user_pipes: Mutex::new(HashMap::new()),
            watch: Arc::new(Mutex::new(UserWatch::new())),
            timestamps: device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            interpret_only: false,
            fuse: fuse_passes(),
            graph: None,
        }
    }

    /// The working texture format.
    pub(crate) fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Set the working-texture budget (bytes; see [`FX_VRAM_BUDGET`]).
    pub(crate) fn set_budget(&mut self, bytes: u64) {
        self.budget = bytes;
    }

    /// Start a frame: pooled textures of sizes no layer uses by [`end_frame`](Self::end_frame)
    /// are released then.
    pub(crate) fn begin_frame(&mut self) {
        self.used.clear();
    }

    pub(crate) fn end_frame(&mut self) {
        let used = &self.used;
        self.pool.retain(|k, _| used.contains(k));
    }

    /// Run colour chains on the interpreter only (false: specialised shaders where they build).
    #[cfg(test)]
    pub(crate) fn set_interpret_only(&mut self, on: bool) {
        self.interpret_only = on;
    }

    /// Fuse passes (true, the default unless `FILMCRAFT_FX_FUSE=0`) or run them one by one.
    #[cfg(test)]
    pub(crate) fn set_fuse(&mut self, on: bool) {
        self.fuse = on;
    }

    /// Specialised chain pipelines built so far, and builds that failed.
    #[cfg(test)]
    pub(crate) fn specialised_count(&self) -> (usize, usize) {
        let cache = self.special.lock().unwrap_or_else(PoisonError::into_inner);
        let built = cache.values().filter(|p| p.is_some()).count();
        (built, cache.len() - built)
    }

    /// Per-op effect pipelines built so far, and builds that failed.
    #[cfg(test)]
    pub(crate) fn px_specialised_count(&self) -> (usize, usize) {
        let cache = self.px_special.lock().unwrap_or_else(PoisonError::into_inner);
        let built = cache.values().filter(|p| p.is_some()).count();
        (built, cache.len() - built)
    }

    /// No working textures held.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        self.pool.is_empty()
    }

    /// Sizes of working textures held.
    #[cfg(test)]
    pub(crate) fn pooled_sizes(&self) -> usize {
        self.pool.len()
    }

    fn targets(&mut self, device: &wgpu::Device, size: (u32, u32), n: usize) -> Vec<Target> {
        let format = self.format;
        self.used.insert(size);
        let v = self.pool.entry(size).or_default();
        while v.len() < n {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("filmcraft-fx"),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = t.create_view(&Default::default());
            v.push((t, view));
        }
        v.iter().take(n).cloned().collect()
    }

    /// Prepare the passes of `fx`, whose source draw uses `source_bg` (a layer bind group whose
    /// uniforms map the frame onto the working image). `neighbours[j]`: for a temporal op at
    /// `fx.ops[j]`, the source bind groups of its `tnr::select`ed neighbours (same mapping as
    /// `source_bg`; None where a neighbour can't be drawn, which skips it). None when the working
    /// image does not fit in a texture (the layer is then drawn without its effects — never
    /// larger than the source frame, which would not have uploaded either).
    pub(crate) fn job(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        fx: &LayerFx,
        source_bg: wgpu::BindGroup,
        neighbours: &[Vec<Option<wgpu::BindGroup>>],
        dummy: &wgpu::TextureView,
    ) -> Option<FxJob> {
        let (w, h) = (fx.size.0.max(1), fx.size.1.max(1));
        let max = device.limits().max_texture_dimension_2d;
        if w > max || h > max {
            return None;
        }
        // the working textures must fit the budget: temporal ops degrade to ±1, then are skipped
        let px_bytes = u64::from(w).saturating_mul(u64::from(h)).saturating_mul(texel_bytes(self.format));
        let wanted = fx.ops.iter().any(|o| matches!(o, FxOp::TemporalDenoise { inputs: Some(_), .. }));
        let fuse = self.fuse && self.box3.is_some();
        // a grade graph (fork C2) needs its pipeline and textures for its slots
        if graph::has_graph(&fx.ops) && !self.ensure_graph(device) {
            return None;
        }
        let mut chosen = None;
        for cap in [filmcraft_render::tnr::MAX_RADIUS, 1, 0] {
            let (all, specs) = plan_steps(fx, neighbours, max, cap, fuse);
            let n_main = textures_for(&all);
            // per temporal op: a (Y, Cb, Cr, α) texture per neighbour plus scratch for its replay
            let extra = specs.iter().map(|t| t.nbrs.len() + if t.pre.is_empty() { 1 } else { textures_for(&t.pre) }).max().unwrap_or(0);
            let g_extra = graph::textures_needed(&all, fuse).saturating_sub(n_main);
            let bytes = px_bytes.saturating_mul((n_main + extra + g_extra) as u64);
            if cap == 0 && g_extra > 0 && bytes > self.budget {
                // (`graphs_fit` sent such a layer to the CPU; never drawn without its grade)
                return None;
            }
            if bytes <= self.budget || cap == 0 {
                if wanted && cap < filmcraft_render::tnr::MAX_RADIUS {
                    log::warn!(
                        "temporal noise reduction at {w}×{h}: working textures over the {} MB GPU budget; {}",
                        self.budget >> 20,
                        if cap == 0 { "skipped (identity)" } else { "limited to ±1 frame" }
                    );
                }
                chosen = Some((all, specs, n_main, extra, g_extra));
                break;
            }
        }
        let (all, specs, n_main, extra, g_extra) = chosen?;
        let targets = self.targets(device, (w, h), n_main + extra + g_extra);
        let views: Vec<wgpu::TextureView> = targets.iter().map(|t| t.1.clone()).collect();
        let main: Vec<usize> = (0..n_main).collect();
        let spare: Vec<usize> = (n_main..n_main + extra).collect();
        let pool: Vec<usize> = main.iter().copied().chain(n_main + extra..n_main + extra + g_extra).collect();
        let mut work = vec![Work::Source { bg: source_bg, target: views.first()?.clone() }];
        let cx = RouteCx { device, queue, views: &views, size: (w, h), dummy, specs: &specs, spare: &spare, pool: &pool, fuse };
        let cur = self.route(&cx, &all, &main, 0, &mut work)?;
        let timing = self.user_timing(device, queue, &mut work, u64::from(w) * u64::from(h));
        Some(FxJob { work, result: views.get(cur)?.clone(), result_texture: targets.get(cur)?.0.clone(), timing })
    }

    /// Give the job's user passes timestamp pairs (None: no user pass, or no timestamp queries).
    fn user_timing(&self, device: &wgpu::Device, queue: &wgpu::Queue, work: &mut [Work], pixels: u64) -> Option<UserTiming> {
        if !self.timestamps {
            return None;
        }
        let mut users = Vec::new();
        for w in work.iter_mut() {
            if let Work::Compute(d) = w
                && let Some(key) = &d.user
                && users.len() < MAX_TIMED_USER_PASSES
            {
                d.slot = Some(users.len() as u32);
                users.push(key.clone());
            }
        }
        if users.is_empty() {
            return None;
        }
        let n = (users.len() * 2) as u32;
        let bytes = u64::from(n) * 8;
        let set = device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("fx-user-time"), ty: wgpu::QueryType::Timestamp, count: n });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-user-time"),
            size: bytes,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-user-time-read"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(UserTiming { set, resolve, read, users, pixels, period: queue.get_timestamp_period() })
    }

    /// After the job's submission: read its user passes' timestamps back (when the device is
    /// next polled) and apply the slow-pass rule.
    pub(crate) fn after_submit(&self, queue: &wgpu::Queue, job: &FxJob) {
        // in flight until the GPU finishes (a device loss meanwhile quarantines them)
        let keys: Vec<(String, u64)> = job.work.iter().filter_map(|w| if let Work::Compute(d) = w { d.user.clone() } else { None }).collect();
        if !keys.is_empty() {
            crate::user_in_flight(&keys, true);
            queue.on_submitted_work_done(move || crate::user_in_flight(&keys, false));
        }
        let Some(t) = &job.timing else { return };
        let (watch, read, users, pixels, period) = (self.watch.clone(), t.read.clone(), t.users.clone(), t.pixels, t.period);
        t.read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            if r.is_err() {
                return;
            }
            let ticks: Vec<u64> = match read.slice(..).get_mapped_range() {
                Ok(v) => v.as_chunks::<8>().0.iter().map(|b| u64::from_le_bytes(*b)).collect(),
                Err(_) => return,
            };
            read.unmap();
            let mut w = watch.lock().unwrap_or_else(PoisonError::into_inner);
            for (key, pair) in users.iter().zip(ticks.as_chunks::<2>().0) {
                let ms = pair[1].saturating_sub(pair[0]) as f64 * f64::from(period) / 1e6;
                w.observe(key, ms, pixels);
            }
        });
    }

    /// Messages about user effects turned off by the slow-pass rule since the last call.
    pub(crate) fn take_user_messages(&self) -> Vec<String> {
        std::mem::take(&mut self.watch.lock().unwrap_or_else(PoisonError::into_inner).messages)
    }

    /// The slow-pass limit in ms at 1080p ([`USER_SLOW_MS_1080P`]; tests lower it).
    pub(crate) fn set_user_slow_limit(&self, ms_1080p: f64) {
        self.watch.lock().unwrap_or_else(PoisonError::into_inner).limit_ms_1080p = ms_1080p;
    }

    /// Route `steps` through the working textures `slots` (indices into `cx.views`) starting
    /// from the image in `start`; the passes are appended to `work`. The index holding the
    /// result.
    fn route(&self, cx: &RouteCx<'_>, steps: &[Step], slots: &[usize], start: usize, work: &mut Vec<Work>) -> Option<usize> {
        let mut cur = start;
        // Unsharp: the image its blur started from (kept while its blur passes run)
        let mut orig: Option<usize> = None;
        // a masked effect: the image before it (kept until its mask mix)
        let mut saved: Option<usize> = None;
        for s in steps {
            if s.save {
                saved = Some(cur);
                continue;
            }
            if let Some(p) = &s.graph {
                // (the image a masked effect or a combine keeps stays untouched)
                let keep: Vec<usize> = [saved, orig].into_iter().flatten().collect();
                cur = self.route_graph(cx, p, cur, &keep, work)?;
                continue;
            }
            // a user effect whose module doesn't build is skipped (the image passes through)
            let user = match &s.user {
                Some(u) if self.watch.lock().unwrap_or_else(PoisonError::into_inner).disabled.contains(&(u.id.clone(), u.hash)) => continue,
                Some(u) => match self.user_pipeline(cx.device, u) {
                    Some(p) => Some((u, p)),
                    None => continue,
                },
                None => None,
            };
            // (an Unsharp without blur passes, radius 0, combines the original with itself)
            if (s.unsharp_blur || s.combine) && orig.is_none() {
                orig = Some(cur);
            }
            let keep = if s.unsharp_blur || s.combine { orig } else { None };
            let dst = slots.iter().copied().find(|k| *k != cur && Some(*k) != keep && Some(*k) != saved)?;
            // (a pass before a combine reads the original too: Ultra Key's choke)
            let aux = match (s.combine || s.unsharp_blur, orig, s.restore, saved) {
                (true, Some(o), _, _) => cx.views.get(o)?,
                (_, _, true, Some(v)) => cx.views.get(v)?,
                _ => cx.dummy,
            };
            let (src_v, dst_v) = (cx.views.get(cur)?, cx.views.get(dst)?);
            match s.temporal {
                Some(t) => {
                    let spec = cx.specs.get(t)?;
                    let nbr = self.temporal_neighbours(cx, spec, work)?;
                    work.push(Work::Compute(self.temporal_dispatch(cx, spec, src_v, dst_v, aux, &nbr)?));
                }
                None => work.push(Work::Compute(match (user, &s.matte) {
                    (Some((u, p)), _) => self.user_dispatch(cx, u, p, src_v, dst_v),
                    (None, Some(m)) => self.matte_dispatch(cx, m, src_v, dst_v, aux)?,
                    (None, None) => self.dispatch(cx, s, src_v, dst_v, aux),
                })),
            }
            cur = dst;
            if s.combine {
                orig = None;
            }
            if s.restore {
                saved = None;
            }
        }
        Some(cur)
    }

    /// One pass of `s` from `src` into `dst` (`aux`: the image a combine or mask mix reads).
    fn dispatch(&self, cx: &RouteCx<'_>, s: &Step, src: &wgpu::TextureView, dst: &wgpu::TextureView, aux: &wgpu::TextureView) -> Dispatch {
        let (device, queue, (w, h)) = (cx.device, cx.queue, cx.size);
        let mut bytes = Vec::with_capacity(128);
        for v in [s.code, w, h, 0].iter().chain(&s.i1) {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in &s.p {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-u"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buf, 0, &bytes);
        let program = s.program.as_ref().map(|p| {
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fx-program"),
                size: (p.len().max(4) * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bytes: Vec<u8> = p.iter().flat_map(|v| v.to_le_bytes()).collect();
            queue.write_buffer(&b, 0, &bytes);
            b
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(dst) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(aux) },
                wgpu::BindGroupEntry { binding: 4, resource: program.as_ref().unwrap_or(&self.no_program).as_entire_binding() },
            ],
        });
        let groups = if s.run {
            let lines = if s.i1[2] != 0 { w } else { h };
            (lines.div_ceil(64), 1)
        } else if s.code == OP_BOX3 {
            let (n, lines) = if s.i1[2] != 0 { (h, w) } else { (w, h) };
            (n.div_ceil(BOX3_TILE), lines)
        } else {
            (w.div_ceil(16), h.div_ceil(16))
        };
        let pipe = if s.program.is_some() {
            match s.code {
                CHAIN_KEY => Pipe::ChainKey,
                CHAIN_MASKED => Pipe::ChainMasked,
                MASK_MIX | MASK_SCALE => Pipe::Mask,
                _ => Pipe::Chain,
            }
        } else if s.run {
            Pipe::Run
        } else if s.code == OP_BOX3 {
            Pipe::Box3
        } else {
            Pipe::Px
        };
        let special = match pipe {
            Pipe::Chain | Pipe::ChainKey | Pipe::ChainMasked => self.special_pipeline(device, pipe, &s.codes),
            Pipe::Px => self.px_pipeline(device, s.code),
            _ => None,
        };
        Dispatch { pipe, bg, bg1: None, groups, special, user: None, slot: None }
    }

    /// A user effect's pass from `src` into `dst`: uniform `size` (w, h, time, 0) and one `vec4`
    /// per parameter (`filmcraft_usershader::compose`'s `FxUniform`, 272 bytes).
    fn user_dispatch(
        &self,
        cx: &RouteCx<'_>,
        u: &filmcraft_render::gpufx::UserOp,
        p: wgpu::ComputePipeline,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
    ) -> Dispatch {
        let (device, queue, (w, h)) = (cx.device, cx.queue, cx.size);
        let mut v: Vec<f32> = vec![w as f32, h as f32, u.time, 0.0];
        for k in 0..filmcraft_render::gpufx::USER_PARAM_SLOTS {
            v.extend(u.pv.get(k).copied().unwrap_or([0.0; 4]));
        }
        let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-user-u"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buf, 0, &bytes);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx-user"),
            layout: &self.ubgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(dst) },
            ],
        });
        Dispatch { pipe: Pipe::User, bg, bg1: None, groups: (w.div_ceil(16), h.div_ceil(16)), special: Some(p), user: Some((u.id.clone(), u.hash)), slot: None }
    }

    /// The pipeline of user effect `u`, built on first use and cached by (id, hash). None: the
    /// module failed to build (logged once; the pass is skipped).
    fn user_pipeline(&self, device: &wgpu::Device, u: &filmcraft_render::gpufx::UserOp) -> Option<wgpu::ComputePipeline> {
        let key = (u.id.clone(), u.hash);
        let mut cache = self.user_pipes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = cache.get(&key) {
            return p.clone();
        }
        let built = self.build_user(device, u);
        if cache.len() >= MAX_SPECIAL_PIPELINES {
            cache.clear();
        }
        cache.insert(key, built.clone());
        built
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn build_user(&self, device: &wgpu::Device, u: &filmcraft_render::gpufx::UserOp) -> Option<wgpu::ComputePipeline> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx-user"),
            source: wgpu::ShaderSource::Wgsl(wgsl_for(&u.module, self.format).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fx-user"),
            layout: Some(&self.upl),
            module: &module,
            entry_point: Some("fx_user_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("user effect {} failed to build on the GPU, skipped: {e}", u.id);
            return None;
        }
        Some(pipeline)
    }

    /// (User effects need the native GPU path.)
    #[cfg(target_arch = "wasm32")]
    fn build_user(&self, _device: &wgpu::Device, _u: &filmcraft_render::gpufx::UserOp) -> Option<wgpu::ComputePipeline> {
        None
    }

    /// The chain pipeline for `pipe` specialised to `codes`, built on first use and cached.
    /// None: specialisation off, no codes, or the build failed (the interpreter runs the chain).
    fn special_pipeline(&self, device: &wgpu::Device, pipe: Pipe, codes: &[u32]) -> Option<wgpu::ComputePipeline> {
        if codes.is_empty() || codes.len() > crate::colorops::MAX_SPECIALISED_OPS || self.interpret_only || !specialise_chains() {
            return None;
        }
        let key = (pipe, codes.to_vec());
        let mut cache = self.special.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = cache.get(&key) {
            return p.clone();
        }
        let built = self.build_special(device, pipe, codes);
        if cache.len() >= MAX_SPECIAL_PIPELINES {
            cache.clear();
        }
        cache.insert(key, built.clone());
        built
    }

    /// The `fx_px` pipeline built for op `code` alone, built on first use and cached. None:
    /// specialisation off, code 0, or the build failed (the generic pipeline runs the pass).
    fn px_pipeline(&self, device: &wgpu::Device, code: u32) -> Option<wgpu::ComputePipeline> {
        if code == 0 || self.interpret_only || !specialise_chains() {
            return None;
        }
        let mut cache = self.px_special.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = cache.get(&code) {
            return p.clone();
        }
        let built = self.build_px(device, code);
        if cache.len() >= MAX_SPECIAL_PIPELINES {
            cache.clear();
        }
        cache.insert(code, built.clone());
        built
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn build_px(&self, device: &wgpu::Device, code: u32) -> Option<wgpu::ComputePipeline> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fx-px-specialised"),
            layout: Some(&self.pl),
            module: &self.shader,
            entry_point: Some("fx_px"),
            compilation_options: wgpu::PipelineCompilationOptions { constants: &[("PX_OP", f64::from(code))], ..Default::default() },
            cache: None,
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("effect pass {code} failed to build alone, using the generic pass: {e}");
            return None;
        }
        Some(pipeline)
    }

    #[cfg(target_arch = "wasm32")]
    fn build_px(&self, _device: &wgpu::Device, _code: u32) -> Option<wgpu::ComputePipeline> {
        None
    }

    fn build_special(&self, device: &wgpu::Device, pipe: Pipe, codes: &[u32]) -> Option<wgpu::ComputePipeline> {
        let entry = match pipe {
            Pipe::ChainKey => "chain_key",
            Pipe::ChainMasked => "chain_masked",
            _ => "chain_px",
        };
        crate::colorops::build_specialised(device, &self.pl, &self.chain_src, codes, entry)
    }

    /// Draw each neighbour of `spec` into scratch, replay the passes before the temporal op on
    /// it and convert it to (Y, Cb, Cr, α) into its own texture. Those textures' indices.
    fn temporal_neighbours(&self, cx: &RouteCx<'_>, spec: &TemporalSpec, work: &mut Vec<Work>) -> Option<Vec<usize>> {
        let k = spec.nbrs.len();
        let (slots, scratch) = (cx.spare.get(..k)?, cx.spare.get(k..)?);
        let first = *scratch.first()?;
        for ((bg, _, _), &slot) in spec.nbrs.iter().zip(slots) {
            work.push(Work::Source { bg: bg.clone(), target: cx.views.get(first)?.clone() });
            let end = self.route(cx, &spec.pre, scratch, first, work)?;
            let init = step(OP_TEMPORAL_INIT, [0; 4], &[]);
            work.push(Work::Compute(self.dispatch(cx, &init, cx.views.get(end)?, cx.views.get(slot)?, cx.dummy)));
        }
        Some(slots.to_vec())
    }

    /// The temporal blend: `src` = the current image as (Y, Cb, Cr, α), `aux` = the original,
    /// `nbr` = the neighbours' converted textures (indices into `cx.views`).
    fn temporal_dispatch(
        &self,
        cx: &RouteCx<'_>,
        spec: &TemporalSpec,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        aux: &wgpu::TextureView,
        nbr: &[usize],
    ) -> Option<Dispatch> {
        let (device, queue) = (cx.device, cx.queue);
        let base = self.dispatch(cx, &step(0, [0; 4], &[]), src, dst, aux);
        let p = &spec.params;
        // TU: n = (count, luma on, chroma on, 0), k = (luma k, chroma k, agree k, 0),
        // gi[4] = (grid w, grid h, 0, 0), gs[4] = (scale x, scale y, 0, 0)
        let mut bytes: Vec<u8> = Vec::with_capacity(160);
        for v in [spec.nbrs.len() as u32, u32::from(p.luma_k.is_some()), u32::from(p.chroma_k.is_some()), 0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in [p.luma_k.unwrap_or(0.0), p.chroma_k.unwrap_or(0.0), p.agree_k, 0.0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for i in 0..TEMPORAL_SLOTS {
            let (gw, gh) = spec.nbrs.get(i).map_or((1, 1), |n| (n.1.w as u32, n.1.h as u32));
            for v in [gw, gh, 0, 0] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        for i in 0..TEMPORAL_SLOTS {
            let [sx, sy] = spec.nbrs.get(i).map_or([1.0; 2], |n| n.1.scale);
            for v in [sx, sy, 0.0, 0.0] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-temporal-u"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buf, 0, &bytes);
        let flow_view = |f: &filmcraft_render::flow::FlowField| {
            let (fw, fh) = (f.w.max(1) as u32, f.h.max(1) as u32);
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fx-flow"),
                size: wgpu::Extent3d { width: fw, height: fh, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rg32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let data: Vec<u8> = f.uv.iter().flat_map(|v| v.iter().flat_map(|c| c.to_le_bytes())).collect();
            if data.len() == (fw as usize) * (fh as usize) * 8 {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    &data,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(fw * 8), rows_per_image: Some(fh) },
                    wgpu::Extent3d { width: fw, height: fh, depth_or_array_layers: 1 },
                );
            }
            t.create_view(&Default::default())
        };
        let mut views: Vec<wgpu::TextureView> = Vec::with_capacity(3 * TEMPORAL_SLOTS);
        for i in 0..TEMPORAL_SLOTS {
            views.push(match nbr.get(i) {
                Some(&v) => cx.views.get(v)?.clone(),
                None => cx.dummy.clone(),
            });
        }
        for i in 0..TEMPORAL_SLOTS {
            match spec.nbrs.get(i) {
                Some((_, f, b)) => {
                    views.push(flow_view(f));
                    views.push(flow_view(b));
                }
                None => {
                    views.push(cx.dummy.clone());
                    views.push(cx.dummy.clone());
                }
            }
        }
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }];
        entries.extend(views.iter().enumerate().map(|(i, v)| wgpu::BindGroupEntry { binding: i as u32 + 1, resource: wgpu::BindingResource::TextureView(v) }));
        let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("fx-temporal"), layout: &self.tbgl, entries: &entries });
        Some(Dispatch { pipe: Pipe::Temporal, bg1: Some(bg1), ..base })
    }

    /// The bitmap matte mix: `aux` (the saved image) mixed with `src` (the effected one) by the
    /// matte's bilinear coverage. None: the matte is invalid or larger than the device's textures.
    fn matte_dispatch(
        &self,
        cx: &RouteCx<'_>,
        m: &filmcraft_render::matte::BitmapMatte,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        aux: &wgpu::TextureView,
    ) -> Option<Dispatch> {
        let (device, queue) = (cx.device, cx.queue);
        let max = device.limits().max_texture_dimension_2d;
        if !m.is_valid() || m.width > max || m.height > max {
            return None;
        }
        let base = self.dispatch(cx, &step(0, [0; 4], &[]), src, dst, aux);
        let size = wgpu::Extent3d { width: m.width, height: m.height, depth_or_array_layers: 1 };
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fx-matte"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &m.alpha,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(m.width), rows_per_image: Some(m.height) },
            size,
        );
        let view = t.create_view(&Default::default());
        let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx-matte"),
            layout: &self.mbgl,
            entries: &[wgpu::BindGroupEntry { binding: MATTE_BINDING, resource: wgpu::BindingResource::TextureView(&view) }],
        });
        Some(Dispatch { pipe: Pipe::Matte, bg1: Some(bg1), special: None, ..base })
    }

    /// Record a job (source draws and compute passes, in order) into `enc`.
    pub(crate) fn record(&self, enc: &mut wgpu::CommandEncoder, job: &FxJob) {
        let mut i = 0;
        while let Some(w) = job.work.get(i) {
            match w {
                Work::Source { bg, target } => {
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("fx-source"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    pass.set_pipeline(&self.source);
                    pass.set_bind_group(0, bg, &[]);
                    pass.draw(0..6, 0..1);
                    i += 1;
                }
                Work::Compute(first) => {
                    // a timed user pass runs in a compute pass of its own
                    let timed = |d: &Dispatch| d.slot.zip(job.timing.as_ref());
                    let tw = timed(first).map(|(k, t)| wgpu::ComputePassTimestampWrites {
                        query_set: &t.set,
                        beginning_of_pass_write_index: Some(2 * k),
                        end_of_pass_write_index: Some(2 * k + 1),
                    });
                    let single = tw.is_some();
                    let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("fx"), timestamp_writes: tw });
                    let mut n = 0;
                    while let Some(Work::Compute(d)) = job.work.get(i) {
                        if n > 0 && (single || timed(d).is_some()) {
                            break;
                        }
                        n += 1;
                        pass.set_pipeline(match (&d.special, d.pipe) {
                            (Some(p), _) => p,
                            (None, pipe) => match pipe {
                                Pipe::Px => &self.px,
                                Pipe::Run => &self.run,
                                // (only planned when the pipeline exists)
                                Pipe::Box3 => self.box3.as_ref().unwrap_or(&self.px),
                                Pipe::Chain => &self.chain,
                                Pipe::ChainKey => &self.chain_key,
                                Pipe::ChainMasked => &self.chain_masked,
                                Pipe::Mask => &self.mask,
                                Pipe::Temporal => &self.temporal,
                                Pipe::Matte => &self.matte,
                                // (always have their pipeline in `special`)
                                Pipe::User | Pipe::Graph => &self.px,
                            },
                        });
                        pass.set_bind_group(0, &d.bg, &[]);
                        if let Some(b) = &d.bg1 {
                            pass.set_bind_group(1, b, &[]);
                        }
                        pass.dispatch_workgroups(d.groups.0, d.groups.1, 1);
                        i += 1;
                    }
                }
            }
        }
        if let Some(t) = &job.timing {
            let n = (t.users.len() * 2) as u32;
            enc.resolve_query_set(&t.set, 0..n, &t.resolve, 0);
            enc.copy_buffer_to_buffer(&t.resolve, 0, &t.read, 0, u64::from(n) * 8);
        }
    }
}

/// What routing passes needs besides the steps.
struct RouteCx<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    views: &'a [wgpu::TextureView],
    size: (u32, u32),
    dummy: &'a wgpu::TextureView,
    specs: &'a [TemporalSpec],
    /// Textures beyond the main chain's (the temporal neighbours' and their scratch).
    spare: &'a [usize],
    /// The textures a grade graph may use (the main chain's and the graph's own).
    pool: &'a [usize],
    /// Whether passes are fused (as the layer's own).
    fuse: bool,
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn the_preview_format_follows_the_device_rule_and_the_override() {
        let integrated = if HALF_ON_INTEGRATED { FX_FORMAT_HALF } else { FX_FORMAT };
        assert_eq!(fx_format_pick(wgpu::DeviceType::IntegratedGpu, None), integrated);
        assert_eq!(fx_format_pick(wgpu::DeviceType::DiscreteGpu, None), FX_FORMAT);
        assert_eq!(fx_format_pick(wgpu::DeviceType::Cpu, None), FX_FORMAT);
        assert_eq!(fx_format_pick(wgpu::DeviceType::DiscreteGpu, Some(" f16 ")), FX_FORMAT_HALF);
        assert_eq!(fx_format_pick(wgpu::DeviceType::IntegratedGpu, Some("f32")), FX_FORMAT);
        // anything else: the device's default
        assert_eq!(fx_format_pick(wgpu::DeviceType::IntegratedGpu, Some("bogus")), integrated);
        assert_eq!(texel_bytes(FX_FORMAT_HALF), 8);
        assert_eq!(texel_bytes(FX_FORMAT), 16);
    }

    #[test]
    fn half_float_shaders_write_half_float_storage() {
        for src in [include_str!("fx.wgsl"), include_str!("colorops.wgsl")] {
            assert!(src.contains("texture_storage_2d<rgba32float, write>"));
            let half = wgsl_for(src, FX_FORMAT_HALF);
            assert!(!half.contains("rgba32float") && half.contains("texture_storage_2d<rgba16float, write>"));
            assert_eq!(wgsl_for(src, FX_FORMAT), src);
        }
    }
}

#[cfg(test)]
mod fuse_tests {
    use super::*;

    fn codes(s: &[Step]) -> Vec<(u32, [u32; 4], bool, bool)> {
        s.iter().map(|s| (s.code, s.i1, s.unsharp_blur, s.combine)).collect()
    }

    #[test]
    fn box_runs_fuse_per_axis_and_edge_mode() {
        // Gaussian: three boxes along x, three along y → one fused pass each
        let g = fuse_boxes(steps(&FxOp::Gaussian { rx: vec![3, 4, 5], ry: vec![2, 0, 7], repeat: true }));
        assert_eq!(codes(&g), vec![(OP_BOX3, [3 | 4 << 8 | 5 << 16, 1, 0, 3], false, false), (OP_BOX3, [2 | 7 << 8, 1, 1, 2], false, false)]);
        // a lone box stays a plain pass; running-sum radii are never fused
        let one = fuse_boxes(steps(&FxOp::Gaussian { rx: vec![6], ry: vec![40, 3, 3], repeat: false }));
        assert_eq!(codes(&one).iter().map(|c| c.0).collect::<Vec<_>>(), vec![OP_BOX, OP_BOX, OP_BOX3]);
        assert!(one.get(1).is_some_and(|s| s.run));
        // Unsharp: the fused blur keeps the original for the combine
        let u = fuse_boxes(steps(&FxOp::Unsharp { rx: vec![1, 1, 2], ry: vec![1, 1, 2], amount: 0.4, threshold: 0.0 }));
        assert_eq!(
            u.iter().map(|s| (s.code, s.unsharp_blur, s.combine)).collect::<Vec<_>>(),
            vec![(OP_BOX3, true, false), (OP_BOX3, true, false), (OP_UNSHARP, false, true)]
        );
        // Sharpen-sized Unsharp: one pass; a cascade or a bigger radius keeps the blur passes
        let one = fused_steps(&FxOp::Unsharp { rx: vec![0, 0, 1], ry: vec![0, 0, 1], amount: 0.4, threshold: 0.0 });
        assert_eq!(codes(&one), vec![(OP_UNSHARP_DIRECT, [1, 1, 0, 0], false, false)]);
        let wide = fused_steps(&FxOp::Unsharp { rx: vec![4], ry: vec![], amount: 0.4, threshold: 0.0 });
        assert_eq!(wide.last().map(|s| s.code), Some(OP_UNSHARP));
        let cascade = fused_steps(&FxOp::Unsharp { rx: vec![1, 1], ry: vec![1], amount: 0.4, threshold: 0.0 });
        assert_eq!(cascade.iter().map(|s| s.code).collect::<Vec<_>>(), vec![OP_BOX3, OP_BOX, OP_UNSHARP]);
        // HSL Secondary without blur / denoise: one plain chain, merged with the chain before it
        use filmcraft_color::ops::ColorOp;
        let clamp: std::sync::Arc<[ColorOp]> = vec![ColorOp::Clamp { lo: 0.0, hi: 1.0 }].into();
        let hsl = |blur: Vec<u32>, median_r: u32| FxOp::HslSecondary { key: clamp.clone(), median_r, denoise: 0.5, blur, correct: clamp.clone() };
        let one_pass = fused_steps(&hsl(vec![0, 0], 0));
        assert_eq!(one_pass.len(), 1);
        assert!(one_pass.first().is_some_and(|s| plain_chain(s) && s.codes.first() == Some(&(crate::colorops::CO_KEY_BEGIN as u32))));
        assert!(fused_steps(&hsl(vec![2], 0)).len() > 1, "a blurred key keeps its passes");
        assert!(fused_steps(&hsl(vec![], 1)).len() > 1, "a denoised key keeps its passes");
        let merged = merge_chains([fused_steps(&FxOp::ColorChain(clamp.clone())), one_pass].into_iter().flatten().collect());
        assert_eq!(merged.len(), 1);
        assert!(merged.first().is_some_and(|s| s.codes.len() == 5 && s.program.as_ref().is_some_and(|p| p.len() as u32 == s.i1[0])));
        // never past the specialisation limit (a longer chain would fall back to the slow interpreter)
        let long: std::sync::Arc<[ColorOp]> = vec![ColorOp::Clamp { lo: 0.0, hi: 1.0 }; 40].into();
        assert_eq!(merge_chains([steps(&FxOp::ColorChain(long.clone())), steps(&FxOp::ColorChain(long))].into_iter().flatten().collect()).len(), 2);
        // hostile radii: a run never packs more than three, nor a radius over the per-pixel limit
        let many = fuse_boxes(steps(&FxOp::Gaussian { rx: vec![32; 7], ry: vec![], repeat: true }));
        assert_eq!(codes(&many).iter().map(|c| (c.0, c.1[3])).collect::<Vec<_>>(), vec![(OP_BOX3, 3), (OP_BOX3, 3), (OP_BOX, 0)]);
        assert!(many.iter().filter(|s| s.code == OP_BOX3).all(|s| (0..3).all(|k| (s.i1[0] >> (8 * k)) & 0xff <= BOX_PER_PIXEL_MAX)));
    }
}
