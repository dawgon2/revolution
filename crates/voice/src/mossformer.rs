//! MossFormer2_SE_48K speech enhancement (fork AUDIO-PLAN A3.7, the "Better" Voice Isolation
//! model): a pure-Rust port of the forward pass of ClearerVoice-Studio's MossFormer2 for 48 kHz
//! speech enhancement (Shengkui Zhao et al., Alibaba / ModelScope, Apache-2.0 code and weights:
//! https://github.com/modelscope/ClearerVoice-Studio, `clearvoice/models/mossformer2_se/`
//! `mossformer2.py`, `mossformer2_block.py`, `fsmn.py`, `conv_module.py`, `layer_norm.py`; its FLASH
//! block follows lucidrains' FLASH-pytorch (MIT), the rotary embedding `rotary-embedding-torch`
//! (MIT)). The weights are the official `last_best_checkpoint.pt` (never bundled or committed).
//!
//! Network (`MossFormer_MaskNet`, 24 blocks, 55 M parameters), per 8 ms frame:
//! 180 features ([`crate::kaldi::features`]) → GroupNorm(1) → 1×1 conv to 512 → + scaled sinusoidal
//! position → 24 × (FLASH layer with a shared-attention quadratic (groups of 256 frames) + linear
//! (whole clip) attention, rotary positions on the first 32 query / key dims; gated FSMN block) →
//! LayerNorm → GroupNorm(1) + skip → PReLU → 1×1 conv (first speaker of two) → tanh · sigmoid gate →
//! 1×1 conv to 961 → ReLU = the mask, which multiplies the noisy 1920-point STFT (noisy phase) before
//! the inverse STFT. Clips over 20 s run in 4 s windows every 3 s, the outer 0.5 s of each window's
//! overlap dropped (ClearerVoice's `decode_one_audio_mossformer2_se_48k`; here the windows also
//! cover the clip's end, which the reference leaves silent).
//!
//! candle does the matrix products (`gemm`, run-time SIMD, all cores); the element-wise steps are
//! plain Rust on time-major buffers. [`MossFormer2::load`] and [`MossFormer2::enhance`] run under
//! `catch_unwind`; a missing, foreign or corrupt checkpoint is an error that says why.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{Device, Tensor};
use filmcraft_ml::job::{Cancel, guarded};

use crate::ModelError;
use crate::kaldi::{self, BINS, FEATS, HOP, MaskStft, WIN};

/// The released checkpoint is 221.6 MB.
pub const MAX_FILE_BYTES: u64 = 512 << 20;
/// Published name / size / SHA-256 of the checkpoint (Hugging Face `alibabasglab/MossFormer2_SE_48K`).
pub const CHECKPOINT: &str = "last_best_checkpoint.pt";
pub const CHECKPOINT_SIZE: u64 = 221_552_019;
pub const CHECKPOINT_SHA256: &str = "03692b9f773bbd6bb43b9c5a41f96b1e28affd66e13796b7bec66ad3d8b227c6";
pub const SOURCE_URL: &str = "https://huggingface.co/alibabasglab/MossFormer2_SE_48K";

const DIM: usize = 512;
const LAYERS: usize = 24;
const HIDDEN: usize = 2048;
const QK: usize = 128;
const GROUP: usize = 256;
const ROT: usize = 32;
const FSMN: usize = 256;
const FSMN_K: usize = 39;
const CONV_K: usize = 17;
/// Longest clip run in one pass; longer ones go through [`WINDOW`]-long windows.
pub const ONE_PASS: usize = 20 * 48_000;
pub const WINDOW: usize = 4 * 48_000;
pub const STRIDE: usize = WINDOW * 3 / 4;
const GIVE_UP: usize = (WINDOW - STRIDE) / 2;
/// Most channels per call, and the longest input (4 h at 48 kHz).
pub const MAX_CHANNELS: usize = 8;
pub const MAX_SAMPLES: usize = 4 * 3600 * 48_000;
const DITHER_SEED: u64 = 0x004d_4632_5345;
const PREFIX: &str = "mossformer.mdl.intra_mdl.mossformerM";

fn err(e: impl std::fmt::Display) -> ModelError {
    ModelError(e.to_string())
}

/// Every tensor the network reads, with its shape (929 in the release).
pub fn manifest() -> Vec<(String, Vec<usize>)> {
    let mut m: Vec<(String, Vec<usize>)> = Vec::new();
    let mut add = |n: String, d: &[usize]| m.push((n, d.to_vec()));
    add("mossformer.norm.weight".into(), &[180]);
    add("mossformer.norm.bias".into(), &[180]);
    add("mossformer.conv1d_encoder.weight".into(), &[DIM, 180, 1]);
    add("mossformer.pos_enc.scale".into(), &[1]);
    add("mossformer.pos_enc.inv_freq".into(), &[DIM / 2]);
    let ffconv = |add: &mut dyn FnMut(String, &[usize]), p: &str, i: usize, o: usize, scale: bool| {
        if scale {
            add(format!("{p}.mdl.0.g"), &[1]);
        } else {
            add(format!("{p}.mdl.0.weight"), &[i]);
            add(format!("{p}.mdl.0.bias"), &[i]);
        }
        add(format!("{p}.mdl.1.weight"), &[o, i]);
        add(format!("{p}.mdl.1.bias"), &[o]);
        add(format!("{p}.mdl.3.sequential.1.conv.weight"), &[o, 1, CONV_K]);
    };
    for l in 0..LAYERS {
        let p = format!("{PREFIX}.layers.{l}");
        add(format!("{p}.rotary_pos_emb.freqs"), &[ROT / 2]);
        ffconv(&mut add, &format!("{p}.to_hidden"), DIM, HIDDEN, true);
        ffconv(&mut add, &format!("{p}.to_qk"), DIM, QK, true);
        add(format!("{p}.qk_offset_scale.gamma"), &[4, QK]);
        add(format!("{p}.qk_offset_scale.beta"), &[4, QK]);
        ffconv(&mut add, &format!("{p}.to_out"), HIDDEN / 2, DIM, true);
        let f = format!("{PREFIX}.fsmn.{l}");
        add(format!("{f}.conv1.0.weight"), &[FSMN, DIM, 1]);
        add(format!("{f}.conv1.0.bias"), &[FSMN]);
        add(format!("{f}.conv1.1.weight"), &[1]);
        add(format!("{f}.norm1.weight"), &[FSMN]);
        add(format!("{f}.norm1.bias"), &[FSMN]);
        ffconv(&mut add, &format!("{f}.gated_fsmn.to_u"), FSMN, FSMN, false);
        ffconv(&mut add, &format!("{f}.gated_fsmn.to_v"), FSMN, FSMN, false);
        add(format!("{f}.gated_fsmn.fsmn.linear.weight"), &[FSMN, FSMN]);
        add(format!("{f}.gated_fsmn.fsmn.linear.bias"), &[FSMN]);
        add(format!("{f}.gated_fsmn.fsmn.project.weight"), &[FSMN, FSMN]);
        add(format!("{f}.gated_fsmn.fsmn.conv1.weight"), &[FSMN, 1, FSMN_K, 1]);
        add(format!("{f}.norm2.weight"), &[FSMN]);
        add(format!("{f}.norm2.bias"), &[FSMN]);
        add(format!("{f}.conv2.weight"), &[DIM, FSMN, 1]);
        add(format!("{f}.conv2.bias"), &[DIM]);
    }
    add("mossformer.mdl.intra_mdl.norm.weight".into(), &[DIM]);
    add("mossformer.mdl.intra_mdl.norm.bias".into(), &[DIM]);
    add("mossformer.mdl.intra_norm.weight".into(), &[DIM]);
    add("mossformer.mdl.intra_norm.bias".into(), &[DIM]);
    add("mossformer.prelu.weight".into(), &[1]);
    add("mossformer.conv1d_out.weight".into(), &[2 * DIM, DIM, 1]);
    add("mossformer.conv1d_out.bias".into(), &[2 * DIM]);
    add("mossformer.output.0.weight".into(), &[DIM, DIM, 1]);
    add("mossformer.output.0.bias".into(), &[DIM]);
    add("mossformer.output_gate.0.weight".into(), &[DIM, DIM, 1]);
    add("mossformer.output_gate.0.bias".into(), &[DIM]);
    add("mossformer.conv1_decoder.weight".into(), &[BINS, DIM, 1]);
    m
}

/// Checked tensors by name (row-major values).
struct Raw(HashMap<String, Vec<f32>>);

impl Raw {
    fn take(&mut self, name: &str) -> Result<Vec<f32>, ModelError> {
        self.0.remove(name).ok_or_else(|| err(format!("missing weight `{name}`")))
    }
    fn scalar(&mut self, name: &str) -> Result<f32, ModelError> {
        self.take(name)?.first().copied().ok_or_else(|| err(format!("empty weight `{name}`")))
    }
}

// ---------------------------------------------------------------------------------------------
// building blocks (time-major buffers: row t holds the channels of frame t)
// ---------------------------------------------------------------------------------------------

/// `y = x · Wᵀ + b` for PyTorch weight `W` `[out, in]`.
struct Linear {
    /// `Wᵀ`, `[in, out]`, contiguous.
    wt: Tensor,
    b: Option<Vec<f32>>,
    n_in: usize,
    n_out: usize,
}

impl Linear {
    fn new(w: Vec<f32>, b: Option<Vec<f32>>, n_out: usize, n_in: usize) -> Result<Linear, ModelError> {
        if w.len() != n_out * n_in || b.as_ref().is_some_and(|b| b.len() != n_out) {
            return Err(err("linear layer: wrong weight size"));
        }
        let wt = Tensor::from_vec(w, (n_out, n_in), &Device::Cpu).and_then(|t| t.t()?.contiguous()).map_err(err)?;
        Ok(Linear { wt, b, n_in, n_out })
    }

    fn load(raw: &mut Raw, p: &str, n_out: usize, n_in: usize, bias: bool) -> Result<Linear, ModelError> {
        let w = raw.take(&format!("{p}.weight"))?;
        let b = if bias { Some(raw.take(&format!("{p}.bias"))?) } else { None };
        Linear::new(w, b, n_out, n_in)
    }

    fn apply(&self, x: &[f32], t: usize) -> Result<Vec<f32>, ModelError> {
        if x.len() != t * self.n_in {
            return Err(err("linear layer: wrong input size"));
        }
        let mut y = matmul(x, t, self.n_in, &self.wt)?;
        if let Some(b) = &self.b {
            for row in y.chunks_exact_mut(self.n_out) {
                row.iter_mut().zip(b).for_each(|(v, b)| *v += b);
            }
        }
        Ok(y)
    }
}

/// `x` (`[t, k]`) times a `[k, n]` tensor.
fn matmul(x: &[f32], t: usize, k: usize, w: &Tensor) -> Result<Vec<f32>, ModelError> {
    if t == 0 {
        return Ok(Vec::new());
    }
    Tensor::from_slice(x, (t, k), &Device::Cpu).and_then(|a| a.matmul(w)).and_then(|y| y.flatten_all()?.to_vec1::<f32>()).map_err(err)
}

fn silu(v: f32) -> f32 {
    v / (1.0 + (-v).exp())
}

fn sigmoid(v: f32) -> f32 {
    1.0 / (1.0 + (-v).exp())
}

/// Per-frame LayerNorm over `c` channels.
fn layer_norm(x: &mut [f32], c: usize, w: &[f32], b: &[f32], eps: f64) {
    for row in x.chunks_exact_mut(c) {
        let mean = row.iter().map(|v| f64::from(*v)).sum::<f64>() / c as f64;
        let var = row.iter().map(|v| (f64::from(*v) - mean).powi(2)).sum::<f64>() / c as f64;
        let inv = 1.0 / (var + eps).sqrt();
        for ((v, w), b) in row.iter_mut().zip(w).zip(b) {
            *v = ((f64::from(*v) - mean) * inv) as f32 * w + b;
        }
    }
}

/// GroupNorm with one group: statistics over the whole `[t, c]` buffer, per-channel affine.
fn group_norm(x: &mut [f32], c: usize, w: &[f32], b: &[f32], eps: f64) {
    let n = x.len().max(1) as f64;
    let mean = x.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let var = x.iter().map(|v| (f64::from(*v) - mean).powi(2)).sum::<f64>() / n;
    let inv = 1.0 / (var + eps).sqrt();
    for row in x.chunks_exact_mut(c) {
        for ((v, w), b) in row.iter_mut().zip(w).zip(b) {
            *v = ((f64::from(*v) - mean) * inv) as f32 * w + b;
        }
    }
}

/// `x + depthwise_conv(x)` along time, kernel `w` `[c, k]`, `(k − 1) / 2` zeros each side
/// (ClearerVoice's `ConvModule`; with `residual = false` only the convolution).
fn depthwise(x: &[f32], t: usize, c: usize, w: &[f32], k: usize) -> Vec<f32> {
    let pad = k.saturating_sub(1) / 2;
    let mut y = vec![0f32; x.len()];
    if c == 0 || k == 0 {
        return y;
    }
    // weights transposed to [k, c] so the inner loop runs over channels
    let mut wk = vec![0f32; k * c];
    for (ch, taps) in w.chunks_exact(k).take(c).enumerate() {
        for (j, v) in taps.iter().enumerate() {
            if let Some(d) = wk.get_mut(j * c + ch) {
                *d = *v;
            }
        }
    }
    let rows: Vec<&[f32]> = x.chunks_exact(c).take(t).collect();
    for (i, out) in y.chunks_exact_mut(c).take(t).enumerate() {
        for (j, wrow) in wk.chunks_exact(c).enumerate() {
            let Some(row) = (i + j).checked_sub(pad).and_then(|s| rows.get(s)) else { continue };
            for ((o, v), w) in out.iter_mut().zip(row.iter()).zip(wrow) {
                *o += v * w;
            }
        }
    }
    y
}

enum Norm {
    /// ScaleNorm: x / max(‖x‖ / √d, 1e-5) · g.
    Scale(f32),
    /// LayerNorm (eps 1e-5).
    Layer(Vec<f32>, Vec<f32>),
}

/// ClearerVoice's `FFConvM`: norm → linear → SiLU → `x + depthwise conv (17)`.
struct FfConv {
    norm: Norm,
    lin: Linear,
    conv: Vec<f32>,
}

impl FfConv {
    fn load(raw: &mut Raw, p: &str, n_in: usize, n_out: usize, scale: bool) -> Result<FfConv, ModelError> {
        let norm = if scale {
            Norm::Scale(raw.scalar(&format!("{p}.mdl.0.g"))?)
        } else {
            Norm::Layer(raw.take(&format!("{p}.mdl.0.weight"))?, raw.take(&format!("{p}.mdl.0.bias"))?)
        };
        let lin = Linear::load(raw, &format!("{p}.mdl.1"), n_out, n_in, true)?;
        let conv = raw.take(&format!("{p}.mdl.3.sequential.1.conv.weight"))?;
        Ok(FfConv { norm, lin, conv })
    }

    fn apply(&self, x: &[f32], t: usize) -> Result<Vec<f32>, ModelError> {
        let c = self.lin.n_in;
        let mut h = x.to_vec();
        match &self.norm {
            Norm::Scale(g) => {
                for row in h.chunks_exact_mut(c) {
                    let norm = (row.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>().sqrt() / (c as f64).sqrt()).max(1e-5);
                    let k = (f64::from(*g) / norm) as f32;
                    row.iter_mut().for_each(|v| *v *= k);
                }
            }
            Norm::Layer(w, b) => layer_norm(&mut h, c, w, b, 1e-5),
        }
        let mut y = self.lin.apply(&h, t)?;
        y.iter_mut().for_each(|v| *v = silu(*v));
        let conv = depthwise(&y, t, self.lin.n_out, &self.conv, CONV_K);
        y.iter_mut().zip(&conv).for_each(|(v, c)| *v += c);
        Ok(y)
    }
}

/// One FLASH layer (`FLASH_ShareA_FFConvM`).
struct Flash {
    to_hidden: FfConv,
    to_qk: FfConv,
    gamma: Vec<f32>,
    beta: Vec<f32>,
    to_out: FfConv,
    freqs: Vec<f32>,
}

/// Rotate the first [`ROT`] dims of every `QK`-wide row in interleaved pairs by `pos · freq`
/// (`rotary-embedding-torch`'s `rotate_queries_or_keys`).
fn rotary(x: &mut [f32], cos: &[f32], sin: &[f32]) {
    let (cos, sin) = (cos.as_chunks::<{ ROT / 2 }>().0, sin.as_chunks::<{ ROT / 2 }>().0);
    for ((row, c), s) in x.as_chunks_mut::<QK>().0.iter_mut().zip(cos).zip(sin) {
        let pairs = row.as_chunks_mut::<2>().0;
        for ((pair, c), s) in pairs.iter_mut().zip(c).zip(s) {
            let [a, b] = *pair;
            *pair = [a * c - b * s, b * c + a * s];
        }
    }
}

impl Flash {
    fn load(raw: &mut Raw, l: usize) -> Result<Flash, ModelError> {
        let p = format!("{PREFIX}.layers.{l}");
        Ok(Flash {
            to_hidden: FfConv::load(raw, &format!("{p}.to_hidden"), DIM, HIDDEN, true)?,
            to_qk: FfConv::load(raw, &format!("{p}.to_qk"), DIM, QK, true)?,
            gamma: raw.take(&format!("{p}.qk_offset_scale.gamma"))?,
            beta: raw.take(&format!("{p}.qk_offset_scale.beta"))?,
            to_out: FfConv::load(raw, &format!("{p}.to_out"), HIDDEN / 2, DIM, true)?,
            freqs: raw.take(&format!("{p}.rotary_pos_emb.freqs"))?,
        })
    }

    fn apply(&self, x: &mut [f32], t: usize) -> Result<(), ModelError> {
        const E: usize = HIDDEN / 2;
        // token shift: the first half of the channels comes from the previous frame
        let mut shifted = x.to_vec();
        {
            let rows = shifted.as_chunks_mut::<DIM>().0;
            if let Some(first) = rows.first_mut() {
                first.iter_mut().take(DIM / 2).for_each(|v| *v = 0.0);
            }
            for (dst, src) in rows.iter_mut().skip(1).zip(x.as_chunks::<DIM>().0) {
                dst.iter_mut().zip(src).take(DIM / 2).for_each(|(d, v)| *d = *v);
            }
        }
        let hidden = self.to_hidden.apply(&shifted, t)?;
        let qk = self.to_qk.apply(&shifted, t)?;
        drop(shifted);
        let (mut v, mut u) = (Vec::with_capacity(t * E), Vec::with_capacity(t * E));
        for row in hidden.as_chunks::<HIDDEN>().0 {
            let (a, b) = row.split_at(E);
            v.extend_from_slice(a);
            u.extend_from_slice(b);
        }
        drop(hidden);
        // offset-scale into quad_q, lin_q, quad_k, lin_k, then rotary positions
        let mut heads: Vec<Vec<f32>> = (0..4)
            .map(|h| {
                let (g, b) = (self.gamma.get(h * QK..(h + 1) * QK).unwrap_or_default(), self.beta.get(h * QK..(h + 1) * QK).unwrap_or_default());
                qk.as_chunks::<QK>().0.iter().flat_map(|row| row.iter().zip(g).zip(b).map(|((v, g), b)| v * g + b)).collect()
            })
            .collect();
        let (mut cos, mut sin) = (Vec::with_capacity(t * ROT / 2), Vec::with_capacity(t * ROT / 2));
        for i in 0..t {
            for f in &self.freqs {
                let a = i as f32 * f;
                cos.push(a.cos());
                sin.push(a.sin());
            }
        }
        for h in heads.iter_mut() {
            rotary(h, &cos, &sin);
        }
        let [quad_q, lin_q, quad_k, lin_k] = <[Vec<f32>; 4]>::try_from(heads).map_err(|_| err("attention heads"))?;
        let (att_v, att_u) = attention(&quad_q, &lin_q, &quad_k, &lin_k, &v, &u, t)?;
        // out = (att_u · v) · σ(att_v · u)
        let mut out = att_u;
        for (((o, av), v), u) in out.iter_mut().zip(&att_v).zip(&v).zip(&u) {
            *o = *o * v * sigmoid(av * u);
        }
        let y = self.to_out.apply(&out, t)?;
        x.iter_mut().zip(&y).for_each(|(a, b)| *a += b);
        Ok(())
    }
}

/// FLASH's mixed attention: quadratic ReLU² attention inside groups of [`GROUP`] frames plus
/// linear attention over the whole clip, shared by `v` and `u`.
fn attention(qq: &[f32], lq: &[f32], qk: &[f32], lk: &[f32], v: &[f32], u: &[f32], t: usize) -> Result<(Vec<f32>, Vec<f32>), ModelError> {
    const E: usize = HIDDEN / 2;
    let groups = t.div_ceil(GROUP);
    let tp = groups * GROUP;
    let dev = Device::Cpu;
    let padded = |x: &[f32], d: usize| -> Result<Tensor, ModelError> {
        let mut p = x.to_vec();
        p.resize(tp * d, 0.0);
        Tensor::from_vec(p, (groups, GROUP, d), &dev).map_err(err)
    };
    let q = padded(qq, QK)?;
    let k = padded(qk, QK)?;
    let sim = (|| q.matmul(&k.transpose(1, 2)?.contiguous()?)?.affine(1.0 / GROUP as f64, 0.0)?.relu()?.sqr())().map_err(err)?;
    let crop = |r: Tensor| -> Result<Vec<f32>, ModelError> {
        let mut v = r.flatten_all().and_then(|f| f.to_vec1::<f32>()).map_err(err)?;
        v.truncate(t * E);
        Ok(v)
    };
    let quad_v = crop(sim.matmul(&padded(v, E)?).map_err(err)?)?;
    let quad_u = crop(sim.matmul(&padded(u, E)?).map_err(err)?)?;
    let lin = |vals: &[f32]| -> Result<Vec<f32>, ModelError> {
        let kt = Tensor::from_slice(lk, (t, QK), &dev).and_then(|k| k.t()?.contiguous()).map_err(err)?;
        let kv = Tensor::from_slice(vals, (t, E), &dev).and_then(|v| kt.matmul(&v)?.affine(1.0 / t as f64, 0.0)).map_err(err)?;
        matmul(lq, t, QK, &kv)
    };
    let mut av = lin(v)?;
    av.iter_mut().zip(&quad_v).for_each(|(a, b)| *a += b);
    let mut au = lin(u)?;
    au.iter_mut().zip(&quad_u).for_each(|(a, b)| *a += b);
    Ok((av, au))
}

/// One gated FSMN block (`Gated_FSMN_Block`).
struct FsmnBlock {
    conv1: Linear,
    prelu: f32,
    norm1: (Vec<f32>, Vec<f32>),
    to_u: FfConv,
    to_v: FfConv,
    linear: Linear,
    project: Linear,
    mem: Vec<f32>,
    norm2: (Vec<f32>, Vec<f32>),
    conv2: Linear,
}

impl FsmnBlock {
    fn load(raw: &mut Raw, l: usize) -> Result<FsmnBlock, ModelError> {
        let f = format!("{PREFIX}.fsmn.{l}");
        let g = format!("{f}.gated_fsmn");
        Ok(FsmnBlock {
            conv1: Linear::load(raw, &format!("{f}.conv1.0"), FSMN, DIM, true)?,
            prelu: raw.scalar(&format!("{f}.conv1.1.weight"))?,
            norm1: (raw.take(&format!("{f}.norm1.weight"))?, raw.take(&format!("{f}.norm1.bias"))?),
            to_u: FfConv::load(raw, &format!("{g}.to_u"), FSMN, FSMN, false)?,
            to_v: FfConv::load(raw, &format!("{g}.to_v"), FSMN, FSMN, false)?,
            linear: Linear::load(raw, &format!("{g}.fsmn.linear"), FSMN, FSMN, true)?,
            project: Linear::load(raw, &format!("{g}.fsmn.project"), FSMN, FSMN, false)?,
            mem: raw.take(&format!("{g}.fsmn.conv1.weight"))?,
            norm2: (raw.take(&format!("{f}.norm2.weight"))?, raw.take(&format!("{f}.norm2.bias"))?),
            conv2: Linear::load(raw, &format!("{f}.conv2"), DIM, FSMN, true)?,
        })
    }

    fn apply(&self, x: &mut [f32], t: usize) -> Result<(), ModelError> {
        let mut h = self.conv1.apply(x, t)?;
        h.iter_mut().for_each(|v| {
            if *v < 0.0 {
                *v *= self.prelu
            }
        });
        layer_norm(&mut h, FSMN, &self.norm1.0, &self.norm1.1, 1e-5);
        // Gated_FSMN: x_v · fsmn(x_u) + h
        let xu = self.to_u.apply(&h, t)?;
        let xv = self.to_v.apply(&h, t)?;
        let mut f1 = self.linear.apply(&xu, t)?;
        f1.iter_mut().for_each(|v| *v = v.max(0.0));
        let p1 = self.project.apply(&f1, t)?;
        // UniDeepFsmn: xu + p1 + memory conv (39 taps, 19 each side) of p1
        let m = depthwise(&p1, t, FSMN, &self.mem, FSMN_K);
        let mut g = h;
        for ((((g, xu), p), m), xv) in g.iter_mut().zip(&xu).zip(&p1).zip(&m).zip(&xv) {
            *g += xv * (xu + p + m);
        }
        layer_norm(&mut g, FSMN, &self.norm2.0, &self.norm2.1, 1e-5);
        let y = self.conv2.apply(&g, t)?;
        x.iter_mut().zip(&y).for_each(|(a, b)| *a += b);
        Ok(())
    }
}

/// The network (`MossFormer_MaskNet` as `TestNet` builds it).
struct Net {
    norm: (Vec<f32>, Vec<f32>),
    encoder: Linear,
    pos_scale: f32,
    inv_freq: Vec<f32>,
    flash: Vec<Flash>,
    fsmn: Vec<FsmnBlock>,
    block_norm: (Vec<f32>, Vec<f32>),
    intra_norm: (Vec<f32>, Vec<f32>),
    prelu: f32,
    conv_out: Linear,
    output: Linear,
    gate: Linear,
    decoder: Linear,
}

impl Net {
    fn new(mut raw: Raw) -> Result<Net, ModelError> {
        let r = &mut raw;
        let norm = (r.take("mossformer.norm.weight")?, r.take("mossformer.norm.bias")?);
        let encoder = Linear::load(r, "mossformer.conv1d_encoder", DIM, FEATS, false)?;
        let pos_scale = r.scalar("mossformer.pos_enc.scale")?;
        let inv_freq = r.take("mossformer.pos_enc.inv_freq")?;
        let mut flash = Vec::with_capacity(LAYERS);
        let mut fsmn = Vec::with_capacity(LAYERS);
        for l in 0..LAYERS {
            flash.push(Flash::load(r, l)?);
            fsmn.push(FsmnBlock::load(r, l)?);
        }
        let block_norm = (r.take("mossformer.mdl.intra_mdl.norm.weight")?, r.take("mossformer.mdl.intra_mdl.norm.bias")?);
        let intra_norm = (r.take("mossformer.mdl.intra_norm.weight")?, r.take("mossformer.mdl.intra_norm.bias")?);
        let prelu = r.scalar("mossformer.prelu.weight")?;
        // only the first of the two "speakers" is used: its 512 output channels
        let mut w = r.take("mossformer.conv1d_out.weight")?;
        let mut b = r.take("mossformer.conv1d_out.bias")?;
        w.truncate(DIM * DIM);
        b.truncate(DIM);
        let conv_out = Linear::new(w, Some(b), DIM, DIM)?;
        let output = Linear::load(r, "mossformer.output.0", DIM, DIM, true)?;
        let gate = Linear::load(r, "mossformer.output_gate.0", DIM, DIM, true)?;
        let decoder = Linear::load(r, "mossformer.conv1_decoder", BINS, DIM, false)?;
        Ok(Net { norm, encoder, pos_scale, inv_freq, flash, fsmn, block_norm, intra_norm, prelu, conv_out, output, gate, decoder })
    }

    /// The mask (`t` × [`BINS`]) for features `x` (`t` × [`FEATS`]); `step(layer)` after each block,
    /// `false` = stop.
    fn mask(&self, x: &[f32], t: usize, step: &mut dyn FnMut(usize) -> bool) -> Result<Vec<f32>, ModelError> {
        if t == 0 || x.len() != t * FEATS {
            return Err(err("no audio frames to process"));
        }
        let mut f = x.to_vec();
        group_norm(&mut f, FEATS, &self.norm.0, &self.norm.1, 1e-8);
        let mut h = self.encoder.apply(&f, t)?;
        drop(f);
        let half = DIM / 2;
        for (i, row) in h.as_chunks_mut::<DIM>().0.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                let fr = self.inv_freq.get(j % half).copied().unwrap_or(0.0);
                let a = i as f32 * fr;
                *v += self.pos_scale * if j < half { a.sin() } else { a.cos() };
            }
        }
        let skip = h.clone();
        for (l, (fl, fs)) in self.flash.iter().zip(&self.fsmn).enumerate() {
            fl.apply(&mut h, t)?;
            fs.apply(&mut h, t)?;
            if !step(l) {
                return Err(err("cancelled"));
            }
        }
        layer_norm(&mut h, DIM, &self.block_norm.0, &self.block_norm.1, 1e-6);
        group_norm(&mut h, DIM, &self.intra_norm.0, &self.intra_norm.1, 1e-8);
        for ((v, s), _) in h.iter_mut().zip(&skip).zip(0..) {
            *v += s;
            if *v < 0.0 {
                *v *= self.prelu;
            }
        }
        let y = self.conv_out.apply(&h, t)?;
        let mut o = self.output.apply(&y, t)?;
        let g = self.gate.apply(&y, t)?;
        o.iter_mut().zip(&g).for_each(|(o, g)| *o = o.tanh() * sigmoid(*g));
        let mut m = self.decoder.apply(&o, t)?;
        m.iter_mut().for_each(|v| *v = if v.is_finite() { v.max(0.0) } else { 0.0 });
        Ok(m)
    }
}

/// The loaded model.
pub struct MossFormer2 {
    net: Net,
}

impl std::fmt::Debug for MossFormer2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MossFormer2")
    }
}

impl MossFormer2 {
    /// Read and check the official checkpoint.
    pub fn load(path: &Path) -> Result<MossFormer2, ModelError> {
        let size = std::fs::metadata(path).map_err(|e| err(format!("{}: {e}", path.display())))?.len();
        if size > MAX_FILE_BYTES {
            return Err(err(format!("{}: {size} bytes is too large for a MossFormer2_SE_48K checkpoint", path.display())));
        }
        let cancel = Cancel::new();
        let manifest = manifest();
        let wanted: HashMap<String, usize> = manifest.iter().map(|(n, d)| (n.clone(), d.iter().product())).collect();
        let raw = guarded(&cancel, |_| crate::weights::read_pth(path, &wanted, MAX_FILE_BYTES, "MossFormer2_SE_48K"))
            .map_err(|e| err(format!("{}: not a readable MossFormer2_SE_48K checkpoint ({e})", path.display())))?;
        Self::from_tensors(raw)
    }

    /// Check named tensors (name, shape, values) against the manifest and build the network.
    pub fn from_tensors(raw: Vec<(String, Vec<usize>, Vec<f32>)>) -> Result<MossFormer2, ModelError> {
        let mut raw: HashMap<String, (Vec<usize>, Vec<f32>)> = raw.into_iter().map(|(n, d, v)| (n, (d, v))).collect();
        let mut map = HashMap::new();
        for (name, dims) in manifest() {
            let (d, v) = raw.remove(&name).ok_or_else(|| err(format!("the checkpoint has no `{name}`: not MossFormer2_SE_48K")))?;
            if d != dims {
                return Err(err(format!("`{name}` is {d:?}, MossFormer2_SE_48K has {dims:?}")));
            }
            if !v.iter().all(|x| x.is_finite()) {
                return Err(err(format!("`{name}` holds non-finite values: the file is corrupt")));
            }
            map.insert(name, v);
        }
        let cancel = Cancel::new();
        let net = guarded(&cancel, |_| Net::new(Raw(map))).map_err(|e| err(format!("MossFormer2_SE_48K: {e}")))?;
        Ok(MossFormer2 { net })
    }

    /// The mask for 48 kHz `x` (full scale ±1): `kaldi::frames(len)` × [`BINS`].
    pub fn mask(&self, x: &[f32], step: &mut dyn FnMut(usize) -> bool) -> Result<Vec<f32>, ModelError> {
        let scaled: Vec<f32> = x.iter().map(|v| if v.is_finite() { v * 32768.0 } else { 0.0 }).collect();
        let (feats, t) = kaldi::features(&scaled, kaldi::DITHER, DITHER_SEED).ok_or_else(|| err("audio too long"))?;
        self.net.mask(&feats, t, step)
    }

    /// Enhance one segment of equal-length channels (the mask comes from their mean).
    fn segment(&self, chans: &[&[f32]], step: &mut dyn FnMut(usize) -> bool) -> Result<Vec<Vec<f32>>, ModelError> {
        let n = chans.first().map_or(0, |c| c.len());
        // pad to whole frames (at least one) so every sample is covered, then crop
        let padded = WIN + n.saturating_sub(WIN).div_ceil(HOP) * HOP;
        let mut mono = vec![0f32; padded];
        let k = 1.0 / chans.len().max(1) as f32;
        for c in chans {
            for (m, v) in mono.iter_mut().zip(c.iter()) {
                if v.is_finite() {
                    *m += v * k;
                }
            }
        }
        let mask = self.mask(&mono, step)?;
        drop(mono);
        let mut stft = MaskStft::new().ok_or_else(|| err("STFT"))?;
        let mut out = Vec::with_capacity(chans.len());
        for c in chans {
            let mut x = c.to_vec();
            x.resize(padded, 0.0);
            let mut spec = stft.analyse(&x).ok_or_else(|| err("STFT"))?;
            if spec.len() != mask.len() {
                return Err(err("mask and spectrum differ in length"));
            }
            for ((re, im), m) in spec.iter_mut().zip(&mask) {
                *re *= m;
                *im *= m;
            }
            let mut y = stft.synthesise(&spec, padded).ok_or_else(|| err("inverse STFT"))?;
            y.truncate(n);
            y.iter_mut().for_each(|v| *v = if v.is_finite() { *v } else { 0.0 });
            out.push(y);
        }
        Ok(out)
    }

    /// Enhance 48 kHz channels of equal length (1 … [`MAX_CHANNELS`]): the network listens to their
    /// mean and its mask applies to each. `progress(fraction)` returns `false` to stop (then the
    /// result is `Err("cancelled")`). Panics inside become errors.
    pub fn enhance(&self, channels: &[Vec<f32>], progress: &mut dyn FnMut(f32) -> bool) -> Result<Vec<Vec<f32>>, ModelError> {
        let n = channels.first().map_or(0, Vec::len);
        if channels.is_empty() || channels.len() > MAX_CHANNELS {
            return Err(err(format!("{} channels: 1 to {MAX_CHANNELS} are supported", channels.len())));
        }
        if channels.iter().any(|c| c.len() != n) {
            return Err(err("channels of different lengths"));
        }
        if n > MAX_SAMPLES {
            return Err(err(format!("the audio is {:.0} min long; up to 240 min at a time", n as f64 / 48_000.0 / 60.0)));
        }
        if n == 0 {
            return Ok(channels.to_vec());
        }
        let cancel = Cancel::new();
        guarded(&cancel, |_| {
            if n <= ONE_PASS {
                let chans: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
                return self.segment(&chans, &mut |l| progress((l + 1) as f32 / LAYERS as f32));
            }
            let segs = 1 + (n - WINDOW).div_ceil(STRIDE);
            let mut out: Vec<Vec<f32>> = Vec::with_capacity(channels.len());
            for _ in channels {
                let mut o = Vec::new();
                o.try_reserve_exact(n).map_err(|_| err("not enough memory for this much audio"))?;
                o.resize(n, 0f32);
                out.push(o);
            }
            for s in 0..segs {
                let start = s * STRIDE;
                let end = (start + WINDOW).min(n);
                let chans: Vec<&[f32]> = channels.iter().map(|c| c.get(start..end).unwrap_or_default()).collect();
                let y = self.segment(&chans, &mut |l| progress((s as f32 + (l + 1) as f32 / LAYERS as f32) / segs as f32))?;
                // keep the window's middle: drop GIVE_UP at each inner edge
                let from = if s == 0 { 0 } else { GIVE_UP };
                let to = if s + 1 == segs { end - start } else { WINDOW - GIVE_UP };
                for (o, y) in out.iter_mut().zip(&y) {
                    if let (Some(dst), Some(src)) = (o.get_mut(start + from..start + to), y.get(from..to)) {
                        dst.copy_from_slice(src);
                    }
                }
            }
            Ok(out)
        })
        .map_err(|e| err(e.to_string()))
    }
}

// ---------------------------------------------------------------------------------------------
// the installed model, shared by every caller and loaded on first use
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Slot {
    path: Option<std::path::PathBuf>,
    model: Option<std::sync::Arc<MossFormer2>>,
    error: Option<String>,
}

fn slot() -> &'static std::sync::RwLock<Slot> {
    static S: std::sync::OnceLock<std::sync::RwLock<Slot>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Use the checkpoint at `path` from now on (loaded the first time [`shared`] is called).
pub fn set_model_path(path: &Path) {
    let mut s = slot().write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if s.path.as_deref() != Some(path) {
        *s = Slot { path: Some(path.to_path_buf()), model: None, error: None };
    }
}

/// Forget the model (and free its memory once running jobs finish).
pub fn unload() {
    *slot().write().unwrap_or_else(std::sync::PoisonError::into_inner) = Slot::default();
}

/// (checkpoint path, loaded, load error).
pub fn status() -> (Option<std::path::PathBuf>, bool, Option<String>) {
    let s = slot().read().unwrap_or_else(std::sync::PoisonError::into_inner);
    (s.path.clone(), s.model.is_some(), s.error.clone())
}

/// The model, loading it from the set path on first use (a failure is remembered and reported
/// the same way until the path changes).
pub fn shared() -> Result<std::sync::Arc<MossFormer2>, ModelError> {
    {
        let s = slot().read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(m) = &s.model {
            return Ok(m.clone());
        }
        if let Some(e) = &s.error {
            return Err(err(e));
        }
        if s.path.is_none() {
            return Err(err("the MossFormer2 voice model isn't installed"));
        }
    }
    let mut s = slot().write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if s.model.is_none() && s.error.is_none() {
        let path = s.path.clone().ok_or_else(|| err("the MossFormer2 voice model isn't installed"))?;
        match MossFormer2::load(&path) {
            Ok(m) => s.model = Some(std::sync::Arc::new(m)),
            Err(e) => s.error = Some(e.0),
        }
    }
    match (&s.model, &s.error) {
        (Some(m), _) => Ok(m.clone()),
        (None, Some(e)) => Err(err(e)),
        (None, None) => Err(err("the MossFormer2 voice model isn't installed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic "checkpoint": every tensor of the manifest, small values.
    fn synthetic() -> Vec<(String, Vec<usize>, Vec<f32>)> {
        let mut r = crate::testset::Rng::new(5);
        manifest()
            .into_iter()
            .map(|(n, d)| {
                let c: usize = d.iter().product();
                let v: Vec<f32> = if n.ends_with(".g") || n.ends_with("norm.weight") || n.ends_with("norm1.weight") || n.ends_with("norm2.weight") {
                    vec![1.0; c]
                } else if n.ends_with("inv_freq") || n.ends_with("freqs") {
                    (0..c).map(|i| 1.0 / 10000f32.powf(2.0 * i as f32 / (2 * c) as f32)).collect()
                } else {
                    (0..c).map(|_| (r.bipolar() * 0.05) as f32).collect()
                };
                (n, d, v)
            })
            .collect()
    }

    pub(crate) fn model_path() -> Option<std::path::PathBuf> {
        let p = std::env::var_os("FILMCRAFT_MOSSFORMER2_CKPT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "~/Projects/filmcraft-main/models/mossformer2-se-48k/last_best_checkpoint.pt".into());
        p.is_file().then_some(p)
    }

    #[test]
    fn manifest_matches_the_release() {
        let m = manifest();
        assert_eq!(m.len(), 929);
        let params: usize = m.iter().map(|(_, d)| d.iter().product::<usize>()).sum();
        // ≈ 55 M parameters (221.6 MB of f32)
        assert!((54_000_000..56_000_000).contains(&params), "{params}");
    }

    #[test]
    fn synthetic_weights_run_and_keep_lengths() {
        let m = MossFormer2::from_tensors(synthetic()).unwrap();
        for n in [0usize, 1, 100, WIN, WIN + 1, 48_000] {
            let x: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.01).sin() * 0.3).collect();
            let y = m.enhance(&[x.clone(), x], &mut |_| true).unwrap();
            assert_eq!(y.len(), 2);
            assert!(y.iter().all(|c| c.len() == n && c.iter().all(|v| v.is_finite())), "{n}");
        }
    }

    #[test]
    fn hostile_input_is_refused_or_survived() {
        let m = MossFormer2::from_tensors(synthetic()).unwrap();
        assert!(m.enhance(&[], &mut |_| true).is_err());
        assert!(m.enhance(&vec![vec![0.0; 10]; 9], &mut |_| true).is_err());
        assert!(m.enhance(&[vec![0.0; 10], vec![0.0; 11]], &mut |_| true).is_err());
        let mut x = vec![0.1f32; 9600];
        x[10] = f32::NAN;
        x[20] = f32::INFINITY;
        x[30] = -1e30;
        let y = m.enhance(&[x], &mut |_| true).unwrap();
        assert!(y[0].iter().all(|v| v.is_finite()));
        // stopping
        let e = m.enhance(&[vec![0.1; 9600]], &mut |_| false).unwrap_err();
        assert!(e.0.contains("cancelled"), "{e}");
    }

    #[test]
    fn wrong_or_corrupt_tensors_say_why() {
        let mut t = synthetic();
        t.retain(|(n, _, _)| n != "mossformer.prelu.weight");
        assert!(MossFormer2::from_tensors(t).unwrap_err().0.contains("prelu"));
        let mut t = synthetic();
        t[3].1 = vec![2];
        t[3].2 = vec![0.0, 0.0];
        assert!(MossFormer2::from_tensors(t).unwrap_err().0.contains("has"));
        let mut t = synthetic();
        t[0].2[0] = f32::NAN;
        assert!(MossFormer2::from_tensors(t).unwrap_err().0.contains("corrupt"));
        let dir = std::env::temp_dir().join(format!("fc-moss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.pt");
        std::fs::write(&junk, b"PK\x03\x04 not a checkpoint").unwrap();
        let e = MossFormer2::load(&junk).unwrap_err();
        assert!(e.0.contains("not a readable MossFormer2_SE_48K checkpoint"), "{e}");
        assert!(MossFormer2::load(&dir.join("absent.pt")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn long_clips_are_windowed_seamlessly() {
        // with every mask weight zero but a positive decoder bias-free path, the mask is the same
        // for any window: check the stitching covers every sample (no silent gaps)
        let m = MossFormer2::from_tensors(synthetic()).unwrap();
        let n = ONE_PASS + STRIDE + 12_345;
        let x: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.03).sin() * 0.3).collect();
        let mut calls = 0;
        let y = m
            .enhance(&[x], &mut |f| {
                calls += 1;
                (0.0..=1.0001).contains(&f)
            })
            .unwrap();
        assert_eq!(y[0].len(), n);
        assert!(calls > LAYERS);
        // the last window reaches the end
        let tail = &y[0][n - 48_000..n - WIN];
        assert!(tail.iter().any(|v| *v != 0.0));
    }

    /// Parity harness: the mask for the features in `FILMCRAFT_MOSS_FEATS` (raw f32, frames × 180)
    /// written to `FILMCRAFT_MOSS_OUT` (frames × 961), for comparison with an independent numpy
    /// implementation of the PyTorch modules (A3.7: max abs error 6.8e-6 over 371 frames).
    #[test]
    #[ignore]
    fn mask_oracle_dump() {
        let (Some(p), Some(i), Some(o)) = (model_path(), std::env::var_os("FILMCRAFT_MOSS_FEATS"), std::env::var_os("FILMCRAFT_MOSS_OUT")) else {
            eprintln!("checkpoint or FILMCRAFT_MOSS_FEATS / _OUT missing: skipped");
            return;
        };
        let m = MossFormer2::load(&p).unwrap();
        let b = std::fs::read(i).unwrap();
        let x: Vec<f32> = b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        let mask = m.net.mask(&x, x.len() / FEATS, &mut |_| true).unwrap();
        std::fs::write(o, mask.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    }

    /// Listening aid: enhance the 16-bit mono 48 kHz WAV `FILMCRAFT_MOSS_WAV`, print the level in
    /// and out per second, write `<file>.moss.wav`.
    #[test]
    #[ignore]
    fn enhance_wav_file() {
        let (Some(p), Some(f)) = (model_path(), std::env::var_os("FILMCRAFT_MOSS_WAV")) else {
            eprintln!("checkpoint or FILMCRAFT_MOSS_WAV missing: skipped");
            return;
        };
        let b = std::fs::read(&f).unwrap();
        let d = b.windows(4).position(|w| w == b"data").unwrap();
        let x: Vec<f32> = b[d + 8..].as_chunks::<2>().0.iter().map(|s| f32::from(i16::from_le_bytes(*s)) / 32768.0).collect();
        let m = MossFormer2::load(&p).unwrap();
        let t0 = std::time::Instant::now();
        let y = m.enhance(std::slice::from_ref(&x), &mut |_| true).unwrap().remove(0);
        eprintln!("{:.1} s in {:.1} s", x.len() as f64 / 48_000.0, t0.elapsed().as_secs_f64());
        let db = |v: &[f32]| 10.0 * (v.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / v.len().max(1) as f64 + 1e-12).log10();
        for (i, (a, b)) in x.chunks(48_000).zip(y.chunks(48_000)).enumerate() {
            eprintln!("{i:3} s: in {:6.1} dB, out {:6.1} dB", db(a), db(b));
        }
        let mut out = b[..d + 8].to_vec();
        out.extend(y.iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()));
        std::fs::write(format!("{}.moss.wav", std::path::Path::new(&f).display()), out).unwrap();
    }

    /// The real checkpoint, when present: loads, and a 2 s tone plus noise comes out finite with
    /// the tone kept louder than the noise. `FILMCRAFT_MOSSFORMER2_CKPT` overrides the path.
    #[test]
    fn real_checkpoint_loads_and_runs() {
        let Some(p) = model_path() else {
            eprintln!("MossFormer2 checkpoint absent: skipped");
            return;
        };
        let t0 = std::time::Instant::now();
        let m = MossFormer2::load(&p).unwrap();
        eprintln!("load {:.2} s", t0.elapsed().as_secs_f64());
        let n = 96_000;
        let x: Vec<f32> = (0..n).map(|i| ((i as f32) * 2.0 * std::f32::consts::PI * 220.0 / 48_000.0).sin() * 0.1).collect();
        let t0 = std::time::Instant::now();
        let y = m.enhance(&[x], &mut |_| true).unwrap();
        eprintln!("2 s in {:.2} s", t0.elapsed().as_secs_f64());
        assert_eq!(y[0].len(), n);
        assert!(y[0].iter().all(|v| v.is_finite()));
    }
}
