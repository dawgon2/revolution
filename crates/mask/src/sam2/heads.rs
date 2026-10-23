//! SAM 2's prompt encoder and mask decoder on the CPU via candle (`prompt_encoder.py`,
//! `mask_decoder.py`, `transformer.py`'s `TwoWayTransformer`, and `SAM2Base._forward_sam_heads`):
//! small next to the image encoder (a few GFLOP), so it runs here for every implementation.
//!
//! Inputs are the memory-conditioned embedding (64 × 64 × 256), the two high-resolution maps,
//! points in the 1024 square (box corners as labels 2 / 3, as the video predictor feeds them)
//! and optionally the frame's previous low-resolution logits as a mask prompt.

use std::sync::Arc;

use candle_core::{Device, IndexOp, Tensor};

use super::config::{D_MODEL, EMBED_SIDE, IMAGE_SIZE};
use super::cpu::{R, conv, failed, guarded, layer_norm, layer_norm2d, linear, sdpa};
use super::weights::Sam2Weights;
use super::{EMBED_TOKENS, S0_DIM, S0_SIDE, S1_DIM, S1_SIDE, dense_pe};
use crate::{LOW_RES, MaskError};

/// [`Sam2Heads::forward`]'s `prefer` applies only when the best-scored candidate overlaps the
/// preferred mask less than this.
pub const PREFER_GATE: f32 = 0.7;

/// SAM 2's "no object" logit (`NO_OBJ_SCORE`).
pub const NO_OBJ_SCORE: f32 = -1024.0;

/// A point for the prompt encoder, in the 1024 square, with SAM's label: 0 negative, 1
/// positive, 2 / 3 box corners, −1 padding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadPoint {
    pub x: f32,
    pub y: f32,
    pub label: i32,
}

/// What `_forward_sam_heads` returns for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadOutput {
    /// The chosen low-resolution mask logits (256 × 256), `NO_OBJ_SCORE` when the object is not
    /// on the frame.
    pub mask: Vec<f32>,
    pub iou: f32,
    /// The object pointer (256), already blended with `no_obj_ptr` when the object is absent.
    pub obj_ptr: Vec<f32>,
    pub object_score: f32,
    /// Multimask: each candidate's predicted IoU and its overlap (IoU) with the preferred mask
    /// (diagnostics).
    pub candidates: Vec<(f32, f32)>,
}

/// IoU of the insides (logit > 0) of two low-res masks.
pub fn mask_overlap(a: &[f32], b: &[f32]) -> f32 {
    let (mut i, mut u) = (0usize, 0usize);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x > 0.0, *y > 0.0);
        i += usize::from(x && y);
        u += usize::from(x || y);
    }
    if u == 0 { 1.0 } else { i as f32 / u as f32 }
}

/// The decoder's raw outputs (all four masks), for parity tests.
#[derive(Clone, Debug, PartialEq)]
pub struct DecoderRaw {
    pub masks: Vec<Vec<f32>>,
    pub ious: Vec<f32>,
    pub tokens: Vec<Vec<f32>>,
    pub object_score: f32,
}

pub struct Sam2Heads {
    w: Arc<Sam2Weights>,
    dev: Device,
    /// The dense positional encoding (1, 256, 64, 64).
    image_pe: Tensor,
}

impl std::fmt::Debug for Sam2Heads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sam2Heads")
    }
}

impl Sam2Heads {
    pub fn new(w: Arc<Sam2Weights>) -> Result<Self, MaskError> {
        let dev = w.device();
        let g = w.plain("sam_prompt_encoder.pe_layer.positional_encoding_gaussian_matrix")?;
        let pe = dense_pe(&g.data, EMBED_SIDE);
        let image_pe = Tensor::from_vec(pe, (1, D_MODEL, EMBED_SIDE, EMBED_SIDE), &dev).map_err(failed)?;
        Ok(Self { w, dev, image_pe })
    }

    pub fn weights(&self) -> &Arc<Sam2Weights> {
        &self.w
    }

    fn t(&self, name: &str) -> R<Tensor> {
        self.w.get(name).cloned().map_err(|e| candle_core::Error::Msg(e.to_string()))
    }

    fn lin(&self, x: &Tensor, p: &str) -> R<Tensor> {
        linear(x, &self.t(&format!("{p}.weight"))?, &self.t(&format!("{p}.bias"))?)
    }

    fn ln(&self, x: &Tensor, p: &str) -> R<Tensor> {
        layer_norm(x, &self.t(&format!("{p}.weight"))?, &self.t(&format!("{p}.bias"))?, 1e-5)
    }

    /// An `MLP` of `n` linear layers with ReLU between.
    fn mlp(&self, x: &Tensor, p: &str, n: usize) -> R<Tensor> {
        let mut x = x.clone();
        for i in 0..n {
            x = self.lin(&x, &format!("{p}.layers.{i}"))?;
            if i + 1 < n {
                x = x.relu()?;
            }
        }
        Ok(x)
    }

    /// `Attention` with `heads` heads (q, k, v: (1, n, 256)).
    fn attn(&self, p: &str, q: &Tensor, k: &Tensor, v: &Tensor, heads: usize) -> R<Tensor> {
        let sep = |x: Tensor| -> R<Tensor> {
            let (b, n, c) = x.dims3()?;
            x.reshape((b, n, heads, c / heads))?.transpose(1, 2)
        };
        let q = sep(self.lin(q, &format!("{p}.q_proj"))?)?;
        let k = sep(self.lin(k, &format!("{p}.k_proj"))?)?;
        let v = sep(self.lin(v, &format!("{p}.v_proj"))?)?;
        let o = sdpa(&q, &k, &v)?;
        let (b, _, n, _) = o.dims4()?;
        let o = o.transpose(1, 2)?.contiguous()?.reshape((b, n, ()))?;
        self.lin(&o, &format!("{p}.out_proj"))
    }

    /// `ConvTranspose2d(k = 2, s = 2)` (+ bias) of `side`² channel-last pixels: a matmul to the
    /// four output pixels each input pixel covers, then their interleave; out `(2·side)²` rows.
    fn up2x2(&self, x: &Tensor, side: usize, i: usize) -> R<Tensor> {
        let d = "sam_mask_decoder";
        let w = self.t(&format!("{d}.output_upscaling.{i}.weight"))?; // (cin, cout, 2, 2)
        let b = self.t(&format!("{d}.output_upscaling.{i}.bias"))?;
        let (cin, cout, _, _) = w.dims4()?;
        // (cin, cout·4) with columns [dy][dx][co]
        let wr = w.permute((0, 2, 3, 1))?.contiguous()?.reshape((cin, 4 * cout))?;
        let y = x.matmul(&wr)?; // (side², 4·cout)
        let y = y.reshape((side, side, 2, 2, cout))?.permute((0, 2, 1, 3, 4))?.contiguous()?.reshape((4 * side * side, cout))?;
        y.broadcast_add(&b)
    }

    /// The prompt encoder's sparse embeddings for `points` (a padding point is appended, as SAM
    /// 2 always does without a box prompt): (1, n + 1, 256).
    fn sparse(&self, points: &[HeadPoint]) -> R<Tensor> {
        let pe = "sam_prompt_encoder";
        let gauss = self.t(&format!("{pe}.pe_layer.positional_encoding_gaussian_matrix"))?;
        let mut pts: Vec<HeadPoint> = points.to_vec();
        pts.push(HeadPoint { x: 0.0, y: 0.0, label: -1 });
        let s = IMAGE_SIZE as f32;
        let coords: Vec<f32> = pts.iter().flat_map(|p| [2.0 * ((p.x + 0.5) / s) - 1.0, 2.0 * ((p.y + 0.5) / s) - 1.0]).collect();
        let n = pts.len();
        let c = Tensor::from_vec(coords, (n, 2), &self.dev)?;
        let proj = (c.matmul(&gauss)? * (2.0 * std::f64::consts::PI))?;
        let enc = Tensor::cat(&[proj.sin()?, proj.cos()?], 1)?;
        let mut rows = Vec::with_capacity(n);
        for (i, p) in pts.iter().enumerate() {
            let e = enc.i(i..i + 1)?;
            let row = match p.label {
                -1 => self.t(&format!("{pe}.not_a_point_embed.weight"))?,
                l @ 0..=3 => (e + self.t(&format!("{pe}.point_embeddings.{l}.weight"))?)?,
                _ => e,
            };
            rows.push(row);
        }
        Tensor::cat(&rows, 0)?.unsqueeze(0)
    }

    /// The dense embedding: the mask prompt through `mask_downscaling`, else `no_mask_embed`.
    fn dense(&self, mask: Option<&[f32]>) -> R<Tensor> {
        let pe = "sam_prompt_encoder";
        match mask {
            Some(m) => {
                let x = Tensor::from_slice(m, (1, 1, LOW_RES, LOW_RES), &self.dev)?;
                let x = conv(&x, &self.t(&format!("{pe}.mask_downscaling.0.weight"))?, &self.t(&format!("{pe}.mask_downscaling.0.bias"))?, 0, 2, 1)?;
                let x = layer_norm2d(&x, &self.t(&format!("{pe}.mask_downscaling.1.weight"))?, &self.t(&format!("{pe}.mask_downscaling.1.bias"))?, 1e-6)?
                    .gelu_erf()?;
                let x = conv(&x, &self.t(&format!("{pe}.mask_downscaling.3.weight"))?, &self.t(&format!("{pe}.mask_downscaling.3.bias"))?, 0, 2, 1)?;
                let x = layer_norm2d(&x, &self.t(&format!("{pe}.mask_downscaling.4.weight"))?, &self.t(&format!("{pe}.mask_downscaling.4.bias"))?, 1e-6)?
                    .gelu_erf()?;
                conv(&x, &self.t(&format!("{pe}.mask_downscaling.6.weight"))?, &self.t(&format!("{pe}.mask_downscaling.6.bias"))?, 0, 1, 1)
            }
            None => {
                self.t(&format!("{pe}.no_mask_embed.weight"))?.reshape((1, D_MODEL, 1, 1))?.broadcast_as((1, D_MODEL, EMBED_SIDE, EMBED_SIDE))?.contiguous()
            }
        }
    }

    /// `MaskDecoder.predict_masks`: every mask token's mask, the IoU predictions, the mask
    /// tokens and the object score.
    fn predict(&self, pix: &[f32], s0: &[f32], s1: &[f32], points: &[HeadPoint], mask: Option<&[f32]>) -> R<DecoderRaw> {
        let d = "sam_mask_decoder";
        if pix.len() != EMBED_TOKENS * D_MODEL || s0.len() != S0_SIDE * S0_SIDE * S0_DIM || s1.len() != S1_SIDE * S1_SIDE * S1_DIM {
            candle_core::bail!("decoder inputs of {} / {} / {} values", pix.len(), s0.len(), s1.len());
        }
        if mask.is_some_and(|m| m.len() != LOW_RES * LOW_RES) {
            candle_core::bail!("a mask prompt of the wrong size");
        }
        let sparse = self.sparse(points)?;
        let dense = self.dense(mask)?;
        let out_tokens = Tensor::cat(
            &[self.t(&format!("{d}.obj_score_token.weight"))?, self.t(&format!("{d}.iou_token.weight"))?, self.t(&format!("{d}.mask_tokens.weight"))?],
            0,
        )?
        .unsqueeze(0)?;
        let tokens = Tensor::cat(&[out_tokens, sparse], 1)?;
        let img = Tensor::from_slice(pix, (1, EMBED_SIDE, EMBED_SIDE, D_MODEL), &self.dev)?.permute((0, 3, 1, 2))?;
        let src = (img + dense)?;
        // TwoWayTransformer
        let keys0 = src.flatten_from(2)?.transpose(1, 2)?.contiguous()?;
        let kpe = self.image_pe.flatten_from(2)?.transpose(1, 2)?.contiguous()?;
        let (mut q, mut k) = (tokens.clone(), keys0);
        for l in 0..2 {
            let p = format!("{d}.transformer.layers.{l}");
            if l == 0 {
                q = self.attn(&format!("{p}.self_attn"), &q, &q, &q, 8)?;
            } else {
                let qq = (&q + &tokens)?;
                q = (&q + self.attn(&format!("{p}.self_attn"), &qq, &qq, &q, 8)?)?;
            }
            q = self.ln(&q, &format!("{p}.norm1"))?;
            let (qq, kk) = ((&q + &tokens)?, (&k + &kpe)?);
            q = (&q + self.attn(&format!("{p}.cross_attn_token_to_image"), &qq, &kk, &k, 8)?)?;
            q = self.ln(&q, &format!("{p}.norm2"))?;
            let m = self.lin(&self.lin(&q, &format!("{p}.mlp.layers.0"))?.relu()?, &format!("{p}.mlp.layers.1"))?;
            q = self.ln(&(&q + m)?, &format!("{p}.norm3"))?;
            let (qq, kk) = ((&q + &tokens)?, (&k + &kpe)?);
            k = (&k + self.attn(&format!("{p}.cross_attn_image_to_token"), &kk, &qq, &q, 8)?)?;
            k = self.ln(&k, &format!("{p}.norm4"))?;
        }
        let (qq, kk) = ((&q + &tokens)?, (&k + &kpe)?);
        q = (&q + self.attn(&format!("{d}.transformer.final_attn_token_to_image"), &qq, &kk, &k, 8)?)?;
        let hs = self.ln(&q, &format!("{d}.transformer.norm_final_attn"))?;
        // upscaling with the high-res skips, channel-last: each 2 × 2 stride-2 transposed
        // convolution is one matmul to 4 output pixels per input pixel (non-overlapping)
        let src = k.squeeze(0)?; // (4096, 256) = HWC
        let feat_s1 = Tensor::from_slice(s1, (S1_SIDE * S1_SIDE, S1_DIM), &self.dev)?;
        let feat_s0 = Tensor::from_slice(s0, (S0_SIDE * S0_SIDE, S0_DIM), &self.dev)?;
        let u = (self.up2x2(&src, EMBED_SIDE, 0)? + feat_s1)?;
        let u = layer_norm(&u, &self.t(&format!("{d}.output_upscaling.1.weight"))?, &self.t(&format!("{d}.output_upscaling.1.bias"))?, 1e-6)?.gelu_erf()?;
        let u = (self.up2x2(&u, S1_SIDE, 3)? + feat_s0)?.gelu_erf()?; // (65536, 32)
        let mut hyper = Vec::with_capacity(4);
        let mut toks = Vec::with_capacity(4);
        for i in 0..4 {
            let t = hs.i((.., 2 + i, ..))?;
            toks.push(t.flatten_all()?.to_vec1::<f32>()?);
            hyper.push(self.mlp(&t, &format!("{d}.output_hypernetworks_mlps.{i}"), 3)?);
        }
        let hyper = Tensor::cat(&hyper, 0)?; // (4, 32)
        let masks = hyper.matmul(&u.t()?)?.unsqueeze(0)?; // (1, 4, 65536)
        let ious = candle_nn::ops::sigmoid(&self.mlp(&hs.i((.., 1, ..))?, &format!("{d}.iou_prediction_head"), 3)?)?;
        let score = self.mlp(&hs.i((.., 0, ..))?, &format!("{d}.pred_obj_score_head"), 3)?;
        let mut mv = Vec::with_capacity(4);
        for i in 0..4 {
            mv.push(masks.i((0, i))?.to_vec1::<f32>()?);
        }
        Ok(DecoderRaw {
            masks: mv,
            ious: ious.flatten_all()?.to_vec1::<f32>()?,
            tokens: toks,
            object_score: score.flatten_all()?.to_vec1::<f32>()?.first().copied().unwrap_or(0.0),
        })
    }

    /// The decoder's raw outputs (parity tests / diagnostics).
    pub fn decode_raw(&self, pix: &[f32], s0: &[f32], s1: &[f32], points: &[HeadPoint], mask: Option<&[f32]>) -> Result<DecoderRaw, MaskError> {
        guarded("the mask decoder", || self.predict(pix, s0, s1, points, mask).map_err(failed))
    }

    /// `_forward_sam_heads`: the mask SAM 2 keeps for the frame (multimask: the best-scored of
    /// three; else the single mask unless it is unstable, as the video predictor's
    /// `dynamic_multimask_via_stability`), the object score and the object pointer.
    ///
    /// `prefer` (not in the reference): with multimask, pick the candidate maximising predicted
    /// IoU + `prefer.1` × its overlap with the mask `prefer.0` (the previous frame's), so a
    /// tracked subject doesn't flip to one of its parts when the decoder scores the part a
    /// little higher.
    #[allow(clippy::too_many_arguments)]
    pub fn forward(
        &self,
        pix: &[f32],
        s0: &[f32],
        s1: &[f32],
        points: &[HeadPoint],
        mask: Option<&[f32]>,
        multimask: bool,
        prefer: Option<(&[f32], f32)>,
    ) -> Result<HeadOutput, MaskError> {
        let raw = self.decode_raw(pix, s0, s1, points, mask)?;
        guarded("the mask decoder", || {
            let iou_of = |i: usize| raw.ious.get(i).copied().unwrap_or(f32::MIN);
            let overlap: Vec<f32> = (0..4).map(|i| prefer.map_or(0.0, |(p, _)| raw.masks.get(i).map_or(0.0, |m| mask_overlap(m, p)))).collect();
            let mut weight = prefer.map_or(0.0, |(_, w)| if w.is_finite() { w.clamp(0.0, 10.0) } else { 0.0 });
            // only a genuine flip (the best-scored candidate leaves most of the previous mask)
            // brings the overlap in: otherwise small steps could ratchet the mask down
            let top = (1..4).fold(1usize, |b, i| if iou_of(i) > iou_of(b) { i } else { b });
            if overlap.get(top).copied().unwrap_or(0.0) >= PREFER_GATE {
                weight = 0.0;
            }
            let score = |i: usize| iou_of(i) + weight * overlap.get(i).copied().unwrap_or(0.0);
            let best_multi = (1..4).fold(1usize, |b, i| if score(i) > score(b) { i } else { b });
            let candidates = (1..4).map(|i| (iou_of(i), overlap.get(i).copied().unwrap_or(0.0))).collect();
            let (pick, token) = if multimask {
                (best_multi, best_multi)
            } else {
                let m0 = raw.masks.first().map(Vec::as_slice).unwrap_or(&[]);
                let inter = m0.iter().filter(|v| **v > 0.05).count();
                let union = m0.iter().filter(|v| **v > -0.05).count();
                let stability = if union > 0 { inter as f32 / union as f32 } else { 1.0 };
                (if stability >= 0.98 { 0 } else { best_multi }, 0)
            };
            let visible = raw.object_score > 0.0;
            let mut mask = raw.masks.get(pick).cloned().ok_or_else(|| MaskError::Failed("the decoder gave no mask".into()))?;
            if !visible {
                mask.iter_mut().for_each(|v| *v = NO_OBJ_SCORE);
            }
            let tok = raw.tokens.get(token).cloned().ok_or_else(|| MaskError::Failed("the decoder gave no token".into()))?;
            let tok = Tensor::from_vec(tok, (1, D_MODEL), &self.dev).map_err(failed)?;
            let ptr = self.mlp(&tok, "obj_ptr_proj", 3).map_err(failed)?;
            let ptr = if visible { ptr } else { self.t("no_obj_ptr").map_err(failed)? };
            let obj_ptr = ptr.flatten_all().and_then(|t| t.to_vec1::<f32>()).map_err(failed)?;
            if mask.iter().chain(&obj_ptr).any(|v| !v.is_finite()) {
                return Err(MaskError::Failed("the mask decoder produced non-finite values".into()));
            }
            Ok(HeadOutput { mask, iou: raw.ious.get(pick).copied().unwrap_or(0.0), obj_ptr, object_score: raw.object_score, candidates })
        })
    }

    /// `obj_ptr_tpos_proj(get_1d_sine_pe(t / t_max, 256))`: an object pointer's temporal
    /// position encoding (64).
    pub fn ptr_tpos(&self, t: f32) -> Result<Vec<f32>, MaskError> {
        let pe = super::sine_1d(t, D_MODEL);
        let x = Tensor::from_vec(pe, (1, D_MODEL), &self.dev).map_err(failed)?;
        self.lin(&x, "obj_ptr_tpos_proj").and_then(|t| t.flatten_all()?.to_vec1::<f32>()).map_err(failed)
    }

    /// A plain parameter (constants the tracker needs).
    pub fn param(&self, name: &str) -> Result<Vec<f32>, MaskError> {
        Ok(self.w.plain(name)?.data)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use candle_core::{DType, Tensor};

    use super::*;
    use crate::sam2::weights::expected;
    use crate::sam2::{Sam2Config, Sam2Size};

    /// The channel-last 2 × 2 upscaling equals candle's `conv_transpose2d` on NCHW.
    #[test]
    fn upscaling_by_matmul_equals_the_transposed_convolution() {
        let cfg = Sam2Config::builtin(Sam2Size::BasePlus);
        let dev = Device::Cpu;
        let mut t: HashMap<String, Tensor> = expected(&cfg).into_iter().map(|(n, s)| (n, Tensor::zeros(s.as_slice(), DType::F32, &dev).unwrap())).collect();
        let w = Tensor::randn(0f32, 1.0, (24, 8, 2, 2), &dev).unwrap();
        let b = Tensor::randn(0f32, 1.0, 8, &dev).unwrap();
        t.insert("sam_mask_decoder.output_upscaling.0.weight".into(), w.clone());
        t.insert("sam_mask_decoder.output_upscaling.0.bias".into(), b.clone());
        // shapes don't match the checkpoint's, so bypass the shape check for this one test
        let weights = Arc::new(Sam2Weights::from_tensors_unchecked(cfg, t));
        let h = Sam2Heads { w: weights, dev: dev.clone(), image_pe: Tensor::zeros((1, 1), DType::F32, &dev).unwrap() };
        let side = 5;
        let x = Tensor::randn(0f32, 1.0, (side * side, 24), &dev).unwrap();
        let ours = h.up2x2(&x, side, 0).unwrap();
        let nchw = x.reshape((1, side, side, 24)).unwrap().permute((0, 3, 1, 2)).unwrap().contiguous().unwrap();
        let theirs = nchw.conv_transpose2d(&w, 0, 0, 2, 1).unwrap().broadcast_add(&b.reshape((1, 8, 1, 1)).unwrap()).unwrap();
        let theirs = theirs.permute((0, 2, 3, 1)).unwrap().contiguous().unwrap().reshape((4 * side * side, 8)).unwrap();
        let d = (ours - theirs).unwrap().abs().unwrap().max_all().unwrap().to_scalar::<f32>().unwrap();
        assert!(d < 1e-5, "{d}");
    }
}
