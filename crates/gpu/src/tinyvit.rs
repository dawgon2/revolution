//! Magic Mask's image encoder on the GPU (fork GRADE-PLAN G3, FORK-PLAN §1a.122): MobileSAM's
//! TinyViT-5M forward pass built from the [`crate::nn`] ops, the counterpart of
//! candle-transformers 0.9's `segment_anything::tiny_vit` that `filmcraft-mask` runs on the CPU
//! (which stays the reference and the fallback).
//!
//! Input: the (1, 3, 1024, 1024) normalised frame; output: the (1, 256, 64, 64) embedding the
//! CPU mask decoder takes. The weights arrive as plain f32 tensors named as in `mobile_sam.pt`
//! without the `image_encoder.` prefix ([`Weight`]); this crate knows nothing of candle.
//!
//! **How it runs.** At [`TinyVitGpu::new`] the whole forward pass is planned once into ~120
//! [`NnStep`]s, the weights are uploaded and stay resident, and eight activation buffers are
//! allocated (sized by the plan, each checked against the device's binding limit) and reused
//! every frame. [`TinyVitGpu::encode`] uploads the frame, records the steps into one command
//! buffer, submits and reads the embedding back.
//! - Every convolution's batch norm is folded into its weights and bias at load
//!   (`w·γ/√(σ²+ε)`, `β − μ·γ/√(σ²+ε)`, ε = 1e-5, as candle's eval-mode `BatchNorm`).
//! - 1 × 1 convolutions and linear layers are the tiled matmul (fused bias, GELU and residual
//!   epilogues); 3 × 3 full convolutions (patch embedding, neck) are im2col + matmul; the
//!   depthwise 3 × 3 convolutions have their own kernel.
//! - GELU is the tanh form, as candle's `Tensor::gelu`. Layer norms are two-pass, ε 1e-5 on
//!   tokens and 1e-6 in the neck's LayerNorm2d.
//! - Windowed attention: the attention norm gathers tokens into zero-padded windows (a padding
//!   token normalises to the norm's bias, as in candle, where the padding is zeros before the
//!   norm); qkv and proj run over the padded rows; the attention op does the softmax with the
//!   relative position bias (expanded at load with candle's index order); the result is added
//!   back onto the tokens without the padding.
//!
//! Every failure (no adapter, a validation or out-of-memory error, a lost device, a timeout,
//! hostile weight shapes) is a [`TinyVitError`], never a panic: callers use the CPU instead.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use wgpu::util::DeviceExt;

use crate::nn::{Add, Attention, Depthwise, Im2col, LayerNorm, Matmul, NnError, NnKernels, NnStep, WindowGrid};

/// Side of the encoder's square input.
pub const INPUT_SIDE: usize = 1024;
/// Values in the encoder's input (3 × 1024 × 1024, CHW).
pub const INPUT_LEN: usize = 3 * INPUT_SIDE * INPUT_SIDE;
/// Values in the embedding (256 × 64 × 64, CHW).
pub const OUTPUT_LEN: usize = 256 * 64 * 64;
/// Longest wait for one frame (a weak integrated GPU takes under a second).
const GPU_WAIT: Duration = Duration::from_secs(20);

const EMBED_DIMS: [usize; 4] = [64, 128, 160, 320];
const DEPTHS: [usize; 4] = [2, 2, 6, 2];
const HEADS: [usize; 4] = [2, 4, 5, 10];
const WINDOWS: [usize; 4] = [7, 7, 14, 7];
const BN_EPS: f64 = 1e-5;
const LN_EPS: f32 = 1e-5;
const LN2D_EPS: f32 = 1e-6;

/// One weight tensor: its shape and row-major values.
#[derive(Clone, Debug, PartialEq)]
pub struct Weight {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

/// Why the GPU encoder could not be built or run.
#[derive(Clone, Debug, PartialEq)]
pub struct TinyVitError(pub String);

impl std::fmt::Display for TinyVitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GPU image encoder: {}", self.0)
    }
}

impl std::error::Error for TinyVitError {}

impl From<NnError> for TinyVitError {
    fn from(e: NnError) -> Self {
        TinyVitError(e.0)
    }
}

fn err(s: impl Into<String>) -> TinyVitError {
    TinyVitError(s.into())
}

type Result<T> = std::result::Result<T, TinyVitError>;

/// The activation buffers the plan uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Inp,
    H,
    H2,
    X,
    X2,
    A,
    B,
    Out,
}

const ACTS: [Act; 8] = [Act::Inp, Act::H, Act::H2, Act::X, Act::X2, Act::A, Act::B, Act::Out];

impl Act {
    fn index(self) -> usize {
        ACTS.iter().position(|a| *a == self).unwrap_or(0)
    }
}

/// A buffer an op binds: an activation or an uploaded weight.
#[derive(Clone, Copy, Debug)]
enum Res {
    Act(Act),
    W(usize),
}

/// One planned op (buffers are bound after the plan has sized them).
enum PlanOp {
    Matmul(Matmul, [Res; 4]),
    Im2col(Im2col, [Res; 2]),
    Depthwise(Depthwise, [Res; 4]),
    Norm(LayerNorm, [Res; 4]),
    Attn(Attention, [Res; 3]),
    Add(Add, [Res; 2]),
}

/// How a stage's output is laid out in its buffer, and how it is reported (candle's layout).
#[derive(Clone, Copy, Debug)]
enum Layout {
    /// HWC, reported as CHW.
    Hwc { h: usize, w: usize, c: usize },
    /// As stored (tokens HWC, or the CHW embedding).
    Flat,
}

/// The end of a named stage: after op `end`, `len` values of `act`.
#[derive(Clone, Debug)]
struct Stage {
    name: &'static str,
    end: usize,
    act: Act,
    len: usize,
    layout: Layout,
}

/// The names of the stages [`TinyVitGpu::encode_stages`] reports (layouts as candle's:
/// `patch_embed` (64, 256, 256); `layer0` 16384 × 128 tokens; `layer1` 4096 × 160; `layer2`,
/// `layer3` 4096 × 320; `neck` (256, 64, 64)).
pub const STAGES: [&str; 6] = ["patch_embed", "layer0", "layer1", "layer2", "layer3", "neck"];

/// Plans the forward pass: the ops, the weights they bind and the buffer sizes they need.
struct Planner<'a> {
    weights: &'a HashMap<String, Weight>,
    uploads: Vec<Vec<f32>>,
    ops: Vec<PlanOp>,
    /// f32 values each activation buffer must hold.
    need: [usize; 8],
    stages: Vec<Stage>,
    zero_bias: Option<usize>,
}

impl<'a> Planner<'a> {
    fn new(weights: &'a HashMap<String, Weight>) -> Self {
        Planner { weights, uploads: Vec::new(), ops: Vec::new(), need: [0; 8], stages: Vec::new(), zero_bias: None }
    }

    fn get(&self, name: &str, shape: &[usize]) -> Result<&'a [f32]> {
        let w = self.weights.get(name).ok_or_else(|| err(format!("weight {name} missing")))?;
        if w.shape != shape {
            return Err(err(format!("weight {name} has shape {:?}, expected {shape:?}", w.shape)));
        }
        let n = shape.iter().try_fold(1usize, |a, b| a.checked_mul(*b)).ok_or_else(|| err(format!("weight {name} too large")))?;
        if w.data.len() != n {
            return Err(err(format!("weight {name} holds {} values, its shape {n}", w.data.len())));
        }
        if w.data.iter().any(|v| !v.is_finite()) {
            return Err(err(format!("weight {name} has non-finite values")));
        }
        Ok(&w.data)
    }

    fn upload(&mut self, data: Vec<f32>) -> usize {
        self.uploads.push(data);
        self.uploads.len() - 1
    }

    fn zero_bias(&mut self) -> usize {
        match self.zero_bias {
            Some(i) => i,
            None => {
                let i = self.upload(vec![0.0; 512]);
                self.zero_bias = Some(i);
                i
            }
        }
    }

    fn uses(&mut self, a: Act, n: usize) {
        let slot = &mut self.need[a.index()];
        *slot = (*slot).max(n);
    }

    fn stage(&mut self, name: &'static str, act: Act, len: usize, layout: Layout) {
        let end = self.ops.len().saturating_sub(1);
        self.stages.push(Stage { name, end, act, len, layout });
    }

    /// A convolution with its batch norm folded: (weight upload, bias upload). A full k × k
    /// convolution (one group, k > 1) is reordered to [out][ky][kx][in] (im2col's order).
    fn conv_bn(&mut self, p: &str, cout: usize, cin_per_group: usize, k: usize) -> Result<(usize, usize)> {
        let w = self.get(&format!("{p}.c.weight"), &[cout, cin_per_group, k, k])?;
        let gamma = self.get(&format!("{p}.bn.weight"), &[cout])?;
        let beta = self.get(&format!("{p}.bn.bias"), &[cout])?;
        let mean = self.get(&format!("{p}.bn.running_mean"), &[cout])?;
        let var = self.get(&format!("{p}.bn.running_var"), &[cout])?;
        let taps = k * k;
        let per = cin_per_group * taps;
        let mut fw = Vec::with_capacity(w.len());
        let mut fb = Vec::with_capacity(cout);
        for o in 0..cout {
            let (Some(g), Some(b), Some(m), Some(v)) = (gamma.get(o), beta.get(o), mean.get(o), var.get(o)) else {
                return Err(err(format!("{p}: short batch norm")));
            };
            if *v < 0.0 {
                return Err(err(format!("{p}: negative running variance")));
            }
            let s = f64::from(*g) / (f64::from(*v) + BN_EPS).sqrt();
            fb.push((f64::from(*b) - f64::from(*m) * s) as f32);
            let row = w.get(o * per..(o + 1) * per).ok_or_else(|| err(format!("{p}: short weight")))?;
            if k > 1 && cin_per_group > 1 {
                for t in 0..taps {
                    for ci in 0..cin_per_group {
                        fw.push((f64::from(row.get(ci * taps + t).copied().unwrap_or(0.0)) * s) as f32);
                    }
                }
            } else {
                fw.extend(row.iter().map(|x| (f64::from(*x) * s) as f32));
            }
        }
        Ok((self.upload(fw), self.upload(fb)))
    }

    /// A linear layer: (weight [n][k], bias [n]).
    fn linear(&mut self, p: &str, n: usize, k: usize) -> Result<(usize, usize)> {
        let w = self.get(&format!("{p}.weight"), &[n, k])?.to_vec();
        let b = self.get(&format!("{p}.bias"), &[n])?.to_vec();
        Ok((self.upload(w), self.upload(b)))
    }

    fn norm_weights(&mut self, p: &str, c: usize) -> Result<(usize, usize)> {
        let g = self.get(&format!("{p}.weight"), &[c])?.to_vec();
        let b = self.get(&format!("{p}.bias"), &[c])?.to_vec();
        Ok((self.upload(g), self.upload(b)))
    }

    fn matmul(&mut self, a: Act, wb: (usize, usize), out: Act, s: Matmul) {
        self.uses(a, s.m * s.k);
        self.uses(out, s.m * s.n);
        self.ops.push(PlanOp::Matmul(s, [Res::Act(a), Res::W(wb.0), Res::W(wb.1), Res::Act(out)]));
    }

    fn im2col(&mut self, x: Act, s: Im2col, out: Act) -> Result<(usize, usize)> {
        let (oh, ow) = s.out_size()?;
        self.uses(x, s.cin * s.h * s.w);
        self.uses(out, oh * ow * s.k * s.k * s.cin);
        self.ops.push(PlanOp::Im2col(s, [Res::Act(x), Res::Act(out)]));
        Ok((oh, ow))
    }

    fn dwconv(&mut self, x: Act, wb: (usize, usize), out: Act, c: usize, res: usize, stride: usize, gelu: bool) -> Result<usize> {
        let s = Depthwise { c, h: res, w: res, k: 3, stride, pad: 1, gelu };
        let (oh, ow) = s.out_size()?;
        self.uses(x, res * res * c);
        self.uses(out, oh * ow * c);
        self.ops.push(PlanOp::Depthwise(s, [Res::Act(x), Res::W(wb.0), Res::W(wb.1), Res::Act(out)]));
        Ok(oh)
    }

    fn norm(&mut self, x: Act, gb: (usize, usize), out: Act, s: LayerNorm) {
        self.uses(x, s.gather.map_or(s.rows * s.c, |g| g.rh * g.rw * s.c));
        self.uses(out, s.rows * s.c);
        self.ops.push(PlanOp::Norm(s, [Res::Act(x), Res::W(gb.0), Res::W(gb.1), Res::Act(out)]));
    }

    /// One TinyViT block on `x` (`res` × `res` tokens of `dim`); the result is in `y`.
    fn block(&mut self, p: &str, x: Act, y: Act, dim: usize, res: usize, heads: usize, ws: usize) -> Result<()> {
        if heads == 0 || !dim.is_multiple_of(heads) || ws == 0 {
            return Err(err(format!("{p}: unsupported attention shape (dim {dim}, {heads} heads)")));
        }
        let grid = WindowGrid { rh: res, rw: res, ws };
        let (n, rows) = (grid.n()?, grid.rows()?);
        let windows = rows / n.max(1);
        let a = format!("{p}.attn");
        let gb = self.norm_weights(&format!("{a}.norm"), dim)?;
        self.norm(x, gb, Act::A, LayerNorm { rows, c: dim, eps: LN_EPS, gather: Some(grid), channel_major: false });
        let qkv = self.linear(&format!("{a}.qkv"), 3 * dim, dim)?;
        self.matmul(Act::A, qkv, Act::H, Matmul { m: rows, n: 3 * dim, k: dim, ..Default::default() });
        // relative position bias, expanded as candle does: offsets numbered in first-seen order
        let points: Vec<(i64, i64)> = (0..ws).flat_map(|px| (0..ws).map(move |py| (px as i64, py as i64))).collect();
        let mut idxs = Vec::with_capacity(n * n);
        let mut offsets: HashMap<(i64, i64), usize> = HashMap::new();
        for &(x1, y1) in &points {
            for &(x2, y2) in &points {
                let l = offsets.len();
                idxs.push(*offsets.entry(((x2 - x1).abs(), (y2 - y1).abs())).or_insert(l));
            }
        }
        let biases = self.get(&format!("{a}.attention_biases"), &[heads, offsets.len()])?;
        let mut ab = Vec::with_capacity(heads * n * n);
        for h in 0..heads {
            for i in &idxs {
                ab.push(biases.get(h * offsets.len() + i).copied().ok_or_else(|| err(format!("{a}: short attention biases")))?);
            }
        }
        let ab = self.upload(ab);
        let att = Attention::fused_qkv(windows, n, heads, dim / heads, true)?;
        self.uses(Act::A, rows * dim);
        self.ops.push(PlanOp::Attn(att, [Res::Act(Act::H), Res::W(ab), Res::Act(Act::A)]));
        let proj = self.linear(&format!("{a}.proj"), dim, dim)?;
        self.matmul(Act::A, proj, Act::B, Matmul { m: rows, n: dim, k: dim, ..Default::default() });
        self.uses(x, res * res * dim);
        self.ops.push(PlanOp::Add(Add { len: res * res * dim, scatter: Some((grid, dim)) }, [Res::Act(Act::B), Res::Act(x)]));
        let lc = self.conv_bn(&format!("{p}.local_conv"), dim, 1, 3)?;
        self.dwconv(x, lc, y, dim, res, 1, false)?;
        let m = format!("{p}.mlp");
        let gb = self.norm_weights(&format!("{m}.norm"), dim)?;
        self.norm(y, gb, Act::A, LayerNorm { rows: res * res, c: dim, eps: LN_EPS, gather: None, channel_major: false });
        let fc1 = self.linear(&format!("{m}.fc1"), 4 * dim, dim)?;
        self.matmul(Act::A, fc1, Act::H, Matmul { m: res * res, n: 4 * dim, k: dim, gelu: true, ..Default::default() });
        let fc2 = self.linear(&format!("{m}.fc2"), dim, 4 * dim)?;
        self.matmul(Act::H, fc2, y, Matmul { m: res * res, n: dim, k: 4 * dim, accumulate: true, ..Default::default() });
        Ok(())
    }

    /// PatchMerging on `x` (`res`², `dim`) into `out` channels, in place; returns the new side.
    fn patch_merging(&mut self, p: &str, x: Act, res: usize, dim: usize, out: usize) -> Result<usize> {
        let stride = if [320, 448, 576].contains(&out) { 1 } else { 2 };
        let c1 = self.conv_bn(&format!("{p}.conv1"), out, dim, 1)?;
        self.matmul(x, c1, Act::H, Matmul { m: res * res, n: out, k: dim, gelu: true, ..Default::default() });
        let c2 = self.conv_bn(&format!("{p}.conv2"), out, 1, 3)?;
        let oh = self.dwconv(Act::H, c2, Act::H2, out, res, stride, true)?;
        let c3 = self.conv_bn(&format!("{p}.conv3"), out, out, 1)?;
        self.matmul(Act::H2, c3, x, Matmul { m: oh * oh, n: out, k: out, ..Default::default() });
        Ok(oh)
    }

    fn plan(&mut self) -> Result<()> {
        // patch embedding: two stride-2 3×3 convs, GELU between
        let c = self.conv_bn("patch_embed.seq.0", 32, 3, 3)?;
        let conv = |cin: usize, side: usize, chw: bool| Im2col { cin, h: side, w: side, k: 3, stride: 2, pad: 1, chw };
        let (h, w) = self.im2col(Act::Inp, conv(3, INPUT_SIDE, true), Act::H2)?;
        self.matmul(Act::H2, c, Act::H, Matmul { m: h * w, n: 32, k: 27, gelu: true, ..Default::default() });
        let c = self.conv_bn("patch_embed.seq.2", 64, 32, 3)?;
        let (h2, w2) = self.im2col(Act::H, conv(32, h, false), Act::H2)?;
        self.matmul(Act::H2, c, Act::X, Matmul { m: h2 * w2, n: 64, k: 288, ..Default::default() });
        let mut res = h2;
        self.stage("patch_embed", Act::X, res * res * 64, Layout::Hwc { h: res, w: res, c: 64 });
        // layer 0: MBConv blocks, then PatchMerging
        let dim = EMBED_DIMS[0];
        for i in 0..DEPTHS[0] {
            let p = format!("layers.0.blocks.{i}");
            let c1 = self.conv_bn(&format!("{p}.conv1"), dim * 4, dim, 1)?;
            self.matmul(Act::X, c1, Act::H, Matmul { m: res * res, n: dim * 4, k: dim, gelu: true, ..Default::default() });
            let c2 = self.conv_bn(&format!("{p}.conv2"), dim * 4, 1, 3)?;
            self.dwconv(Act::H, c2, Act::H2, dim * 4, res, 1, true)?;
            let c3 = self.conv_bn(&format!("{p}.conv3"), dim, dim * 4, 1)?;
            self.matmul(Act::H2, c3, Act::X, Matmul { m: res * res, n: dim, k: dim * 4, accumulate: true, gelu_after: true, ..Default::default() });
        }
        res = self.patch_merging("layers.0.downsample", Act::X, res, dim, EMBED_DIMS[1])?;
        self.stage("layer0", Act::X, res * res * EMBED_DIMS[1], Layout::Flat);
        // layers 1–3: TinyViT blocks (+ PatchMerging after 1 and 2)
        let (mut x, mut y) = (Act::X, Act::X2);
        let names = ["layer1", "layer2", "layer3"];
        for li in 1..4 {
            let dim = EMBED_DIMS[li];
            for i in 0..DEPTHS[li] {
                self.block(&format!("layers.{li}.blocks.{i}"), x, y, dim, res, HEADS[li], WINDOWS[li])?;
                std::mem::swap(&mut x, &mut y);
            }
            let mut out_dim = dim;
            if li < 3 {
                out_dim = EMBED_DIMS[li + 1];
                res = self.patch_merging(&format!("layers.{li}.downsample"), x, res, dim, out_dim)?;
            }
            self.stage(names.get(li - 1).copied().unwrap_or("layer"), x, res * res * out_dim, Layout::Flat);
        }
        if res != 64 {
            return Err(err(format!("the encoder ends at {res}×{res}, expected 64×64")));
        }
        // neck: 1×1 conv, LayerNorm2d, 3×3 conv, LayerNorm2d (no biases on the convs)
        let rows = res * res;
        let w0 = self.get("neck.0.weight", &[256, EMBED_DIMS[3], 1, 1])?.to_vec();
        let w0 = self.upload(w0);
        let zb = self.zero_bias();
        self.matmul(x, (w0, zb), Act::A, Matmul { m: rows, n: 256, k: EMBED_DIMS[3], ..Default::default() });
        let n1 = self.norm_weights("neck.1", 256)?;
        self.norm(Act::A, n1, Act::B, LayerNorm { rows, c: 256, eps: LN2D_EPS, gather: None, channel_major: false });
        let w2 = self.get("neck.2.weight", &[256, 256, 3, 3])?;
        let mut r = Vec::with_capacity(w2.len());
        for o in 0..256 {
            for t in 0..9 {
                for ci in 0..256 {
                    r.push(w2.get((o * 256 + ci) * 9 + t).copied().unwrap_or(0.0));
                }
            }
        }
        let w2 = self.upload(r);
        self.im2col(Act::B, Im2col { cin: 256, h: res, w: res, k: 3, stride: 1, pad: 1, chw: false }, Act::H2)?;
        self.matmul(Act::H2, (w2, zb), Act::A, Matmul { m: rows, n: 256, k: 256 * 9, ..Default::default() });
        let n3 = self.norm_weights("neck.3", 256)?;
        self.norm(Act::A, n3, Act::Out, LayerNorm { rows, c: 256, eps: LN2D_EPS, gather: None, channel_major: true });
        self.uses(Act::Inp, INPUT_LEN);
        self.uses(Act::Out, OUTPUT_LEN);
        self.stage("neck", Act::Out, OUTPUT_LEN, Layout::Flat);
        Ok(())
    }
}

/// MobileSAM's image encoder on a wgpu device, weights resident, buffers reused per frame.
pub struct TinyVitGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    steps: Vec<NnStep>,
    acts: Vec<wgpu::Buffer>,
    stages: Vec<Stage>,
    /// Held for a whole frame (the activation buffers are shared); also the read-back buffer.
    run: Mutex<Option<wgpu::Buffer>>,
    name: String,
}

impl std::fmt::Debug for TinyVitGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TinyVitGpu({}, {} dispatches)", self.name, self.steps.len())
    }
}

impl TinyVitGpu {
    /// A device of its own on the adapter Settings ▸ Playback ▸ Graphics device picks
    /// ([`crate::adapter_pick`]), else the high-performance one (CPU adapters such as llvmpipe
    /// are refused: candle on the CPU is faster), then [`TinyVitGpu::new`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(weights: &HashMap<String, Weight>) -> Result<Self> {
        if wgpu::Instance::enabled_backend_features().is_empty() {
            return Err(err("this build has no GPU backend"));
        }
        let instance = wgpu::Instance::default();
        // Settings ▸ Playback ▸ Graphics device (as exports), else the high-performance one
        let adapter = match crate::adapter_pick::choose(&instance, wgpu::Backends::all()) {
            Some(a) => a,
            None => {
                let opts = wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() };
                pollster::block_on(instance.request_adapter(&opts)).map_err(|e| err(format!("no GPU adapter: {e}")))?
            }
        };
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err(err(format!("{} is a CPU renderer", info.name)));
        }
        let limits = adapter.limits();
        let desc = wgpu::DeviceDescriptor {
            label: Some("filmcraft magic mask encoder"),
            required_limits: wgpu::Limits {
                max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
                max_buffer_size: limits.max_buffer_size,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&desc)).map_err(|e| err(format!("GPU device: {e}")))?;
        // wgpu's default handler panics; errors are caught by the scopes around each use
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log::warn!("Magic Mask GPU error: {e}")));
        device.set_device_lost_callback(|reason, msg| log::warn!("Magic Mask GPU device lost ({reason:?}): {msg}"));
        let mut gpu = Self::new(&device, &queue, weights)?;
        gpu.name = format!("{} ({:?})", info.name, info.backend);
        Ok(gpu)
    }

    /// Plan the forward pass on `device`, upload the weights and allocate the buffers. Missing
    /// or misshapen weights, buffers over the device's limits and kernel errors are refused.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, weights: &HashMap<String, Weight>) -> Result<Self> {
        let mut p = Planner::new(weights);
        p.plan()?;
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let val = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let built = Self::build(device, queue, p);
        // the uploads' staging copies retire with this (empty) submission
        let index = queue.submit(std::iter::empty());
        let waited = device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(GPU_WAIT) });
        if let Some(e) = pollster::block_on(val.pop()) {
            return Err(err(format!("kernels: {e}")));
        }
        if let Some(e) = pollster::block_on(oom.pop()) {
            return Err(err(format!("out of GPU memory: {e}")));
        }
        waited.map_err(|e| err(format!("upload: {e}")))?;
        built
    }

    fn build(device: &wgpu::Device, queue: &wgpu::Queue, p: Planner) -> Result<Self> {
        let nn = NnKernels::new(device);
        let max = nn.max_binding();
        let mut acts = Vec::with_capacity(ACTS.len());
        for (i, a) in ACTS.iter().enumerate() {
            let bytes = (p.need.get(i).copied().unwrap_or(0).max(4) as u64).saturating_mul(4);
            if bytes > max {
                return Err(err(format!("{a:?} buffer: {bytes} bytes are over the GPU's {max}-byte binding limit")));
            }
            acts.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tinyvit-act"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let mut wbufs = Vec::with_capacity(p.uploads.len());
        for data in &p.uploads {
            if (data.len() as u64).saturating_mul(4) > max {
                return Err(err("a weight is over the GPU's binding limit"));
            }
            let contents: &[u8] = if data.is_empty() { &[0; 16] } else { bytemuck::cast_slice(data) };
            wbufs.push(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("tinyvit-weight"),
                contents,
                usage: wgpu::BufferUsages::STORAGE,
            }));
        }
        let buf = |r: Res| -> Result<&wgpu::Buffer> {
            match r {
                Res::Act(a) => acts.get(a.index()),
                Res::W(w) => wbufs.get(w),
            }
            .ok_or_else(|| err("internal: a planned buffer is missing"))
        };
        let mut steps = Vec::with_capacity(p.ops.len());
        for op in &p.ops {
            steps.push(match op {
                PlanOp::Matmul(s, [a, w, b, o]) => nn.matmul(s, buf(*a)?, buf(*w)?, buf(*b)?, buf(*o)?)?,
                PlanOp::Im2col(s, [x, o]) => nn.im2col(s, buf(*x)?, buf(*o)?)?,
                PlanOp::Depthwise(s, [x, w, b, o]) => nn.depthwise(s, buf(*x)?, buf(*w)?, buf(*b)?, buf(*o)?)?,
                PlanOp::Norm(s, [x, g, b, o]) => nn.layer_norm(s, buf(*x)?, buf(*g)?, buf(*b)?, buf(*o)?)?,
                PlanOp::Attn(s, [qkv, ab, o]) => {
                    let q = buf(*qkv)?;
                    nn.attention(s, q, q, Some(buf(*ab)?), buf(*o)?)?
                }
                PlanOp::Add(s, [src, x]) => nn.add(s, buf(*src)?, buf(*x)?)?,
            });
        }
        Ok(Self { device: device.clone(), queue: queue.clone(), steps, acts, stages: p.stages, run: Mutex::new(None), name: String::from("GPU") })
    }

    /// The adapter's name, for logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The embedding (256 × 64 × 64, CHW) of a (3, 1024, 1024) normalised input.
    pub fn encode(&self, input: &[f32]) -> Result<Vec<f32>> {
        let last = self.steps.len();
        let mut out = self.run_stages(input, &[(last, Act::Out, OUTPUT_LEN)])?;
        out.pop().ok_or_else(|| err("no output"))
    }

    /// Every stage's output (see [`STAGES`]), for parity tests.
    pub fn encode_stages(&self, input: &[f32]) -> Result<Vec<(String, Vec<f32>)>> {
        let marks: Vec<(usize, Act, usize)> = self.stages.iter().map(|s| (s.end + 1, s.act, s.len)).collect();
        let outs = self.run_stages(input, &marks)?;
        let mut v = Vec::with_capacity(outs.len());
        for (s, data) in self.stages.iter().zip(outs) {
            let data = match s.layout {
                Layout::Flat => data,
                Layout::Hwc { h, w, c } => {
                    let mut t = vec![0f32; data.len()];
                    for (i, x) in data.iter().enumerate() {
                        let (ch, pix) = (i % c.max(1), i / c.max(1));
                        if let Some(slot) = t.get_mut(ch * h * w + pix) {
                            *slot = *x;
                        }
                    }
                    t
                }
            };
            v.push((s.name.to_string(), data));
        }
        Ok(v)
    }

    /// Runs the steps up to each mark's end and reads that mark's buffer.
    fn run_stages(&self, input: &[f32], marks: &[(usize, Act, usize)]) -> Result<Vec<Vec<f32>>> {
        if input.len() != INPUT_LEN {
            return Err(err(format!("an input of {} values, expected {INPUT_LEN}", input.len())));
        }
        let mut guard = self.run.lock().unwrap_or_else(PoisonError::into_inner);
        let inp = self.acts.get(Act::Inp.index()).ok_or_else(|| err("internal: no input buffer"))?;
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let val = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        self.queue.write_buffer(inp, 0, bytemuck::cast_slice(input));
        let mut outs = Vec::with_capacity(marks.len());
        let mut from = 0usize;
        let mut failure = None;
        for &(end, act, len) in marks {
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("tinyvit") });
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("tinyvit"), timestamp_writes: None });
                for step in self.steps.get(from..end.min(self.steps.len())).unwrap_or(&[]) {
                    step.record(&mut pass);
                }
            }
            from = end;
            let Some(src) = self.acts.get(act.index()) else {
                failure = Some(err("internal: no stage buffer"));
                break;
            };
            match self.submit_read(&mut guard, enc, src, len) {
                Ok(v) => outs.push(v),
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = pollster::block_on(val.pop()) {
            return Err(err(format!("validation: {e}")));
        }
        if let Some(e) = pollster::block_on(oom.pop()) {
            return Err(err(format!("out of GPU memory: {e}")));
        }
        if let Some(e) = failure {
            return Err(e);
        }
        if outs.iter().flatten().any(|v| !v.is_finite()) {
            return Err(err("the encoder produced non-finite values"));
        }
        Ok(outs)
    }

    /// Submits `enc` plus a copy of the first `n` f32 of `src`, waits and returns them.
    fn submit_read(&self, cache: &mut Option<wgpu::Buffer>, mut enc: wgpu::CommandEncoder, src: &wgpu::Buffer, n: usize) -> Result<Vec<f32>> {
        let bytes = (n as u64).saturating_mul(4);
        if bytes == 0 || bytes > src.size() {
            return Err(err(format!("read-back of {n} values from a {}-byte buffer", src.size())));
        }
        let read = match cache.take().filter(|b| b.size() >= bytes) {
            Some(b) => b,
            None => self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tinyvit-read"),
                size: bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        };
        enc.copy_buffer_to_buffer(src, 0, &read, 0, bytes);
        let index = self.queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        read.slice(..bytes).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(GPU_WAIT) }).map_err(|e| err(format!("GPU wait: {e}")))?;
        match rx.recv_timeout(GPU_WAIT) {
            Ok(r) => r.map_err(|e| err(format!("read-back: {e}")))?,
            Err(e) => return Err(err(format!("read-back never completed: {e}"))),
        }
        let out: Vec<f32> = {
            let data = read.slice(..bytes).get_mapped_range().map_err(|e| err(format!("read-back: {e}")))?;
            data.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
        };
        read.unmap();
        *cache = Some(read);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_misshapen_weights_are_refused_before_any_gpu_work() {
        let w = HashMap::new();
        assert!(Planner::new(&w).plan().unwrap_err().0.contains("missing"));
        let mut w = HashMap::new();
        w.insert("patch_embed.seq.0.c.weight".to_string(), Weight { shape: vec![32, 3, 3, 3], data: vec![0.0; 7] });
        assert!(Planner::new(&w).plan().unwrap_err().0.contains("holds 7"));
        w.insert("patch_embed.seq.0.c.weight".to_string(), Weight { shape: vec![usize::MAX, 2, 3, 3], data: vec![] });
        assert!(Planner::new(&w).plan().is_err());
        let mut nan = vec![0.0; 32 * 27];
        nan[3] = f32::NAN;
        w.insert("patch_embed.seq.0.c.weight".to_string(), Weight { shape: vec![32, 3, 3, 3], data: nan });
        assert!(Planner::new(&w).plan().unwrap_err().0.contains("non-finite"));
    }
}
