//! SAM 2.1's heavy parts on the CPU via candle, op for op as the reference PyTorch modules
//! (`hieradet.py`, `image_encoder.py`, `memory_attention.py`, `memory_encoder.py`,
//! `transformer.py`'s `RoPEAttention`). The parity oracle for the GPU passes and the fallback
//! when there is no usable GPU (slow: seconds per frame for Large).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use candle_core::{D, Device, IndexOp, Tensor};

use super::config::{BlockSpec, D_MODEL, EMBED_SIDE, IMAGE_SIZE, MEM_DIM};
use super::weights::Sam2Weights;
use super::{EMBED_TOKENS, MEM_MASK_SIDE, Sam2Compute, Sam2Features, apply_rope, hiera_position, rope_tables, sine_position};
use crate::MaskError;

pub(crate) type R<T> = candle_core::Result<T>;

pub(crate) fn failed(e: candle_core::Error) -> MaskError {
    MaskError::Failed(e.to_string())
}

/// Run model code under `catch_unwind` (candle may panic on shapes it doesn't expect).
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, MaskError>) -> Result<T, MaskError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => Err(MaskError::Failed(format!("{what} stopped unexpectedly ({})", filmcraft_ml::job::panic_message(p.as_ref())))),
    }
}

/// `x · wᵀ + b` over the last dim (PyTorch `Linear`).
pub(crate) fn linear(x: &Tensor, w: &Tensor, b: &Tensor) -> R<Tensor> {
    x.broadcast_matmul(&w.t()?)?.broadcast_add(b)
}

/// LayerNorm over the last dim.
pub(crate) fn layer_norm(x: &Tensor, g: &Tensor, b: &Tensor, eps: f64) -> R<Tensor> {
    let mean = x.mean_keepdim(D::Minus1)?;
    let xc = x.broadcast_sub(&mean)?;
    let var = xc.sqr()?.mean_keepdim(D::Minus1)?;
    xc.broadcast_div(&(var + eps)?.sqrt()?)?.broadcast_mul(g)?.broadcast_add(b)
}

/// LayerNorm2d on NCHW (over channels).
pub(crate) fn layer_norm2d(x: &Tensor, g: &Tensor, b: &Tensor, eps: f64) -> R<Tensor> {
    let y = x.permute((0, 2, 3, 1))?;
    layer_norm(&y, g, b, eps)?.permute((0, 3, 1, 2))?.contiguous()
}

/// Conv2d (+ bias) on NCHW.
pub(crate) fn conv(x: &Tensor, w: &Tensor, b: &Tensor, pad: usize, stride: usize, groups: usize) -> R<Tensor> {
    let c = b.dims1()?;
    x.conv2d(w, pad, stride, 1, groups)?.broadcast_add(&b.reshape((1, c, 1, 1))?)
}

/// Softmax attention: q (…, nq, d), k / v (…, nk, d).
pub(crate) fn sdpa(q: &Tensor, k: &Tensor, v: &Tensor) -> R<Tensor> {
    let d = q.dim(D::Minus1)?;
    let s = (q.contiguous()?.matmul(&k.contiguous()?.t()?)? / (d as f64).sqrt())?;
    candle_nn::ops::softmax_last_dim(&s)?.matmul(&v.contiguous()?)
}

/// 2 × 2 max pool of NHWC.
fn pool_nhwc(x: &Tensor) -> R<Tensor> {
    x.permute((0, 3, 1, 2))?.contiguous()?.max_pool2d(2)?.permute((0, 2, 3, 1))?.contiguous()
}

/// `window_partition`: NHWC (B, H, W, C) → (B·nW, ws, ws, C), zero-padded; and the padded size.
fn window_partition(x: &Tensor, ws: usize) -> R<(Tensor, (usize, usize))> {
    let (b, h, w, c) = x.dims4()?;
    let (ph, pw) = ((ws - h % ws) % ws, (ws - w % ws) % ws);
    let mut x = x.clone();
    if ph > 0 {
        x = x.pad_with_zeros(1, 0, ph)?;
    }
    if pw > 0 {
        x = x.pad_with_zeros(2, 0, pw)?;
    }
    let (hp, wp) = (h + ph, w + pw);
    let x = x.reshape((b, hp / ws, ws, wp / ws, ws, c))?.permute((0, 1, 3, 2, 4, 5))?.contiguous()?.reshape(((), ws, ws, c))?;
    Ok((x, (hp, wp)))
}

/// `window_unpartition`: back to (B, H, W, C), dropping the padding.
fn window_unpartition(x: &Tensor, ws: usize, pad: (usize, usize), hw: (usize, usize)) -> R<Tensor> {
    let (hp, wp) = pad;
    let c = x.dim(D::Minus1)?;
    let n = x.dim(0)?;
    let b = n / ((hp / ws) * (wp / ws)).max(1);
    let x = x.reshape((b, hp / ws, wp / ws, ws, ws, c))?.permute((0, 1, 3, 2, 4, 5))?.contiguous()?.reshape((b, hp, wp, c))?;
    x.narrow(1, 0, hw.0)?.narrow(2, 0, hw.1)?.contiguous()
}

/// The CPU reference.
pub struct Sam2Cpu {
    w: Arc<Sam2Weights>,
    dev: Device,
    /// Hiera's position embedding, (1, 256, 256, C).
    pos: Tensor,
    /// The 64 × 64 sine encoding of the image embedding (1, 4096, 256).
    vision_pos: Tensor,
    rope_cos: Vec<f32>,
    rope_sin: Vec<f32>,
}

impl std::fmt::Debug for Sam2Cpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sam2Cpu")
    }
}

/// Names of the stage outputs [`Sam2Cpu::image_stages`] returns (HWC each): the patch embedding
/// with its position embedding, the four trunk stages, then `s0`, `s1`, `feat`.
pub const IMAGE_STAGES: [&str; 8] = ["patch_embed", "stage0", "stage1", "stage2", "stage3", "s0", "s1", "feat"];

impl Sam2Cpu {
    pub fn new(w: Arc<Sam2Weights>) -> Result<Self, MaskError> {
        let dev = w.device();
        let cfg = &w.cfg;
        let c = cfg.embed_dim;
        let bkg = w.plain("image_encoder.trunk.pos_embed")?;
        let win = w.plain("image_encoder.trunk.pos_embed_window")?;
        let pos = hiera_position(&bkg.data, cfg.pos_bkg.0, cfg.pos_bkg.1, &win.data, cfg.window_spec[0], c);
        let pos = Tensor::from_vec(pos, (1, 256, 256, c), &dev).map_err(failed)?;
        let vp = sine_position(EMBED_SIDE, EMBED_SIDE, D_MODEL);
        let vision_pos = Tensor::from_vec(vp, (1, EMBED_TOKENS, D_MODEL), &dev).map_err(failed)?;
        let (rope_cos, rope_sin) = rope_tables(EMBED_SIDE, D_MODEL, cfg.rope_theta);
        Ok(Self { w, dev, pos, vision_pos, rope_cos, rope_sin })
    }

    fn t(&self, name: &str) -> R<Tensor> {
        self.w.get(name).cloned().map_err(|e| candle_core::Error::Msg(e.to_string()))
    }

    fn lin(&self, x: &Tensor, p: &str) -> R<Tensor> {
        linear(x, &self.t(&format!("{p}.weight"))?, &self.t(&format!("{p}.bias"))?)
    }

    fn ln(&self, x: &Tensor, p: &str, eps: f64) -> R<Tensor> {
        layer_norm(x, &self.t(&format!("{p}.weight"))?, &self.t(&format!("{p}.bias"))?, eps)
    }

    fn ln2d(&self, x: &Tensor, p: &str, eps: f64) -> R<Tensor> {
        layer_norm2d(x, &self.t(&format!("{p}.weight"))?, &self.t(&format!("{p}.bias"))?, eps)
    }

    fn conv(&self, x: &Tensor, p: &str, pad: usize, stride: usize, groups: usize) -> R<Tensor> {
        conv(x, &self.t(&format!("{p}.weight"))?, &self.t(&format!("{p}.bias"))?, pad, stride, groups)
    }

    /// `MultiScaleAttention` on (B, H, W, dim) windows.
    fn attn(&self, p: &str, s: &BlockSpec, x: &Tensor) -> R<Tensor> {
        let (b, h, w, _) = x.dims4()?;
        let hd = s.dim_out / s.heads.max(1);
        let qkv = self.lin(x, &format!("{p}.qkv"))?.reshape((b, h * w, 3, s.heads, hd))?;
        let (q, k, v) = (qkv.i((.., .., 0))?, qkv.i((.., .., 1))?, qkv.i((.., .., 2))?);
        let (q, h, w) = if s.pool {
            let q = pool_nhwc(&q.reshape((b, h, w, s.dim_out))?)?;
            let (_, h2, w2, _) = q.dims4()?;
            (q.reshape((b, h2 * w2, s.heads, hd))?, h2, w2)
        } else {
            (q, h, w)
        };
        let o = sdpa(&q.transpose(1, 2)?, &k.transpose(1, 2)?, &v.transpose(1, 2)?)?;
        let o = o.transpose(1, 2)?.contiguous()?.reshape((b, h, w, s.dim_out))?;
        self.lin(&o, &format!("{p}.proj"))
    }

    /// `MultiScaleBlock` on NHWC.
    fn block(&self, i: usize, s: &BlockSpec, x: &Tensor) -> R<Tensor> {
        let p = format!("image_encoder.trunk.blocks.{i}");
        let (_, h, w, _) = x.dims4()?;
        let xn = self.ln(x, &format!("{p}.norm1"), 1e-6)?;
        let mut shortcut = x.clone();
        if s.dim != s.dim_out {
            shortcut = self.lin(&xn, &format!("{p}.proj"))?;
            if s.pool {
                shortcut = pool_nhwc(&shortcut)?;
            }
        }
        let (win, mut pad) = if s.window > 0 { window_partition(&xn, s.window)? } else { (xn, (h, w)) };
        let a = self.attn(&format!("{p}.attn"), s, &win)?;
        let (mut ws, mut hw) = (s.window, (h, w));
        if s.pool {
            ws = s.window / 2;
            let (_, sh, sw, _) = shortcut.dims4()?;
            hw = (sh, sw);
            if ws > 0 {
                pad = (sh + (ws - sh % ws) % ws, sw + (ws - sw % ws) % ws);
            }
        }
        let a = if s.window > 0 { window_unpartition(&a, ws, pad, hw)? } else { a };
        let x = (shortcut + a)?;
        let m = self.ln(&x, &format!("{p}.norm2"), 1e-6)?;
        let m = self.lin(&m, &format!("{p}.mlp.layers.0"))?.gelu_erf()?;
        let m = self.lin(&m, &format!("{p}.mlp.layers.1"))?;
        x + m
    }

    /// The image encoder with every stage kept: see [`IMAGE_STAGES`].
    fn image_all(&self, input: &[f32], keep: bool) -> R<(Sam2Features, Vec<(String, Vec<f32>)>)> {
        let cfg = &self.w.cfg;
        if input.len() != 3 * IMAGE_SIZE * IMAGE_SIZE {
            candle_core::bail!("an encoder input of {} values", input.len());
        }
        let mut stages = Vec::new();
        let plain = |t: &Tensor| -> R<Vec<f32>> { t.flatten_all()?.to_vec1::<f32>() };
        let x = Tensor::from_slice(input, (1, 3, IMAGE_SIZE, IMAGE_SIZE), &self.dev)?;
        let x = self.conv(&x, "image_encoder.trunk.patch_embed.proj", 3, 4, 1)?;
        let mut x = x.permute((0, 2, 3, 1))?.contiguous()?.broadcast_add(&self.pos)?;
        if keep {
            stages.push((IMAGE_STAGES[0].to_string(), plain(&x)?));
        }
        let ends = cfg.stage_ends();
        let mut outs = Vec::with_capacity(4);
        for (i, s) in cfg.blocks().iter().enumerate() {
            x = self.block(i, s, &x)?;
            if ends.contains(&i) {
                if keep {
                    stages.push((IMAGE_STAGES.get(1 + outs.len()).copied().unwrap_or("stage").to_string(), plain(&x)?));
                }
                outs.push(x.permute((0, 3, 1, 2))?.contiguous()?);
            }
        }
        // FPN neck: convs[3 − i] on stage i, top-down (nearest ×2) into levels 2 and 3 only
        let mut prev: Option<Tensor> = None;
        let mut levels: Vec<Option<Tensor>> = vec![None; 4];
        for i in (0..4).rev() {
            let Some(xi) = outs.get(i) else { candle_core::bail!("the trunk gave {} stages", outs.len()) };
            let lat = self.conv(xi, &format!("image_encoder.neck.convs.{}.conv", 3 - i), 0, 1, 1)?;
            let cur = match (&prev, i >= 2) {
                (Some(p), true) => {
                    let (_, _, h, w) = p.dims4()?;
                    (lat + p.upsample_nearest2d(h * 2, w * 2)?)?
                }
                _ => lat,
            };
            if let Some(slot) = levels.get_mut(i) {
                *slot = Some(cur.clone());
            }
            prev = Some(cur);
        }
        let level = |i: usize| -> R<Tensor> { levels.get(i).cloned().flatten().ok_or_else(|| candle_core::Error::Msg("neck level missing".into())) };
        let s0 = self.conv(&level(0)?, "sam_mask_decoder.conv_s0", 0, 1, 1)?;
        let s1 = self.conv(&level(1)?, "sam_mask_decoder.conv_s1", 0, 1, 1)?;
        let hwc = |t: &Tensor| -> R<Vec<f32>> { plain(&t.permute((0, 2, 3, 1))?.contiguous()?) };
        let f = Sam2Features { s0: hwc(&s0)?, s1: hwc(&s1)?, feat: hwc(&level(2)?)? };
        if keep {
            stages.push(("s0".into(), f.s0.clone()));
            stages.push(("s1".into(), f.s1.clone()));
            stages.push(("feat".into(), f.feat.clone()));
        }
        Ok((f, stages))
    }

    /// Every stage of the image encoder (parity tests).
    pub fn image_stages(&self, input: &[f32]) -> Result<Vec<(String, Vec<f32>)>, MaskError> {
        guarded("the image encoder", || self.image_all(input, true).map(|(_, s)| s).map_err(failed))
    }

    /// `RoPEAttention` (one head) with the tables; `rope_k`: how many key rows get RoPE.
    fn rope_attn(&self, p: &str, q: &Tensor, k: &Tensor, v: &Tensor, rope_k: usize) -> R<Tensor> {
        let q = self.lin(q, &format!("{p}.q_proj"))?;
        let k = self.lin(k, &format!("{p}.k_proj"))?;
        let v = self.lin(v, &format!("{p}.v_proj"))?;
        let (_, nq, d) = q.dims3()?;
        let nk = k.dim(1)?;
        let mut qv = q.flatten_all()?.to_vec1::<f32>()?;
        apply_rope(&mut qv, nq, d, &self.rope_cos, &self.rope_sin, EMBED_TOKENS);
        let mut kv = k.flatten_all()?.to_vec1::<f32>()?;
        apply_rope(&mut kv, rope_k.min(nk), d, &self.rope_cos, &self.rope_sin, EMBED_TOKENS);
        let q = Tensor::from_vec(qv, (1, nq, d), &self.dev)?;
        let k = Tensor::from_vec(kv, (1, nk, d), &self.dev)?;
        let o = sdpa(&q, &k, &v)?;
        self.lin(&o, &format!("{p}.out_proj"))
    }

    fn mem_attn(&self, feat: &[f32], memory: &[f32], memory_pos: &[f32], ptr_tokens: usize) -> R<Vec<f32>> {
        let n = memory.len() / MEM_DIM;
        if feat.len() != EMBED_TOKENS * D_MODEL || n == 0 || memory.len() != n * MEM_DIM || memory_pos.len() != memory.len() || ptr_tokens > n {
            candle_core::bail!("memory attention inputs of {} / {} / {} values ({ptr_tokens} pointer tokens)", feat.len(), memory.len(), memory_pos.len());
        }
        let rope_k = n - ptr_tokens;
        if !rope_k.is_multiple_of(EMBED_TOKENS) {
            candle_core::bail!("{rope_k} memory tokens are not whole frames");
        }
        let curr = Tensor::from_slice(feat, (1, EMBED_TOKENS, D_MODEL), &self.dev)?;
        let mem = Tensor::from_slice(memory, (1, n, MEM_DIM), &self.dev)?;
        let mpos = Tensor::from_slice(memory_pos, (1, n, MEM_DIM), &self.dev)?;
        let mut x = (curr + (&self.vision_pos * 0.1)?)?;
        let mem_k = (&mem + &mpos)?;
        for l in 0..self.w.cfg.mem_layers {
            let p = format!("memory_attention.layers.{l}");
            let t2 = self.ln(&x, &format!("{p}.norm1"), 1e-5)?;
            x = (&x + self.rope_attn(&format!("{p}.self_attn"), &t2, &t2, &t2, EMBED_TOKENS)?)?;
            let t2 = self.ln(&x, &format!("{p}.norm2"), 1e-5)?;
            x = (&x + self.rope_attn(&format!("{p}.cross_attn_image"), &t2, &mem_k, &mem, rope_k)?)?;
            let t2 = self.ln(&x, &format!("{p}.norm3"), 1e-5)?;
            let t2 = self.lin(&self.lin(&t2, &format!("{p}.linear1"))?.relu()?, &format!("{p}.linear2"))?;
            x = (x + t2)?;
        }
        self.ln(&x, "memory_attention.norm", 1e-5)?.flatten_all()?.to_vec1::<f32>()
    }

    fn mem_enc(&self, feat: &[f32], mask: &[f32]) -> R<Vec<f32>> {
        if feat.len() != EMBED_TOKENS * D_MODEL || mask.len() != MEM_MASK_SIDE * MEM_MASK_SIDE {
            candle_core::bail!("memory encoder inputs of {} / {} values", feat.len(), mask.len());
        }
        let m = "memory_encoder";
        let mut x = Tensor::from_slice(mask, (1, 1, MEM_MASK_SIDE, MEM_MASK_SIDE), &self.dev)?;
        for i in 0..4 {
            x = self.conv(&x, &format!("{m}.mask_downsampler.encoder.{}", 3 * i), 1, 2, 1)?;
            x = self.ln2d(&x, &format!("{m}.mask_downsampler.encoder.{}", 3 * i + 1), 1e-6)?.gelu_erf()?;
        }
        let masks = self.conv(&x, &format!("{m}.mask_downsampler.encoder.12"), 0, 1, 1)?;
        let pix = Tensor::from_slice(feat, (1, EMBED_SIDE, EMBED_SIDE, D_MODEL), &self.dev)?.permute((0, 3, 1, 2))?.contiguous()?;
        let mut x = (self.conv(&pix, &format!("{m}.pix_feat_proj"), 0, 1, 1)? + masks)?;
        for l in 0..self.w.cfg.fuser_layers {
            let p = format!("{m}.fuser.layers.{l}");
            let y = self.conv(&x, &format!("{p}.dwconv"), 3, 1, D_MODEL)?;
            let y = self.ln2d(&y, &format!("{p}.norm"), 1e-6)?.permute((0, 2, 3, 1))?;
            let y = self.lin(&self.lin(&y, &format!("{p}.pwconv1"))?.gelu_erf()?, &format!("{p}.pwconv2"))?;
            let y = y.broadcast_mul(&self.t(&format!("{p}.gamma"))?)?.permute((0, 3, 1, 2))?;
            x = (x + y)?;
        }
        let x = self.conv(&x, &format!("{m}.out_proj"), 0, 1, 1)?;
        x.permute((0, 2, 3, 1))?.contiguous()?.flatten_all()?.to_vec1::<f32>()
    }
}

impl Sam2Compute for Sam2Cpu {
    fn name(&self) -> String {
        "CPU".into()
    }

    fn image(&self, input: &[f32]) -> Result<Sam2Features, MaskError> {
        let f = guarded("the image encoder", || self.image_all(input, false).map(|(f, _)| f).map_err(failed))?;
        f.check()?;
        Ok(f)
    }

    fn memory_attention(&self, feat: &[f32], memory: &[f32], memory_pos: &[f32], ptr_tokens: usize) -> Result<Vec<f32>, MaskError> {
        guarded("memory attention", || self.mem_attn(feat, memory, memory_pos, ptr_tokens).map_err(failed))
    }

    fn memory_encoder(&self, feat: &[f32], mask: &[f32]) -> Result<Vec<f32>, MaskError> {
        guarded("the memory encoder", || self.mem_enc(feat, mask).map_err(failed))
    }
}
