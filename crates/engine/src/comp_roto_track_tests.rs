//! Track-assisted roto (fork C3.7): a shape tracked with a fake video segmenter over a disc
//! moving along a known path, the tracker links, and the hostile / failure paths. No model is
//! needed: the segmenter is installed on `MagicMasks::video_segmenter`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use filmcraft_geom::Vec2;
use filmcraft_mask::video::{Direction, VideoMask, VideoSegmenter};
use filmcraft_mask::{LOW_RES, MaskError, Prompt, RgbImage, Segment};
use filmcraft_project::comp::CompKind;
use filmcraft_project::roto::{RotoKind, RotoShape};
use filmcraft_project::{ItemId, ItemKind, MaskPath};
use filmcraft_time::FrameRate;
use serde_json::{Value, json};

use crate::Session;

const RATE: FrameRate = FrameRate::FPS_24;
const W: usize = 256;
const H: usize = 256;
const R: f64 = 30.0;

/// The disc's centre at frame `k` (comp pixels).
fn centre(k: f64) -> Vec2 {
    Vec2::new(90.0 + 2.3 * k + 0.4 * (k * 0.5).sin(), 100.0 + 1.1 * k + 0.05 * k * k)
}

/// A white disc of radius [`R`] moving along [`centre`] over a dark grey frame (4× supersampled).
struct Disc {
    info: filmcraft_media::MediaInfo,
}

impl filmcraft_media::MediaSource for Disc {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        let c = centre(RATE.frame_at(req.time) as f64);
        let mut px = vec![0u8; W * H * 4];
        for (i, out) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = ((i % W) as f64, (i / W) as f64);
            let hits = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)].iter().filter(|(ox, oy)| (Vec2::new(x + ox, y + oy) - c).length() < R).count();
            let v = (30.0 + 225.0 * hits as f64 / 4.0).round() as u8;
            out.copy_from_slice(&[v, v, v, 255]);
        }
        Ok(Arc::new(filmcraft_frame::VideoFrame::rgba8(W as u32, H as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

/// A video segmenter that calls the bright part of each frame the subject; `gate` (when set)
/// holds every propagation until it opens (bounded). `prompts` counts prompts (1 each) and
/// propagations (100 each).
struct Bright {
    gate: Option<Arc<AtomicBool>>,
    prompts: Arc<AtomicUsize>,
}

fn bright(img: &RgbImage) -> Segment {
    let (w, h) = (img.width(), img.height());
    let data = img.data();
    let mut logits = vec![-20.0f32; LOW_RES * LOW_RES];
    for (g, l) in logits.iter_mut().enumerate() {
        let (gx, gy) = (g % LOW_RES, g / LOW_RES);
        let x = ((gx as f64 + 0.5) * w as f64 / LOW_RES as f64) as usize;
        let y = ((gy as f64 + 0.5) * h as f64 / LOW_RES as f64) as usize;
        let v = data.get((y.min(h - 1) * w + x.min(w - 1)) * 3).copied().unwrap_or(0);
        *l = (f32::from(v) - 128.0) / 16.0;
    }
    Segment { logits, cover: (1.0, 1.0), score: 1.0 }
}

impl VideoSegmenter for Bright {
    fn name(&self) -> String {
        "bright test segmenter".into()
    }
    fn add_prompt(&mut self, _frame: usize, image: &RgbImage, prompt: &Prompt) -> Result<VideoMask, MaskError> {
        assert!(prompt.rect.is_some() && !prompt.points.is_empty(), "the shape's box and interior points");
        self.prompts.fetch_add(1, Ordering::Relaxed);
        Ok(VideoMask { segment: bright(image), visible: true })
    }
    fn propagate(&mut self, _frame: usize, image: &RgbImage, _direction: Direction) -> Result<VideoMask, MaskError> {
        self.prompts.fetch_add(100, Ordering::Relaxed);
        if let Some(g) = &self.gate {
            let start = std::time::Instant::now();
            while !g.load(Ordering::Relaxed) && start.elapsed().as_secs() < 30 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let segment = bright(image);
        let visible = segment.logits.iter().any(|v| *v > 0.0);
        Ok(VideoMask { segment, visible })
    }
    fn reset(&mut self) {}
}

fn install(s: &mut Session, gate: Option<Arc<AtomicBool>>) -> Arc<AtomicUsize> {
    let prompts = Arc::new(AtomicUsize::new(0));
    let p = prompts.clone();
    s.magic_mask.video_segmenter = Some(Arc::new(move || Box::new(Bright { gate: gate.clone(), prompts: p.clone() }) as Box<dyn VideoSegmenter>));
    prompts
}

/// A session with the disc shot, a W×H comp of 48 frames: Media In (m) → Roto (r) → Media Out,
/// and a shape 0 drawn round the disc at frame 0 (vertex 1 feathered). (session, comp, roto)
fn session() -> (Session, u64, u64) {
    use filmcraft_media::MediaSource;
    let g = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
        W as u32,
        H as u32,
        RATE,
        RATE.tick_of(48),
    );
    let mut p = filmcraft_project::Project::new("roto");
    let item = p.add_item(
        "disc",
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
    s.media.insert(item, Arc::new(Disc { info: g.info().clone() }));
    let c = s.execute("comp.new", json!({"name": "R", "width": W, "height": H, "fps": 24, "duration": RATE.tick_of(48).0})).unwrap()["comp"].as_u64().unwrap();
    let m = s.execute("comp.addNode", json!({"comp": c, "kind": "mediaIn", "item": item.0})).unwrap()["node"].as_u64().unwrap();
    let r = s.execute("comp.addNode", json!({"comp": c, "kind": "roto"})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": m, "to": r, "toPort": 0})).unwrap();
    s.execute("comp.connect", json!({"comp": c, "from": r, "to": 1, "toPort": 0})).unwrap();
    let mut path = MaskPath::ellipse(centre(0.0), Vec2::new(R, R));
    path.vertices[1] = path.vertices[1].with_feather(2.0, 5.0);
    let shape = RotoShape::new("disc", RotoKind::Bezier, path);
    s.execute("comp.roto.addShape", json!({"comp": c, "node": r, "shape": shape})).unwrap();
    (s, c, r)
}

fn shape(s: &Session, comp: u64, node: u64, i: usize) -> RotoShape {
    let Some(ItemKind::Comp(c)) = s.project.item(ItemId(comp)).map(|i| &i.kind) else { panic!("comp") };
    let Some(CompKind::Roto { roto, .. }) = c.node(node).map(|n| &n.kind) else { panic!("roto") };
    roto.shapes[i].clone()
}

fn keys(sh: &RotoShape) -> Vec<(i64, MaskPath)> {
    sh.shape.keyframes.iter().map(|k| (RATE.frame_at(k.time), k.value.as_path().unwrap().clone())).collect()
}

/// The area centroid of a path's outline.
fn centroid(p: &MaskPath) -> Vec2 {
    let q = p.flatten(0.05);
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..q.len() {
        let (p0, p1) = (q[i], q[(i + 1) % q.len()]);
        let cr = p0.x * p1.y - p1.x * p0.y;
        a += cr;
        cx += (p0.x + p1.x) * cr;
        cy += (p0.y + p1.y) * cr;
    }
    Vec2::new(cx / (3.0 * a), cy / (3.0 * a))
}

fn finish_jobs(s: &mut Session) {
    let start = std::time::Instant::now();
    while s.jobs.iter().any(|j| !j.progress.finished.load(Ordering::Relaxed)) {
        assert!(start.elapsed().as_secs() < 300, "the track never finished");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    crate::comp_roto_track::poll(s);
}

#[test]
fn a_tracked_shape_follows_the_moving_disc_in_one_undo_step() {
    let (mut s, c, r) = session();
    let prompts = install(&mut s, None);
    let undo = s.history.undo.len();
    let res = s.execute("comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": RATE.tick_of(20).0, "wait": true})).unwrap();
    assert_eq!(res["frames"], 20, "{res}");
    assert_eq!(prompts.load(Ordering::Relaxed), 1 + 100 * 20, "one prompt, then one propagation a frame");
    assert!(s.roto_track_jobs.is_empty(), "written");
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    let sh = shape(&s, c, r, 0);
    let ks = keys(&sh);
    assert_eq!(ks.len(), 21, "every frame keyed");
    let mut worst = 0.0f64;
    for (f, path) in &ks {
        assert_eq!(path.vertices.len(), 4, "the start shape's vertex count");
        assert_eq!((path.vertices[1].feather_in, path.vertices[1].feather_out), (2.0, 5.0), "feathers copied by index");
        assert_eq!((path.vertices[0].feather_in, path.vertices[0].feather_out), (0.0, 0.0));
        worst = worst.max((centroid(path) - centre(*f as f64)).length());
    }
    assert!(worst <= 0.5, "worst centroid error {worst:.3} px");
    // a frame between keyframes interpolates (same vertex count)
    assert_eq!(sh.path_at(RATE.tick_of(7) + filmcraft_time::Tick(1000)).vertices.len(), 4);
    // undo takes the whole track back
    s.execute("edit.undo", json!({})).unwrap();
    assert!(shape(&s, c, r, 0).shape.keyframes.is_empty());
}

#[test]
fn keyframe_spacing_and_backward_tracks() {
    let (mut s, c, r) = session();
    install(&mut s, None);
    // the shape drawn on frame 20 (a keyframe there), tracked back to frame 8 every 3 frames
    let at20 = MaskPath::ellipse(centre(20.0), Vec2::new(R, R));
    s.execute("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 0, "time": RATE.tick_of(20).0, "path": at20})).unwrap();
    let res = s
        .execute("comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": RATE.tick_of(8).0, "direction": "backward", "every": 3, "wait": true}))
        .unwrap();
    assert_eq!(res["frames"], 12, "{res}");
    let ks = keys(&shape(&s, c, r, 0));
    assert_eq!(ks.iter().map(|(f, _)| *f).collect::<Vec<_>>(), vec![8, 11, 14, 17, 20]);
    for (f, path) in &ks {
        let e = (centroid(path) - centre(*f as f64)).length();
        assert!(e <= 0.5, "frame {f}: {e:.3} px");
    }
}

#[test]
fn stopping_a_track_keeps_what_was_tracked() {
    let (mut s, c, r) = session();
    let gate = Arc::new(AtomicBool::new(false));
    let calls = install(&mut s, Some(gate.clone()));
    let res = s.execute("comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": RATE.tick_of(20).0})).unwrap();
    let job = res["job"].as_u64().unwrap();
    // wait (bounded) until the job is inside its first propagation
    let start = std::time::Instant::now();
    while calls.load(Ordering::Relaxed) < 100 && start.elapsed().as_secs() < 60 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.execute("comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0})).is_err(), "one track per node at a time");
    assert_eq!(s.execute("comp.roto.stopTrack", json!({"comp": c, "node": r})).unwrap()["stopped"], 1);
    gate.store(true, Ordering::Relaxed);
    finish_jobs(&mut s);
    assert!(s.roto_track_jobs.is_empty());
    let j = s.jobs.iter().find(|j| j.id == job).unwrap();
    assert!(j.progress.status.lock().unwrap().contains("stopped"), "{}", j.progress.status.lock().unwrap());
    // the first frame (held by the gate) finished, then the stop landed: start + frame 1
    let ks = keys(&shape(&s, c, r, 0));
    assert_eq!(ks.iter().map(|(f, _)| *f).collect::<Vec<_>>(), vec![0, 1]);
    assert!(s.execute("comp.roto.stopTrack", json!({"comp": c, "node": r})).is_err(), "nothing running");
}

#[test]
fn a_shape_changed_while_tracking_is_not_overwritten() {
    let (mut s, c, r) = session();
    let gate = Arc::new(AtomicBool::new(false));
    install(&mut s, Some(gate.clone()));
    s.execute("comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": RATE.tick_of(6).0})).unwrap();
    let moved = MaskPath::ellipse(Vec2::new(60.0, 60.0), Vec2::new(10.0, 10.0));
    s.execute("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 0, "time": 0, "path": moved})).unwrap();
    let undo = s.history.undo.len();
    gate.store(true, Ordering::Relaxed);
    finish_jobs(&mut s);
    assert_eq!(s.history.undo.len(), undo, "nothing written");
    assert_eq!(shape(&s, c, r, 0).shape.keyframes.len(), 1, "only the user's keyframe");
    // the node deleted mid-track: nothing written, no panic
    s.execute("comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": RATE.tick_of(4).0})).unwrap();
    s.execute("comp.removeNode", json!({"comp": c, "nodes": [r]})).unwrap();
    let undo = s.history.undo.len();
    finish_jobs(&mut s);
    assert!(s.roto_track_jobs.is_empty());
    assert_eq!(s.history.undo.len(), undo);
}

fn refused(s: &mut Session, cmd: &str, p: Value) -> String {
    let undo = s.history.undo.len();
    let e = s.execute(cmd, p.clone()).err().unwrap_or_else(|| panic!("{cmd} {p} should be refused")).to_string();
    assert_eq!(s.history.undo.len(), undo, "{cmd} {p} changed nothing");
    assert!(s.roto_track_jobs.is_empty(), "{cmd} {p} started nothing");
    e
}

#[test]
fn hostile_track_and_link_params_are_refused() {
    let (mut s, c, r) = session();
    install(&mut s, None);
    let t = |f: i64| RATE.tick_of(f).0;
    for p in [
        json!({"comp": c, "node": r, "shape": 9}),
        json!({"comp": c, "node": r, "shape": -1}),
        json!({"comp": c, "node": r, "shape": 0.5}),
        json!({"comp": c, "node": 1, "shape": 0}),
        json!({"comp": c, "node": 999, "shape": 0}),
        json!({"comp": c, "node": r, "shape": 0, "every": 0}),
        json!({"comp": c, "node": r, "shape": 0, "every": 5000}),
        json!({"comp": c, "node": r, "shape": 0, "every": 1.5}),
        json!({"comp": c, "node": r, "shape": 0, "model": "huge"}),
        json!({"comp": c, "node": r, "shape": 0, "model": 3}),
        json!({"comp": c, "node": r, "shape": 0, "direction": "up"}),
        json!({"comp": c, "node": r, "shape": 0, "to": 0}),
        json!({"comp": c, "node": r, "shape": 0, "from": t(5), "to": t(9), "direction": "backward"}),
        json!({"comp": c, "node": r, "shape": 0, "from": i64::MAX}),
        json!({"comp": c, "node": r, "shape": 0, "from": "soon"}),
        json!({"comp": c, "node": r, "shape": 0, "clip": 12345}),
    ] {
        refused(&mut s, "comp.roto.trackShape", p);
    }
    // a shape with nothing at the start, a B-spline, an open outline
    for (name, kind, path) in [
        ("off", RotoKind::Bezier, MaskPath::rect(-500.0, -500.0, -400.0, -400.0)),
        ("bs", RotoKind::BSpline, MaskPath::ellipse(centre(0.0), Vec2::new(R, R))),
        ("open", RotoKind::Bezier, MaskPath { closed: false, ..MaskPath::ellipse(centre(0.0), Vec2::new(R, R)) }),
    ] {
        let sh = RotoShape::new(name, kind, path);
        let i = s.execute("comp.roto.addShape", json!({"comp": c, "node": r, "shape": sh})).unwrap()["shape"].as_u64().unwrap();
        refused(&mut s, "comp.roto.trackShape", json!({"comp": c, "node": r, "shape": i, "to": t(3)}));
    }
    // no input
    let lone = s.execute("comp.addNode", json!({"comp": c, "kind": "roto"})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.roto.addShape", json!({"comp": c, "node": lone})).unwrap();
    assert!(refused(&mut s, "comp.roto.trackShape", json!({"comp": c, "node": lone, "shape": 0, "to": t(3)})).contains("input"));
    // links
    let tr = s.execute("comp.addNode", json!({"comp": c, "kind": "tracker"})).unwrap()["node"].as_u64().unwrap();
    let pl = s.execute("comp.addNode", json!({"comp": c, "kind": "planarTracker"})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.track.setPoints", json!({"comp": c, "node": tr, "points": [{"name": "A", "at": [50, 50]}]})).unwrap();
    for p in [
        json!({"comp": c, "node": r, "shape": 0}),
        json!({"comp": c, "node": r, "shape": 0, "tracker": 999}),
        json!({"comp": c, "node": r, "shape": 0, "tracker": r}),
        json!({"comp": c, "node": r, "shape": 0, "tracker": "x"}),
        json!({"comp": c, "node": r, "shape": 9, "tracker": tr}),
        json!({"comp": c, "node": tr, "shape": 0, "tracker": tr}),
        json!({"comp": c, "node": r, "shape": 0, "tracker": tr, "output": "B"}),
        json!({"comp": c, "node": r, "shape": 0, "tracker": tr, "output": "x".repeat(500)}),
        json!({"comp": c, "node": r, "shape": 0, "tracker": tr, "output": 7}),
        json!({"comp": c, "node": r, "shape": 0, "tracker": pl, "output": "A"}),
    ] {
        refused(&mut s, "comp.roto.linkTracker", p);
    }
    refused(&mut s, "comp.roto.unlinkTracker", json!({"comp": c, "node": r, "shape": 0}));
    refused(&mut s, "comp.roto.stopTrack", json!({"comp": c}));
    // a shape following a tracker isn't tracked (it would move twice)
    s.execute("comp.roto.linkTracker", json!({"comp": c, "node": r, "shape": 0, "tracker": tr, "output": "A"})).unwrap();
    assert!(refused(&mut s, "comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": t(3)})).contains("tracker"));
}

#[test]
fn without_a_model_the_track_says_what_to_install() {
    let (mut s, c, r) = session();
    let dir = std::env::temp_dir().join(format!("fc-c37-no-model-{}", std::process::id()));
    s.magic_mask.model_path = Some(dir.join("mobilesam").join("mobile_sam.pt"));
    s.magic_mask.sam2_dir = Some(dir.join("sam2"));
    let e = refused(&mut s, "comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": RATE.tick_of(3).0}));
    assert!(e.contains("mobile_sam.pt"), "{e}");
    let e = refused(&mut s, "comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": RATE.tick_of(3).0, "model": "better"}));
    assert!(e.contains("SAM 2.1"), "{e}");
}

#[test]
fn links_survive_save_and_load_and_a_deleted_tracker_is_explained() {
    let (mut s, c, r) = session();
    let tr = s.execute("comp.addNode", json!({"comp": c, "kind": "tracker"})).unwrap()["node"].as_u64().unwrap();
    s.execute("comp.track.setPoints", json!({"comp": c, "node": tr, "points": [{"name": "A", "at": [50, 50]}]})).unwrap();
    let undo = s.history.undo.len();
    s.execute("comp.roto.linkTracker", json!({"comp": c, "node": r, "shape": 0, "tracker": tr})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    let link = shape(&s, c, r, 0).track_link.unwrap();
    assert_eq!((link.tracker.as_str(), link.output.as_str()), (tr.to_string().as_str(), "transform"));
    // save / load
    let back = filmcraft_project::Project::from_json(&s.project.to_json()).unwrap();
    let s2 = Session { project: Arc::new(back), ..Default::default() };
    assert_eq!(shape(&s2, c, r, 0).track_link, Some(link));
    let ex = s.execute("comp.explain", json!({"comp": c})).unwrap().to_string();
    assert!(ex.contains("follow the motion"), "{ex}");
    // the tracker deleted: the link dangles (the shape stays put) and the explanation says so
    s.execute("comp.removeNode", json!({"comp": c, "nodes": [tr]})).unwrap();
    let ex = s.execute("comp.explain", json!({"comp": c})).unwrap().to_string();
    assert!(ex.contains("tracker that is gone"), "{ex}");
    // unlink is one undo step and clears it
    s.execute("comp.roto.unlinkTracker", json!({"comp": c, "node": r, "shape": 0})).unwrap();
    assert!(shape(&s, c, r, 0).track_link.is_none());
}

/// Adds node 50: a Tracker whose point "A" moves by (2, 1) px a frame from frame 0 (its
/// reference), analysed over 48 frames; and node 51: a Planar Tracker whose plane turns in
/// perspective. Returns their ids.
fn add_trackers(s: &mut Session, comp: u64) -> (u64, u64) {
    use filmcraft_project::comp::{CompNode, TrackOperation, TrackerPoint};
    use filmcraft_project::{Keyframe, Param, ParamValue, TrackMethod};
    let track = |at: &dyn Fn(i64) -> Vec2| {
        let mut p = Param::new(ParamValue::Vec2(at(0)));
        p.keyframes = (0..48).map(|f| Keyframe::new(RATE.tick_of(f), ParamValue::Vec2(at(f)))).collect();
        p
    };
    let a = TrackerPoint { track: track(&|f| Vec2::new(40.0 + 2.0 * f as f64, 40.0 + f as f64)), ..TrackerPoint::at("A", Vec2::new(40.0, 40.0)) };
    let tracker = CompKind::Tracker {
        points: vec![a],
        operation: TrackOperation::MatchMove,
        smooth_radius: 0,
        reference: filmcraft_time::Tick::ZERO,
        method: TrackMethod::Position,
    };
    let quad = |f: i64| {
        let k = f as f64;
        [Vec2::new(60.0 + k, 60.0), Vec2::new(190.0 + k, 62.0 + 0.4 * k), Vec2::new(200.0 + 0.6 * k, 180.0), Vec2::new(55.0 + k, 175.0 - 0.3 * k)]
    };
    let corners = (0..4).map(|i| track(&|f| quad(f)[i])).collect();
    let planar = CompKind::PlanarTracker {
        region: MaskPath::polygon(&quad(0)),
        quad: quad(0),
        reference: filmcraft_time::Tick::ZERO,
        corners,
        confidence: Vec::new(),
        operation: TrackOperation::CornerPin,
    };
    let proj = Arc::make_mut(&mut s.project);
    let Some(ItemKind::Comp(c)) = proj.item_mut(ItemId(comp)).map(|i| &mut i.kind) else { panic!("comp") };
    Arc::make_mut(c).nodes.extend([CompNode::new(50, tracker), CompNode::new(51, planar)]);
    (50, 51)
}

/// Shape 0 of the Roto node as the renderer draws it at frame `f` (tracker links applied).
fn drawn(s: &Session, comp: u64, node: u64, f: i64) -> MaskPath {
    let Some(ItemKind::Comp(c)) = s.project.item(ItemId(comp)).map(|i| &i.kind) else { panic!("comp") };
    let Some(CompKind::Roto { roto, .. }) = c.node(node).map(|n| &n.kind) else { panic!("roto") };
    let t = RATE.tick_of(f);
    let size = (f64::from(c.width), f64::from(c.height));
    filmcraft_render::comp::track::linked_roto(roto, |id| c.node(id).map(|n| &n.kind), c.rate, t, size).shapes[0].path_at(t)
}

fn max_vertex_gap(a: &MaskPath, b: &MaskPath) -> f64 {
    assert_eq!(a.vertices.len(), b.vertices.len());
    a.vertices.iter().zip(&b.vertices).map(|(x, y)| (x.p - y.p).length().max((x.t_out - y.t_out).length()).max((x.t_in - y.t_in).length())).fold(0.0, f64::max)
}

/// Review fix: a shape linked (or unlinked) on a frame other than the tracker's reference frame
/// used to jump by the tracker's motion up to that frame. Now it stays where it is on screen at
/// `at`, keyframes stay where they are at their own times, and it then moves with the tracker.
#[test]
fn linking_and_unlinking_off_the_reference_frame_does_not_jump() {
    let (mut s, c, r) = session();
    let (tr, pl) = add_trackers(&mut s, c);
    let seen = |s: &Session, f| drawn(s, c, r, f);
    // unkeyframed shape, linked at frame 10 to point A
    let before = seen(&s, 10);
    let res = s.execute("comp.roto.linkTracker", json!({"comp": c, "node": r, "shape": 0, "tracker": tr, "output": "A", "at": RATE.tick_of(10).0})).unwrap();
    assert_eq!(res["rebased"], 1, "{res}");
    assert!(max_vertex_gap(&seen(&s, 10), &before) < 1e-9, "no jump at the playhead");
    // and it follows from there: 5 frames later it moved by (10, 5)
    let d = centroid(&seen(&s, 15)) - centroid(&before);
    assert!((d - Vec2::new(10.0, 5.0)).length() < 1e-6, "{d:?}");
    // re-linking to the planar tracker at frame 20: still no jump (comes back through A first)
    let before = seen(&s, 20);
    s.execute("comp.roto.linkTracker", json!({"comp": c, "node": r, "shape": 0, "tracker": pl, "at": RATE.tick_of(20).0})).unwrap();
    assert!(max_vertex_gap(&seen(&s, 20), &before) < 1e-6, "no jump through a perspective plane");
    // unlinking at frame 30 bakes the motion in there
    let before = seen(&s, 30);
    s.execute("comp.roto.unlinkTracker", json!({"comp": c, "node": r, "shape": 0, "at": RATE.tick_of(30).0})).unwrap();
    assert!(shape(&s, c, r, 0).track_link.is_none());
    assert!(max_vertex_gap(&seen(&s, 30), &before) < 1e-6, "no jump on unlink");
    // one undo step each
    s.execute("edit.undo", json!({})).unwrap();
    assert!(shape(&s, c, r, 0).track_link.is_some());

    // a keyframed shape: every keyframe stays where it was drawn at its own time
    let (mut s, c, r) = session();
    let (tr, _) = add_trackers(&mut s, c);
    for f in [4, 12] {
        let path = MaskPath::ellipse(centre(f as f64), Vec2::new(R, R));
        s.execute("comp.roto.setShapeKeyframe", json!({"comp": c, "node": r, "shape": 0, "time": RATE.tick_of(f).0, "path": path})).unwrap();
    }
    let was: Vec<MaskPath> = [4, 12].iter().map(|f| drawn(&s, c, r, *f)).collect();
    let res = s.execute("comp.roto.linkTracker", json!({"comp": c, "node": r, "shape": 0, "tracker": tr, "output": "A"})).unwrap();
    assert_eq!(res["rebased"], 2, "{res}");
    for (f, w) in [4, 12].iter().zip(&was) {
        assert!(max_vertex_gap(&drawn(&s, c, r, *f), w) < 1e-9, "keyframe at {f} kept its place");
    }
    // without `at`, an unkeyframed shape is left as stored (the tracker's reference frame)
    let (mut s, c, r) = session();
    let (tr, _) = add_trackers(&mut s, c);
    let stored = shape(&s, c, r, 0).path_at(filmcraft_time::Tick::ZERO);
    assert_eq!(s.execute("comp.roto.linkTracker", json!({"comp": c, "node": r, "shape": 0, "tracker": tr})).unwrap()["rebased"], 0);
    assert_eq!(shape(&s, c, r, 0).path_at(filmcraft_time::Tick::ZERO), stored);
    // hostile `at`
    refused(&mut s, "comp.roto.unlinkTracker", json!({"comp": c, "node": r, "shape": 0, "at": "x"}));
    refused(&mut s, "comp.roto.unlinkTracker", json!({"comp": c, "node": r, "shape": 0, "at": i64::MAX}));
}

// -- the real model ----------------------------------------------------------------------------

#[cfg(feature = "magic-mask")]
mod real {
    use super::*;
    use filmcraft_project::MaskVertex;

    const RW: usize = 512;
    const RH: usize = 384;
    const AXES: (f64, f64) = (70.0, 44.0);

    /// The textured ellipse's centre and turn (radians) at frame `k`.
    fn pose(k: f64) -> (Vec2, f64) {
        (Vec2::new(150.0 + 6.0 * k, 170.0 + 2.0 * k + 0.08 * k * k), 0.035 * k)
    }

    /// Where `p` is in the object's own (unturned, centred) coordinates at frame `k`.
    fn local(p: Vec2, k: f64) -> Vec2 {
        let (c, a) = pose(k);
        let d = p - c;
        Vec2::new(d.x * a.cos() + d.y * a.sin(), -d.x * a.sin() + d.y * a.cos())
    }

    fn inside(p: Vec2, k: f64) -> bool {
        let q = local(p, k);
        (q.x / AXES.0).powi(2) + (q.y / AXES.1).powi(2) < 1.0
    }

    /// An orange ellipse with a dark checker that turns with it, moving over a blue-green
    /// sinusoidal background (4× supersampled).
    struct Blob {
        info: filmcraft_media::MediaInfo,
    }

    impl filmcraft_media::MediaSource for Blob {
        fn info(&self) -> &filmcraft_media::MediaInfo {
            &self.info
        }
        fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
            let k = RATE.frame_at(req.time) as f64;
            let mut px = vec![0u8; RW * RH * 4];
            for (i, out) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let (x, y) = ((i % RW) as f64, (i / RW) as f64);
                let mut acc = [0.0f64; 3];
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let p = Vec2::new(x + ox, y + oy);
                    let c = if inside(p, k) {
                        let q = local(p, k);
                        let check = ((q.x / 12.0).floor() as i64 + (q.y / 12.0).floor() as i64).rem_euclid(2) == 0;
                        if check { [235.0, 140.0, 40.0] } else { [120.0, 60.0, 20.0] }
                    } else {
                        let s = 0.5 + 0.5 * ((p.x * 0.07).sin() * (p.y * 0.05).cos());
                        [30.0 + 40.0 * s, 90.0 + 60.0 * s, 110.0 + 50.0 * s]
                    };
                    for (a, v) in acc.iter_mut().zip(c) {
                        *a += v / 4.0;
                    }
                }
                out.copy_from_slice(&[acc[0].round() as u8, acc[1].round() as u8, acc[2].round() as u8, 255]);
            }
            Ok(Arc::new(filmcraft_frame::VideoFrame::rgba8(RW as u32, RH as u32, px)))
        }
        fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
        }
    }

    /// The object's outline at frame `k` as an `n`-point Bézier.
    fn outline(k: f64, n: usize) -> MaskPath {
        let (c, a) = pose(k);
        let turn = |v: Vec2| Vec2::new(v.x * a.cos() - v.y * a.sin(), v.x * a.sin() + v.y * a.cos());
        let step = std::f64::consts::TAU / n as f64;
        let h = 4.0 / 3.0 * (step / 4.0).tan();
        let vertices = (0..n)
            .map(|i| {
                let phi = step * i as f64;
                let p = c + turn(Vec2::new(AXES.0 * phi.cos(), AXES.1 * phi.sin()));
                let d = turn(Vec2::new(-AXES.0 * phi.sin(), AXES.1 * phi.cos())) * h;
                MaskVertex { p, t_in: d * -1.0, t_out: d, ..MaskVertex::default() }
            })
            .collect();
        MaskPath { vertices, closed: true }
    }

    /// IoU of `path`'s rasterised coverage (as the Roto node draws it) with the true ellipse.
    fn iou(path: &MaskPath, k: f64) -> f64 {
        let roto = filmcraft_project::roto::Roto { shapes: vec![RotoShape::new("", RotoKind::Bezier, path.clone())] };
        let prepared = filmcraft_render::roto::prepare(&roto, filmcraft_time::Tick::ZERO, RW, RH, 1.0);
        let cov = filmcraft_render::roto::rasterize(&prepared, RW, RH);
        let (mut i, mut u) = (0.0, 0.0);
        for (n, c) in cov.iter().enumerate() {
            let p = Vec2::new((n % RW) as f64 + 0.5, (n / RW) as f64 + 0.5);
            let t = if inside(p, k) { 1.0 } else { 0.0 };
            let c = f64::from(*c);
            i += c.min(t);
            u += c.max(t);
        }
        i / u.max(1e-9)
    }

    /// The point of track-assisted roto, on the real model: SAM 2.1 Base+ (Faster) on the GPU
    /// tracks an 8-point shape drawn round a textured ellipse that drifts, accelerates and turns
    /// for 24 frames. Reports per-keyframe centroid error, IoU of the rasterised roto with the
    /// truth, vertex counts and time per frame. Skips without the checkpoint or a usable GPU.
    /// Run: `cargo test -p filmcraft-engine --features magic-mask --lib sam2_tracks_a_roto_shape -- --ignored --nocapture`.
    #[test]
    #[ignore = "real SAM 2.1 model on the GPU (minutes); run by hand"]
    fn sam2_tracks_a_roto_shape_on_a_textured_moving_object() {
        use filmcraft_mask::sam2::{Sam2Size, tests_support};
        use filmcraft_media::MediaSource;
        let Some((pt, _)) = tests_support::files(Sam2Size::BasePlus) else { return };
        let _g = crate::magic_mask_tests::SAM2_GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let g = filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
            RW as u32,
            RH as u32,
            RATE,
            RATE.tick_of(48),
        );
        let mut p = filmcraft_project::Project::new("roto-sam2");
        let item = p.add_item(
            "blob",
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
        s.media.insert(item, Arc::new(Blob { info: g.info().clone() }));
        s.magic_mask.segmenter = None;
        s.magic_mask.video_segmenter = None;
        s.magic_mask.sam2_dir = pt.parent().map(|d| d.to_path_buf());
        // no MobileSAM: a SAM 2.1 failure fails the test instead of falling back
        s.magic_mask.model_path = Some(std::env::temp_dir().join("fc-c37-no-mobilesam").join("mobile_sam.pt"));
        let c = s.execute("comp.new", json!({"name": "R", "width": RW, "height": RH, "fps": 24, "duration": RATE.tick_of(48).0})).unwrap()["comp"]
            .as_u64()
            .unwrap();
        let m = s.execute("comp.addNode", json!({"comp": c, "kind": "mediaIn", "item": item.0})).unwrap()["node"].as_u64().unwrap();
        let r = s.execute("comp.addNode", json!({"comp": c, "kind": "roto"})).unwrap()["node"].as_u64().unwrap();
        s.execute("comp.connect", json!({"comp": c, "from": m, "to": r, "toPort": 0})).unwrap();
        s.execute("comp.connect", json!({"comp": c, "from": r, "to": 1, "toPort": 0})).unwrap();
        let shape0 = RotoShape::new("blob", RotoKind::Bezier, outline(0.0, 8));
        s.execute("comp.roto.addShape", json!({"comp": c, "node": r, "shape": shape0})).unwrap();
        let res =
            s.execute("comp.roto.trackShape", json!({"comp": c, "node": r, "shape": 0, "to": RATE.tick_of(24).0, "model": "faster", "wait": true})).unwrap();
        let job = res["job"].as_u64().unwrap();
        let j = s.jobs.iter().find(|j| j.id == job).unwrap();
        let status = j.progress.status.lock().unwrap().clone();
        let report = j.result.lock().unwrap().clone();
        eprintln!("status: {status}");
        if status.contains("GPU") && report.as_ref().is_some_and(|r| r.is_err()) {
            eprintln!("skipped: {status}");
            return;
        }
        let report = report.unwrap().unwrap_or_else(|e| panic!("track failed: {e}"));
        assert!(status.contains("SAM 2.1"), "{status}");
        let ks = keys(&shape(&s, c, r, 0));
        assert_eq!(ks.len(), 25, "every frame keyed");
        let (mut worst_c, mut worst_iou, mut sum_iou) = (0.0f64, 1.0f64, 0.0f64);
        for (f, path) in &ks {
            let k = *f as f64;
            let e = (centroid(path) - pose(k).0).length();
            let q = iou(path, k);
            eprintln!("frame {f:2}: {} points, centroid error {e:.2} px, IoU {q:.4}", path.vertices.len());
            assert_eq!(path.vertices.len(), 8, "frame {f}: vertex count");
            (worst_c, worst_iou, sum_iou) = (worst_c.max(e), worst_iou.min(q), sum_iou + q);
        }
        let per = report.seconds / report.frames.max(1) as f64;
        eprintln!(
            "worst centroid {worst_c:.2} px, worst IoU {worst_iou:.4}, mean IoU {:.4}, {:.0} ms / frame ({} frames, {:.1} s incl. load)",
            sum_iou / ks.len() as f64,
            per * 1000.0,
            report.frames,
            report.seconds
        );
        assert!(worst_c <= 2.0, "worst centroid error {worst_c:.2} px");
        assert!(worst_iou >= 0.9, "worst IoU {worst_iou:.4}");
    }
}
