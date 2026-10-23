//! Comp evaluation: each node against the primitive it reuses, scheduling, and the graph rules.

use super::*;
use filmcraft_project::comp::{Comp, CompKind, CompNode, MediaInSource};
use filmcraft_project::dag::{Edge, Port};
use filmcraft_project::{ItemId, Param, ParamValue};
use filmcraft_time::FrameRate;

const W: usize = 32;
const H: usize = 18;

fn ctx() -> CompCtx<'static> {
    CompCtx { t: Tick(0), size: (W, H), project: None, high_quality: false }
}

/// A comp of `W`×`H` holding `nodes` (Media Out is node 1), wired by `(from, to, port)`.
fn comp(nodes: Vec<CompNode>, edges: &[(NodeId, NodeId, u8)]) -> Comp {
    let mut c = Comp::new(W as u32, H as u32, FrameRate::FPS_24, Tick(1000));
    c.nodes.extend(nodes);
    c.edges = edges.iter().map(|&(f, t, p)| Edge::new(Port::new(f, 0), Port::new(t, p))).collect();
    c
}

fn node(id: NodeId, kind: CompKind, params: &[(&str, ParamValue)]) -> CompNode {
    let mut n = CompNode::new(id, kind);
    for (k, v) in params {
        n.params.insert((*k).to_string(), Param::new(v.clone()));
    }
    n
}

fn solid(id: NodeId, c: [f32; 4]) -> CompNode {
    node(id, CompKind::Solid, &[("color", ParamValue::Color(c))])
}

fn run(c: &Comp) -> Image {
    execute_cpu(&compile(c, ctx(), &NoSources).unwrap())
}

fn max_diff(a: &Image, b: &Image) -> f32 {
    assert_eq!((a.w, a.h), (b.w, b.h));
    a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// The picture a Solid of sRGB colour `c` draws (linear, premultiplied).
fn solid_image(c: [f32; 4]) -> Image {
    let [r, g, b] = dec([c[0], c[1], c[2]]);
    Image::filled(W, H, [r * c[3], g * c[3], b * c[3], c[3]])
}

#[test]
fn solid_draws_its_colour_in_linear_light() {
    let c = comp(vec![solid(2, [1.0, 0.5, 0.0, 0.5])], &[(2, 1, 0)]);
    assert_eq!(max_diff(&run(&c), &solid_image([1.0, 0.5, 0.0, 0.5])), 0.0);
}

#[test]
fn merge_is_the_blend_composite() {
    let (bg, fg) = ([0.2, 0.4, 0.6, 1.0], [0.9, 0.1, 0.3, 0.7]);
    let merge = node(4, CompKind::Merge, &[("opacity", ParamValue::Float(60.0)), ("blend", ParamValue::Choice(Blend::Screen.index()))]);
    let c = comp(vec![solid(2, bg), solid(3, fg), merge], &[(2, 4, 0), (3, 4, 1), (4, 1, 0)]);
    let mut want = solid_image(bg);
    blend::composite(&mut want, &solid_image(fg), 0.6, Blend::Screen);
    assert_eq!(max_diff(&run(&c), &want), 0.0);
}

#[test]
fn merge_without_foreground_passes_the_background() {
    let c = comp(vec![solid(2, [0.3, 0.3, 0.3, 1.0]), node(4, CompKind::Merge, &[])], &[(2, 4, 0), (4, 1, 0)]);
    assert_eq!(max_diff(&run(&c), &solid_image([0.3, 0.3, 0.3, 1.0])), 0.0);
}

#[test]
fn transform_is_the_motion_matrix_draw() {
    let g = node(2, CompKind::Gradient, &[]);
    let tr = node(
        3,
        CompKind::Transform,
        &[("scale", ParamValue::Float(50.0)), ("rotation", ParamValue::Float(30.0)), ("position", ParamValue::Vec2(Vec2::new(10.0, 6.0)))],
    );
    let c = comp(vec![g.clone(), tr], &[(2, 3, 0), (3, 1, 0)]);
    let src = gradient(&g, Tick(0), W, H);
    let m = Affine::motion(Vec2::new(10.0, 6.0), Vec2::new(0.5, 0.5), 30.0, Vec2::new(W as f64 / 2.0, H as f64 / 2.0));
    assert_eq!(max_diff(&run(&c), &src.transformed(W, H, &m)), 0.0);
}

#[test]
fn blur_is_the_gaussian_blur_effect() {
    let g = node(2, CompKind::Gradient, &[("shape", ParamValue::Choice(1))]);
    let blur = node(3, CompKind::Blur, &[("blurriness", ParamValue::Float(6.0))]);
    let c = comp(vec![g.clone(), blur], &[(2, 3, 0), (3, 1, 0)]);
    let mut want = gradient(&g, Tick(0), W, H);
    let mut e = find_effect("gaussian_blur").unwrap().instance();
    e.params.insert("blurriness".into(), Param::new(ParamValue::Float(6.0)));
    let cx = FxCtx { t: Tick(0), px_scale: 1.0, seconds: 0.0, timecode: "", clip_name: "", project: None, env: None, working: Default::default() };
    gpufx::apply_ops(&FxOp::eval_all(&e, &cx, W, H).unwrap(), &mut want);
    let got = run(&c);
    assert_eq!(max_diff(&got, &want), 0.0);
    assert!(max_diff(&got, &gradient(&g, Tick(0), W, H)) > 1e-4, "the blur changed the picture");
}

#[test]
fn color_node_is_the_grade_compiler() {
    let mut gn = filmcraft_project::grade::GradeNode::new(1);
    gn.params.insert("saturation".into(), Param::new(ParamValue::Float(0.0)));
    let g = node(2, CompKind::Gradient, &[("color_b", ParamValue::Color([1.0, 0.2, 0.1, 1.0]))]);
    let c = comp(vec![g.clone(), node(3, CompKind::Color { node: gn.clone() }, &[])], &[(2, 3, 0), (3, 1, 0)]);
    let mut want = gradient(&g, Tick(0), W, H);
    let grade = Grade { nodes: vec![gn], ..Default::default() };
    let ops = crate::grade::compile_fx(&grade, Tick(0), c.color.working, Default::default(), W, H);
    assert!(!ops.is_empty());
    gpufx::apply_ops(&ops, &mut want);
    assert_eq!(max_diff(&run(&c), &want), 0.0);
}

#[test]
fn gradient_runs_from_start_to_end_colour() {
    let g = node(2, CompKind::Gradient, &[]);
    let img = gradient(&g, Tick(0), W, H);
    let px = |x: usize| img.px.get((H / 2 * W + x) * 4).copied().unwrap();
    assert!(px(0) < 0.05 && px(W - 1) > 0.9, "{} {}", px(0), px(W - 1));
    assert!((1..W).all(|x| px(x) >= px(x - 1)), "monotonic");
    // a large comp generates small and upscales
    let big = gradient(&g, Tick(0), 4000, 10);
    assert_eq!((big.w, big.h), (4000, 10));
}

struct Flat(Tick);

impl CompSources for Flat {
    fn media_in(&self, source: &MediaInSource, t: Tick, w: usize, h: usize) -> Option<Image> {
        // a picture whose red channel says which item and when it was asked for
        match source {
            MediaInSource::Item { item } if t == self.0 => Some(Image::filled(w, h, [item.0 as f32 / 10.0, 0.0, 0.0, 1.0])),
            _ => None,
        }
    }
}

#[test]
fn media_in_reads_its_source_at_comp_time_plus_offset() {
    let m = CompNode::new(2, CompKind::MediaIn { source: MediaInSource::Item { item: ItemId(3) }, offset: Tick(5) });
    let c = comp(vec![m], &[(2, 1, 0)]);
    let at = |t| execute_cpu(&compile(&c, CompCtx { t: Tick(t), ..ctx() }, &Flat(Tick(15))).unwrap());
    assert_eq!(max_diff(&at(10), &Image::filled(W, H, [0.3, 0.0, 0.0, 1.0])), 0.0);
    // nothing there at another time: transparent
    assert_eq!(max_diff(&at(11), &Image::new(W, H)), 0.0);
}

#[test]
fn bypass_and_missing_inputs() {
    // a bypassed Blur passes its input; a bypassed source is transparent
    let mut blur = node(3, CompKind::Blur, &[("blurriness", ParamValue::Float(9.0))]);
    blur.bypass = true;
    let g = node(2, CompKind::Gradient, &[]);
    let c = comp(vec![g.clone(), blur], &[(2, 3, 0), (3, 1, 0)]);
    assert_eq!(max_diff(&run(&c), &gradient(&g, Tick(0), W, H)), 0.0);
    let mut s = solid(2, [1.0; 4]);
    s.bypass = true;
    assert_eq!(max_diff(&run(&comp(vec![s], &[(2, 1, 0)])), &Image::new(W, H)), 0.0);
    // an unconnected Media Out and an effect node without input are transparent
    assert_eq!(max_diff(&run(&comp(vec![], &[])), &Image::new(W, H)), 0.0);
    let c = comp(vec![node(3, CompKind::Blur, &[])], &[(3, 1, 0)]);
    assert_eq!(max_diff(&run(&c), &Image::new(W, H)), 0.0);
}

#[test]
fn a_shared_input_is_not_overwritten_in_place() {
    // Solid 2 feeds a Mask-less Color node (in place) and the Merge's background
    let mut gn = filmcraft_project::grade::GradeNode::new(1);
    gn.params.insert("saturation".into(), Param::new(ParamValue::Float(0.0)));
    let col = [0.8, 0.2, 0.1, 1.0];
    let merge = node(4, CompKind::Merge, &[("opacity", ParamValue::Float(50.0))]);
    let c = comp(vec![solid(2, col), node(3, CompKind::Color { node: gn.clone() }, &[]), merge], &[(2, 3, 0), (2, 4, 0), (3, 4, 1), (4, 1, 0)]);
    let mut grey = solid_image(col);
    let grade = Grade { nodes: vec![gn], ..Default::default() };
    gpufx::apply_ops(&crate::grade::compile_fx(&grade, Tick(0), c.color.working, Default::default(), W, H), &mut grey);
    let mut want = solid_image(col);
    blend::composite(&mut want, &grey, 0.5, Blend::Normal);
    assert_eq!(max_diff(&run(&c), &want), 0.0);
    // a Merge of a picture with itself
    let c = comp(vec![solid(2, [0.5, 0.5, 0.5, 0.5]), node(4, CompKind::Merge, &[])], &[(2, 4, 0), (2, 4, 1), (4, 1, 0)]);
    let mut want = solid_image([0.5, 0.5, 0.5, 0.5]);
    blend::composite(&mut want, &solid_image([0.5, 0.5, 0.5, 0.5]), 1.0, Blend::Normal);
    assert_eq!(max_diff(&run(&c), &want), 0.0);
}

#[test]
fn liveness_keeps_texture_counts_small() {
    // acceptance comp A's shape: Media In → Key → Merge over Solid → Color → Out
    let gn = filmcraft_project::grade::GradeNode::new(1);
    let a = comp(
        vec![
            CompNode::new(2, CompKind::MediaIn { source: MediaInSource::Item { item: ItemId(1) }, offset: Tick(0) }),
            node(3, CompKind::from_key("key").unwrap(), &[]),
            solid(4, [0.0, 0.0, 1.0, 1.0]),
            node(5, CompKind::Merge, &[]),
            node(6, CompKind::Color { node: gn }, &[]),
        ],
        &[(2, 3, 0), (4, 5, 0), (3, 5, 1), (5, 6, 0), (6, 1, 0)],
    );
    assert!(compile(&a, ctx(), &NoSources).unwrap().textures <= 3);
    // a 62-step chain of in-place nodes and Transforms needs two
    let mut nodes = vec![solid(2, [1.0; 4])];
    let mut edges = vec![];
    for i in 3..64u64 {
        let kind = if i % 2 == 0 { CompKind::Transform } else { CompKind::Blur };
        nodes.push(node(i, kind, &[]));
        edges.push((i - 1, i, 0));
    }
    edges.push((63, 1, 0));
    assert_eq!(compile(&comp(nodes, &edges), ctx(), &NoSources).unwrap().textures, 2);
}

#[test]
fn too_many_live_pictures_and_bad_graphs_render_transparent() {
    // nine sources all merged at the end: more than MAX_LIVE_TEXTURES alive at once
    let mut nodes: Vec<CompNode> = (10..19).map(|i| solid(i, [1.0; 4])).collect();
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
    // sources come first in id order, so all nine are alive before the first Merge
    assert_eq!(compile(&comp(nodes, &edges), ctx(), &NoSources).unwrap_err(), CompError::TooManyTextures);
    // a cycle
    let c = comp(vec![node(3, CompKind::Blur, &[]), node(4, CompKind::Blur, &[])], &[(3, 4, 0), (4, 3, 0), (4, 1, 0)]);
    assert!(matches!(compile(&c, ctx(), &NoSources), Err(CompError::Graph(_))));
    assert_eq!(max_diff(&render_cpu(&c, ctx(), &NoSources), &Image::new(W, H)), 0.0);
    // a zero-size request still gives a 1×1 picture
    let tiny = render_cpu(&c, CompCtx { size: (0, 0), ..ctx() }, &NoSources);
    assert_eq!((tiny.w, tiny.h), (1, 1));
}

#[test]
fn text_is_the_graphic_text_layer() {
    let CompKind::Text { mut layer } = CompKind::from_key("text").unwrap() else { panic!("text kind") };
    layer.params.insert("text".into(), Param::new(ParamValue::Text("Hi".into())));
    layer.params.insert("size".into(), Param::new(ParamValue::Float(14.0)));
    let c = comp(vec![CompNode::new(2, CompKind::Text { layer: layer.clone() })], &[(2, 1, 0)]);
    let mut want = Image::new(W, H);
    crate::graphic_clip::render_graphic_layers(std::slice::from_ref(&*layer), Tick(0), (W as u32, H as u32), &Affine::scale(1.0, 1.0), &mut want);
    let got = run(&c);
    assert_eq!(max_diff(&got, &want), 0.0);
    assert!(got.px.chunks(4).any(|p| p[3] > 0.5), "glyphs drawn");
    // at half size the same layer is drawn scaled, still within the canvas
    let half = execute_cpu(&compile(&c, CompCtx { size: (W / 2, H / 2), ..ctx() }, &NoSources).unwrap());
    assert_eq!((half.w, half.h), (W / 2, H / 2));
}

/// Acceptance comp B's shape: a title moved by keyframes (as a tracker writes them) over a
/// background: the text follows the keyframed position.
#[test]
fn a_keyframed_transform_moves_a_title_over_a_background() {
    let CompKind::Text { mut layer } = CompKind::from_key("text").unwrap() else { panic!("text kind") };
    layer.params.insert("text".into(), Param::new(ParamValue::Text("II".into())));
    layer.params.insert("size".into(), Param::new(ParamValue::Float(12.0)));
    let mut pos = Param::new(ParamValue::Vec2(Vec2::new(8.0, 9.0)));
    pos.keyframes = vec![
        filmcraft_project::Keyframe::new(Tick(0), ParamValue::Vec2(Vec2::new(8.0, 9.0))),
        filmcraft_project::Keyframe::new(Tick(1000), ParamValue::Vec2(Vec2::new(24.0, 9.0))),
    ];
    let mut tr = CompNode::new(3, CompKind::Transform);
    tr.params.insert("position".into(), pos);
    let c = comp(
        vec![CompNode::new(2, CompKind::Text { layer }), tr, solid(4, [0.0, 0.0, 0.2, 1.0]), node(5, CompKind::Merge, &[])],
        &[(2, 3, 0), (4, 5, 0), (3, 5, 1), (5, 1, 0)],
    );
    // the bright pixels' mean x at each time
    let centre_x = |t: i64| {
        let img = execute_cpu(&compile(&c, CompCtx { t: Tick(t), ..ctx() }, &NoSources).unwrap());
        let xs: Vec<f32> = img.px.chunks(4).enumerate().filter(|(_, p)| p[0] > 0.5).map(|(i, _)| (i % W) as f32).collect();
        assert!(!xs.is_empty(), "title drawn at {t}");
        xs.iter().sum::<f32>() / xs.len() as f32
    };
    let (a, b) = (centre_x(0), centre_x(1000));
    assert!((a - 8.0).abs() < 3.0 && (b - 24.0).abs() < 3.0, "{a} {b}");
    assert!((centre_x(500) - 16.0).abs() < 3.0);
}

fn title(id: NodeId, text: &str) -> CompNode {
    let CompKind::Text { mut layer } = CompKind::from_key("text").unwrap() else { panic!("text kind") };
    layer.params.insert("text".into(), Param::new(ParamValue::Text(text.into())));
    layer.params.insert("size".into(), Param::new(ParamValue::Float(14.0)));
    CompNode::new(id, CompKind::Text { layer })
}

fn media(id: NodeId) -> CompNode {
    CompNode::new(id, CompKind::MediaIn { source: MediaInSource::Item { item: ItemId(3) }, offset: Tick::ZERO })
}

/// The cached compile of `c` at the Flat source's time, and the uncached one.
fn both(c: &Comp) -> (Program, Image, Image) {
    let cx = CompCtx { t: Tick(15), ..ctx() };
    let cached = compile_cached(c, cx, &Flat(Tick(15))).unwrap();
    let plain = execute_cpu(&compile(c, cx, &Flat(Tick(15))).unwrap());
    let img = execute_cpu(&cached);
    (cached, img, plain)
}

#[test]
fn a_static_title_is_cached_and_renders_the_same() {
    // Text → Transform → Merge over a Media In: the title subgraph renders once
    let c = comp(
        vec![title(2, "Cache me"), node(3, CompKind::Transform, &[("scale", ParamValue::Float(80.0))]), media(4), node(5, CompKind::Merge, &[])],
        &[(2, 3, 0), (4, 5, 0), (3, 5, 1), (5, 1, 0)],
    );
    let order = c.validate().unwrap();
    let tops = static_tops(&c, &order);
    assert_eq!(tops, vec![(3, BTreeSet::from([2, 3]))]);
    let (p1, img, plain) = both(&c);
    assert_eq!(max_diff(&img, &plain), 0.0);
    let image_of = |p: &Program| -> Vec<Arc<Image>> {
        p.passes.iter().filter_map(|x| if let Pass::Image { image, .. } = x { Some(Arc::clone(image)) } else { None }).collect()
    };
    // Media In + the cached title, no Draw of the title any more
    assert_eq!(image_of(&p1).len(), 2);
    assert!(!p1.passes.iter().any(|x| matches!(x, Pass::Draw { .. })));
    // a second compile reuses the same picture; another text is another entry
    let title_of = |p: &Program| image_of(p).into_iter().find(|i| i.px.chunks(4).any(|q| q[3] < 0.5)).unwrap();
    let (p2, ..) = both(&c);
    assert!(Arc::ptr_eq(&title_of(&p1), &title_of(&p2)));
    let mut other = c.clone();
    other.nodes[1] = title(2, "Other");
    let (p3, img3, plain3) = both(&other);
    assert_eq!(max_diff(&img3, &plain3), 0.0);
    assert!(!Arc::ptr_eq(&title_of(&p1), &title_of(&p3)));
}

#[test]
fn keyframes_footage_and_color_nodes_are_not_cached() {
    let mut moving = node(3, CompKind::Transform, &[]);
    let mut pos = Param::new(ParamValue::Vec2(Vec2::new(0.0, 0.0)));
    pos.keyframes.push(filmcraft_project::Keyframe::new(Tick(0), ParamValue::Vec2(Vec2::new(0.0, 0.0))));
    pos.keyframes.push(filmcraft_project::Keyframe::new(Tick(100), ParamValue::Vec2(Vec2::new(10.0, 5.0))));
    moving.params.insert("position".into(), pos);
    let c = comp(vec![title(2, "Moving"), moving, media(4), node(5, CompKind::Merge, &[])], &[(2, 3, 0), (4, 5, 0), (3, 5, 1), (5, 1, 0)]);
    // only the title itself (read by the keyframed Transform) is static
    assert_eq!(static_tops(&c, &c.validate().unwrap()), vec![(2, BTreeSet::from([2]))]);
    let (_, img, plain) = both(&c);
    assert_eq!(max_diff(&img, &plain), 0.0);
    // a lone Solid over footage is not worth a cache entry; a Color node never is
    let c = comp(vec![solid(2, [1.0, 0.0, 0.0, 1.0]), media(3), node(4, CompKind::Merge, &[])], &[(3, 4, 0), (2, 4, 1), (4, 1, 0)]);
    assert!(static_tops(&c, &c.validate().unwrap()).is_empty());
    let c = comp(vec![title(2, "Graded"), CompNode::new(3, CompKind::from_key("color").unwrap())], &[(2, 3, 0), (3, 1, 0)]);
    assert_eq!(static_tops(&c, &c.validate().unwrap()), vec![(2, BTreeSet::from([2]))]);
}

#[test]
fn a_cached_node_still_read_elsewhere_is_still_compiled() {
    // Solid → Mask (cached with its Solid) → Merge over footage, and the same Solid → a second Merge
    let mask = CompNode::new(3, CompKind::Mask { masks: Vec::new() });
    let c = comp(
        vec![solid(2, [0.0, 1.0, 0.0, 0.5]), mask, media(4), node(5, CompKind::Merge, &[]), node(6, CompKind::Merge, &[])],
        &[(2, 3, 0), (4, 5, 0), (3, 5, 1), (5, 6, 0), (2, 6, 1), (6, 1, 0)],
    );
    let (_, img, plain) = both(&c);
    assert_eq!(max_diff(&img, &plain), 0.0);
    assert!(img.px.chunks(4).any(|q| q[1] > 0.1), "the solid is drawn");
}

#[test]
fn the_static_cache_stays_within_its_bytes() {
    let mut cache = StaticCache::default();
    let img = |n: usize| Arc::new(Image::new(n, n));
    let cap = image_bytes(&img(10)) * 2;
    cache.put("a".into(), img(10), cap);
    cache.put("b".into(), img(10), cap);
    assert!(cache.get("a").is_some());
    cache.put("c".into(), img(10), cap);
    assert!(cache.get("b").is_none(), "least recently used out first");
    assert!(cache.get("a").is_some() && cache.get("c").is_some());
    cache.put("huge".into(), img(100), cap);
    assert!(cache.get("huge").is_none());
    assert!(cache.bytes <= cap);
    cache.put("a".into(), img(10), cap);
    assert_eq!(cache.bytes, cap);
}

// -- C3.5b: Roto and Paint nodes ---------------------------------------------------------------

fn roto_square(output: filmcraft_project::comp::RotoOutput) -> CompKind {
    use filmcraft_project::roto::{Roto, RotoKind, RotoShape};
    let mut shape = RotoShape::new("sq", RotoKind::Bezier, filmcraft_project::MaskPath::rect(4.0, 3.0, 20.0, 12.0));
    shape.feather = Param::new(ParamValue::Float(4.0));
    CompKind::Roto { roto: Roto { shapes: vec![shape] }, output }
}

#[test]
fn roto_alpha_cuts_the_input_by_the_shapes_coverage() {
    use filmcraft_project::comp::RotoOutput;
    let col = [0.8, 0.4, 0.2, 1.0];
    let c = comp(vec![solid(2, col), node(3, roto_square(RotoOutput::Alpha), &[])], &[(2, 3, 0), (3, 1, 0)]);
    let out = run(&c);
    let CompKind::Roto { roto, .. } = &c.node(3).unwrap().kind else { unreachable!() };
    let cov = crate::roto::rasterize(&crate::roto::prepare(roto, Tick(0), W, H, 1.0), W, H);
    let base = solid_image(col);
    let mut want = base.clone();
    for (p, c) in want.px.chunks_mut(4).zip(&cov) {
        p.iter_mut().for_each(|v| *v *= c);
    }
    // (C3.6: the f32 coverage itself, no 8-bit matte)
    assert_eq!(max_diff(&out, &want), 0.0);
    assert!(cov.iter().any(|c| *c > 0.0 && *c < 1.0), "the feather makes a ramp");
    assert_eq!(out.px[3], 0.0, "outside: cut away");
    let mid = (8 * W + 12) * 4;
    assert_eq!(&out.px[mid..mid + 4], &base.px[mid..mid + 4], "inside: unchanged");

    // no enabled shape: the input passes
    let mut c2 = c.clone();
    if let Some(CompKind::Roto { roto, .. }) = c2.node_mut(3).map(|n| &mut n.kind) {
        roto.shapes[0].enabled = false;
    }
    assert_eq!(max_diff(&run(&c2), &base), 0.0);
}

#[test]
fn roto_matte_is_the_coverage_white_on_black() {
    use filmcraft_project::comp::RotoOutput;
    let c = comp(vec![solid(2, [0.1, 0.9, 0.1, 0.5]), node(3, roto_square(RotoOutput::Matte), &[])], &[(2, 3, 0), (3, 1, 0)]);
    let out = run(&c);
    let CompKind::Roto { roto, .. } = &c.node(3).unwrap().kind else { unreachable!() };
    let cov = crate::roto::rasterize(&crate::roto::prepare(roto, Tick(0), W, H, 1.0), W, H);
    for (p, c) in out.px.chunks(4).zip(&cov) {
        assert_eq!(p, &[*c, *c, *c, 1.0]);
    }
    // the static-subgraph cache gives the same picture
    assert_eq!(max_diff(&render_cpu(&c, ctx(), &NoSources), &out), 0.0);
    // no shapes: black
    let empty = comp(vec![node(3, CompKind::Roto { roto: Default::default(), output: RotoOutput::Matte }, &[])], &[(3, 1, 0)]);
    assert_eq!(max_diff(&run(&empty), &Image::filled(W, H, [0.0, 0.0, 0.0, 1.0])), 0.0);
}

fn stroke(tool: filmcraft_project::paint::PaintTool, duration: filmcraft_project::paint::StrokeDuration) -> filmcraft_project::paint::PaintStroke {
    use filmcraft_project::paint::{Brush, PaintPoint, PaintStroke};
    let pts = vec![PaintPoint::new(4.0, 9.0), PaintPoint::new(28.0, 9.0)];
    PaintStroke { color: [1.0, 0.0, 0.0, 1.0], duration, ..PaintStroke::new(tool, pts, Brush { size: 6.0, ..Brush::default() }) }
}

#[test]
fn paint_is_the_stroke_renderer_with_reveal_from_input_two_and_strokes_by_time() {
    use filmcraft_project::paint::{Paint, PaintTool, StrokeDuration};
    let (a, b) = ([0.2, 0.3, 0.9, 1.0], [0.9, 0.9, 0.1, 1.0]);
    let frame = FrameRate::FPS_24.frame_duration();
    let strokes = vec![stroke(PaintTool::Color, StrokeDuration::default()), stroke(PaintTool::Reveal, StrokeDuration::SingleFrame { at: Tick(frame.0 * 3) })];
    let paint = Paint { strokes };
    let c = comp(vec![solid(2, a), solid(3, b), node(4, CompKind::Paint { paint: paint.clone() }, &[])], &[(2, 4, 0), (3, 4, 1), (4, 1, 0)]);
    let at = |t: Tick| render_cpu(&c, CompCtx { t, ..ctx() }, &NoSources);
    let (sa, sb) = (solid_image(a), solid_image(b));
    for t in [Tick(0), Tick(frame.0 * 3)] {
        let want = crate::paint::apply(&paint, t, frame, 1.0, &sa.px, &[], Some(&sb.px), W, H);
        let got = at(t);
        assert_eq!(max_diff(&got, &Image { px: want, ..Image::new(W, H) }), 0.0, "t {t:?}");
    }
    // the reveal stroke shows only on its frame (the cache must not keep a painted picture)
    assert!(max_diff(&at(Tick(0)), &at(Tick(frame.0 * 3))) > 0.1);
    // nothing active: no pass, the input passes
    let late = comp(
        vec![
            solid(2, a),
            node(4, CompKind::Paint { paint: Paint { strokes: vec![stroke(PaintTool::Color, StrokeDuration::SingleFrame { at: Tick(1 << 40) })] } }, &[]),
        ],
        &[(2, 4, 0), (4, 1, 0)],
    );
    let p = compile(&late, ctx(), &NoSources).unwrap();
    assert!(!p.passes.iter().any(|x| matches!(x, Pass::Paint { .. })));
    assert_eq!(max_diff(&execute_cpu(&p), &sa), 0.0);
    // a paint node without its input paints over transparency
    let alone = comp(vec![node(4, CompKind::Paint { paint }, &[])], &[(4, 1, 0)]);
    let img = run(&alone);
    let mid = (9 * W + 16) * 4;
    assert!(img.px[mid] > 0.9 && img.px[mid + 3] > 0.9 && img.px[3] == 0.0);
}

/// Review fix guard: a keyframed roto shape is never served from the static-subgraph cache at
/// another time (its JSON holds `keyframes`), and a comp rendered at half size rasterises the
/// shapes at half scale (px_scale), so the cached path and the plain compile agree at every time
/// and size.
#[test]
fn keyframed_roto_follows_time_through_the_cache_and_scales_with_the_render_size() {
    use filmcraft_project::comp::RotoOutput;
    use filmcraft_project::roto::{Roto, RotoKind, RotoShape};
    let mut shape = RotoShape::new("k", RotoKind::Bezier, filmcraft_project::MaskPath::rect(2.0, 2.0, 10.0, 10.0));
    shape.put_path_keyframe(Tick(0), filmcraft_project::MaskPath::rect(2.0, 2.0, 10.0, 10.0));
    shape.put_path_keyframe(Tick(1000), filmcraft_project::MaskPath::rect(18.0, 6.0, 30.0, 16.0));
    let c = comp(vec![node(3, CompKind::Roto { roto: Roto { shapes: vec![shape] }, output: RotoOutput::Matte }, &[])], &[(3, 1, 0)]);
    let at = |t: Tick, size: (usize, usize)| {
        let cx = CompCtx { t, size, ..ctx() };
        (render_cpu(&c, cx, &NoSources), execute_cpu(&compile(&c, cx, &NoSources).unwrap()))
    };
    let (a0, p0) = at(Tick(0), (W, H));
    let (a1, p1) = at(Tick(1000), (W, H));
    assert_eq!(max_diff(&a0, &p0), 0.0);
    assert_eq!(max_diff(&a1, &p1), 0.0, "the cache must not keep the shape of another time");
    assert!(max_diff(&a0, &a1) > 0.5, "the keyframed shape moved");
    // half size: (6, 6) full-size pixels is (3, 3) at half size, inside at t 0
    let (h0, q0) = at(Tick(0), (W / 2, H / 2));
    assert_eq!(max_diff(&h0, &q0), 0.0);
    assert_eq!(h0.px[(3 * (W / 2) + 3) * 4], 1.0, "inside at half size");
    assert_eq!(h0.px[(7 * (W / 2) + 7) * 4], 0.0, "outside at half size (full-size (14, 14))");
}

/// C3.10: a moving picture (red = frame / 10, green = x / W), there for frames 0..10 only.
struct Moving;

impl CompSources for Moving {
    fn media_in(&self, _: &MediaInSource, t: Tick, w: usize, h: usize) -> Option<Image> {
        let fd = FrameRate::FPS_24.frame_duration().0;
        let f = t.0.checked_div(fd).filter(|f| (0..10).contains(f) && t.0 >= 0)?;
        Some(Image { w, h, px: (0..w * h).flat_map(|i| [f as f32 / 10.0, (i % w) as f32 / w as f32, 0.5, 1.0]).collect() })
    }
}

/// A hard 1 px Clone stroke along row `y` taken `dt` later (it covers that row only).
fn clone_row(y: usize, dt: Tick) -> filmcraft_project::paint::PaintStroke {
    use filmcraft_project::paint::{Brush, PaintPoint, PaintStroke, PaintTool};
    let yc = y as f64 + 0.5;
    let pts = vec![PaintPoint::new(2.0, yc), PaintPoint::new(30.0, yc)];
    PaintStroke { clone_time_offset: dt, ..PaintStroke::new(PaintTool::Clone, pts, Brush { size: 1.0, hardness: 1.0, opacity: 1.0, spacing: 0.1 }) }
}

fn paint_comp(strokes: Vec<filmcraft_project::paint::PaintStroke>) -> Comp {
    let m = CompNode::new(2, CompKind::MediaIn { source: MediaInSource::Item { item: ItemId(1) }, offset: Tick(0) });
    comp(vec![m, node(3, CompKind::Paint { paint: filmcraft_project::paint::Paint { strokes } }, &[])], &[(2, 3, 0), (3, 1, 0)])
}

#[test]
fn clone_strokes_copy_from_their_time_offset() {
    let fd = FrameRate::FPS_24.frame_duration();
    let frames = |n: i64| Tick(fd.0 * n);
    let t = frames(5);
    let red = |img: &Image, y: usize| img.px[(y * W + 16) * 4];
    let rows = [1usize, 4, 7, 10, 13, 16];
    // −1, +2 frames, 0, two more, then one more offset than the cap allows: it falls back
    let dts = [frames(-1), frames(2), Tick::ZERO, frames(-2), frames(1), frames(3)];
    let strokes: Vec<_> = rows.iter().zip(dts).map(|(y, dt)| clone_row(*y, dt)).collect();
    let c = paint_comp(strokes);
    let p = compile(&c, CompCtx { t, ..ctx() }, &Moving).unwrap();
    let Some(Pass::Paint { clones, .. }) = p.passes.iter().find(|x| matches!(x, Pass::Paint { .. })) else { panic!("a paint pass") };
    assert_eq!(clones.iter().map(|c| c.0).collect::<Vec<_>>(), vec![frames(-1), frames(2), frames(-2), frames(1)]);
    let out = execute_cpu(&p);
    let eps = 1e-6;
    assert!((red(&out, 1) - 0.4).abs() < eps, "the frame before: {}", red(&out, 1));
    assert!((red(&out, 4) - 0.7).abs() < eps, "two frames later");
    assert!((red(&out, 7) - 0.5).abs() < eps, "offset 0: the same frame");
    assert!((red(&out, 10) - 0.3).abs() < eps && (red(&out, 13) - 0.6).abs() < eps);
    assert!((red(&out, 16) - 0.5).abs() < eps, "past the cap: the same frame");
    assert!((red(&out, 0) - 0.5).abs() < eps && (red(&out, 2) - 0.5).abs() < eps, "outside the strokes");
    // the same through the cached compile
    assert_eq!(max_diff(&render_cpu(&c, CompCtx { t, ..ctx() }, &Moving), &out), 0.0);
    // a source time past the media's end: what Media In gives there (transparent)
    let past = paint_comp(vec![clone_row(4, frames(9))]);
    let out = render_cpu(&past, CompCtx { t, ..ctx() }, &Moving);
    assert_eq!(&out.px[(4 * W + 16) * 4..(4 * W + 16) * 4 + 4], &[0.0; 4]);
    // offset 0 everywhere: no clone source, the stroke copies the same frame (unchanged picture)
    let zero = paint_comp(vec![clone_row(4, Tick::ZERO)]);
    let p = compile(&zero, CompCtx { t, ..ctx() }, &Moving).unwrap();
    assert!(p.passes.iter().all(|x| !matches!(x, Pass::Paint { clones, .. } if !clones.is_empty())));
    assert_eq!(max_diff(&execute_cpu(&p), &Moving.media_in(&MediaInSource::Item { item: ItemId(1) }, t, W, H).unwrap()), 0.0);
}

#[test]
fn hostile_and_nested_clone_offsets_stay_bounded() {
    let fd = FrameRate::FPS_24.frame_duration();
    // huge offsets saturate (no source there: transparent), never panic
    for dt in [i64::MIN, i64::MAX, i64::MIN + 1, -1, 1] {
        for t in [Tick(i64::MIN), Tick(0), Tick(i64::MAX), Tick(fd.0 * 5)] {
            let c = paint_comp(vec![clone_row(4, Tick(dt)), clone_row(8, Tick(dt / 2))]);
            let img = render_cpu(&c, CompCtx { t, ..ctx() }, &Moving);
            assert!(img.px.iter().all(|v| v.is_finite()));
        }
    }
    // a chain of Paint nodes, each cloning from four other times: depth and compile counts are
    // capped, the slots stay within the program limit and the comp still renders
    let m = CompNode::new(2, CompKind::MediaIn { source: MediaInSource::Item { item: ItemId(1) }, offset: Tick(0) });
    let mut nodes = vec![m];
    let mut edges = Vec::new();
    let strokes: Vec<_> = (1..=4).map(|k| clone_row(k * 3, Tick(fd.0 * (k as i64 - 3)))).collect();
    for i in 0..8u64 {
        let id = 10 + i;
        nodes.push(node(id, CompKind::Paint { paint: filmcraft_project::paint::Paint { strokes: strokes.clone() } }, &[]));
        edges.push((if i == 0 { 2 } else { id - 1 }, id, 0));
    }
    edges.push((17, 1, 0));
    let c = comp(nodes, &edges);
    let t0 = std::time::Instant::now();
    let p = compile(&c, CompCtx { t: Tick(fd.0 * 5), ..ctx() }, &Moving).unwrap();
    assert!(p.textures <= MAX_LIVE_TEXTURES);
    let n_paint = p.passes.iter().filter(|x| matches!(x, Pass::Paint { .. })).count();
    // 8 paint nodes at the top, plus at most MAX_CLONE_COMPILES sub-graphs of at most 8 each
    assert!(n_paint <= 8 + MAX_CLONE_COMPILES as usize * 8, "{n_paint}");
    assert!(p.passes.iter().any(|x| matches!(x, Pass::Paint { clones, .. } if !clones.is_empty())));
    let img = execute_cpu(&p);
    assert!(img.px.iter().all(|v| v.is_finite()));
    assert!(t0.elapsed().as_secs_f64() < 10.0);
}

/// Review fix (C3.10): a comp whose Media In is the comp itself (a hostile project file; the
/// engine refuses it) nested down to the render's nest limit, cloning from four other times at
/// every level: the clone budget shrinks with the nesting, so the renders stay a few hundred
/// instead of (1 + 4)^8.
struct SelfNested<'a> {
    comp: &'a Comp,
    depth: u32,
    calls: &'a std::sync::atomic::AtomicUsize,
}

impl CompSources for SelfNested<'_> {
    fn nest_depth(&self) -> u32 {
        self.depth
    }
    fn media_in(&self, _: &MediaInSource, t: Tick, w: usize, h: usize) -> Option<Image> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.depth >= crate::MAX_NEST_DEPTH {
            return Moving.media_in(&MediaInSource::Item { item: ItemId(1) }, t, w, h);
        }
        let inner = SelfNested { depth: self.depth + 1, ..*self };
        Some(render_cpu(self.comp, CompCtx { t, size: (w, h), ..ctx() }, &inner))
    }
}

#[test]
fn clone_sources_of_comps_nested_in_themselves_stay_bounded() {
    let fd = FrameRate::FPS_24.frame_duration();
    let strokes: Vec<_> = (1..=4).map(|k| clone_row(k * 3, Tick(fd.0 * k as i64))).collect();
    let c = paint_comp(strokes);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let t0 = std::time::Instant::now();
    let img = render_cpu(&c, CompCtx { t: Tick(fd.0 * 5), ..ctx() }, &SelfNested { comp: &c, depth: 0, calls: &calls });
    let n = calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(img.px.iter().all(|v| v.is_finite()));
    // Media In renders per level: 1 + 4 at depths 0 and 1, 1 + 1 at depth 2, then 1 each (5 +
    // 25 + 50 · 7 = 380; without the shrinking budget 5 + 25 + … + 5^9, about 2.4 million)
    assert!(n <= 380, "{n} Media In renders");
    assert!(t0.elapsed().as_secs_f64() < 10.0);
    // the top level still clones from its four times
    let p = compile(&c, CompCtx { t: Tick(fd.0 * 5), ..ctx() }, &Moving).unwrap();
    assert!(p.passes.iter().any(|x| matches!(x, Pass::Paint { clones, .. } if clones.len() == 4)));
}

// -- C4.2: parameter expressions

fn with_expr(mut n: CompNode, key: &str, value: ParamValue, expr: &str) -> CompNode {
    let mut p = Param::new(value);
    p.expression = Some(expr.into());
    n.params.insert(key.into(), p);
    n
}

fn at(c: &Comp, t: Tick, cached: bool) -> Image {
    let cx = CompCtx { t, ..ctx() };
    let p = if cached { compile_cached(c, cx, &NoSources) } else { compile(c, cx, &NoSources) };
    execute_cpu(&p.unwrap())
}

/// A Solid's colour and a Transform's position driven by `time * 10` change between two times; the
/// static-subgraph cache doesn't hold them still.
#[test]
fn expressions_drive_node_parameters_over_time() {
    let tick = Tick::from_seconds_f64;
    let red = with_expr(CompNode::new(2, CompKind::Solid), "color", ParamValue::Color([0.0, 0.0, 0.0, 1.0]), "[time * 10, 0, 0]");
    let c = comp(vec![red], &[(2, 1, 0)]);
    assert_eq!(max_diff(&at(&c, tick(0.0), false), &solid_image([0.0, 0.0, 0.0, 1.0])), 0.0);
    assert!(max_diff(&at(&c, tick(0.05), false), &solid_image([0.5, 0.0, 0.0, 1.0])) < 1e-6);

    // Solid → Transform (x = time * 10 px) → Blur → Out: the Blur subgraph would be cached
    // were the expression not time-varying
    let white = node(2, CompKind::Solid, &[("color", ParamValue::Color([1.0, 1.0, 1.0, 1.0]))]);
    let mover = with_expr(CompNode::new(3, CompKind::Transform), "position", ParamValue::Vec2(Vec2::new(f64::NAN, f64::NAN)), "value + [time * 10, 0]");
    let blur = node(4, CompKind::Blur, &[("blurriness", ParamValue::Float(2.0))]);
    let c = comp(vec![white, mover, blur], &[(2, 3, 0), (3, 4, 0), (4, 1, 0)]);
    assert!(static_tops(&c, &c.validate().unwrap()).is_empty(), "time-varying: nothing static");
    let (a, b) = (at(&c, tick(0.0), true), at(&c, tick(1.0), true));
    assert!(max_diff(&a, &b) > 0.1, "moved by 10 px");
    assert_eq!(max_diff(&a, &at(&c, tick(0.0), false)), 0.0);
    assert_eq!(max_diff(&b, &at(&c, tick(1.0), false)), 0.0);
    // a constant expression is static (and cached) and renders as its value
    let mut fixed = c.clone();
    fixed.nodes[2] = with_expr(CompNode::new(3, CompKind::Transform), "position", ParamValue::Vec2(Vec2::ZERO), "[16 + 5, 9]");
    assert_eq!(static_tops(&fixed, &fixed.validate().unwrap()).len(), 1);
    let moved = at(&fixed, tick(0.0), true);
    assert_eq!(max_diff(&moved, &at(&fixed, tick(3.0), true)), 0.0);
    assert_eq!(max_diff(&moved, &at(&c, tick(0.5), false)), 0.0, "x + 5 is time 0.5 of the moving one");
}

/// A clone source (the Paint node's input at another time) evaluates its expressions against
/// the whole comp at that time: a reference to a node outside the input's subgraph works.
#[test]
fn clone_sources_read_references_outside_their_subgraph() {
    let fd = FrameRate::FPS_24.frame_duration();
    let frames = |n: i64| Tick(fd.0 * n);
    // "Ref" (not wired to anything) holds the value; the Solid reads it
    let reference = CompNode { label: "Ref".into(), ..with_expr(CompNode::new(5, CompKind::Merge), "opacity", ParamValue::Float(0.0), "frame * 10") };
    let src = with_expr(CompNode::new(2, CompKind::Solid), "color", ParamValue::Color([0.0, 0.0, 0.0, 1.0]), "[node(\"Ref\").opacity / 100, 0, 0]");
    let paint = node(3, CompKind::Paint { paint: filmcraft_project::paint::Paint { strokes: vec![clone_row(4, frames(2))] } }, &[]);
    let c = comp(vec![src, paint, reference], &[(2, 3, 0), (3, 1, 0)]);
    let out = render_cpu(&c, CompCtx { t: frames(5), ..ctx() }, &NoSources);
    let red = |img: &Image, y: usize| img.px[(y * W + 16) * 4];
    let want = |f: f32| solid_image([f / 10.0, 0.0, 0.0, 1.0]).px[0];
    assert!((red(&out, 4) - want(7.0)).abs() < 1e-6, "the clone row is frame 7's: {}", red(&out, 4));
    assert!((red(&out, 0) - want(5.0)).abs() < 1e-6, "elsewhere frame 5's");
}

/// B reads A's colour: when A's keyframes change, B's picture does, through the static cache.
#[test]
fn a_reference_follows_its_node_through_the_cache() {
    let keyed = |c: [f32; 4]| {
        let mut p = Param::new(ParamValue::Color(c));
        p.keyframes = vec![filmcraft_project::Keyframe::new(Tick(0), ParamValue::Color(c)), filmcraft_project::Keyframe::new(Tick(100), ParamValue::Color(c))];
        CompNode { label: "A".into(), params: [("color".to_string(), p)].into(), ..CompNode::new(5, CompKind::Solid) }
    };
    let b = with_expr(CompNode::new(2, CompKind::Solid), "color", ParamValue::Color([0.0; 4]), "node(\"A\").color");
    let blur = node(3, CompKind::Blur, &[("blurriness", ParamValue::Float(2.0))]);
    let mut c = comp(vec![b, blur, keyed([1.0, 0.0, 0.0, 1.0])], &[(2, 3, 0), (3, 1, 0)]);
    assert!(static_tops(&c, &c.validate().unwrap()).is_empty(), "B reads another node: not static");
    // what B gives with A's colour set straight on it
    let direct = |col: [f32; 4]| {
        at(&comp(vec![solid(2, col), node(3, CompKind::Blur, &[("blurriness", ParamValue::Float(2.0))])], &[(2, 3, 0), (3, 1, 0)]), Tick(0), false)
    };
    let red = at(&c, Tick(0), true);
    assert_eq!(max_diff(&red, &direct([1.0, 0.0, 0.0, 1.0])), 0.0);
    c.nodes[3] = keyed([0.0, 1.0, 0.0, 1.0]);
    let green = at(&c, Tick(0), true);
    assert_eq!(max_diff(&green, &direct([0.0, 1.0, 0.0, 1.0])), 0.0);
    assert!(max_diff(&red, &green) > 0.5);
    // fps / width / height in an expression keep a node out of the cache too (they're not in its key)
    let sized = with_expr(CompNode::new(2, CompKind::Solid), "color", ParamValue::Color([0.0; 4]), "[width / 64, 0, 0]");
    let c = comp(vec![sized, node(3, CompKind::Blur, &[("blurriness", ParamValue::Float(2.0))])], &[(2, 3, 0), (3, 1, 0)]);
    assert!(static_tops(&c, &c.validate().unwrap()).is_empty());
}

/// A broken expression renders as the keyframed value, never an error.
#[test]
fn a_broken_expression_renders_the_keyframed_value() {
    let plain = comp(vec![solid(2, [0.2, 0.4, 0.6, 1.0])], &[(2, 1, 0)]);
    for bad in ["sin(time", "1/0", "node(\"nope\").color", "self.color", "🙂", "[1, 2]"] {
        let broken = comp(vec![with_expr(CompNode::new(2, CompKind::Solid), "color", ParamValue::Color([0.2, 0.4, 0.6, 1.0]), bad)], &[(2, 1, 0)]);
        for cached in [false, true] {
            assert_eq!(max_diff(&at(&broken, Tick(0), cached), &at(&plain, Tick(0), cached)), 0.0, "{bad}");
        }
    }
}
