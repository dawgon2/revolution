//! SAM 2.1's mask decoder on the GPU (fork LO.6): the two-way transformer, the upscaling with
//! the high-resolution skips, the hypernetwork / IoU / object-score heads, built from the same
//! [`crate::nn`] ops as the encoder. `filmcraft-mask`'s candle code (`sam2::heads`) is the
//! reference it is tested against and the fallback; the prompt encoder (a few hundred values)
//! stays there: the caller passes the decoder's input tokens (output tokens + sparse prompt
//! embeddings) and the dense prompt embedding.
//!
//! Ported from github.com/facebookresearch/sam2 (`mask_decoder.py`, `transformer.py`),
//! Apache-2.0, Copyright (c) Meta Platforms, Inc. and affiliates; see `NOTICE`.
//!
//! Layouts are HWC / token-major as everywhere in [`super`]. The plan depends on the number of
//! tokens and on whether the dense embedding is one vector (no mask prompt) or a whole grid, so
//! plans are cached per (tokens, dense length) over buffers sized for the largest.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use wgpu::util::DeviceExt;

use super::{B, D, Exec, FEAT_LEN, GPU_WAIT, Op, Ops, Reg, Result, S0_LEN, S1_LEN, TOKENS, Weight, alloc, err, steps};
use crate::nn::{Act, Bmm, LayerNorm, NnKernels, NnStep, Strided, UnaryOp};

/// A weight whose presence means the decoder's weights were given.
pub const PROBE: &str = "sam_mask_decoder.transformer.layers.0.self_attn.q_proj.weight";
/// Most decoder input tokens: 6 output tokens + 64 points + 2 box corners + the padding point.
pub const MAX_TOKENS: usize = 80;
/// Mask tokens (masks / IoU predictions).
pub const MASKS: usize = 4;
/// Side of a low-resolution mask.
pub const MASK_SIDE: usize = 256;
const MASK_LEN: usize = MASK_SIDE * MASK_SIDE;
/// Attention heads everywhere in the decoder.
const HEADS: usize = 8;
/// The cross attentions' inner width (`attention_downsample_rate` 2).
const HALF: usize = D / 2;
const MLP_DIM: usize = 2048;
/// Plans kept (one per token count and dense length).
const MAX_CACHED_PLANS: usize = 32;

/// What the decoder gives for one prompt: every mask token's mask (4 × 256 × 256), the IoU
/// predictions (sigmoid, 4), the object score logit, and the mask tokens after the transformer
/// (4 × 256, for the object pointer).
#[derive(Clone, Debug, PartialEq)]
pub struct DecoderOut {
    pub masks: Vec<f32>,
    pub ious: Vec<f32>,
    pub score: f32,
    pub tokens: Vec<f32>,
}

// Activations.
const X_PIX: usize = 0;
const X_DENSE: usize = 1;
const X_TOK: usize = 2;
const X_S0: usize = 3;
const X_S1: usize = 4;
const X_K: usize = 5;
const X_K2: usize = 6;
const X_KK: usize = 7;
const X_Q: usize = 8;
const X_Q2: usize = 9;
const X_QQ: usize = 10;
const X_AQ: usize = 11;
const X_KV: usize = 12;
const X_AO: usize = 13;
const X_S: usize = 14;
const X_H: usize = 15;
const X_U: usize = 16;
const X_U2: usize = 17;
const X_V: usize = 18;
const X_V2: usize = 19;
const X_HY0: usize = 20;
const X_HY1: usize = 21;
const X_HYP: usize = 22;
const X_MASKS: usize = 23;
const X_IOU: usize = 24;
const X_SCORE: usize = 25;
const X_HS: usize = 26;

type Wb = (B, B);

#[derive(Clone, Copy, Debug)]
struct AttnIds {
    q: Wb,
    k: Wb,
    v: Wb,
    out: Wb,
    inner: usize,
}

#[derive(Clone, Copy, Debug)]
struct LayerIds {
    self_attn: AttnIds,
    t2i: AttnIds,
    i2t: AttnIds,
    mlp0: Wb,
    mlp1: Wb,
    norms: [Wb; 4],
}

/// The decoder's uploaded weights.
#[derive(Clone, Debug)]
struct DecIds {
    layers: Vec<LayerIds>,
    final_attn: AttnIds,
    norm_final: Wb,
    /// The dense positional encoding (4096 × 256).
    pe: B,
    /// The two upscaling steps: per output row `dy` the [2·cout][cin] matrix and its bias.
    up0: [Wb; 2],
    up_norm: Wb,
    up1: [Wb; 2],
    hyper: Vec<[Wb; 3]>,
    iou: [Wb; 3],
    score: [Wb; 3],
}

fn attn_ids(reg: &mut Reg, p: &str, inner: usize) -> Result<AttnIds> {
    Ok(AttnIds {
        q: reg.linear(&format!("{p}.q_proj"), inner, D)?,
        k: reg.linear(&format!("{p}.k_proj"), inner, D)?,
        v: reg.linear(&format!("{p}.v_proj"), inner, D)?,
        out: reg.linear(&format!("{p}.out_proj"), D, inner)?,
        inner,
    })
}

/// `ConvTranspose2d(k 2, s 2)` weights `[cin][cout][2][2]` as two matrices (one per output row
/// `dy`) of `[dx][cout]` rows × `cin`, and the bias repeated for both `dx`.
fn up_ids(reg: &mut Reg, p: &str, cin: usize, cout: usize) -> Result<[Wb; 2]> {
    let w = reg.get(&format!("{p}.weight"), &[cin, cout, 2, 2])?;
    let b = reg.get(&format!("{p}.bias"), &[cout])?;
    let bias: Vec<f32> = b.iter().chain(b.iter()).copied().collect();
    let mut out = [(B::W(0), B::W(0)); 2];
    for (dy, slot) in out.iter_mut().enumerate() {
        let mut m = Vec::with_capacity(2 * cout * cin);
        for dx in 0..2 {
            for co in 0..cout {
                for ci in 0..cin {
                    m.push(w.get(((ci * cout + co) * 2 + dy) * 2 + dx).copied().unwrap_or(0.0));
                }
            }
        }
        *slot = (reg.put(&format!("{p}#dy{dy}"), m), reg.put(&format!("{p}#bias2"), bias.clone()));
    }
    Ok(out)
}

fn mlp3(reg: &mut Reg, p: &str, out: usize) -> Result<[Wb; 3]> {
    Ok([reg.linear(&format!("{p}.layers.0"), D, D)?, reg.linear(&format!("{p}.layers.1"), D, D)?, reg.linear(&format!("{p}.layers.2"), out, D)?])
}

/// `PositionEmbeddingRandom` over the 64 × 64 grid, HWC (the same f32 arithmetic as the CPU's
/// `dense_pe`, which writes CHW).
fn dense_pe(gauss: &[f32]) -> Vec<f32> {
    let side = 64;
    let feats = D / 2;
    let mut out = vec![0f32; side * side * D];
    for y in 0..side {
        for x in 0..side {
            let cx = 2.0 * ((x as f32 + 0.5) / side as f32) - 1.0;
            let cy = 2.0 * ((y as f32 + 0.5) / side as f32) - 1.0;
            for f in 0..feats {
                let g0 = gauss.get(f).copied().unwrap_or(0.0);
                let g1 = gauss.get(feats + f).copied().unwrap_or(0.0);
                let a = 2.0 * std::f32::consts::PI * (cx * g0 + cy * g1);
                let row = (y * side + x) * D;
                if let Some(s) = out.get_mut(row + f) {
                    *s = a.sin();
                }
                if let Some(s) = out.get_mut(row + feats + f) {
                    *s = a.cos();
                }
            }
        }
    }
    out
}

fn register(reg: &mut Reg) -> Result<DecIds> {
    let d = "sam_mask_decoder";
    let t = format!("{d}.transformer");
    let mut layers = Vec::with_capacity(2);
    for l in 0..2 {
        let p = format!("{t}.layers.{l}");
        layers.push(LayerIds {
            self_attn: attn_ids(reg, &format!("{p}.self_attn"), D)?,
            t2i: attn_ids(reg, &format!("{p}.cross_attn_token_to_image"), HALF)?,
            i2t: attn_ids(reg, &format!("{p}.cross_attn_image_to_token"), HALF)?,
            mlp0: reg.linear(&format!("{p}.mlp.layers.0"), MLP_DIM, D)?,
            mlp1: reg.linear(&format!("{p}.mlp.layers.1"), D, MLP_DIM)?,
            norms: [
                reg.norm(&format!("{p}.norm1"), D)?,
                reg.norm(&format!("{p}.norm2"), D)?,
                reg.norm(&format!("{p}.norm3"), D)?,
                reg.norm(&format!("{p}.norm4"), D)?,
            ],
        });
    }
    let gauss = reg.get("sam_prompt_encoder.pe_layer.positional_encoding_gaussian_matrix", &[2, D / 2])?;
    let pe = dense_pe(gauss);
    Ok(DecIds {
        layers,
        final_attn: attn_ids(reg, &format!("{t}.final_attn_token_to_image"), HALF)?,
        norm_final: reg.norm(&format!("{t}.norm_final_attn"), D)?,
        pe: reg.put("#decoder_pe", pe),
        up0: up_ids(reg, &format!("{d}.output_upscaling.0"), D, D / 4)?,
        up_norm: reg.norm(&format!("{d}.output_upscaling.1"), D / 4)?,
        up1: up_ids(reg, &format!("{d}.output_upscaling.3"), D / 4, D / 8)?,
        hyper: (0..MASKS).map(|i| mlp3(reg, &format!("{d}.output_hypernetworks_mlps.{i}"), D / 8)).collect::<Result<_>>()?,
        iou: mlp3(reg, &format!("{d}.iou_prediction_head"), MASKS)?,
        score: mlp3(reg, &format!("{d}.pred_obj_score_head"), 1)?,
    })
}

impl Ops {
    /// `o = a·wᵀ + b` with `m` input rows of `k` (row stride `a_ld`, from float `a_off`) into
    /// rows of stride `o_ld` from float `o_off`.
    #[allow(clippy::too_many_arguments)]
    fn linear_at(&mut self, a: (B, usize, usize), wb: Wb, o: (B, usize, usize), m: usize, n: usize, k: usize, act: Act, accumulate: bool) {
        let end = |off: usize, ld: usize, cols: usize| off + m.saturating_sub(1) * ld + cols;
        self.uses(a.0, end(a.1, a.2, k));
        self.uses(o.0, end(o.1, o.2, n));
        let s = Bmm { a: Strided { off: a.1, ld: a.2, bs: 0 }, o: Strided { off: o.1, ld: o.2, bs: 0 }, act, accumulate, ..Bmm::linear(m, n, k) };
        self.push(Op::Bmm(s, a.0, wb.0, Some(wb.1), o.0));
    }

    fn ew(&mut self, len: usize, blen: usize, beta: f32, a: B, b: B, o: B) {
        self.uses(a, len);
        self.uses(b, blen);
        self.uses(o, len);
        self.push(Op::Ew { len, blen, alpha: 1.0, beta, a, b, o });
    }

    /// SAM's `Attention`: queries `nq` rows of `q_in`, keys `nk` rows of `k_in`, values `nk` rows
    /// of `v_in` (all 256 wide), into `nq` rows of `out` (added to it when `accumulate`).
    #[allow(clippy::too_many_arguments)]
    fn sam_attn(&mut self, a: &AttnIds, q_in: B, k_in: B, v_in: B, nq: usize, nk: usize, out: B, accumulate: bool) {
        let i = a.inner;
        self.linear_at((q_in, 0, D), a.q, (B::A(X_AQ), 0, i), nq, i, D, Act::None, false);
        self.linear_at((k_in, 0, D), a.k, (B::A(X_KV), 0, 2 * i), nk, i, D, Act::None, false);
        self.linear_at((v_in, 0, D), a.v, (B::A(X_KV), i, 2 * i), nk, i, D, Act::None, false);
        self.sdpa(HEADS, i / HEADS, nq, nk, (B::A(X_AQ), 0, i), (B::A(X_KV), 0, 2 * i), (B::A(X_KV), i, 2 * i), (B::A(X_AO), i), B::A(X_S));
        self.linear_at((B::A(X_AO), 0, i), a.out, (out, 0, D), nq, D, i, Act::None, accumulate);
    }

    fn ln(&mut self, x: B, gb: Wb, o: B, rows: usize, c: usize, eps: f32) {
        self.norm(x, gb, o, LayerNorm { rows, c, eps, gather: None, channel_major: false }, false);
    }

    /// A 3-layer MLP (ReLU between) on row `row` of `x` (256 wide) into `o` from float `o_off`.
    fn mlp_row(&mut self, w: &[Wb; 3], x: B, row: usize, o: B, o_off: usize, out: usize) {
        self.linear_at((x, row * D, D), w[0], (B::A(X_HY0), 0, D), 1, D, D, Act::Relu, false);
        self.linear_at((B::A(X_HY0), 0, D), w[1], (B::A(X_HY1), 0, D), 1, D, D, Act::Relu, false);
        self.linear_at((B::A(X_HY1), 0, D), w[2], (o, o_off, out), 1, out, D, Act::None, false);
    }

    /// The 2 × 2 stride-2 transposed convolution of a `side`² × `cin` grid into a `(2·side)²` ×
    /// `cout` grid: per output row `dy`, one batched matmul (batch = input row y, rows = input
    /// column x) writing both `dx` pixels, which sit next to each other.
    fn up2x2(&mut self, w: &[Wb; 2], x: B, o: B, side: usize, cin: usize, cout: usize) {
        let ow = 2 * side;
        self.uses(x, side * side * cin);
        self.uses(o, ow * ow * cout);
        for (dy, wb) in w.iter().enumerate() {
            let s = Bmm {
                m: side,
                n: 2 * cout,
                k: cin,
                batch: side,
                a: Strided { off: 0, ld: cin, bs: side * cin },
                w: Strided::rows(cin),
                o: Strided { off: dy * ow * cout, ld: 2 * cout, bs: 2 * ow * cout },
                w_kn: false,
                alpha: 1.0,
                bias: true,
                act: Act::None,
                accumulate: false,
            };
            self.push(Op::Bmm(s, x, wb.0, Some(wb.1), o));
        }
    }
}

/// The decoder for `t` input tokens and a dense embedding of `dense_len` values (256: one vector
/// for every pixel; 4096 × 256: a mask prompt's).
fn plan(ids: &DecIds, t: usize, dense_len: usize) -> Result<Ops> {
    if !(1..=MAX_TOKENS).contains(&t) || (dense_len != D && dense_len != FEAT_LEN) {
        return Err(err(format!("a decoder of {t} tokens and a dense embedding of {dense_len} values")));
    }
    let mut o = Ops::default();
    let eps = 1e-5;
    let tl = t * D;
    for (a, n) in [(X_PIX, FEAT_LEN), (X_DENSE, FEAT_LEN), (X_TOK, MAX_TOKENS * D), (X_S0, S0_LEN), (X_S1, S1_LEN)] {
        o.uses(B::A(a), n);
    }
    // src = image embedding + dense prompt embedding; the queries start as the tokens
    o.ew(FEAT_LEN, dense_len, 1.0, B::A(X_PIX), B::A(X_DENSE), B::A(X_K));
    o.ew(tl, tl, 0.0, B::A(X_TOK), B::A(X_TOK), B::A(X_Q));
    let (mut q, mut q2) = (B::A(X_Q), B::A(X_Q2));
    let (mut k, mut k2) = (B::A(X_K), B::A(X_K2));
    let (tok, qq, kk, pe) = (B::A(X_TOK), B::A(X_QQ), B::A(X_KK), ids.pe);
    for (l, ly) in ids.layers.iter().enumerate() {
        // self-attention (the first layer without the query position encoding or residual)
        if l == 0 {
            o.sam_attn(&ly.self_attn, q, q, q, t, t, q2, false);
            o.ln(q2, ly.norms[0], q, t, D, eps);
        } else {
            o.ew(tl, tl, 1.0, q, tok, qq);
            o.sam_attn(&ly.self_attn, qq, qq, q, t, t, q, true);
            o.ln(q, ly.norms[0], q2, t, D, eps);
            std::mem::swap(&mut q, &mut q2);
        }
        // tokens attend to the image
        o.ew(tl, tl, 1.0, q, tok, qq);
        o.ew(FEAT_LEN, FEAT_LEN, 1.0, k, pe, kk);
        o.sam_attn(&ly.t2i, qq, kk, k, t, TOKENS, q, true);
        o.ln(q, ly.norms[1], q2, t, D, eps);
        std::mem::swap(&mut q, &mut q2);
        // MLP
        o.linear_at((q, 0, D), ly.mlp0, (B::A(X_H), 0, MLP_DIM), t, MLP_DIM, D, Act::Relu, false);
        o.linear_at((B::A(X_H), 0, MLP_DIM), ly.mlp1, (q, 0, D), t, D, MLP_DIM, Act::None, true);
        o.ln(q, ly.norms[2], q2, t, D, eps);
        std::mem::swap(&mut q, &mut q2);
        // the image attends to the tokens
        o.ew(tl, tl, 1.0, q, tok, qq);
        o.ew(FEAT_LEN, FEAT_LEN, 1.0, k, pe, kk);
        o.sam_attn(&ly.i2t, kk, qq, q, TOKENS, t, k, true);
        o.ln(k, ly.norms[3], k2, TOKENS, D, eps);
        std::mem::swap(&mut k, &mut k2);
    }
    o.ew(tl, tl, 1.0, q, tok, qq);
    o.ew(FEAT_LEN, FEAT_LEN, 1.0, k, pe, kk);
    o.sam_attn(&ids.final_attn, qq, kk, k, t, TOKENS, q, true);
    o.ln(q, ids.norm_final, B::A(X_HS), t, D, eps);
    // upscaling: 64² × 256 → 128² × 64 (+ s1, LayerNorm2d, GELU) → 256² × 32 (+ s0, GELU)
    let (c1, c2) = (D / 4, D / 8);
    o.up2x2(&ids.up0, k, B::A(X_U), 64, D, c1);
    o.ew(S1_LEN, S1_LEN, 1.0, B::A(X_U), B::A(X_S1), B::A(X_U2));
    o.ln(B::A(X_U2), ids.up_norm, B::A(X_U), 128 * 128, c1, 1e-6);
    o.push(Op::Unary(UnaryOp::GeluErf, S1_LEN, B::A(X_U)));
    o.up2x2(&ids.up1, B::A(X_U), B::A(X_V), 128, c1, c2);
    o.ew(S0_LEN, S0_LEN, 1.0, B::A(X_V), B::A(X_S0), B::A(X_V2));
    o.push(Op::Unary(UnaryOp::GeluErf, S0_LEN, B::A(X_V2)));
    // heads: each mask token's hypernetwork, the masks, IoU predictions, object score
    for (i, h) in ids.hyper.iter().enumerate() {
        o.mlp_row(h, B::A(X_HS), 2 + i, B::A(X_HYP), i * c2, c2);
    }
    o.uses(B::A(X_MASKS), MASKS * MASK_LEN);
    let masks = Bmm { bias: false, ..Bmm::linear(MASKS, MASK_LEN, c2) };
    o.push(Op::Bmm(masks, B::A(X_HYP), B::A(X_V2), None, B::A(X_MASKS)));
    o.mlp_row(&ids.iou, B::A(X_HS), 1, B::A(X_IOU), 0, MASKS);
    o.push(Op::Unary(UnaryOp::Sigmoid, MASKS, B::A(X_IOU)));
    o.mlp_row(&ids.score, B::A(X_HS), 0, B::A(X_SCORE), 0, 1);
    o.uses(B::A(X_HS), (2 + MASKS) * D);
    Ok(o)
}

/// SAM 2.1's mask decoder on a wgpu device, weights resident.
pub struct Sam2GpuDecoder {
    exec: Exec,
    nn: NnKernels,
    weights: Vec<wgpu::Buffer>,
    ids: DecIds,
    acts: Vec<wgpu::Buffer>,
    plans: Mutex<HashMap<(usize, usize), Arc<Vec<NnStep>>>>,
}

impl std::fmt::Debug for Sam2GpuDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sam2GpuDecoder({} weights)", self.weights.len())
    }
}

impl Sam2GpuDecoder {
    /// A device of its own (as [`super::Sam2Gpu::open`]) with only the decoder: tests and
    /// benchmarks. Returns the adapter's name too.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(weights: &HashMap<String, Weight>) -> Result<(Self, String)> {
        let (device, queue, info) = super::open_device("filmcraft magic mask sam2 decoder")?;
        let d = Self::new(&device, &queue, weights)?;
        Ok((d, format!("{} ({:?})", info.name, info.backend)))
    }

    /// Plan the decoder on `device` and upload its weights (`sam_mask_decoder.*` and the prompt
    /// encoder's `pe_layer`, named as in the checkpoint).
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, weights: &HashMap<String, Weight>) -> Result<Self> {
        let mut reg = Reg { src: weights, uploads: Vec::new(), ids: HashMap::new() };
        let ids = register(&mut reg)?;
        let biggest = plan(&ids, MAX_TOKENS, FEAT_LEN)?;
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let val = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let built = (|| -> Result<Self> {
            let nn = NnKernels::new(device);
            let max = nn.max_binding();
            let mut bufs = Vec::with_capacity(reg.uploads.len());
            for data in &reg.uploads {
                if (data.len() as u64).saturating_mul(4) > max {
                    return Err(err("a decoder weight is over the GPU's binding limit"));
                }
                let contents: &[u8] = if data.is_empty() { &[0; 16] } else { bytemuck::cast_slice(data) };
                bufs.push(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("sam2-decoder-weight"),
                    contents,
                    usage: wgpu::BufferUsages::STORAGE,
                }));
            }
            let acts = alloc(device, max, &biggest.need, "sam2-decoder")?;
            // the largest plan binds
            let _ = steps(&nn, &biggest.ops, &acts, &bufs, (ids.pe, ids.pe))?;
            Ok(Self { exec: Exec::new(device, queue), nn, weights: bufs, ids: ids.clone(), acts, plans: Mutex::new(HashMap::new()) })
        })();
        let index = queue.submit(std::iter::empty());
        let waited = device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(GPU_WAIT) });
        if let Some(e) = pollster::block_on(val.pop()) {
            return Err(err(format!("decoder kernels: {e}")));
        }
        if let Some(e) = pollster::block_on(oom.pop()) {
            return Err(err(format!("out of GPU memory: {e}")));
        }
        waited.map_err(|e| err(format!("decoder upload: {e}")))?;
        built
    }

    /// The decoder on the memory-conditioned embedding `pix` (4096 × 256), the high-resolution
    /// maps `s0` (256² × 32) and `s1` (128² × 64), the input `tokens` (`t` × 256: the object
    /// score, IoU and four mask tokens, then the sparse prompt embeddings) and the dense prompt
    /// embedding `dense` (256 values for every pixel, or 4096 × 256).
    pub fn decode(&self, pix: &[f32], s0: &[f32], s1: &[f32], tokens: &[f32], dense: &[f32]) -> Result<DecoderOut> {
        let t = tokens.len() / D;
        if pix.len() != FEAT_LEN || s0.len() != S0_LEN || s1.len() != S1_LEN || tokens.len() != t * D || !(2 + MASKS..=MAX_TOKENS).contains(&t) {
            return Err(err(format!("decoder inputs of {} / {} / {} / {} values", pix.len(), s0.len(), s1.len(), tokens.len())));
        }
        if dense.len() != D && dense.len() != FEAT_LEN {
            return Err(err(format!("a dense embedding of {} values", dense.len())));
        }
        let steps_for = {
            let mut cache = self.plans.lock().unwrap_or_else(PoisonError::into_inner);
            match cache.get(&(t, dense.len())) {
                Some(p) => p.clone(),
                None => {
                    let ops = plan(&self.ids, t, dense.len())?;
                    let st = Arc::new(steps(&self.nn, &ops.ops, &self.acts, &self.weights, (self.ids.pe, self.ids.pe))?);
                    if cache.len() >= MAX_CACHED_PLANS {
                        cache.clear();
                    }
                    cache.insert((t, dense.len()), st.clone());
                    st
                }
            }
        };
        let inputs = [(X_PIX, pix), (X_DENSE, dense), (X_TOK, tokens), (X_S0, s0), (X_S1, s1)];
        let reads = [(X_MASKS, MASKS * MASK_LEN), (X_IOU, MASKS), (X_SCORE, 1), (X_HS, (2 + MASKS) * D)];
        let mut out = self.exec.run(&self.acts, &inputs, &[(&steps_for, &reads)])?;
        let hs = out.pop().ok_or_else(|| err("no output"))?;
        let score = out.pop().ok_or_else(|| err("no output"))?;
        let ious = out.pop().ok_or_else(|| err("no output"))?;
        let masks = out.pop().ok_or_else(|| err("no output"))?;
        let tokens = hs.get(2 * D..).map(<[f32]>::to_vec).ok_or_else(|| err("short read-back"))?;
        Ok(DecoderOut { masks, ious, score: score.first().copied().unwrap_or(0.0), tokens })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_shapes_are_refused_while_planning() {
        let w = HashMap::new();
        let mut reg = Reg { src: &w, uploads: Vec::new(), ids: HashMap::new() };
        assert!(register(&mut reg).unwrap_err().0.contains("missing"));
        let a = AttnIds { q: (B::W(0), B::W(0)), k: (B::W(0), B::W(0)), v: (B::W(0), B::W(0)), out: (B::W(0), B::W(0)), inner: HALF };
        let ly = LayerIds { self_attn: a, t2i: a, i2t: a, mlp0: a.q, mlp1: a.q, norms: [a.q; 4] };
        let ids = DecIds {
            layers: vec![ly, ly],
            final_attn: a,
            norm_final: a.q,
            pe: B::W(0),
            up0: [a.q; 2],
            up_norm: a.q,
            up1: [a.q; 2],
            hyper: vec![[a.q; 3]; MASKS],
            iou: [a.q; 3],
            score: [a.q; 3],
        };
        assert!(plan(&ids, 0, D).is_err());
        assert!(plan(&ids, MAX_TOKENS + 1, D).is_err());
        assert!(plan(&ids, 8, 7).is_err());
        assert!(plan(&ids, 8, D).is_ok() && plan(&ids, 8, FEAT_LEN).is_ok());
    }

    /// The HWC encoding is the CPU's CHW one transposed: token (y, x) channel f.
    #[test]
    fn dense_pe_matches_the_reference_formula() {
        let g: Vec<f32> = (0..D).map(|i| (i as f32 * 0.37).sin()).collect();
        let pe = dense_pe(&g);
        let (y, x, f) = (5usize, 9usize, 3usize);
        let a = 2.0 * std::f32::consts::PI * ((2.0 * ((x as f32 + 0.5) / 64.0) - 1.0) * g[f] + (2.0 * ((y as f32 + 0.5) / 64.0) - 1.0) * g[128 + f]);
        assert_eq!(pe[(y * 64 + x) * D + f], a.sin());
        assert_eq!(pe[(y * 64 + x) * D + 128 + f], a.cos());
    }
}
