//! The roto kernel (fork LO.5) against the CPU rasteriser. Run on each GPU with
//! `MESA_VK_DEVICE_SELECT` (AMD and Intel); they skip without an adapter.

use super::*;
use filmcraft_geom::Vec2;
use filmcraft_project::roto::{Roto, RotoKind, RotoMode, RotoShape};
use filmcraft_project::{MaskPath, MaskVertex, Param, ParamValue};
use filmcraft_render::comp::Coverage;
use filmcraft_time::Tick;

use crate::comp_roto;

fn float(v: f64) -> Param {
    Param::new(ParamValue::Float(v))
}

fn poly(pts: &[(f64, f64)]) -> MaskPath {
    MaskPath::polygon(&pts.iter().map(|&(x, y)| Vec2::new(x, y)).collect::<Vec<_>>())
}

fn with(mut sh: RotoShape, mode: RotoMode, inverted: bool, opacity: f64) -> RotoShape {
    sh.mode = mode;
    sh.inverted = inverted;
    sh.opacity = float(opacity);
    sh
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64, feather: f64) -> RotoShape {
    let mut r = RotoShape::new("r", RotoKind::Bezier, MaskPath::rect(x0, y0, x1, y1));
    r.feather = float(feather);
    r
}

/// Roto scenes on a 320 × 200 frame: curves (Bézier ellipse, tangents, B-splines open and
/// closed), per-point and global feather, expansion, every mode, inversion and opacity, several
/// shapes, a shape partly and one wholly outside the frame, a self-intersecting outline and
/// degenerate shapes.
fn scenes() -> Vec<(&'static str, Roto)> {
    let ellipse = RotoShape::new("ellipse", RotoKind::Bezier, MaskPath::ellipse(Vec2::new(150.0, 100.0), Vec2::new(90.0, 60.0)));
    let mut feathered = ellipse.clone();
    feathered.feather = float(12.0);
    feathered.expansion = float(-3.0);
    // per-point feather (inner / outer differ) and tangents on a Bézier quad
    let mut quad = MaskPath::rect(60.0, 40.0, 260.0, 160.0);
    quad.vertices[0] = quad.vertices[0].with_feather(4.0, 25.0);
    quad.vertices[1].t_out = Vec2::new(40.0, -30.0);
    quad.vertices[2] = quad.vertices[2].with_feather(10.0, 0.0);
    quad.vertices[2].t_in = Vec2::new(30.0, 40.0);
    let mut per_point = RotoShape::new("per-point", RotoKind::Bezier, quad);
    per_point.feather = float(3.0);
    let star: Vec<(f64, f64)> = (0..10)
        .map(|i| {
            let a = f64::from(i) * std::f64::consts::PI / 5.0;
            let r = if i % 2 == 0 { 90.0 } else { 35.0 };
            (160.0 + r * a.cos(), 100.0 + r * a.sin())
        })
        .collect();
    let mut bspline = RotoShape::new("bspline", RotoKind::BSpline, poly(&star));
    bspline.feather = float(6.0);
    let mut open_path = poly(&star);
    open_path.closed = false;
    let mut open = RotoShape::new("open", RotoKind::BSpline, open_path);
    open.feather = float(6.0);
    // a pentagram in one stroke: its centre has winding 2 (nonzero: inside)
    let penta: Vec<(f64, f64)> = (0..5)
        .map(|i| {
            let a = f64::from(i * 2) * std::f64::consts::TAU / 5.0 - std::f64::consts::FRAC_PI_2;
            (160.0 + 90.0 * a.cos(), 105.0 + 90.0 * a.sin())
        })
        .collect();
    let mut pentagram = RotoShape::new("pentagram", RotoKind::Bezier, poly(&penta));
    pentagram.feather = float(4.0);
    // several shapes with every mode, inversion and opacity
    let modes = vec![
        with(rect(20.0, 20.0, 200.0, 150.0, 8.0), RotoMode::Subtract, false, 100.0),
        with(feathered.clone(), RotoMode::Add, false, 80.0),
        with(rect(100.0, 60.0, 300.0, 190.0, 0.0), RotoMode::Intersect, true, 60.0),
        with(per_point.clone(), RotoMode::Add, true, 50.0),
        with(rect(140.0, 90.0, 180.0, 120.0, 2.0), RotoMode::Subtract, false, 100.0),
    ];
    let replace = vec![ellipse.clone(), with(rect(120.0, 30.0, 200.0, 170.0, 5.0), RotoMode::Replace, false, 90.0)];
    // partly outside (left / top / bottom), wholly outside (inverted: the whole frame)
    let mut outside = RotoShape::new("outside", RotoKind::Bezier, MaskPath::ellipse(Vec2::new(10.0, 20.0), Vec2::new(120.0, 220.0)));
    outside.feather = float(15.0);
    let partly = vec![
        outside,
        with(rect(250.0, 150.0, 400.0, 260.0, 20.0), RotoMode::Add, false, 100.0),
        with(rect(-500.0, -400.0, -300.0, -200.0, 3.0), RotoMode::Add, true, 70.0),
    ];
    // degenerate: two points, collinear points, all points on one spot, a NaN point, a disabled
    // shape, an empty outline; and a hard square so the frame isn't empty
    let mut nan = MaskPath::rect(30.0, 30.0, 90.0, 90.0);
    nan.vertices[1].p.x = f64::NAN;
    let mut disabled = ellipse.clone();
    disabled.enabled = false;
    let spot = MaskPath { vertices: vec![MaskVertex { p: Vec2::new(50.0, 50.0), ..MaskVertex::default() }; 4], closed: true };
    let degenerate = vec![
        rect(200.0, 40.0, 280.0, 120.0, 0.0),
        RotoShape::new("two", RotoKind::Bezier, poly(&[(10.0, 10.0), (100.0, 150.0)])),
        RotoShape::new("line", RotoKind::Bezier, poly(&[(10.0, 180.0), (100.0, 180.0), (200.0, 180.0)])),
        with(RotoShape::new("spot", RotoKind::BSpline, spot), RotoMode::Subtract, false, 100.0),
        RotoShape::new("nan", RotoKind::Bezier, nan),
        disabled,
        RotoShape::new("none", RotoKind::Bezier, MaskPath::default()),
    ];
    vec![
        ("hard ellipse", Roto { shapes: vec![ellipse] }),
        ("feathered ellipse", Roto { shapes: vec![feathered] }),
        ("per-point feather", Roto { shapes: vec![per_point] }),
        ("closed b-spline", Roto { shapes: vec![bspline] }),
        ("open b-spline", Roto { shapes: vec![open] }),
        ("pentagram", Roto { shapes: vec![pentagram] }),
        ("modes", Roto { shapes: modes }),
        ("replace", Roto { shapes: replace }),
        ("partly outside", Roto { shapes: partly }),
        ("degenerate", Roto { shapes: degenerate }),
    ]
}

/// LO.5: the roto kernel's f32 coverage against the CPU rasteriser, read back directly (no
/// half-float slot in between): every pixel within 1e-3 (a quarter of 1/255; measured ≤ 7e-5 on
/// AMD RADV and Intel ANV). The winding and the band tests use the
/// CPU's own numbers, so only float rounding of the edge distance differs (see the report).
#[test]
fn roto_kernel_matches_the_cpu_rasteriser() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut worst = 0.0f32;
    // an odd size (partial tiles on the right and bottom), then the 6.2K frame (largest
    // coordinates, so the largest rounding)
    for (w, h, s) in [(317usize, 203usize, 1.0f32), (6240, 4160, 19.5)] {
        for (name, roto) in scenes() {
            let prepared = filmcraft_render::roto::prepare(&roto, Tick(0), w, h, s);
            let cpu = filmcraft_render::roto::rasterize(&prepared, w, h);
            let Some(gpu) = c.roto_coverage_readback(&prepared, (w as u32, h as u32)) else {
                assert!(c.roto_kernel().is_none(), "{name}: the kernel refused a small scene");
                eprintln!("the roto kernel can't run on this device; skipping");
                return;
            };
            assert_eq!(gpu.len(), cpu.len());
            let (mut max, mut over, mut sum) = (0.0f32, 0usize, 0.0f64);
            for (a, b) in cpu.iter().zip(&gpu) {
                let d = (a - b).abs();
                assert!(d.is_finite() && (0.0..=1.0).contains(b), "{name}: {b}");
                max = max.max(d);
                over += usize::from(d > 1e-4);
                sum += f64::from(d);
            }
            eprintln!("{w}x{h} {name}: max {max:.2e}, mean {:.2e}, pixels > 1e-4: {over}", sum / cpu.len() as f64);
            assert!(max <= 1e-3, "{w}x{h} {name}: max {max}");
            worst = worst.max(max);
        }
    }
    eprintln!("worst max abs difference: {worst:.2e}");
}

/// LO.5: a Roto node's pass runs in the kernel (Matte and Alpha match the CPU program within the
/// slot's half-float rounding); with the kernel's budget at 0 the CPU raster is uploaded instead.
#[test]
fn roto_pass_runs_in_the_kernel_with_cpu_fallback() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let all = scenes();
    let Some((_, roto)) = all.iter().find(|(n, _)| *n == "modes") else { return };
    let kernel = c.roto_kernel().is_some();
    for cap in [comp_roto::MAX_TILE_ENTRIES, 0] {
        c.comp_roto_max_entries = cap;
        for matte in [false, true] {
            // (the shapes scaled onto the small frame)
            let prepared = filmcraft_render::roto::prepare(roto, Tick(0), W, H, 0.3);
            let cov = Arc::new(Coverage::from_roto(prepared));
            let (gpu0, cpu0) = (c.comp_roto_gpu, c.comp_roto_cpu);
            let p = program(1, vec![Pass::Image { dst: 0, image: picture() }, Pass::Coverage { tex: 0, coverage: cov, matte }], 0);
            assert!(c.run_comp(&p, 0).is_some(), "ran on the GPU");
            if cap == 0 || !kernel {
                assert!(c.comp_roto_cpu > cpu0 && c.comp_roto_gpu == gpu0, "the CPU raster was uploaded");
            } else {
                assert!(c.comp_roto_gpu > gpu0 && c.comp_roto_cpu == cpu0, "the kernel ran");
            }
            let (p99, mean, max) = compare_sized(&mut c, p);
            eprintln!("roto pass cap {cap} matte={matte}: p99 {p99:.6} mean {mean:.7} max {max:.6}");
            assert!(max <= 2e-3 && mean <= 1e-4, "cap {cap} matte {matte}: max {max}, mean {mean}");
        }
    }
    c.comp_roto_max_entries = comp_roto::MAX_TILE_ENTRIES;
}

/// LO.5 timings (ignored; `--release -- --ignored --nocapture`; synthetic shapes): the CPU
/// rasteriser vs packing + the roto kernel at 6240 × 4160.
#[test]
#[ignore]
fn roto_kernel_timings() {
    use std::time::Instant;
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h, s) = (6240usize, 4160usize, 19.5f32);
    let runs = 5u32;
    for (name, roto) in scenes() {
        let prepared = filmcraft_render::roto::prepare(&roto, Tick(0), w, h, s);
        let _ = filmcraft_render::roto::rasterize(&prepared, w, h);
        let t0 = Instant::now();
        for _ in 0..runs {
            let _ = filmcraft_render::roto::rasterize(&prepared, w, h);
        }
        let cpu = t0.elapsed() / runs;
        let t0 = Instant::now();
        let packed = comp_roto::pack(&prepared, w, h, u64::MAX, comp_roto::MAX_TILE_ENTRIES).map(|d| d.index.len());
        let pack = t0.elapsed();
        let _ = c.gpu_roto_fill(&prepared, (w as u32, h as u32));
        let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
        let t0 = Instant::now();
        for _ in 0..runs {
            let _ = c.gpu_roto_fill(&prepared, (w as u32, h as u32));
            let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
        }
        let gpu = t0.elapsed() / runs;
        eprintln!("{w}x{h} {name}: CPU raster {cpu:?}; GPU pack + kernel {gpu:?} (pack alone {pack:?}, {packed:?} index words)");
    }
}
