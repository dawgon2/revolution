//! Fork C2 lane 2: grade graphs on the CPU (`graph.rs`): the serial fast path stays bit-exact,
//! every node kind against hand-built expectations, hostile graphs never panic.

use super::*;
use crate::blend::{self, Blend};
use crate::image::Image;
use filmcraft_project::dag::{Edge, Port};
use filmcraft_project::grade::SOURCE;
use filmcraft_project::{Mask, MaskPath, Param, ParamValue};

const WS: WorkingSpace = WorkingSpace::DaVinciWide;
const RED: [f32; 3] = [0.3, 0.02, 0.02];
const BLUE: [f32; 3] = [0.02, 0.03, 0.3];

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

fn gain(id: u64, g: f32) -> GradeNode {
    node(id, &[("gain", ParamValue::Color([0.0, 0.0, 0.0, g]))])
}

/// A red-qualified node lifting its gain (the skin node of the tests).
fn skin(id: u64) -> GradeNode {
    node(
        id,
        &[
            ("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5])),
            ("qual.on", f(1.0)),
            ("qual.hue", f(0.0)),
            ("qual.hueWidth", f(0.1)),
            ("qual.satLow", f(0.3)),
            ("qual.soft", f(0.02)),
        ],
    )
}

/// 32×16: red on the left half, blue on the right.
fn picture() -> Image {
    let mut img = Image::new(32, 16);
    for (i, p) in img.px.chunks_mut(4).enumerate() {
        let c = if i % 32 < 16 { RED } else { BLUE };
        p.copy_from_slice(&[c[0], c[1], c[2], 1.0]);
    }
    img
}

fn run(g: &Grade, img: &Image) -> Image {
    let mut out = img.clone();
    for op in compile_fx(g, Tick(0), WS, GradeCtx::default(), img.w, img.h) {
        op.apply(&mut out);
    }
    out
}

fn graph(nodes: Vec<GradeNode>, edges: Vec<Edge>) -> Grade {
    Grade { nodes, edges, ..Default::default() }
}

fn program(g: &Grade) -> Arc<GradeProgram> {
    match compile_fx(g, Tick(0), WS, GradeCtx::default(), 32, 16).as_slice() {
        [FxOp::GradeGraph(p)] => Arc::clone(p),
        other => panic!("not a graph program: {other:?}"),
    }
}

fn close(a: &Image, b: &Image, tol: f32) {
    assert_eq!((a.w, a.h), (b.w, b.h));
    for (i, (x, y)) in a.px.iter().zip(&b.px).enumerate() {
        assert!((x - y).abs() <= tol * y.abs().max(1.0), "value {i}: {x} vs {y}");
    }
}

fn gs() -> GradeSpace {
    GradeSpace::new(WS, HDR_WHITE_NITS)
}

/// `img` through a colour chain (straight colour, alpha kept).
fn chain(img: &Image, ops: Vec<ColorOp>) -> Image {
    let mut out = img.clone();
    FxOp::ColorChain(ops.into()).apply(&mut out);
    out
}

/// Several existing serial fixtures: plain, fused runs, a blurred qualifier, windows with noise
/// reduction, grain, a LUT-less look, a disabled node.
fn fixtures() -> Vec<Grade> {
    let mut win = node(2, &[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.5])), ("nr.luma", f(0.5))]);
    let mut m = Mask::new("w", MaskPath::rect(0.0, 0.0, 16.0, 16.0));
    m.feather = Param::new(f(0.0));
    win.windows.push(m);
    let mut off = gain(4, 0.3);
    off.enabled = false;
    let mut blurred = skin(3);
    blurred.params.insert("qual.blur".into(), Param::new(f(0.5)));
    vec![
        Grade::with_node(),
        graph(vec![node(1, &[("lift", ParamValue::Color([0.1, 0.0, 0.0, 0.0])), ("contrast", f(1.2))])], vec![]),
        graph(vec![gain(1, 0.2), skin(2), node(3, &[("saturation", f(0.5))])], vec![]),
        graph(vec![gain(1, 0.2), blurred, off, node(5, &[("grain.strength", f(0.4)), ("curve.g", ParamValue::Curve(vec![[0.0, 0.1], [1.0, 1.0]]))])], vec![]),
        graph(vec![win, node(5, &[("hl.rolloff", f(0.6)), ("temperature", f(20.0))])], vec![]),
    ]
}

/// A serial grade written out as explicit edges (or as any plain chain) compiles to exactly the
/// serial grade's steps, and renders bit-identically.
#[test]
fn a_chain_compiles_bit_exactly_like_the_serial_grade() {
    for g in fixtures() {
        let want = compile_fx(&g, Tick(0), WS, GradeCtx::default(), 32, 16);
        assert!(!want.iter().any(|o| matches!(o, FxOp::GradeGraph(_))));
        let mut explicit = g.clone();
        explicit.make_explicit();
        assert!(explicit.is_graph() || g.nodes.is_empty());
        assert_eq!(compile_fx(&explicit, Tick(0), WS, GradeCtx::default(), 32, 16), want);
        assert_eq!(compile_at(&explicit, Tick(0), WS, GradeCtx::default()), compile_at(&g, Tick(0), WS, GradeCtx::default()));
        assert_eq!(run(&explicit, &picture()).px, run(&g, &picture()).px);
    }
    // a chain in another order than the node list: the chain's order
    let (a, b) = (gain(1, 0.2), node(2, &[("saturation", f(0.3))]));
    let reordered = graph(vec![a.clone(), b.clone()], vec![e(SOURCE, 0, 2, 0), e(2, 0, 1, 0)]);
    let serial = graph(vec![b, a], vec![]);
    let want = compile_fx(&serial, Tick(0), WS, GradeCtx::default(), 32, 16);
    assert!(!want.is_empty());
    assert_eq!(compile_fx(&reordered, Tick(0), WS, GradeCtx::default(), 32, 16), want);
    // a node of another kind in a grade without edges (hostile) is the identity
    let hostile = graph(vec![gain(1, 0.2), kind(2, NodeKind::LayerMixer, &[]), kind(3, NodeKind::Splitter, &[])], vec![]);
    assert_eq!(
        compile_fx(&hostile, Tick(0), WS, GradeCtx::default(), 32, 16),
        compile_fx(&graph(vec![gain(1, 0.2)], vec![]), Tick(0), WS, GradeCtx::default(), 32, 16)
    );
}

/// A parallel mixer of two identical nodes is that node.
#[test]
fn parallel_mixer_of_identical_nodes_is_the_node() {
    let g = graph(
        vec![gain(1, 0.3), gain(2, 0.3), kind(3, NodeKind::ParallelMixer, &[])],
        vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)],
    );
    let p = program(&g);
    assert!(p.textures <= 3, "{}", p.textures);
    let img = picture();
    close(&run(&g, &img), &run(&graph(vec![gain(1, 0.3)], vec![]), &img), 1e-5);
}

/// Skin (a qualified node) and background in parallel: on the grading signal, the mean of each
/// node's result.
#[test]
fn parallel_skin_and_background_mix_on_the_grading_signal() {
    let background = node(2, &[("saturation", f(0.2))]);
    let g = graph(
        vec![skin(1), background.clone(), kind(3, NodeKind::ParallelMixer, &[])],
        vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)],
    );
    let img = picture();
    let a = chain(&run(&graph(vec![skin(1)], vec![]), &img), gs().encode_ops());
    let b = chain(&run(&graph(vec![background], vec![]), &img), gs().encode_ops());
    let mut mean = a.clone();
    mean.px.iter_mut().zip(&b.px).for_each(|(x, y)| *x = 0.5 * *x + 0.5 * y);
    let want = chain(&mean, gs().decode_ops());
    let got = run(&g, &img);
    close(&got, &want, 1e-4);
    // the skin's lift reaches the red and not the blue
    let red_only_bg = run(&graph(vec![node(2, &[("saturation", f(0.2))])], vec![]), &img);
    assert!(got.px[0] > red_only_bg.px[0] + 1e-3, "{} vs {}", got.px[0], red_only_bg.px[0]);
    // explicit gains weight the inputs: all of input 1
    let mut weighted = g.clone();
    weighted.nodes[2].params.insert("mix.gain.0".into(), Param::new(f(0.0)));
    weighted.nodes[2].params.insert("mix.gain.1".into(), Param::new(f(1.0)));
    close(&run(&weighted, &img), &run(&graph(vec![node(2, &[("saturation", f(0.2))])], vec![]), &img), 1e-5);
}

/// A layer mixer: the higher input over the lower, with each blend mode and its opacity, as C1's
/// Merge composites (on the grading signal).
#[test]
fn layer_mixer_blends_each_mode_like_merge() {
    let img = picture();
    let (lower, upper) = (node(1, &[("lift", ParamValue::Color([0.05, 0.0, 0.1, 0.0]))]), node(2, &[("gain", ParamValue::Color([0.3, -0.2, 0.0, 0.1]))]));
    let a = chain(&run(&graph(vec![lower.clone()], vec![]), &img), gs().encode_ops());
    let b = chain(&run(&graph(vec![upper.clone()], vec![]), &img), gs().encode_ops());
    for (i, mode) in Blend::ALL.iter().enumerate() {
        let mixer = kind(3, NodeKind::LayerMixer, &[("layer.mode.1", f(i as f64)), ("layer.opacity.1", f(0.7))]);
        let g = graph(vec![lower.clone(), upper.clone(), mixer], vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)]);
        let mut want = a.clone();
        blend::composite(&mut want, &b, 0.7, *mode);
        let want = chain(&want, gs().decode_ops());
        close(&run(&g, &img), &want, 1e-4);
    }
    // a single input passes through
    let one = graph(vec![lower.clone(), kind(3, NodeKind::LayerMixer, &[])], vec![e(SOURCE, 0, 1, 0), e(1, 0, 3, 0)]);
    close(&run(&one, &img), &run(&graph(vec![lower], vec![]), &img), 1e-5);
}

/// The outside node: a correction node whose key input is the previous node's key, inverted,
/// corrects exactly the complement of that node's selection.
#[test]
fn outside_node_corrects_the_complement() {
    let outside = node(2, &[("saturation", f(0.0)), ("gain", ParamValue::Color([0.0, 0.0, 0.0, -0.3])), ("key.invert", f(1.0))]);
    let mut g = graph(vec![skin(1), outside.clone()], vec![]);
    g.make_explicit();
    g.edges.push(e(1, 1, 2, 1));
    let p = program(&g);
    assert!(p.passes.iter().any(|q| matches!(q, GradePass::KeyIn { invert: true, .. })));
    let img = picture();
    let got = run(&g, &img);
    // left (red): the skin node only; right (blue): the outside node only
    let skin_only = run(&graph(vec![skin(1)], vec![]), &img);
    let mut plain_outside = outside;
    plain_outside.params.remove("key.invert");
    let outside_only = run(&graph(vec![plain_outside], vec![]), &img);
    let at = |img: &Image, x: usize| img.px.get(x * 4..x * 4 + 3).map(<[f32]>::to_vec).unwrap_or_default();
    for (a, b) in at(&got, 4).iter().zip(at(&skin_only, 4)) {
        assert!((a - b).abs() < 1e-4, "{:?} vs {:?}", at(&got, 4), at(&skin_only, 4));
    }
    for (a, b) in at(&got, 28).iter().zip(at(&outside_only, 28)) {
        assert!((a - b).abs() < 1e-4, "{:?} vs {:?}", at(&got, 28), at(&outside_only, 28));
    }
    // not inverted: the outside node's correction lands on the red instead (inside the key)
    g.nodes[1].params.insert("key.invert".into(), Param::new(f(0.0)));
    let inside = run(&g, &img);
    assert!(at(&inside, 28).iter().zip(at(&skin_only, 28)).all(|(a, b)| (a - b).abs() < 1e-4));
}

/// Splitter → combiner is the identity; a node on one channel changes only that channel.
#[test]
fn splitter_and_combiner_round_trip_and_fix_one_channel() {
    let img = picture();
    let split = kind(1, NodeKind::Splitter, &[]);
    let comb = kind(2, NodeKind::Combiner, &[]);
    let g = graph(vec![split.clone(), comb.clone()], vec![e(SOURCE, 0, 1, 0), e(1, 0, 2, 0), e(1, 1, 2, 1), e(1, 2, 2, 2)]);
    close(&run(&g, &img), &img, 1e-5);
    // green through a gain node
    let fix = gain(3, 0.5);
    let g = graph(vec![split, comb, fix], vec![e(SOURCE, 0, 1, 0), e(1, 0, 2, 0), e(1, 1, 3, 0), e(3, 0, 2, 1), e(1, 2, 2, 2)]);
    let got = run(&g, &img);
    let enc = chain(&img, gs().encode_ops());
    let mut want = enc.clone();
    want.px.chunks_mut(4).for_each(|p| p[1] *= 1.5);
    let want = chain(&want, gs().decode_ops());
    close(&got, &want, 1e-4);
    for (a, b) in got.px.chunks(4).zip(img.px.chunks(4)) {
        assert!((a[0] - b[0]).abs() < 1e-5 && (a[2] - b[2]).abs() < 1e-5 && a[1] > b[1]);
    }
}

/// A key mixer intersecting two windows' keys limits a node to where both select.
#[test]
fn key_mixer_intersect_limits_to_both_keys() {
    let rect = |x, y, w, h| {
        let mut m = Mask::new("w", MaskPath::rect(x, y, w, h));
        m.feather = Param::new(f(0.0));
        m
    };
    let mut left = GradeNode::new(1);
    left.windows.push(rect(0.0, 0.0, 16.0, 16.0));
    let mut top = GradeNode::new(2);
    top.windows.push(rect(0.0, 0.0, 32.0, 8.0));
    let km = kind(3, NodeKind::KeyMixer, &[("kmix.mode.1", f(2.0))]);
    let fix = gain(4, 0.5);
    let edges = vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 1, 3, 0), e(2, 1, 3, 1), e(SOURCE, 0, 4, 0), e(3, 0, 4, 1)];
    let g = Grade { output: Some(4), ..graph(vec![left, top, km, fix], edges) };
    let img = picture();
    let got = run(&g, &img);
    let px = |img: &Image, x: usize, y: usize| img.px.get((y * 32 + x) * 4).copied().unwrap_or(f32::NAN);
    assert!(px(&got, 4, 2) > px(&img, 4, 2) + 1e-3, "top-left corrected");
    for (x, y) in [(4, 12), (28, 2), (28, 12)] {
        assert!((px(&got, x, y) - px(&img, x, y)).abs() < 1e-5, "({x}, {y}) untouched");
    }
    // add: the union
    let mut union = g.clone();
    union.nodes[2].params.insert("kmix.mode.1".into(), Param::new(f(0.0)));
    let u = run(&union, &img);
    assert!(px(&u, 28, 2) > px(&img, 28, 2) + 1e-3 && px(&u, 4, 12) > px(&img, 4, 12) + 1e-3);
    assert!((px(&u, 28, 12) - px(&img, 28, 12)).abs() < 1e-5);
}

/// Hostile graphs: cycles, too many pictures at once, two outputs, non-finite parameters,
/// unconnected inputs, odd image sizes: no panic, and a graph that can't render is ungraded.
#[test]
fn hostile_graphs_never_panic_and_render_ungraded() {
    let img = picture();
    let ungraded = |g: &Grade| {
        assert!(compile_fx(g, Tick(0), WS, GradeCtx::default(), 32, 16).is_empty(), "{g:?}");
        assert_eq!(run(g, &img).px, img.px);
    };
    // a cycle
    ungraded(&graph(vec![gain(1, 0.2), gain(2, 0.2)], vec![e(SOURCE, 0, 1, 0), e(2, 0, 1, 1), e(1, 0, 2, 0)]));
    // two candidate outputs
    ungraded(&graph(vec![gain(1, 0.2), gain(2, 0.2)], vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0)]));
    // more pictures alive at once than the slot cap: 12 parallel nodes into one mixer
    let mut nodes: Vec<GradeNode> = (1..=12).map(|i| gain(i, 0.01 * i as f32)).collect();
    nodes.push(kind(13, NodeKind::ParallelMixer, &[]));
    let mut edges: Vec<Edge> = (1..=12).map(|i| e(SOURCE, 0, i, 0)).collect();
    edges.extend((1..=12u64).map(|i| e(i, 0, 13, (i - 1) as u8)));
    ungraded(&graph(nodes, edges));
    // non-finite and absurd parameters everywhere: finite pictures
    let nan = f(f64::NAN);
    let mut nodes = vec![
        node(
            1,
            &[
                ("gain", ParamValue::Color([f32::NAN, f32::INFINITY, 0.0, 0.0])),
                ("key.gain", nan.clone()),
                ("key.offset", f(1e300)),
                ("qual.on", f(1.0)),
                ("qual.blur", nan.clone()),
            ],
        ),
        node(2, &[("contrast", f(f64::INFINITY))]),
        kind(3, NodeKind::ParallelMixer, &[("mix.gain.0", nan.clone()), ("mix.gain.1", f(1e300))]),
        kind(4, NodeKind::LayerMixer, &[("layer.mode.1", nan.clone()), ("layer.opacity.1", f(-1e300))]),
        kind(5, NodeKind::KeyMixer, &[("kmix.mode.0", nan.clone()), ("kmix.invert.1", nan)]),
    ];
    nodes[0].windows.push(Mask::new("empty", MaskPath { vertices: Vec::new(), closed: true }));
    let edges = vec![
        e(SOURCE, 0, 1, 0),
        e(SOURCE, 0, 2, 0),
        e(1, 1, 5, 0),
        e(SOURCE, 0, 5, 1),
        e(5, 0, 2, 1),
        e(1, 0, 3, 0),
        e(2, 0, 3, 1),
        e(3, 0, 4, 0),
        e(1, 0, 4, 1),
    ];
    let g = graph(nodes, edges);
    let p = program(&g);
    assert!(p.is_finite() && p.textures <= crate::comp::MAX_LIVE_TEXTURES);
    for (w, h) in [(32, 16), (1, 1), (7, 3)] {
        let mut small = Image::filled(w, h, [0.2, 0.4, 0.1, 1.0]);
        FxOp::GradeGraph(Arc::clone(&p)).apply(&mut small);
        assert!(small.px.iter().all(|v| v.is_finite()));
    }
    assert!(execute_graph_cpu(&p, &Image::new(0, 0)).px.is_empty());
    assert_eq!(execute_graph_cpu(&p, &Image { w: 4, h: 4, px: vec![0.5; 7] }).px, vec![0.5; 7]);
    // unconnected inputs, disabled nodes, a key mixer as the output
    let mut off = gain(2, 0.4);
    off.enabled = false;
    let g = Grade {
        output: Some(4),
        ..graph(
            vec![gain(1, 0.2), off, kind(3, NodeKind::Combiner, &[]), kind(4, NodeKind::KeyMixer, &[])],
            vec![e(SOURCE, 0, 2, 0), e(2, 1, 4, 0), e(2, 0, 3, 1), e(3, 0, 1, 0), e(1, 1, 4, 1)],
        )
    };
    let out = run(&g, &img);
    assert!(out.px.iter().all(|v| v.is_finite()));
    // a bypassed graph renders as no grade
    let mut bypassed = graph(
        vec![gain(1, 0.2), gain(2, 0.2), kind(3, NodeKind::ParallelMixer, &[])],
        vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)],
    );
    assert!(!compile_fx(&bypassed, Tick(0), WS, GradeCtx::default(), 32, 16).is_empty());
    bypassed.bypass = true;
    ungraded(&bypassed);
}

/// Slots are reused by liveness: a long branched chain needs only a few pictures at once.
#[test]
fn slots_are_reused_by_liveness() {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut prev = SOURCE;
    // ten parallel pairs in a row, each mixed back into one picture
    for k in 0..10u64 {
        let (a, b, m) = (k * 3 + 1, k * 3 + 2, k * 3 + 3);
        nodes.extend([gain(a, 0.01), gain(b, -0.01), kind(m, NodeKind::ParallelMixer, &[])]);
        edges.extend([e(prev, 0, a, 0), e(prev, 0, b, 0), e(a, 0, m, 0), e(b, 0, m, 1)]);
        prev = m;
    }
    let g = graph(nodes, edges);
    let p = program(&g);
    assert!(p.textures <= 4, "{}", p.textures);
    assert!(run(&g, &picture()).px.iter().all(|v| v.is_finite()));
}

/// Parallel gains are relative: all at 1 is the default equal mix, 3 : 1 weighs 75 / 25.
#[test]
fn parallel_gains_are_normalised() {
    let (a, b) = (gain(1, 0.4), node(2, &[("saturation", f(0.2))]));
    let mixer = |params: &[(&str, ParamValue)]| {
        graph(vec![a.clone(), b.clone(), kind(3, NodeKind::ParallelMixer, params)], vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)])
    };
    let img = picture();
    let default = run(&mixer(&[]), &img);
    assert_eq!(run(&mixer(&[("mix.gain.0", f(1.0)), ("mix.gain.1", f(1.0))]), &img).px, default.px);
    let ea = chain(&run(&graph(vec![a.clone()], vec![]), &img), gs().encode_ops());
    let eb = chain(&run(&graph(vec![b.clone()], vec![]), &img), gs().encode_ops());
    let mut want = ea.clone();
    want.px.iter_mut().zip(&eb.px).for_each(|(x, y)| *x = 0.75 * *x + 0.25 * y);
    close(&run(&mixer(&[("mix.gain.0", f(1.5)), ("mix.gain.1", f(0.5))]), &img), &chain(&want, gs().decode_ops()), 1e-4);
    // gains summing to 0: an equal mix
    close(&run(&mixer(&[("mix.gain.0", f(0.0)), ("mix.gain.1", f(0.0))]), &img), &default, 1e-6);
}

/// A noisy red / blue picture (deterministic).
fn noisy() -> Image {
    let mut img = picture();
    for (i, p) in img.px.chunks_mut(4).enumerate() {
        for (c, v) in p[..3].iter_mut().enumerate() {
            let n = (((i * 3 + c) as u32).wrapping_mul(2_654_435_761) >> 16) as f32 / 65_536.0 - 0.5;
            *v = (*v + 0.16 * n).max(0.0);
        }
    }
    img
}

/// A node with noise reduction keys its denoised input (as the serial grade does), so its key out
/// is the key of that picture, and an inside and an outside node add up to one full correction.
#[test]
fn key_out_reads_the_denoised_input() {
    let mut n1 = skin(1);
    n1.params.insert("nr.luma".into(), Param::new(f(1.0)));
    n1.params.insert("nr.chroma".into(), Param::new(f(1.0)));
    let km = kind(2, NodeKind::KeyMixer, &[]);
    let g = Grade { output: Some(2), ..graph(vec![n1.clone(), km], vec![e(SOURCE, 0, 1, 0), e(1, 1, 2, 0)]) };
    let img = noisy();
    let got = run(&g, &img);
    // by hand: encode, denoise in linear light, key, as a grey picture, decode
    let enc = chain(&img, gs().encode_ops());
    let mut den = chain(&enc, gs().decode_ops());
    let nr = super::denoise(&n1, Tick(0), 1.0).unwrap();
    nr.apply(&mut den);
    let den = chain(&den, gs().encode_ops());
    let key = vec![key_op(&n1.qualifier_at(Tick(0)).unwrap(), gs().is_hdr())];
    let k = |src: &Image| -> Vec<f32> {
        src.px
            .chunks(4)
            .enumerate()
            .map(|(i, p)| filmcraft_color::ops::apply_chain_px(&key, Image::unpremul([p[0], p[1], p[2], p[3]]), [(i % 32) as f32, (i / 32) as f32], 1.0).1)
            .collect()
    };
    let (kd, kn) = (k(&den), k(&enc));
    assert!(kd.iter().zip(&kn).any(|(a, b)| (a - b).abs() > 1e-3), "the noise matters to the key");
    let want = chain(&Image { w: 32, h: 16, px: kd.iter().flat_map(|&v| [v, v, v, 1.0]).collect() }, gs().decode_ops());
    close(&got, &want, 1e-5);
    // inside + outside node = the picture plus one full correction
    let fix = node(3, &[("gain", ParamValue::Color([0.0, 0.0, 0.0, 0.4]))]);
    let pair = |invert: f64| {
        let mut fix = fix.clone();
        fix.params.insert("key.invert".into(), Param::new(f(invert)));
        Grade { output: Some(3), ..graph(vec![n1.clone(), fix], vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 3, 0), e(1, 1, 3, 1)]) }
    };
    let (inside, outside) = (chain(&run(&pair(0.0), &img), gs().encode_ops()), chain(&run(&pair(1.0), &img), gs().encode_ops()));
    let full = chain(&run(&graph(vec![fix], vec![]), &img), gs().encode_ops());
    for ((a, b), (c, o)) in inside.px.iter().zip(&outside.px).zip(full.px.iter().zip(&enc.px)) {
        assert!((a + b - o - c).abs() < 1e-4, "{a} + {b} - {o} vs {c}");
    }
}

/// A NaN in a picture fed into a key input reads as no key, never a NaN picture.
#[test]
fn non_finite_keys_read_as_zero() {
    let mut img = picture();
    img.px[0] = f32::NAN;
    // the picture: green only (splitter → combiner); the key: the source's red (NaN at 0)
    for via_mixer in [false, true] {
        let mut nodes = vec![kind(1, NodeKind::Splitter, &[]), kind(2, NodeKind::Combiner, &[]), gain(3, 0.5)];
        let mut edges = vec![e(SOURCE, 0, 1, 0), e(1, 1, 2, 1), e(2, 0, 3, 0)];
        if via_mixer {
            nodes.push(kind(4, NodeKind::KeyMixer, &[]));
            edges.extend([e(SOURCE, 0, 4, 0), e(4, 0, 3, 1)]);
        } else {
            edges.push(e(SOURCE, 0, 3, 1));
        }
        let out = run(&Grade { output: Some(3), ..graph(nodes, edges) }, &img);
        assert!(out.px.iter().all(|v| v.is_finite()), "{via_mixer}: {:?}", &out.px[..4]);
    }
}

/// A grade without edges naming an earlier node as its output (hostile) stops there, as its graph
/// does; an unknown output renders ungraded.
#[test]
fn serial_grade_honours_an_explicit_output() {
    let nodes = vec![gain(1, 0.3), node(2, &[("saturation", f(0.1))])];
    let g = Grade { output: Some(1), ..graph(nodes.clone(), vec![]) };
    let want = compile_fx(&graph(vec![gain(1, 0.3)], vec![]), Tick(0), WS, GradeCtx::default(), 32, 16);
    assert_eq!(compile_fx(&g, Tick(0), WS, GradeCtx::default(), 32, 16), want);
    assert!(compile_fx(&Grade { output: Some(99), ..graph(nodes, vec![]) }, Tick(0), WS, GradeCtx::default(), 32, 16).is_empty());
}

/// A refused graph doesn't count as changing the picture; a real one does.
#[test]
fn broken_graphs_dont_change_the_picture() {
    let cycle = graph(vec![gain(1, 0.2), gain(2, 0.2)], vec![e(SOURCE, 0, 1, 0), e(2, 0, 1, 1), e(1, 0, 2, 0)]);
    assert!(!grade_changes(&cycle, Tick(0), None));
    let real = graph(
        vec![gain(1, 0.2), gain(2, 0.2), kind(3, NodeKind::ParallelMixer, &[])],
        vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)],
    );
    assert!(grade_changes(&real, Tick(0), None));
}

/// A splitter with only some outputs connected; disabled mixers and splitters pass their picture.
#[test]
fn partial_splitters_and_disabled_nodes() {
    let img = picture();
    // only blue reaches the combiner (alpha from its first connected input: the blue picture's)
    let g =
        Grade { output: Some(2), ..graph(vec![kind(1, NodeKind::Splitter, &[]), kind(2, NodeKind::Combiner, &[])], vec![e(SOURCE, 0, 1, 0), e(1, 2, 2, 2)]) };
    let mut want = chain(&img, gs().encode_ops());
    want.px.chunks_mut(4).for_each(|p| {
        p[0] = 0.0;
        p[1] = 0.0;
    });
    close(&run(&g, &img), &chain(&want, gs().decode_ops()), 1e-5);
    // a disabled parallel mixer passes its input 0
    let mut mixer = kind(3, NodeKind::ParallelMixer, &[]);
    mixer.enabled = false;
    let g = graph(vec![gain(1, 0.3), node(2, &[("saturation", f(0.1))]), mixer], vec![e(SOURCE, 0, 1, 0), e(SOURCE, 0, 2, 0), e(1, 0, 3, 0), e(2, 0, 3, 1)]);
    close(&run(&g, &img), &run(&graph(vec![gain(1, 0.3)], vec![]), &img), 1e-5);
    // a disabled splitter passes the whole picture on each output (enabled: green as grey)
    let mut split = kind(1, NodeKind::Splitter, &[]);
    split.enabled = false;
    let g = Grade { output: Some(2), ..graph(vec![split.clone(), GradeNode::new(2)], vec![e(SOURCE, 0, 1, 0), e(1, 1, 2, 0)]) };
    close(&run(&g, &img), &img, 1e-5);
    split.enabled = true;
    let g = Grade { output: Some(2), ..graph(vec![split, GradeNode::new(2)], vec![e(SOURCE, 0, 1, 0), e(1, 1, 2, 0)]) };
    let out = run(&g, &img);
    assert!(out.px.chunks(4).all(|p| (p[0] - p[1]).abs() < 1e-6 && (p[2] - p[1]).abs() < 1e-6));
}
