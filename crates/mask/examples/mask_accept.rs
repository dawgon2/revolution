//! G3 acceptance harness: MobileSAM (or SAM 2.1) following a person-like figure for 10 s.
//!
//! `cargo run --release -p filmcraft-mask --features sam --example mask_accept -- [model] [frames]`
//!
//! `FC_TRACKER=sam2-large` / `sam2-base` runs SAM 2.1 (Better / Faster) through the
//! `VideoSegmenter` interface on the GPU instead (G3.4): frame 0 gets the click + box, every later
//! frame is propagated from the memory; `FC_FIX=<frames>` adds the fix stroke (click + box + a
//! negative click on the nearest bystander) into the memory on those frames.
//!
//! Three synthetic composites at 1920×1080 (frame count default 240 = 10 s at 24 fps), each a
//! figure with a head, torso, swinging arms and walking legs (a capsule per limb) whose clothing
//! is textured, over a different background:
//! - `gradient`: a smooth sky-to-ground gradient;
//! - `busy`: stripes, a checker and noise in colours close to the figure's;
//! - `crowd`: three other static figures of the same shape in other colours behind it;
//! - `shadow`: the gradient, with the right half of the frame (figure included) in a shadow that
//!   darkens it by half, so the figure's colours change as it walks in (halfway through);
//! - `drift`: the figure barely moving, its swinging arm touching a wall of a slightly similar
//!   colour beside it for the whole walk (the mask must not creep into it).
//!
//! Frame 0 gets one click on the torso plus a box; every later frame only `follow_prompt`. Prints
//! the mean and minimum IoU against the drawn figure per scene and checks the G3 target (mean ≥
//! 0.90, minimum ≥ 0.80). Synthetic, not real footage.
//!
//! `FC_GPU=1`: the image encoder on the GPU (`filmcraft_gpu::tinyvit`, as the engine uses it)
//! instead of candle on the CPU; the per-frame time is then printed split into render, encode
//! and follow (decoder calls + colour analysis).

use std::path::PathBuf;
use std::time::Instant;

#[path = "../tests/common/sam2_gpu.rs"]
mod sam2_gpu;

use std::sync::Arc;

use filmcraft_gpu::tinyvit::{TinyVitGpu, Weight};
use filmcraft_mask::sam::MobileSam;
use filmcraft_mask::sam2::heads::Sam2Heads;
use filmcraft_mask::sam2::tracker::{Sam2Tracker, Selection};
use filmcraft_mask::sam2::weights::Sam2Weights;
use filmcraft_mask::sam2::{Sam2Compute, Sam2Config, Sam2Size};
use filmcraft_mask::track::{FollowOptions, Track, follow};
use filmcraft_mask::video::{Direction, VideoSegmenter};
use filmcraft_mask::{Matte, Point, Prompt, Rect, RgbImage, Segmenter, follow_prompt};

const W: usize = 1920;
const H: usize = 1080;

/// Distance from p to the segment a–b.
fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = (dx * dx + dy * dy).max(1e-6);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// A figure: capsules (a, b, radius). `cx` is the hip centre, `phase` drives the walk.
fn figure(cx: f32, ground: f32, s: f32, phase: f32) -> Vec<((f32, f32), (f32, f32), f32)> {
    let hip = (cx, ground - 0.45 * s);
    let neck = (cx, ground - 0.85 * s);
    let head = (cx, ground - 0.95 * s);
    let swing = phase.sin() * 0.18 * s;
    let foot = |d: f32| (cx + d, ground);
    let hand = |d: f32| (cx + d, ground - 0.5 * s);
    vec![
        (head, head, 0.075 * s),
        (neck, hip, 0.09 * s),
        (neck, hand(swing), 0.035 * s),
        (neck, hand(-swing), 0.035 * s),
        (hip, foot(-swing), 0.045 * s),
        (hip, foot(swing), 0.045 * s),
    ]
}

fn inside(p: (f32, f32), f: &[((f32, f32), (f32, f32), f32)]) -> bool {
    f.iter().any(|(a, b, r)| seg_dist(p, *a, *b) <= *r)
}

fn hash(x: usize, y: usize, k: usize) -> u8 {
    let v = (x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663) ^ (k as u32).wrapping_mul(83_492_791);
    (v.wrapping_mul(2_654_435_761) >> 24) as u8
}

#[derive(Clone, Copy)]
enum Scene {
    Gradient,
    Busy,
    Crowd,
    Shadow,
    Drift,
    /// Not part of the target: the crowd dressed exactly like the subject (FC_SCENE=twins), to
    /// show where colour stops helping.
    Twins,
}

fn render(scene: Scene, t: f32, k: usize) -> (RgbImage, Matte, Rect, (f32, f32)) {
    let s = H as f32 * 0.6;
    let ground = H as f32 * 0.92;
    let cx = match scene {
        Scene::Drift => W as f32 * 0.45 + 12.0 * (t * 6.0).sin(),
        _ => W as f32 * (0.2 + 0.6 * t),
    };
    let me = figure(cx, ground, s, t * 40.0);
    // the wall beside the drift figure: just past its hand's reach at the start
    let wall = (W as f32 * 0.45 + 0.2 * s, W as f32 * 0.45 + 0.6 * s, ground - 1.0 * s);
    let others: Vec<_> = match scene {
        Scene::Crowd | Scene::Twins => [0.3f32, 0.55, 0.8].iter().map(|x| figure(W as f32 * x, ground - 30.0, s * 0.9, *x * 7.0)).collect(),
        _ => Vec::new(),
    };
    let mut rgb = Vec::with_capacity(W * H * 3);
    let mut alpha = Vec::with_capacity(W * H);
    for y in 0..H {
        for x in 0..W {
            let p = (x as f32, y as f32);
            let n = hash(x, y, k) as i32 / 16 - 8;
            let dim = |v: i32| if matches!(scene, Scene::Shadow) && x >= W / 2 { v / 2 } else { v };
            if inside(p, &me) {
                // a striped shirt / dark trousers, with noise
                let shirt = y as f32 > ground - 0.45 * s;
                let stripe = if (y / 9) % 2 == 0 { 25 } else { -25 };
                let (r, g, b) = if shirt { (60, 60, 90) } else { (170 + stripe, 120 + stripe, 80) };
                let c = |v: i32| dim(v + n).clamp(0, 255) as u8;
                rgb.extend_from_slice(&[c(r), c(g), c(b)]);
                alpha.push(255);
                continue;
            }
            alpha.push(0);
            let (r, g, b) = match scene {
                Scene::Gradient | Scene::Shadow => {
                    let v = y as f32 / H as f32;
                    ((90.0 + 80.0 * v) as i32, (140.0 - 30.0 * v) as i32, (210.0 - 120.0 * v) as i32)
                }
                Scene::Busy => {
                    let st = ((x as f32 / 37.0).sin() * 40.0) as i32;
                    let ch = if ((x / 64) + (y / 48)) % 2 == 0 { 30 } else { -30 };
                    (150 + st + ch, 110 + ch, 80 - st)
                }
                Scene::Crowd => {
                    let hit = others.iter().enumerate().find(|(_, f)| inside(p, f)).map(|(i, _)| i);
                    match hit {
                        Some(0) => (80, 160, 90),
                        Some(1) => (180, 170, 60),
                        Some(_) => (90, 90, 170),
                        None => (120, 120, 125),
                    }
                }
                Scene::Drift => {
                    if (wall.0..wall.1).contains(&p.0) && p.1 >= wall.2 && p.1 <= ground {
                        (150, 112, 95)
                    } else {
                        (120, 120, 125)
                    }
                }
                Scene::Twins => {
                    if others.iter().any(|f| inside(p, f)) {
                        let shirt = y as f32 > ground - 30.0 - 0.45 * s * 0.9;
                        let stripe = if (y / 9) % 2 == 0 { 25 } else { -25 };
                        if shirt { (60, 60, 90) } else { (170 + stripe, 120 + stripe, 80) }
                    } else {
                        (120, 120, 125)
                    }
                }
            };
            let c = |v: i32| dim(v + n * 2).clamp(0, 255) as u8;
            rgb.extend_from_slice(&[c(r), c(g), c(b)]);
        }
    }
    let img = RgbImage::new(W, H, rgb).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let r = Rect { x0: (cx - 0.25 * s) / W as f32, y0: (ground - 1.05 * s) / H as f32, x1: (cx + 0.25 * s) / W as f32, y1: ground / H as f32 };
    (img, Matte { width: W, height: H, alpha }, r, (cx / W as f32, (ground - 0.65 * s) / H as f32))
}

/// The inside pixel nearest the inside's centroid (normalised).
fn interior_point(m: &Matte) -> Option<Point> {
    let (mut sx, mut sy, mut n) = (0f64, 0f64, 0f64);
    for (i, a) in m.alpha.iter().enumerate() {
        if *a >= 128 {
            sx += (i % m.width) as f64;
            sy += (i / m.width) as f64;
            n += 1.0;
        }
    }
    if n == 0.0 {
        return None;
    }
    let (cx, cy) = (sx / n, sy / n);
    let best = m.alpha.iter().enumerate().filter(|(_, a)| **a >= 128).map(|(i, _)| ((i % m.width) as f64, (i / m.width) as f64)).min_by(|a, b| {
        let d = |p: &(f64, f64)| (p.0 - cx).powi(2) + (p.1 - cy).powi(2);
        d(a).total_cmp(&d(b))
    })?;
    Some(Point { x: ((best.0 + 0.5) / m.width as f64) as f32, y: ((best.1 + 0.5) / m.height as f64) as f32, positive: true })
}

/// `FC_CACHE=<dir>`: keep encoder outputs on disk (the scenes are deterministic), so decoder-only
/// experiments run in seconds.
fn embed_cached(
    sam: &MobileSam,
    gpu: Option<&TinyVitGpu>,
    img: &RgbImage,
    scene: &str,
    frames: usize,
    i: usize,
) -> Result<filmcraft_mask::Embedding, filmcraft_mask::MaskError> {
    let embed = |img: &RgbImage| match gpu {
        Some(g) => {
            let (input, fitted) = sam.prepare_input(img)?;
            let feats = g.encode(&input).map_err(|e| filmcraft_mask::MaskError::Failed(e.to_string()))?;
            sam.embedding_from_features(feats, img.width(), img.height(), fitted)
        }
        None => sam.embed(img),
    };
    let Ok(dir) = std::env::var("FC_CACHE") else { return embed(img) };
    let path = PathBuf::from(dir).join(format!("{scene}-{frames}-{i}.bin"));
    if let Ok(bytes) = std::fs::read(&path)
        && bytes.len() > 8
    {
        let fw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let fh = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let v: Vec<f32> = bytes[8..].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        return sam.import_embedding((img.width(), img.height()), v, (fw, fh));
    }
    let e = embed(img)?;
    let (v, (fw, fh)) = sam.export_embedding(&e)?;
    let mut bytes = Vec::with_capacity(8 + v.len() * 4);
    bytes.extend_from_slice(&(fw as u32).to_le_bytes());
    bytes.extend_from_slice(&(fh as u32).to_le_bytes());
    for f in v {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(&path, bytes);
    Ok(e)
}

/// `FC_DUMP=3,6,12`: write a half-size overlay PPM per listed frame to `target/agent-mask/diag/`
/// (frame darkened; truth only = green, prediction only = red, both = yellow).
fn dump(scene: &str, i: usize, img: &RgbImage, truth: &Matte, seg: &filmcraft_mask::Segment) {
    let Ok(list) = std::env::var("FC_DUMP") else { return };
    if !list.split(',').any(|v| v.trim().parse() == Ok(i)) {
        return;
    }
    let Ok(m) = seg.matte(W, H) else { return };
    let (w, h) = (W / 2, H / 2);
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for y in 0..h {
        for x in 0..w {
            let k = (y * 2) * W + x * 2;
            let (t, p) = (truth.alpha[k] >= 128, m.alpha[k] >= 128);
            let px = &img.data()[k * 3..k * 3 + 3];
            let mix = |c: u8, o: u8| ((c as u16 + o as u16 * 2) / 3) as u8;
            let o = match (t, p) {
                (true, true) => [255, 255, 0],
                (true, false) => [0, 255, 0],
                (false, true) => [255, 0, 0],
                _ => [0, 0, 0],
            };
            if t || p {
                out.extend_from_slice(&[mix(px[0], o[0]), mix(px[1], o[1]), mix(px[2], o[2])]);
            } else {
                out.extend_from_slice(px);
            }
        }
    }
    let dir = PathBuf::from(std::env::var("FC_DUMP_DIR").unwrap_or_else(|_| "target/agent-mask/diag".into()));
    let _ = std::fs::create_dir_all(&dir);
    let tag = std::env::var("FC_FOLLOW").unwrap_or_default();
    let _ = std::fs::write(dir.join(format!("{scene}-{tag}-{i:03}.ppm")), out);
}

/// The fix stroke for a frame, as a user would paint it: the box, positive points down the
/// subject (the centre of the first run of subject pixels on five rows of its box, from the
/// drawn truth), and negative points on the nearest bystander's head and torso where the subject
/// doesn't cover them.
fn fix_prompt(torso: (f32, f32), rect: Rect, truth: &Matte) -> Prompt {
    let mut prompt = Prompt { points: Vec::new(), rect: Some(rect), prior: None, prefer_whole: false };
    if let Some(b) = truth.bounds() {
        for f in [0.06f32, 0.25, 0.45, 0.65, 0.85] {
            let y = (((b.y0 + (b.y1 - b.y0) * f) * H as f32) as usize).min(H - 1);
            let row = &truth.alpha[y * W..(y + 1) * W];
            if let Some(x0) = row.iter().position(|a| *a >= 128) {
                let len = row[x0..].iter().take_while(|a| **a >= 128).count();
                prompt.points.push(Point { x: (x0 as f32 + len as f32 / 2.0) / W as f32, y: y as f32 / H as f32, positive: true });
            }
        }
    }
    if prompt.points.is_empty() {
        prompt.points.push(Point { x: torso.0, y: torso.1, positive: true });
    }
    let s = H as f32 * 0.6;
    let ground = H as f32 * 0.92 - 30.0;
    let near = [0.3f32, 0.55, 0.8].iter().map(|x| W as f32 * x).min_by(|a, b| (a - torso.0 * W as f32).abs().total_cmp(&(b - torso.0 * W as f32).abs()));
    if let Some(nx) = near {
        for ty in [ground - 0.95 * s * 0.9, ground - 0.65 * s * 0.9] {
            if truth.alpha[ty as usize * W + nx as usize] < 128 {
                prompt.points.push(Point { x: nx / W as f32, y: ty / H as f32, positive: false });
            }
        }
    }
    prompt
}

/// SAM 2.1 through `VideoSegmenter` (G3.4).
fn run_sam2(size: Sam2Size, frames: usize) {
    let fail = |e: &dyn std::fmt::Display| -> ! {
        eprintln!("{e}");
        std::process::exit(1)
    };
    let (pt, yaml) = filmcraft_mask::sam2::tests_support::files(size).unwrap_or_else(|| fail(&"SAM 2.1 is not installed"));
    let cfg = Sam2Config::from_yaml(&std::fs::read_to_string(yaml).unwrap_or_else(|e| fail(&e))).unwrap_or_else(|e| fail(&e));
    let w = Arc::new(Sam2Weights::load(&pt, cfg).unwrap_or_else(|e| fail(&e)));
    let compute: Arc<dyn Sam2Compute> = Arc::new(sam2_gpu::open(&w).unwrap_or_else(|e| fail(&e)));
    let heads = Arc::new(Sam2Heads::new(w).unwrap_or_else(|e| fail(&e)));
    println!("{} on {}", size.label(), compute.name());
    let only = std::env::var("FC_SCENE").unwrap_or_default();
    let fixes: Vec<usize> = std::env::var("FC_FIX").map(|l| l.split(',').filter_map(|v| v.trim().parse().ok()).collect()).unwrap_or_default();
    let mut pass = true;
    for (name, scene) in [
        ("gradient", Scene::Gradient),
        ("busy", Scene::Busy),
        ("crowd", Scene::Crowd),
        ("shadow", Scene::Shadow),
        ("drift", Scene::Drift),
        ("twins", Scene::Twins),
    ] {
        if !only.is_empty() && !only.split(',').any(|o| o == name) {
            continue;
        }
        let mut tracker = Sam2Tracker::new(compute.clone(), heads.clone(), frames).unwrap_or_else(|e| fail(&e));
        // FC_ANCHOR=0: no re-anchoring of collapsing masks
        if std::env::var("FC_ANCHOR").is_ok_and(|v| v == "0") {
            tracker.set_anchor(false);
        }
        // FC_SELECT=ref: the reference's candidate choice; FC_SELECT=<w>: consistency weight w
        match std::env::var("FC_SELECT").ok().as_deref() {
            Some("ref") => tracker.set_selection(Selection::Reference),
            Some(w) => tracker.set_selection(Selection::Consistent { weight: w.parse().unwrap_or(1.0) }),
            None => {}
        }
        let mut ious = Vec::with_capacity(frames);
        let mut ms = Vec::with_capacity(frames);
        let start = Instant::now();
        let stop = std::env::var("FC_STOP").ok().and_then(|v| v.parse().ok()).unwrap_or(frames).min(frames);
        for i in 0..stop {
            let (img, truth, rect, torso) = render(scene, i as f32 / (frames - 1) as f32, i);
            let t = Instant::now();
            let r = if i == 0 {
                tracker.add_prompt(
                    0,
                    &img,
                    &Prompt { points: vec![Point { x: torso.0, y: torso.1, positive: true }], rect: Some(rect), prior: None, prefer_whole: false },
                )
            } else if fixes.contains(&i) {
                // the frame is tracked first (as in the editor), then the fix stroke enters the memory
                tracker.propagate(i, &img, Direction::Forward).and_then(|_| tracker.add_prompt(i, &img, &fix_prompt(torso, rect, &truth)))
            } else {
                tracker.propagate(i, &img, Direction::Forward)
            };
            let m = r.unwrap_or_else(|e| fail(&format!("{name} frame {i}: {e}")));
            ms.push(t.elapsed().as_secs_f64() * 1e3);
            let iou = m.segment.matte(W, H).ok().and_then(|mm| mm.iou(&truth)).unwrap_or(0.0);
            let verbose = std::env::var("FC_VERBOSE").ok();
            if verbose.is_some() && (i % 12 == 0 || iou < 0.9 || verbose.as_deref() == Some("2")) {
                let c: Vec<String> = tracker.last_candidates().iter().map(|(a, b)| format!("{a:.2}/{b:.2}")).collect();
                println!("  {name} frame {i}: IoU {iou:.3}{} [{}]", if m.visible { "" } else { " (not visible)" }, c.join(" "));
            }
            dump(name, i, &img, &truth, &m.segment);
            ious.push(iou);
        }
        let mean = ious.iter().sum::<f32>() / ious.len() as f32;
        let min = ious.iter().copied().fold(1.0f32, f32::min);
        let ok = mean >= 0.90 && min >= 0.80;
        pass &= ok;
        ms.sort_by(f64::total_cmp);
        println!(
            "{name:9} {frames} frames: mean IoU {mean:.3}, min {min:.3} (frame {}), {:.0} ms / frame median ({:.1} s total) — {}",
            ious.iter().position(|v| *v == min).unwrap_or(0),
            ms.get(ms.len() / 2).copied().unwrap_or(0.0),
            start.elapsed().as_secs_f64(),
            if ok { "PASS" } else { "FAIL" }
        );
    }
    println!("{}", if pass { "G3 target met (synthetic)" } else { "G3 target NOT met (synthetic)" });
}

fn main() {
    if let Ok(t) = std::env::var("FC_TRACKER")
        && t.starts_with("sam2")
    {
        let frames: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(240).clamp(2, 10_000);
        run_sam2(if t.contains("base") { Sam2Size::BasePlus } else { Sam2Size::Large }, frames);
        return;
    }
    let mut args = std::env::args().skip(1);
    let model = args.next().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("models/mobilesam/mobile_sam.pt"));
    let frames: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(240).clamp(2, 10_000);
    let sam = MobileSam::load(&model).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let gpu = std::env::var("FC_GPU").is_ok_and(|v| v == "1").then(|| {
        let w: std::collections::HashMap<String, Weight> = sam
            .encoder_weights()
            .unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1)
            })
            .into_iter()
            .map(|t| (t.name, Weight { shape: t.shape, data: t.data }))
            .collect();
        let g = TinyVitGpu::open(&w).unwrap_or_else(|e| {
            eprintln!("GPU encoder: {e}");
            std::process::exit(1)
        });
        println!("image encoder on {}", g.name());
        g
    });
    let mut pass = true;
    let only = std::env::var("FC_SCENE").unwrap_or_default();
    for (name, scene) in [
        ("gradient", Scene::Gradient),
        ("busy", Scene::Busy),
        ("crowd", Scene::Crowd),
        ("shadow", Scene::Shadow),
        ("drift", Scene::Drift),
        ("twins", Scene::Twins),
    ] {
        if name == "twins" && only != name {
            continue;
        }
        if !only.is_empty() && only != name {
            continue;
        }
        let start = Instant::now();
        let (mut t_render, mut t_encode, mut t_follow) = (0f64, 0f64, 0f64);
        let mut prompt = Prompt::default();
        let mut ious = Vec::with_capacity(frames);
        let mut last_box: Option<Rect> = None;
        let mut track: Option<Track> = None;
        let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
        let d = FollowOptions::default();
        let opts = FollowOptions {
            positives: env("FC_POS").map_or(d.positives, |v| v as usize),
            negatives: env("FC_NEG").map_or(d.negatives, |v| v as usize),
            prior: env("FC_PRIOR").map_or(d.prior, |v| v > 0.0),
            multimask: env("FC_MULTI").map_or(d.multimask, |v| v > 0.0),
            carve: env("FC_CARVE").map_or(d.carve, |v| v > 0.0),
            grow: env("FC_GROW").unwrap_or(d.grow),
            carve_min: env("FC_CARVEMIN").map_or(d.carve_min, |v| v as usize),
            carve_grow: env("FC_CARVEGROW").map_or(d.carve_grow, |v| v as usize),
            accept: env("FC_ACCEPT").unwrap_or(d.accept),
            grow_min: env("FC_GROWMIN").unwrap_or(d.grow_min),
        };
        let verbose_all = std::env::var("FC_VERBOSE").is_ok_and(|v| v == "2");
        // FC_STOP=n: only the first n frames (of a `frames`-long walk, so cached embeddings match)
        let stop = std::env::var("FC_STOP").ok().and_then(|v| v.parse().ok()).unwrap_or(frames).min(frames);
        for i in 0..stop {
            let t0 = Instant::now();
            let (img, truth, rect, torso) = render(scene, i as f32 / (frames - 1) as f32, i);
            t_render += t0.elapsed().as_secs_f64();
            // FC_FIX=12,40: a user fix stroke on these frames (the frame-0 click + box again)
            let fix = std::env::var("FC_FIX").is_ok_and(|l| l.split(',').any(|v| v.trim().parse() == Ok(i)));
            if i == 0 || fix {
                prompt = Prompt { points: vec![Point { x: torso.0, y: torso.1, positive: true }], rect: Some(rect), prior: None, prefer_whole: false };
                // the fix stroke also marks the nearest bystander's torso as "not this" (when
                // it is not hidden behind the subject)
                if fix {
                    let s = H as f32 * 0.6;
                    let ty = H as f32 * 0.92 - 30.0 - 0.65 * s * 0.9;
                    let near = [0.3f32, 0.55, 0.8]
                        .iter()
                        .map(|x| W as f32 * x)
                        .min_by(|a, b| (a - torso.0 * W as f32).abs().total_cmp(&(b - torso.0 * W as f32).abs()));
                    if let Some(nx) = near
                        && truth.alpha[ty as usize * W + nx as usize] < 128
                    {
                        prompt.points.push(Point { x: nx / W as f32, y: ty / H as f32, positive: false });
                    }
                }
            }
            let tracking = std::env::var("FC_FOLLOW").map(|v| v.is_empty() || v == "track").unwrap_or(true);
            let t0 = Instant::now();
            let emb = embed_cached(&sam, gpu.as_ref(), &img, name, frames, i).unwrap_or_else(|e| {
                eprintln!("{name} frame {i}: {e}");
                std::process::exit(1)
            });
            t_encode += t0.elapsed().as_secs_f64();
            let t0 = Instant::now();
            let seg = if tracking && i > 0 && !fix {
                let Some(t) = track.as_ref() else {
                    ious.push(0.0);
                    continue;
                };
                match follow(&sam, &emb, &img, t, &opts) {
                    Ok(Some((t, rep))) => {
                        if verbose_all {
                            let f = |v: &[Point]| v.iter().map(|p| format!("({:.0},{:.0})", p.x * W as f32, p.y * H as f32)).collect::<Vec<_>>().join(" ");
                            println!("    {i}: calls {} leak {:.3} pos {} neg {}", rep.calls, rep.leak, f(&rep.positives), f(&rep.negatives));
                        }
                        let s = t.segment().clone();
                        track = Some(t);
                        s
                    }
                    Ok(None) => {
                        // lost: hold the last mask
                        t.segment().clone()
                    }
                    Err(e) => {
                        eprintln!("{name} frame {i}: {e}");
                        std::process::exit(1)
                    }
                }
            } else {
                match sam.segment(&emb, &prompt) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{name} frame {i}: {e}");
                        std::process::exit(1)
                    }
                }
            };
            if tracking && (i == 0 || fix) {
                track = Track::start(seg.clone(), &img);
            }
            t_follow += t0.elapsed().as_secs_f64();
            let iou = seg.matte(W, H).ok().and_then(|m| m.iou(&truth)).unwrap_or(0.0);
            if std::env::var("FC_VERBOSE").is_ok() && (i % 6 == 0 || verbose_all) {
                println!("  {name} frame {i}: IoU {iou:.3}");
            }
            ious.push(iou);
            dump(name, i, &img, &truth, &seg);
            if let Some(mut p) = follow_prompt(&seg, (W, H)) {
                // FC_FOLLOW: "prior" (default: prior + box), "point" (box + interior point, no
                // prior), "both" (prior + box + interior point)
                let style = std::env::var("FC_FOLLOW").unwrap_or_default();
                if (style == "point" || style == "both")
                    && let Some(pt) = seg.matte(480, 270).ok().and_then(|m| interior_point(&m))
                {
                    p.points.push(pt);
                }
                if style == "point" {
                    p.prior = None;
                }
                // "steady": the box may change size by at most 5 % a frame, centred on the
                // subject's interior point (a runaway box can't swallow a neighbour)
                if style == "steady" {
                    let pt = seg.matte(480, 270).ok().and_then(|m| interior_point(&m));
                    if let (Some(prev), Some(r), Some(pt)) = (last_box.or(prompt.rect), p.rect, pt) {
                        let (pw, ph) = (prev.x1 - prev.x0, prev.y1 - prev.y0);
                        let w = (r.x1 - r.x0).clamp(pw * 0.95, pw * 1.05);
                        let h = (r.y1 - r.y0).clamp(ph * 0.95, ph * 1.05);
                        // keep the box's offset from the interior point as it was
                        let (ox, oy) = (((r.x0 + r.x1) / 2.0) - pt.x, ((r.y0 + r.y1) / 2.0) - pt.y);
                        let (cx, cy) = (pt.x + ox.clamp(-w / 4.0, w / 4.0), pt.y + oy.clamp(-h / 4.0, h / 4.0));
                        p.rect = Rect { x0: cx - w / 2.0, y0: cy - h / 2.0, x1: cx + w / 2.0, y1: cy + h / 2.0 }.sorted();
                        p.points.push(pt);
                    }
                    last_box = p.rect;
                }
                prompt = p;
            }
        }
        let mean = ious.iter().sum::<f32>() / ious.len() as f32;
        let min = ious.iter().copied().fold(1.0f32, f32::min);
        let ok = mean >= 0.90 && min >= 0.80;
        pass &= ok;
        println!(
            "{name:9} {frames} frames: mean IoU {mean:.3}, min {min:.3} (frame {}), {:.1} s/frame — {}",
            ious.iter().position(|v| *v == min).unwrap_or(0),
            start.elapsed().as_secs_f64() / frames as f64,
            if ok { "PASS" } else { "FAIL" }
        );
        let n = stop.max(1) as f64;
        println!("          per frame: render {:.0} ms, encode {:.0} ms, follow {:.0} ms", t_render / n * 1e3, t_encode / n * 1e3, t_follow / n * 1e3);
    }
    println!("{}", if pass { "G3 target met (synthetic)" } else { "G3 target NOT met (synthetic)" });
}
