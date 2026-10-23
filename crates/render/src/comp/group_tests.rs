//! C4.3: node groups render exactly as their nodes ungrouped; exposed parameters drive the inside.

use super::*;
use filmcraft_project::comp::group::{expose, group_nodes, ungroup};
use filmcraft_project::comp::{Comp, CompKind, CompNode, MAX_GROUP_DEPTH, MediaInSource};
use filmcraft_project::dag::{Edge, Port};
use filmcraft_project::{Param, ParamValue};
use filmcraft_time::FrameRate;

const W: usize = 32;
const H: usize = 18;

fn ctx() -> CompCtx<'static> {
    CompCtx { t: Tick(0), size: (W, H), project: None, high_quality: false }
}

fn node(id: NodeId, kind: CompKind, params: &[(&str, ParamValue)]) -> CompNode {
    let mut n = CompNode::new(id, kind);
    for (k, v) in params {
        n.params.insert((*k).to_string(), Param::new(v.clone()));
    }
    n
}

fn comp(nodes: Vec<CompNode>, edges: &[(NodeId, NodeId, u8)]) -> Comp {
    let mut c = Comp::new(W as u32, H as u32, FrameRate::FPS_24, Tick(1000));
    c.nodes.extend(nodes);
    c.edges = edges.iter().map(|&(f, t, p)| Edge::new(Port::new(f, 0), Port::new(t, p))).collect();
    c
}

/// Radial gradient (2) → Blur (3) → Transform (4) → Out (1).
fn chain() -> Comp {
    comp(
        vec![
            node(2, CompKind::Gradient, &[("shape", ParamValue::Choice(1))]),
            node(3, CompKind::Blur, &[("blurriness", ParamValue::Float(6.0))]),
            node(4, CompKind::Transform, &[("scale", ParamValue::Float(70.0)), ("rotation", ParamValue::Float(30.0))]),
        ],
        &[(2, 3, 0), (3, 4, 0), (4, 1, 0)],
    )
}

fn run(c: &Comp) -> Image {
    execute_cpu(&compile(c, ctx(), &NoSources).unwrap())
}

fn same(a: &Image, b: &Image) -> bool {
    (a.w, a.h) == (b.w, b.h) && a.px == b.px
}

#[test]
fn a_group_renders_bit_identical_to_its_nodes_ungrouped() {
    let flat = chain();
    let mut grouped = flat.clone();
    let g = group_nodes(&mut grouped, &[3, 4], "Soft move").unwrap();
    assert!(matches!(grouped.node(g).map(|n| &n.kind), Some(CompKind::Group { .. })));
    let want = run(&flat);
    assert!(want.px.iter().any(|v| *v > 0.0));
    assert!(same(&run(&grouped), &want));
    // through the cached path too (the flat comp's blur is a cached static subgraph)
    assert!(same(&render_cpu(&grouped, ctx(), &NoSources), &render_cpu(&flat, ctx(), &NoSources)));
    // high quality and another size
    let hq = CompCtx { high_quality: true, size: (W * 2, H * 2), ..ctx() };
    assert!(same(&execute_cpu(&compile(&grouped, hq, &NoSources).unwrap()), &execute_cpu(&compile(&flat, hq, &NoSources).unwrap())));
    // a group of a group
    let mut twice = grouped.clone();
    let g2 = group_nodes(&mut twice, &[g], "").unwrap();
    assert!(same(&run(&twice), &want));
    // ungrouping gives back the same picture
    ungroup(&mut twice, g2).unwrap();
    ungroup(&mut twice, g).unwrap();
    assert!(same(&run(&twice), &want));
}

#[test]
fn a_two_input_group_and_a_bypassed_group() {
    // gradient (2) and solid (3) merged (5) then blurred (6): group the merge and blur
    let flat = comp(
        vec![
            node(2, CompKind::Gradient, &[]),
            node(3, CompKind::Solid, &[("color", ParamValue::Color([0.9, 0.2, 0.1, 0.6]))]),
            node(5, CompKind::Merge, &[("opacity", ParamValue::Float(70.0))]),
            node(6, CompKind::Blur, &[("blurriness", ParamValue::Float(3.0))]),
        ],
        &[(2, 5, 0), (3, 5, 1), (5, 6, 0), (6, 1, 0)],
    );
    let mut grouped = flat.clone();
    let g = group_nodes(&mut grouped, &[5, 6], "").unwrap();
    assert_eq!(grouped.node(g).unwrap().kind.inputs(), 2);
    assert!(same(&run(&grouped), &run(&flat)));
    // bypassed: input 0 (the gradient) passes through
    grouped.node_mut(g).unwrap().bypass = true;
    let grad = comp(vec![node(2, CompKind::Gradient, &[])], &[(2, 1, 0)]);
    assert!(same(&run(&grouped), &run(&grad)));
}

#[test]
fn exposed_parameters_drive_the_inside() {
    let mut grouped = chain();
    let g = group_nodes(&mut grouped, &[3, 4], "").unwrap();
    let id = expose(&mut grouped, g, 3, "blurriness", "Softness").unwrap();
    // not set on the group: the inner value
    let base = run(&grouped);
    assert!(same(&base, &run(&chain())));
    grouped.node_mut(g).unwrap().params.insert(id, Param::new(ParamValue::Float(15.0)));
    let soft = run(&grouped);
    assert!(!same(&soft, &base), "the exposed value changed the picture");
    let mut flat = chain();
    flat.node_mut(3).unwrap().params.insert("blurriness".into(), Param::new(ParamValue::Float(15.0)));
    assert!(same(&soft, &run(&flat)));
    // ungrouping keeps the exposed value
    ungroup(&mut grouped, g).unwrap();
    assert!(same(&run(&grouped), &soft));
}

#[test]
fn too_deep_empty_and_orphan_groups_are_transparent() {
    // a Group Input outside a group
    let c = comp(vec![CompNode::new(2, CompKind::MediaIn { source: MediaInSource::GroupInput { index: 0 }, offset: Tick(0) })], &[(2, 1, 0)]);
    assert!(same(&run(&c), &Image::new(W, H)));
    // a group whose inside doesn't validate
    let inner = comp(vec![node(3, CompKind::Blur, &[]), node(4, CompKind::Blur, &[])], &[(3, 4, 0), (4, 3, 0), (4, 1, 0)]);
    let c = comp(vec![CompNode::new(2, CompKind::Group { comp: Box::new(inner), exposed: Vec::new() })], &[(2, 1, 0)]);
    assert!(same(&run(&c), &Image::new(W, H)));
    // nested past the depth limit (unsanitized): the evaluator stops at MAX_GROUP_DEPTH
    let solid = || node(2, CompKind::Solid, &[("color", ParamValue::Color([0.5, 0.5, 0.5, 1.0]))]);
    let mut c = comp(vec![solid()], &[(2, 1, 0)]);
    for _ in 0..(MAX_GROUP_DEPTH + 3) {
        c = comp(vec![CompNode::new(2, CompKind::Group { comp: Box::new(c), exposed: Vec::new() })], &[(2, 1, 0)]);
    }
    assert!(same(&run(&c), &Image::new(W, H)));
    let mut ok = comp(vec![solid()], &[(2, 1, 0)]);
    let want = run(&ok);
    for _ in 0..MAX_GROUP_DEPTH {
        ok = comp(vec![CompNode::new(2, CompKind::Group { comp: Box::new(ok), exposed: Vec::new() })], &[(2, 1, 0)]);
    }
    assert!(same(&run(&ok), &want));
    assert!(!same(&want, &Image::new(W, H)));
}

#[test]
fn groups_share_the_texture_budget() {
    // nine solids merged inside a group: more than MAX_LIVE_TEXTURES at once, an error not a panic
    let mut nodes: Vec<CompNode> = (10..19).map(|i| node(i, CompKind::Solid, &[])).collect();
    let mut edges = vec![];
    let mut prev = 10;
    for (k, i) in (11..19u64).enumerate() {
        let m = 30 + k as u64;
        nodes.push(node(m, CompKind::Merge, &[]));
        edges.push((prev, m, 0));
        edges.push((i, m, 1));
        prev = m;
    }
    edges.push((prev, 1, 0));
    let c = comp(vec![CompNode::new(2, CompKind::Group { comp: Box::new(comp(nodes, &edges)), exposed: Vec::new() })], &[(2, 1, 0)]);
    assert_eq!(compile(&c, ctx(), &NoSources).unwrap_err(), CompError::TooManyTextures);
    assert!(same(&render_cpu(&c, ctx(), &NoSources), &Image::new(W, H)));
    // the chain group needs no more slots than the chain plus the input's copy
    let mut grouped = chain();
    group_nodes(&mut grouped, &[3, 4], "").unwrap();
    assert!(compile(&grouped, ctx(), &NoSources).unwrap().textures <= 3);
}

#[test]
fn an_unconnected_group_input_is_transparent_like_a_missing_input() {
    let mut flat = chain();
    let mut grouped = flat.clone();
    let g = group_nodes(&mut grouped, &[3, 4], "").unwrap();
    grouped.edges.retain(|e| e.to.node != g);
    flat.edges.retain(|e| e.to.node != 3);
    assert!(same(&run(&grouped), &run(&flat)));
    assert!(same(&run(&grouped), &Image::new(W, H)));
}

/// A moving picture (red = frame / 10), frames 0..10.
struct Moving;

impl CompSources for Moving {
    fn media_in(&self, _: &MediaInSource, t: Tick, w: usize, h: usize) -> Option<Image> {
        let fd = FrameRate::FPS_24.frame_duration().0;
        let f = t.0.checked_div(fd).filter(|f| (0..10).contains(f) && t.0 >= 0)?;
        Some(Image { w, h, px: (0..w * h).flat_map(|i| [f as f32 / 10.0, (i % w) as f32 / w as f32, 0.5, 1.0]).collect() })
    }
}

#[test]
fn paint_clone_strokes_inside_a_group() {
    use filmcraft_project::paint::{Brush, Paint, PaintPoint, PaintStroke, PaintTool};
    let fd = FrameRate::FPS_24.frame_duration();
    let t = Tick(fd.0 * 5);
    let pts = vec![PaintPoint::new(2.0, 4.5), PaintPoint::new(30.0, 4.5)];
    let stroke = PaintStroke {
        clone_time_offset: Tick(fd.0 * 2),
        ..PaintStroke::new(PaintTool::Clone, pts, Brush { size: 1.0, hardness: 1.0, opacity: 1.0, spacing: 0.1 })
    };
    let m = CompNode::new(2, CompKind::MediaIn { source: MediaInSource::Item { item: filmcraft_project::ItemId(1) }, offset: Tick(0) });
    let flat = comp(vec![m, node(3, CompKind::Paint { paint: Paint { strokes: vec![stroke] } }, &[])], &[(2, 3, 0), (3, 1, 0)]);
    let at = CompCtx { t, ..ctx() };
    let red = |img: &Image| img.px[(4 * W + 16) * 4];
    let want = execute_cpu(&compile(&flat, at, &Moving).unwrap());
    assert!((red(&want) - 0.7).abs() < 1e-6, "two frames later");
    // Media In and Paint together in a group: the same clone from another frame
    let mut both = flat.clone();
    group_nodes(&mut both, &[2, 3], "").unwrap();
    assert!(same(&execute_cpu(&compile(&both, at, &Moving).unwrap()), &want));
    // only the Paint grouped: its input is the group's, there at this frame only, so the stroke
    // clones from this frame
    let mut paint = flat.clone();
    group_nodes(&mut paint, &[3], "").unwrap();
    let got = execute_cpu(&compile(&paint, at, &Moving).unwrap());
    assert!((red(&got) - 0.5).abs() < 1e-6, "{}", red(&got));
}

/// The chain with the Transform's rotation set by `expr`, an unused outer Blur "Knob" (9) and
/// the inner Blur labelled `blur_label`; grouped or not.
fn expr_chain(expr: &str, blur_label: &str, grouped: bool) -> Comp {
    let mut c = chain();
    c.node_mut(3).unwrap().label = blur_label.into();
    let mut knob = node(9, CompKind::Blur, &[("blurriness", ParamValue::Float(9.0))]);
    knob.label = "Knob".into();
    c.nodes.push(knob);
    let mut p = Param::new(ParamValue::Float(0.0));
    p.expression = Some(expr.into());
    c.node_mut(4).unwrap().params.insert("rotation".into(), p);
    if grouped {
        group_nodes(&mut c, &[3, 4], "").unwrap();
    }
    c
}

/// The chain with a plain rotation.
fn rotated(deg: f64) -> Image {
    let mut c = chain();
    c.node_mut(4).unwrap().params.insert("rotation".into(), Param::new(ParamValue::Float(deg)));
    run(&c)
}

#[test]
fn expressions_inside_a_group_read_the_group_first_then_the_parent() {
    // an inner node by name: 2 × the inner blur's 6
    let flat = run(&expr_chain("node(\"Soft\").blurriness * 2", "Soft", false));
    assert!(same(&flat, &rotated(12.0)));
    assert!(same(&run(&expr_chain("node(\"Soft\").blurriness * 2", "Soft", true)), &flat));
    // a name only the parent has: the outer Knob's 9
    assert!(same(&run(&expr_chain("node(\"Knob\").blurriness", "Soft", true)), &rotated(9.0)));
    // a name both have: the group's own (the inner blur's 6)
    let shadow = run(&expr_chain("node(\"Knob\").blurriness", "Knob", true));
    assert!(same(&shadow, &rotated(6.0)), "the group's own node wins");
    // a name nobody has: the keyframed value (0)
    assert!(same(&run(&expr_chain("node(\"Nope\").blurriness", "Soft", true)), &rotated(0.0)));
}
