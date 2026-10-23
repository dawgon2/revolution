//! Comp programs on the GPU (fork C1 step 7b) against the CPU oracle
//! ([`filmcraft_render::comp::execute_cpu`] through the frame plan). Run on each GPU with
//! `MESA_VK_DEVICE_SELECT` (AMD and Intel).

use super::*;
use filmcraft_geom::Affine;
use filmcraft_render::Blend;
use filmcraft_render::comp::{Pass, Program};
use filmcraft_render::gpufx::FxOp;
use filmcraft_render::image::Image;
use filmcraft_render::plan::{LayerFx, execute_cpu};

const W: usize = 96;
const H: usize = 54;

fn compositor() -> Option<GpuCompositor> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let desc = wgpu::DeviceDescriptor { required_features: adapter.features() & wanted_features(), ..Default::default() };
    let (d, q) = pollster::block_on(adapter.request_device(&desc)).ok()?;
    eprintln!("adapter: {}", adapter.get_info().name);
    Some(GpuCompositor::new(&d, &q))
}

/// A premultiplied picture with colour ramps, a bright square and alpha falling off to the right.
fn picture() -> Arc<Image> {
    let mut img = Image::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let a = 1.0 - 0.6 * x as f32 / W as f32;
            let bright = (30..60).contains(&x) && (15..40).contains(&y);
            let (r, g, b) = if bright { (0.9, 0.85, 0.8) } else { (x as f32 / W as f32, y as f32 / H as f32, 0.3) };
            let i = (y * W + x) * 4;
            img.px[i..i + 4].copy_from_slice(&[r * a, g * a, b * a, a]);
        }
    }
    Arc::new(img)
}

/// The comp as one layer, on the GPU (linear accumulator) and the CPU; the 99th percentile and
/// the mean of the per-channel difference.
fn compare(c: &mut GpuCompositor, p: Program) -> (f32, f32) {
    let plan = FramePlan::Layers { width: W, height: H, layers: vec![PlanLayer::comp(Arc::new(p), Affine::IDENTITY, 1.0, Blend::Normal)], output: None };
    let cpu = execute_cpu(&plan);
    let (gw, gh, gpu) = c.render_export_prepared(&plan, None).expect("export read-back");
    assert_eq!((gw as usize, gh as usize), (W, H));
    let mut d: Vec<f32> = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).collect();
    d.sort_by(f32::total_cmp);
    let p99 = d.get(d.len() * 99 / 100).copied().unwrap_or(f32::MAX);
    let mean = d.iter().sum::<f32>() / d.len().max(1) as f32;
    (p99, mean)
}

fn program(textures: u16, passes: Vec<Pass>, out: u16) -> Program {
    Program { size: (W, H), textures, passes, out }
}

#[test]
fn every_pass_kind_matches_the_cpu() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let img = || Pass::Image { dst: 0, image: picture() };
    let blue = Pass::Fill { dst: 1, color: [0.0, 0.1, 0.6, 1.0] };
    let turn = Affine::motion(filmcraft_geom::Vec2::new(48.0, 27.0), filmcraft_geom::Vec2::new(0.7, 0.7), 15.0, filmcraft_geom::Vec2::new(48.0, 27.0));
    let cases: Vec<(&str, Program)> = vec![
        ("image", program(1, vec![img()], 0)),
        ("fill", program(2, vec![blue.clone()], 1)),
        ("copy", program(2, vec![img(), Pass::Copy { src: 0, dst: 1 }], 1)),
        ("draw", program(2, vec![img(), Pass::Draw { src: 0, dst: 1, xform: Affine::translate(10.0, 6.0), hq: false }], 1)),
        ("draw in place", program(1, vec![img(), Pass::Draw { src: 0, dst: 0, xform: Affine::scale(0.5, 0.5), hq: false }], 0)),
        ("over", program(2, vec![img(), blue.clone(), Pass::Over { src: 0, dst: 1, mode: Blend::Normal, opacity: 0.8 }], 1)),
        ("over multiply", program(2, vec![img(), blue.clone(), Pass::Over { src: 0, dst: 1, mode: Blend::Multiply, opacity: 1.0 }], 1)),
        ("over itself", program(1, vec![img(), Pass::Over { src: 0, dst: 0, mode: Blend::Normal, opacity: 0.5 }], 0)),
        ("fx key", program(1, vec![img(), Pass::Fx { tex: 0, ops: vec![FxOp::LumaKey { th: 0.6, cut: 0.2 }] }], 0)),
        ("fx blur", program(1, vec![img(), Pass::Fx { tex: 0, ops: vec![FxOp::Gaussian { rx: vec![2], ry: vec![2], repeat: false }] }], 0)),
        ("missing input", program(2, vec![Pass::Over { src: 0, dst: 1, mode: Blend::Normal, opacity: 1.0 }], 1)),
        (
            "plan layers",
            program(
                1,
                vec![Pass::Layers {
                    dst: 0,
                    layers: vec![PlanLayer::new(
                        Arc::new(VideoFrame::rgba_f32(W as u32, H as u32, picture().px.clone())),
                        Affine::translate(6.0, 3.0),
                        0.9,
                        Blend::Normal,
                    )]
                    .into(),
                }],
                0,
            ),
        ),
    ];
    for (name, p) in cases {
        let (p99, mean) = compare(&mut c, p);
        eprintln!("{name}: p99 {p99:.5} mean {mean:.6}");
        assert!(p99 <= 4e-3 && mean <= 1e-3, "{name}: p99 {p99}, mean {mean}");
    }
    // a turned picture: its antialiased edges differ (as for timeline layers), so the bulk is
    // compared (the 90th percentile) and the mean stays small
    let (p90, mean) = {
        let p = program(2, vec![img(), Pass::Draw { src: 0, dst: 1, xform: turn, hq: false }], 1);
        let plan = FramePlan::Layers { width: W, height: H, layers: vec![PlanLayer::comp(Arc::new(p), Affine::IDENTITY, 1.0, Blend::Normal)], output: None };
        let cpu = execute_cpu(&plan);
        let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("read-back");
        let mut d: Vec<f32> = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).collect();
        d.sort_by(f32::total_cmp);
        (d.get(d.len() * 9 / 10).copied().unwrap_or(f32::MAX), d.iter().sum::<f32>() / d.len().max(1) as f32)
    };
    eprintln!("turned draw: p90 {p90:.5} mean {mean:.6}");
    assert!(p90 <= 4e-3 && mean <= 5e-3, "turned draw: p90 {p90}, mean {mean}");
    // the passes ran on the GPU (slots exist), not through the CPU fallback
    assert!(c.comp_sets.iter().flatten().any(|s| s.target.is_some()));
}

/// Comp A's shape (C1 acceptance): Media In → Key → Merge over a background → Color → Out, with a
/// Transform on the foreground; slots reused by liveness.
#[test]
fn a_keyed_merge_with_a_grade_matches_the_cpu() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let p = program(
        2,
        vec![
            Pass::Image { dst: 0, image: picture() },
            Pass::Fx { tex: 0, ops: vec![FxOp::LumaKey { th: 0.5, cut: 0.25 }] },
            Pass::Draw { src: 0, dst: 0, xform: Affine::translate(8.0, -4.0), hq: false },
            Pass::Fill { dst: 1, color: [0.2, 0.05, 0.05, 1.0] },
            Pass::Over { src: 0, dst: 1, mode: Blend::Normal, opacity: 1.0 },
            Pass::Fx { tex: 1, ops: vec![FxOp::BrightnessContrast { br: 10.0, co: 20.0 }] },
        ],
        1,
    );
    let (p99, mean) = compare(&mut c, p);
    assert!(p99 <= 4e-3 && mean <= 1e-3, "p99 {p99}, mean {mean}");
}

/// Hostile sizes: a comp wider than any texture can't run on the GPU; it renders through the CPU
/// fallback instead of failing (and a later normal comp still runs on the GPU).
#[test]
fn a_comp_too_wide_for_a_texture_falls_back_to_the_cpu() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    // an op the GPU effect stage can't run: the whole comp runs on the CPU
    let cpu_only = FxOp::Gamma { g: f32::NAN };
    assert!(!cpu_only.gpu_ok());
    let p = program(1, vec![Pass::Image { dst: 0, image: picture() }, Pass::Fx { tex: 0, ops: vec![cpu_only] }], 0);
    assert!(c.run_comp(&p, 0).is_none());
    let wide = Program { size: (100_000, 2), textures: 1, passes: vec![Pass::Fill { dst: 0, color: [0.5, 0.0, 0.0, 0.5] }], out: 0 };
    assert!(c.run_comp(&wide, 0).is_none());
    let plan = FramePlan::Layers {
        width: W,
        height: H,
        layers: vec![PlanLayer::comp(Arc::new(wide), Affine::scale(W as f64 / 100_000.0, H as f64 / 2.0), 1.0, Blend::Normal)],
        output: None,
    };
    let (_, _, px) = c.render_export_prepared(&plan, None).expect("read-back");
    let mid = (H / 2 * W + W / 2) * 4;
    assert!((px[mid] - 0.5).abs() < 0.02 && (px[mid + 3] - 0.5).abs() < 0.02, "{:?}", &px[mid..mid + 4]);
    let (p99, _) = compare(&mut c, program(1, vec![Pass::Image { dst: 0, image: picture() }], 0));
    assert!(p99 <= 4e-3);
    // a picture of another size than the comp: the CPU keeps it as it is, so the CPU runs the comp
    // (the GPU drew it transparent)
    let mut small = Image::new(W / 2, H / 2);
    small.px.iter_mut().for_each(|v| *v = 0.5);
    let odd = program(1, vec![Pass::Image { dst: 0, image: Arc::new(small) }], 0);
    assert!(c.run_comp(&odd, 0).is_none());
    let (p99, mean) = compare(&mut c, odd);
    assert!(p99 <= 4e-3 && mean <= 1e-3, "p99 {p99}, mean {mean}");
}

/// Two comp clips in one frame (comps A and B on V2 / V3): each keeps its own result.
#[test]
fn two_comps_in_one_frame_keep_their_own_pictures() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let red = program(1, vec![Pass::Fill { dst: 0, color: [0.8, 0.0, 0.0, 1.0] }], 0);
    let blue = program(1, vec![Pass::Fill { dst: 0, color: [0.0, 0.0, 0.6, 1.0] }], 0);
    let plan = FramePlan::Layers {
        width: W,
        height: H,
        layers: vec![
            PlanLayer::comp(Arc::new(red), Affine::IDENTITY, 1.0, Blend::Normal),
            // the second comp covers only the right half
            PlanLayer::comp(Arc::new(blue), Affine::translate(W as f64 / 2.0, 0.0), 1.0, Blend::Normal),
        ],
        output: None,
    };
    let cpu = execute_cpu(&plan);
    let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("read-back");
    for x in [4usize, W - 4] {
        let i = (H / 2 * W + x) * 4;
        for k in 0..4 {
            assert!((cpu.px[i + k] - gpu[i + k]).abs() < 2e-3, "x {x}: cpu {:?} gpu {:?}", &cpu.px[i..i + 4], &gpu[i..i + 4]);
        }
    }
}

/// A busy second comp (more uploads than the upload cache keeps) must not push the first comp's
/// result out of the cache before the frame draws it (it drew transparent).
#[test]
fn a_busy_later_comp_does_not_evict_an_earlier_result() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let red = program(1, vec![Pass::Fill { dst: 0, color: [0.8, 0.0, 0.0, 1.0] }], 0);
    // every fill uploads its own 1×1 colour: 40 new cache entries after the red comp's result
    let busy: Vec<Pass> = (0..40).map(|i| Pass::Fill { dst: 0, color: [0.0, 0.0, 0.3 + i as f32 / 100.0, 1.0] }).collect();
    let blue = program(1, busy, 0);
    let plan = FramePlan::Layers {
        width: W,
        height: H,
        layers: vec![
            PlanLayer::comp(Arc::new(red), Affine::IDENTITY, 1.0, Blend::Normal),
            PlanLayer::comp(Arc::new(blue), Affine::translate(W as f64 / 2.0, 0.0), 1.0, Blend::Normal),
        ],
        output: None,
    };
    let cpu = execute_cpu(&plan);
    let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("read-back");
    for x in [4usize, W - 4] {
        let i = (H / 2 * W + x) * 4;
        for k in 0..4 {
            assert!((cpu.px[i + k] - gpu[i + k]).abs() < 2e-3, "x {x}: cpu {:?} gpu {:?}", &cpu.px[i..i + 4], &gpu[i..i + 4]);
        }
    }
}

/// A comp with effects and a frame layer with effects of another size share one effect-pool frame:
/// both sizes stay pooled (the comp's slot draws ended the pool frame and dropped the frame-size
/// textures, the frame then dropped the comp-size ones, so playback reallocated them every frame).
#[test]
fn comp_and_frame_effects_keep_their_pooled_textures() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    if c.fx.is_none() {
        eprintln!("no effect stage; skipping");
        return;
    }
    let blur = || vec![FxOp::Gaussian { rx: vec![2], ry: vec![2], repeat: false }];
    let comp = Program {
        size: (W / 2, H / 2),
        textures: 1,
        passes: vec![Pass::Fill { dst: 0, color: [0.2, 0.5, 0.1, 1.0] }, Pass::Fx { tex: 0, ops: blur() }],
        out: 0,
    };
    let below = PlanLayer {
        frame: Arc::new(VideoFrame::rgba_f32(W as u32, H as u32, picture().px.clone())),
        matrix: Affine::IDENTITY,
        opacity: 1.0,
        blend: Blend::Normal,
        fx: Some(LayerFx { size: (W as u32, H as u32), decimation: 1, ops: blur() }.into()),
        comp: None,
    };
    let plan =
        FramePlan::Layers { width: W, height: H, layers: vec![below, PlanLayer::comp(Arc::new(comp), Affine::IDENTITY, 1.0, Blend::Normal)], output: None };
    let cpu = execute_cpu(&plan);
    for round in 0..2 {
        let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("read-back");
        let worst = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        assert!(worst < 5e-3, "round {round}: worst difference {worst}");
        let sizes = c.fx.as_ref().map_or(0, |f| f.pooled_sizes());
        assert_eq!(sizes, 2, "round {round}: comp-size and frame-size effect textures both pooled");
    }
}

/// The comp budget covers every slot set held: a second comp that would go above it runs on the
/// CPU (each comp used to have the whole budget, so eight comps could hold eight budgets) and the
/// picture is still right; sets a frame no longer uses and, after a comp-free stretch, all comp
/// textures are let go.
#[test]
fn comps_share_one_budget_and_their_textures_are_let_go() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let fill = |color: [f32; 4]| Arc::new(program(1, vec![Pass::Fill { dst: 0, color }], 0));
    // one comp's two slots (its picture and the scratch, counted whether drawn or not) fit; with
    // the first comp's drawn picture held, the second's don't (2.5 slots)
    c.comp_vram_budget = 5 * (W * H) as u64 * 4;
    let plan = FramePlan::Layers {
        width: W,
        height: H,
        layers: vec![
            PlanLayer::comp(fill([0.8, 0.0, 0.0, 1.0]), Affine::IDENTITY, 1.0, Blend::Normal),
            PlanLayer::comp(fill([0.0, 0.0, 0.6, 1.0]), Affine::translate(W as f64 / 2.0, 0.0), 1.0, Blend::Normal),
        ],
        output: None,
    };
    let cpu = execute_cpu(&plan);
    let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("read-back");
    let worst = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
    assert!(worst < 2e-3, "worst difference {worst}");
    let held = |c: &GpuCompositor| c.comp_sets.iter().map(|s| s.iter().filter(|x| x.target.is_some()).count()).collect::<Vec<_>>();
    assert_eq!(held(&c), vec![1, 0], "the second comp ran on the CPU");
    // a frame with one comp keeps one set
    c.comp_vram_budget = comp::COMP_VRAM_BUDGET;
    let one = FramePlan::Layers {
        width: W,
        height: H,
        layers: vec![PlanLayer::comp(fill([0.1, 0.6, 0.1, 1.0]), Affine::IDENTITY, 1.0, Blend::Normal)],
        output: None,
    };
    c.render_export_prepared(&one, None).expect("read-back");
    assert_eq!(c.comp_sets.len(), 1);
    // comp-free frames: kept for a while, then let go
    let plain = FramePlan::Layers { width: W, height: H, layers: Vec::new(), output: None };
    for _ in 1..comp::COMP_IDLE_FRAMES {
        c.render_export_prepared(&plain, None).expect("read-back");
    }
    assert_eq!(c.comp_sets.len(), 1, "kept until the idle stretch ends");
    c.render_export_prepared(&plain, None).expect("read-back");
    assert!(c.comp_sets.is_empty() && c.comp_slots.is_empty() && c.comp_output.is_none(), "comp textures let go");
}

/// A comp smaller than the frame draws into its own output / backdrop textures: the frame's keep
/// their size (they were reallocated twice per frame when comp and frame sizes alternated) and the
/// picture still matches the CPU.
#[test]
fn a_comp_of_another_size_leaves_the_frames_targets_alone() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (cw, ch) = (W / 2, H / 2);
    let comp = Program { size: (cw, ch), textures: 1, passes: vec![Pass::Fill { dst: 0, color: [0.2, 0.5, 0.1, 1.0] }], out: 0 };
    let pic = picture();
    let below = PlanLayer {
        frame: Arc::new(VideoFrame::rgba_f32(W as u32, H as u32, pic.px.clone())),
        matrix: Affine::IDENTITY,
        opacity: 1.0,
        blend: Blend::Normal,
        fx: None,
        comp: None,
    };
    let plan = FramePlan::Layers {
        width: W,
        height: H,
        // a blend-mode layer reads the destination: the frame's backdrop is used
        layers: vec![below, PlanLayer::comp(Arc::new(comp), Affine::translate(8.0, 4.0), 0.8, Blend::Screen)],
        output: None,
    };
    let cpu = execute_cpu(&plan);
    for round in 0..3 {
        let made = TARGETS_MADE.with(std::cell::Cell::get);
        let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("read-back");
        if round > 0 {
            assert_eq!(TARGETS_MADE.with(std::cell::Cell::get), made, "round {round} created render targets again");
        }
        let worst = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        assert!(worst < 3e-3, "worst difference {worst}");
        let size = |s: &Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>| s.as_ref().map(|t| t.2);
        assert_eq!(size(&c.output), Some((W as u32, H as u32)), "the frame's output");
        assert_eq!(size(&c.backdrop), Some((W as u32, H as u32)), "the frame's backdrop");
    }
}

/// C3.5b: Roto (Alpha and Matte, compiled from a comp) and Paint passes (every tool, a reveal
/// source) on the GPU against the CPU oracle; both run on the GPU, not by falling back.
#[test]
fn roto_and_paint_nodes_match_the_cpu() {
    use filmcraft_project::comp::{Comp, CompKind, CompNode, RotoOutput};
    use filmcraft_project::dag::{Edge, Port};
    use filmcraft_project::paint::{Brush, Paint, PaintPoint, PaintStroke, PaintTool, StrokeDuration};
    use filmcraft_project::roto::{Roto, RotoKind, RotoMode, RotoShape};
    use filmcraft_project::{MaskPath, Param, ParamValue};
    use filmcraft_render::comp::{CompCtx, NoSources, compile};
    use filmcraft_time::{FrameRate, Tick};
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut cases: Vec<(&str, Program)> = Vec::new();
    for output in RotoOutput::ALL {
        let ellipse = MaskPath::ellipse(filmcraft_geom::Vec2::new(48.0, 27.0), filmcraft_geom::Vec2::new(30.0, 18.0));
        let mut shape = RotoShape::new("s", RotoKind::BSpline, ellipse);
        shape.feather = Param::new(ParamValue::Float(6.0));
        let mut cut = RotoShape::new("hole", RotoKind::Bezier, MaskPath::rect(40.0, 20.0, 56.0, 34.0));
        cut.mode = RotoMode::Subtract;
        let mut comp = Comp::new(W as u32, H as u32, FrameRate::FPS_24, Tick(1000));
        comp.nodes.push(CompNode::new(2, CompKind::Gradient));
        comp.nodes.push(CompNode::new(3, CompKind::Roto { roto: Roto { shapes: vec![shape, cut] }, output }));
        comp.edges = vec![Edge::new(Port::new(2, 0), Port::new(3, 0)), Edge::new(Port::new(3, 0), Port::new(1, 0))];
        let ctx = CompCtx { t: Tick(0), size: (W, H), project: None, high_quality: false };
        let p = compile(&comp, ctx, &NoSources).expect("compiles");
        cases.push((if output == RotoOutput::Alpha { "roto alpha" } else { "roto matte" }, p));
    }
    let pts = |y: f64| vec![PaintPoint::new(8.0, y), PaintPoint { x: 50.0, y: y + 6.0, pressure: 0.5 }, PaintPoint::new(88.0, y)];
    let brush = Brush { size: 9.0, hardness: 0.4, ..Brush::default() };
    let strokes = vec![
        PaintStroke { color: [1.0, 0.2, 0.1, 0.8], ..PaintStroke::new(PaintTool::Color, pts(10.0), brush) },
        PaintStroke { clone_offset: filmcraft_geom::Vec2::new(-12.0, 5.0), ..PaintStroke::new(PaintTool::Clone, pts(22.0), brush) },
        PaintStroke::new(PaintTool::Reveal, pts(34.0), brush),
        PaintStroke {
            duration: StrokeDuration::Hold { from: Tick(0), to: None },
            ..PaintStroke::new(PaintTool::Erase, pts(12.0), Brush { size: 5.0, ..brush })
        },
    ];
    let paint = Arc::new(Paint { strokes });
    let frame = FrameRate::FPS_24.frame_duration();
    let blue = Pass::Fill { dst: 1, color: [0.0, 0.1, 0.6, 1.0] };
    let pass = |reveal| Pass::Paint { tex: 0, reveal, clones: Vec::new(), paint: Arc::clone(&paint), at: Tick(0), frame, px_scale: 1.0 };
    cases.push(("paint", program(2, vec![Pass::Image { dst: 0, image: picture() }, blue.clone(), pass(Some(1))], 0)));
    cases.push(("paint without reveal", program(1, vec![Pass::Image { dst: 0, image: picture() }, pass(None)], 0)));
    for (name, p) in cases {
        assert!(c.run_comp(&p, 0).is_some(), "{name}: ran on the GPU");
        let (p99, mean) = compare(&mut c, p);
        assert!(p99 <= 4e-3 && mean <= 1e-3, "{name}: p99 {p99}, mean {mean}");
    }
}

/// Tracker / Planar Tracker nodes (C3.5a): an affine track (match move) compiles to the ordinary
/// Draw + Over passes and matches the CPU on the GPU; a perspective corner pin is a projective
/// Warp, which the GPU hands back to the CPU (same pixels through the frame plan).
#[test]
fn tracker_nodes_run_on_the_gpu() {
    use filmcraft_geom::Vec2;
    use filmcraft_project::comp::{Comp, CompKind, CompNode, TrackOperation, TrackerPoint};
    use filmcraft_project::dag::{Edge, Port};
    use filmcraft_project::{Keyframe, Param, ParamValue, TrackMethod};
    use filmcraft_render::comp::{CompCtx, NoSources, compile};
    use filmcraft_time::{FrameRate, Tick};
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let rate = FrameRate::FPS_24;
    let track = |at: &dyn Fn(i64) -> Vec2| {
        let mut p = Param::new(ParamValue::Vec2(at(0)));
        p.keyframes = (0..10).map(|f| Keyframe::new(rate.tick_of(f), ParamValue::Vec2(at(f)))).collect();
        p
    };
    let solid = |id: u64, col: [f32; 4]| {
        let mut n = CompNode::new(id, CompKind::Solid);
        n.params.insert("color".into(), Param::new(ParamValue::Color(col)));
        n
    };
    let build = |kind: CompKind| {
        let mut comp = Comp::new(W as u32, H as u32, rate, rate.tick_of(10));
        let mut grad = CompNode::new(4, CompKind::Gradient);
        grad.params.insert("shape".into(), Param::new(ParamValue::Choice(1)));
        comp.nodes.extend([solid(2, [0.1, 0.2, 0.6, 1.0]), grad, CompNode::new(3, kind)]);
        comp.edges = [(2, 3, 0), (4, 3, 1), (3, 1, 0)].iter().map(|&(f, t, p)| Edge::new(Port::new(f, 0), Port::new(t, p))).collect();
        let ctx = CompCtx { t: rate.tick_of(6), size: (W, H), project: None, high_quality: false };
        compile(&comp, ctx, &NoSources).expect("compiles")
    };
    let point = TrackerPoint { track: track(&|f| Vec2::new(30.0 + 2.0 * f as f64, 20.0 + f as f64)), ..TrackerPoint::at("", Vec2::ZERO) };
    let mm = build(CompKind::Tracker {
        points: vec![point],
        operation: TrackOperation::MatchMove,
        smooth_radius: 0,
        reference: Tick::ZERO,
        method: TrackMethod::Position,
    });
    assert!(mm.passes.iter().any(|p| matches!(p, Pass::Draw { .. })) && !mm.passes.iter().any(|p| matches!(p, Pass::Warp { .. })));
    assert!(c.run_comp(&mm, 0).is_some(), "an affine track runs on the GPU");
    // (whole-pixel motion: antialiased edges differ between the two, as for timeline layers)
    let (p99, mean) = compare(&mut c, mm);
    assert!(p99 <= 4e-3 && mean <= 1e-3, "match move: p99 {p99}, mean {mean}");

    let plane = |f: i64| {
        let k = f as f64;
        [Vec2::new(20.0 + k, 8.0), Vec2::new(70.0 + k, 10.0 + 0.3 * k), Vec2::new(74.0 + k, 44.0), Vec2::new(16.0 + k, 40.0 - 0.2 * k)]
    };
    let pin = build(CompKind::PlanarTracker {
        region: filmcraft_project::MaskPath::default(),
        quad: plane(0),
        reference: Tick::ZERO,
        corners: (0..4).map(|i| track(&|f| plane(f)[i])).collect(),
        confidence: Vec::new(),
        operation: TrackOperation::CornerPin,
    });
    assert!(pin.passes.iter().any(|p| matches!(p, Pass::Warp { .. })));
    let ran = c.comp_warp_gpu;
    assert!(c.run_comp(&pin, 0).is_some(), "a projective warp runs on the GPU (C3.8)");
    assert_eq!(c.comp_warp_gpu, ran + 1, "in the warp kernel");
    let (p99, mean) = compare(&mut c, pin);
    assert!(p99 <= 4e-3 && mean <= 1e-3, "corner pin: p99 {p99}, mean {mean}");
}

/// A deterministic pseudo-random stream in 0..1.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.next()
    }
}

/// A `w × h` premultiplied picture with ramps and a little noise (clone sources must differ).
fn sized_picture(w: usize, h: usize) -> Arc<Image> {
    let mut img = Image::new(w, h);
    for (i, px) in img.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
        let a = 1.0 - 0.5 * x;
        let n = ((i.wrapping_mul(2654435761) >> 7) % 97) as f32 / 970.0;
        *px = [(x + n) * a, y * a, (0.3 + n) * a, a];
    }
    Arc::new(img)
}

/// `n` strokes of every tool all over (and past the edges of) a `w × h` frame: soft and hard
/// brushes, pressure, colours with alpha, clone offsets in and out of the frame.
fn random_strokes(n: usize, w: f64, h: f64, seed: u64) -> Vec<filmcraft_project::paint::PaintStroke> {
    use filmcraft_project::paint::{Brush, PaintPoint, PaintStroke, PaintTool};
    let mut r = Lcg(seed);
    (0..n)
        .map(|i| {
            let tool = [PaintTool::Color, PaintTool::Clone, PaintTool::Reveal, PaintTool::Erase][i % 4];
            let pts = (0..2 + (r.next() * 4.0) as usize)
                .map(|_| PaintPoint { x: r.range(-0.15 * w, 1.15 * w), y: r.range(-0.15 * h, 1.15 * h), pressure: r.range(0.2, 1.0) })
                .collect();
            let brush = Brush { size: r.range(2.0, 0.25 * h), hardness: r.range(0.0, 1.0), opacity: r.range(0.2, 1.0), spacing: r.range(0.03, 0.4) };
            let far = r.next() < 0.15;
            let off = if far {
                filmcraft_geom::Vec2::new(r.range(-2.0, 2.0) * w * 3.0, 1e9)
            } else {
                filmcraft_geom::Vec2::new(r.range(-0.3, 0.3) * w, r.range(-0.3, 0.3) * h)
            };
            PaintStroke {
                color: [r.next() as f32 * 1.5, r.next() as f32, r.next() as f32, r.range(0.3, 1.0) as f32],
                clone_offset: off,
                ..PaintStroke::new(tool, pts, brush)
            }
        })
        .collect()
}

/// A comp-sized comparison: p99, mean and max of the per-channel difference.
fn compare_sized(c: &mut GpuCompositor, p: Program) -> (f32, f32, f32) {
    let (w, h) = p.size;
    let plan = FramePlan::Layers { width: w, height: h, layers: vec![PlanLayer::comp(Arc::new(p), Affine::IDENTITY, 1.0, Blend::Normal)], output: None };
    let cpu = execute_cpu(&plan);
    let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("export read-back");
    let mut d: Vec<f32> = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).collect();
    d.sort_by(f32::total_cmp);
    let p99 = d.get(d.len() * 99 / 100).copied().unwrap_or(f32::MAX);
    let mean = d.iter().sum::<f32>() / d.len().max(1) as f32;
    (p99, mean, d.last().copied().unwrap_or(0.0))
}

/// C3.6: the dab kernel against `paint::apply` (through the CPU comp oracle): every tool, many
/// strokes over and past the frame edges, clone offsets out of the frame, with and without a
/// reveal source, at px_scale 1 and 2; it runs in the kernel, not the read-back fallback.
#[test]
fn paint_kernel_matches_the_cpu() {
    use filmcraft_project::paint::Paint;
    use filmcraft_time::{FrameRate, Tick};
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let frame = FrameRate::FPS_24.frame_duration();
    for (name, (w, h), n, px_scale, reveal) in [
        ("small", (96, 54), 12, 1.0f32, true),
        ("many", (203, 117), 160, 1.0, true),
        ("no reveal", (203, 117), 60, 1.0, false),
        ("scaled", (300, 170), 40, 2.0, true),
    ] {
        let strokes = random_strokes(n, w as f64 / f64::from(px_scale), h as f64 / f64::from(px_scale), w as u64 * 31 + n as u64);
        let paint = Arc::new(Paint { strokes });
        let mut passes = vec![Pass::Image { dst: 0, image: sized_picture(w, h) }];
        if reveal {
            passes.push(Pass::Fill { dst: 1, color: [0.0, 0.1, 0.6, 1.0] });
        }
        passes.push(Pass::Paint { tex: 0, reveal: reveal.then_some(1), clones: Vec::new(), paint, at: Tick(0), frame, px_scale });
        let p = Program { size: (w, h), textures: 2, passes, out: 0 };
        let before = (c.comp_paint_gpu, c.comp_paint_cpu);
        let (p99, mean, max) = compare_sized(&mut c, p);
        eprintln!("paint {name}: p99 {p99:.5} mean {mean:.6} max {max:.5}");
        assert_eq!((c.comp_paint_gpu, c.comp_paint_cpu), (before.0 + 1, before.1), "{name}: ran in the kernel");
        assert!(p99 <= 4e-3 && mean <= 1e-3, "{name}: p99 {p99}, mean {mean}");
    }
}

/// C3.6: past the kernel's limits (here a lowered entry cap) the pass falls back to the read-back
/// CPU paint with the same result; hostile stroke values never panic on either path.
#[test]
fn paint_kernel_falls_back_and_survives_hostile_strokes() {
    use filmcraft_project::paint::{Brush, Paint, PaintPoint, PaintStroke, PaintTool};
    use filmcraft_time::{FrameRate, Tick};
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let frame = FrameRate::FPS_24.frame_duration();
    let nan = PaintStroke {
        tool: PaintTool::Color,
        points: vec![
            PaintPoint { x: f64::NAN, y: 1.0, pressure: f64::NAN },
            PaintPoint { x: 1e300, y: -1e300, pressure: 1e300 },
            PaintPoint { x: 2.0, y: 2.0, pressure: -1.0 },
            PaintPoint::new(40.0, 30.0),
        ],
        brush: Brush { size: f64::INFINITY, hardness: f64::NAN, opacity: -3.0, spacing: f64::NAN },
        color: [f32::NAN, f32::INFINITY, -1e38, f32::NAN],
        clone_offset: filmcraft_geom::Vec2::new(f64::NAN, f64::INFINITY),
        ..PaintStroke::default()
    };
    let long = PaintStroke::new(
        PaintTool::Erase,
        vec![PaintPoint::new(-1e7, 3.0), PaintPoint::new(1e7, 3.0), PaintPoint::new(-1e7, 20.0)],
        Brush { size: 3.0, hardness: 1.0, opacity: 1.0, spacing: 0.0 },
    );
    let mut far = PaintStroke::new(PaintTool::Clone, vec![PaintPoint::new(10.0, 10.0), PaintPoint::new(80.0, 40.0)], Brush { size: 12.0, ..Brush::default() });
    far.clone_offset = filmcraft_geom::Vec2::new(-1e300, 1e300);
    let mut strokes = vec![nan, long, far];
    strokes.extend(random_strokes(30, W as f64, H as f64, 7));
    let paint = Arc::new(Paint { strokes });
    for px_scale in [1.0f32, f32::NAN, 1e30, -1.0] {
        for cap in [comp_paint::MAX_TILE_ENTRIES, 10] {
            c.comp_paint_max_entries = cap;
            let passes = vec![
                Pass::Image { dst: 0, image: picture() },
                Pass::Fill { dst: 1, color: [0.3, 0.1, 0.0, 1.0] },
                Pass::Paint { tex: 0, reveal: Some(1), clones: Vec::new(), paint: Arc::clone(&paint), at: Tick(0), frame, px_scale },
            ];
            let before = c.comp_paint_cpu;
            let (p99, mean, _) = compare_sized(&mut c, program(2, passes, 0));
            assert!(p99 <= 4e-3 && mean <= 1e-3, "scale {px_scale} cap {cap}: p99 {p99}, mean {mean}");
            if cap == 10 && px_scale == 1.0 {
                assert_eq!(c.comp_paint_cpu, before + 1, "the read-back fallback ran");
            }
        }
    }
    c.comp_paint_max_entries = comp_paint::MAX_TILE_ENTRIES;
}

/// C3.6: Roto coverage is uploaded as f32 (no 8-bit matte): Alpha and Matte match the CPU within
/// the slot's half-float rounding, and a coverage of the wrong length is handled as on the CPU.
#[test]
fn roto_coverage_matches_without_8_bit_tolerance() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (W, H);
    // a smooth ramp with every 1/255 step between: an 8-bit matte errs by up to 2e-3 here
    let cov: Arc<[f32]> = (0..w * h).map(|i| ((i % w) as f32 / (w - 1) as f32).powf(1.7) * 0.999 + 0.0005).collect();
    for matte in [false, true] {
        let p = program(1, vec![Pass::Image { dst: 0, image: picture() }, Pass::Coverage { tex: 0, coverage: Arc::clone(&cov), matte }], 0);
        assert!(c.run_comp(&p, 0).is_some(), "ran on the GPU");
        let (p99, mean, max) = compare_sized(&mut c, p);
        eprintln!("coverage matte={matte}: p99 {p99:.6} mean {mean:.7} max {max:.6}");
        // half-float rounding of values ≤ 1: ≤ 2^-11 relative
        assert!(max <= 5e-4 && mean <= 1e-4, "matte {matte}: max {max}, mean {mean}");
    }
    // a wrong length: alpha leaves the picture, matte is black
    let short: Arc<[f32]> = vec![0.5f32; 7].into();
    for matte in [false, true] {
        let p = program(1, vec![Pass::Image { dst: 0, image: picture() }, Pass::Coverage { tex: 0, coverage: Arc::clone(&short), matte }], 0);
        let (_, _, max) = compare_sized(&mut c, p);
        assert!(max <= 2e-3, "short coverage, matte {matte}: {max}");
    }
}

/// C3.6 timings (ignored; `--ignored --nocapture`): CPU paint vs the dab kernel vs the read-back
/// fallback, and the CPU roto rasteriser vs the coverage upload + draw, at 1080p and 6240×4160.
#[test]
#[ignore]
fn paint_and_roto_timings() {
    use filmcraft_project::paint::{Brush, Paint, PaintPoint, PaintStroke, PaintTool};
    use filmcraft_project::roto::{Roto, RotoKind, RotoShape};
    use filmcraft_project::{MaskPath, Param, ParamValue};
    use filmcraft_time::{FrameRate, Tick};
    use std::time::Instant;
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let frame = FrameRate::FPS_24.frame_duration();
    // 500 small strokes in 1080p frame pixels (the CPU paint test's set) and 10 big soft clones
    let mut strokes: Vec<PaintStroke> = (0..500)
        .map(|i| {
            let (x, y) = (f64::from(i % 50) * 38.0 + 10.0, f64::from(i / 50) * 100.0 + 20.0);
            let tool = [PaintTool::Color, PaintTool::Erase, PaintTool::Clone][i as usize % 3];
            let pts = vec![PaintPoint::new(x, y), PaintPoint::new(x + 20.0, y + 10.0)];
            let mut s = PaintStroke::new(tool, pts, Brush { size: 8.0, hardness: 0.7, ..Brush::default() });
            s.clone_offset = filmcraft_geom::Vec2::new(30.0, -20.0);
            s
        })
        .collect();
    for k in 0..10 {
        let y = 100.0 + f64::from(k) * 90.0;
        let pts = vec![PaintPoint::new(100.0, y), PaintPoint::new(1800.0, y + 40.0)];
        let mut s = PaintStroke::new(PaintTool::Clone, pts, Brush { size: 120.0, hardness: 0.2, ..Brush::default() });
        s.clone_offset = filmcraft_geom::Vec2::new(-60.0, 35.0);
        strokes.push(s);
    }
    let paint = Arc::new(Paint { strokes });
    let ellipse = MaskPath::ellipse(filmcraft_geom::Vec2::new(960.0, 540.0), filmcraft_geom::Vec2::new(600.0, 400.0));
    let mut shape = RotoShape::new("s", RotoKind::BSpline, ellipse);
    shape.feather = Param::new(ParamValue::Float(40.0));
    let roto = Roto { shapes: vec![shape] };
    let timed = |c: &mut GpuCompositor, p: &Program| {
        let _ = c.run_comp(p, 0);
        let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
        let t = Instant::now();
        for _ in 0..3 {
            let _ = c.run_comp(p, 0);
            let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
        }
        t.elapsed() / 3
    };
    for (w, h) in [(1920usize, 1080usize), (6240, 4160)] {
        let s = w as f32 / 1920.0;
        let img = sized_picture(w, h);
        let t0 = Instant::now();
        let cpu = filmcraft_render::paint::apply(&paint, Tick(0), frame, s, &img.px, &[], None, w, h);
        let cpu_paint = t0.elapsed();
        drop(cpu);
        let base = Program { size: (w, h), textures: 1, passes: vec![Pass::Image { dst: 0, image: Arc::clone(&img) }], out: 0 };
        let mut with_paint = base.clone();
        with_paint.passes.push(Pass::Paint { tex: 0, reveal: None, clones: Vec::new(), paint: Arc::clone(&paint), at: Tick(0), frame, px_scale: s });
        let base_t = timed(&mut c, &base);
        let t0 = Instant::now();
        let planned = filmcraft_render::paint::plan(&paint, Tick(0), frame, s, w, h, false, &[]);
        let plan_t = t0.elapsed();
        let t0 = Instant::now();
        let packed = comp_paint::pack(&planned, w, h, u64::MAX, comp_paint::MAX_TILE_ENTRIES).map(|d| d.index.len() / 4);
        let pack_t = t0.elapsed();
        let kernel = timed(&mut c, &with_paint).saturating_sub(base_t);
        c.comp_paint_max_entries = 0;
        let fallback = timed(&mut c, &with_paint).saturating_sub(base_t);
        c.comp_paint_max_entries = comp_paint::MAX_TILE_ENTRIES;
        let prepared = filmcraft_render::roto::prepare(&roto, Tick(0), w, h, s);
        let t0 = Instant::now();
        let cov = filmcraft_render::roto::rasterize(&prepared, w, h);
        let raster = t0.elapsed();
        let mut with_cov = base.clone();
        with_cov.passes.push(Pass::Coverage { tex: 0, coverage: cov.into(), matte: false });
        let cover = timed(&mut c, &with_cov).saturating_sub(base_t);
        eprintln!(
            "{w}x{h}: paint CPU {cpu_paint:?}, kernel pass {kernel:?} (plan {plan_t:?}, pack {pack_t:?}, {packed:?} entries), read-back fallback {fallback:?}; \
             roto CPU raster {raster:?}, coverage upload + draw {cover:?}"
        );
    }
}

/// Comp set `set`'s slot `i` read back as f32 (non-finite values kept).
fn read_set_slot(c: &GpuCompositor, set: usize, i: usize) -> Option<Vec<f32>> {
    let (tex, _, size) = c.comp_sets.get(set)?.get(i)?.target.as_ref()?;
    let mut enc = c.device.create_command_encoder(&Default::default());
    let bands = record_readback(&c.device, &mut enc, tex, *size, 8);
    c.queue.submit([enc.finish()]);
    let bytes = map_readback(&c.device, &bands?, *size, 8)?;
    Some(bytes.as_chunks::<2>().0.iter().map(|b| f16_to_f32(u16::from_le_bytes(*b))).collect())
}

/// C3.6 review: like `paint::apply`, the kernel turns non-finite input channels into 0 over the
/// whole frame, not only in the 16 × 16 tiles a dab touches (that left tile-shaped seams in
/// blown-out f16 highlights).
#[test]
fn paint_kernel_cleans_non_finite_pixels_everywhere() {
    use filmcraft_project::paint::{Brush, Paint, PaintPoint, PaintStroke, PaintTool};
    use filmcraft_time::{FrameRate, Tick};
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let frame = FrameRate::FPS_24.frame_duration();
    let mut img = (*picture()).clone();
    // (2, 2): in the dab's tile, outside the dab; (80, 40): a tile no dab touches
    let bad = [(2usize, 2usize, f32::NAN), (80, 40, f32::INFINITY), (81, 40, 1e6)];
    for (x, y, v) in bad {
        let i = (y * W + x) * 4;
        img.px[i..i + 4].copy_from_slice(&[v, 0.5, v, 1.0]);
    }
    let img = Arc::new(img);
    let image_only = program(1, vec![Pass::Image { dst: 0, image: Arc::clone(&img) }], 0);
    assert!(c.run_comp(&image_only, 0).is_some(), "ran on the GPU");
    let before = read_set_slot(&c, 0, 0).expect("read back");
    let kept = bad.iter().filter(|(x, y, _)| !before[(y * W + x) * 4].is_finite()).count();
    if kept == 0 {
        eprintln!("this device's layer draw already cleans non-finite values; nothing to check");
        return;
    }
    let stroke = PaintStroke::new(PaintTool::Color, vec![PaintPoint::new(10.0, 10.0), PaintPoint::new(12.0, 11.0)], Brush { size: 4.0, ..Brush::default() });
    let paint = Arc::new(Paint { strokes: vec![stroke] });
    let p = program(
        1,
        vec![Pass::Image { dst: 0, image: img }, Pass::Paint { tex: 0, reveal: None, clones: Vec::new(), paint, at: Tick(0), frame, px_scale: 1.0 }],
        0,
    );
    let gpu_before = c.comp_paint_gpu;
    assert!(c.run_comp(&p, 0).is_some(), "ran on the GPU");
    assert_eq!(c.comp_paint_gpu, gpu_before + 1, "ran in the kernel");
    let out = read_set_slot(&c, 0, usize::from(p.out)).expect("read back");
    assert!(out.iter().all(|v| v.is_finite()), "no non-finite value left");
    for (x, y, _) in bad {
        let i = (y * W + x) * 4;
        if !before[i].is_finite() {
            assert_eq!(out[i], 0.0, "({x}, {y}) cleaned as on the CPU");
        }
    }
    // the CPU oracle agrees
    let cpu = filmcraft_render::comp::execute_cpu(&p);
    assert!(cpu.px.iter().all(|v| v.is_finite()));
}

/// The homography moving the `w × h` frame's corners (clockwise from the top left) onto `quad`.
fn pin(w: usize, h: usize, quad: [(f64, f64); 4]) -> filmcraft_render::track::Homography {
    use filmcraft_geom::Vec2;
    let dst: Vec<Vec2> = quad.iter().map(|&(x, y)| Vec2::new(x, y)).collect();
    filmcraft_render::track::fit_homography(&filmcraft_render::comp::track::frame_quad(w as f64, h as f64), &dst).expect("a fit")
}

/// C3.8: `Pass::Warp` in the kernel against `warp_projective` (through the CPU comp oracle):
/// strong tilts, a near-degenerate quad, a quad partly outside the frame, the horizon inside the
/// frame (points behind the projection plane), minified maps (halved levels), in place, merged
/// over a background, a missing source; all in the kernel, not the CPU fallback.
#[test]
fn warp_kernel_matches_the_cpu() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (320usize, 180usize);
    let (fw, fh) = (w as f64, h as f64);
    let img = sized_picture(w, h);
    let image = || Pass::Image { dst: 0, image: Arc::clone(&img) };
    let bg = Pass::Fill { dst: 1, color: [0.05, 0.1, 0.4, 1.0] };
    let tilt = pin(w, h, [(40.0, 20.0), (300.0, 60.0), (290.0, 120.0), (50.0, 170.0)]);
    let horizon = filmcraft_render::track::Homography { m: [1.0, 0.2, 0.0, 0.05, 1.0, 0.0, 0.004, 0.001, 0.4] };
    let cases: Vec<(&str, filmcraft_render::track::Homography)> = vec![
        ("strong tilt", tilt),
        ("strong perspective", pin(w, h, [(10.0, 70.0), (310.0, 5.0), (315.0, 178.0), (5.0, 110.0)])),
        ("near-degenerate", pin(w, h, [(100.0, 80.0), (220.0, 80.0), (220.0, 83.0), (100.0, 82.5)])),
        ("partly outside", pin(w, h, [(-120.0, -40.0), (250.0, 20.0), (380.0, 230.0), (-30.0, 160.0)])),
        ("horizon in frame", horizon),
        ("minified tilt", pin(w, h, [(100.0, 60.0), (170.0, 70.0), (165.0, 100.0), (110.0, 105.0)])),
        ("minified far", pin(w, h, [(150.0, 80.0), (162.0, 81.0), (161.0, 88.0), (151.0, 89.0)])),
        ("magnified", pin(w, h, [(-fw, -fh), (2.0 * fw, -0.5 * fh), (2.2 * fw, 2.0 * fh), (-0.8 * fw, 2.1 * fh)])),
    ];
    let mut kernels = 0u64;
    for (name, hm) in cases {
        let progs = [
            ("", Program { size: (w, h), textures: 2, passes: vec![image(), Pass::Warp { src: 0, dst: 1, h: hm }], out: 1 }),
            (" in place", Program { size: (w, h), textures: 1, passes: vec![image(), Pass::Warp { src: 0, dst: 0, h: hm }], out: 0 }),
            (
                " over a background",
                Program {
                    size: (w, h),
                    textures: 3,
                    passes: vec![image(), bg.clone(), Pass::Warp { src: 0, dst: 2, h: hm }, Pass::Over { src: 2, dst: 1, mode: Blend::Normal, opacity: 1.0 }],
                    out: 1,
                },
            ),
        ];
        for (how, p) in progs {
            let before = c.comp_warp_gpu;
            assert!(c.run_comp(&p, 0).is_some(), "{name}{how}: runs on the GPU");
            assert_eq!(c.comp_warp_gpu, before + 1, "{name}{how}: in the kernel");
            kernels += 1;
            let (p99, mean, max) = compare_sized(&mut c, p);
            eprintln!("{name}{how}: p99 {p99:.5} mean {mean:.6} max {max:.4}");
            assert!(p99 <= 4e-3 && mean <= 1e-3, "{name}{how}: p99 {p99}, mean {mean}");
        }
    }
    assert!(kernels > 0);
    // a missing source: transparent, as on the CPU
    let p = Program { size: (w, h), textures: 2, passes: vec![Pass::Warp { src: 0, dst: 1, h: tilt }], out: 1 };
    let (p99, mean, max) = compare_sized(&mut c, p);
    assert!(max == 0.0 && p99 == 0.0 && mean == 0.0, "missing source: max {max}");
}

/// C3.8: singular and non-finite homographies draw transparency on the GPU exactly as the CPU
/// does (no crash, no device error), and the next comp still runs in the kernel.
#[test]
fn a_singular_warp_draws_nothing_and_the_kernel_survives() {
    use filmcraft_render::track::Homography;
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let img = picture();
    let hostile = [
        Homography { m: [0.0; 9] },
        Homography { m: [1.0, 2.0, 3.0, 2.0, 4.0, 6.0, 0.0, 0.0, 1.0] },
        Homography { m: [f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] },
        Homography { m: [f64::INFINITY, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] },
        Homography { m: [1e300, 0.0, 0.0, 0.0, 1e300, 0.0, 0.0, 0.0, 1e-300] },
        Homography { m: [1e-200, 0.0, 0.0, 0.0, 1e-200, 0.0, 0.0, 0.0, 1.0] },
        // everything behind the projection plane (w < 0 over the whole frame)
        Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -1.0] },
        // a map that sends the frame to infinity along a line through it
        Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0 / 48.0, 0.0, -1.0] },
    ];
    for hm in hostile {
        let p = program(2, vec![Pass::Image { dst: 0, image: Arc::clone(&img) }, Pass::Warp { src: 0, dst: 1, h: hm }], 1);
        assert!(c.run_comp(&p, 0).is_some(), "{hm:?}: no CPU fallback needed");
        let (p99, mean) = compare(&mut c, p);
        assert!(p99 <= 4e-3 && mean <= 1e-3, "{hm:?}: p99 {p99}, mean {mean}");
    }
    let ok = program(
        2,
        vec![Pass::Image { dst: 0, image: img }, Pass::Warp { src: 0, dst: 1, h: pin(W, H, [(5.0, 4.0), (90.0, 10.0), (80.0, 50.0), (10.0, 45.0)]) }],
        1,
    );
    let before = c.comp_warp_gpu;
    assert!(c.run_comp(&ok, 0).is_some());
    assert_eq!(c.comp_warp_gpu, before + 1);
    let (p99, mean) = compare(&mut c, ok);
    assert!(p99 <= 4e-3 && mean <= 1e-3, "after hostile maps: p99 {p99}, mean {mean}");
}

/// C3.8 timings: a corner pin at 1080p and 6240 × 4160, CPU `warp_projective` against the kernel
/// (`cargo test -p filmcraft-gpu --release warp_timings -- --ignored --nocapture`).
#[test]
#[ignore]
fn warp_timings() {
    use std::time::Instant;
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let timed = |c: &mut GpuCompositor, p: &Program| {
        let _ = c.run_comp(p, 0);
        let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
        let t = Instant::now();
        for _ in 0..20 {
            let _ = c.run_comp(p, 0);
            let _ = c.device.poll(wgpu::PollType::wait_indefinitely());
        }
        t.elapsed() / 20
    };
    for (w, h) in [(1920usize, 1080usize), (6240, 4160)] {
        let (fw, fh) = (w as f64, h as f64);
        let img = sized_picture(w, h);
        for (name, quad) in [
            ("tilt", [(0.1 * fw, 0.1 * fh), (0.95 * fw, 0.2 * fh), (0.9 * fw, 0.8 * fh), (0.15 * fw, 0.95 * fh)]),
            ("minified", [(0.4 * fw, 0.4 * fh), (0.55 * fw, 0.42 * fh), (0.56 * fw, 0.6 * fh), (0.41 * fw, 0.58 * fh)]),
        ] {
            let hm = pin(w, h, quad);
            let t0 = Instant::now();
            let cpu = filmcraft_render::comp::track::warp_projective(&img, w, h, &hm);
            let cpu_t = t0.elapsed();
            drop(cpu);
            let base = Program { size: (w, h), textures: 2, passes: vec![Pass::Image { dst: 0, image: Arc::clone(&img) }], out: 0 };
            let mut warp = base.clone();
            warp.passes.push(Pass::Warp { src: 0, dst: 1, h: hm });
            warp.out = 1;
            let base_t = timed(&mut c, &base);
            let with_t = timed(&mut c, &warp);
            eprintln!("{w}x{h} {name}: CPU warp {cpu_t:?}; GPU comp {with_t:?} with the warp, {base_t:?} without");
        }
    }
}

/// C3.8 regression: comps run outside a frame (as the timing tests do) counted every slot draw
/// as a comp-free frame; the 120th let the comp textures go mid-program and `run_comp` indexed
/// the emptied slot sets (a panic).
#[test]
fn many_comps_run_outside_a_frame_keep_their_slots() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let p = program(2, vec![Pass::Image { dst: 0, image: picture() }, Pass::Copy { src: 0, dst: 1 }, Pass::Fill { dst: 0, color: [0.1, 0.2, 0.3, 1.0] }], 1);
    for _ in 0..comp::COMP_IDLE_FRAMES.saturating_add(10) {
        assert!(c.run_comp(&p, 0).is_some());
    }
    let (p99, mean) = compare(&mut c, p);
    assert!(p99 <= 4e-3 && mean <= 1e-3, "p99 {p99}, mean {mean}");
}

/// C3.8 review: a 6240 × 4160 floor-plane corner pin (strong perspective, far edge a few
/// percent of the width; minified levels near the far edge) in the kernel against the CPU. The
/// map used to be refused as singular on both (a blank picture).
#[test]
fn warp_kernel_matches_the_cpu_at_6k_with_strong_perspective() {
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (6240usize, 4160usize);
    let img = sized_picture(w, h);
    let hm = pin(w, h, [(2900.0, 1500.0), (3340.0, 1500.0), (6240.0, 4160.0), (0.0, 4160.0)]);
    let p = Program { size: (w, h), textures: 2, passes: vec![Pass::Image { dst: 0, image: img }, Pass::Warp { src: 0, dst: 1, h: hm }], out: 1 };
    let before = c.comp_warp_gpu;
    assert!(c.run_comp(&p, 0).is_some(), "runs on the GPU");
    assert_eq!(c.comp_warp_gpu, before + 1, "in the kernel");
    let plan = FramePlan::Layers { width: w, height: h, layers: vec![PlanLayer::comp(Arc::new(p), Affine::IDENTITY, 1.0, Blend::Normal)], output: None };
    let cpu = execute_cpu(&plan);
    let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("export read-back");
    drop(plan);
    assert_eq!(cpu.px.len(), gpu.len());
    let (mut max, mut sum, mut over) = (0f32, 0f64, 0usize);
    for (a, b) in cpu.px.iter().zip(&gpu) {
        let d = (a - b).abs();
        max = max.max(d);
        sum += f64::from(d);
        over += usize::from(d > 4e-3);
    }
    let drawn = cpu.px.as_chunks::<4>().0.iter().filter(|p| p[3] > 0.0).count();
    let mean = sum / gpu.len().max(1) as f64;
    eprintln!("6k floor plane: drawn {drawn}, mean {mean:.6}, over 4e-3 {over}, max {max:.4}");
    assert!(drawn > w * h / 4, "the CPU draws the plane ({drawn} pixels)");
    assert!(over <= gpu.len() / 100 && mean <= 1e-3, "mean {mean}, {over} values over 4e-3");
}

/// C3.10: Clone strokes with clone time offsets read their own source (the input at another
/// time, here distinct pictures in slots 1..=4) in the kernel as on the CPU; an offset without a
/// source clones from the input; the read-back fallback gives the same.
#[test]
fn paint_kernel_clones_from_other_times_like_the_cpu() {
    use filmcraft_project::paint::{Paint, PaintTool};
    use filmcraft_time::{FrameRate, Tick};
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let frame = FrameRate::FPS_24.frame_duration();
    let (w, h) = (203, 117);
    let mut strokes = random_strokes(80, w as f64, h as f64, 99);
    // every clone stroke takes one of six offsets: four with a source, 0 and one without
    let dts = [Tick(-frame.0), Tick(frame.0), Tick(2 * frame.0), Tick(-3 * frame.0), Tick::ZERO, Tick(i64::MAX)];
    for (i, s) in strokes.iter_mut().filter(|s| s.tool == PaintTool::Clone).enumerate() {
        s.clone_time_offset = dts[i % dts.len()];
    }
    let paint = Arc::new(Paint { strokes });
    let shifted = |k: usize| {
        let base = sized_picture(w, h);
        let px = base
            .px
            .iter()
            .enumerate()
            .map(|(i, v)| if i % 4 == 3 { *v } else { (v * (0.3 + 0.2 * k as f32)).min(*v) + 0.05 * k as f32 * (i % 4) as f32 })
            .collect();
        Arc::new(Image { w, h, px })
    };
    for cap in [comp_paint::MAX_TILE_ENTRIES, 10] {
        c.comp_paint_max_entries = cap;
        let mut passes = vec![Pass::Image { dst: 0, image: sized_picture(w, h) }];
        for k in 1..=4u16 {
            passes.push(Pass::Image { dst: k, image: shifted(usize::from(k)) });
        }
        passes.push(Pass::Fill { dst: 5, color: [0.0, 0.1, 0.6, 1.0] });
        let clones: Vec<(Tick, u16)> = dts[..4].iter().copied().zip(1..=4u16).collect();
        passes.push(Pass::Paint { tex: 0, reveal: Some(5), clones, paint: Arc::clone(&paint), at: Tick(0), frame, px_scale: 1.0 });
        let p = Program { size: (w, h), textures: 6, passes, out: 0 };
        // the clone sources change the picture (the test sees them)
        let mut plain = p.clone();
        if let Some(Pass::Paint { clones, .. }) = plain.passes.last_mut() {
            clones.clear();
        }
        let d = filmcraft_render::comp::execute_cpu(&p)
            .px
            .iter()
            .zip(&filmcraft_render::comp::execute_cpu(&plain).px)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(d > 0.05, "clone sources used: {d}");
        let before = (c.comp_paint_gpu, c.comp_paint_cpu);
        let (p99, mean, max) = compare_sized(&mut c, p);
        eprintln!("paint clones cap {cap}: p99 {p99:.5} mean {mean:.6} max {max:.5}");
        let want = if cap == 10 { (before.0, before.1 + 1) } else { (before.0 + 1, before.1) };
        assert_eq!((c.comp_paint_gpu, c.comp_paint_cpu), want, "cap {cap}: kernel / fallback");
        assert!(p99 <= 4e-3 && mean <= 1e-3, "cap {cap}: p99 {p99}, mean {mean}");
    }
    c.comp_paint_max_entries = comp_paint::MAX_TILE_ENTRIES;
}

/// Review fix guard (C3.10): a compiled comp whose Paint nodes clone from other times (two
/// sources on one node, so two of the kernel's four are dummies; four on the next, whose sources
/// hold the first Paint node again at shifted times) runs on the GPU like the CPU oracle: the
/// shifted sub-graphs' slots, reused by the rest of the program, hold the right pictures when the
/// Paint passes read them.
#[test]
fn compiled_clone_offsets_with_reused_slots_match_the_cpu() {
    use filmcraft_project::comp::{Comp, CompKind, CompNode, MediaInSource};
    use filmcraft_project::dag::{Edge, Port};
    use filmcraft_project::paint::{Paint, PaintTool};
    use filmcraft_render::comp::{CompCtx, CompSources};
    use filmcraft_time::{FrameRate, Tick};
    let Some(mut c) = compositor() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    /// A picture that changes with the frame (and with the item, for the second Media In).
    struct Frames;
    impl CompSources for Frames {
        fn media_in(&self, source: &MediaInSource, t: Tick, w: usize, h: usize) -> Option<Image> {
            let f = t.0.checked_div(FrameRate::FPS_24.frame_duration().0)?;
            let item = match source {
                MediaInSource::Item { item } => item.0 as f32,
                _ => 0.0,
            };
            let base = sized_picture(w, h);
            let k = (f.rem_euclid(13) as f32) / 13.0;
            let px = base.px.iter().enumerate().map(|(i, v)| if i % 4 == 3 { *v } else { (v * (0.4 + 0.6 * k) + 0.1 * item * k).min(1.0) }).collect();
            Some(Image { w, h, px })
        }
    }
    let frame = FrameRate::FPS_24.frame_duration();
    let paint = |offsets: &[i64], seed| {
        let mut strokes = random_strokes(40, W as f64, H as f64, seed);
        for (i, s) in strokes.iter_mut().filter(|s| s.tool == PaintTool::Clone).enumerate() {
            s.clone_time_offset = Tick(frame.0 * offsets[i % offsets.len()]);
        }
        CompKind::Paint { paint: Paint { strokes } }
    };
    let mut comp = Comp::new(W as u32, H as u32, FrameRate::FPS_24, Tick(1000));
    comp.nodes.extend([
        CompNode::new(2, CompKind::MediaIn { source: MediaInSource::Item { item: filmcraft_project::ItemId(1) }, offset: Tick(0) }),
        CompNode::new(3, paint(&[-2, 3], 7)),
        CompNode::new(4, paint(&[-1, 1, 4, -5, 0], 8)),
        CompNode::new(6, CompKind::MediaIn { source: MediaInSource::Item { item: filmcraft_project::ItemId(2) }, offset: Tick(0) }),
        CompNode::new(5, CompKind::Merge),
    ]);
    let wire = |f, t, p| Edge::new(Port::new(f, 0), Port::new(t, p));
    comp.edges = vec![wire(2, 3, 0), wire(3, 4, 0), wire(4, 5, 0), wire(6, 5, 1), wire(5, 1, 0)];
    let ctx = CompCtx { t: Tick(frame.0 * 6), size: (W, H), project: None, high_quality: false };
    let p = filmcraft_render::comp::compile(&comp, ctx, &Frames).expect("compiles");
    let clones: Vec<usize> = p.passes.iter().filter_map(|x| if let Pass::Paint { clones, .. } = x { Some(clones.len()) } else { None }).collect();
    assert!(clones.contains(&2) && clones.contains(&4), "clone sources per Paint pass: {clones:?}");
    assert!(p.textures > 2, "the shifted sub-graphs take slots of their own: {}", p.textures);
    let before = c.comp_paint_gpu;
    let (p99, mean, max) = compare_sized(&mut c, p);
    eprintln!("compiled clones: p99 {p99:.5} mean {mean:.6} max {max:.5}");
    assert!(c.comp_paint_gpu >= before + 2, "ran in the kernel");
    assert!(p99 <= 4e-3 && mean <= 1e-3, "p99 {p99}, mean {mean}");
}
