//! Branched colour grades on the GPU (fork C2 lane 3, `fx/graph.rs`) against the CPU oracle
//! (`filmcraft_render::grade::execute_graph_cpu`, through `FxOp::apply`): the bounds of the
//! serial grade parity test (`colorops_tests::gpu_compiled_grades_match_cpu`). Run on each GPU
//! with `MESA_VK_DEVICE_SELECT` (AMD and Intel).

use std::sync::Arc;

use super::fx_tests::{compare, full_precision, picture};
use super::*;
use filmcraft_color::WorkingSpace;
use filmcraft_project::dag::{Edge, Port};
use filmcraft_project::grade::{Grade, GradeNode, NodeKind, SOURCE};
use filmcraft_project::{Mask, MaskPath, Param, ParamValue};
use filmcraft_render::gpufx::FxOp;
use filmcraft_render::grade::{GradeCtx, GradePass, compile_fx};
use filmcraft_render::plan::LayerFx;
use filmcraft_time::Tick;

/// A device on the default adapter (named on stderr: the per-GPU runs report which one ran).
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    eprintln!("adapter: {}", adapter.get_info().name);
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// Relative difference allowed (the serial grade parity test's).
const TOL: f32 = 2e-4;
const W: u32 = 67;
const H: u32 = 41;

fn node(id: u64, params: &[(&str, ParamValue)]) -> GradeNode {
    let mut n = GradeNode::new(id);
    for (k, v) in params {
        n.params.insert((*k).into(), Param::new(v.clone()));
    }
    n
}

fn kind(id: u64, k: NodeKind, params: &[(&str, ParamValue)]) -> GradeNode {
    GradeNode { kind: k, ..node(id, params) }
}

fn e(from: u64, fi: u8, to: u64, ti: u8) -> Edge {
    Edge::new(Port::new(from, fi), Port::new(to, ti))
}

fn f(v: f64) -> ParamValue {
    ParamValue::Float(v)
}

fn graph(nodes: Vec<GradeNode>, edges: Vec<Edge>) -> Grade {
    Grade { nodes, edges, ..Default::default() }
}

fn gain(id: u64, g: f32) -> GradeNode {
    node(id, &[("gain", ParamValue::Color([0.0, 0.0, 0.0, g]))])
}

/// A qualified node (red-orange hues) lifting its gain, with a blurred matte and noise reduction
/// (the key reads the denoised input).
fn skin(id: u64) -> GradeNode {
    node(
        id,
        &[
            ("gain", ParamValue::Color([0.05, 0.0, -0.02, 0.4])),
            ("saturation", f(1.2)),
            ("qual.on", f(1.0)),
            ("qual.hue", f(0.03)),
            ("qual.hueWidth", f(0.2)),
            ("qual.satLow", f(0.2)),
            ("qual.lumLow", f(0.05)),
            ("qual.lumHigh", f(0.95)),
            ("qual.soft", f(0.08)),
            ("qual.blur", f(0.15)),
            ("nr.luma", f(0.4)),
            ("nr.chroma", f(0.3)),
        ],
    )
}

fn window(x: f64, y: f64, w: f64, h: f64, feather: f64) -> Mask {
    let mut m = Mask::new("w", MaskPath::rect(x, y, w, h));
    m.feather = Param::new(f(feather));
    m
}

/// The grade's ops for the test picture (one `GradeGraph` when `branched`).
fn ops(g: &Grade, ws: WorkingSpace, branched: bool) -> Vec<FxOp> {
    let ctx = GradeCtx { seq_time: Tick(5), ..Default::default() };
    let ops = compile_fx(g, Tick(123_456_789), ws, ctx, W as usize, H as usize);
    let graphs = ops.iter().filter(|o| matches!(o, FxOp::GradeGraph(_))).count();
    assert_eq!(graphs, usize::from(branched), "{ops:?}");
    assert!(ops.iter().all(FxOp::gpu_ok), "not GPU-capable: {ops:?}");
    ops
}

/// The grade on the CPU and the GPU: (max relative difference, share off by > 2 %).
fn parity(c: &mut GpuCompositor, ops: Vec<FxOp>) -> (f32, f64) {
    let px = picture(W, H);
    let mut cpu = filmcraft_render::Image { w: W as usize, h: H as usize, px: px.clone() };
    ops.iter().for_each(|o| o.apply(&mut cpu));
    assert!(cpu.px.iter().all(|v| v.is_finite()));
    let frame = VideoFrame::rgba_f32(W, H, px);
    let fx = LayerFx { size: (W, H), decimation: 1, ops };
    let (_, _, gpu) = c.effect_image(&frame, &fx).expect("effect image");
    compare(&cpu.px, &gpu)
}

/// The branched grades of DESIGN-C2 §3, by name.
fn cases() -> Vec<(String, Grade)> {
    let mut v = Vec::new();
    // skin + background on a parallel mixer: equal, weighted, and all-zero gains (equal shares)
    let background = node(2, &[("saturation", f(0.4)), ("lift", ParamValue::Color([0.0, 0.02, 0.04, 0.0])), ("contrast", f(1.15))]);
    let parallel = |mix: GradeNode| graph(vec![skin(1), background.clone(), mix], vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)]);
    v.push(("parallel".into(), parallel(kind(3, NodeKind::ParallelMixer, &[]))));
    v.push(("parallel weighted".into(), parallel(kind(3, NodeKind::ParallelMixer, &[("mix.gain.0", f(0.25)), ("mix.gain.1", f(1.5))]))));
    v.push(("parallel zero gains".into(), parallel(kind(3, NodeKind::ParallelMixer, &[("mix.gain.0", f(0.0)), ("mix.gain.1", f(0.0))]))));
    // a disabled mixer passes its first input through
    let mut off = kind(3, NodeKind::ParallelMixer, &[]);
    off.enabled = false;
    v.push(("disabled mixer".into(), parallel(off)));
    // a look on a layer mixer, each blend mode
    let lower = node(1, &[("lift", ParamValue::Color([0.05, 0.0, 0.1, 0.0])), ("contrast", f(1.1))]);
    let upper = node(2, &[("gain", ParamValue::Color([0.3, -0.2, 0.0, 0.1])), ("saturation", f(1.4))]);
    for (i, mode) in filmcraft_render::Blend::ALL.iter().enumerate() {
        let mixer = kind(3, NodeKind::LayerMixer, &[("layer.mode.1", f(i as f64)), ("layer.opacity.1", f(0.7))]);
        let g = graph(vec![lower.clone(), upper.clone(), mixer], vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)]);
        v.push((format!("layer {mode:?}"), g));
    }
    // an outside node (inverted key in) with a window of its own
    let mut outside = node(2, &[("saturation", f(0.3)), ("gain", ParamValue::Color([0.0, 0.0, 0.0, -0.3])), ("key.invert", f(1.0))]);
    outside.windows.push(window(5.0, 3.0, 50.0, 30.0, 4.0));
    let mut g = graph(vec![skin(1), outside], vec![]);
    g.make_explicit();
    g.edges.push(e(1, 1, 2, 1));
    v.push(("outside".into(), g));
    // splitter / combiner fixing green
    let g = graph(
        vec![kind(1, NodeKind::Splitter, &[]), kind(2, NodeKind::Combiner, &[]), gain(3, 0.5)],
        vec![e(SOURCE, 0, 1, 0), e(1, 0, 2, 0), e(1, 1, 3, 0), e(3, 0, 2, 1), e(1, 2, 2, 2)],
    );
    v.push(("splitter / combiner".into(), g));
    // a key mixer intersecting a window's key and a qualifier's (one inverted), limiting a node
    let mut left = GradeNode::new(1);
    left.windows.push(window(0.0, 0.0, 34.0, 41.0, 3.0));
    let km = kind(3, NodeKind::KeyMixer, &[("kmix.mode.1", f(2.0)), ("kmix.invert.1", f(1.0))]);
    let fix = node(4, &[("gain", ParamValue::Color([0.0, 0.1, 0.0, 0.5])), ("key.gain", f(0.9)), ("key.offset", f(0.05))]);
    let edges = vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 1, 3, 0), e(2, 1, 3, 1), e(SOURCE, 0, 4, 0), e(3, 0, 4, 1)];
    v.push(("key mixer intersect".into(), Grade { output: Some(4), ..graph(vec![left, skin(2), km, fix], edges) }));
    v
}

/// Every branched grade of DESIGN-C2 §3 renders on the GPU like on the CPU, in a DWG / DI and a
/// Rec. 709 working space.
#[test]
fn gpu_grade_graphs_match_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let mut failures = Vec::new();
    let mut worst_all = 0f32;
    for ws in [WorkingSpace::DaVinciWide, WorkingSpace::Rec709] {
        for (name, g) in cases() {
            let ops = ops(&g, ws, true);
            let FxOp::GradeGraph(p) = &ops[0] else { panic!("{name}: {ops:?}") };
            let kinds = |f: fn(&GradePass) -> bool| p.passes.iter().any(f);
            match name.as_str() {
                "outside" => assert!(kinds(|q| matches!(q, GradePass::KeyIn { invert: true, .. })) && kinds(|q| matches!(q, GradePass::Limit { .. }))),
                "key mixer intersect" => assert!(kinds(|q| matches!(q, GradePass::KeyMix { .. }))),
                "splitter / combiner" => assert!(kinds(|q| matches!(q, GradePass::Combine { .. }))),
                n if n.starts_with("parallel") => assert!(kinds(|q| matches!(q, GradePass::Mix { .. }))),
                n if n.starts_with("layer") => assert!(kinds(|q| matches!(q, GradePass::Over { .. }))),
                _ => {}
            }
            let (worst, flips) = parity(&mut c, ops);
            worst_all = worst_all.max(worst);
            eprintln!("{name} in {ws:?}: max rel diff {worst:.2e}, {:.3}% off by > 2 %", flips * 100.0);
            if worst > TOL || flips > 0.0 {
                failures.push(format!("{name} {ws:?}: {worst:.2e}, flips {flips:.4}"));
            }
        }
    }
    eprintln!("worst over all graphs: {worst_all:.2e}");
    assert!(failures.is_empty(), "GPU grade graphs differ from the CPU:\n{}", failures.join("\n"));
}

/// A serial grade, with or without its chain written out as edges, compiles to the serial steps
/// (no graph) and renders bit-identically on the GPU either way, matching the CPU.
#[test]
fn serial_grades_keep_the_serial_gpu_path() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let mut serial =
        graph(vec![gain(1, 0.2), skin(2), node(3, &[("saturation", f(0.5)), ("curve.g", ParamValue::Curve(vec![[0.0, 0.1], [1.0, 1.0]]))])], vec![]);
    serial.nodes[1].windows.push(window(10.0, 5.0, 30.0, 20.0, 2.0));
    let mut explicit = serial.clone();
    explicit.make_explicit();
    assert!(explicit.is_graph());
    let ws = WorkingSpace::DaVinciWide;
    let (a, b) = (ops(&serial, ws, false), ops(&explicit, ws, false));
    assert_eq!(a, b);
    let frame = VideoFrame::rgba_f32(W, H, picture(W, H));
    let run = |c: &mut GpuCompositor, ops: Vec<FxOp>| c.effect_image(&frame, &LayerFx { size: (W, H), decimation: 1, ops }).expect("effect image").2;
    let (ga, gb) = (run(&mut c, a.clone()), run(&mut c, b));
    assert!(ga.iter().zip(&gb).all(|(x, y)| x.to_bits() == y.to_bits()), "serial GPU path changed");
    let (worst, flips) = parity(&mut c, a);
    eprintln!("serial grade: max rel diff {worst:.2e}, {:.3}% off by > 2 %", flips * 100.0);
    assert!(worst <= TOL && flips == 0.0);
}

/// A graph the GPU stage can't hold (a budget of one byte) renders its clip on the CPU: the frame
/// still has its grade, as the CPU renders it.
#[test]
fn a_graph_over_the_budget_renders_on_the_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let (_, g) = cases().into_iter().next().expect("a case");
    let ops = ops(&g, WorkingSpace::DaVinciWide, true);
    let px = picture(W, H);
    let fx = LayerFx { size: (W, H), decimation: 1, ops };
    let layer = PlanLayer {
        frame: Arc::new(VideoFrame::rgba_f32(W, H, px.clone())),
        matrix: filmcraft_geom::Affine::IDENTITY,
        opacity: 1.0,
        blend: filmcraft_render::Blend::Normal,
        fx: Some(Arc::new(fx.clone())),
        comp: None,
    };
    let plan = FramePlan::Layers { width: W as usize, height: H as usize, layers: vec![layer], output: None };
    let cpu = filmcraft_render::plan::execute_cpu(&plan);
    let ungraded = filmcraft_render::plan::execute_cpu(&FramePlan::Layers {
        width: W as usize,
        height: H as usize,
        layers: vec![PlanLayer::new(Arc::new(VideoFrame::rgba_f32(W, H, px.clone())), filmcraft_geom::Affine::IDENTITY, 1.0, filmcraft_render::Blend::Normal)],
        output: None,
    });
    let diff = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
    assert!(diff(&cpu.px, &ungraded.px) > 0.01, "the grade changes the picture");
    for budget in [u64::MAX, 1] {
        c.set_fx_vram_budget(budget);
        let frame = VideoFrame::rgba_f32(W, H, px.clone());
        assert_eq!(c.effect_image(&frame, &fx).is_some(), budget == u64::MAX, "the stage refuses a graph over its budget");
        let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("export read-back");
        // (the accumulator is half float)
        let d = diff(&cpu.px, &gpu);
        eprintln!("budget {budget}: max diff from the CPU {d:.2e}");
        assert!(d < 4e-3, "budget {budget}: {d}");
    }
}

/// The test picture through `ops` on the CPU and through the frame plan on the GPU (so a graph
/// the stage can't route renders on the CPU): (max |cpu − gpu|, the CPU picture).
fn plan_parity(c: &mut GpuCompositor, ops: Vec<FxOp>) -> (f32, Vec<f32>) {
    let px = picture(W, H);
    let layer = PlanLayer {
        frame: Arc::new(VideoFrame::rgba_f32(W, H, px)),
        matrix: filmcraft_geom::Affine::IDENTITY,
        opacity: 1.0,
        blend: filmcraft_render::Blend::Normal,
        fx: Some(Arc::new(LayerFx { size: (W, H), decimation: 1, ops })),
        comp: None,
    };
    let plan = FramePlan::Layers { width: W as usize, height: H as usize, layers: vec![layer], output: None };
    let cpu = filmcraft_render::plan::execute_cpu(&plan);
    let (_, _, gpu) = c.render_export_prepared(&plan, None).expect("export read-back");
    (cpu.px.iter().zip(&gpu).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max), cpu.px)
}

/// More graphs: under an effect mask (the mask mix reads the picture the graph started from),
/// between other effects of the layer, a hand-built program with Fill / Copy passes, and a node
/// whose own steps are masked by its window.
#[test]
fn gpu_grade_graphs_in_context_match_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let ws = WorkingSpace::DaVinciWide;
    let graph_op = |name: &str| {
        let (_, g) = cases().into_iter().find(|(n, _)| n == name).expect("case");
        ops(&g, ws, true).into_iter().next().expect("graph op")
    };
    let win = window(8.0, 6.0, 40.0, 25.0, 5.0);
    let masks: Arc<[filmcraft_render::mask::FlatMask]> = filmcraft_render::mask::prepare(std::slice::from_ref(&win), Tick(0), 1.0).into();
    // a background node limited by its window (a `Masked` step inside the node's `Fx` pass)
    let mut bg = node(2, &[("saturation", f(0.4)), ("lift", ParamValue::Color([0.0, 0.02, 0.04, 0.0]))]);
    bg.windows.push(win);
    let windowed = graph(vec![skin(1), bg, kind(3, NodeKind::ParallelMixer, &[])], vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)]);
    let windowed = ops(&windowed, ws, true);
    let Some(FxOp::GradeGraph(p)) = windowed.first() else { panic!("not a graph") };
    assert!(p.passes.iter().any(|q| matches!(q, GradePass::Fx { ops, .. } if ops.iter().any(|o| matches!(o, FxOp::Masked { .. })))));
    // Fill / Copy (one of a slot never written) / Over / Mix, by hand
    let hand = FxOp::GradeGraph(Arc::new(filmcraft_render::grade::GradeProgram {
        size: (W as usize, H as usize),
        textures: 3,
        passes: vec![
            GradePass::Source { dst: 0 },
            GradePass::Fill { dst: 1, color: [0.1, 0.2, 0.05, 0.5] },
            GradePass::Copy { src: 0, dst: 2 },
            GradePass::Over { src: 1, dst: 2, mode: filmcraft_render::Blend::Multiply, opacity: 0.8 },
            GradePass::Copy { src: 7, dst: 1 },
            GradePass::Mix { srcs: vec![(2, 0.6), (1, 0.1), (0, 0.3)], dst: 0 },
        ],
        out: 0,
    }));
    assert!(hand.gpu_ok());
    let blur = FxOp::Gaussian { rx: vec![2, 2], ry: vec![1, 1], repeat: true };
    let bc = FxOp::BrightnessContrast { br: 0.05, co: 1.2 };
    let cases: Vec<(&str, Vec<FxOp>)> = vec![
        ("under an effect mask", vec![FxOp::Masked { ops: vec![graph_op("outside")], masks: masks.clone() }]),
        ("masked with an effect after it", vec![FxOp::Masked { ops: vec![graph_op("parallel"), bc.clone()], masks }]),
        ("between effects", vec![bc.clone(), graph_op("key mixer intersect"), blur.clone()]),
        ("two graphs and a blur", vec![graph_op("layer Screen"), blur, graph_op("splitter / combiner")]),
        ("window inside a node", windowed),
        ("fill / copy / over / mix", vec![bc, hand]),
    ];
    let mut failures = Vec::new();
    for (name, ops) in cases {
        assert!(ops.iter().all(FxOp::gpu_ok), "{name}");
        let (worst, flips) = parity(&mut c, ops);
        eprintln!("{name}: max rel diff {worst:.2e}, {:.3}% off by > 2 %", flips * 100.0);
        if worst > TOL || flips > 0.0 {
            failures.push(format!("{name}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "GPU grade graphs differ from the CPU:\n{}", failures.join("\n"));
}

/// A program the stage can't finish routing (its output slot is never written) renders the layer
/// on the CPU, as the oracle does (the input picture), not the layer without its effects.
#[test]
fn a_graph_the_stage_cannot_route_renders_on_the_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let broken = FxOp::GradeGraph(Arc::new(filmcraft_render::grade::GradeProgram {
        size: (W as usize, H as usize),
        textures: 2,
        passes: vec![GradePass::Source { dst: 0 }, GradePass::Fill { dst: 0, color: [1.0, 0.0, 0.0, 1.0] }],
        out: 1,
    }));
    assert!(broken.gpu_ok());
    let frame = VideoFrame::rgba_f32(W, H, picture(W, H));
    let ops = vec![FxOp::Invert { channel: 0, blend: 0.0 }, broken, FxOp::BrightnessContrast { br: 0.1, co: 1.3 }];
    assert!(c.effect_image(&frame, &LayerFx { size: (W, H), decimation: 1, ops: ops.clone() }).is_none());
    let (d, cpu) = plan_parity(&mut c, ops);
    let ungraded = picture(W, H);
    assert!(cpu.iter().zip(&ungraded).any(|(a, b)| (a - b).abs() > 0.05), "the effects around the graph apply");
    eprintln!("unroutable graph: max diff from the CPU {d:.2e}");
    // (the accumulator is half float)
    assert!(d < 4e-3, "{d}");
}

/// Fork G3: Magic Mask mattes in branched grades on the GPU like on the CPU: a branch node's
/// steps limited by its matte (`MaskedBy` inside its `Fx` pass), a key output limited by it
/// (blurred-qualifier, plain and window-only keys: an outside node, a key mixer), an outside node
/// limited by its own, a coarse matte stretched bilinearly, an empty matte.
#[test]
fn gpu_grade_graphs_with_magic_mask_mattes_match_cpu() {
    use filmcraft_render::grade::NodeMattes;
    use filmcraft_render::matte::BitmapMatte;
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = full_precision(&dev, &q);
    let top = Arc::new(BitmapMatte::new(W, H, (0..W * H).map(|i| if i / W < H / 2 { 255 } else { 0 }).collect()).expect("matte"));
    let coarse = Arc::new(BitmapMatte::new(5, 3, vec![0, 40, 255, 255, 90, 10, 200, 128, 0, 255, 255, 0, 60, 180, 30]).expect("matte"));
    let empty = Arc::new(BitmapMatte::new(1, 1, vec![0]).expect("matte"));
    let case = |name: &str| cases().into_iter().find(|(n, _)| n == name).expect("case").1;
    let runs: Vec<(&str, Grade, Vec<(u64, Arc<BitmapMatte>)>)> = vec![
        ("parallel, branch matte", case("parallel"), vec![(2, top.clone())]),
        ("parallel, coarse matte on the qualified branch", case("parallel"), vec![(1, coarse.clone())]),
        ("parallel, empty matte", case("parallel"), vec![(1, empty)]),
        ("outside, matte on the inside node", case("outside"), vec![(1, top.clone())]),
        ("outside, matte on the outside node", case("outside"), vec![(2, coarse.clone())]),
        ("outside, both", case("outside"), vec![(1, coarse.clone()), (2, top.clone())]),
        ("key mixer, matte on the window key", case("key mixer intersect"), vec![(1, coarse), (4, top)]),
    ];
    let mut failures = Vec::new();
    for (name, g, m) in runs {
        let mattes = NodeMattes(m.into_iter().collect());
        let ctx = GradeCtx { seq_time: Tick(5), mattes: Some(&mattes), ..Default::default() };
        let ops = compile_fx(&g, Tick(123_456_789), WorkingSpace::DaVinciWide, ctx, W as usize, H as usize);
        let Some(FxOp::GradeGraph(p)) = ops.first() else { panic!("{name}: {ops:?}") };
        let bitmap_key = p.passes.iter().any(|q| matches!(q, GradePass::Matte { bitmap: Some(_), .. }));
        let bitmap_fx = p.passes.iter().any(|q| matches!(q, GradePass::Fx { ops, .. } if ops.iter().any(|o| matches!(o, FxOp::MaskedBy { .. }))));
        assert!(bitmap_key || bitmap_fx, "{name}: no matte in the program");
        assert!(ops.iter().all(FxOp::gpu_ok), "{name}: not GPU-capable");
        let frame = VideoFrame::rgba_f32(W, H, picture(W, H));
        assert!(c.effect_image(&frame, &LayerFx { size: (W, H), decimation: 1, ops: ops.clone() }).is_some(), "{name}: the stage refused it");
        let (worst, flips) = parity(&mut c, ops);
        eprintln!("{name}: max rel diff {worst:.2e}, {:.3}% off by > 2 %", flips * 100.0);
        if worst > TOL || flips > 0.0 {
            failures.push(format!("{name}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    assert!(failures.is_empty(), "GPU grade graphs with mattes differ from the CPU:\n{}", failures.join("\n"));
}

/// Half-float working textures (integrated GPUs' optional preview format) stay within
/// [`HALF_TOL`] of the CPU.
#[test]
fn gpu_grade_graphs_in_half_float() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::with_fx_format(&dev, &q, fx::FX_FORMAT_HALF);
    let mut worst_all = (0f32, 0f64);
    let mut failures = Vec::new();
    for (name, g) in cases() {
        let (worst, flips) = parity(&mut c, ops(&g, WorkingSpace::DaVinciWide, true));
        worst_all = (worst_all.0.max(worst), worst_all.1.max(flips));
        if worst > HALF_TOL || flips > HALF_FLIPS {
            failures.push(format!("{name}: {worst:.2e}, flips {flips:.4}"));
        }
    }
    eprintln!("half float: worst {:.2e}, flips {:.4}", worst_all.0, worst_all.1);
    assert!(failures.is_empty(), "half-float GPU grade graphs differ from the CPU:\n{}", failures.join("\n"));
}

/// Half-float bound (the format is opt-in, off by default: `fx::HALF_ON_INTEGRATED`): a graph
/// rounds to 11 significant bits after each of a dozen passes on the grading signal (one serial
/// node through it, the disabled mixer: ~9e-3; measured worst 2.0e-2, Vivid Light). Darker /
/// Lighter Color pick per pixel by luma, so near-ties flip (measured 0.22 %).
const HALF_TOL: f32 = 3e-2;
const HALF_FLIPS: f64 = 0.005;

/// The graph shader's source is found (its sections of `composite.wgsl` / `colorops.wgsl`) and
/// parses and validates for both working formats, without a GPU: a moved marker fails here
/// instead of sending every graph to the CPU.
#[test]
fn graph_shader_source_validates() {
    use wgpu::naga;
    let src = fx::graph::module_source().expect("graph shader sections");
    for s in [src.clone(), src.replace("rgba32float", "rgba16float")] {
        let module = naga::front::wgsl::parse_str(&s).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&s)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
}
