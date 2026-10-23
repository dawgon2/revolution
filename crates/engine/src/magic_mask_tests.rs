//! Magic Mask commands, propagation job and matte cache (fork G3.2), with a fake segmenter (the
//! subject is every bright pixel inside the prompt's box) on a moving bright square.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use filmcraft_mask::{Embedding, LOW_RES, MaskError, Matte, Prompt, Rect, RgbImage, Segment, Segmenter};
use filmcraft_project::ClipId;
use filmcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::Session;
use crate::background::{BackgroundWorker, WorkerConfig};
use crate::magic_mask::{MAX_HELD_FRAMES, MatteCache, TrackState, magic_matte, owner};

const W: usize = 160;
const H: usize = 96;
const SIDE: usize = 24;
const RATE: FrameRate = FrameRate::FPS_24;

/// The square's left edge at frame `k`; None once it has left (`vanish` frames on).
fn square_x(k: i64, vanish: Option<i64>) -> Option<usize> {
    if vanish.is_some_and(|v| k >= v) {
        return None;
    }
    Some(8 + 3 * k.clamp(0, 40) as usize)
}

/// A bright square moving 3 px a frame to the right over a dark background.
struct MovingSquare {
    info: filmcraft_media::MediaInfo,
    vanish: Option<i64>,
    /// A red object (bright to the fake segmenter too) at the right of the frame, touching the
    /// square's right side from frame [`NEIGHBOUR_FROM`] on.
    neighbour: bool,
}

const NEIGHBOUR_FROM: i64 = 4;
const NEIGHBOUR_SIDE: usize = 20;

/// The red object's left edge at frame `k`.
fn neighbour_x(k: i64) -> usize {
    if k < NEIGHBOUR_FROM { 130 } else { square_x(k, None).unwrap_or(0) + SIDE }
}

impl filmcraft_media::MediaSource for MovingSquare {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        let k = RATE.frame_at(req.time);
        let x0 = square_x(k, self.vanish);
        let mut px = vec![0u8; W * H * 4];
        for y in 0..H {
            for x in 0..W {
                let on = x0.is_some_and(|x0| (x0..x0 + SIDE).contains(&x)) && (36..36 + SIDE).contains(&y);
                let red = self.neighbour && (neighbour_x(k)..neighbour_x(k) + NEIGHBOUR_SIDE).contains(&x) && (36..36 + SIDE).contains(&y);
                let c = if on {
                    [250, 250, 250, 255]
                } else if red {
                    [250, 50, 50, 255]
                } else {
                    [30, 30, 30, 255]
                };
                px[(y * W + x) * 4..][..4].copy_from_slice(&c);
            }
        }
        Ok(Arc::new(filmcraft_frame::VideoFrame::rgba8(W as u32, H as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn truth(k: i64) -> Matte {
    let x0 = square_x(k, None).unwrap();
    let alpha = (0..W * H).map(|i| if (x0..x0 + SIDE).contains(&(i % W)) && (36..36 + SIDE).contains(&(i / W)) { 255 } else { 0 }).collect();
    Matte { width: W, height: H, alpha }
}

/// The fake segmenter: bright pixels (red > 128) inside the prompt's box (or the whole frame),
/// counting its calls.
struct Bright {
    calls: Arc<AtomicUsize>,
}

impl Segmenter for Bright {
    fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError> {
        Ok(Embedding::new(image.width(), image.height(), Box::new(image.clone())))
    }
    fn segment(&self, e: &Embedding, prompt: &Prompt) -> Result<Segment, MaskError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let img: &RgbImage = e.inner()?;
        let (_, rect) = prompt.checked()?;
        let r = rect.unwrap_or(Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 });
        let mut s = Segment::empty((1.0, 1.0));
        for gy in 0..LOW_RES {
            for gx in 0..LOW_RES {
                let (u, v) = ((gx as f32 + 0.5) / LOW_RES as f32, (gy as f32 + 0.5) / LOW_RES as f32);
                let x = ((u * img.width() as f32) as usize).min(img.width() - 1);
                let y = ((v * img.height() as f32) as usize).min(img.height() - 1);
                let inside = u >= r.x0 && u < r.x1 && v >= r.y0 && v < r.y1 && img.data()[(y * img.width() + x) * 3] > 128;
                s.logits[gy * LOW_RES + gx] = if inside { 10.0 } else { -10.0 };
            }
        }
        s.score = 1.0;
        Ok(s)
    }
}

struct Fixture {
    s: Session,
    clip: ClipId,
    node: u64,
    calls: Arc<AtomicUsize>,
}

fn fixture(frames: i64, vanish: Option<i64>) -> Fixture {
    fixture_with(frames, vanish, false)
}

fn fixture_with(frames: i64, vanish: Option<i64>, neighbour: bool) -> Fixture {
    let g = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::ColorMatte { color: [0.1, 0.1, 0.1, 1.0] },
        W as u32,
        H as u32,
        RATE,
        RATE.tick_of(frames),
    );
    let info = filmcraft_media::MediaSource::info(&g).clone();
    let mut p = filmcraft_project::Project::new("magic");
    let item = p.add_item(
        "square",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::Generator(g.generator.clone()),
            info: info.clone(),
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
    let seq =
        p.new_sequence("s", filmcraft_project::SequenceSettings { width: W as u32, height: H as u32, frame_rate: RATE, ..Default::default() }, 1, 0, None);
    let ti = p
        .make_track_item(item, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, RATE.tick_of(frames)), RATE)
        .unwrap();
    let clip = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut s = Session { project: Arc::new(p), ..Default::default() };
    s.state.active_sequence = Some(seq);
    s.state.selection = vec![clip];
    s.media.insert(item, Arc::new(MovingSquare { info, vanish, neighbour }));
    s.set_playhead(Tick::ZERO);
    // (no memory check or resume delay: tests must not wait on the machine)
    s.background = BackgroundWorker::new(WorkerConfig { resume_delay: Duration::ZERO, scrub_window: Duration::ZERO, pace: None, memory: None });
    let calls = Arc::new(AtomicUsize::new(0));
    s.magic_mask.segmenter = Some(Arc::new(Bright { calls: calls.clone() }));
    let node = s.execute("grade.addNode", json!({"clip": clip.0})).unwrap()["node"].as_u64().unwrap();
    Fixture { s, clip, node, calls }
}

impl Fixture {
    fn run(&mut self, id: &str, p: Value) -> Value {
        let mut p = p;
        p["clip"] = json!(self.clip.0);
        p["node"] = json!(self.node);
        self.s.execute(id, p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }
    fn err(&mut self, id: &str, p: Value) -> String {
        let mut p = p;
        p["clip"] = json!(self.clip.0);
        p["node"] = json!(self.node);
        self.s.execute(id, p).unwrap_err().to_string()
    }
    /// A click on the square at frame `k` with a box around it.
    fn stroke_at(&mut self, k: i64) {
        let x0 = square_x(k, None).unwrap() as f32;
        let (cx, cy) = ((x0 + SIDE as f32 / 2.0) / W as f32, 48.0 / H as f32);
        let rect = [(x0 - 4.0) / W as f32, 32.0 / H as f32, (x0 + SIDE as f32 + 4.0) / W as f32, 64.0 / H as f32];
        self.run("grade.magicMask.stroke", json!({"frame": k, "points": [{"x": cx, "y": cy}], "rect": rect}));
    }
    fn track(&mut self, p: Value) -> Value {
        let r = self.run("grade.magicMask.track", p);
        assert!(self.s.background.wait_idle(Duration::from_secs(60)), "track did not finish");
        let job = r["job"].as_u64().unwrap();
        let st = self.s.magic_mask.statuses(Some(job));
        serde_json::to_value(&st[0]).unwrap()
    }
    fn matte(&self, k: i64) -> Option<Arc<Matte>> {
        magic_matte(&self.s, self.clip, self.node, RATE.tick_of(k))
    }
    fn mask_json(&mut self) -> Value {
        let g = self.s.execute("grade.get", json!({"clip": self.clip.0})).unwrap();
        g["grade"]["nodes"][0]["magicMask"].clone()
    }
}

#[test]
fn add_stroke_refine_remove_are_single_undo_steps() {
    let mut f = fixture(12, None);
    assert!(f.mask_json().is_null());
    f.run("grade.magicMask.add", json!({"mode": "person"}));
    assert_eq!(f.mask_json()["mode"], "person");
    assert!(f.err("grade.magicMask.add", json!({})).contains("already has a Magic Mask"));
    f.stroke_at(3);
    let m = f.mask_json();
    assert_eq!(m["strokes"].as_array().unwrap().len(), 1);
    assert_eq!(m["strokes"][0]["time"], RATE.tick_of(3).0);
    f.run("grade.magicMask.setRefine", json!({"grow": 1e9, "soften": 2.0, "invert": true}));
    let r = f.mask_json()["refine"].clone();
    assert_eq!(r["grow"], 100.0, "clamped: {r}");
    assert_eq!(r["invert"], true);
    f.s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(f.mask_json()["refine"]["invert"], false);
    f.s.execute("edit.undo", json!({})).unwrap();
    assert!(f.mask_json().get("strokes").is_none_or(Value::is_null));
    f.s.execute("edit.undo", json!({})).unwrap();
    assert!(f.mask_json().is_null());
    // a stroke on a node without a mask creates it; clear one frame's, then remove
    f.stroke_at(0);
    f.stroke_at(5);
    let r = f.run("grade.magicMask.clearStrokes", json!({"frame": 5}));
    assert_eq!(r["removed"], 1);
    assert_eq!(f.mask_json()["strokes"].as_array().unwrap().len(), 1);
    f.run("grade.magicMask.remove", json!({}));
    assert!(f.mask_json().is_null());
    f.s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(f.mask_json()["strokes"].as_array().unwrap().len(), 1);
}

#[test]
fn hostile_params_are_refused_in_plain_words() {
    let mut f = fixture(12, None);
    let cases = [
        ("grade.magicMask.add", json!({"mode": "blob"}), "`mode`"),
        ("grade.magicMask.stroke", json!({}), "need `points`"),
        ("grade.magicMask.stroke", json!({"points": [{"x": 2.0, "y": 0.5}]}), "outside the frame"),
        ("grade.magicMask.stroke", json!({"points": [{"x": "a", "y": 0.5}]}), "must be a number"),
        ("grade.magicMask.stroke", json!({"points": [{"x": 0.5, "y": 0.5, "positive": 3}]}), "true or false"),
        ("grade.magicMask.stroke", json!({"points": [1, 2]}), "each point"),
        ("grade.magicMask.stroke", json!({"points": "x"}), "list"),
        ("grade.magicMask.stroke", json!({"points": vec![json!({"x": 0.5, "y": 0.5}); 65]}), "at most 64"),
        ("grade.magicMask.stroke", json!({"rect": [0.1, 0.2, 0.3]}), "`rect`"),
        ("grade.magicMask.stroke", json!({"rect": [0.1, 0.2, 0.3, -1e300]}), "outside the frame"),
        ("grade.magicMask.setRefine", json!({"grow": 1}), "no Magic Mask"),
        ("grade.magicMask.clearStrokes", json!({}), "no Magic Mask"),
        ("grade.magicMask.remove", json!({}), "no Magic Mask"),
        ("grade.magicMask.track", json!({}), "no Magic Mask"),
    ];
    for (id, p, want) in cases {
        let e = f.err(id, p.clone());
        assert!(e.contains(want), "{id} {p}: {e}");
    }
    f.run("grade.magicMask.add", json!({}));
    let more = [
        ("grade.magicMask.setRefine", json!({}), "need one of"),
        ("grade.magicMask.setRefine", json!({"grow": "big"}), "`grow` must be a number"),
        ("grade.magicMask.setRefine", json!({"invert": 1}), "`invert`"),
        ("grade.magicMask.track", json!({}), "draw a stroke"),
    ];
    for (id, p, want) in more {
        let e = f.err(id, p.clone());
        assert!(e.contains(want), "{id} {p}: {e}");
    }
    f.stroke_at(0);
    for (p, want) in [
        (json!({"direction": "sideways"}), "direction"),
        (json!({"direction": 5}), "direction"),
        (json!({"fromTime": 100, "toTime": 50}), "range is empty"),
        (json!({"fromTime": i64::MAX - 10}), "range"),
    ] {
        let e = f.err("grade.magicMask.track", p.clone());
        assert!(e.contains(want), "{p}: {e}");
    }
    let e = f.s.execute("grade.magicMask.stroke", json!({"clip": f.clip.0, "node": 999, "points": [{"x": 0.5, "y": 0.5}]})).unwrap_err().to_string();
    assert!(e.contains("no node 999"), "{e}");
    let e = f.s.execute("grade.magicMask.cancel", json!({"job": 12345})).unwrap_err().to_string();
    assert!(e.contains("no Magic Mask track"), "{e}");
    // nothing above changed the project but the add and the stroke
    assert_eq!(f.mask_json()["strokes"].as_array().unwrap().len(), 1);
    // enabled(): no clip, no node
    let spec = crate::commands::find("grade.magicMask.stroke").unwrap();
    assert!((spec.enabled)(&Session::default()).is_err());
    assert!((spec.enabled)(&f.s).is_ok());
}

#[test]
fn track_follows_a_moving_square_on_every_frame() {
    let mut f = fixture(24, None);
    f.stroke_at(0);
    let st = f.track(json!({}));
    assert_eq!(st["state"], "done", "{st}");
    assert_eq!(st["frames"], 24);
    assert_eq!(st["segmented"], 24);
    assert!(st["notes"].as_array().unwrap().is_empty(), "{st}");
    for k in 0..24 {
        let m = f.matte(k).unwrap_or_else(|| panic!("frame {k} not tracked"));
        let iou = m.iou(&truth(k)).unwrap();
        assert!(iou > 0.8, "frame {k}: IoU {iou}");
    }
    // refine is applied at use: no new model run
    let before = f.calls.load(Ordering::SeqCst);
    f.run("grade.magicMask.setRefine", json!({"invert": true}));
    let m = f.matte(5).unwrap();
    assert!(m.iou(&truth(5)).unwrap() < 0.1);
    assert_eq!(m.alpha[0], 255);
    assert_eq!(f.calls.load(Ordering::SeqCst), before);
    // tracking again reuses every frame
    let st = f.track(json!({}));
    assert_eq!((st["segmented"].as_u64(), st["reused"].as_u64()), (Some(0), Some(24)), "{st}");
}

/// A differently coloured object that the fake segmenter also finds touches the square: the
/// follow carves it off (the old box-and-prior follow let the box grow into it).
#[test]
fn the_follow_keeps_the_subject_when_a_differently_coloured_object_touches_it() {
    let mut f = fixture_with(18, None, true);
    f.stroke_at(0);
    let st = f.track(json!({}));
    assert_eq!(st["state"], "done", "{st}");
    assert_eq!(st["segmented"], 18, "{st}");
    for k in 0..18 {
        let m = f.matte(k).unwrap_or_else(|| panic!("frame {k} not tracked"));
        let iou = m.iou(&truth(k)).unwrap();
        assert!(iou > 0.9, "frame {k}: IoU {iou}");
        let edge = square_x(k, None).unwrap() + SIDE + 1;
        let leaked = (0..W * H).filter(|i| i % W >= edge && m.alpha[*i] >= 128).count();
        assert_eq!(leaked, 0, "frame {k}: {leaked} pixels of the red object");
    }
}

#[test]
fn a_fix_stroke_reruns_only_the_frames_up_to_its_neighbours() {
    let mut f = fixture(24, None);
    f.stroke_at(0);
    f.stroke_at(23);
    let st = f.track(json!({}));
    assert_eq!(st["segmented"], 24, "{st}");
    f.calls.store(0, Ordering::SeqCst);
    f.stroke_at(12);
    // the frames 12 now owns (7..=17: nearest stroke, ties to the earlier) are gone, the rest kept
    for k in 0..24 {
        assert_eq!(f.matte(k).is_some(), !(7..=17).contains(&k), "frame {k}");
    }
    let st = f.track(json!({}));
    assert_eq!(f.calls.load(Ordering::SeqCst), 11, "{st}");
    assert_eq!(st["reused"], 13, "{st}");
    for k in 0..24 {
        assert!(f.matte(k).unwrap().iou(&truth(k)).unwrap() > 0.8, "frame {k}");
    }
    // forward only from a range: just those frames
    f.run("grade.magicMask.clearStrokes", json!({}));
    assert!(f.matte(0).is_none() && f.matte(20).is_none());
    f.stroke_at(4);
    f.calls.store(0, Ordering::SeqCst);
    let st = f.track(json!({"direction": "forward", "fromTime": RATE.tick_of(4).0, "toTime": RATE.tick_of(10).0}));
    assert_eq!(f.calls.load(Ordering::SeqCst), 6, "{st}");
    assert!(f.matte(3).is_none() && f.matte(9).is_some() && f.matte(10).is_none());
}

#[test]
fn a_lost_subject_keeps_the_last_good_matte_and_says_where() {
    let mut f = fixture(16, Some(10));
    f.stroke_at(0);
    let st = f.track(json!({}));
    assert_eq!(st["state"], "done", "{st}");
    let notes = st["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{st}");
    assert!(notes[0].as_str().unwrap().contains("lost the subject at 0.42 s"), "{st}");
    let last = f.matte(9).unwrap();
    for k in 10..16 {
        assert!(Arc::ptr_eq(&f.matte(k).unwrap(), &last), "frame {k} holds the last good matte");
    }
}

#[test]
fn cancel_stops_a_track() {
    let mut f = fixture(24, None);
    f.stroke_at(0);
    f.s.background.pause();
    let r = f.run("grade.magicMask.track", json!({}));
    let job = r["job"].as_u64().unwrap();
    assert_eq!(f.s.execute("grade.magicMask.cancel", json!({"job": job})).unwrap()["cancelled"], 1);
    f.s.background.resume();
    assert!(f.s.background.wait_idle(Duration::from_secs(30)));
    let st = &f.s.magic_mask.statuses(Some(job))[0];
    assert_eq!(st.state, TrackState::Cancelled);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.s.magic_mask.cache.frames(f.clip, f.node), 0);
    // a new track replaces a running one of the same node
    f.s.background.pause();
    let a = f.run("grade.magicMask.track", json!({}))["job"].as_u64().unwrap();
    let b = f.run("grade.magicMask.track", json!({}))["job"].as_u64().unwrap();
    f.s.background.resume();
    assert!(f.s.background.wait_idle(Duration::from_secs(60)));
    assert_eq!(f.s.magic_mask.statuses(Some(a))[0].state, TrackState::Cancelled);
    assert_eq!(f.s.magic_mask.statuses(Some(b))[0].state, TrackState::Done);
}

#[test]
fn a_missing_model_is_a_plain_error() {
    let mut f = fixture(4, None);
    f.stroke_at(0);
    f.s.magic_mask.segmenter = None;
    let dir = crate::media_test_util::tmp_dir("magicmask-model");
    f.s.magic_mask.sam2_dir = Some(dir.join("sam2"));
    f.s.magic_mask.model_path = Some(dir.join("mobilesam").join("mobile_sam.pt"));
    let e = f.err("grade.magicMask.track", json!({}));
    assert!(e.contains("the Magic Mask model is not installed"), "{e}");
    assert!(e.contains("put mobile_sam.pt in"), "{e}");
    assert!(!e.contains("Err(") && !e.contains("panick"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn old_projects_without_a_magic_mask_are_unaffected() {
    let mut f = fixture(4, None);
    let before = f.s.project.to_json();
    assert!(f.matte(0).is_none());
    assert!(!serde_json::to_string(&f.s.execute("grade.get", json!({"clip": f.clip.0})).unwrap()).unwrap().contains("magicMask"));
    assert!(f.err("grade.magicMask.setRefine", json!({"grow": 1})).contains("no Magic Mask"));
    assert_eq!(f.s.project.to_json(), before);
    let st = f.s.execute("grade.magicMask.status", json!({})).unwrap();
    assert_eq!(st["tracks"], json!([]));
}

#[test]
fn the_cache_is_byte_capped_and_owners_tie_to_the_earlier_stroke() {
    let t = |v| Tick(v);
    assert_eq!(owner(&[], t(5)), None);
    assert_eq!(owner(&[t(0), t(10)], t(5)), Some(t(0)));
    assert_eq!(owner(&[t(0), t(10)], t(6)), Some(t(10)));
    assert_eq!(owner(&[t(0), t(10)], t(-50)), Some(t(0)));
    assert_eq!(owner(&[t(0), t(10)], t(i64::MAX)), Some(t(10)));
    assert_eq!(owner(&[t(i64::MIN), t(i64::MAX)], t(0)), Some(t(i64::MIN)));

    // a tiny cap: tracking 24 frames of 160 × 96 keeps only the newest few
    let mut f = fixture(24, None);
    f.s.magic_mask.cache = Arc::new(MatteCache::with_cap(5 * (W * H + 64)));
    f.stroke_at(0);
    let st = f.track(json!({}));
    assert_eq!(st["state"], "done", "{st}");
    assert!(f.s.magic_mask.cache.bytes() <= 5 * (W * H + 64));
    let kept = f.s.magic_mask.cache.frames(f.clip, f.node);
    assert!((1..=5).contains(&kept), "{kept}");
    assert!(f.matte(23).is_some() && f.matte(0).is_none());
}

impl Fixture {
    fn motion_scale(&mut self, v: f64) {
        let seq = self.s.state.active_sequence.unwrap();
        let p = Arc::make_mut(&mut self.s.project);
        let it = &mut p.sequence_mut(seq).unwrap().video_tracks[0].items[0];
        it.effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = filmcraft_project::ParamValue::Float(v);
    }
}

/// Review fix 1: frames holding the last good matte share it and are charged a few bytes; at
/// most MAX_HELD_FRAMES of them per direction.
#[test]
fn held_frames_share_the_matte_and_are_capped() {
    let mut f = fixture(16, Some(10));
    f.stroke_at(0);
    f.track(json!({}));
    assert_eq!(f.s.magic_mask.cache.bytes(), 10 * (64 + W * H) + 6 * 64);
    let n = 10 + MAX_HELD_FRAMES as i64 + 6;
    let mut f = fixture(n, Some(10));
    f.stroke_at(0);
    let st = f.track(json!({}));
    assert_eq!(st["state"], "done", "{st}");
    assert_eq!(st["done"], n, "{st}");
    let last_held = 9 + MAX_HELD_FRAMES as i64;
    assert!(f.matte(last_held).is_some() && f.matte(last_held + 1).is_none());
    let note = format!("held the last good matte for {MAX_HELD_FRAMES} frames");
    assert!(st["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains(&note)), "{st}");
}

/// Review fix 2: editing the strokes stops a running track of the node.
#[test]
fn a_stroke_edit_supersedes_a_running_track() {
    let mut f = fixture(24, None);
    f.stroke_at(0);
    f.s.background.pause();
    let job = f.run("grade.magicMask.track", json!({}))["job"].as_u64().unwrap();
    f.stroke_at(5);
    f.s.background.resume();
    assert!(f.s.background.wait_idle(Duration::from_secs(30)));
    let st = &f.s.magic_mask.statuses(Some(job))[0];
    assert_eq!(st.state, TrackState::Superseded, "{st:?}");
    assert!(st.notes.iter().any(|n| n.contains("strokes changed")), "{st:?}");
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    // a refine change doesn't stop it; clearStrokes / remove do
    f.s.background.pause();
    let job = f.run("grade.magicMask.track", json!({}))["job"].as_u64().unwrap();
    f.run("grade.magicMask.setRefine", json!({"grow": 2.0}));
    f.s.background.resume();
    assert!(f.s.background.wait_idle(Duration::from_secs(60)));
    assert_eq!(f.s.magic_mask.statuses(Some(job))[0].state, TrackState::Done);
    for cmd in ["grade.magicMask.clearStrokes", "grade.magicMask.remove"] {
        f.stroke_at(0);
        f.s.background.pause();
        let job = f.run("grade.magicMask.track", json!({}))["job"].as_u64().unwrap();
        f.run(cmd, json!({}));
        f.s.background.resume();
        assert!(f.s.background.wait_idle(Duration::from_secs(30)));
        let st = f.s.magic_mask.statuses(Some(job))[0].state;
        assert!(matches!(st, TrackState::Superseded | TrackState::Cancelled), "{cmd}: {st:?}");
    }
}

/// Review fix 3: a Motion change after tracking hides the mattes (until tracked again).
#[test]
fn a_motion_change_hides_the_mattes() {
    let mut f = fixture(12, None);
    f.stroke_at(0);
    f.track(json!({}));
    assert!(f.matte(4).is_some());
    f.motion_scale(50.0);
    assert!(f.matte(4).is_none(), "the picture moved: the matte no longer lines up");
    f.motion_scale(100.0);
    assert!(f.matte(4).is_some(), "back as tracked");
    f.motion_scale(50.0);
    let st = f.track(json!({}));
    // (re-segmented, nothing reused; the old stroke box misses the shrunken square, which the
    // note reports)
    assert_eq!(st["reused"], 0, "{st}");
    assert!(st["segmented"].as_u64().unwrap() >= 1, "{st}");
}

/// Review fix 5: undoing a stroke shows the mattes of the strokes before it again, at once.
#[test]
fn undo_of_a_stroke_shows_the_old_mattes_again() {
    let mut f = fixture(24, None);
    f.stroke_at(0);
    f.stroke_at(23);
    f.track(json!({}));
    let before: Vec<_> = (0..24).map(|k| f.matte(k).unwrap()).collect();
    f.stroke_at(12);
    assert!(f.matte(12).is_none());
    f.s.execute("edit.undo", json!({})).unwrap();
    let calls = f.calls.load(Ordering::SeqCst);
    for (k, m) in before.iter().enumerate() {
        assert!(Arc::ptr_eq(&f.matte(k as i64).unwrap(), m), "frame {k}");
    }
    assert_eq!(f.calls.load(Ordering::SeqCst), calls);
}

/// Review fix 6: a short range far from the clip start is tracked without walking the clip.
#[test]
fn only_the_frames_between_the_strokes_and_the_range_are_visited() {
    let mut f = fixture(48, None);
    f.stroke_at(30);
    let st = f.track(json!({"fromTime": RATE.tick_of(30).0, "toTime": RATE.tick_of(36).0}));
    assert_eq!(st["frames"], 6, "{st}");
    assert!(f.matte(29).is_none() && f.matte(35).is_some());
}

/// Review fix 7: a stroke at a time outside the clip is refused in plain words.
#[test]
fn a_stroke_outside_the_clip_is_refused() {
    let mut f = fixture(12, None);
    for k in [12, 500] {
        let e = f.err("grade.magicMask.stroke", json!({"frame": k, "points": [{"x": 0.5, "y": 0.5}]}));
        assert!(e.contains("outside the clip"), "{e}");
    }
    assert!(f.mask_json().is_null(), "nothing was added");
}

/// Part B: the node's correction shows only inside the tracked subject on the Program frame;
/// a render without the session's matte token changes the whole picture.
#[test]
fn the_program_frame_limits_the_node_to_the_subject() {
    let mut f = fixture(12, None);
    f.stroke_at(0);
    f.track(json!({}));
    let k = 4;
    let t = RATE.tick_of(k);
    let px = |img: &filmcraft_render::Image, x: usize, y: usize| img.px[(y * img.w + x) * 4];
    let plain = f.s.render_program_at(1.0, t).unwrap();
    f.run("grade.setParam", json!({"param": "offset", "value": [-0.1, -0.1, -0.1, -0.1]}));
    let graded = f.s.render_program_at(1.0, t).unwrap();
    let (cx, cy) = (square_x(k, None).unwrap() + SIDE / 2, 48);
    let (ox, oy) = (W - 10, 10);
    assert!((px(&graded, cx, cy) - px(&plain, cx, cy)).abs() > 0.01, "inside: corrected");
    assert!((px(&graded, ox, oy) - px(&plain, ox, oy)).abs() < 1e-4, "outside: untouched");
    // without the token (no mattes at all) the node changes nothing, not the whole picture
    let seq = f.s.state.active_sequence.unwrap();
    let provider = f.s.media.provider(f.s.project.clone(), f.s.services.clone());
    let none = filmcraft_render::render_sequence(&f.s.project, seq, t, filmcraft_render::RenderOptions::default(), &provider);
    assert!((px(&none, cx, cy) - px(&plain, cx, cy)).abs() < 1e-4 && (px(&none, ox, oy) - px(&plain, ox, oy)).abs() < 1e-4);
    // ...unlike without the mask, where it does change the background
    let mut whole = (*f.s.project).clone();
    whole.sequence_mut(seq).unwrap().video_tracks[0].items[0].grade.as_mut().unwrap().nodes[0].magic_mask = None;
    let whole = filmcraft_render::render_sequence(&whole, seq, t, filmcraft_render::RenderOptions::default(), &provider);
    assert!((px(&whole, ox, oy) - px(&plain, ox, oy)).abs() > 1e-3, "the correction does reach the background");
}

/// Review 2, fix 2: a frame not tracked (or tracked for another picture) shows the node's
/// correction nowhere.
#[test]
fn an_untracked_frame_shows_the_node_nowhere() {
    let mut f = fixture(12, None);
    f.stroke_at(0);
    let t = RATE.tick_of(4);
    let plain = f.s.render_program_at(1.0, t).unwrap();
    f.run("grade.setParam", json!({"param": "offset", "value": [-0.1, -0.1, -0.1, -0.1]}));
    let untracked = f.s.render_program_at(1.0, t).unwrap();
    assert!(untracked.px.iter().zip(&plain.px).all(|(a, b)| (a - b).abs() < 1e-4));
    f.track(json!({}));
    assert!(f.s.render_program_at(1.0, t).unwrap().px.iter().zip(&plain.px).any(|(a, b)| (a - b).abs() > 0.01));
    f.motion_scale(50.0);
    let seq = f.s.state.active_sequence.unwrap();
    let mut ungraded = (*f.s.project).clone();
    ungraded.sequence_mut(seq).unwrap().video_tracks[0].items[0].grade = None;
    let provider = f.s.media.provider(f.s.project.clone(), f.s.services.clone());
    let moved_plain = filmcraft_render::render_sequence(&ungraded, seq, t, filmcraft_render::RenderOptions::default(), &provider);
    let moved = f.s.render_program_at(1.0, t).unwrap();
    assert!(moved.px.iter().zip(&moved_plain.px).all(|(a, b)| (a - b).abs() < 1e-4), "fingerprint changed: nowhere");
}

/// Review 2, fix 1: a render-cache segment showing the node gets a new hash once the node is
/// tracked (a part rendered before is not reused); other segments keep theirs.
#[test]
fn tracking_changes_the_render_cache_hash_of_its_segment() {
    let mut f = fixture(12, None);
    f.stroke_at(0);
    let seq = f.s.state.active_sequence.unwrap();
    let before = f.s.previews.segments(&f.s.project, seq)[0].hash.clone();
    f.track(json!({}));
    let after = f.s.previews.segments(&f.s.project, seq)[0].hash.clone();
    assert_ne!(before, after);
    assert_eq!(after.len(), 32);
    assert_eq!(f.s.previews.segments(&f.s.project, seq)[0].hash, after, "stable while nothing changes");
    // dropping its frames moves it again
    f.s.magic_mask.cache.invalidate_node(f.clip, f.node);
    assert_ne!(f.s.previews.segments(&f.s.project, seq)[0].hash, after);
    // a clip without a Magic Mask keeps its hash
    let g = fixture(12, None);
    let seq = g.s.state.active_sequence.unwrap();
    let h = g.s.previews.segments(&g.s.project, seq)[0].hash.clone();
    g.s.previews.set_mattes(f.s.magic_mask.cache.clone());
    assert_eq!(g.s.previews.segments(&g.s.project, seq)[0].hash, h);
}

/// Review 2, fixes 3, 4 and 6: an export's frozen mattes don't change when the live cache does;
/// a frame converts once; removing the mask forgets its generation.
#[test]
fn an_export_snapshot_is_frozen_and_conversions_are_kept() {
    let mut f = fixture(12, None);
    f.stroke_at(0);
    f.track(json!({}));
    let seq = f.s.state.active_sequence.unwrap();
    let project = f.s.project.clone();
    let (_, item) = project.sequence(seq).unwrap().find_item(f.clip).unwrap();
    let node = item.grade.as_ref().unwrap().node(f.node).unwrap();
    let t = RATE.tick_of(3);
    let live = filmcraft_render::matte::source(f.s.magic_mask.token()).unwrap();
    let a = live.matte(&project, item, node, t).unwrap();
    assert!(Arc::ptr_eq(&a, &live.matte(&project, item, node, t).unwrap()), "converted once");
    let snap = f.s.magic_mask.snapshot_registration();
    f.s.magic_mask.cache.invalidate_node(f.clip, f.node);
    assert!(live.matte(&project, item, node, t).is_none());
    let frozen = filmcraft_render::matte::source(snap.token()).unwrap();
    assert_eq!(frozen.matte(&project, item, node, t).unwrap().alpha, a.alpha);
    let token = snap.token();
    drop((frozen, snap));
    assert!(filmcraft_render::matte::source(token).is_none(), "dropping the handle unregisters");
    f.run("grade.magicMask.remove", json!({}));
    assert_eq!(f.s.magic_mask.cache.generation(f.clip, f.node), 0, "forgotten");
}

/// Held by tests that load SAM 2.1 or read its process-wide cache (`magic_mask_sam2::LOADED`,
/// `FAIL_NEXT`): one loading the other size (or bogus files) clears it under another's feet.
pub(crate) static SAM2_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn sam2_choices_are_checked_in_plain_words() {
    let _g = SAM2_GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut f = fixture(4, None);
    f.stroke_at(0);
    let e = f.err("grade.magicMask.track", json!({"model": "huge"}));
    assert!(e.contains("unknown model `huge`"), "{e}");
    let e = f.err("grade.magicMask.track", json!({"model": 3}));
    assert!(e.contains("`model` must be"), "{e}");
    // an installed segmenter (a host's / a test's) always wins over SAM 2.1
    let st = f.track(json!({"model": "better"}));
    assert_eq!(st["state"], "done", "{st}");
    // without one, Better / Faster need their files
    f.s.magic_mask.segmenter = None;
    let dir = crate::media_test_util::tmp_dir("magicmask-sam2");
    f.s.magic_mask.sam2_dir = Some(dir.join("sam2"));
    f.s.magic_mask.model_path = Some(dir.join("mobilesam").join("mobile_sam.pt"));
    let e = f.err("grade.magicMask.track", json!({"model": "faster"}));
    if cfg!(feature = "magic-mask") {
        assert!(e.contains("Faster (SAM 2.1 Hiera Base+) is not installed") && e.contains("sam2.1_hiera_base_plus.pt"), "{e}");
    } else {
        assert!(e.contains("without the `magic-mask` feature"), "{e}");
    }
    // Auto without SAM 2.1 asks for MobileSAM, as before
    let e = f.err("grade.magicMask.track", json!({}));
    assert!(e.contains("put mobile_sam.pt in"), "{e}");
    // with SAM 2.1 Large installed, Auto plans Better (Q66); these fake files then fail to load on
    // the worker and the track falls back to MobileSAM, which is missing here
    let sam2 = dir.join("sam2");
    std::fs::create_dir_all(&sam2).unwrap();
    for size in [filmcraft_mask::sam2::Sam2Size::Large, filmcraft_mask::sam2::Sam2Size::BasePlus] {
        let (pt, yaml) = size.files();
        std::fs::write(sam2.join(pt), b"x").unwrap();
        std::fs::write(sam2.join(yaml), b"x").unwrap();
    }
    let r = f.s.execute("grade.magicMask.track", json!({"model": "auto"}));
    if cfg!(feature = "magic-mask") {
        // the track starts (SAM 2.1 loads on the worker; these files fail and it falls back)
        assert!(r.as_ref().is_ok_and(|v| v["job"].is_number()), "{r:?}");
    } else {
        assert!(r.is_err(), "{r:?}");
    }
    let st = f.s.execute("grade.magicMask.status", json!({})).unwrap();
    assert_eq!(st["tracker"], "auto");
    let _ = std::fs::remove_dir_all(dir);
}

/// SAM 2.1 Base+ (Faster) tracks the moving square through the engine, on the GPU, without
/// MobileSAM installed. Skips without the checkpoint or a GPU.
#[cfg(feature = "magic-mask")]
#[test]
fn sam2_faster_tracks_the_square_on_every_frame() {
    let _g = SAM2_GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    use filmcraft_mask::sam2::{Sam2Size, tests_support};
    let Some((pt, _)) = tests_support::files(Sam2Size::BasePlus) else { return };
    let mut f = fixture(12, None);
    f.stroke_at(0);
    f.s.magic_mask.segmenter = None;
    f.s.magic_mask.sam2_dir = pt.parent().map(|p| p.to_path_buf());
    let dir = crate::media_test_util::tmp_dir("magicmask-sam2-run");
    f.s.magic_mask.model_path = Some(dir.join("mobilesam").join("mobile_sam.pt"));
    let r = f.run("grade.magicMask.track", json!({"model": "faster"}));
    assert!(f.s.background.wait_idle(Duration::from_secs(600)), "track did not finish");
    let st = serde_json::to_value(&f.s.magic_mask.statuses(r["job"].as_u64())[0]).unwrap();
    if st["state"] == "failed" && st["error"].as_str().is_some_and(|e| e.contains("GPU")) {
        eprintln!("skipped: {st}");
        return;
    }
    assert_eq!(st["state"], "done", "{st}");
    assert!(st["notes"].as_array().unwrap().iter().any(|n| n.as_str().is_some_and(|n| n.contains("SAM 2.1"))), "{st}");
    for k in 0..12 {
        let m = f.matte(k).unwrap_or_else(|| panic!("frame {k} not tracked"));
        let iou = m.iou(&truth(k)).unwrap();
        assert!(iou > 0.8, "frame {k}: IoU {iou}");
    }
    let _ = std::fs::remove_dir_all(dir);
    assert!(crate::magic_mask_sam2::is_loaded(Sam2Size::BasePlus));

    // The GPU failing mid-track: the model is forgotten and MobileSAM (when installed) goes on.
    let Some(mobile) = std::env::var_os("FILMCRAFT_MOBILESAM").map(std::path::PathBuf::from).filter(|p| p.is_file()) else {
        eprintln!("skipped the GPU-failure half: $FILMCRAFT_MOBILESAM not set");
        return;
    };
    let mut f = fixture(6, None);
    f.stroke_at(0);
    f.s.magic_mask.segmenter = None;
    f.s.magic_mask.sam2_dir = pt.parent().map(|p| p.to_path_buf());
    f.s.magic_mask.model_path = Some(mobile);
    crate::magic_mask_sam2::FAIL_NEXT.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = f.run("grade.magicMask.track", json!({"model": "faster"}));
    assert!(f.s.background.wait_idle(Duration::from_secs(600)), "track did not finish");
    let st = serde_json::to_value(&f.s.magic_mask.statuses(r["job"].as_u64())[0]).unwrap();
    assert_eq!(st["state"], "done", "{st}");
    assert!(st["notes"].as_array().unwrap().iter().any(|n| n.as_str().is_some_and(|n| n.contains("SAM 2.1 stopped"))), "{st}");
    assert!(!crate::magic_mask_sam2::is_loaded(Sam2Size::BasePlus));
    assert!(f.matte(3).is_some());
}

/// LO.6: the Magic Mask model is a preference: a saved choice comes back, an unknown or hostile
/// value loads as Auto (the rest of the preferences intact), and a changed preference sets the
/// session's model while an unrelated change leaves a model set directly alone.
#[test]
fn the_magic_mask_model_is_remembered_in_the_preferences() {
    use crate::autosave::Preferences;
    use crate::magic_mask::MaskModel;
    let mut p = Preferences::default();
    p.color.magic_mask = MaskModel::Faster;
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["color"]["magicMask"], json!("faster"));
    assert_eq!(Preferences::from_value(v.clone()).color.magic_mask, MaskModel::Faster);
    for bad in [json!("sam9"), json!(42), json!(null), json!({"a": [1]})] {
        let mut w = v.clone();
        w["color"]["magicMask"] = bad.clone();
        let q = Preferences::from_value(w);
        assert_eq!(q.color.magic_mask, MaskModel::Auto, "{bad}");
        assert_eq!(q.color.hdr_graphics_white, p.color.hdr_graphics_white, "{bad}: the rest loads");
    }
    let mut s = Session::default();
    let mut next = s.prefs.clone();
    next.color.magic_mask = MaskModel::Better;
    s.set_prefs(next).unwrap();
    assert_eq!(s.magic_mask.model, MaskModel::Better);
    s.magic_mask.model = MaskModel::MobileSam;
    let mut next = s.prefs.clone();
    next.color.coach.hints = !next.color.coach.hints;
    s.set_prefs(next).unwrap();
    assert_eq!(s.magic_mask.model, MaskModel::MobileSam, "an unrelated preference change keeps it");
}

/// LO.6: a Magic Mask on a nested sequence's clip is hidden by edits inside the nest (they change
/// its picture), not only by edits of the nest clip itself; nests inside nests count too.
#[test]
fn edits_inside_a_nested_sequence_change_the_picture_fingerprint() {
    use crate::magic_mask::picture_fingerprint;
    use filmcraft_project::ItemId;
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let first = s.active_sequence().and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id.0).unwrap();
    s.execute("timeline.select", json!({"clips": [first]})).unwrap();
    let inner = ItemId(s.execute("clip.nest", json!({"name": "Inner"})).unwrap()["sequence"].as_u64().unwrap());
    let fp = |s: &Session| {
        let q = s.active_sequence().unwrap();
        let it = q.video_tracks.iter().flat_map(|t| &t.items).find(|i| i.item == inner).expect("the nest clip");
        picture_fingerprint(&s.project, it)
    };
    let before = fp(&s);
    assert_eq!(fp(&s), before, "stable");
    // an edit inside the nest: its clip plays at half speed
    let mut p = (*s.project).clone();
    let q = p.sequence_mut(inner).unwrap();
    q.video_tracks[0].items[0].speed = 0.5;
    s.project = Arc::new(p);
    let after = fp(&s);
    assert_ne!(after, before, "an edit inside the nest changes its picture");
    // the nest's own audio doesn't
    let mut p = (*s.project).clone();
    let q = p.sequence_mut(inner).unwrap();
    q.audio_tracks.iter_mut().for_each(|t| t.items.clear());
    s.project = Arc::new(p);
    assert_eq!(fp(&s), after, "audio inside the nest leaves the picture alone");
}

/// Person mode asks for the whole figure: a segmenter whose candidates for an ambiguous prompt
/// are the subject's left half (best scored), the whole subject (a little lower) and the whole
/// frame (far lower). One click on the square, no box: Object keeps the half, Person the whole
/// square (fork LO.6; the mode used to change only the cache hash).
struct Parts(Bright);

impl Segmenter for Parts {
    fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError> {
        self.0.embed(image)
    }
    fn segment(&self, e: &Embedding, prompt: &Prompt) -> Result<Segment, MaskError> {
        filmcraft_mask::segment_by_candidates(self, e, prompt)
    }
    fn candidates(&self, e: &Embedding, prompt: &Prompt, multimask: bool) -> Result<Vec<Segment>, MaskError> {
        let mut whole = self.0.segment(e, prompt)?;
        if !multimask {
            return Ok(vec![whole]);
        }
        let xs: Vec<usize> = whole.logits.iter().enumerate().filter(|(_, v)| **v > 0.0).map(|(i, _)| i % LOW_RES).collect();
        let mid = (xs.iter().min().copied().unwrap_or(0) + xs.iter().max().copied().unwrap_or(0)) / 2;
        let mut part = whole.clone();
        part.logits.iter_mut().enumerate().filter(|(i, _)| i % LOW_RES > mid).for_each(|(_, v)| *v = -10.0);
        part.score = 0.95;
        whole.score = 0.9;
        let mut all = Segment::empty((1.0, 1.0));
        all.logits.iter_mut().for_each(|v| *v = 10.0);
        all.score = 0.4;
        Ok(vec![part, whole, all])
    }
}

#[test]
fn person_mode_asks_for_the_whole_figure() {
    let iou = |mode: &str| {
        let mut f = fixture(3, None);
        f.s.magic_mask.segmenter = Some(Arc::new(Parts(Bright { calls: f.calls.clone() })));
        f.run("grade.magicMask.add", json!({"mode": mode}));
        let x0 = square_x(0, None).unwrap() as f32;
        let (cx, cy) = ((x0 + SIDE as f32 / 2.0) / W as f32, 48.0 / H as f32);
        f.run("grade.magicMask.stroke", json!({"frame": 0, "points": [{"x": cx, "y": cy}]}));
        let st = f.track(json!({}));
        assert_eq!(st["state"], "done", "{st}");
        f.matte(0).expect("frame 0 tracked").iou(&truth(0)).unwrap()
    };
    let (object, person) = (iou("object"), iou("person"));
    assert!(object < 0.7, "Object keeps the best-scored part: IoU {object}");
    assert!(person > 0.9, "Person keeps the whole square: IoU {person}");
}
