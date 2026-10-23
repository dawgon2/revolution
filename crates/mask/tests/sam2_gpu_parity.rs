//! SAM 2.1's GPU passes (`filmcraft_gpu::sam2`, fork G3.4) against the CPU reference: every
//! image-encoder stage, the memory encoder and the memory attention within 1e-3 of the largest
//! reference value, and the decoded masks equal (IoU ≥ 0.99). Base+ by default (the CPU side
//! takes about a minute); `FC_SAM2_LARGE=1` checks Large too. Skips without the checkpoints or a
//! GPU. `MESA_VK_DEVICE_SELECT=8086:4c8a MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1` runs it
//! on the owner's Intel iGPU. Hostile inputs are refused with errors.

#![cfg(feature = "sam")]

mod common {
    pub mod sam2_gpu;
}

use std::sync::Arc;

use common::sam2_gpu;
use filmcraft_mask::RgbImage;
use filmcraft_mask::sam2::cpu::Sam2Cpu;
use filmcraft_mask::sam2::heads::{HeadPoint, Pick, Sam2Heads};
use filmcraft_mask::sam2::weights::Sam2Weights;
use filmcraft_mask::sam2::{Sam2Compute, Sam2Config, Sam2Size, prepare_input, sine_position, tests_support};
use filmcraft_mask::{Matte, Segment};

fn rel(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let d = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
    d / b.iter().map(|v| v.abs()).fold(0f32, f32::max).max(1e-12)
}

fn check(size: Sam2Size) {
    let Some((pt, yaml)) = tests_support::files(size) else { return };
    let cfg = Sam2Config::from_yaml(&std::fs::read_to_string(yaml).unwrap()).unwrap();
    let w = Arc::new(Sam2Weights::load(&pt, cfg).unwrap());
    let gpu = match sam2_gpu::open(&w) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let gpu = Arc::new(gpu);
    let cpu = Sam2Cpu::new(w.clone()).unwrap();
    let heads = Sam2Heads::new(w.clone()).unwrap();
    let gheads = Sam2Heads::new(w.clone()).unwrap().with_decoder(gpu.clone());
    let (iw, ih) = (640, 360);
    let inside = |x: usize, y: usize| ((x as f32 - 320.0) / 70.0).powi(2) + ((y as f32 - 190.0) / 115.0).powi(2) <= 1.0;
    let rgb: Vec<u8> = (0..iw * ih)
        .flat_map(|i| {
            let (x, y) = (i % iw, i / iw);
            if inside(x, y) { [200, 70, 50] } else { [(x / 3 % 255) as u8, (y / 2 % 255) as u8, 150] }
        })
        .collect();
    let img = RgbImage::new(iw, ih, rgb).unwrap();
    let input = prepare_input(&img);
    let g = gpu.0.image_stages(&input).unwrap();
    let c = cpu.image_stages(&input).unwrap();
    assert_eq!(g.len(), c.len());
    for ((gn, gv), (cn, cv)) in g.iter().zip(&c) {
        assert_eq!(gn, cn);
        let r = rel(gv, cv);
        eprintln!("{size:?} {gn}: {r:.2e}");
        assert!(r < 1e-3, "{size:?} {gn}: relative difference {r}");
    }
    let get = |v: &[(String, Vec<f32>)], n: &str| v.iter().find(|(k, _)| k == n).map(|(_, d)| d.clone()).unwrap();
    let feat = get(&c, "feat");
    // memory encoder on a mask of the ellipse
    let mask: Vec<f32> = (0..1024 * 1024).map(|i| if inside(i % 1024 * iw / 1024, i / 1024 * ih / 1024) { 10.0 } else { -10.0 }).collect();
    let cm = cpu.memory_encoder(&feat, &mask).unwrap();
    let r = rel(&gpu.memory_encoder(&feat, &mask).unwrap(), &cm);
    assert!(r < 1e-4, "memory encoder: {r}");
    // memory attention over one frame and two pointers
    let mut memory = cm.clone();
    let mut pos = sine_position(64, 64, 64);
    for k in 0..8 {
        memory.extend((0..64).map(|i| ((i * 7 + k * 13) % 17) as f32 / 17.0 - 0.5));
        pos.extend((0..64).map(|i| ((i + k) % 5) as f32 / 5.0));
    }
    let r = rel(&gpu.memory_attention(&feat, &memory, &pos, 8).unwrap(), &cpu.memory_attention(&feat, &memory, &pos, 8).unwrap());
    assert!(r < 1e-4, "memory attention: {r}");
    // masks decoded from either side's features agree
    let click = [HeadPoint { x: 512.0, y: 540.0, label: 1 }];
    let seg = |f: &[(String, Vec<f32>)]| {
        let o = heads.forward(&get(f, "feat"), &get(f, "s0"), &get(f, "s1"), &click, None, true, Pick::Best).unwrap();
        Segment { logits: o.mask, cover: (1.0, 1.0), score: o.iou }.matte(iw, ih).unwrap()
    };
    let (a, b) = (seg(&g), seg(&c));
    let iou = a.iou(&b).unwrap();
    assert!(iou >= 0.99, "mask IoU {iou}");
    let truth = Matte { width: iw, height: ih, alpha: (0..iw * ih).map(|i| if inside(i % iw, i / iw) { 255 } else { 0 }).collect() };
    eprintln!("{size:?}: mask IoU GPU vs CPU {iou:.4}, vs the ellipse {:.3}", a.iou(&truth).unwrap());

    // the mask decoder on the GPU against the CPU's, on the GPU's features as a first frame
    // sees them (+ no_mem_embed): 2e-4 of the largest logit / token value (absolute for IoUs).
    // Same f32 rounding argument as `sam2_decoder_gpu.rs` (1e-4 there, measured 2e-6), with
    // room for the real model's cancellation: a mask logit is a 32-term dot product of large
    // terms (measured ≤ 4.7e-5 on Base+, AMD and Intel); 2e-4 of a ~25 peak logit is 0.005,
    // which can only flip a pixel already on the 0 edge
    let mut pix = get(&g, "feat");
    let nm = heads.param("no_mem_embed").unwrap();
    pix.chunks_mut(256).for_each(|r| r.iter_mut().zip(&nm).for_each(|(a, b)| *a += b));
    let (s0, s1) = (get(&g, "s0"), get(&g, "s1"));
    let prev = heads.decode_raw_cpu(&pix, &s0, &s1, &click, None).unwrap().masks[1].clone();
    let boxed = [HeadPoint { x: 330.0, y: 150.0, label: 2 }, HeadPoint { x: 700.0, y: 930.0, label: 3 }, HeadPoint { x: 512.0, y: 540.0, label: 1 }];
    let cases: [(&[HeadPoint], Option<&[f32]>); 3] = [(&click, None), (&boxed, None), (&click, Some(&prev))];
    for (pts, m) in cases {
        let a = gheads.decode_raw(&pix, &s0, &s1, pts, m).unwrap();
        assert!(gheads.decoder_on_device(), "the GPU decoder failed");
        let b = heads.decode_raw_cpu(&pix, &s0, &s1, pts, m).unwrap();
        let masks = a.masks.iter().zip(&b.masks).map(|(x, y)| rel(x, y)).fold(0f32, f32::max);
        let tokens = a.tokens.iter().zip(&b.tokens).map(|(x, y)| rel(x, y)).fold(0f32, f32::max);
        let ious = a.ious.iter().zip(&b.ious).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
        let score = (a.object_score - b.object_score).abs() / b.object_score.abs().max(1.0);
        let worst = masks.max(tokens).max(ious).max(score);
        eprintln!(
            "{size:?} decoder ({} points, mask prompt {}): masks {masks:.1e}, tokens {tokens:.1e}, IoUs {ious:.1e}, score {score:.1e}",
            pts.len(),
            m.is_some()
        );
        assert!(worst < 2e-4, "{size:?} decoder: {worst}");
    }

    // hostile inputs are refused
    assert!(gpu.0.image(&input[1..]).is_err());
    let mut nan = input.clone();
    nan[77] = f32::NAN;
    assert!(gpu.0.image(&nan).is_err());
    assert!(gpu.memory_encoder(&feat[..100], &mask).is_err());
    assert!(gpu.memory_attention(&feat, &memory[..100], &pos[..100], 0).is_err());
    assert!(gpu.memory_attention(&feat, &memory, &pos, 9).is_err());
    assert!(gpu.memory_attention(&feat, &memory, &pos, memory.len()).is_err());
    let huge = vec![0.0f32; (filmcraft_gpu::sam2::MAX_MEMORY_TOKENS + 4096) * 64];
    assert!(gpu.memory_attention(&feat, &huge, &huge, 0).is_err());
    // and it still works afterwards
    assert!(gpu.memory_encoder(&feat, &mask).is_ok());
}

#[test]
fn base_plus_gpu_matches_the_cpu_reference() {
    check(Sam2Size::BasePlus);
}

#[test]
fn large_gpu_matches_the_cpu_reference() {
    if std::env::var("FC_SAM2_LARGE").is_ok_and(|v| v == "1") {
        check(Sam2Size::Large);
    } else {
        eprintln!("skipped: set FC_SAM2_LARGE=1 (the CPU side takes minutes)");
    }
}
