//! MobileSAM's TinyViT-5M image encoder step by step on the CPU (candle ops), so encoders
//! written elsewhere (the GPU one in `filmcraft-gpu`) can be checked stage by stage.
//!
//! The forward pass is candle-transformers 0.9's `segment_anything::tiny_vit` (MIT / Apache-2.0)
//! op for op, with the stage outputs kept; a test checks its final output equals candle's.
//! Test support only (hidden from the docs): it loads the checkpoint on every call.

use std::path::Path;

use candle_core::{D, DType, Device, IndexOp, Tensor};
use candle_nn::{Conv2dConfig, VarBuilder};

use crate::MaskError;

type R<T> = candle_core::Result<T>;

/// The stage names [`stages`] returns, in order, with their layouts.
/// `patch_embed`: (1, 64, 256, 256); `layer0`: (1, 16384, 128) tokens; `layer1`: (1, 4096, 160);
/// `layer2`, `layer3`: (1, 4096, 320); `neck`: (1, 256, 64, 64).
pub const STAGES: [&str; 6] = ["patch_embed", "layer0", "layer1", "layer2", "layer3", "neck"];

const EMBED_DIMS: [usize; 4] = [64, 128, 160, 320];
const DEPTHS: [usize; 4] = [2, 2, 6, 2];
const HEADS: [usize; 4] = [2, 4, 5, 10];
const WINDOWS: [usize; 4] = [7, 7, 14, 7];

fn conv_bn(vb: VarBuilder, x: &Tensor, cin: usize, cout: usize, ks: usize, stride: usize, padding: usize, groups: usize) -> R<Tensor> {
    let cfg = Conv2dConfig { stride, padding, groups, ..Default::default() };
    let c = candle_nn::conv2d_no_bias(cin, cout, ks, cfg, vb.pp("c"))?;
    let bn = candle_nn::batch_norm(cout, 1e-5, vb.pp("bn"))?;
    x.apply(&c)?.apply_t(&bn, false)
}

fn mbconv(vb: VarBuilder, x: &Tensor, dim: usize) -> R<Tensor> {
    let hidden = dim * 4;
    let y = conv_bn(vb.pp("conv1"), x, dim, hidden, 1, 1, 0, 1)?.gelu()?;
    let y = conv_bn(vb.pp("conv2"), &y, hidden, hidden, 3, 1, 1, hidden)?.gelu()?;
    let y = conv_bn(vb.pp("conv3"), &y, hidden, dim, 1, 1, 0, 1)?;
    (y + x)?.gelu()
}

fn patch_merging(vb: VarBuilder, x: &Tensor, res: usize, dim: usize, out: usize) -> R<Tensor> {
    let x = if x.rank() == 3 {
        let b = x.dim(0)?;
        x.reshape((b, res, res, ()))?.permute((0, 3, 1, 2))?
    } else {
        x.clone()
    };
    let stride = if [320, 448, 576].contains(&out) { 1 } else { 2 };
    let y = conv_bn(vb.pp("conv1"), &x, dim, out, 1, 1, 0, 1)?.gelu()?;
    let y = conv_bn(vb.pp("conv2"), &y, out, out, 3, stride, 1, out)?.gelu()?;
    conv_bn(vb.pp("conv3"), &y, out, out, 1, 1, 0, 1)?.flatten_from(2)?.transpose(1, 2)
}

fn attention(vb: VarBuilder, x: &Tensor, dim: usize, heads: usize, ws: usize) -> R<Tensor> {
    let key_dim = dim / heads;
    let norm = candle_nn::layer_norm(dim, 1e-5, vb.pp("norm"))?;
    let qkv = candle_nn::linear(dim, dim * 3, vb.pp("qkv"))?;
    let proj = candle_nn::linear(dim, dim, vb.pp("proj"))?;
    let points: Vec<(i64, i64)> = (0..ws).flat_map(|x| (0..ws).map(move |y| (x as i64, y as i64))).collect();
    let mut idxs = Vec::with_capacity(points.len() * points.len());
    let mut offsets = std::collections::HashMap::new();
    for &(x1, y1) in &points {
        for &(x2, y2) in &points {
            let l = offsets.len();
            let idx = offsets.entry(((x2 - x1).abs(), (y2 - y1).abs())).or_insert(l);
            idxs.push(*idx as u32);
        }
    }
    let biases = vb.get((heads, offsets.len()), "attention_biases")?;
    let idxs = Tensor::new(idxs, biases.device())?;
    let ab = biases.index_select(&idxs, 1)?.reshape(((), points.len(), points.len()))?;
    let (b, n, _) = x.dims3()?;
    let x = x.apply(&norm)?;
    let qkv = x.apply(&qkv)?.reshape((b, n, heads, ()))?;
    let q = qkv.narrow(D::Minus1, 0, key_dim)?.permute((0, 2, 1, 3))?.contiguous()?;
    let k = qkv.narrow(D::Minus1, key_dim, key_dim)?.permute((0, 2, 1, 3))?.contiguous()?;
    let v = qkv.narrow(D::Minus1, 2 * key_dim, key_dim)?.permute((0, 2, 1, 3))?.contiguous()?;
    let attn = (q.matmul(&k.t()?)? * (1f64 / (key_dim as f64).sqrt()))?;
    let attn = attn.broadcast_add(&ab)?;
    let attn = candle_nn::ops::softmax_last_dim(&attn)?;
    attn.matmul(&v)?.transpose(1, 2)?.reshape((b, n, dim))?.apply(&proj)
}

fn block(vb: VarBuilder, x: &Tensor, dim: usize, res: usize, heads: usize, ws: usize) -> R<Tensor> {
    let (h, w) = (res, res);
    let (b, l, c) = x.dims3()?;
    let y = if h == ws && w == ws {
        attention(vb.pp("attn"), x, dim, heads, ws)?
    } else {
        let y = x.reshape((b, h, w, c))?;
        let pad_b = (ws - h % ws) % ws;
        let pad_r = (ws - w % ws) % ws;
        let y = if pad_b > 0 { y.pad_with_zeros(1, 0, pad_b)? } else { y };
        let y = if pad_r > 0 { y.pad_with_zeros(2, 0, pad_r)? } else { y };
        let (p_h, p_w) = (h + pad_b, w + pad_r);
        let (n_h, n_w) = (p_h / ws, p_w / ws);
        let y = y.reshape((b, n_h, ws, n_w, ws, c))?.transpose(2, 3)?.reshape((b * n_h * n_w, ws * ws, c))?;
        let y = attention(vb.pp("attn"), &y, dim, heads, ws)?;
        let y = y.reshape((b, n_h, n_w, ws, ws, c))?.transpose(2, 3)?.reshape((b, p_h, p_w, c))?;
        let y = if pad_r > 0 { y.i((.., .., ..w))?.contiguous()? } else { y };
        let y = if pad_b > 0 { y.i((.., ..h, ..))?.contiguous()? } else { y };
        y.reshape((b, l, c))?
    };
    let y = (y + x)?;
    let y = y.transpose(1, 2)?.reshape((b, c, h, w))?;
    let y = conv_bn(vb.pp("local_conv"), &y, dim, dim, 3, 1, 1, dim)?.reshape((b, c, l))?.transpose(1, 2)?;
    let mlp = vb.pp("mlp");
    let norm = candle_nn::layer_norm(dim, 1e-5, mlp.pp("norm"))?;
    let fc1 = candle_nn::linear(dim, dim * 4, mlp.pp("fc1"))?;
    let fc2 = candle_nn::linear(dim * 4, dim, mlp.pp("fc2"))?;
    let m = y.apply(&norm)?.apply(&fc1)?.gelu()?.apply(&fc2)?;
    &y + m
}

fn layer_norm_2d(vb: VarBuilder, x: &Tensor, c: usize) -> R<Tensor> {
    let weight = vb.get(c, "weight")?;
    let bias = vb.get(c, "bias")?;
    let u = x.mean_keepdim(1)?;
    let x = x.broadcast_sub(&u)?;
    let s = x.sqr()?.mean_keepdim(1)?;
    let x = x.broadcast_div(&(s + 1e-6)?.sqrt()?)?;
    x.broadcast_mul(&weight.reshape((1, c, 1, 1))?)?.broadcast_add(&bias.reshape((1, c, 1, 1))?)
}

fn forward(vb: VarBuilder, x: &Tensor) -> R<Vec<(String, Tensor)>> {
    let mut out = Vec::new();
    let pe = vb.pp("patch_embed");
    let x = conv_bn(pe.pp("seq.0"), x, 3, 32, 3, 2, 1, 1)?.gelu()?;
    let mut x = conv_bn(pe.pp("seq.2"), &x, 32, 64, 3, 2, 1, 1)?;
    out.push(("patch_embed".to_string(), x.clone()));
    let l0 = vb.pp("layers.0");
    for i in 0..DEPTHS[0] {
        x = mbconv(l0.pp("blocks").pp(i), &x, EMBED_DIMS[0])?;
    }
    x = patch_merging(l0.pp("downsample"), &x, 256, EMBED_DIMS[0], EMBED_DIMS[1])?;
    out.push(("layer0".to_string(), x.clone()));
    for li in 1..4 {
        let res = 256 / (1 << li.min(2));
        let vl = vb.pp(format!("layers.{li}"));
        for i in 0..DEPTHS[li] {
            x = block(vl.pp("blocks").pp(i), &x, EMBED_DIMS[li], res, HEADS[li], WINDOWS[li])?;
        }
        if li < 3 {
            x = patch_merging(vl.pp("downsample"), &x, res, EMBED_DIMS[li], EMBED_DIMS[li + 1])?;
        }
        out.push((format!("layer{li}"), x.clone()));
    }
    let (b, _, c) = x.dims3()?;
    let x = x.reshape((b, 64, 64, c))?.permute((0, 3, 1, 2))?;
    let n0 = candle_nn::conv2d_no_bias(c, 256, 1, Default::default(), vb.pp("neck.0"))?;
    let x = layer_norm_2d(vb.pp("neck.1"), &x.apply(&n0)?, 256)?;
    let n2 = candle_nn::conv2d_no_bias(256, 256, 3, Conv2dConfig { padding: 1, ..Default::default() }, vb.pp("neck.2"))?;
    let x = layer_norm_2d(vb.pp("neck.3"), &x.apply(&n2)?, 256)?;
    out.push(("neck".to_string(), x));
    Ok(out)
}

/// Every stage's output for the (1, 3, 1024, 1024) `input`, flattened in the layouts of
/// [`STAGES`].
pub fn stages(path: &Path, input: &Tensor, device: &Device) -> Result<Vec<(String, Vec<f32>)>, MaskError> {
    let failed = |e: candle_core::Error| MaskError::Failed(e.to_string());
    let vb = VarBuilder::from_pth(path, DType::F32, device).map_err(failed)?;
    let outs = forward(vb.pp("image_encoder"), input).map_err(failed)?;
    outs.into_iter().map(|(n, t)| Ok((n, t.flatten_all().and_then(|t| t.to_vec1()).map_err(failed)?))).collect()
}

#[cfg(test)]
mod tests {
    /// The step-by-step encoder gives candle's TinyViT output exactly. Skips without the model.
    #[test]
    fn reference_stages_end_in_candles_output() {
        let Some(sam) = crate::tests::real_model() else { return };
        let input: Vec<f32> = (0..3 * 1024 * 1024).map(|i| ((i as f32 * 0.618).fract() - 0.5) * 3.0).collect();
        let theirs = sam.encode_cpu(&input).unwrap();
        let stages = sam.encoder_stages_cpu(&input).unwrap();
        assert_eq!(stages.len(), super::STAGES.len());
        let ours = &stages.last().unwrap().1;
        let worst = theirs.iter().zip(ours).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert_eq!(theirs.len(), ours.len());
        assert!(worst < 1e-4, "max abs diff {worst}");
    }
}
