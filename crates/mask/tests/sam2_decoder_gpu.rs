//! SAM 2.1's mask decoder on the GPU (`filmcraft_gpu::sam2::decoder`, fork LO.6) against the
//! CPU reference (`filmcraft_mask::sam2::heads`) on fixed pseudo-random weights and inputs (no
//! checkpoint needed): every mask, IoU prediction, mask token and the object score, for a single
//! click (multimask), points with a box, and a mask prompt. Skips without a GPU.
//!
//! Tolerance: 1e-4 of the largest reference value (masks, tokens; absolute for the IoUs and the
//! object score). Both sides compute in f32 but sum in different orders (64 × 64 tiles vs
//! candle's gemm, a softmax over 4096 keys, sums of up to 2048 products); the rounding that
//! leaves is about √K · 2⁻²⁴ ≈ 3e-6 per layer, a few 1e-6 after the transformer's 2 layers and
//! final attention (the measured worst is printed); 1e-4 is far below anything that moves a mask
//! edge (logits are thresholded at 0) and far above that rounding.
//!
//! `cargo test --release -p filmcraft-mask --features sam --test sam2_decoder_gpu -- --nocapture`
//! prints the time per decode on the CPU and the GPU. `MESA_VK_DEVICE_SELECT=8086:4c8a
//! MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1` runs it on the owner's Intel iGPU.

#![cfg(feature = "sam")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use candle_core::{Device, Tensor};
use filmcraft_gpu::sam2::Weight;
use filmcraft_gpu::sam2::decoder::{MASK_SIDE, Sam2GpuDecoder};
use filmcraft_mask::MaskError;
use filmcraft_mask::sam2::heads::{DecoderRaw, HeadPoint, Sam2Heads};
use filmcraft_mask::sam2::weights::{DECODER_PREFIXES, Sam2Weights, expected};
use filmcraft_mask::sam2::{DecoderInput, Sam2Compute, Sam2Config, Sam2Features, Sam2Size};

const TOL: f32 = 1e-4;

/// The decoder alone as a [`Sam2Compute`].
struct DecoderOnly(Sam2GpuDecoder);

impl Sam2Compute for DecoderOnly {
    fn name(&self) -> String {
        "decoder".into()
    }
    fn image(&self, _: &[f32]) -> Result<Sam2Features, MaskError> {
        Err(MaskError::Failed("decoder only".into()))
    }
    fn memory_attention(&self, _: &[f32], _: &[f32], _: &[f32], _: usize) -> Result<Vec<f32>, MaskError> {
        Err(MaskError::Failed("decoder only".into()))
    }
    fn memory_encoder(&self, _: &[f32], _: &[f32]) -> Result<Vec<f32>, MaskError> {
        Err(MaskError::Failed("decoder only".into()))
    }
    fn mask_decoder(&self, i: &DecoderInput<'_>) -> Option<Result<DecoderRaw, MaskError>> {
        Some(self.0.decode(i.pix, i.s0, i.s1, i.tokens, i.dense).map_err(|e| MaskError::Failed(e.to_string())).map(|o| DecoderRaw {
            masks: o.masks.chunks(MASK_SIDE * MASK_SIDE).map(<[f32]>::to_vec).collect(),
            ious: o.ious,
            tokens: o.tokens.chunks(256).map(<[f32]>::to_vec).collect(),
            object_score: o.score,
        }))
    }
}

/// A fixed pseudo-random sequence in −1..1.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
    fn vec(&mut self, n: usize, scale: f32) -> Vec<f32> {
        (0..n).map(|_| self.next() * scale).collect()
    }
}

/// Base+'s tensors: zeros except the decoder and prompt encoder, which get pseudo-random values
/// scaled as an initialisation would (LayerNorm weights near 1).
fn random_weights() -> Sam2Weights {
    let cfg = Sam2Config::builtin(Sam2Size::BasePlus);
    let mut r = Lcg(0x5a2d_ec0d);
    let mut t = HashMap::new();
    for (name, shape) in expected(&cfg) {
        let n: usize = shape.iter().product();
        let data = if name.starts_with("sam_mask_decoder.") || name.starts_with("sam_prompt_encoder.") {
            if shape.len() == 1 {
                if name.ends_with("weight") { r.vec(n, 0.1).into_iter().map(|v| 1.0 + v).collect() } else { r.vec(n, 0.1) }
            } else {
                let fan: usize = if name.contains("output_upscaling") { shape[0] } else { shape[1..].iter().product() };
                let scale = if name.contains("gaussian") || name.ends_with("_token.weight") || name.ends_with("tokens.weight") {
                    1.0
                } else {
                    1.0 / (fan.max(1) as f32).sqrt()
                };
                r.vec(n, scale * 1.7)
            }
        } else {
            vec![0.0; n]
        };
        t.insert(name, Tensor::from_vec(data, shape.as_slice(), &Device::Cpu).unwrap());
    }
    Sam2Weights::from_tensors(cfg, t).unwrap()
}

fn rel(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let d = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
    d / b.iter().map(|v| v.abs()).fold(0f32, f32::max).max(1e-12)
}

/// The worst difference of the GPU's outputs from the CPU's (relative for masks and tokens,
/// absolute for IoUs and the score).
fn compare(g: &DecoderRaw, c: &DecoderRaw) -> f32 {
    let mut worst = 0f32;
    for (a, b) in g.masks.iter().zip(&c.masks).chain(g.tokens.iter().zip(&c.tokens)) {
        worst = worst.max(rel(a, b));
    }
    for (a, b) in g.ious.iter().zip(&c.ious) {
        worst = worst.max((a - b).abs());
    }
    worst.max((g.object_score - c.object_score).abs() / c.object_score.abs().max(1.0))
}

pub fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

#[test]
fn gpu_decoder_matches_the_cpu_on_random_weights() {
    let w = random_weights();
    let plain: HashMap<String, Weight> =
        w.plain_with(&DECODER_PREFIXES).unwrap().into_iter().map(|(k, t)| (k, Weight { shape: t.shape, data: t.data })).collect();
    let (dec, name) = match Sam2GpuDecoder::open(&plain) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let dec = Arc::new(DecoderOnly(dec));
    let heads = Sam2Heads::new(Arc::new(w)).unwrap().with_decoder(dec.clone());
    let mut r = Lcg(77);
    let pix = r.vec(4096 * 256, 1.0);
    let s0 = r.vec(256 * 256 * 32, 1.0);
    let s1 = r.vec(128 * 128 * 64, 1.0);
    let mask = r.vec(256 * 256, 8.0);
    let click = vec![HeadPoint { x: 512.0, y: 540.0, label: 1 }];
    let mut several = vec![HeadPoint { x: 200.0, y: 100.0, label: 2 }, HeadPoint { x: 800.0, y: 900.0, label: 3 }];
    several.extend((0..5).map(|i| HeadPoint { x: 300.0 + 90.0 * i as f32, y: 400.0 + 31.0 * i as f32, label: i % 2 }));
    let many: Vec<HeadPoint> = (0..64).map(|i| HeadPoint { x: (i * 16) as f32, y: 1023.0 - (i * 15) as f32, label: i % 2 }).collect();
    let cases: [(&str, &[HeadPoint], Option<&[f32]>); 4] =
        [("click", &click, None), ("box + points", &several, None), ("click + mask prompt", &click, Some(&mask)), ("64 points", &many, None)];
    let mut worst = 0f32;
    for (what, pts, m) in cases {
        let g = heads.decode_raw(&pix, &s0, &s1, pts, m).unwrap();
        assert!(heads.decoder_on_device(), "{what}: the GPU decoder failed and the CPU took over");
        let c = heads.decode_raw_cpu(&pix, &s0, &s1, pts, m).unwrap();
        let d = compare(&g, &c);
        eprintln!("{name}: {what}: worst {d:.2e}, ious {:?} / {:?}, score {:.4} / {:.4}", g.ious, c.ious, g.object_score, c.object_score);
        assert!(d < TOL, "{what}: {d}");
        worst = worst.max(d);
    }
    eprintln!("{name}: worst difference {worst:.2e} (tolerance {TOL:.0e})");
    // timings (synthetic; meaningful in --release)
    let _ = heads.decode_raw(&pix, &s0, &s1, &click, None);
    let time = |f: &dyn Fn()| {
        median(
            (0..5)
                .map(|_| {
                    let t = Instant::now();
                    f();
                    t.elapsed().as_secs_f64() * 1e3
                })
                .collect(),
        )
    };
    let gpu = time(&|| {
        heads.decode_raw(&pix, &s0, &s1, &click, None).unwrap();
    });
    let cpu = time(&|| {
        heads.decode_raw_cpu(&pix, &s0, &s1, &click, None).unwrap();
    });
    let tok = vec![0.1f32; 8 * 256];
    let dense = vec![0.1f32; 256];
    let bare = time(&|| {
        dec.0.decode(&pix, &s0, &s1, &tok, &dense).unwrap();
    });
    eprintln!("{name}: decoder per frame: CPU {cpu:.1} ms, GPU {gpu:.1} ms (of which the GPU call {bare:.1} ms; median of 5, one click)");
    // hostile inputs: refused with errors, the device stays usable
    assert!(heads.decode_raw(&pix[1..], &s0, &s1, &click, None).is_err());
    assert!(heads.decode_raw(&pix, &s0[..10], &s1, &click, None).is_err());
    assert!(heads.decode_raw(&pix, &s0, &s1, &click, Some(&mask[1..])).is_err());
    assert!(heads.decoder_on_device());
}

/// The decoder's own checks, straight on the GPU type (no fallback in between).
#[test]
fn gpu_decoder_refuses_hostile_inputs() {
    let w = random_weights();
    let plain: HashMap<String, Weight> =
        w.plain_with(&DECODER_PREFIXES).unwrap().into_iter().map(|(k, t)| (k, Weight { shape: t.shape, data: t.data })).collect();
    let mut short = plain.clone();
    short.remove("sam_mask_decoder.iou_prediction_head.layers.2.weight");
    assert!(Sam2GpuDecoder::open(&short).is_err());
    let mut bent = plain.clone();
    if let Some(x) = bent.get_mut("sam_mask_decoder.transformer.layers.1.mlp.layers.0.weight") {
        x.shape = vec![2047, 256];
    }
    assert!(Sam2GpuDecoder::open(&bent).is_err());
    let (dec, _) = match Sam2GpuDecoder::open(&plain) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let (pix, s0, s1) = (vec![0.1f32; 4096 * 256], vec![0.1f32; 256 * 256 * 32], vec![0.1f32; 128 * 128 * 64]);
    let tok = vec![0.2f32; 8 * 256];
    let dense = vec![0.0f32; 256];
    assert!(dec.decode(&pix, &s0, &s1, &tok, &dense).is_ok());
    assert!(dec.decode(&pix[..4096], &s0, &s1, &tok, &dense).is_err());
    assert!(dec.decode(&pix, &s0, &s1, &tok[..5 * 256], &dense).is_err());
    assert!(dec.decode(&pix, &s0, &s1, &tok[..300], &dense).is_err());
    assert!(dec.decode(&pix, &s0, &s1, &vec![0.0; 81 * 256], &dense).is_err());
    assert!(dec.decode(&pix, &s0, &s1, &tok, &dense[..100]).is_err());
    let mut nan = tok.clone();
    nan[9] = f32::NAN;
    assert!(dec.decode(&pix, &s0, &s1, &nan, &dense).is_err());
    assert!(dec.decode(&pix, &s0, &s1, &tok, &dense).is_ok());
}
