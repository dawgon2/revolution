//! SAM 2.1's heavy parts on the GPU (fork G3.4, FORK-PLAN §1a.124): the Hiera image encoder +
//! FPN neck, the memory attention (4 layers, 2-D axial RoPE) and the memory encoder, built from
//! the [`crate::nn`] ops. `filmcraft-mask`'s candle code (`sam2::cpu`) is the reference these are
//! tested against op for op. The mask decoder is [`decoder`] (fork LO.6); the prompt encoder
//! stays on the CPU there.
//!
//! Ported from github.com/facebookresearch/sam2 (Apache-2.0, Copyright (c) Meta Platforms,
//! Inc. and affiliates; see `NOTICE`). This crate knows nothing of YAML or candle: the caller
//! passes the architecture as a [`Sam2GpuSpec`] (one per checkpoint: Hiera Large or Base+) and
//! the checkpoint's tensors as plain [`Weight`]s named as in the state dict.
//!
//! **How it runs.** At [`Sam2Gpu::new`] the image encoder and memory encoder are planned once
//! into [`NnStep`]s, every weight is uploaded (some folded: the neck's level-0/1 convolutions
//! into `conv_s0` / `conv_s1`, the fuser's layer scale into its last linear layer, convolution
//! kernels reordered for im2col), and activation buffers are sized by the plans. The memory
//! attention's length changes with the memory bank, so its steps are planned per memory length
//! (cached) over buffers sized for the largest bank. Each call uploads its inputs, records the
//! steps into one command buffer, submits and reads the outputs back.
//! - Every tensor layout is HWC / token-major.
//! - Windowed attention gathers zero-padded windows in the first norm (padding after the norm, as
//!   Hiera); the query of a stage's first block is max-pooled 2 × 2 inside its windows; global
//!   attention and the memory attention are `q·kᵀ` / softmax / `p·v` as batched matmuls in query
//!   chunks (so the score matrix stays within one binding).
//! - GELU is the exact erf form (PyTorch `nn.GELU`); LayerNorm ε 1e-6 in Hiera and the memory
//!   encoder, 1e-5 in the memory attention.
//!
//! Every failure (no adapter, a validation or out-of-memory error, a lost device, a timeout,
//! hostile weight shapes) is a [`Sam2GpuError`], never a panic: callers fall back to the CPU.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use wgpu::util::DeviceExt;

use crate::nn::{Act, Add, Attention, Bmm, Depthwise, Im2col, LayerNorm, NnError, NnKernels, NnStep, Resample, ResampleMode, Strided, UnaryOp, WindowGrid};
pub use crate::tinyvit::Weight;

/// Side of the encoder's square input.
pub const INPUT_SIDE: usize = 1024;
/// Values in the encoder's input (3 × 1024 × 1024, CHW).
pub const INPUT_LEN: usize = 3 * INPUT_SIDE * INPUT_SIDE;
const D: usize = 256;
const MEM: usize = 64;
const TOKENS: usize = 64 * 64;
const S0_LEN: usize = 256 * 256 * 32;
const S1_LEN: usize = 128 * 128 * 64;
const FEAT_LEN: usize = TOKENS * D;
/// The memory encoder's mask side.
pub const MASK_SIDE: usize = 1024;
/// Most memory tokens the memory attention takes: 7 + 16 frames of 4096 tokens plus 32 object
/// pointers of 4 tokens (more is refused).
pub const MAX_MEMORY_TOKENS: usize = (7 + 16) * TOKENS + 4 * 32;
/// Floats one attention score chunk may hold (128 MiB).
const SCORE_BUDGET: usize = 32 << 20;
/// Floats an MLP hidden chunk may hold (128 MiB).
const HIDDEN_BUDGET: usize = 32 << 20;
/// Longest wait for one call.
const GPU_WAIT: Duration = Duration::from_secs(60);
/// Memory attention plans kept (one per memory length).
const MAX_CACHED_PLANS: usize = 48;

/// Why the GPU passes could not be built or run.
#[derive(Clone, Debug, PartialEq)]
pub struct Sam2GpuError(pub String);

impl std::fmt::Display for Sam2GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SAM 2 on the GPU: {}", self.0)
    }
}

impl std::error::Error for Sam2GpuError {}

impl From<NnError> for Sam2GpuError {
    fn from(e: NnError) -> Self {
        Sam2GpuError(e.0)
    }
}

fn err(s: impl Into<String>) -> Sam2GpuError {
    Sam2GpuError(s.into())
}

type Result<T> = std::result::Result<T, Sam2GpuError>;

/// One Hiera block's shape (see `filmcraft_mask::sam2::BlockSpec`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuBlock {
    pub dim: usize,
    pub dim_out: usize,
    pub heads: usize,
    /// 0: global attention.
    pub window: usize,
    pub pool: bool,
    /// Token grid side at the block's input.
    pub res: usize,
}

/// The architecture and the derived constants (computed by the caller with the same code the
/// CPU reference uses).
#[derive(Clone, Debug, PartialEq)]
pub struct Sam2GpuSpec {
    pub embed_dim: usize,
    pub blocks: Vec<GpuBlock>,
    /// Index of each stage's last block.
    pub stage_ends: [usize; 4],
    /// The neck's input widths per stage, shallow first (`embed_dim`, ×2, ×4, ×8).
    pub stage_dims: [usize; 4],
    pub mem_layers: usize,
    pub mem_ffn: usize,
    pub fuser_layers: usize,
    /// Hiera's position embedding (256 × 256 × embed_dim).
    pub hiera_pos: Vec<f32>,
    /// The image embedding's sine position (4096 × 256).
    pub vision_pos: Vec<f32>,
    /// RoPE tables (4096 × 128 each).
    pub rope_cos: Vec<f32>,
    pub rope_sin: Vec<f32>,
}

impl Sam2GpuSpec {
    fn validate(&self) -> Result<()> {
        let c = self.embed_dim;
        let ok = |b: bool, what: &str| if b { Ok(()) } else { Err(err(format!("spec: {what}"))) };
        ok((8..=2048).contains(&c), "trunk width")?;
        ok(!self.blocks.is_empty() && self.blocks.len() <= 256, "block count")?;
        ok(self.stage_ends.iter().all(|e| *e < self.blocks.len()), "stage ends")?;
        ok(self.stage_dims == [c, c * 2, c * 4, c * 8], "stage widths")?;
        for b in &self.blocks {
            ok(
                b.heads > 0 && b.dim_out.is_multiple_of(b.heads) && (b.dim_out / b.heads).is_multiple_of(4) && b.dim_out / b.heads <= crate::nn::MAX_HEAD_DIM,
                "heads",
            )?;
            ok(b.res > 0 && b.res <= 256 && b.window <= b.res.max(64), "block grid")?;
            ok(!b.pool || (b.window >= 2 && b.window.is_multiple_of(2) && b.res.is_multiple_of(2)), "pooled block")?;
            ok(b.dim <= 4096 && b.dim_out <= 4096, "block width")?;
        }
        ok(self.hiera_pos.len() == 256 * 256 * c, "position embedding length")?;
        ok(self.vision_pos.len() == FEAT_LEN, "vision position length")?;
        ok(self.rope_cos.len() == TOKENS * D / 2 && self.rope_sin.len() == TOKENS * D / 2, "rope table length")?;
        ok((1..=16).contains(&self.mem_layers) && (1..=16384).contains(&self.mem_ffn) && (1..=8).contains(&self.fuser_layers), "memory parts")?;
        Ok(())
    }
}

/// A buffer an op binds: an activation of its pipeline or an uploaded weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum B {
    A(usize),
    W(usize),
}

#[derive(Clone, Debug)]
enum Op {
    Bmm(Bmm, B, B, Option<B>, B),
    Im2col(Im2col, B, B),
    Dw(Depthwise, B, B, B, B),
    Norm(LayerNorm, bool, B, B, B, B),
    Attn(Attention, B, B, B),
    Add(Add, B, B),
    Unary(UnaryOp, usize, B),
    Softmax(usize, usize, B),
    Resample(Resample, B, B),
    Pool2 { batch: usize, h: usize, w: usize, c: usize, stride: usize, off: usize, x: B, o: B },
    Rope { rows: usize, pairs: usize, stride: usize, off: usize, x: B },
    Ew { len: usize, blen: usize, alpha: f32, beta: f32, a: B, b: B, o: B },
}

/// The weights to upload, by name (deduplicated).
struct Reg<'w> {
    src: &'w HashMap<String, Weight>,
    /// Checkpoint tensors are borrowed (no copy before the upload), derived ones owned.
    uploads: Vec<Cow<'w, [f32]>>,
    ids: HashMap<String, usize>,
}

impl<'w> Reg<'w> {
    fn get(&self, name: &str, shape: &[usize]) -> Result<&'w [f32]> {
        let w = self.src.get(name).ok_or_else(|| err(format!("weight {name} missing")))?;
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

    /// Upload `name` as it is.
    fn w(&mut self, name: &str, shape: &[usize]) -> Result<B> {
        if let Some(i) = self.ids.get(name) {
            return Ok(B::W(*i));
        }
        let data = self.get(name, shape)?;
        self.uploads.push(Cow::Borrowed(data));
        let i = self.uploads.len() - 1;
        self.ids.insert(name.to_string(), i);
        Ok(B::W(i))
    }

    /// Upload derived data under `key`.
    fn put(&mut self, key: &str, data: Vec<f32>) -> B {
        if let Some(i) = self.ids.get(key) {
            return B::W(*i);
        }
        self.uploads.push(Cow::Owned(data));
        let i = self.uploads.len() - 1;
        self.ids.insert(key.to_string(), i);
        B::W(i)
    }

    /// A Linear: (weight [n][k], bias [n]).
    fn linear(&mut self, p: &str, n: usize, k: usize) -> Result<(B, B)> {
        Ok((self.w(&format!("{p}.weight"), &[n, k])?, self.w(&format!("{p}.bias"), &[n])?))
    }

    /// A 1 × 1 convolution as a Linear.
    fn conv1(&mut self, p: &str, n: usize, k: usize) -> Result<(B, B)> {
        let w = self.get(&format!("{p}.weight"), &[n, k, 1, 1])?.to_vec();
        let b = self.w(&format!("{p}.bias"), &[n])?;
        Ok((self.put(&format!("{p}.weight#1x1"), w), b))
    }

    /// A full k × k convolution reordered to im2col's [out][ky][kx][in].
    fn conv_k(&mut self, p: &str, cout: usize, cin: usize, k: usize) -> Result<(B, B)> {
        let w = self.get(&format!("{p}.weight"), &[cout, cin, k, k])?;
        let taps = k * k;
        let mut r = Vec::with_capacity(w.len());
        for o in 0..cout {
            for t in 0..taps {
                for ci in 0..cin {
                    r.push(w.get((o * cin + ci) * taps + t).copied().unwrap_or(0.0));
                }
            }
        }
        let b = self.w(&format!("{p}.bias"), &[cout])?;
        Ok((self.put(&format!("{p}.weight#im2col"), r), b))
    }

    fn norm(&mut self, p: &str, c: usize) -> Result<(B, B)> {
        Ok((self.w(&format!("{p}.weight"), &[c])?, self.w(&format!("{p}.bias"), &[c])?))
    }
}

/// The end of a named stage: after op `end`, `len` values of activation `act`.
#[derive(Clone, Debug)]
struct Mark {
    name: String,
    end: usize,
    act: usize,
    len: usize,
}

/// An op list over one pipeline's activations, with the sizes they need.
#[derive(Debug, Default)]
struct Ops {
    ops: Vec<Op>,
    need: Vec<usize>,
    marks: Vec<Mark>,
}

impl Ops {
    fn uses(&mut self, b: B, n: usize) {
        if let B::A(a) = b {
            if self.need.len() <= a {
                self.need.resize(a + 1, 0);
            }
            if let Some(s) = self.need.get_mut(a) {
                *s = (*s).max(n);
            }
        }
    }

    fn push(&mut self, op: Op) {
        self.ops.push(op);
    }

    fn mark(&mut self, name: &str, act: usize, len: usize) {
        let end = self.ops.len();
        self.marks.push(Mark { name: name.to_string(), end, act, len });
    }

    /// `o = a·wᵀ + b` over `m` contiguous rows (`act`, `accumulate`), in row chunks of at most
    /// `chunk` rows.
    #[allow(clippy::too_many_arguments)]
    fn linear(&mut self, a: B, wb: (B, B), o: B, m: usize, n: usize, k: usize, act: Act, accumulate: bool) {
        self.uses(a, m * k);
        self.uses(o, m * n);
        self.push(Op::Bmm(Bmm { act, accumulate, ..Bmm::linear(m, n, k) }, a, wb.0, Some(wb.1), o));
    }

    fn norm(&mut self, x: B, gb: (B, B), o: B, s: LayerNorm, zero_pad: bool) {
        self.uses(x, s.gather.map_or(s.rows * s.c, |g| g.rh * g.rw * s.c));
        self.uses(o, s.rows * s.c);
        self.push(Op::Norm(s, zero_pad, x, gb.0, gb.1, o));
    }

    /// An MLP `x += fc2(act(fc1(norm_out)))` over `m` rows, the hidden layer in row chunks.
    #[allow(clippy::too_many_arguments)]
    fn mlp(&mut self, inp: B, hidden: B, x: B, fc1: (B, B), fc2: (B, B), m: usize, dim: usize, hid: usize, act: Act) {
        let rows = (HIDDEN_BUDGET / hid.max(1)).clamp(64, m.max(64)) / 64 * 64;
        let mut r0 = 0;
        while r0 < m {
            let rc = rows.min(m - r0);
            self.uses(inp, (r0 + rc) * dim);
            self.uses(hidden, rc * hid);
            self.uses(x, (r0 + rc) * dim);
            let s1 = Bmm { a: Strided { off: r0 * dim, ld: dim, bs: 0 }, act, ..Bmm::linear(rc, hid, dim) };
            self.push(Op::Bmm(s1, inp, fc1.0, Some(fc1.1), hidden));
            let s2 = Bmm { o: Strided { off: r0 * dim, ld: dim, bs: 0 }, accumulate: true, ..Bmm::linear(rc, dim, hid) };
            self.push(Op::Bmm(s2, hidden, fc2.0, Some(fc2.1), x));
            r0 += rc;
        }
    }

    /// Softmax attention as matmuls: for `heads` heads of width `hd`, queries rows `nq` of `q`
    /// (row stride `q_ld`, head h at `q_off + h·hd`), keys / values `nk` rows of `k` / `v`; the
    /// output rows (stride `o_ld`, head h at `h·hd`) go to `o`. Chunks of queries keep the
    /// scores within [`SCORE_BUDGET`].
    #[allow(clippy::too_many_arguments)]
    fn sdpa(&mut self, heads: usize, hd: usize, nq: usize, nk: usize, q: (B, usize, usize), k: (B, usize, usize), v: (B, usize, usize), o: (B, usize), s: B) {
        let per = heads.saturating_mul(nk).max(1);
        let chunk = ((SCORE_BUDGET / per).max(64) / 64 * 64).min(nq.max(1));
        let alpha = 1.0 / (hd as f32).sqrt();
        let mut q0 = 0;
        while q0 < nq {
            let cq = chunk.min(nq - q0);
            self.uses(s, heads * cq * nk);
            self.uses(q.0, (q0 + cq) * q.2);
            self.uses(k.0, nk * k.2);
            self.uses(v.0, nk * v.2);
            self.uses(o.0, (q0 + cq) * o.1);
            let sc = Bmm {
                m: cq,
                n: nk,
                k: hd,
                batch: heads,
                a: Strided { off: q0 * q.2 + q.1, ld: q.2, bs: hd },
                w: Strided { off: k.1, ld: k.2, bs: hd },
                o: Strided { off: 0, ld: nk, bs: cq * nk },
                w_kn: false,
                alpha,
                bias: false,
                act: Act::None,
                accumulate: false,
            };
            self.push(Op::Bmm(sc, q.0, k.0, None, s));
            self.push(Op::Softmax(heads * cq, nk, s));
            let pv = Bmm {
                m: cq,
                n: hd,
                k: nk,
                batch: heads,
                a: Strided { off: 0, ld: nk, bs: cq * nk },
                w: Strided { off: v.1, ld: v.2, bs: hd },
                o: Strided { off: q0 * o.1, ld: o.1, bs: hd },
                w_kn: true,
                alpha: 1.0,
                bias: false,
                act: Act::None,
                accumulate: false,
            };
            self.push(Op::Bmm(pv, s, v.0, None, o.0));
            q0 += cq;
        }
    }
}

// Image encoder activations.
const I_INP: usize = 0;
const I_COL: usize = 1;
const I_X: usize = 2;
const I_X2: usize = 3;
const I_A: usize = 4;
const I_H: usize = 5;
const I_B: usize = 6;
const I_Q: usize = 7;
const I_S: usize = 8;
const I_S0: usize = 9;
const I_S1: usize = 10;
const I_FEAT: usize = 11;
const I_L3: usize = 12;
const I_TMP: usize = 13;

/// Names of the stage outputs [`Sam2Gpu::image_stages`] returns (HWC each; the same as the CPU
/// reference's `IMAGE_STAGES`).
pub const IMAGE_STAGES: [&str; 8] = ["patch_embed", "stage0", "stage1", "stage2", "stage3", "s0", "s1", "feat"];

fn plan_image(reg: &mut Reg, spec: &Sam2GpuSpec) -> Result<Ops> {
    let mut o = Ops::default();
    let c = spec.embed_dim;
    let t = "image_encoder.trunk";
    // patch embedding: 7 × 7 stride 4 convolution (im2col + matmul), then the position embedding
    let pe = reg.conv_k(&format!("{t}.patch_embed.proj"), c, 3, 7)?;
    let im = Im2col { cin: 3, h: INPUT_SIDE, w: INPUT_SIDE, k: 7, stride: 4, pad: 3, chw: true };
    let (oh, ow) = im.out_size()?;
    if (oh, ow) != (256, 256) {
        return Err(err("the patch embedding does not give 256 × 256 tokens"));
    }
    o.uses(B::A(I_INP), INPUT_LEN);
    o.uses(B::A(I_COL), oh * ow * 147);
    o.push(Op::Im2col(im, B::A(I_INP), B::A(I_COL)));
    o.linear(B::A(I_COL), pe, B::A(I_X), oh * ow, c, 147, Act::None, false);
    let pos = reg.put("#hiera_pos", spec.hiera_pos.clone());
    o.push(Op::Add(Add { len: oh * ow * c, scatter: None }, pos, B::A(I_X)));
    o.mark("patch_embed", I_X, oh * ow * c);
    let (mut x, mut x2) = (I_X, I_X2);
    let mut stage = 0usize;
    for (i, b) in spec.blocks.iter().enumerate() {
        let p = format!("{t}.blocks.{i}");
        block(reg, &mut o, &p, b, &mut x, &mut x2)?;
        if spec.stage_ends.get(stage) == Some(&i) {
            let res = if b.pool { b.res / 2 } else { b.res };
            let n = res * res;
            let dim = b.dim_out;
            o.mark(&format!("stage{stage}"), x, n * dim);
            neck_level(reg, &mut o, spec, stage, x, n, dim)?;
            stage += 1;
        }
    }
    if stage != 4 {
        return Err(err(format!("the trunk gave {stage} stages")));
    }
    o.mark("s0", I_S0, S0_LEN);
    o.mark("s1", I_S1, S1_LEN);
    o.mark("feat", I_FEAT, FEAT_LEN);
    Ok(o)
}

/// The FPN neck's work for stage `s` (its `x`, `n` tokens of `dim`): levels 0 and 1 fold the
/// neck's 1 × 1 convolution into the decoder's `conv_s0` / `conv_s1`; level 2 is the embedding;
/// level 3 is upsampled (nearest) into level 2.
fn neck_level(reg: &mut Reg, o: &mut Ops, spec: &Sam2GpuSpec, s: usize, x: usize, n: usize, dim: usize) -> Result<()> {
    let conv = |reg: &mut Reg, k: usize| -> Result<(Vec<f32>, Vec<f32>)> {
        let p = format!("image_encoder.neck.convs.{k}.conv");
        Ok((reg.get(&format!("{p}.weight"), &[D, dim, 1, 1])?.to_vec(), reg.get(&format!("{p}.bias"), &[D])?.to_vec()))
    };
    if spec.stage_dims.get(s) != Some(&dim) {
        return Err(err(format!("stage {s} is {dim} wide")));
    }
    match s {
        0 | 1 => {
            let (wn, bn) = conv(reg, 3 - s)?;
            let (name, out, act) = if s == 0 { ("conv_s0", 32, I_S0) } else { ("conv_s1", 64, I_S1) };
            let ws = reg.get(&format!("sam_mask_decoder.{name}.weight"), &[out, D, 1, 1])?;
            let bs = reg.get(&format!("sam_mask_decoder.{name}.bias"), &[out])?;
            // (Ws·Wn)[o][i] = Σ_c Ws[o][c]·Wn[c][i];  b = Ws·bn + bs
            let mut w = vec![0f64; out * dim];
            let mut b = vec![0f64; out];
            for oo in 0..out {
                for cc in 0..D {
                    let a = f64::from(ws.get(oo * D + cc).copied().unwrap_or(0.0));
                    if let Some(bv) = b.get_mut(oo) {
                        *bv += a * f64::from(bn.get(cc).copied().unwrap_or(0.0));
                    }
                    for ii in 0..dim {
                        if let Some(wv) = w.get_mut(oo * dim + ii) {
                            *wv += a * f64::from(wn.get(cc * dim + ii).copied().unwrap_or(0.0));
                        }
                    }
                }
                if let Some(bv) = b.get_mut(oo) {
                    *bv += f64::from(bs.get(oo).copied().unwrap_or(0.0));
                }
            }
            let wid = reg.put(&format!("#{name}_fused"), w.iter().map(|v| *v as f32).collect());
            let bid = reg.put(&format!("#{name}_fused_bias"), b.iter().map(|v| *v as f32).collect());
            o.linear(B::A(x), (wid, bid), B::A(act), n, out, dim, Act::None, false);
        }
        2 => {
            let (w, b) = conv(reg, 1)?;
            let wb = (reg.put("#neck1_w", w), reg.put("#neck1_b", b));
            o.linear(B::A(x), wb, B::A(I_FEAT), n, D, dim, Act::None, false);
        }
        _ => {
            let (w, b) = conv(reg, 0)?;
            let wb = (reg.put("#neck0_w", w), reg.put("#neck0_b", b));
            o.linear(B::A(x), wb, B::A(I_L3), n, D, dim, Act::None, false);
            let side = (n as f64).sqrt() as usize;
            let r = Resample { c: D, h: side, w: side, oh: side * 2, ow: side * 2, mode: ResampleMode::Nearest };
            o.uses(B::A(I_TMP), FEAT_LEN);
            o.push(Op::Resample(r, B::A(I_L3), B::A(I_TMP)));
            o.push(Op::Add(Add { len: FEAT_LEN, scatter: None }, B::A(I_TMP), B::A(I_FEAT)));
        }
    }
    Ok(())
}

/// One `MultiScaleBlock` on activation `x` (`b.res`² tokens of `b.dim`); a pooled block leaves
/// its result in `x2` and swaps the two.
fn block(reg: &mut Reg, o: &mut Ops, p: &str, b: &GpuBlock, x: &mut usize, x2: &mut usize) -> Result<()> {
    let (dim, out, res) = (b.dim, b.dim_out, b.res);
    let hd = out / b.heads;
    let n = res * res;
    let n1 = reg.norm(&format!("{p}.norm1"), dim)?;
    let qkv = reg.linear(&format!("{p}.attn.qkv"), 3 * out, dim)?;
    let proj = reg.linear(&format!("{p}.attn.proj"), out, out)?;
    let ln = |rows: usize, gather: Option<WindowGrid>| LayerNorm { rows, c: dim, eps: 1e-6, gather, channel_major: false };
    // the residual path
    let (resid, out_res) = if dim != out {
        let pr = reg.linear(&format!("{p}.proj"), out, dim)?;
        o.norm(B::A(*x), n1, B::A(I_A), ln(n, None), false);
        o.linear(B::A(I_A), pr, B::A(I_B), n, out, dim, Act::None, false);
        if b.pool {
            let r = Resample { c: out, h: res, w: res, oh: res / 2, ow: res / 2, mode: ResampleMode::MaxPool(2) };
            o.uses(B::A(*x2), (res / 2) * (res / 2) * out);
            o.push(Op::Resample(r, B::A(I_B), B::A(*x2)));
            (*x2, res / 2)
        } else {
            return Err(err(format!("{p}: a widening block without pooling")));
        }
    } else {
        if b.pool {
            return Err(err(format!("{p}: a pooling block that keeps its width")));
        }
        (*x, res)
    };
    if b.window > 0 {
        let ws = b.window;
        let grid = WindowGrid { rh: res, rw: res, ws };
        let rows = grid.rows()?;
        let nw = rows / grid.n()?.max(1);
        o.norm(B::A(*x), n1, B::A(I_A), ln(rows, Some(grid)), true);
        o.linear(B::A(I_A), qkv, B::A(I_H), rows, 3 * out, dim, Act::None, false);
        let (q, nq, q_stride, q_off) = if b.pool {
            let nq = (ws / 2) * (ws / 2);
            o.uses(B::A(I_Q), nw * nq * out);
            o.push(Op::Pool2 { batch: nw, h: ws, w: ws, c: out, stride: 3 * out, off: 0, x: B::A(I_H), o: B::A(I_Q) });
            (I_Q, nq, out, 0)
        } else {
            (I_H, ws * ws, 3 * out, 0)
        };
        let att = Attention {
            groups: nw,
            nq,
            nkv: ws * ws,
            heads: b.heads,
            head_dim: hd,
            q_stride,
            kv_stride: 3 * out,
            out_stride: out,
            q_off,
            k_off: out,
            v_off: 2 * out,
            q_head: hd,
            kv_head: hd,
            out_head: hd,
            scale: 1.0 / (hd as f32).sqrt(),
        };
        o.uses(B::A(I_A), nw * nq * out);
        o.push(Op::Attn(att, B::A(q), B::A(I_H), B::A(I_A)));
        o.linear(B::A(I_A), proj, B::A(I_B), nw * nq, out, out, Act::None, false);
        let ows = if b.pool { ws / 2 } else { ws };
        let og = WindowGrid { rh: out_res, rw: out_res, ws: ows };
        o.push(Op::Add(Add { len: out_res * out_res * out, scatter: Some((og, out)) }, B::A(I_B), B::A(resid)));
    } else {
        if b.pool {
            return Err(err(format!("{p}: global attention with query pooling")));
        }
        o.norm(B::A(*x), n1, B::A(I_A), ln(n, None), false);
        o.linear(B::A(I_A), qkv, B::A(I_H), n, 3 * out, dim, Act::None, false);
        o.sdpa(b.heads, hd, n, n, (B::A(I_H), 0, 3 * out), (B::A(I_H), out, 3 * out), (B::A(I_H), 2 * out, 3 * out), (B::A(I_A), out), B::A(I_S));
        o.linear(B::A(I_A), proj, B::A(resid), n, out, out, Act::None, true);
    }
    // MLP
    let m = out_res * out_res;
    let n2 = reg.norm(&format!("{p}.norm2"), out)?;
    let fc1 = reg.linear(&format!("{p}.mlp.layers.0"), 4 * out, out)?;
    let fc2 = reg.linear(&format!("{p}.mlp.layers.1"), out, 4 * out)?;
    o.norm(B::A(resid), n2, B::A(I_A), LayerNorm { rows: m, c: out, eps: 1e-6, gather: None, channel_major: false }, false);
    o.mlp(B::A(I_A), B::A(I_H), B::A(resid), fc1, fc2, m, out, 4 * out, Act::GeluErf);
    if resid != *x {
        std::mem::swap(x, x2);
    }
    Ok(())
}

// Memory encoder activations.
const E_MASK: usize = 0;
const E_COL: usize = 1;
const E_A: usize = 2;
const E_B: usize = 3;
const E_FEAT: usize = 4;
const E_X: usize = 5;
const E_M: usize = 6;
const E_H: usize = 7;
const E_OUT: usize = 8;

fn plan_mem_encoder(reg: &mut Reg, spec: &Sam2GpuSpec) -> Result<Ops> {
    let mut o = Ops::default();
    let m = "memory_encoder";
    let chans = [1usize, 4, 16, 64, 256];
    let mut side = MASK_SIDE;
    let mut src = E_MASK;
    o.uses(B::A(E_MASK), MASK_SIDE * MASK_SIDE);
    for i in 0..4 {
        let (Some(ci), Some(co)) = (chans.get(i).copied(), chans.get(i + 1).copied()) else { break };
        let cv = reg.conv_k(&format!("{m}.mask_downsampler.encoder.{}", 3 * i), co, ci, 3)?;
        let im = Im2col { cin: ci, h: side, w: side, k: 3, stride: 2, pad: 1, chw: false };
        let (oh, ow) = im.out_size()?;
        o.uses(B::A(src), side * side * ci);
        o.uses(B::A(E_COL), oh * ow * 9 * ci);
        o.push(Op::Im2col(im, B::A(src), B::A(E_COL)));
        o.linear(B::A(E_COL), cv, B::A(E_A), oh * ow, co, 9 * ci, Act::None, false);
        let nb = reg.norm(&format!("{m}.mask_downsampler.encoder.{}", 3 * i + 1), co)?;
        o.norm(B::A(E_A), nb, B::A(E_B), LayerNorm { rows: oh * ow, c: co, eps: 1e-6, gather: None, channel_major: false }, false);
        o.push(Op::Unary(UnaryOp::GeluErf, oh * ow * co, B::A(E_B)));
        src = E_B;
        side = oh;
    }
    if side != 64 {
        return Err(err("the mask downsampler does not end at 64 × 64"));
    }
    let c12 = reg.conv1(&format!("{m}.mask_downsampler.encoder.12"), D, D)?;
    o.linear(B::A(E_B), c12, B::A(E_M), TOKENS, D, D, Act::None, false);
    let pix = reg.conv1(&format!("{m}.pix_feat_proj"), D, D)?;
    o.uses(B::A(E_FEAT), FEAT_LEN);
    o.linear(B::A(E_FEAT), pix, B::A(E_X), TOKENS, D, D, Act::None, false);
    o.push(Op::Add(Add { len: FEAT_LEN, scatter: None }, B::A(E_M), B::A(E_X)));
    for l in 0..spec.fuser_layers {
        let p = format!("{m}.fuser.layers.{l}");
        let dw = reg.get(&format!("{p}.dwconv.weight"), &[D, 1, 7, 7])?.to_vec();
        let dw = reg.put(&format!("{p}.dwconv.weight#dw"), dw);
        let dwb = reg.w(&format!("{p}.dwconv.bias"), &[D])?;
        let s = Depthwise { c: D, h: 64, w: 64, k: 7, stride: 1, pad: 3, gelu: false };
        o.uses(B::A(E_A), FEAT_LEN);
        o.push(Op::Dw(s, B::A(E_X), dw, dwb, B::A(E_A)));
        let nb = reg.norm(&format!("{p}.norm"), D)?;
        o.norm(B::A(E_A), nb, B::A(E_B), LayerNorm { rows: TOKENS, c: D, eps: 1e-6, gather: None, channel_major: false }, false);
        let fc1 = reg.linear(&format!("{p}.pwconv1"), 4 * D, D)?;
        // layer scale folded into pwconv2: (γ ⊙ (W x + b)) = (diag γ W) x + γ ⊙ b
        let gamma = reg.get(&format!("{p}.gamma"), &[D])?.to_vec();
        let w2 = reg.get(&format!("{p}.pwconv2.weight"), &[D, 4 * D])?;
        let b2 = reg.get(&format!("{p}.pwconv2.bias"), &[D])?;
        let w2: Vec<f32> = w2.iter().enumerate().map(|(i, v)| v * gamma.get(i / (4 * D)).copied().unwrap_or(0.0)).collect();
        let b2: Vec<f32> = b2.iter().zip(&gamma).map(|(b, g)| b * g).collect();
        let fc2 = (reg.put(&format!("{p}.pwconv2#gamma"), w2), reg.put(&format!("{p}.pwconv2.bias#gamma"), b2));
        o.mlp(B::A(E_B), B::A(E_H), B::A(E_X), fc1, fc2, TOKENS, D, 4 * D, Act::GeluErf);
    }
    let op = reg.conv1(&format!("{m}.out_proj"), MEM, D)?;
    o.linear(B::A(E_X), op, B::A(E_OUT), TOKENS, MEM, D, Act::None, false);
    o.mark("memory", E_OUT, TOKENS * MEM);
    Ok(o)
}

// Memory attention activations.
const M_CUR: usize = 0;
const M_MEM: usize = 1;
const M_MPOS: usize = 2;
const M_X: usize = 3;
const M_T: usize = 4;
const M_QKV: usize = 5;
const M_KM: usize = 6;
const M_K: usize = 7;
const M_V: usize = 8;
const M_S: usize = 9;
const M_O: usize = 10;
const M_Q: usize = 11;
const M_FFN: usize = 12;
const M_OUT: usize = 13;

/// The memory attention's uploaded weights.
#[derive(Clone, Debug)]
struct MemIds {
    layers: Vec<MemLayer>,
    norm: (B, B),
    vpos: B,
    cos: B,
    sin: B,
    ffn: usize,
}

#[derive(Clone, Copy, Debug)]
struct MemLayer {
    n1: (B, B),
    n2: (B, B),
    n3: (B, B),
    sa_qkv: (B, B),
    sa_out: (B, B),
    ca_q: (B, B),
    ca_k: (B, B),
    ca_v: (B, B),
    ca_out: (B, B),
    l1: (B, B),
    l2: (B, B),
}

fn register_mem_attn(reg: &mut Reg, spec: &Sam2GpuSpec) -> Result<MemIds> {
    let mut layers = Vec::with_capacity(spec.mem_layers);
    for l in 0..spec.mem_layers {
        let p = format!("memory_attention.layers.{l}");
        // self-attention q, k, v fused into one [768][256] layer
        let mut w = Vec::with_capacity(3 * D * D);
        let mut b = Vec::with_capacity(3 * D);
        for a in ["q", "k", "v"] {
            w.extend_from_slice(reg.get(&format!("{p}.self_attn.{a}_proj.weight"), &[D, D])?);
            b.extend_from_slice(reg.get(&format!("{p}.self_attn.{a}_proj.bias"), &[D])?);
        }
        let sa_qkv = (reg.put(&format!("{p}.self_attn#qkv"), w), reg.put(&format!("{p}.self_attn#qkv_b"), b));
        layers.push(MemLayer {
            n1: reg.norm(&format!("{p}.norm1"), D)?,
            n2: reg.norm(&format!("{p}.norm2"), D)?,
            n3: reg.norm(&format!("{p}.norm3"), D)?,
            sa_qkv,
            sa_out: reg.linear(&format!("{p}.self_attn.out_proj"), D, D)?,
            ca_q: reg.linear(&format!("{p}.cross_attn_image.q_proj"), D, D)?,
            ca_k: reg.linear(&format!("{p}.cross_attn_image.k_proj"), D, MEM)?,
            ca_v: reg.linear(&format!("{p}.cross_attn_image.v_proj"), D, MEM)?,
            ca_out: reg.linear(&format!("{p}.cross_attn_image.out_proj"), D, D)?,
            l1: reg.linear(&format!("{p}.linear1"), spec.mem_ffn, D)?,
            l2: reg.linear(&format!("{p}.linear2"), D, spec.mem_ffn)?,
        });
    }
    Ok(MemIds {
        layers,
        norm: reg.norm("memory_attention.norm", D)?,
        vpos: reg.put("#vision_pos", spec.vision_pos.clone()),
        cos: reg.put("#rope_cos", spec.rope_cos.clone()),
        sin: reg.put("#rope_sin", spec.rope_sin.clone()),
        ffn: spec.mem_ffn,
    })
}

/// The memory attention over `n` memory tokens of which the last `ptr` are object pointers.
fn plan_mem_attn(ids: &MemIds, n: usize, ptr: usize) -> Result<Ops> {
    if n == 0 || n > MAX_MEMORY_TOKENS || ptr > n || !(n - ptr).is_multiple_of(TOKENS) {
        return Err(err(format!("a memory of {n} tokens ({ptr} pointer tokens)")));
    }
    let mut o = Ops::default();
    let ln = |rows: usize| LayerNorm { rows, c: D, eps: 1e-5, gather: None, channel_major: false };
    // the score chunk's size depends on the memory length: size it for the budget
    o.uses(B::A(M_S), SCORE_BUDGET);
    o.uses(B::A(M_CUR), FEAT_LEN);
    o.uses(B::A(M_MEM), n * MEM);
    o.uses(B::A(M_MPOS), n * MEM);
    o.uses(B::A(M_X), FEAT_LEN);
    o.uses(B::A(M_KM), n * MEM);
    o.push(Op::Ew { len: FEAT_LEN, blen: FEAT_LEN, alpha: 1.0, beta: 0.1, a: B::A(M_CUR), b: ids.vpos, o: B::A(M_X) });
    o.push(Op::Ew { len: n * MEM, blen: n * MEM, alpha: 1.0, beta: 1.0, a: B::A(M_MEM), b: B::A(M_MPOS), o: B::A(M_KM) });
    let rope = |o: &mut Ops, x: usize, rows: usize, stride: usize, off: usize| {
        o.uses(B::A(x), rows * stride);
        o.push(Op::Rope { rows, pairs: D / 2, stride, off, x: B::A(x) });
    };
    for l in &ids.layers {
        // self-attention (RoPE on q and k)
        o.norm(B::A(M_X), l.n1, B::A(M_T), ln(TOKENS), false);
        o.linear(B::A(M_T), l.sa_qkv, B::A(M_QKV), TOKENS, 3 * D, D, Act::None, false);
        rope(&mut o, M_QKV, TOKENS, 3 * D, 0);
        rope(&mut o, M_QKV, TOKENS, 3 * D, D);
        o.sdpa(1, D, TOKENS, TOKENS, (B::A(M_QKV), 0, 3 * D), (B::A(M_QKV), D, 3 * D), (B::A(M_QKV), 2 * D, 3 * D), (B::A(M_O), D), B::A(M_S));
        o.linear(B::A(M_O), l.sa_out, B::A(M_X), TOKENS, D, D, Act::None, true);
        // cross-attention to the memory (RoPE on q and the frame tokens of k)
        o.norm(B::A(M_X), l.n2, B::A(M_T), ln(TOKENS), false);
        o.linear(B::A(M_T), l.ca_q, B::A(M_Q), TOKENS, D, D, Act::None, false);
        o.linear(B::A(M_KM), l.ca_k, B::A(M_K), n, D, MEM, Act::None, false);
        o.linear(B::A(M_MEM), l.ca_v, B::A(M_V), n, D, MEM, Act::None, false);
        rope(&mut o, M_Q, TOKENS, D, 0);
        if n > ptr {
            rope(&mut o, M_K, n - ptr, D, 0);
        }
        o.sdpa(1, D, TOKENS, n, (B::A(M_Q), 0, D), (B::A(M_K), 0, D), (B::A(M_V), 0, D), (B::A(M_O), D), B::A(M_S));
        o.linear(B::A(M_O), l.ca_out, B::A(M_X), TOKENS, D, D, Act::None, true);
        // feed-forward
        o.norm(B::A(M_X), l.n3, B::A(M_T), ln(TOKENS), false);
        o.mlp(B::A(M_T), B::A(M_FFN), B::A(M_X), l.l1, l.l2, TOKENS, D, ids.ffn, Act::Relu);
    }
    o.norm(B::A(M_X), ids.norm, B::A(M_OUT), ln(TOKENS), false);
    o.mark("out", M_OUT, FEAT_LEN);
    Ok(o)
}

/// SAM 2.1's heavy parts on a wgpu device, weights resident.
pub struct Sam2Gpu {
    exec: Exec,
    nn: NnKernels,
    weights: Vec<wgpu::Buffer>,
    image_acts: Vec<wgpu::Buffer>,
    image_steps: Vec<NnStep>,
    image_marks: Vec<Mark>,
    enc_acts: Vec<wgpu::Buffer>,
    enc_steps: Vec<NnStep>,
    mem_ids: MemIds,
    mem_acts: Vec<wgpu::Buffer>,
    mem_plans: Mutex<HashMap<(usize, usize), Arc<Vec<NnStep>>>>,
    /// The mask decoder, when its weights were given (see [`decoder`]).
    decoder: Option<decoder::Sam2GpuDecoder>,
    name: String,
    discrete: bool,
}

impl std::fmt::Debug for Sam2Gpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sam2Gpu({}, {} image dispatches)", self.name, self.image_steps.len())
    }
}

fn alloc(device: &wgpu::Device, max: u64, need: &[usize], label: &str) -> Result<Vec<wgpu::Buffer>> {
    let mut v = Vec::with_capacity(need.len());
    for (i, n) in need.iter().enumerate() {
        let bytes = ((*n).max(4) as u64).saturating_mul(4);
        if bytes > max {
            return Err(err(format!("{label} buffer {i}: {bytes} bytes are over the GPU's {max}-byte binding limit")));
        }
        v.push(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }
    Ok(v)
}

fn steps(nn: &NnKernels, ops: &[Op], acts: &[wgpu::Buffer], weights: &[wgpu::Buffer], rope: (B, B)) -> Result<Vec<NnStep>> {
    let buf = |b: B| -> Result<&wgpu::Buffer> {
        match b {
            B::A(i) => acts.get(i),
            B::W(i) => weights.get(i),
        }
        .ok_or_else(|| err("internal: a planned buffer is missing"))
    };
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        out.push(match op {
            Op::Bmm(s, a, w, bias, o) => {
                let bias = match bias {
                    Some(b) => Some(buf(*b)?),
                    None => None,
                };
                nn.bmm(s, buf(*a)?, buf(*w)?, bias, buf(*o)?)?
            }
            Op::Im2col(s, x, o) => nn.im2col(s, buf(*x)?, buf(*o)?)?,
            Op::Dw(s, x, w, b, o) => nn.depthwise(s, buf(*x)?, buf(*w)?, buf(*b)?, buf(*o)?)?,
            Op::Norm(s, z, x, g, b, o) => nn.layer_norm_with(s, *z, buf(*x)?, buf(*g)?, buf(*b)?, buf(*o)?)?,
            Op::Attn(s, q, kv, o) => nn.attention(s, buf(*q)?, buf(*kv)?, None, buf(*o)?)?,
            Op::Add(s, src, x) => nn.add(s, buf(*src)?, buf(*x)?)?,
            Op::Unary(u, len, x) => nn.unary(*u, *len, buf(*x)?)?,
            Op::Softmax(rows, n, x) => nn.softmax(*rows, *n, buf(*x)?)?,
            Op::Resample(s, x, o) => nn.resample(s, buf(*x)?, buf(*o)?)?,
            Op::Pool2 { batch, h, w, c, stride, off, x, o } => nn.pool2(*batch, *h, *w, *c, *stride, *off, buf(*x)?, buf(*o)?)?,
            Op::Rope { rows, pairs, stride, off, x } => nn.rope(*rows, *pairs, *stride, *off, TOKENS, buf(*x)?, buf(rope.0)?, buf(rope.1)?)?,
            Op::Ew { len, blen, alpha, beta, a, b, o } => nn.ew(*len, *blen, *alpha, *beta, buf(*a)?, buf(*b)?, buf(*o)?)?,
        });
    }
    Ok(out)
}

impl Sam2Gpu {
    /// A device of its own on the adapter Settings ▸ Playback ▸ Graphics device picks
    /// ([`crate::adapter_pick`]), else the high-performance one (CPU adapters are refused), then
    /// [`Sam2Gpu::new`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(spec: &Sam2GpuSpec, weights: &HashMap<String, Weight>) -> Result<Self> {
        let (device, queue, info) = open_device("filmcraft magic mask sam2")?;
        let mut gpu = Self::new(&device, &queue, spec, weights)?;
        gpu.name = format!("{} ({:?})", info.name, info.backend);
        gpu.discrete = info.device_type == wgpu::DeviceType::DiscreteGpu;
        Ok(gpu)
    }

    /// Plan the passes on `device`, upload the weights and allocate the buffers.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, spec: &Sam2GpuSpec, weights: &HashMap<String, Weight>) -> Result<Self> {
        spec.validate()?;
        let mut reg = Reg { src: weights, uploads: Vec::new(), ids: HashMap::new() };
        let image = plan_image(&mut reg, spec)?;
        let enc = plan_mem_encoder(&mut reg, spec)?;
        let mem_ids = register_mem_attn(&mut reg, spec)?;
        let mem_max = plan_mem_attn(&mem_ids, MAX_MEMORY_TOKENS, 4 * 32)?;
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let val = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let built = Self::build(device, queue, reg, image, enc, mem_ids, mem_max);
        let index = queue.submit(std::iter::empty());
        let waited = device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(GPU_WAIT) });
        if let Some(e) = pollster::block_on(val.pop()) {
            return Err(err(format!("kernels: {e}")));
        }
        if let Some(e) = pollster::block_on(oom.pop()) {
            return Err(err(format!("out of GPU memory: {e}")));
        }
        waited.map_err(|e| err(format!("upload: {e}")))?;
        let mut built = built?;
        if weights.contains_key(decoder::PROBE) {
            // the decoder is optional: without it (or if it can't be built) the CPU decodes
            match decoder::Sam2GpuDecoder::new(device, queue, weights) {
                Ok(d) => built.decoder = Some(d),
                Err(e) => log::warn!("Magic Mask (SAM 2): the mask decoder stays on the CPU: {e}"),
            }
        }
        Ok(built)
    }

    fn build(device: &wgpu::Device, queue: &wgpu::Queue, reg: Reg, image: Ops, enc: Ops, mem_ids: MemIds, mem_max: Ops) -> Result<Self> {
        let nn = NnKernels::new(device);
        let max = nn.max_binding();
        let mut weights = Vec::with_capacity(reg.uploads.len());
        for data in &reg.uploads {
            if (data.len() as u64).saturating_mul(4) > max {
                return Err(err("a weight is over the GPU's binding limit"));
            }
            let contents: &[u8] = if data.is_empty() { &[0; 16] } else { bytemuck::cast_slice(data) };
            weights.push(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sam2-weight"),
                contents,
                usage: wgpu::BufferUsages::STORAGE,
            }));
        }
        let rope = (mem_ids.cos, mem_ids.sin);
        let image_acts = alloc(device, max, &image.need, "sam2-image")?;
        let image_steps = steps(&nn, &image.ops, &image_acts, &weights, rope)?;
        let enc_acts = alloc(device, max, &enc.need, "sam2-memenc")?;
        let enc_steps = steps(&nn, &enc.ops, &enc_acts, &weights, rope)?;
        let mem_acts = alloc(device, max, &mem_max.need, "sam2-memattn")?;
        // check the largest plan binds
        let _ = steps(&nn, &mem_max.ops, &mem_acts, &weights, rope)?;
        Ok(Self {
            exec: Exec::new(device, queue),
            nn,
            weights,
            image_acts,
            image_steps,
            image_marks: image.marks,
            enc_acts,
            enc_steps,
            mem_ids,
            mem_acts,
            mem_plans: Mutex::new(HashMap::new()),
            decoder: None,
            name: String::from("GPU"),
            discrete: false,
        })
    }

    /// The adapter's name, for logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Opened on a discrete GPU (the one class fast enough for SAM 2 Large by default).
    pub fn discrete(&self) -> bool {
        self.discrete
    }

    /// The mask decoder on this device, when its weights were given at [`Sam2Gpu::new`].
    pub fn decoder(&self) -> Option<&decoder::Sam2GpuDecoder> {
        self.decoder.as_ref()
    }

    /// The image encoder: (s0 256²×32, s1 128²×64, feat 64²×256), HWC.
    pub fn image(&self, input: &[f32]) -> Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        if input.len() != INPUT_LEN {
            return Err(err(format!("an input of {} values, expected {INPUT_LEN}", input.len())));
        }
        let reads = [(I_S0, S0_LEN), (I_S1, S1_LEN), (I_FEAT, FEAT_LEN)];
        let mut out = self.exec.run(&self.image_acts, &[(I_INP, input)], &[(&self.image_steps, &reads)])?;
        let feat = out.pop().ok_or_else(|| err("no output"))?;
        let s1 = out.pop().ok_or_else(|| err("no output"))?;
        let s0 = out.pop().ok_or_else(|| err("no output"))?;
        Ok((s0, s1, feat))
    }

    /// Every stage of the image encoder (see [`IMAGE_STAGES`]), for parity tests.
    pub fn image_stages(&self, input: &[f32]) -> Result<Vec<(String, Vec<f32>)>> {
        if input.len() != INPUT_LEN {
            return Err(err(format!("an input of {} values, expected {INPUT_LEN}", input.len())));
        }
        let mut segs: Vec<(&[NnStep], Vec<(usize, usize)>)> = Vec::new();
        let mut from = 0;
        for m in &self.image_marks {
            let end = m.end.min(self.image_steps.len());
            segs.push((self.image_steps.get(from..end).unwrap_or(&[]), vec![(m.act, m.len)]));
            from = end;
        }
        let refs: Vec<(&[NnStep], &[(usize, usize)])> = segs.iter().map(|(s, r)| (*s, r.as_slice())).collect();
        let outs = self.exec.run(&self.image_acts, &[(I_INP, input)], &refs)?;
        Ok(self.image_marks.iter().map(|m| m.name.clone()).zip(outs).collect())
    }

    /// The memory attention (see `filmcraft_mask::sam2::Sam2Compute::memory_attention`).
    pub fn memory_attention(&self, feat: &[f32], memory: &[f32], memory_pos: &[f32], ptr_tokens: usize) -> Result<Vec<f32>> {
        let n = memory.len() / MEM;
        if feat.len() != FEAT_LEN || memory.len() != n * MEM || memory_pos.len() != memory.len() || n == 0 {
            return Err(err(format!("memory attention inputs of {} / {} / {} values", feat.len(), memory.len(), memory_pos.len())));
        }
        let plan = {
            let mut cache = self.mem_plans.lock().unwrap_or_else(PoisonError::into_inner);
            match cache.get(&(n, ptr_tokens)) {
                Some(p) => p.clone(),
                None => {
                    let ops = plan_mem_attn(&self.mem_ids, n, ptr_tokens)?;
                    let st = Arc::new(steps(&self.nn, &ops.ops, &self.mem_acts, &self.weights, (self.mem_ids.cos, self.mem_ids.sin))?);
                    if cache.len() >= MAX_CACHED_PLANS {
                        cache.clear();
                    }
                    cache.insert((n, ptr_tokens), st.clone());
                    st
                }
            }
        };
        let mut out = self.exec.run(&self.mem_acts, &[(M_CUR, feat), (M_MEM, memory), (M_MPOS, memory_pos)], &[(&plan, &[(M_OUT, FEAT_LEN)])])?;
        out.pop().ok_or_else(|| err("no output"))
    }

    /// The memory encoder: `feat` (4096 × 256) and the scaled mask (1024 × 1024) → 4096 × 64.
    pub fn memory_encoder(&self, feat: &[f32], mask: &[f32]) -> Result<Vec<f32>> {
        if feat.len() != FEAT_LEN || mask.len() != MASK_SIDE * MASK_SIDE {
            return Err(err(format!("memory encoder inputs of {} / {} values", feat.len(), mask.len())));
        }
        let mut out = self.exec.run(&self.enc_acts, &[(E_FEAT, feat), (E_MASK, mask)], &[(&self.enc_steps, &[(E_OUT, TOKENS * MEM)])])?;
        out.pop().ok_or_else(|| err("no output"))
    }
}

/// A device and queue with the read-back buffer, held for a whole call (the caller's activation
/// buffers are shared between calls).
struct Exec {
    device: wgpu::Device,
    queue: wgpu::Queue,
    read: Mutex<Option<wgpu::Buffer>>,
}

impl Exec {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self { device: device.clone(), queue: queue.clone(), read: Mutex::new(None) }
    }

    /// Uploads `inputs`, runs each segment's steps and reads its `(activation, len)` outputs.
    fn run(&self, acts: &[wgpu::Buffer], inputs: &[(usize, &[f32])], segments: &[(&[NnStep], &[(usize, usize)])]) -> Result<Vec<Vec<f32>>> {
        let mut guard = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        if inputs.iter().any(|(_, v)| v.iter().any(|x| !x.is_finite())) {
            return Err(err("an input has non-finite values"));
        }
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let val = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = (|| -> Result<Vec<Vec<f32>>> {
            for (a, data) in inputs {
                let b = acts.get(*a).ok_or_else(|| err("internal: no input buffer"))?;
                if (data.len() as u64).saturating_mul(4) > b.size() {
                    return Err(err("an input is larger than its buffer"));
                }
                self.queue.write_buffer(b, 0, bytemuck::cast_slice(data));
            }
            let total: u64 = segments.iter().flat_map(|(_, r)| r.iter()).map(|(_, n)| (*n as u64).saturating_mul(4)).sum();
            if total == 0 {
                return Err(err("nothing to read back"));
            }
            let read = match guard.take().filter(|b| b.size() >= total) {
                Some(b) => b,
                None => self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("sam2-read"),
                    size: total,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            };
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sam2") });
            let mut at = 0u64;
            for (st, reads) in segments {
                {
                    let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("sam2"), timestamp_writes: None });
                    for s in st.iter() {
                        s.record(&mut pass);
                    }
                }
                for (a, n) in reads.iter() {
                    let src = acts.get(*a).ok_or_else(|| err("internal: no output buffer"))?;
                    let bytes = (*n as u64).saturating_mul(4);
                    if bytes > src.size() {
                        return Err(err("a read-back is larger than its buffer"));
                    }
                    enc.copy_buffer_to_buffer(src, 0, &read, at, bytes);
                    at += bytes;
                }
            }
            let index = self.queue.submit([enc.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            read.slice(..total).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            self.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(GPU_WAIT) }).map_err(|e| err(format!("GPU wait: {e}")))?;
            match rx.recv_timeout(GPU_WAIT) {
                Ok(r) => r.map_err(|e| err(format!("read-back: {e}")))?,
                Err(e) => return Err(err(format!("read-back never completed: {e}"))),
            }
            let mut outs = Vec::new();
            {
                let data = read.slice(..total).get_mapped_range().map_err(|e| err(format!("read-back: {e}")))?;
                let all: Vec<f32> = data.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
                let mut i = 0usize;
                for (_, reads) in segments {
                    for (_, n) in reads.iter() {
                        outs.push(all.get(i..i + n).map(<[f32]>::to_vec).ok_or_else(|| err("short read-back"))?);
                        i += n;
                    }
                }
            }
            read.unmap();
            *guard = Some(read);
            Ok(outs)
        })();
        if let Some(e) = pollster::block_on(val.pop()) {
            return Err(err(format!("validation: {e}")));
        }
        if let Some(e) = pollster::block_on(oom.pop()) {
            return Err(err(format!("out of GPU memory: {e}")));
        }
        let outs = result?;
        if outs.iter().flatten().any(|v| !v.is_finite()) {
            return Err(err("the GPU produced non-finite values"));
        }
        Ok(outs)
    }
}

/// A device of its own on the adapter Settings ▸ Playback ▸ Graphics device picks
/// ([`crate::adapter_pick`]), else the high-performance one (CPU adapters are refused).
#[cfg(not(target_arch = "wasm32"))]
fn open_device(label: &'static str) -> Result<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    if wgpu::Instance::enabled_backend_features().is_empty() {
        return Err(err("this build has no GPU backend"));
    }
    let instance = wgpu::Instance::default();
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
        label: Some(label),
        required_limits: wgpu::Limits {
            max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
            max_buffer_size: limits.max_buffer_size,
            ..wgpu::Limits::default()
        },
        ..Default::default()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&desc)).map_err(|e| err(format!("GPU device: {e}")))?;
    device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log::warn!("Magic Mask (SAM 2) GPU error: {e}")));
    device.set_device_lost_callback(|reason, msg| log::warn!("Magic Mask (SAM 2) GPU device lost ({reason:?}): {msg}"));
    Ok((device, queue, info))
}

#[path = "sam2_decoder.rs"]
pub mod decoder;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_specs_and_weights_are_refused_before_any_gpu_work() {
        let spec = Sam2GpuSpec {
            embed_dim: 16,
            blocks: vec![GpuBlock { dim: 16, dim_out: 16, heads: 1, window: 8, pool: false, res: 256 }],
            stage_ends: [0, 0, 0, 0],
            stage_dims: [16, 32, 64, 128],
            mem_layers: 1,
            mem_ffn: 32,
            fuser_layers: 1,
            hiera_pos: vec![0.0; 256 * 256 * 16],
            vision_pos: vec![0.0; FEAT_LEN],
            rope_cos: vec![0.0; TOKENS * 128],
            rope_sin: vec![0.0; TOKENS * 128],
        };
        spec.validate().unwrap();
        let w = HashMap::new();
        let mut reg = Reg { src: &w, uploads: Vec::new(), ids: HashMap::new() };
        assert!(plan_image(&mut reg, &spec).unwrap_err().0.contains("missing"));
        assert!(Sam2GpuSpec { hiera_pos: vec![], ..spec.clone() }.validate().is_err());
        assert!(Sam2GpuSpec { stage_dims: [16, 16, 16, 16], ..spec.clone() }.validate().is_err());
        let bad = GpuBlock { heads: 0, ..spec.blocks[0] };
        assert!(Sam2GpuSpec { blocks: vec![bad], ..spec.clone() }.validate().is_err());
        let ids = MemIds { layers: vec![], norm: (B::W(0), B::W(0)), vpos: B::W(0), cos: B::W(0), sin: B::W(0), ffn: 8 };
        assert!(plan_mem_attn(&ids, 0, 0).is_err());
        assert!(plan_mem_attn(&ids, TOKENS + 3, 4).is_err());
        assert!(plan_mem_attn(&ids, MAX_MEMORY_TOKENS + 4, 4).is_err());
        assert!(plan_mem_attn(&ids, TOKENS + 4, 4).is_ok());
    }
}
