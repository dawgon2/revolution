//! The GPU image encoder (`filmcraft_gpu::tinyvit`, fork G3 / FORK-PLAN §1a.122) against the
//! CPU candle encoder: every stage on a synthetic frame and on noise, and masks decoded from
//! both embeddings on the `sam_spike` scene (IoU ≥ 0.99). Skips without the model or a GPU.
//! `MESA_VK_DEVICE_SELECT=8086:4c8a MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1` runs it on
//! the Intel iGPU of the owner's machine.

#![cfg(feature = "sam")]

use std::collections::HashMap;
use std::time::Instant;

use filmcraft_gpu::tinyvit::{TinyVitGpu, Weight};
use filmcraft_mask::sam::MobileSam;
use filmcraft_mask::{Matte, Point, Prompt, Rect, RgbImage, Segmenter};

fn model() -> Option<MobileSam> {
    let path = std::env::var_os("FILMCRAFT_MOBILESAM")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/mobilesam/mobile_sam.pt"));
    if !path.is_file() {
        eprintln!("skipped: {} not installed", path.display());
        return None;
    }
    Some(MobileSam::load(&path).unwrap())
}

fn gpu(sam: &MobileSam) -> Option<TinyVitGpu> {
    let w: HashMap<String, Weight> = sam.encoder_weights().unwrap().into_iter().map(|t| (t.name, Weight { shape: t.shape, data: t.data })).collect();
    match TinyVitGpu::open(&w) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

/// The sam_spike figure at time `t` in a `w` × `h` frame: centre, body radii, head radius.
fn figure(t: f32, w: f32, h: f32) -> (f32, f32, f32, f32, f32) {
    (w * (0.25 + 0.5 * t), h * (0.58 + 0.04 * (t * 12.0).sin()), w * 0.06, h * 0.22, h * 0.07)
}

/// The sam_spike frame (a figure over stripes and a checker) at time `t`.
fn scene(t: f32, w: usize, h: usize) -> RgbImage {
    let (cx, cy, rx, ry, rh) = figure(t, w as f32, h as f32);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (xf, yf) = (x as f32, y as f32);
            let body = ((xf - cx) / rx).powi(2) + ((yf - cy) / ry).powi(2) <= 1.0;
            let hy = cy - ry - rh * 0.8;
            let head = (xf - cx).powi(2) + (yf - hy).powi(2) <= rh * rh;
            if body || head {
                rgb.extend_from_slice(&[200, 70, 50]);
            } else {
                let ground = yf > h as f32 * 0.7;
                let stripe = ((xf / w as f32 * 23.0).sin() * 30.0) as i32;
                let check = if ((x / 97) + (y / 61)) % 2 == 0 { 18 } else { -18 };
                let (r, g, b) = if ground { (70, 110, 60) } else { (110, 150, 200) };
                let px = |c: i32| (c + stripe + check).clamp(0, 255) as u8;
                rgb.extend_from_slice(&[px(r), px(g), px(b)]);
            }
        }
    }
    RgbImage::new(w, h, rgb).unwrap()
}

fn prompt(t: f32, w: usize, h: usize) -> Prompt {
    let (cx, cy, rx, ry, rh) = figure(t, w as f32, h as f32);
    let (w, h) = (w as f32, h as f32);
    Prompt {
        points: vec![Point { x: cx / w, y: cy / h, positive: true }],
        rect: Some(Rect { x0: (cx - rx) / w, y0: (cy - ry - rh * 1.8) / h, x1: (cx + rx) / w, y1: (cy + ry) / h }),
        prior: None,
        prefer_whole: false,
    }
}

/// (max abs diff, max abs diff / max |reference|).
fn diff(gpu: &[f32], cpu: &[f32]) -> (f32, f32) {
    let worst = gpu.iter().zip(cpu).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    let scale = cpu.iter().map(|v| v.abs()).fold(0f32, f32::max).max(1e-6);
    (worst, worst / scale)
}

#[test]
fn every_stage_matches_the_cpu_encoder() {
    let Some(sam) = model() else { return };
    let Some(gpu) = gpu(&sam) else { return };
    eprintln!("GPU: {}", gpu.name());
    let (synthetic, _) = sam.prepare_input(&scene(0.3, 1920, 1080)).unwrap();
    let mut s = 0x9e37_79b9u32;
    let noise: Vec<f32> = (0..3 * 1024 * 1024)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32 - 0.5) * 4.0
        })
        .collect();
    for (what, input) in [("synthetic", &synthetic), ("noise", &noise)] {
        let cpu = sam.encoder_stages_cpu(input).unwrap();
        let ours = gpu.encode_stages(input).unwrap();
        assert_eq!(cpu.len(), ours.len());
        for ((name, c), (gname, g)) in cpu.iter().zip(&ours) {
            assert_eq!(name, gname);
            assert_eq!(c.len(), g.len(), "{what} {name}");
            let (abs, rel) = diff(g, c);
            eprintln!("{what:9} {name:12} max abs {abs:.3e}  rel to max {rel:.3e}");
            assert!(rel < 2e-3, "{what} {name}: max abs {abs}, relative {rel}");
        }
        let enc = gpu.encode(input).unwrap();
        assert_eq!(&enc, &ours.last().unwrap().1);
    }
    assert!(gpu.encode(&[0.0; 5]).is_err());
}

#[test]
fn masks_from_the_gpu_embedding_match_the_cpu_ones() {
    let Some(sam) = model() else { return };
    let Some(gpu) = gpu(&sam) else { return };
    let (w, h) = (1920, 1080);
    let mut times = Vec::new();
    let mut cpu_times = Vec::new();
    for t in [0.0, 0.35, 0.7, 1.0] {
        let img = scene(t, w, h);
        let (input, fitted) = sam.prepare_input(&img).unwrap();
        let t0 = Instant::now();
        let feats = gpu.encode(&input).unwrap();
        times.push(t0.elapsed().as_secs_f64() * 1e3);
        let t0 = Instant::now();
        let cpu_e = sam.embed(&img).unwrap();
        cpu_times.push(t0.elapsed().as_secs_f64() * 1e3);
        let gpu_e = sam.embedding_from_features(feats, w, h, fitted).unwrap();
        let p = prompt(t, w, h);
        let a: Matte = sam.segment(&cpu_e, &p).unwrap().matte(w, h).unwrap();
        let b: Matte = sam.segment(&gpu_e, &p).unwrap().matte(w, h).unwrap();
        let iou = a.iou(&b).unwrap();
        eprintln!("t {t:.2}: IoU GPU vs CPU {iou:.5}");
        assert!(iou >= 0.99, "t {t}: IoU {iou}");
    }
    eprintln!("{}: encode ms per frame {times:.1?} (first includes warm-up); CPU embed ms {cpu_times:.0?}", gpu.name());
}
