//! Tracker / Planar Tracker comp nodes (fork C3.5a, C3's acceptance): synthetic shots moving along
//! known paths, analysed through the comp, then rendered.

use crate::Session;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::comp::CompKind;
use filmcraft_project::{ItemId, ItemKind};
use filmcraft_render::track::Homography;
use filmcraft_time::FrameRate;
use serde_json::json;
use std::sync::Arc;

const RATE: FrameRate = FrameRate::FPS_24;
const W: usize = 320;
const H: usize = 240;

fn texture(u: f64, v: f64) -> f64 {
    let s = (u * 0.21).sin() * (v * 0.17).cos() + 0.5 * ((u + v) * 0.43).sin() + 0.35 * ((u * 0.9 - v * 0.6).sin() * (v * 0.75).cos());
    let (iu, iv) = ((u / 6.0).floor() as i64, (v / 6.0).floor() as i64);
    let mut x = (iu.wrapping_mul(73_856_093) ^ iv.wrapping_mul(19_349_663)) as u64;
    x ^= x >> 13;
    x = x.wrapping_mul(0x5bd1_e995);
    let h = (x >> 40) as f64 / (1u64 << 24) as f64;
    (0.5 + 0.22 * s + 0.12 * h).clamp(0.0, 1.0)
}

/// A shot: the grey value of pixel (x, y) at frame `k` (4× supersampled).
struct Shot {
    info: filmcraft_media::MediaInfo,
    at: fn(f64, Vec2) -> f64,
}

impl filmcraft_media::MediaSource for Shot {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        let k = RATE.frame_at(req.time) as f64;
        let mut px = vec![0u8; W * H * 4];
        for (i, out) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = ((i % W) as f64, (i / W) as f64);
            let acc: f64 = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)].iter().map(|(ox, oy)| (self.at)(k, Vec2::new(x + ox, y + oy))).sum();
            let v = (acc / 4.0 * 255.0).round() as u8;
            out.copy_from_slice(&[v, v, v, 255]);
        }
        Ok(Arc::new(filmcraft_frame::VideoFrame::rgba8(W as u32, H as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

/// A session holding the shot as a media item, and a W×H comp of 48 frames reading it into node
/// 3 (`kind`) → Media Out. (session, comp, media in node, tracker node)
fn session(at: fn(f64, Vec2) -> f64, kind: &str) -> (Session, u64, u64, u64) {
    use filmcraft_media::MediaSource;
    let g = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
        W as u32,
        H as u32,
        RATE,
        RATE.tick_of(48),
    );
    let mut p = filmcraft_project::Project::new("track");
    let item = p.add_item(
        "shot",
        filmcraft_project::Label::Iris,
        ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::Generator(g.generator.clone()),
            info: g.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let mut s = Session { project: Arc::new(p), ..Default::default() };
    s.media.insert(item, Arc::new(Shot { info: g.info().clone(), at }));
    let c = s.execute("comp.new", json!({"name": "T", "width": W, "height": H, "fps": 24, "duration": RATE.tick_of(48).0})).unwrap()["comp"].as_u64().unwrap();
    let m = s.execute("comp.addNode", json!({"comp": c, "kind": "mediaIn", "item": item.0})).unwrap()["node"].as_u64().unwrap();
    let t = s.execute("comp.addNode", json!({"comp": c, "kind": kind})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": m, "to": t, "toPort": 0})).unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": t, "to": 1, "toPort": 0})).unwrap();
    (s, c, m, t)
}

fn node_kind(s: &Session, comp: u64, node: u64) -> CompKind {
    let Some(ItemKind::Comp(c)) = s.project.item(ItemId(comp)).map(|i| &i.kind) else { panic!("comp") };
    c.node(node).unwrap().kind.clone()
}

/// The pan of the point test (comp pixels at frame `k`).
fn pan(k: f64) -> Vec2 {
    Vec2::new(1.7 * k + 0.3 * (k * 0.4).sin(), 0.9 * k - 0.2 * k * k / 24.0)
}

fn panning(k: f64, p: Vec2) -> f64 {
    let q = p - pan(k);
    texture(q.x + 50.0, q.y + 70.0)
}

#[test]
fn analysed_points_follow_the_known_path() {
    let (mut s, c, _, t) = session(panning, "tracker");
    let starts = [Vec2::new(100.3, 90.7), Vec2::new(180.6, 120.2), Vec2::new(140.0, 60.5)];
    let points: Vec<_> = starts.iter().enumerate().map(|(i, p)| json!({"name": format!("P{i}"), "at": [p.x, p.y], "pattern": 12, "search": 16})).collect();
    s.execute("comp.track.setPoints", json!({"comp": c, "node": t, "points": points})).unwrap();
    let undo = s.history.undo.len();
    let r = s.execute("comp.track.analyse", json!({"comp": c, "node": t, "to": RATE.tick_of(30).0, "wait": true})).unwrap();
    assert_eq!(r["frames"], 30, "{r}");
    assert!(s.comp_track_jobs.is_empty(), "the job was written");
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    let CompKind::Tracker { points, .. } = node_kind(&s, c, t) else { panic!() };
    let mut worst = 0.0f64;
    for (pt, p0) in points.iter().zip(starts) {
        assert_eq!(pt.track.keyframes.len(), 31, "every frame keyed");
        assert_eq!(pt.confidence.len(), 31);
        for k in &pt.track.keyframes {
            let f = RATE.frame_at(k.time) as f64;
            let truth = p0 + pan(f) - pan(0.0);
            let got = k.value.as_vec2().unwrap();
            worst = worst.max((got - truth).length());
        }
    }
    eprintln!("comp tracker: worst point error {worst:.3} px over 30 frames");
    assert!(worst <= 0.5, "worst {worst}");
    // the Match Move it gives at a late frame is the pan since the reference
    let Some(ItemKind::Comp(comp)) = s.project.item(ItemId(c)).map(|i| &i.kind) else { panic!() };
    let plan = filmcraft_render::comp::track::plan(&node_kind(&s, c, t), RATE, RATE.tick_of(25), (W as f64, H as f64), false).unwrap();
    let filmcraft_render::comp::track::TrackWarp::Affine(m) = plan.warp else { panic!() };
    let d = pan(25.0) - pan(0.0);
    assert!((m.e - d.x).abs() < 0.5 && (m.f - d.y).abs() < 0.5, "{m:?} vs {d:?} ({})", comp.nodes.len());
    // undo takes the whole analysis back
    s.execute("edit.undo", json!({})).unwrap();
    let CompKind::Tracker { points, .. } = node_kind(&s, c, t) else { panic!() };
    assert!(points.iter().all(|p| p.track.keyframes.is_empty()));
    // clearing frames 10..=20, then tracking backward into them, fills just that span
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("comp.track.clear", json!({"comp": c, "node": t, "from": RATE.tick_of(10).0, "to": RATE.tick_of(20).0})).unwrap();
    let CompKind::Tracker { points, .. } = node_kind(&s, c, t) else { panic!() };
    assert_eq!(points[0].track.keyframes.len(), 31 - 11);
    // (starting from frame 21, still analysed: the points start exactly where they were tracked)
    s.execute("comp.track.analyse", json!({"comp": c, "node": t, "from": RATE.tick_of(21).0, "to": RATE.tick_of(10).0, "direction": "backward", "wait": true}))
        .unwrap();
    let CompKind::Tracker { points, .. } = node_kind(&s, c, t) else { panic!() };
    for (pt, p0) in points.iter().zip(starts) {
        assert_eq!(pt.track.keyframes.len(), 31, "the span is filled again");
        for k in &pt.track.keyframes {
            let truth = p0 + pan(RATE.frame_at(k.time) as f64) - pan(0.0);
            assert!((k.value.as_vec2().unwrap() - truth).length() <= 0.5, "{:?}", k.time);
        }
    }
}

/// The plane at frame `k`: object coordinates (a 160 × 160 square around 0) → frame pixels.
fn pose(k: f64) -> Homography {
    let a = Affine::motion(Vec2::new(160.0 + 2.5 * k, 120.0 - 1.2 * k), Vec2::new(1.0 + 0.01 * k, 1.0), 1.5 * k, Vec2::ZERO);
    Homography { m: [a.a, a.c, a.e, a.b, a.d, a.f, 0.00012 * k, -0.00008 * k, 1.0] }
}

fn plane_shot(k: f64, p: Vec2) -> f64 {
    match pose(k).inverse().and_then(|inv| inv.apply(p)) {
        Some(o) if o.x.abs() < 80.0 && o.y.abs() < 80.0 => texture(o.x + 200.0, o.y + 300.0),
        _ => 0.25 + 0.1 * texture(p.x * 0.5 + 900.0, p.y * 0.5),
    }
}

fn obj_quad(r: f64) -> [Vec2; 4] {
    [Vec2::new(-r, -r), Vec2::new(r, -r), Vec2::new(r, r), Vec2::new(-r, r)]
}

#[test]
fn a_corner_pinned_insert_stays_locked_to_the_plane() {
    let (mut s, c, _, t) = session(plane_shot, "planarTracker");
    // the insert: a red solid on input 1
    let red = s.execute("comp.addNode", json!({"comp": c, "kind": "solid"})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.setParam", json!({"comp": c, "node": red, "param": "color", "value": [1.0, 0.0, 0.0, 1.0]})).unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": red, "to": t, "toPort": 1})).unwrap();
    let region: Vec<_> = obj_quad(64.0).iter().map(|o| pose(0.0).apply(*o).unwrap()).map(|p| json!([p.x, p.y])).collect();
    let quad: Vec<_> = obj_quad(70.0).iter().map(|o| pose(0.0).apply(*o).unwrap()).map(|p| json!([p.x, p.y])).collect();
    s.execute("comp.planar.setRegion", json!({"comp": c, "node": t, "region": region, "quad": quad})).unwrap();
    let last = 20;
    s.execute("comp.track.analyse", json!({"comp": c, "node": t, "to": RATE.tick_of(last).0, "wait": true})).unwrap();
    let CompKind::PlanarTracker { corners, confidence, .. } = node_kind(&s, c, t) else { panic!() };
    assert_eq!(corners.len(), 4);
    assert_eq!(confidence.len(), last as usize + 1);
    let mut worst = 0.0f64;
    for f in 0..=last {
        for (corner, o) in corners.iter().zip(obj_quad(70.0)) {
            let truth = pose(f as f64).apply(o).unwrap();
            worst = worst.max((corner.value_at(RATE.tick_of(f)).as_vec2().unwrap() - truth).length());
        }
    }
    eprintln!("comp planar tracker: worst analysed corner error {worst:.3} px over {last} frames");
    assert!(worst <= 1.0, "worst {worst}");
    // the insert's corners as the comp places them at the last frame (the node's plan: the
    // insert's frame rectangle through the corner pin) are the true plane corners within a pixel
    let plan = filmcraft_render::comp::track::plan(&node_kind(&s, c, t), RATE, RATE.tick_of(last), (W as f64, H as f64), true).unwrap();
    assert!(plan.foreground, "the insert is drawn");
    let filmcraft_render::comp::track::TrackWarp::Projective(pin) = plan.warp else { panic!("a corner pin is projective: {:?}", plan.warp) };
    let placed = filmcraft_render::comp::track::frame_quad(W as f64, H as f64).map(|p| pin.apply(p).unwrap());
    for (got, o) in placed.iter().zip(obj_quad(70.0)) {
        let truth = pose(last as f64).apply(o).unwrap();
        let e = (*got - truth).length();
        eprintln!("insert corner at frame {last}: {got:?} vs true {truth:?} ({e:.3} px)");
        assert!(e <= 1.0, "insert corner {got:?} is {e} px from the true corner {truth:?}");
    }
    // render the comp late in the shot: the insert covers the true quad to within a pixel
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let img = filmcraft_render::render_comp_node_at(&s.project, ItemId(c), 1, None, RATE.tick_of(last), (W, H), &provider).unwrap();
    let q = obj_quad(70.0).map(|o| pose(last as f64).apply(o).unwrap());
    let signed = |p: Vec2| -> f64 {
        // distance to the quad's boundary, positive inside (clockwise quad in y-down pixels)
        (0..4)
            .map(|i| {
                let (a, b) = (q[i], q[(i + 1) % 4]);
                let e = b - a;
                (e.x * (p.y - a.y) - e.y * (p.x - a.x)) / e.length()
            })
            .fold(f64::INFINITY, f64::min)
    };
    let (mut inside, mut wrong) = (0, Vec::new());
    for y in 0..H {
        for x in 0..W {
            let d = signed(Vec2::new(x as f64 + 0.5, y as f64 + 0.5));
            if d.abs() < 1.0 {
                continue;
            }
            let px = img.get(x, y);
            let red = px[0] - px[1] > 0.5;
            if d > 0.0 {
                inside += 1;
            }
            if red != (d > 0.0) {
                wrong.push((x, y, d));
            }
        }
    }
    assert!(inside > 10_000, "{inside}");
    assert!(wrong.is_empty(), "{} pixel(s) more than 1 px off the true quad: {:?}", wrong.len(), &wrong[..wrong.len().min(8)]);
}

#[test]
fn hostile_tracker_params_are_refused() {
    let (mut s, c, m, t) = session(panning, "tracker");
    let refused = |s: &mut Session, cmd: &str, p: serde_json::Value| {
        let (before, undo) = (s.project.clone(), s.history.undo.len());
        let e = s.execute(cmd, p.clone()).expect_err(&format!("{cmd} {p}"));
        assert!(Arc::ptr_eq(&before, &s.project), "{cmd} left the project alone");
        assert_eq!(s.history.undo.len(), undo);
        e.to_string()
    };
    let nan = f64::NAN;
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": t, "wait": true})); // no points
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": m, "wait": true})); // not a tracker
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": 999, "wait": true}));
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "wait": true}));
    refused(&mut s, "comp.track.setPoints", json!({"comp": c, "node": t, "points": vec![json!({"at": [1.0, 1.0]}); 65]}));
    refused(&mut s, "comp.track.setPoints", json!({"comp": c, "node": t, "points": [{"at": [nan, 1.0]}]}));
    refused(&mut s, "comp.track.setPoints", json!({"comp": c, "node": t, "points": [{"at": [1e300, 1.0]}]}));
    refused(&mut s, "comp.track.setPoints", json!({"comp": c, "node": t, "points": [{"at": [1.0, 1.0], "pattern": -4}]}));
    refused(&mut s, "comp.track.setPoints", json!({"comp": c, "node": t, "points": [{"name": "no position"}]}));
    refused(&mut s, "comp.track.setPoints", json!({"comp": c, "node": t, "points": [{"track": "nope"}]}));
    refused(&mut s, "comp.track.setPoints", json!({"comp": c, "node": m, "points": []}));
    refused(&mut s, "comp.track.setOptions", json!({"comp": c, "node": t, "operation": "warp"}));
    refused(&mut s, "comp.track.setOptions", json!({"comp": c, "node": t, "smoothRadius": 100_000}));
    refused(&mut s, "comp.track.setOptions", json!({"comp": c, "node": t, "smoothRadius": -1}));
    refused(&mut s, "comp.track.setOptions", json!({"comp": c, "node": t, "reference": i64::MAX}));
    refused(&mut s, "comp.track.setOptions", json!({"comp": c, "node": t, "method": "warp"}));
    refused(&mut s, "comp.planar.setRegion", json!({"comp": c, "node": t, "region": [[0, 0], [1, 1], [2, 0]]})); // not planar
    refused(&mut s, "comp.track.clear", json!({"comp": c, "node": m}));
    refused(&mut s, "comp.track.cancel", json!({"comp": c, "node": t}));
    s.execute("comp.track.setPoints", json!({"comp": c, "node": t, "points": [{"at": [100.0, 100.0]}]})).unwrap();
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": t, "direction": "sideways", "wait": true}));
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": t, "from": RATE.tick_of(10).0, "to": RATE.tick_of(5).0, "wait": true}));
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": t, "to": RATE.tick_of(5).0, "direction": "backward", "wait": true}));
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": t, "from": "soon", "wait": true}));
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": t, "clip": 12345, "wait": true}));
    // an unconnected input
    s.execute("comp.disconnect", json!({"comp": c, "to": t, "toPort": 0})).unwrap();
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": t, "wait": true}));

    let (mut s, c, _, p) = session(plane_shot, "planarTracker");
    refused(&mut s, "comp.track.analyse", json!({"comp": c, "node": p, "wait": true})); // no region
    refused(&mut s, "comp.planar.setRegion", json!({"comp": c, "node": p, "region": [[0, 0], [1, 1]]}));
    refused(&mut s, "comp.planar.setRegion", json!({"comp": c, "node": p, "region": [[0, 0], [1, nan], [2, 0]]}));
    refused(&mut s, "comp.planar.setRegion", json!({"comp": c, "node": p, "region": vec![json!([1, 1]); 2000]}));
    refused(&mut s, "comp.planar.setRegion", json!({"comp": c, "node": p, "region": [[0, 0], [1, 1], [2, 0]], "quad": [[0, 0]]}));
    refused(&mut s, "comp.planar.setRegion", json!({"comp": c, "node": p, "region": {"vertices": "x"}}));
    refused(&mut s, "comp.track.setOptions", json!({"comp": c, "node": p, "smoothRadius": 3}));
    // a flat region can't be tracked: the job fails and writes nothing
    s.execute("comp.planar.setRegion", json!({"comp": c, "node": p, "region": [[1, 1], [6, 1], [6, 6], [1, 6]]})).unwrap();
    let undo = s.history.undo.len();
    let r = s.execute("comp.track.analyse", json!({"comp": c, "node": p, "to": RATE.tick_of(3).0, "wait": true})).unwrap();
    assert!(r["status"].to_string().contains("can't be tracked") || s.history.undo.len() == undo, "{r}");
    assert_eq!(s.history.undo.len(), undo, "nothing written");
    // the options round-trip
    s.execute("comp.track.setOptions", json!({"comp": c, "node": p, "operation": "stabilize", "reference": RATE.tick_of(2).0})).unwrap();
    assert!(
        matches!(node_kind(&s, c, p), CompKind::PlanarTracker { operation: filmcraft_project::comp::TrackOperation::Stabilise, reference, .. } if reference == RATE.tick_of(2))
    );
}

#[test]
fn the_frame_map_scales_comp_pixels_to_the_trackers_frames_and_back() {
    use crate::comp_track::FrameMap;
    // small comps are tracked at full size: frame pixel centres are the comp's minus a half
    let m = FrameMap::new(320, 240);
    assert_eq!(m.size, (320, 240));
    assert_eq!(m.to_frame(Vec2::new(10.5, 20.5)), Vec2::new(10.0, 20.0));
    // a 6.2K comp is analysed at 1920 px: comp pixels 0..=3 (one frame pixel and a bit) map about
    // the frame pixel's centre, and every point comes back where it was
    let m = FrameMap::new(6240, 4160);
    assert_eq!(m.size, (1920, 1280));
    let k = 1920.0 / 6240.0;
    let centre = Vec2::new(0.5 / k, 0.5 / k); // the comp position of frame pixel 0's centre
    assert!((m.to_frame(centre) - Vec2::ZERO).length() < 1e-9, "{:?}", m.to_frame(centre));
    for p in [Vec2::new(0.0, 0.0), Vec2::new(3120.25, 2080.75), Vec2::new(6239.9, 4159.1)] {
        assert!((m.to_comp(m.to_frame(p)) - p).length() < 1e-9, "{p:?}");
    }
    assert!((m.scale(Vec2::new(16.0, 16.0)) - Vec2::new(16.0 * k, 16.0 * k)).length() < 1e-9);
    // hostile sizes never divide by zero
    let m = FrameMap::new(0, 0);
    assert_eq!(m.size, (1, 1));
    assert!(m.to_comp(Vec2::new(0.0, 0.0)).x.is_finite());
}

/// Waits (bounded) for every background job to finish, then writes their results.
fn finish_jobs(s: &mut Session) {
    let start = std::time::Instant::now();
    while s.jobs.iter().any(|j| !j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)) {
        assert!(start.elapsed().as_secs() < 300, "the analysis never finished");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    crate::comp_track::poll(s);
}

#[test]
fn a_background_analysis_writes_once_and_never_into_a_changed_tracker() {
    let (mut s, c, _, t) = session(panning, "tracker");
    let pts = json!([{"name": "A", "at": [100.3, 90.7], "pattern": 12, "search": 16}, {"name": "B", "at": [180.6, 120.2], "pattern": 12, "search": 16}]);
    s.execute("comp.track.setPoints", json!({"comp": c, "node": t, "points": pts})).unwrap();
    // in the background, untouched: written in one undo step when polled
    let undo = s.history.undo.len();
    s.execute("comp.track.analyse", json!({"comp": c, "node": t, "to": RATE.tick_of(6).0})).unwrap();
    assert!(s.execute("comp.track.analyse", json!({"comp": c, "node": t, "to": RATE.tick_of(6).0})).is_err(), "one analysis per node at a time");
    finish_jobs(&mut s);
    assert!(s.comp_track_jobs.is_empty());
    assert_eq!(s.history.undo.len(), undo + 1);
    let CompKind::Tracker { points, .. } = node_kind(&s, c, t) else { panic!() };
    assert!(points.iter().all(|p| p.track.keyframes.len() == 7), "{:?}", points.iter().map(|p| p.track.keyframes.len()).collect::<Vec<_>>());
    // the first point deleted while the analysis runs: its track must not land on point B
    s.execute("comp.track.clear", json!({"comp": c, "node": t})).unwrap();
    s.execute("comp.track.analyse", json!({"comp": c, "node": t, "to": RATE.tick_of(6).0})).unwrap();
    let only_b = json!([{"name": "B", "at": [180.6, 120.2], "pattern": 12, "search": 16}]);
    s.execute("comp.track.setPoints", json!({"comp": c, "node": t, "points": only_b})).unwrap();
    let undo = s.history.undo.len();
    finish_jobs(&mut s);
    assert!(s.comp_track_jobs.is_empty(), "the job is dropped");
    assert_eq!(s.history.undo.len(), undo, "nothing written");
    let CompKind::Tracker { points, .. } = node_kind(&s, c, t) else { panic!() };
    assert_eq!(points.len(), 1);
    assert!(points[0].track.keyframes.is_empty(), "point B kept no keys from point A's track");
    // the node deleted mid-analysis: nothing written, no panic, and the job is gone
    s.execute("comp.track.analyse", json!({"comp": c, "node": t, "to": RATE.tick_of(6).0})).unwrap();
    s.execute("comp.removeNode", json!({"comp": c, "nodes": [t]})).unwrap();
    let undo = s.history.undo.len();
    finish_jobs(&mut s);
    assert!(s.comp_track_jobs.is_empty());
    assert_eq!(s.history.undo.len(), undo);
}
