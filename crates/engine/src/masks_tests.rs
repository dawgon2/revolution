use serde_json::json;

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::Tick;

fn demo() -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let it = q.video_tracks[0].items[0].clone();
    // middle of the first V1 clip (past its fade from black)
    s.set_playhead(it.start + Tick(it.duration.0 / 2));
    s.state.selection = vec![it.id];
    (s, it.id)
}

fn px(img: &filmcraft_render::Image, fx: f32, fy: f32) -> [f32; 4] {
    img.get(((img.w as f32 * fx) as usize).min(img.w - 1), ((img.h as f32 * fy) as usize).min(img.h - 1))
}

fn dist(a: [f32; 4], b: [f32; 4]) -> f32 {
    (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max)
}

#[test]
fn masked_effect_applies_only_inside_the_mask() {
    let (mut s, clip) = demo();
    let plain = s.render_program(0.25).unwrap();
    s.execute("effects.apply", json!({"effect": "invert"})).unwrap();
    let inverted = s.render_program(0.25).unwrap();
    // ellipse mask on Invert, in the left half of the frame
    let r = s.execute("masks.add", json!({"effect": "invert", "shape": "ellipse", "center": [480, 540], "size": [600, 600]})).unwrap();
    assert_eq!(r["name"], "Mask (1)");
    assert_eq!(r["mask"], 0);
    let masked = s.render_program(0.25).unwrap();
    // inside: inverted; outside: untouched
    assert!(dist(px(&masked, 0.25, 0.5), px(&inverted, 0.25, 0.5)) < 1e-4);
    assert!(dist(px(&masked, 0.8, 0.5), px(&plain, 0.8, 0.5)) < 1e-4);
    assert!(dist(px(&plain, 0.25, 0.5), px(&inverted, 0.25, 0.5)) > 0.05, "invert changes the picture");
    // inverted mask swaps the regions
    s.execute("masks.set", json!({"clip": clip.0, "effect": "invert", "mask": 0, "inverted": true})).unwrap();
    let inv = s.render_program(0.25).unwrap();
    assert!(dist(px(&inv, 0.25, 0.5), px(&plain, 0.25, 0.5)) < 1e-4);
    assert!(dist(px(&inv, 0.8, 0.5), px(&inverted, 0.8, 0.5)) < 1e-4);
    // undo ×2 → no mask
    s.undo();
    s.undo();
    let back = s.render_program(0.25).unwrap();
    assert_eq!(back.px, inverted.px);
    assert!(s.state.selected_mask.is_none(), "selection follows undo");
}

#[test]
fn linear_mask_grades_from_start_to_end() {
    let (mut s, clip) = demo();
    let plain = s.render_program(0.25).unwrap();
    s.execute("effects.apply", json!({"effect": "invert"})).unwrap();
    let inverted = s.render_program(0.25).unwrap();
    // a graduated Invert: full above y = 300, none below y = 700 (clip pixels of a 1080p source)
    s.execute("masks.add", json!({"effect": "invert", "shape": "linear", "start": [960, 300], "end": [960, 700]})).unwrap();
    let list = s.execute("masks.list", json!({"clip": clip.0})).unwrap();
    let m = list["masks"].as_array().unwrap().iter().find(|m| m["effectId"] == "invert").unwrap().clone();
    assert_eq!((m["shape"].as_str(), m["feather"].as_f64()), (Some("linear"), Some(0.0)));
    let g = s.render_program(0.25).unwrap();
    assert!(dist(px(&g, 0.5, 0.1), px(&inverted, 0.5, 0.1)) < 1e-4, "start side: full effect");
    assert!(dist(px(&g, 0.5, 0.9), px(&plain, 0.5, 0.9)) < 1e-4, "end side: untouched");
    let (mid, a, b) = (px(&g, 0.5, 0.463), px(&plain, 0.5, 0.463), px(&inverted, 0.5, 0.463));
    assert!(dist(mid, a) > 0.01 && dist(mid, b) > 0.01, "a blend at the midpoint: {mid:?} {a:?} {b:?}");
    // the points are a keyframable path: moving them moves the ramp
    s.execute("masks.set", json!({"clip": clip.0, "effect": "invert", "mask": 0, "path": [[960, 900], [960, 1000]]})).unwrap();
    let moved = s.render_program(0.25).unwrap();
    assert!(dist(px(&moved, 0.5, 0.5), px(&inverted, 0.5, 0.5)) < 1e-4);
    // it has just its two points: no added vertices, no curve handles
    let at = json!({"clip": clip.0, "effect": "invert", "mask": 0});
    let mut av = at.clone();
    av["after"] = json!(0);
    av["at"] = json!([960, 950]);
    assert!(s.execute("masks.addVertex", av).is_err());
    let mut tv = at.clone();
    tv["vertex"] = json!(0);
    assert!(s.execute("masks.toggleVertexSmooth", tv).is_err());
    let list = s.execute("masks.list", json!({"clip": clip.0})).unwrap();
    let m = list["masks"].as_array().unwrap().iter().find(|m| m["effectId"] == "invert").unwrap().clone();
    assert_eq!(m["path"]["vertices"].as_array().map(Vec::len), Some(2));
    // fewer than two distinct finite points are refused
    for bad in [json!([[1]]), json!([[5, 5]]), json!([[5, 5], [5, 5]]), json!([]), json!([[5, 5], [9, 9], [5, 5]])] {
        assert!(s.execute("masks.add", json!({"effect": "invert", "shape": "linear", "path": bad})).is_err(), "{bad}");
    }
    assert!(s.execute("masks.add", json!({"effect": "invert", "shape": "linear", "start": [5, 5], "end": [5, 5]})).is_err());
}

#[test]
fn opacity_mask_cuts_out_the_clip() {
    let (mut s, clip) = demo();
    // V1 only: an opacity mask leaves black outside
    s.execute("masks.add", json!({"clip": clip.0, "shape": "polygon", "center": [960, 540], "size": [960, 540]})).unwrap();
    s.execute("masks.set", json!({"feather": 0})).unwrap();
    let img = s.render_program(0.25).unwrap();
    assert!(px(&img, 0.5, 0.5)[3] > 0.99, "inside opaque");
    let corner = px(&img, 0.05, 0.05);
    assert!(corner[0] + corner[1] + corner[2] < 1e-4, "outside cut: {corner:?}");
    let list = s.execute("masks.list", json!({})).unwrap();
    let m = &list["masks"][0];
    assert_eq!(m["effectId"], "opacity");
    assert_eq!(m["path"]["vertices"].as_array().unwrap().len(), 4);
    assert_eq!(m["selected"], true);
}

#[test]
fn keyframed_mask_path_interpolates() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    let t1 = it.start + rate.tick_of(10);
    s.set_playhead(t1);
    s.execute("masks.translate", json!({"delta": [100, -50]})).unwrap();
    // feather keyframes too, through the generic keyframe command
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather", "value": 30.0})).unwrap();
    let mid = it.start + rate.tick_of(5);
    let l = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    let v0 = &l["masks"][0]["path"]["vertices"][0]["p"];
    assert!((v0[0].as_f64().unwrap() - 350.0).abs() < 1e-6, "{v0}");
    assert!((v0[1].as_f64().unwrap() - 275.0).abs() < 1e-6, "{v0}");
    assert_eq!(l["masks"][0]["pathKeyframes"].as_array().unwrap().len(), 2);
    let end = s.execute("masks.list", json!({"time": t1.0})).unwrap();
    assert_eq!(end["masks"][0]["feather"], 30.0);
    // vertex edit at a keyframe replaces that keyframe; handles mirror unless broken
    s.execute("masks.moveVertex", json!({"vertex": 1, "handle": "out", "delta": [0, 40]})).unwrap();
    let e = s.execute("masks.list", json!({})).unwrap();
    let v1 = &e["masks"][0]["path"]["vertices"][1];
    assert_eq!(v1["out"], json!([0.0, 40.0]));
    assert_eq!(v1["in"], json!([-0.0, -40.0]));
    assert_eq!(e["masks"][0]["pathKeyframes"].as_array().unwrap().len(), 2);
    // add / remove vertices keep all keyframes interpolable
    s.execute("masks.addVertex", json!({"after": 0, "at": [500, 220]})).unwrap();
    let a = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    assert_eq!(a["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 5);
    s.execute("masks.removeVertex", json!({"vertex": 1})).unwrap();
    let a = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    assert_eq!(a["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 4);
}

#[test]
fn add_vertex_on_animated_path_clamps_oversized_index_and_rejects_empty_path() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    s.set_playhead(it.start + rate.tick_of(10));
    s.execute("masks.translate", json!({"delta": [5, 0]})).unwrap();
    // an index past the last vertex appends instead of panicking
    s.execute("masks.addVertex", json!({"after": 100, "at": [1, 1]})).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 5);

    // an empty animated path is a parameter error and leaves the mask alone
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "path": {"vertices": [], "closed": false}})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 1, "param": "path"})).unwrap();
    s.set_playhead(it.start + rate.tick_of(10));
    s.execute("masks.translate", json!({"mask": 1, "delta": [5, 0]})).unwrap();
    let e = s.execute("masks.addVertex", json!({"mask": 1, "after": 0, "at": [1, 1]})).unwrap_err().to_string();
    assert!(e.contains("no vertices"), "{e}");
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][1]["path"]["vertices"].as_array().unwrap().len(), 0);

    // a static empty path still takes its first vertex
    s.execute("masks.add", json!({"effect": "opacity", "path": {"vertices": [], "closed": false}})).unwrap();
    s.execute("masks.addVertex", json!({"mask": 2, "after": 0, "at": [1, 1]})).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][2]["path"]["vertices"].as_array().unwrap().len(), 1);
}

#[test]
fn merged_drag_is_one_undo_step_and_project_round_trips() {
    let (mut s, _) = demo();
    s.execute("effects.apply", json!({"effect": "gaussian_blur"})).unwrap();
    s.execute("masks.add", json!({"effect": "gaussian_blur", "shape": "ellipse"})).unwrap();
    let n = s.history.undo.len();
    for _ in 0..5 {
        s.execute("masks.translate", json!({"delta": [3, 1], "merge": "drag-1"})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 1);
    s.execute("masks.set", json!({"mode": "subtract", "trackMethod": "position", "expansion": -4})).unwrap();
    let bytes = filmcraft_format::encode(&s.project, false);
    let back = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(&back, &*s.project);
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][0]["mode"], "Subtract");
    assert_eq!(l["masks"][0]["trackMethod"], "Position");
    assert!(s.execute("masks.add", json!({"effect": "motion"})).is_err(), "Motion has no masks");
}

// ---------------------------------------------------------------- tracking

/// A 320×240 24 fps clip of a textured disc moving under a known similarity motion over a static
/// textured background.
struct Moving {
    info: filmcraft_media::MediaInfo,
}

fn texture(u: f64, v: f64) -> f64 {
    let s = (u * 0.21).sin() * (v * 0.17).cos() + 0.5 * ((u + v) * 0.43).sin() + 0.35 * ((u * 0.9 - v * 0.6).sin() * (v * 0.75).cos());
    let (iu, iv) = ((u / 6.0).floor() as i64, (v / 6.0).floor() as i64);
    let mut x = (iu.wrapping_mul(73_856_093) ^ iv.wrapping_mul(19_349_663)) as u64;
    x ^= x >> 13;
    x = x.wrapping_mul(0x5bd1_e995);
    let h = (x >> 40) as f64 / (1u64 << 24) as f64;
    (0.5 + 0.22 * s + 0.12 * h).clamp(0.0, 1.0)
}

/// Ground-truth pose of the disc at frame `k`: translation, 1.5°/frame rotation, +1 %/frame scale.
fn truth(k: f64) -> filmcraft_geom::Affine {
    filmcraft_geom::Affine::motion(
        filmcraft_geom::Vec2::new(120.0 + 3.0 * k, 110.0 + 1.2 * k),
        filmcraft_geom::Vec2::new(1.0 + 0.01 * k, 1.0 + 0.01 * k),
        1.5 * k,
        filmcraft_geom::Vec2::ZERO,
    )
}

impl filmcraft_media::MediaSource for Moving {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
        let rate = filmcraft_time::FrameRate::FPS_24;
        let k = rate.frame_at(req.time) as f64;
        let inv = truth(k).inverse().unwrap();
        let (w, h) = (320usize, 240usize);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let p = filmcraft_geom::Vec2::new(x as f64 + ox, y as f64 + oy);
                    let o = inv.apply(p);
                    acc += if o.length() < 70.0 { texture(o.x + 200.0, o.y + 300.0) } else { 0.25 + 0.1 * texture(p.x * 0.5 + 900.0, p.y * 0.5) };
                }
                let v = (acc / 4.0 * 255.0).round() as u8;
                px[(y * w + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        Ok(std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba8(w as u32, h as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn tracking_session() -> (Session, ClipId) {
    use filmcraft_media::MediaSource;
    let rate = filmcraft_time::FrameRate::FPS_24;
    let g = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
        320,
        240,
        rate,
        rate.tick_of(48),
    );
    let info = g.info().clone();
    let mut p = filmcraft_project::Project::new("track");
    let item = p.add_item(
        "moving",
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
    let seq = p.new_sequence("s", filmcraft_project::SequenceSettings { width: 320, height: 240, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let ti =
        p.make_track_item(item, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
    let clip = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut s = Session { project: std::sync::Arc::new(p), ..Default::default() };
    s.state.active_sequence = Some(seq);
    s.state.selection = vec![clip];
    s.media.insert(item, std::sync::Arc::new(Moving { info }));
    (s, clip)
}

#[test]
fn tracking_follows_known_motion_forward_and_backward() {
    let (mut s, _) = tracking_session();
    let rate = filmcraft_time::FrameRate::FPS_24;
    s.set_playhead(rate.tick_of(10));
    // a circle of radius 45 inside the disc at frame 10
    let c = truth(10.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    let path0 = s.execute("masks.list", json!({})).unwrap()["masks"][0]["path"].clone();
    let p0 = crate::masks::path_from_json(&path0).unwrap();
    // object coordinates of the mask vertices (they ride on the disc)
    let inv10 = truth(10.0).inverse().unwrap();
    let obj: Vec<filmcraft_geom::Vec2> = p0.vertices.iter().map(|v| inv10.apply(v.p)).collect();
    let r = s.execute("masks.track", json!({"direction": "forward", "frames": 12, "wait": true})).unwrap();
    assert_eq!(r["frames"], 12, "{r}");
    assert!(s.mask_jobs.is_empty());
    let r = s.execute("masks.track", json!({"direction": "backward", "frames": 8, "wait": true, "method": "positionScaleRotation"})).unwrap();
    assert_eq!(r["frames"], 8);
    let l = s.execute("masks.list", json!({})).unwrap();
    let keys: Vec<i64> = l["masks"][0]["pathKeyframes"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(keys.len(), 21, "frames 2..=22: {keys:?}");
    let mut worst = 0.0f64;
    for f in [2i64, 6, 10, 14, 18, 22] {
        let t = rate.tick_of(f);
        let m = s.execute("masks.list", json!({"time": t.0})).unwrap();
        let path = crate::masks::path_from_json(&m["masks"][0]["path"]).unwrap();
        for (v, o) in path.vertices.iter().zip(&obj) {
            worst = worst.max((v.p - truth(f as f64).apply(*o)).length());
        }
    }
    eprintln!("mask tracking: max vertex error {worst:.3} px over -8..+12 frames");
    assert!(worst < 1.5, "max vertex error {worst} px");
    // the two tracking runs are two undo steps
    let h = s.execute("history.list", json!({})).unwrap();
    let undo: Vec<&str> = h["undo"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(&undo[undo.len() - 2..], ["Track Mask", "Track Mask"], "{undo:?}");
}

#[test]
fn tracking_runs_in_the_background_and_can_be_cancelled() {
    let (mut s, _) = tracking_session();
    let c = truth(0.0).apply(filmcraft_geom::Vec2::ZERO);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    let r = s.execute("masks.track", json!({"direction": "forward"})).unwrap();
    let job = r["job"].as_u64().unwrap();
    assert_eq!(r["frames"], 47);
    assert!(s.execute("masks.track", json!({})).is_err(), "one job per mask");
    s.execute("jobs.cancel", json!({"job": job})).unwrap();
    let t0 = std::time::Instant::now();
    while !s.mask_jobs.is_empty() && t0.elapsed().as_secs() < 60 {
        s.poll_persistence();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.mask_jobs.is_empty());
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    let j = jobs.as_array().unwrap().iter().find(|j| j["id"] == job).unwrap().clone();
    assert_eq!(j["finished"], true);
    let n = s.execute("masks.list", json!({})).unwrap()["masks"][0]["pathKeyframes"].as_array().unwrap().len();
    assert!(n < 48, "stopped early ({n} keyframes)");
    assert!(s.execute("masks.track", json!({"direction": "backward"})).is_err(), "nothing before the first frame");
}

/// #813: a rectangle mask with rounded corners (created rounded, or rounded afterwards) cuts the
/// clip's corners away while the edges stay where they were; one undo step, every keyframe.
#[test]
fn rounded_rectangle_masks_cut_their_corners() {
    let (mut s, clip) = demo();
    // a 960×540 opacity mask in the middle of the 1920×1080 frame, rounded by 200 px
    s.execute("masks.add", json!({"clip": clip.0, "shape": "rectangle", "center": [960, 540], "size": [960, 540], "cornerRadius": 200})).unwrap();
    s.execute("masks.set", json!({"feather": 0})).unwrap();
    let list = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(list["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 8);
    let img = s.render_program(0.25).unwrap();
    // just inside the rectangle's corner: cut away; middle of an edge and the centre: kept
    let corner = px(&img, 0.26, 0.26);
    assert!(corner[0] + corner[1] + corner[2] < 1e-4, "rounded corner cut: {corner:?}");
    assert!(px(&img, 0.5, 0.26)[3] > 0.99, "top edge kept");
    assert!(px(&img, 0.5, 0.5)[3] > 0.99, "inside kept");

    // round an existing 4-point mask: same picture, one undo step back to sharp corners
    s.undo();
    s.undo();
    s.execute("masks.add", json!({"clip": clip.0, "shape": "polygon", "center": [960, 540], "size": [960, 540]})).unwrap();
    s.execute("masks.set", json!({"feather": 0})).unwrap();
    let sharp = s.render_program(0.25).unwrap();
    assert!(px(&sharp, 0.26, 0.26)[3] > 0.99, "a sharp corner keeps the clip");
    let r = s.execute("masks.roundCorners", json!({"radius": 200})).unwrap();
    assert_eq!(r["corners"], 4);
    assert_eq!(s.render_program(0.25).unwrap().px, img.px, "rounding afterwards = creating rounded");
    // nothing left to round; bad radii are refused without changing the mask
    assert!(s.execute("masks.roundCorners", json!({"radius": 50})).is_err());
    for radius in [json!(0), json!(-5), json!("big"), json!(null)] {
        assert!(s.execute("masks.roundCorners", json!({"radius": radius})).is_err(), "{radius}");
    }
    s.undo();
    assert_eq!(s.render_program(0.25).unwrap().px, sharp.px, "one undo step restores the sharp corners");
}

#[test]
fn rounding_an_animated_mask_rounds_every_keyframe_and_only_the_chosen_corners() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    s.set_playhead(it.start + rate.tick_of(10));
    s.execute("masks.translate", json!({"delta": [300, 0]})).unwrap();
    // only vertices 0 and 2 (top-left, bottom-right), out-of-range indices ignored
    let r = s.execute("masks.roundCorners", json!({"radius": 30, "vertices": [0, 2, 99]})).unwrap();
    assert_eq!(r["corners"], 2);
    let m = &s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.iter().find(|e| e.effect == "opacity").unwrap().masks[0];
    assert_eq!(m.path.keyframes.len(), 2);
    for k in &m.path.keyframes {
        let filmcraft_project::ParamValue::Path(p) = &k.value else { panic!("path keyframe") };
        assert_eq!(p.len(), 6, "both keyframes rounded the same corners");
        assert_eq!(p.vertices.iter().filter(|v| v.is_corner()).count(), 2, "vertices 1 and 3 stay sharp");
    }
    // halfway the path still interpolates (equal vertex counts)
    let l = s.execute("masks.list", json!({"time": (it.start + rate.tick_of(5)).0})).unwrap();
    let v = l["masks"][0]["path"]["vertices"].as_array().unwrap();
    assert_eq!(v.len(), 6);
    assert!((v[0]["p"][0].as_f64().unwrap() - 450.0).abs() < 1e-6, "{}", v[0]);
}

/// Fork G2.2 / G2.3: a grade node's windows through `masks.*` with `node`, tracked like an effect
/// mask.
#[test]
fn grade_node_windows_are_masks_and_track() {
    let (mut s, clip) = tracking_session();
    let rate = filmcraft_time::FrameRate::FPS_24;
    s.set_playhead(rate.tick_of(10));
    let node = s.execute("grade.addNode", json!({"clip": clip.0})).unwrap()["node"].as_u64().unwrap();
    let c = truth(10.0).apply(filmcraft_geom::Vec2::ZERO);
    let r = s.execute("masks.add", json!({"node": node, "shape": "ellipse", "center": [c.x, c.y], "size": [90, 90]})).unwrap();
    assert_eq!((r["node"].as_u64(), r["mask"].as_u64(), r["name"].as_str()), (Some(node), Some(0), Some("Window (1)")), "{r}");
    let sel = s.state.selected_mask.unwrap();
    assert_eq!((sel.node, sel.mask), (Some(node), 0));
    // the selected window is the default target: edit, list
    s.execute("masks.set", json!({"inverted": true, "feather": 12})).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    let w = l["masks"].as_array().unwrap().iter().find(|m| m["node"].as_u64() == Some(node)).cloned().unwrap();
    assert_eq!((w["inverted"].as_bool(), w["feather"].as_f64(), w["selected"].as_bool()), (Some(true), Some(12.0), Some(true)), "{w}");
    s.execute("masks.set", json!({"inverted": false})).unwrap();
    // tracked forward like an effect mask; the keyframes land on the node's window
    let p0 = crate::masks::path_from_json(&w["path"]).unwrap();
    let inv10 = truth(10.0).inverse().unwrap();
    let obj: Vec<filmcraft_geom::Vec2> = p0.vertices.iter().map(|v| inv10.apply(v.p)).collect();
    let r = s.execute("masks.track", json!({"node": node, "mask": 0, "direction": "forward", "frames": 12, "wait": true})).unwrap();
    assert_eq!(r["frames"], 12, "{r}");
    let item = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    let win = &item.grade.as_ref().unwrap().node(node).unwrap().windows[0];
    assert_eq!(win.path.keyframes.len(), 13);
    let mut worst = 0.0f64;
    for f in [10i64, 14, 18, 22] {
        let path = win.path_at(item.source_time_at(rate.tick_of(f)));
        for (v, o) in path.vertices.iter().zip(&obj) {
            worst = worst.max((v.p - truth(f as f64).apply(*o)).length());
        }
    }
    assert!(worst < 1.5, "max vertex error {worst} px");
    // a windowed grade renders only inside the window (at frame 10, where the window was drawn)
    s.set_playhead(rate.tick_of(10));
    let before = s.render_program(1.0).unwrap();
    s.execute("grade.setParam", json!({"clip": clip.0, "node": node, "param": "gain", "value": [0.0, 0.0, 0.0, 1.0]})).unwrap();
    let after = s.render_program(1.0).unwrap();
    let at = |img: &filmcraft_render::Image, x: f64, y: f64| {
        let i = (y as usize * img.w + x as usize) * 4;
        [img.px[i], img.px[i + 1], img.px[i + 2]]
    };
    let (inside_b, inside_a) = (at(&before, c.x, c.y), at(&after, c.x, c.y));
    assert!(inside_a.iter().zip(&inside_b).any(|(a, b)| (a - b).abs() > 1e-3), "{inside_b:?} → {inside_a:?}");
    assert_eq!(at(&before, 2.0, 2.0), at(&after, 2.0, 2.0), "outside the window");
    // removing the node drops the selection; undo brings both back
    s.execute("masks.select", json!({"clip": clip.0, "node": node, "mask": 0})).unwrap();
    s.execute("grade.removeNode", json!({"clip": clip.0, "node": node})).unwrap();
    assert!(s.state.selected_mask.is_none());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.execute("masks.remove", json!({"clip": clip.0, "node": node, "mask": 0})).is_ok());
    // hostile addressing
    for p in [
        json!({"clip": clip.0, "node": 999, "shape": "ellipse"}),
        json!({"clip": clip.0, "node": "x", "shape": "ellipse"}),
        json!({"clip": clip.0, "node": -1, "shape": "ellipse"}),
    ] {
        assert!(s.execute("masks.add", p.clone()).is_err(), "{p}");
    }
    for _ in 0..filmcraft_project::grade::MAX_WINDOWS {
        s.execute("masks.add", json!({"clip": clip.0, "node": node, "shape": "ellipse"})).unwrap();
    }
    assert!(s.execute("masks.add", json!({"clip": clip.0, "node": node, "shape": "ellipse"})).is_err(), "window cap");
}

/// Which mask list a command defaults to when an effect mask or a grade window is selected.
#[test]
fn mask_owner_precedence_with_grade_windows() {
    let (mut s, clip) = tracking_session();
    let node = s.execute("grade.addNode", json!({"clip": clip.0})).unwrap()["node"].as_u64().unwrap();
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse"})).unwrap();
    s.execute("masks.add", json!({"node": node, "shape": "ellipse"})).unwrap();
    let count = |s: &mut Session| {
        let l = s.execute("masks.list", json!({})).unwrap();
        let v = l["masks"].as_array().unwrap().clone();
        (v.iter().filter(|m| m["node"].is_null()).count(), v.iter().filter(|m| !m["node"].is_null()).count())
    };
    assert_eq!(count(&mut s), (1, 1));
    // the window is selected: addressing the opacity effect without `mask` doesn't use its index
    s.execute("masks.add", json!({"effect": "opacity", "shape": "ellipse"})).unwrap();
    s.execute("masks.select", json!({"clip": clip.0, "node": node, "mask": 0})).unwrap();
    assert!(s.execute("masks.remove", json!({"effect": "opacity"})).is_err(), "no opacity mask is selected");
    // with no owner named, the selected window is the target (also with `"effect": null`)
    s.execute("masks.set", json!({"effect": null, "inverted": true})).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    let w = l["masks"].as_array().unwrap().iter().find(|m| !m["node"].is_null()).cloned().unwrap();
    assert_eq!(w["inverted"].as_bool(), Some(true));
    // an opacity mask selected: `node` without `mask` refuses instead of deleting window 0
    s.execute("masks.select", json!({"clip": clip.0, "effect": "opacity", "mask": 0})).unwrap();
    assert!(s.execute("masks.remove", json!({"node": node})).is_err());
    assert_eq!(count(&mut s), (2, 1));
    // the vertex commands work on windows
    s.execute("masks.translate", json!({"node": node, "mask": 0, "delta": [5, 0]})).unwrap();
    s.execute("masks.moveVertex", json!({"node": node, "mask": 0, "vertex": 0, "delta": [1, 1]})).unwrap();
    s.execute("masks.removeVertex", json!({"node": node, "mask": 0, "vertex": 0})).unwrap();
    // a window selected on another clip doesn't redirect commands on this one
    assert_eq!(count(&mut s), (2, 1));
}

/// Every shape the Color page's Windows tab offers (`grade.window.add.*`) creates a window.
#[test]
fn every_window_button_shape_adds_a_window() {
    let (mut s, clip) = tracking_session();
    let node = s.execute("grade.addNode", json!({"clip": clip.0})).unwrap()["node"].as_u64().unwrap();
    for shape in ["ellipse", "rectangle", "linear"] {
        let r = s.execute("masks.add", json!({"clip": clip.0, "node": node, "shape": shape}));
        assert!(r.is_ok(), "{shape}: {r:?}");
    }
    let l = s.execute("masks.list", json!({})).unwrap();
    let shapes: Vec<&str> = l["masks"].as_array().unwrap().iter().filter(|m| !m["node"].is_null()).filter_map(|m| m["shape"].as_str()).collect();
    assert_eq!(shapes, ["path", "path", "linear"]);
}

/// G2 acceptance fixture: a skin-coloured textured disc moving slowly for 5 s over a blue
/// textured background, with a static skin-coloured patch in the corner (outside any window).
struct SkinMoving {
    info: filmcraft_media::MediaInfo,
}

const ACCEPT_FRAMES: i64 = 120;

/// Pose of the skin disc at frame `k` (≈ 0.8 px/frame across, a slow turn).
fn skin_truth(k: f64) -> filmcraft_geom::Affine {
    filmcraft_geom::Affine::motion(
        filmcraft_geom::Vec2::new(100.0 + 0.8 * k, 120.0 + 0.3 * k),
        filmcraft_geom::Vec2::new(1.0, 1.0),
        0.3 * k,
        filmcraft_geom::Vec2::ZERO,
    )
}

impl filmcraft_media::MediaSource for SkinMoving {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
        let rate = filmcraft_time::FrameRate::FPS_24;
        let k = rate.frame_at(req.time) as f64;
        let inv = skin_truth(k).inverse().unwrap();
        let (w, h) = (320usize, 240usize);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let p = filmcraft_geom::Vec2::new(x as f64 + 0.5, y as f64 + 0.5);
                let o = inv.apply(p);
                let (base, tex) = if o.length() < 45.0 {
                    // (0.75…1.05: below 8-bit clipping, which would shift the hue)
                    ([224.0, 172.0, 140.0], 0.75 + 0.3 * texture(o.x + 200.0, o.y + 300.0))
                } else if x < 50 && y < 50 {
                    ([224.0, 172.0, 140.0], 1.0)
                } else {
                    ([90.0, 110.0, 150.0], 0.8 + 0.4 * texture(p.x * 0.5 + 900.0, p.y * 0.5))
                };
                let c = base.map(|v: f64| (v * tex).round().clamp(0.0, 255.0) as u8);
                px[(y * w + x) * 4..][..4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        Ok(std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba8(w as u32, h as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
    }
}

/// GRADE-PLAN G2 acceptance (synthetic): a skin qualifier plus a tracked circle window on a moving
/// subject hold over 5 s — the grade stays on the subject, off the background next to it and off
/// a skin-coloured patch outside the window.
#[test]
fn g2_accept_skin_qualifier_and_tracked_window_hold_over_5_seconds() {
    use filmcraft_media::MediaSource;
    let rate = filmcraft_time::FrameRate::FPS_24;
    let g = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::ColorMatte { color: [0.5, 0.5, 0.5, 1.0] },
        320,
        240,
        rate,
        rate.tick_of(ACCEPT_FRAMES),
    );
    let info = g.info().clone();
    let mut p = filmcraft_project::Project::new("accept");
    let item = p.add_item(
        "skin",
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
    let seq = p.new_sequence("s", filmcraft_project::SequenceSettings { width: 320, height: 240, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let ti = p
        .make_track_item(item, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, rate.tick_of(ACCEPT_FRAMES)), rate)
        .unwrap();
    let clip = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut s = Session { project: std::sync::Arc::new(p), ..Default::default() };
    s.state.active_sequence = Some(seq);
    s.state.selection = vec![clip];
    s.media.insert(item, std::sync::Arc::new(SkinMoving { info }));
    s.set_playhead(Tick::ZERO);

    let node = s.execute("grade.addNode", json!({"clip": clip.0})).unwrap()["node"].as_u64().unwrap();
    // the qualifier picked on the subject, a circle window round it, a strong correction
    let pick = s.execute("grade.pickQualifier", json!({"clip": clip.0, "node": node, "x": 100.0 / 320.0, "y": 120.0 / 240.0})).unwrap();
    assert_eq!(pick["params"]["qual.on"], 1.0, "{pick}");
    // (the subject's texture spans more brightness than the picker's first range: widen it)
    s.execute("grade.setParam", json!({"clip": clip.0, "node": node, "param": "qual.lumLow", "value": 0.25})).unwrap();
    s.execute("grade.setParam", json!({"clip": clip.0, "node": node, "param": "qual.lumHigh", "value": 1.0})).unwrap();
    s.execute("grade.setParam", json!({"clip": clip.0, "node": node, "param": "qual.satHigh", "value": 1.0})).unwrap();
    s.execute("grade.setParam", json!({"clip": clip.0, "node": node, "param": "gain", "value": [0.0, 0.0, 0.0, 0.6]})).unwrap();
    s.execute("masks.add", json!({"clip": clip.0, "node": node, "shape": "ellipse", "center": [100, 120], "size": [110, 110]})).unwrap();
    let r =
        s.execute("masks.track", json!({"clip": clip.0, "node": node, "mask": 0, "direction": "forward", "frames": ACCEPT_FRAMES - 1, "wait": true})).unwrap();
    assert_eq!(r["frames"], ACCEPT_FRAMES - 1, "{r}");

    let lum = |img: &filmcraft_render::Image, x: f64, y: f64| {
        let i = (y as usize * img.w + x as usize) * 4;
        img.px[i] * 0.2126 + img.px[i + 1] * 0.7152 + img.px[i + 2] * 0.0722
    };
    let mut worst_centre = f32::MAX;
    let mut worst_outside = 0.0f32;
    for f in [0i64, 30, 60, 90, ACCEPT_FRAMES - 1] {
        s.set_playhead(rate.tick_of(f));
        s.execute("grade.bypass", json!({"clip": clip.0, "on": true})).unwrap();
        let plain = s.render_program(1.0).unwrap();
        s.execute("grade.bypass", json!({"clip": clip.0, "on": false})).unwrap();
        let graded = s.render_program(1.0).unwrap();
        let c = skin_truth(f as f64).apply(filmcraft_geom::Vec2::ZERO);
        // on the subject: brighter (a few points inside the disc)
        for (dx, dy) in [(0.0, 0.0), (15.0, 0.0), (-15.0, 10.0), (5.0, -20.0)] {
            let gain = lum(&graded, c.x + dx, c.y + dy) / lum(&plain, c.x + dx, c.y + dy).max(1e-6);
            worst_centre = worst_centre.min(gain);
        }
        // the blue background next to the subject (inside the window, outside the key) and the
        // static skin patch (inside the key, outside the window) are untouched
        for (x, y) in [(c.x + 50.0, c.y), (c.x, c.y - 52.0), (25.0, 25.0), (40.0, 10.0)] {
            let d = (lum(&graded, x, y) - lum(&plain, x, y)).abs() / lum(&plain, x, y).max(1e-3);
            worst_outside = worst_outside.max(d);
        }
    }
    eprintln!("G2 accept (synthetic): subject gain ≥ {worst_centre:.3}×, outside change ≤ {:.2} %", worst_outside * 100.0);
    assert!(worst_centre > 1.15, "the subject stays graded over 5 s: {worst_centre}");
    assert!(worst_outside < 0.01, "nothing outside the subject changes: {worst_outside}");
}

/// Fork C3.9 review fix: `path_to_json`'s `in` / `out` form was read by serde as a MaskPath
/// (`[x, y]` points, unknown keys ignored), dropping every tangent; both forms now round-trip
/// with tangents and per-point feather.
#[test]
fn path_json_round_trips_tangents_and_feather_in_both_forms() {
    use crate::masks::{path_from_json, path_to_json};
    use filmcraft_geom::Vec2;
    use filmcraft_project::{MaskPath, MaskVertex};
    let v = |x: f64, y: f64, t: (f64, f64)| MaskVertex { p: Vec2::new(x, y), t_in: Vec2::new(-t.0, -t.1), t_out: Vec2::new(t.0, t.1), ..MaskVertex::default() };
    let path =
        MaskPath { vertices: vec![v(10.0, 10.0, (5.0, 0.0)).with_feather(2.0, 7.5), v(90.0, 20.0, (0.0, 0.0)), v(50.0, 80.0, (-3.0, 4.0))], closed: true };
    assert_eq!(path_from_json(&path_to_json(&path)), Some(path.clone()), "the in / out form");
    assert_eq!(path_from_json(&serde_json::to_value(&path).unwrap()), Some(path.clone()), "the serde form");
    let open = MaskPath { closed: false, ..path.clone() };
    assert_eq!(path_from_json(&path_to_json(&open)), Some(open.clone()));
    assert_eq!(path_from_json(&serde_json::to_value(&open).unwrap()), Some(open));
    // `{x, y}` points, camelCase feathers and bare point lists still read
    let j = json!({"vertices": [{"p": {"x": 1.0, "y": 2.0}, "t_out": {"x": 3.0, "y": 0.0}, "featherOut": 4.0}, [5.0, 6.0], {"x": 7.0, "y": 8.0}]});
    let p = path_from_json(&j).unwrap();
    assert_eq!(p.vertices.len(), 3);
    assert_eq!((p.vertices[0].t_out, p.vertices[0].feather_out), (Vec2::new(3.0, 0.0), 4.0));
    assert_eq!(p.vertices[2].p, Vec2::new(7.0, 8.0));
    // junk is refused, never a panic
    for bad in [json!(null), json!(5), json!({"vertices": [{"p": "x"}]}), json!([[1.0]])] {
        assert_eq!(path_from_json(&bad), None, "{bad}");
    }
}

/// The monitor's pen sends `masks.add` its Bézier path in the `path_to_json` form: the curve
/// handles are kept (they were dropped before the fix above).
#[test]
fn a_pen_mask_keeps_its_bezier_handles() {
    use filmcraft_geom::Vec2;
    use filmcraft_project::{MaskPath, MaskVertex};
    let (mut s, clip) = demo();
    let v = |x: f64, y: f64| MaskVertex { p: Vec2::new(x, y), t_in: Vec2::new(-20.0, 0.0), t_out: Vec2::new(20.0, 0.0), ..MaskVertex::default() };
    let path = MaskPath { vertices: vec![v(100.0, 100.0), v(300.0, 120.0), v(200.0, 260.0)], closed: true };
    let p = json!({"clip": clip.0, "effect": "opacity", "shape": "bezier", "path": crate::masks::path_to_json(&path)});
    s.execute("masks.add", p).unwrap();
    let l = s.execute("masks.list", json!({})).unwrap();
    let got = crate::masks::path_from_json(&l["masks"][0]["path"]).unwrap();
    assert_eq!(got.vertices.iter().map(|q| q.t_out).collect::<Vec<_>>(), vec![Vec2::new(20.0, 0.0); 3], "{got:?}");
}
