//! Tracker / Planar Tracker nodes (C3.5a): the stored tracks become the solves' transforms, drawn
//! with the passes the GPU already runs where the map is affine.

use super::*;
use crate::comp::{CompCtx, NoSources, Pass, compile, execute_cpu};
use filmcraft_project::comp::{Comp, CompNode};
use filmcraft_project::dag::{Edge, NodeId, Port};
use filmcraft_project::{Keyframe, MaskPath, ParamValue};

const W: usize = 48;
const H: usize = 32;
const RATE: FrameRate = FrameRate::FPS_24;

fn ctx(t: Tick) -> CompCtx<'static> {
    CompCtx { t, size: (W, H), project: None, high_quality: false }
}

fn comp(nodes: Vec<CompNode>, edges: &[(NodeId, NodeId, u8)]) -> Comp {
    let mut c = Comp::new(W as u32, H as u32, RATE, RATE.tick_of(100));
    c.nodes.extend(nodes);
    c.edges = edges.iter().map(|&(f, t, p)| Edge::new(Port::new(f, 0), Port::new(t, p))).collect();
    c
}

fn gradient_node(id: NodeId) -> CompNode {
    let mut n = CompNode::new(id, CompKind::Gradient);
    n.params.insert("shape".into(), Param::new(ParamValue::Choice(1)));
    n
}

fn solid(id: NodeId, c: [f32; 4]) -> CompNode {
    let mut n = CompNode::new(id, CompKind::Solid);
    n.params.insert("color".into(), Param::new(ParamValue::Color(c)));
    n
}

fn run(c: &Comp, t: Tick) -> (Image, Vec<Pass>) {
    let p = compile(c, ctx(t), &NoSources).unwrap();
    (execute_cpu(&p), p.passes)
}

fn max_diff(a: &Image, b: &Image) -> f32 {
    a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// A track at `at(frame)` for frames 0..n.
fn track(n: i64, at: impl Fn(i64) -> Vec2) -> Param {
    let mut p = Param::new(ParamValue::Vec2(at(0)));
    p.keyframes = (0..n).map(|f| Keyframe::new(RATE.tick_of(f), ParamValue::Vec2(at(f)))).collect();
    p
}

fn point(p: Param) -> TrackerPoint {
    TrackerPoint { track: p, ..TrackerPoint::at("", Vec2::ZERO) }
}

fn tracker(points: Vec<TrackerPoint>, operation: TrackOperation, smooth_radius: u32) -> CompKind {
    CompKind::Tracker { points, operation, smooth_radius, reference: Tick::ZERO, method: TrackMethod::Position, corner_points: Vec::new() }
}

/// The picture of the comp's gradient node 2.
fn source(c: &Comp) -> Image {
    let mut v = c.clone();
    v.output = Some(2);
    run(&v, Tick::ZERO).0
}

#[test]
fn a_tracker_with_nothing_analysed_passes_its_input() {
    let c = comp(vec![gradient_node(2), CompNode::new(3, CompKind::from_key("tracker").unwrap())], &[(2, 3, 0), (3, 1, 0)]);
    assert_eq!(max_diff(&run(&c, RATE.tick_of(5)).0, &source(&c)), 0.0);
}

#[test]
fn match_move_and_stabilise_are_the_tracked_translation_drawn() {
    let path = |f: i64| Vec2::new(10.0 + 1.5 * f as f64, 8.0 + 0.5 * f as f64);
    let t = RATE.tick_of(6);
    let d = path(6) - path(0);
    for (op, m) in [(TrackOperation::MatchMove, Affine::translate(d.x, d.y)), (TrackOperation::Stabilise, Affine::translate(-d.x, -d.y))] {
        let c = comp(vec![gradient_node(2), CompNode::new(3, tracker(vec![point(track(12, path))], op, 0))], &[(2, 3, 0), (3, 1, 0)]);
        let (img, passes) = run(&c, t);
        let want = source(&c).transformed(W, H, &m);
        assert!(max_diff(&img, &want) < 1e-5, "{op:?}");
        assert!(passes.iter().all(|p| !matches!(p, Pass::Warp { .. })), "{op:?} is an affine draw (GPU-able)");
        // outside the analysed frames the nearest solve holds
        let (late, _) = run(&c, RATE.tick_of(50));
        let d = path(11) - path(0);
        let s = if op == TrackOperation::Stabilise { -1.0 } else { 1.0 };
        assert!(max_diff(&late, &source(&c).transformed(W, H, &Affine::translate(s * d.x, s * d.y))) < 1e-5);
    }
}

#[test]
fn smoothed_stabilise_keeps_a_steady_pan() {
    // a steady pan with jitter: with smoothing, only the jitter is cancelled (≈ no pan removed)
    let jitter = |f: i64| if f % 2 == 0 { 0.6 } else { -0.6 };
    let path = |f: i64| Vec2::new(10.0 + 2.0 * f as f64 + jitter(f), 12.0);
    let c = comp(vec![gradient_node(2), CompNode::new(3, tracker(vec![point(track(40, path))], TrackOperation::Stabilise, 6))], &[(2, 3, 0), (3, 1, 0)]);
    let plan = plan(&c.node(3).unwrap().kind, RATE, RATE.tick_of(20), (W as f64, H as f64), false).unwrap();
    let TrackWarp::Affine(m) = plan.warp else { panic!("affine") };
    // frame 20 has +0.6 jitter: the stabilised picture moves back by about that, not by the pan
    assert!((m.e + 0.6).abs() < 0.2, "{m:?}");
}

#[test]
fn a_foreground_follows_the_track_over_the_background() {
    let path = |f: i64| Vec2::new(4.0 + 2.0 * f as f64, 4.0);
    let fg = {
        let mut n = CompNode::new(4, CompKind::Mask { masks: vec![filmcraft_project::Mask::new("m", MaskPath::rect(0.0, 0.0, 8.0, 8.0))] });
        n.label = "box".into();
        n
    };
    let c = comp(
        vec![
            solid(2, [0.0, 0.0, 1.0, 1.0]),
            solid(5, [1.0, 0.0, 0.0, 1.0]),
            fg,
            CompNode::new(3, tracker(vec![point(track(10, path))], TrackOperation::MatchMove, 0)),
        ],
        &[(2, 3, 0), (5, 4, 0), (4, 3, 1), (3, 1, 0)],
    );
    let (img, _) = run(&c, RATE.tick_of(5));
    let px = |x: usize, y: usize| img.get(x, y);
    // the 8×8 red box moved 10 px right, over the blue background
    assert!(px(14, 4)[0] > 0.9 && px(14, 4)[2] < 0.1, "{:?}", px(14, 4));
    assert!(px(4, 4)[2] > 0.9, "background where the box was: {:?}", px(4, 4));
}

/// The plane's corners at frame `f`: a quad drifting and tilting.
fn plane(f: i64) -> [Vec2; 4] {
    let k = f as f64;
    [Vec2::new(10.0 + k, 6.0), Vec2::new(34.0 + k, 7.0 + 0.2 * k), Vec2::new(36.0 + k, 26.0), Vec2::new(8.0 + k, 24.0 - 0.1 * k)]
}

fn planar(op: TrackOperation, frames: i64) -> CompKind {
    let corners = (0..4).map(|i| track(frames, |f| plane(f)[i])).collect();
    CompKind::PlanarTracker {
        region: MaskPath { vertices: plane(0).iter().map(|p| filmcraft_project::MaskVertex::corner(*p)).collect(), closed: true },
        quad: plane(0),
        reference: Tick::ZERO,
        corners,
        confidence: Vec::new(),
        operation: op,
    }
}

#[test]
fn planar_corner_pin_puts_the_insert_on_the_plane() {
    let c = comp(
        vec![solid(2, [0.0, 0.0, 1.0, 1.0]), solid(4, [1.0, 0.0, 0.0, 1.0]), CompNode::new(3, planar(TrackOperation::CornerPin, 10))],
        &[(2, 3, 0), (4, 3, 1), (3, 1, 0)],
    );
    let f = 7;
    let (img, passes) = run(&c, RATE.tick_of(f));
    assert!(passes.iter().any(|p| matches!(p, Pass::Warp { .. })), "a perspective pin is a projective draw");
    let q = plane(f);
    let inside = |p: Vec2| {
        (0..4).all(|i| {
            let (a, b) = (q[i], q[(i + 1) % 4]);
            (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x) > 1.5
        })
    };
    let mut checked = 0;
    for y in 0..H {
        for x in 0..W {
            let c = Vec2::new(x as f64 + 0.5, y as f64 + 0.5);
            let px = img.get(x, y);
            if inside(c) && !near_edge(&q, c) {
                assert!(px[0] > 0.95 && px[2] < 0.05, "insert at {x},{y}: {px:?}");
                checked += 1;
            } else if !inside(c) && !near_edge(&q, c) {
                assert!(px[2] > 0.95 && px[0] < 0.05, "background at {x},{y}: {px:?}");
            }
        }
    }
    assert!(checked > 200, "{checked}");
}

fn near_edge(q: &[Vec2; 4], p: Vec2) -> bool {
    (0..4).any(|i| {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let ab = b - a;
        let u = (((p - a).x * ab.x + (p - a).y * ab.y) / (ab.x * ab.x + ab.y * ab.y)).clamp(0.0, 1.0);
        (a + ab * u - p).length() < 1.5
    })
}

#[test]
fn planar_stabilise_and_match_move_invert_each_other() {
    let size = (W as f64, H as f64);
    let t = RATE.tick_of(5);
    let mm = plan(&planar(TrackOperation::MatchMove, 10), RATE, t, size, true).unwrap();
    let st = plan(&planar(TrackOperation::Stabilise, 10), RATE, t, size, true).unwrap();
    assert!(mm.foreground && !st.foreground, "stabilise moves the background");
    let (TrackWarp::Projective(a), TrackWarp::Projective(b)) = (mm.warp, st.warp) else { panic!("projective") };
    for (p, q) in plane(0).iter().zip(plane(5)) {
        assert!((a.apply(*p).unwrap() - q).length() < 1e-6);
        assert!((b.apply(q).unwrap() - *p).length() < 1e-6);
    }
    // a plane that only slides is an affine draw
    let mut slide = planar(TrackOperation::MatchMove, 10);
    if let CompKind::PlanarTracker { corners, .. } = &mut slide {
        *corners = (0..4).map(|i| track(10, |f| plane(0)[i] + Vec2::new(f as f64, 0.0))).collect();
    }
    let p = plan(&slide, RATE, t, size, true).unwrap();
    assert!(matches!(p.warp, TrackWarp::Affine(m) if (m.e - 5.0).abs() < 1e-6), "{p:?}");
}

#[test]
fn tracker_corner_pin_uses_four_points() {
    let quad = |f: i64| plane(f);
    let pts = (0..4).map(|i| point(track(10, move |f| quad(f)[i]))).collect();
    let p = plan(&tracker(pts, TrackOperation::CornerPin, 0), RATE, RATE.tick_of(3), (W as f64, H as f64), false).unwrap();
    let TrackWarp::Projective(h) = p.warp else { panic!() };
    for (c, q) in frame_quad(W as f64, H as f64).iter().zip(quad(3)) {
        assert!((h.apply(*c).unwrap() - q).length() < 1e-6);
    }
    // three points can't pin: identity
    let pts = (0..3).map(|i| point(track(10, move |f| quad(f)[i]))).collect();
    let p = plan(&tracker(pts, TrackOperation::CornerPin, 0), RATE, RATE.tick_of(3), (W as f64, H as f64), false).unwrap();
    assert_eq!(p.warp, TrackWarp::Affine(Affine::IDENTITY));
}

/// LO.5: Corner Pin onto four chosen points of six (in corner order), not the first four; an
/// invalid choice falls back to the first four; a chosen point that isn't analysed: identity.
#[test]
fn tracker_corner_pin_uses_the_chosen_points() {
    let quad = |f: i64| plane(f);
    let off = |f: i64| quad(f)[0] + Vec2::new(5.0, 3.0);
    // points: 0, 1 = decoys; 2..6 = the plane's corners in the order br, tl, bl, tr
    let mut pts: Vec<TrackerPoint> = vec![point(track(10, off)), point(track(10, move |f| off(f) + Vec2::new(9.0, 0.0)))];
    for i in [2, 0, 3, 1] {
        pts.push(point(track(10, move |f| quad(f)[i])));
    }
    let size = (W as f64, H as f64);
    let mut kind = tracker(pts, TrackOperation::CornerPin, 0);
    if let CompKind::Tracker { corner_points, .. } = &mut kind {
        *corner_points = vec![3, 5, 2, 4];
    }
    let p = plan(&kind, RATE, RATE.tick_of(3), size, false).unwrap();
    let TrackWarp::Projective(h) = p.warp else { panic!("{p:?}") };
    for (c, q) in frame_quad(size.0, size.1).iter().zip(quad(3)) {
        assert!((h.apply(*c).unwrap() - q).length() < 1e-6);
    }
    // an invalid choice: the first four (decoys included) still pin, differently
    if let CompKind::Tracker { corner_points, .. } = &mut kind {
        *corner_points = vec![3, 5, 2, 2];
    }
    let p = plan(&kind, RATE, RATE.tick_of(3), size, false).unwrap();
    assert!(matches!(p.warp, TrackWarp::Projective(g) if g != h) || p.warp == TrackWarp::Affine(Affine::IDENTITY), "{p:?}");
    // a chosen point without analysis: no pin
    if let CompKind::Tracker { points, corner_points, .. } = &mut kind {
        *corner_points = vec![3, 5, 2, 4];
        points[4].track.keyframes.clear();
    }
    let p = plan(&kind, RATE, RATE.tick_of(3), size, false).unwrap();
    assert_eq!(p.warp, TrackWarp::Affine(Affine::IDENTITY));
}

#[test]
fn the_projective_draw_matches_the_affine_draw() {
    let c = comp(vec![gradient_node(2)], &[(2, 1, 0)]);
    let src = source(&c);
    let m = Affine::motion(Vec2::new(20.0, 14.0), Vec2::new(0.8, 0.8), 20.0, Vec2::new(24.0, 16.0));
    let a = src.transformed(W, H, &m);
    let b = warp_projective(&src, W, H, &homography_of(&m));
    assert!(max_diff(&a, &b) < 1e-4, "{}", max_diff(&a, &b));
    // scaled to a half-size program: the same map in program pixels
    let w = TrackWarp::Affine(Affine::translate(4.0, 2.0)).scaled(0.5);
    assert_eq!(w, TrackWarp::Affine(Affine::translate(2.0, 1.0)));
    let TrackWarp::Projective(h) = TrackWarp::Projective(homography_of(&Affine::translate(4.0, 2.0))).scaled(0.5) else { panic!() };
    assert!((h.apply(Vec2::new(1.0, 1.0)).unwrap() - Vec2::new(3.0, 2.0)).length() < 1e-9);
    // a singular map draws nothing; a strongly minifying one takes the half-size copy
    assert!(warp_projective(&src, W, H, &Homography { m: [0.0; 9] }).px.iter().all(|v| *v == 0.0));
    let small = warp_projective(&src, W, H, &homography_of(&Affine::scale(0.2, 0.2)));
    assert_eq!((small.w, small.h), (W, H));
}

#[test]
fn hostile_tracks_never_panic() {
    let nan = Vec2::new(f64::NAN, f64::INFINITY);
    let mut bad = track(5, |_| nan);
    bad.keyframes.push(Keyframe::new(Tick(i64::MIN), ParamValue::Float(1.0)));
    bad.keyframes.push(Keyframe::new(Tick(i64::MAX), ParamValue::Text("x".into())));
    let kinds = [
        tracker(vec![point(bad.clone()); 6], TrackOperation::MatchMove, 0),
        tracker(vec![point(bad.clone()); 6], TrackOperation::Stabilise, u32::MAX),
        tracker(vec![point(bad.clone()); 6], TrackOperation::CornerPin, 0),
        CompKind::PlanarTracker {
            region: MaskPath::default(),
            quad: [nan; 4],
            reference: Tick(i64::MIN),
            corners: vec![bad.clone(); 4],
            confidence: Vec::new(),
            operation: TrackOperation::CornerPin,
        },
        CompKind::PlanarTracker {
            region: MaskPath::default(),
            quad: [Vec2::ZERO; 4],
            reference: Tick(i64::MAX),
            corners: vec![track(3, |_| Vec2::new(5.0, 5.0)); 4],
            confidence: Vec::new(),
            operation: TrackOperation::MatchMove,
        },
    ];
    for kind in kinds {
        for t in [Tick(i64::MIN), Tick::ZERO, RATE.tick_of(3), Tick(i64::MAX)] {
            let _ = plan(&kind, RATE, t, (W as f64, H as f64), true);
            let mut n = CompNode::new(3, kind.clone());
            n.bypass = false;
            let c = comp(vec![gradient_node(2), solid(4, [1.0; 4]), n], &[(2, 3, 0), (4, 3, 1), (3, 1, 0)]);
            let _ = run(&c, t);
        }
    }
    assert_eq!(reference_quad(&[nan; 4], &[], (4.0, 2.0)), frame_quad(4.0, 2.0));
}

// -- C3.7: roto shapes following a tracker ---------------------------------------------------

const LW: usize = 160;
const LH: usize = 120;

/// The rigid motion of the linked-roto tests at frame `f`: a turn about the frame centre and a drift.
fn rigid(f: i64) -> Affine {
    let k = f as f64;
    let c = Vec2::new(80.0, 60.0);
    Affine::translate(c.x + 1.3 * k, c.y + 0.7 * k).then_apply(&Affine::rotate_deg(0.7 * k)).then_apply(&Affine::translate(-c.x, -c.y))
}

/// A LW×LH comp: node 3 = `tracker`, node 4 = a Roto Matte with one circle linked to node
/// `link` (output `output`) → Media Out.
fn linked_comp(tracker: CompKind, link: &str, output: &str) -> Comp {
    use filmcraft_project::roto::{Roto, RotoKind, RotoShape, TrackLink};
    let mut shape = RotoShape::new("c", RotoKind::Bezier, MaskPath::ellipse(Vec2::new(70.0, 55.0), Vec2::new(14.0, 14.0)));
    shape.track_link = Some(TrackLink { tracker: link.into(), output: output.into() });
    let roto = CompKind::Roto { roto: Roto { shapes: vec![shape] }, output: filmcraft_project::comp::RotoOutput::Matte };
    let mut c = Comp::new(LW as u32, LH as u32, RATE, RATE.tick_of(100));
    c.nodes.extend([solid(2, [1.0; 4]), CompNode::new(3, tracker), CompNode::new(4, roto)]);
    c.edges = vec![Edge::new(Port::new(2, 0), Port::new(3, 0)), Edge::new(Port::new(4, 0), Port::new(1, 0))];
    c
}

/// The coverage centroid of a matte picture.
fn matte_centroid(img: &Image) -> Vec2 {
    let (mut s, mut x, mut y) = (0.0, 0.0, 0.0);
    for j in 0..img.h {
        for i in 0..img.w {
            let v = f64::from(img.px[(j * img.w + i) * 4]);
            s += v;
            x += v * (i as f64 + 0.5);
            y += v * (j as f64 + 0.5);
        }
    }
    Vec2::new(x / s, y / s)
}

fn run_linked(c: &Comp, f: i64) -> Image {
    execute_cpu(&compile(c, CompCtx { t: RATE.tick_of(f), size: (LW, LH), project: None, high_quality: false }, &NoSources).unwrap())
}

fn two_point_tracker() -> CompKind {
    let a = |f: i64| rigid(f).apply(Vec2::new(40.0, 40.0));
    let b = |f: i64| rigid(f).apply(Vec2::new(120.0, 85.0));
    let mut pa = point(track(30, a));
    pa.name = "A".into();
    CompKind::Tracker {
        points: vec![pa, point(track(30, b))],
        operation: TrackOperation::Stabilise,
        smooth_radius: 0,
        reference: Tick::ZERO,
        method: TrackMethod::PositionRotation,
        corner_points: Vec::new(),
    }
}

#[test]
fn a_linked_shape_follows_the_trackers_match_move() {
    // the tracker stabilises its own picture, but a shape following it moves with the object
    let c = linked_comp(two_point_tracker(), "3", "transform");
    let mut worst = 0.0f64;
    for f in [0, 5, 12, 29, 40] {
        let truth = rigid(f.min(29)).apply(Vec2::new(70.0, 55.0));
        worst = worst.max((matte_centroid(&run_linked(&c, f)) - truth).length());
    }
    assert!(worst <= 0.5, "worst centroid error {worst:.3} px");
    // one point's output: that point's translation only
    let c = linked_comp(two_point_tracker(), "3", "A");
    let d = rigid(12).apply(Vec2::new(40.0, 40.0)) - Vec2::new(40.0, 40.0);
    let e = (matte_centroid(&run_linked(&c, 12)) - (Vec2::new(70.0, 55.0) + d)).length();
    assert!(e <= 0.5, "point output {e:.3} px");
}

#[test]
fn a_linked_shape_follows_a_planar_tracker() {
    let quad = |f: i64| {
        let k = f as f64;
        [Vec2::new(40.0 + k, 30.0), Vec2::new(120.0 + k, 32.0 + 0.3 * k), Vec2::new(124.0 + 0.5 * k, 90.0), Vec2::new(36.0 + k, 88.0 - 0.2 * k)]
    };
    let corners = (0..4).map(|i| track(20, move |f| quad(f)[i])).collect();
    let kind = CompKind::PlanarTracker {
        region: MaskPath::polygon(&quad(0)),
        quad: quad(0),
        reference: Tick::ZERO,
        corners,
        confidence: Vec::new(),
        operation: TrackOperation::CornerPin,
    };
    let c = linked_comp(kind, "3", "");
    for f in [0, 7, 19] {
        let h = fit_homography(&quad(0), &quad(f)).unwrap();
        let pts: Vec<Vec2> = MaskPath::ellipse(Vec2::new(70.0, 55.0), Vec2::new(14.0, 14.0)).flatten(0.05).iter().map(|p| h.apply(*p).unwrap()).collect();
        let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
        for i in 0..pts.len() {
            let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
            let cr = p.x * q.y - q.x * p.y;
            (a, cx, cy) = (a + cr, cx + (p.x + q.x) * cr, cy + (p.y + q.y) * cr);
        }
        let truth = Vec2::new(cx / (3.0 * a), cy / (3.0 * a));
        let e = (matte_centroid(&run_linked(&c, f)) - truth).length();
        assert!(e <= 0.5, "frame {f}: {e:.3} px");
    }
}

#[test]
fn a_dangling_link_leaves_the_shape_where_it_was_drawn() {
    let still = Vec2::new(70.0, 55.0);
    for (link, output) in [("99", "transform"), ("2", "transform"), ("x", ""), ("3", "no such point")] {
        let c = linked_comp(two_point_tracker(), link, output);
        let e = (matte_centroid(&run_linked(&c, 12)) - still).length();
        assert!(e <= 0.05, "{link}/{output}: {e:.3}");
    }
    // an unanalysed tracker too
    let c = linked_comp(CompKind::from_key("tracker").unwrap(), "3", "transform");
    assert!((matte_centroid(&run_linked(&c, 12)) - still).length() <= 0.05);
}

#[test]
fn a_linked_roto_is_not_served_from_the_static_cache() {
    let c = linked_comp(two_point_tracker(), "3", "transform");
    let at = |f: i64| CompCtx { t: RATE.tick_of(f), size: (LW, LH), project: None, high_quality: false };
    let first = execute_cpu(&crate::comp::compile_cached(&c, at(0), &NoSources).unwrap());
    let later = execute_cpu(&crate::comp::compile_cached(&c, at(12), &NoSources).unwrap());
    assert!(max_diff(&first, &later) > 0.5, "the shape moved");
    assert_eq!(max_diff(&later, &run_linked(&c, 12)), 0.0);
}

#[test]
fn path_through_maps_points_and_handles() {
    let p = MaskPath::ellipse(Vec2::new(10.0, 10.0), Vec2::new(4.0, 4.0));
    let m = Affine::translate(3.0, -2.0);
    assert_eq!(path_through(&p, TrackWarp::Affine(m)), p.transformed(&m));
    let q = path_through(&p, TrackWarp::Projective(homography_of(&m)));
    for (a, b) in q.vertices.iter().zip(&p.transformed(&m).vertices) {
        assert!((a.p - b.p).length() < 1e-9 && (a.t_in - b.t_in).length() < 1e-9 && (a.t_out - b.t_out).length() < 1e-9);
    }
    // a map sending a vertex to infinity keeps the whole path in place (no lone vertex springs back)
    let bad = Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.1, 0.0, -1.0] };
    assert_eq!(path_through(&p, TrackWarp::Projective(bad)), p);
    // a real perspective map: handles are relative vectors; mapped through their ends they agree
    // with the Jacobian at the vertex to first order (and exactly in the limit), and feathers
    // scale with the local area change
    let h = Homography { m: [1.1, 0.05, 3.0, -0.02, 0.95, -1.0, 0.002, -0.001, 1.0] };
    let mut small = p.clone();
    for v in &mut small.vertices {
        v.t_in = v.t_in * 1e-4;
        v.t_out = v.t_out * 1e-4;
        *v = v.with_feather(1.0, 2.0);
    }
    let q = path_through(&small, TrackWarp::Projective(h));
    for (a, b) in q.vertices.iter().zip(&small.vertices) {
        assert!((a.p - h.apply(b.p).unwrap()).length() < 1e-9);
        let eps = 1e-3;
        let jx = (h.apply(b.p + Vec2::new(eps, 0.0)).unwrap() - h.apply(b.p - Vec2::new(eps, 0.0)).unwrap()) * (0.5 / eps);
        let jy = (h.apply(b.p + Vec2::new(0.0, eps)).unwrap() - h.apply(b.p - Vec2::new(0.0, eps)).unwrap()) * (0.5 / eps);
        let lin = jx * b.t_out.x + jy * b.t_out.y;
        assert!((a.t_out - lin).length() <= 1e-6 * b.t_out.length().max(1e-12) + 1e-12, "{:?} vs {lin:?}", a.t_out);
        let k = (jx.x * jy.y - jx.y * jy.x).abs().sqrt();
        assert!((a.feather_out - 2.0 * k).abs() < 1e-4 && (a.feather_in - k).abs() < 1e-4);
    }
    // a handle whose end crosses the horizon falls back to the Jacobian (finite, vertex mapped)
    let mut long = p.clone();
    long.vertices[0].t_out = Vec2::new(1e6, 0.0);
    let flip = Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1e-6, 0.0, 1.0] };
    let r = path_through(&long, TrackWarp::Projective(flip));
    assert!(r.vertices.iter().all(|v| v.t_out.x.is_finite() && v.t_in.x.is_finite()));
    assert!((r.vertices[0].p - flip.apply(long.vertices[0].p).unwrap()).length() < 1e-9);
    // inverse round trips
    let w = TrackWarp::Projective(h);
    let back = path_through(&path_through(&p, w), w.inverse().unwrap());
    for (a, b) in back.vertices.iter().zip(&p.vertices) {
        assert!((a.p - b.p).length() < 1e-6);
    }
    assert!(TrackWarp::Affine(Affine::scale(0.0, 1.0)).inverse().is_none());
}
