//! G3: MobileSAM's image encoder on the GPU (`filmcraft_gpu::tinyvit`) vs the CPU (candle).
//!
//! `cargo run --release -p filmcraft-mask --features sam --example sam_gpu -- [model] [frames]`
//!
//! Times the GPU encoder per frame (median of `frames`, default 20, after one warm-up), one CPU
//! encode, and compares the embeddings and the masks decoded from both on a 1920×1080 frame.
//! On the owner's machine `MESA_VK_DEVICE_SELECT=8086:4c8a
//! MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1` selects the Intel iGPU.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use filmcraft_gpu::tinyvit::{TinyVitGpu, Weight};
use filmcraft_mask::sam::MobileSam;
use filmcraft_mask::{Point, Prompt, Rect, RgbImage, Segmenter};

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("{e}");
    std::process::exit(1)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let model = args.next().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("models/mobilesam/mobile_sam.pt"));
    let frames: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20).clamp(1, 1000);
    let t = Instant::now();
    let sam = MobileSam::load(&model).unwrap_or_else(|e| fail(e));
    let weights: HashMap<String, Weight> =
        sam.encoder_weights().unwrap_or_else(|e| fail(e)).into_iter().map(|t| (t.name, Weight { shape: t.shape, data: t.data })).collect();
    let gpu = TinyVitGpu::open(&weights).unwrap_or_else(|e| fail(e));
    println!("loaded in {:.2} s; GPU {}", t.elapsed().as_secs_f64(), gpu.name());

    let (w, h) = (1920, 1080);
    let rgb: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let inside = ((x as f32 - 960.0) / 200.0).powi(2) + ((y as f32 - 560.0) / 320.0).powi(2) <= 1.0;
            if inside { [200, 70, 50] } else { [(x / 7 % 255) as u8, (y / 5 % 255) as u8, 150] }
        })
        .collect();
    let img = RgbImage::new(w, h, rgb).unwrap_or_else(|e| fail(e));
    let (input, fitted) = sam.prepare_input(&img).unwrap_or_else(|e| fail(e));

    let _ = gpu.encode(&input).unwrap_or_else(|e| fail(e));
    let mut ms = Vec::with_capacity(frames);
    let mut feats = Vec::new();
    for _ in 0..frames {
        let t = Instant::now();
        feats = gpu.encode(&input).unwrap_or_else(|e| fail(e));
        ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    ms.sort_by(f64::total_cmp);
    let median = ms.get(ms.len() / 2).copied().unwrap_or(0.0);
    println!("GPU encode: median {median:.1} ms, min {:.1}, max {:.1} over {frames} frames", ms.first().unwrap_or(&0.0), ms.last().unwrap_or(&0.0));

    let t = Instant::now();
    let cpu = sam.encode_cpu(&input).unwrap_or_else(|e| fail(e));
    println!("CPU encode: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let worst = feats.iter().zip(&cpu).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    println!("embedding max abs diff {worst:.3e}");

    let p = Prompt { points: vec![Point { x: 0.5, y: 0.52, positive: true }], rect: Some(Rect { x0: 0.38, y0: 0.2, x1: 0.62, y1: 0.82 }), prior: None };
    let e_gpu = sam.embedding_from_features(feats, w, h, fitted).unwrap_or_else(|e| fail(e));
    let e_cpu = sam.embedding_from_features(cpu, w, h, fitted).unwrap_or_else(|e| fail(e));
    let a = sam.segment(&e_gpu, &p).and_then(|s| s.matte(w, h)).unwrap_or_else(|e| fail(e));
    let b = sam.segment(&e_cpu, &p).and_then(|s| s.matte(w, h)).unwrap_or_else(|e| fail(e));
    println!("mask IoU GPU vs CPU: {:.5}", a.iou(&b).unwrap_or(0.0));
}
