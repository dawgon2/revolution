//! Fork G4.4 acceptance (synthetic): a grade window follows a plane moving in perspective.

use serde_json::json;

use crate::Session;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::ClipId;
use filmcraft_render::track::Homography;
use filmcraft_time::{FrameRate, Tick};

const W: usize = 480;
const H: usize = 320;
const FRAMES: i64 = 60;
const RATE: FrameRate = FrameRate::FPS_24;

/// The plane's pose at frame `k`: object coordinates (a 160 px square, centred) → clip pixels,
/// drifting right, turning, growing and tilting in perspective.
fn pose(k: f64) -> Homography {
    let a = Affine::motion(Vec2::new(150.0 + 3.0 * k, 160.0 + 15.0 * (k / 9.0).sin()), Vec2::new(1.0 + 0.004 * k, 1.0 + 0.004 * k), 0.4 * k, Vec2::ZERO);
    let affine = Homography { m: [a.a, a.c, a.e, a.b, a.d, a.f, 0.0, 0.0, 1.0] };
    let tilt = Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0012 * (k / 10.0).sin(), 0.0009 * (k / 13.0).cos() - 0.0004, 1.0] };
    affine.after(&tilt)
}

fn hash(ix: i64, iy: i64, seed: u64) -> f64 {
    let mut x = (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f64 / (1u64 << 24) as f64
}

/// Smooth value noise, -1..1.
fn noise(u: f64, v: f64, seed: u64) -> f64 {
    let (iu, iv) = (u.floor(), v.floor());
    let (fu, fv) = (u - iu, v - iv);
    let (su, sv) = (fu * fu * (3.0 - 2.0 * fu), fv * fv * (3.0 - 2.0 * fv));
    let (iu, iv) = (iu as i64, iv as i64);
    let top = hash(iu, iv, seed) + (hash(iu + 1, iv, seed) - hash(iu, iv, seed)) * su;
    let bot = hash(iu, iv + 1, seed) + (hash(iu + 1, iv + 1, seed) - hash(iu, iv + 1, seed)) * su;
    2.0 * (top + (bot - top) * sv) - 1.0
}

/// A poster on a wall: the textured square under `pose(k)` over a different static texture.
struct Plane {
    info: filmcraft_media::MediaInfo,
    /// From this frame on the picture is flat grey (the plane is gone: a planar track loses it).
    blank_from: Option<i64>,
}

impl filmcraft_media::MediaSource for Plane {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
        let k = RATE.frame_at(req.time) as f64;
        let inv = pose(k).inverse().unwrap();
        if self.blank_from.is_some_and(|b| k as i64 >= b) {
            return Ok(std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba8(W as u32, H as u32, [128u8, 128, 128, 255].repeat(W * H))));
        }
        let mut px = vec![0u8; W * H * 4];
        for y in 0..H {
            for x in 0..W {
                let mut acc = 0.0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let p = Vec2::new(x as f64 + ox, y as f64 + oy);
                    acc += match inv.apply(p) {
                        Some(o) if o.x.abs() < 80.0 && o.y.abs() < 80.0 => {
                            0.5 + 0.22 * noise(o.x / 9.0, o.y / 9.0, 1) + 0.14 * noise(o.x / 3.5, o.y / 3.5, 2) + 0.06 * (o.x * 0.21).sin() * (o.y * 0.17).cos()
                        }
                        _ => 0.3 + 0.12 * noise(p.x / 7.0 + 50.0, p.y / 7.0, 7) + 0.08 * noise(p.x / 2.5, p.y / 2.5 + 30.0, 8),
                    };
                }
                let v = (acc / 4.0 * 255.0).round().clamp(0.0, 255.0) as u8;
                px[(y * W + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        Ok(std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba8(W as u32, H as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

/// A session with one clip of the moving plane (`textured`) or of a flat grey matte.
fn session(textured: bool) -> (Session, ClipId) {
    use filmcraft_media::MediaSource;
    let g = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
        W as u32,
        H as u32,
        RATE,
        RATE.tick_of(FRAMES),
    );
    let info = g.info().clone();
    let mut p = filmcraft_project::Project::new("planar");
    let item = p.add_item(
        "plane",
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
        .make_track_item(item, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, RATE.tick_of(FRAMES)), RATE)
        .unwrap();
    let clip = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut s = Session { project: std::sync::Arc::new(p), ..Default::default() };
    s.state.active_sequence = Some(seq);
    s.state.selection = vec![clip];
    if textured {
        s.media.insert(item, std::sync::Arc::new(Plane { info, blank_from: None }));
    }
    (s, clip)
}

/// The window's corners in object coordinates (a 100 px square on the 160 px poster).
const OBJ: [Vec2; 4] = [Vec2::new(-50.0, -50.0), Vec2::new(50.0, -50.0), Vec2::new(50.0, 50.0), Vec2::new(-50.0, 50.0)];

/// A grade node with a four-corner window drawn on the poster at frame `at`.
fn windowed(s: &mut Session, clip: ClipId, at: i64) -> u64 {
    s.set_playhead(RATE.tick_of(at));
    let node = s.execute("grade.addNode", json!({"clip": clip.0})).unwrap()["node"].as_u64().unwrap();
    let corners: Vec<[f64; 2]> = OBJ.iter().map(|o| pose(at as f64).apply(*o).unwrap()).map(|p| [p.x, p.y]).collect();
    s.execute("masks.add", json!({"node": node, "shape": "polygon", "path": corners})).unwrap();
    node
}

fn window(s: &Session, clip: ClipId, node: u64) -> (filmcraft_project::Mask, filmcraft_project::TrackItem) {
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    (it.grade.as_ref().unwrap().node(node).unwrap().windows[0].clone(), it)
}

/// Largest distance of the shown window corners from the true ones at frame `k`.
fn corner_error(m: &filmcraft_project::Mask, it: &filmcraft_project::TrackItem, k: i64) -> f64 {
    let shown = filmcraft_render::mask::shown_path(m, it.source_time_at(RATE.tick_of(k)));
    shown.vertices.iter().zip(OBJ).map(|(v, o)| (v.p - pose(k as f64).apply(o).unwrap()).length()).fold(0.0, f64::max)
}

#[test]
fn planar_window_follows_a_perspective_plane_both_ways() {
    let (mut s, clip) = session(true);
    let node = windowed(&mut s, clip, 20);
    let r = s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "wait": true})).unwrap();
    assert_eq!(r["frames"], FRAMES - 1, "{r}");
    let (m, it) = window(&s, clip, node);
    let track = m.planar.as_ref().unwrap();
    assert_eq!((track.reference, track.keys.len()), (it.source_time_at(RATE.tick_of(20)), FRAMES as usize), "a key per frame");
    let errs: Vec<f64> = (0..FRAMES).map(|k| corner_error(&m, &it, k)).collect();
    let max = errs.iter().copied().fold(0.0, f64::max);
    let mean = errs.iter().sum::<f64>() / errs.len() as f64;
    eprintln!("planar window over {FRAMES} frames (from frame 20, both ways): corner error max {max:.4} px, mean of per-frame max {mean:.4} px");
    assert!(max < 0.2 && mean < 0.1, "max {max} mean {mean}");
    // the stored path is still the drawn one, its list entry says where it is seen
    let l = s.execute("masks.list", json!({"time": RATE.tick_of(50).0})).unwrap();
    let e = l["masks"].as_array().unwrap().iter().find(|e| e["node"].as_u64() == Some(node)).cloned().unwrap();
    assert_eq!(e["planar"]["keys"], FRAMES, "{e}");
    let shown = crate::masks::path_from_json(&e["shownPath"]).unwrap();
    assert!((shown.vertices[0].p - pose(50.0).apply(OBJ[0]).unwrap()).length() < 0.5);
    // one undo step takes the whole track off (and only it)
    s.undo();
    let (m, _) = window(&s, clip, node);
    assert!(m.planar.is_none());
    assert_eq!(m.path.keyframes.len(), 0);
}

#[test]
fn planar_window_grades_where_the_plane_is() {
    let (mut s, clip) = session(true);
    let node = windowed(&mut s, clip, 20);
    s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "direction": "forward", "wait": true})).unwrap();
    s.set_playhead(RATE.tick_of(50));
    let before = s.render_program(1.0).unwrap();
    s.execute("grade.setParam", json!({"clip": clip.0, "node": node, "param": "gain", "value": [0.0, 0.0, 0.0, 1.0]})).unwrap();
    let after = s.render_program(1.0).unwrap();
    let at = |img: &filmcraft_render::Image, p: Vec2| {
        let i = (p.y as usize * img.w + p.x as usize) * 4;
        [img.px[i], img.px[i + 1], img.px[i + 2]]
    };
    let here = pose(50.0).apply(Vec2::ZERO).unwrap();
    let drawn = pose(20.0).apply(Vec2::ZERO).unwrap();
    assert!(at(&after, here).iter().zip(at(&before, here)).any(|(a, b)| (a - b).abs() > 1e-3), "graded where the poster is at frame 50");
    assert_eq!(at(&before, drawn), at(&after, drawn), "not where the window was drawn");
}

#[test]
fn edits_and_clearing_keep_the_window_where_it_is_seen() {
    let (mut s, clip) = session(true);
    let node = windowed(&mut s, clip, 20);
    s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "wait": true})).unwrap();
    // a vertex dragged on the monitor at frame 45 lands where it was dropped
    s.set_playhead(RATE.tick_of(45));
    let (m, it) = window(&s, clip, node);
    let t45 = it.source_time_at(RATE.tick_of(45));
    let v0 = filmcraft_render::mask::shown_path(&m, t45).vertices[0].p;
    s.execute("masks.moveVertex", json!({"node": node, "mask": 0, "vertex": 0, "delta": [4.0, -3.0]})).unwrap();
    let (m, _) = window(&s, clip, node);
    assert!((filmcraft_render::mask::shown_path(&m, t45).vertices[0].p - (v0 + Vec2::new(4.0, -3.0))).length() < 1e-6);
    s.execute("masks.translate", json!({"node": node, "mask": 0, "delta": [-4.0, 3.0]})).unwrap();
    let (m, _) = window(&s, clip, node);
    let shown45 = filmcraft_render::mask::shown_path(&m, t45);
    assert!((shown45.vertices[0].p - v0).length() < 1e-6, "{:?} vs {v0:?}", shown45.vertices[0].p);
    // clearing at frame 45 leaves the window where it is seen there
    s.execute("masks.clearPlanar", json!({"node": node, "mask": 0})).unwrap();
    let (m, _) = window(&s, clip, node);
    assert!(m.planar.is_none());
    for (a, b) in m.path_at(t45).vertices.iter().zip(&shown45.vertices) {
        assert!((a.p - b.p).length() < 1e-6);
    }
    assert!(s.execute("masks.clearPlanar", json!({"node": node, "mask": 0})).is_err(), "nothing to clear");
}

#[test]
fn keyframed_windows_and_retracking_do_not_jump() {
    let (mut s, clip) = session(true);
    let node = windowed(&mut s, clip, 20);
    // a window animated before (path keyframes at frames 20 and 30, e.g. a point track) keeps
    // its keyframes where they were seen
    {
        let seq = s.state.active_sequence.unwrap();
        let p = std::sync::Arc::make_mut(&mut s.project);
        let it = &mut p.sequence_mut(seq).unwrap().video_tracks[0].items[0];
        let w = &mut it.grade.as_mut().unwrap().node_mut(node).unwrap().windows[0];
        for k in [20i64, 30] {
            let pts: Vec<Vec2> = OBJ.iter().map(|o| pose(k as f64).apply(*o).unwrap()).collect();
            let path = filmcraft_project::MaskPath::polygon(&pts);
            w.path.put_keyframe(RATE.tick_of(k), filmcraft_project::ParamValue::Path(path));
        }
    }
    let (m0, it) = window(&s, clip, node);
    assert_eq!(m0.path.keyframes.len(), 2);
    let keyed: Vec<Tick> = m0.path.keyframes.iter().map(|k| k.time).collect();
    s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "direction": "forward", "frames": 20, "wait": true})).unwrap();
    let (m, _) = window(&s, clip, node);
    for t in keyed {
        let a = m0.path_at(t);
        let b = filmcraft_render::mask::shown_path(&m, t);
        assert!(a.vertices.iter().zip(&b.vertices).all(|(x, y)| (x.p - y.p).length() < 1e-6), "keyframe at {t:?} moved");
    }
    // tracking on from frame 40 composes with the first track: frame 40 doesn't move
    let t40 = it.source_time_at(RATE.tick_of(40));
    let at40 = filmcraft_render::mask::shown_path(&m, t40);
    s.set_playhead(RATE.tick_of(40));
    s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "direction": "forward", "wait": true})).unwrap();
    let (m, it) = window(&s, clip, node);
    let again = filmcraft_render::mask::shown_path(&m, t40);
    assert!(at40.vertices.iter().zip(&again.vertices).all(|(x, y)| (x.p - y.p).length() < 1e-6));
    assert_eq!(m.planar.as_ref().unwrap().keys.len(), FRAMES as usize - 20, "frames 20..=59");
    assert!(corner_error(&m, &it, 59) < 0.3, "{}", corner_error(&m, &it, 59));
}

#[test]
fn planar_tracking_refuses_what_it_cannot_do() {
    // nothing to address
    let mut empty = Session::default();
    assert!((crate::commands::find("masks.trackPlanar").unwrap().enabled)(&empty).is_err());
    assert!(empty.execute("masks.trackPlanar", json!({})).is_err());
    let (mut s, clip) = session(true);
    assert!(s.execute("masks.trackPlanar", json!({"mask": 0})).is_err(), "no mask yet");
    let node = windowed(&mut s, clip, 20);
    for (p, why) in [
        (json!({"node": node, "mask": 0, "direction": "sideways"}), "direction"),
        (json!({"node": node, "mask": 0, "direction": 3}), "direction"),
        (json!({"node": node, "mask": 7}), "no mask 7"),
        (json!({"node": 999, "mask": 0}), "grade node"),
    ] {
        let e = s.execute("masks.trackPlanar", p).unwrap_err().to_string();
        assert!(e.contains(why), "{e}");
    }
    // from the last frame forward there is nothing to track
    s.set_playhead(RATE.tick_of(FRAMES - 1));
    assert!(s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "direction": "forward"})).is_err());
    // a flat picture has nothing to follow
    let (mut f, fclip) = session(false);
    let fnode = windowed(&mut f, fclip, 20);
    // (the job reports it: the command started it)
    let r = f.execute("masks.trackPlanar", json!({"node": fnode, "mask": 0, "wait": true})).unwrap();
    let e = r["status"]["result"]["error"].as_str().unwrap_or_default().to_string();
    assert!(e.contains("detail"), "{r}");
    assert!(window(&f, fclip, fnode).0.planar.is_none());
    // a frame hold doesn't move
    {
        let p = std::sync::Arc::make_mut(&mut s.project);
        let seq = s.state.active_sequence.unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items[0].frame_hold = Some(Tick::ZERO);
    }
    let e = s.execute("masks.trackPlanar", json!({"node": node, "mask": 0})).unwrap_err().to_string();
    assert!(e.contains("frame hold"), "{e}");
}

#[test]
fn planar_tracking_runs_in_the_background_and_stops() {
    let (mut s, clip) = session(true);
    let node = windowed(&mut s, clip, 0);
    let r = s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "direction": "forward"})).unwrap();
    let job = r["job"].as_u64().unwrap();
    assert!(s.execute("masks.trackPlanar", json!({"node": node, "mask": 0})).is_err(), "one job per window");
    assert!(s.execute("masks.track", json!({"node": node, "mask": 0})).is_err(), "nor a point track at once");
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    let t0 = std::time::Instant::now();
    while !s.planar_jobs.is_empty() && t0.elapsed().as_secs() < 120 {
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.planar_jobs.is_empty(), "the job ended");
    // what was tracked before the stop is kept (or nothing, if it stopped on its first frame)
    let (m, it) = window(&s, clip, node);
    if let Some(pl) = &m.planar {
        let last = pl.keys.last().unwrap().time;
        let k = RATE.frame_at(last);
        assert!(corner_error(&m, &it, k) < 0.3);
    }
}

#[test]
fn a_window_changed_while_tracking_is_not_overwritten() {
    let (mut s, clip) = session(true);
    let node = windowed(&mut s, clip, 10);
    let r = s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "direction": "forward", "frames": 30})).unwrap();
    // reshape it while the job runs
    s.execute("masks.moveVertex", json!({"node": node, "mask": 0, "vertex": 1, "delta": [2.0, 0.0]})).unwrap();
    let t0 = std::time::Instant::now();
    while !s.planar_jobs.is_empty() && t0.elapsed().as_secs() < 120 {
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(r["job"].as_u64().is_some());
    assert!(window(&s, clip, node).0.planar.is_none(), "nothing written over the new shape");
}

/// Review fix: a re-track that stops early (here: the plane is lost from frame 45 on) replaces
/// only the frames it tracked; the earlier track's keys beyond survive.
#[test]
fn a_partial_retrack_keeps_the_old_keys_beyond_where_it_stopped() {
    let (mut s, clip) = session(true);
    let node = windowed(&mut s, clip, 20);
    s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "wait": true})).unwrap();
    assert_eq!(window(&s, clip, node).0.planar.unwrap().keys.len(), FRAMES as usize);
    // the picture goes blank from frame 45: re-tracking forward from 30 loses the plane there
    let item = s.active_sequence().unwrap().find_item(clip).unwrap().1.item;
    let info = s.source(item).unwrap().info().clone();
    s.media.insert(item, std::sync::Arc::new(Plane { info, blank_from: Some(45) }));
    s.set_playhead(RATE.tick_of(30));
    s.execute("masks.trackPlanar", json!({"node": node, "mask": 0, "direction": "forward", "wait": true})).unwrap();
    let (m, it) = window(&s, clip, node);
    assert_eq!(m.planar.as_ref().unwrap().keys.len(), FRAMES as usize, "frames 45..=59 keep their keys");
    for k in [44, 45, 50, 59] {
        assert!(corner_error(&m, &it, k) < 0.2, "frame {k}: {}", corner_error(&m, &it, k));
    }
}

/// Review fix: a gesture that would put a point past the tracked plane's horizon is refused.
#[test]
fn gestures_past_the_planes_horizon_are_refused() {
    let (mut s, clip) = session(true);
    let node = windowed(&mut s, clip, 0);
    {
        // w = 1 − 0.002 x: the plane's horizon is at reference x = 500
        let seq = s.state.active_sequence.unwrap();
        let p = std::sync::Arc::make_mut(&mut s.project);
        let it = &mut p.sequence_mut(seq).unwrap().video_tracks[0].items[0];
        let w = &mut it.grade.as_mut().unwrap().node_mut(node).unwrap().windows[0];
        let h = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, -0.002, 0.0, 1.0];
        w.planar = Some(filmcraft_project::PlanarTrack { reference: Tick::ZERO, keys: vec![filmcraft_project::PlanarKey { time: Tick::ZERO, h }] });
    }
    s.set_playhead(Tick::ZERO);
    let before = window(&s, clip, node).0;
    // (on screen x = −1000 is the picture of reference x = 1000, behind the horizon; x just
    // above −500 maps back to ~ −2.5e10, an ill-conditioned inverse)
    for (cmd, p) in [
        ("masks.moveVertex", json!({"node": node, "mask": 0, "vertex": 0, "to": [-1000.0, 50.0]})),
        ("masks.moveVertex", json!({"node": node, "mask": 0, "vertex": 0, "handle": "out", "to": [-1000.0, 50.0]})),
        ("masks.addVertex", json!({"node": node, "mask": 0, "after": 0, "at": [-1000.0, 50.0]})),
        ("masks.translate", json!({"node": node, "mask": 0, "delta": [-3000.0, 0.0]})),
        ("masks.moveVertex", json!({"node": node, "mask": 0, "vertex": 0, "to": [-499.99999, 50.0]})),
    ] {
        let e = s.execute(cmd, p.clone()).unwrap_err().to_string();
        assert!(e.contains("horizon"), "{cmd} {p}: {e}");
    }
    assert_eq!(window(&s, clip, node).0, before, "nothing changed");
    // a point in front still moves
    s.execute("masks.moveVertex", json!({"node": node, "mask": 0, "vertex": 0, "delta": [2.0, 0.0]})).unwrap();
}
