//! G3.1 spike: MobileSAM speed on the CPU and how well it follows a moving subject.
//!
//! `cargo run --release -p filmcraft-mask --features sam --example sam_spike -- [model] [frames]`
//!
//! 1. Times `embed` + `segment` on a 1920×1080 and a 6240×4160 frame.
//! 2. Follows a synthetic figure (an ellipse body + a round head, its own colour) walking over a
//!    busy background for `frames` frames (default 240 = 10 s at 24 fps) at 1080p: frame 0 gets
//!    one click + a box, every later frame only `follow_prompt` of the previous mask. Prints the
//!    IoU against the drawn figure per second and the mean / minimum.
//!
//! Synthetic, not real footage: it shows the propagation loop works and what it costs.

use std::path::PathBuf;
use std::time::Instant;

use filmcraft_mask::sam::MobileSam;
use filmcraft_mask::{Matte, Point, Prompt, Rect, RgbImage, Segmenter, follow_prompt};

/// The figure at time `t` (0..1) in a `w` × `h` frame: centre, body radii, head radius.
fn figure(t: f32, w: f32, h: f32) -> (f32, f32, f32, f32, f32) {
    let cx = w * (0.25 + 0.5 * t);
    let cy = h * (0.58 + 0.04 * (t * 12.0).sin());
    (cx, cy, w * 0.06, h * 0.22, h * 0.07)
}

fn inside(x: f32, y: f32, f: (f32, f32, f32, f32, f32)) -> bool {
    let (cx, cy, rx, ry, rh) = f;
    let body = ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0;
    let hy = cy - ry - rh * 0.8;
    let head = (x - cx).powi(2) + (y - hy).powi(2) <= rh * rh;
    body || head
}

/// The frame and the true matte at time `t`.
fn frame(t: f32, w: usize, h: usize) -> (RgbImage, Matte) {
    let f = figure(t, w as f32, h as f32);
    let mut rgb = Vec::with_capacity(w * h * 3);
    let mut alpha = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let (xf, yf) = (x as f32, y as f32);
            if inside(xf, yf, f) {
                rgb.extend_from_slice(&[200, 70, 50]);
                alpha.push(255);
            } else {
                // Sky / ground split, stripes and a checker so the background isn't flat.
                let ground = yf > h as f32 * 0.7;
                let stripe = ((xf / w as f32 * 23.0).sin() * 30.0) as i32;
                let check = if ((x / 97) + (y / 61)) % 2 == 0 { 18 } else { -18 };
                let (r, g, b) = if ground { (70, 110, 60) } else { (110, 150, 200) };
                let px = |c: i32| (c + stripe + check).clamp(0, 255) as u8;
                rgb.extend_from_slice(&[px(r), px(g), px(b)]);
                alpha.push(0);
            }
        }
    }
    let image = match RgbImage::new(w, h, rgb) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    (image, Matte { width: w, height: h, alpha })
}

fn first_prompt(w: usize, h: usize) -> Prompt {
    let (cx, cy, rx, ry, rh) = figure(0.0, w as f32, h as f32);
    let (w, h) = (w as f32, h as f32);
    Prompt {
        points: vec![Point { x: cx / w, y: cy / h, positive: true }],
        rect: Some(Rect { x0: (cx - rx) / w, y0: (cy - ry - rh * 1.8) / h, x1: (cx + rx) / w, y1: (cy + ry) / h }),
        prior: None,
        prefer_whole: false,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let model = args.next().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("models/mobilesam/mobile_sam.pt"));
    let frames: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(240).clamp(2, 10_000);

    let t = Instant::now();
    let sam = match MobileSam::load(&model) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("load: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);

    for (w, h) in [(1920, 1080), (6240, 4160)] {
        let (img, truth) = frame(0.0, w, h);
        let prompt = first_prompt(w, h);
        let mut best = (f64::MAX, f64::MAX, 0.0f32);
        for _ in 0..3 {
            let t = Instant::now();
            let emb = match sam.embed(&img) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            let te = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let seg = match sam.segment(&emb, &prompt) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            let ts = t.elapsed().as_secs_f64() * 1e3;
            let iou = seg.matte(w, h).ok().and_then(|m| m.iou(&truth)).unwrap_or(0.0);
            if te + ts < best.0 + best.1 {
                best = (te, ts, iou);
            }
        }
        println!("{w}x{h}: embed {:.0} ms, segment {:.1} ms (best of 3), first-click IoU {:.3}", best.0, best.1, best.2);
    }

    let (w, h) = (1920, 1080);
    let mut prompt = first_prompt(w, h);
    let mut ious = Vec::with_capacity(frames);
    let start = Instant::now();
    for i in 0..frames {
        let (img, truth) = frame(i as f32 / (frames - 1) as f32, w, h);
        let seg = match sam.embed(&img).and_then(|e| sam.segment(&e, &prompt)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("frame {i}: {e}");
                std::process::exit(1);
            }
        };
        let iou = seg.matte(w, h).ok().and_then(|m| m.iou(&truth)).unwrap_or(0.0);
        ious.push(iou);
        if i % 24 == 0 {
            println!("frame {i:4}: IoU {iou:.3} (score {:.2})", seg.score);
        }
        match follow_prompt(&seg, (w, h)) {
            Some(p) => prompt = p,
            None => println!("frame {i}: subject lost, keeping the previous prompt"),
        }
    }
    let mean = ious.iter().sum::<f32>() / ious.len() as f32;
    let min = ious.iter().copied().fold(1.0f32, f32::min);
    let secs = start.elapsed().as_secs_f64();
    println!("follow {frames} frames at {w}x{h}: mean IoU {mean:.3}, min {min:.3}, {:.0} ms / frame", secs * 1e3 / frames as f64);
}
