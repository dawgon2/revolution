//! G3.4: SAM 2.1's GPU passes (`filmcraft_gpu::sam2`) against the CPU reference
//! (`filmcraft_mask::sam2::cpu`), stage by stage, plus GPU timings.
//!
//! `cargo run --release -p filmcraft-mask --features sam --example sam2_parity -- [large|base]`
//!
//! Prints, per image-encoder stage, the largest difference relative to the largest reference
//! value; the memory encoder and memory attention likewise; the mask IoU decoded from both; and
//! the GPU's time per image encode / memory attention (7 memory frames + 16 pointers) / memory
//! encode. `FC_CACHE=<dir>` keeps the CPU results on disk (they take minutes). On the owner's
//! machine `MESA_VK_DEVICE_SELECT=8086:4c8a MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1`
//! selects the Intel iGPU.

#[path = "../tests/common/sam2_gpu.rs"]
mod sam2_gpu;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use filmcraft_mask::sam2::cpu::Sam2Cpu;
use filmcraft_mask::sam2::heads::{HeadPoint, Pick, Sam2Heads};
use filmcraft_mask::sam2::weights::Sam2Weights;
use filmcraft_mask::sam2::{Sam2Compute, Sam2Config, Sam2Size, prepare_input, tests_support};
use filmcraft_mask::{LOW_RES, RgbImage, Segment};

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("{e}");
    std::process::exit(1)
}

/// max |a − b| / max |b|, and max |a − b|.
fn rel(a: &[f32], b: &[f32]) -> (f32, f32) {
    if a.len() != b.len() {
        return (f32::INFINITY, f32::INFINITY);
    }
    let d = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
    let m = b.iter().map(|v| v.abs()).fold(0f32, f32::max).max(1e-12);
    (d / m, d)
}

fn cached(name: &str, f: impl FnOnce() -> Vec<f32>) -> Vec<f32> {
    let Ok(dir) = std::env::var("FC_CACHE") else { return f() };
    let path = PathBuf::from(dir).join(format!("{name}.bin"));
    if let Ok(b) = std::fs::read(&path) {
        return b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    }
    let v = f();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    let _ = std::fs::write(&path, bytes);
    v
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn main() {
    let size = match std::env::args().nth(1).as_deref() {
        Some("base") | Some("base+") | Some("b+") => Sam2Size::BasePlus,
        _ => Sam2Size::Large,
    };
    let tag = if size == Sam2Size::Large { "large" } else { "base" };
    let (pt, yaml) = tests_support::files(size).unwrap_or_else(|| fail("SAM 2.1 not installed"));
    let cfg = Sam2Config::from_yaml(&std::fs::read_to_string(yaml).unwrap_or_else(|e| fail(e))).unwrap_or_else(|e| fail(e));
    let t = Instant::now();
    let w = Arc::new(Sam2Weights::load(&pt, cfg).unwrap_or_else(|e| fail(e)));
    println!("{} loaded in {:.1} s", size.label(), t.elapsed().as_secs_f64());
    let t = Instant::now();
    let gpu = sam2_gpu::open(&w).unwrap_or_else(|e| fail(e));
    println!("GPU {} ready in {:.1} s", gpu.name(), t.elapsed().as_secs_f64());
    let cpu = Sam2Cpu::new(w.clone()).unwrap_or_else(|e| fail(e));
    let heads = Sam2Heads::new(w.clone()).unwrap_or_else(|e| fail(e));

    // a figure on a textured background
    let (iw, ih) = (1280, 720);
    let rgb: Vec<u8> = (0..iw * ih)
        .flat_map(|i| {
            let (x, y) = (i % iw, i / iw);
            let inside = ((x as f32 - 640.0) / 140.0).powi(2) + ((y as f32 - 380.0) / 230.0).powi(2) <= 1.0;
            if inside { [200, 70, 50] } else { [(x / 7 % 255) as u8, (y / 5 % 255) as u8, 150] }
        })
        .collect();
    let img = RgbImage::new(iw, ih, rgb).unwrap_or_else(|e| fail(e));
    let input = prepare_input(&img);

    // image encoder, stage by stage
    let t = Instant::now();
    let g_stages = gpu.0.image_stages(&input).unwrap_or_else(|e| fail(e));
    println!("GPU stages in {:.2} s", t.elapsed().as_secs_f64());
    let mut c_stages = Vec::new();
    let names: Vec<String> = g_stages.iter().map(|(n, _)| n.clone()).collect();
    let mut have = names.iter().all(|n| std::env::var("FC_CACHE").is_ok_and(|d| PathBuf::from(d).join(format!("{tag}-{n}.bin")).is_file()));
    if !have {
        let t = Instant::now();
        let s = cpu.image_stages(&input).unwrap_or_else(|e| fail(e));
        println!("CPU stages in {:.1} s", t.elapsed().as_secs_f64());
        for (n, v) in s {
            c_stages.push((n.clone(), cached(&format!("{tag}-{n}"), || v)));
        }
        have = true;
    } else {
        for n in &names {
            c_stages.push((n.clone(), cached(&format!("{tag}-{n}"), Vec::new)));
        }
    }
    let _ = have;
    let mut worst = 0f32;
    for ((n, g), (_, c)) in g_stages.iter().zip(&c_stages) {
        let (r, d) = rel(g, c);
        worst = worst.max(r);
        println!("  {n:12} rel {r:.2e} (max abs diff {d:.2e}, {} values)", c.len());
    }
    let get = |n: &str, v: &[(String, Vec<f32>)]| v.iter().find(|(k, _)| k == n).map(|(_, d)| d.clone()).unwrap_or_default();
    let (cs0, cs1, cfeat) = (get("s0", &c_stages), get("s1", &c_stages), get("feat", &c_stages));
    let (gs0, gs1, gfeat) = (get("s0", &g_stages), get("s1", &g_stages), get("feat", &g_stages));

    // memory encoder on the CPU features with a mask of the figure
    let mut mask = vec![-10.0f32; 1024 * 1024];
    for y in 0..1024 {
        for x in 0..1024 {
            let (fx, fy) = (x as f32 * iw as f32 / 1024.0, y as f32 * ih as f32 / 1024.0);
            if ((fx - 640.0) / 140.0).powi(2) + ((fy - 380.0) / 230.0).powi(2) <= 1.0 {
                mask[y * 1024 + x] = 10.0;
            }
        }
    }
    let c_mem = cached(&format!("{tag}-memenc"), || cpu.memory_encoder(&cfeat, &mask).unwrap_or_else(|e| fail(e)));
    let g_mem = gpu.memory_encoder(&cfeat, &mask).unwrap_or_else(|e| fail(e));
    let (r, d) = rel(&g_mem, &c_mem);
    worst = worst.max(r);
    println!("  {:12} rel {r:.2e} (max abs diff {d:.2e})", "mem_encoder");

    // memory attention: two memory frames + 3 pointers (12 tokens)
    let mem_pos = filmcraft_mask::sam2::sine_position(64, 64, 64);
    let mut memory = c_mem.clone();
    memory.extend(c_mem.iter().map(|v| v * 0.5));
    let mut pos = mem_pos.clone();
    pos.extend(mem_pos.iter().map(|v| v + 0.01));
    for k in 0..12 {
        memory.extend((0..64).map(|i| ((i * 7 + k * 13) % 17) as f32 / 17.0 - 0.5));
        pos.extend((0..64).map(|i| ((i + k) % 5) as f32 / 5.0));
    }
    let c_att = cached(&format!("{tag}-memattn"), || cpu.memory_attention(&cfeat, &memory, &pos, 12).unwrap_or_else(|e| fail(e)));
    let g_att = gpu.memory_attention(&cfeat, &memory, &pos, 12).unwrap_or_else(|e| fail(e));
    let (r, d) = rel(&g_att, &c_att);
    worst = worst.max(r);
    println!("  {:12} rel {r:.2e} (max abs diff {d:.2e})", "mem_attn");
    println!("worst relative difference {worst:.2e}");

    // masks decoded from both
    let click = [HeadPoint { x: 512.0, y: 540.0, label: 1 }];
    let seg = |pix: &[f32], s0: &[f32], s1: &[f32]| {
        let o = heads.forward(pix, s0, s1, &click, None, true, Pick::Best).unwrap_or_else(|e| fail(e));
        Segment { logits: o.mask, cover: (1.0, 1.0), score: o.iou }
    };
    let mut cpix = cfeat.clone();
    let mut gpix = gfeat.clone();
    let nm = heads.param("no_mem_embed").unwrap_or_else(|e| fail(e));
    for (row_c, row_g) in cpix.chunks_mut(256).zip(gpix.chunks_mut(256)) {
        row_c.iter_mut().zip(&nm).for_each(|(a, b)| *a += b);
        row_g.iter_mut().zip(&nm).for_each(|(a, b)| *a += b);
    }
    let a = seg(&cpix, &cs0, &cs1).matte(iw, ih).unwrap_or_else(|e| fail(e));
    let b = seg(&gpix, &gs0, &gs1).matte(iw, ih).unwrap_or_else(|e| fail(e));
    println!("mask IoU GPU vs CPU: {:.5} (area {} px)", a.iou(&b).unwrap_or(0.0), a.alpha.iter().filter(|v| **v > 127).count());
    let _ = LOW_RES;

    // timings
    let _ = gpu.0.image(&input);
    let ms: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let _ = gpu.0.image(&input).unwrap_or_else(|e| fail(e));
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    println!("GPU image encoder: median {:.1} ms", median(ms));
    let mut full = Vec::new();
    let mut fpos = Vec::new();
    for _ in 0..7 {
        full.extend_from_slice(&c_mem);
        fpos.extend_from_slice(&mem_pos);
    }
    for k in 0..64 {
        full.extend((0..64).map(|i| ((i + k) % 9) as f32 / 9.0));
        fpos.extend((0..64).map(|i| ((i * 3 + k) % 7) as f32 / 7.0));
    }
    let _ = gpu.memory_attention(&cfeat, &full, &fpos, 64);
    let ms: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let _ = gpu.memory_attention(&cfeat, &full, &fpos, 64).unwrap_or_else(|e| fail(e));
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    println!("GPU memory attention (7 frames + 16 pointers): median {:.1} ms", median(ms));
    let ms: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let _ = gpu.memory_encoder(&cfeat, &mask).unwrap_or_else(|e| fail(e));
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    println!("GPU memory encoder: median {:.1} ms", median(ms));
    let t = Instant::now();
    let reps = 5;
    for _ in 0..reps {
        let _ = heads.forward(&cpix, &cs0, &cs1, &click, None, true, Pick::Best);
    }
    println!("CPU decoder: {:.1} ms", t.elapsed().as_secs_f64() * 1e3 / reps as f64);
}
