//! C3 acceptance tests: synthetic textured shots moving along known paths.

use super::*;

/// Integer hash → 0..1.
fn hash(ix: i64, iy: i64, seed: u64) -> f64 {
    // splitmix64 (Steele, Lea & Flood 2014) of the combined key
    let mut x = (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f64 / (1u64 << 24) as f64
}

/// Smooth (C¹) value noise, -1..1.
fn noise(u: f64, v: f64, seed: u64) -> f64 {
    let (iu, iv) = (u.floor(), v.floor());
    let (fu, fv) = (u - iu, v - iv);
    let (su, sv) = (fu * fu * (3.0 - 2.0 * fu), fv * fv * (3.0 - 2.0 * fv));
    let (iu, iv) = (iu as i64, iv as i64);
    let a = hash(iu, iv, seed);
    let b = hash(iu + 1, iv, seed);
    let c = hash(iu, iv + 1, seed);
    let d = hash(iu + 1, iv + 1, seed);
    let top = a + (b - a) * su;
    let bot = c + (d - c) * su;
    2.0 * (top + (bot - top) * sv) - 1.0
}

/// The tracked texture (world coordinates).
fn tex(p: Vec2) -> f32 {
    let (u, v) = (p.x, p.y);
    (0.5 + 0.2 * noise(u / 9.0, v / 9.0, 1) + 0.13 * noise(u / 3.5, v / 3.5, 2) + 0.06 * (u * 0.21).sin() * (v * 0.17).cos()) as f32
}

/// A different, static background texture.
fn bg(p: Vec2) -> f32 {
    (0.3 + 0.12 * noise(p.x / 7.0 + 50.0, p.y / 7.0, 7) + 0.08 * noise(p.x / 2.5, p.y / 2.5 + 30.0, 8)) as f32
}

/// A frame from a pixel function, 2×2 supersampled (pixel centres at integers).
fn render(w: usize, h: usize, f: impl Fn(Vec2) -> f32 + Sync) -> Prepared {
    let px = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let mut acc = 0.0;
            for (ox, oy) in [(-0.25, -0.25), (0.25, -0.25), (-0.25, 0.25), (0.25, 0.25)] {
                acc += f(Vec2::new((i % w) as f64 + ox, (i / w) as f64 + oy));
            }
            acc / 4.0
        })
        .collect();
    Prepared::new(Gray { w, h, px })
}

fn flat(w: usize, h: usize) -> Prepared {
    Prepared::new(Gray { w, h, px: vec![0.5; w * h] })
}

/// `p` plus ±0.03 uniform per-pixel noise, different every frame `t` (≈ 1.7 % rms, rougher
/// than 8-bit grain).
fn noisy(p: &Prepared, t: usize) -> Prepared {
    let g = p.base();
    let px = g.px.iter().enumerate().map(|(i, v)| v + 0.06 * (hash(i as i64, t as i64, 99) as f32 - 0.5)).collect();
    Prepared::new(Gray { w: g.w, h: g.h, px })
}

fn stats(errs: &[f64]) -> (f64, f64) {
    let mean = errs.iter().sum::<f64>() / errs.len().max(1) as f64;
    (mean, errs.iter().copied().fold(0.0, f64::max))
}

// ---- (a) point tracking --------------------------------------------------------------------

/// A curving pan with sub-pixel steps.
fn pan(t: f64) -> Vec2 {
    Vec2::new(14.0 * (t / 8.0).sin() + 0.37 * t, 9.0 * (t / 6.0).cos() - 9.0 + 0.21 * t)
}

fn pan_frames(n: usize) -> Vec<Prepared> {
    (0..n).map(|t| render(320, 240, |p| tex(p - pan(t as f64)))).collect()
}

const POINTS: [Vec2; 3] = [Vec2::new(90.0, 80.0), Vec2::new(200.0, 150.0), Vec2::new(160.3, 110.6)];

fn spec(at: Vec2) -> PointSpec {
    PointSpec { at, pattern: Vec2::new(10.0, 10.0), search: Vec2::new(8.0, 8.0) }
}

#[test]
fn point_tracks_a_curving_subpixel_path_forward_and_backward() {
    let n = 60;
    let frames = pan_frames(n);
    let mut errs = Vec::new();
    for start in [0, n - 1, n / 2] {
        for p0 in POINTS {
            let at = p0 + pan(start as f64);
            let track = track_point(&frames, start, spec(at), PointOptions::default()).expect("starts");
            assert_eq!(track.len(), n);
            for (t, s) in track.iter().enumerate() {
                assert!(!s.lost, "lost at frame {t} (start {start}, conf {})", s.confidence);
                errs.push((s.pos - (p0 + pan(t as f64))).length());
            }
        }
    }
    let (mean, max) = stats(&errs);
    eprintln!("point tracker: {} samples, mean error {mean:.4} px, max {max:.4} px", errs.len());
    assert!(mean <= 0.1, "mean {mean}");
    assert!(max <= 0.3, "max {max}");
}

#[test]
fn point_tracks_through_sensor_noise() {
    let n = 48;
    // ±0.03 uniform per-pixel noise, fresh every frame (≈ 2 % rms, worse than 8-bit grain)
    let frames: Vec<Prepared> = (0..n).map(|t| noisy(&render(320, 240, |p| tex(p - pan(t as f64))), t)).collect();
    let mut errs = Vec::new();
    for p0 in POINTS {
        let track = track_point(&frames, 0, spec(p0 + pan(0.0)), PointOptions::default()).expect("starts");
        for (t, s) in track.iter().enumerate() {
            assert!(!s.lost, "lost at {t}");
            errs.push((s.pos - (p0 + pan(t as f64))).length());
        }
    }
    let (mean, max) = stats(&errs);
    eprintln!("point tracker with noise: mean error {mean:.4} px, max {max:.4} px");
    assert!(mean <= 0.25, "mean {mean}");
    assert!(max <= 0.5, "max {max}");
}

// ---- (b) multi-point stabilise / match move ------------------------------------------------

/// Camera shake (similarity about the frame centre): world → frame t.
fn shake(t: f64) -> Affine {
    let c = Vec2::new(160.0, 120.0);
    let s = 1.0 + 0.02 * (0.6 * t).sin();
    Affine::translate(c.x + 3.0 * (1.3 * t).sin(), c.y + 2.5 * (1.7 * t).cos())
        .then_apply(&Affine::rotate_deg(1.5 * (0.9 * t).sin()))
        .then_apply(&Affine::scale(s, s))
        .then_apply(&Affine::translate(-c.x, -c.y))
}

#[test]
fn multi_point_stabilise_cancels_camera_shake() {
    let n = 48;
    let frames: Vec<Prepared> = (0..n)
        .map(|t| {
            let inv = shake(t as f64).inverse().unwrap();
            render(320, 240, move |p| tex(inv.apply(p)))
        })
        .collect();
    let at = [Vec2::new(70.0, 60.0), Vec2::new(250.0, 60.0), Vec2::new(250.0, 180.0), Vec2::new(70.0, 180.0)];
    let specs: Vec<PointSpec> = at.iter().map(|a| PointSpec { at: *a, pattern: Vec2::new(12.0, 12.0), search: Vec2::new(14.0, 14.0) }).collect();
    let tracks: Vec<Vec<TrackSample>> = track_points(&frames, 0, &specs, PointOptions::default()).into_iter().map(|t| t.expect("starts")).collect();
    let stab = stabilise(&tracks, 0, TrackMethod::PositionScaleRotation, 0);
    let mm = match_move(&tracks, 0, TrackMethod::PositionScaleRotation);
    let corners = [Vec2::new(0.0, 0.0), Vec2::new(319.0, 0.0), Vec2::new(319.0, 239.0), Vec2::new(0.0, 239.0)];
    let inv0 = shake(0.0).inverse().unwrap();
    let (mut sq, mut cnt, mut worst, mut mm_worst, mut raw) = (0.0, 0, 0.0f64, 0.0f64, 0.0f64);
    for t in 0..n {
        let truth = shake(t as f64).then_apply(&inv0);
        let s = stab[t].expect("stabilised");
        let m = mm[t].expect("match moved");
        for c in corners {
            let e = (s.apply(truth.apply(c)) - c).length();
            sq += e * e;
            cnt += 1;
            worst = worst.max(e);
            mm_worst = mm_worst.max((m.apply(c) - truth.apply(c)).length());
            raw = raw.max((truth.apply(c) - c).length());
        }
    }
    let rms = (sq / cnt as f64).sqrt();
    eprintln!("stabilise: corner residual RMS {rms:.4} px, max {worst:.4} px (raw shake up to {raw:.2} px); match move max corner error {mm_worst:.4} px");
    assert!(rms <= 0.5, "rms {rms}");
    assert!(worst <= 1.0, "max {worst}");
    assert!(mm_worst <= 1.0, "match move {mm_worst}");
}

#[test]
fn smooth_stabilise_keeps_the_pan_and_removes_the_shake() {
    let n = 48;
    let pan = |t: f64| Vec2::new(1.4 * t, 0.35 * t);
    let jitter = |t: f64| Vec2::new(2.0 * (1.9 * t).sin(), 1.5 * (2.3 * t).cos());
    let frames: Vec<Prepared> = (0..n).map(|t| render(320, 240, move |p| tex(p - pan(t as f64) - jitter(t as f64)))).collect();
    let at = [Vec2::new(60.0, 70.0), Vec2::new(80.0, 170.0), Vec2::new(150.0, 120.0)];
    let specs: Vec<PointSpec> = at.iter().map(|a| PointSpec { at: *a, pattern: Vec2::new(10.0, 10.0), search: Vec2::new(10.0, 10.0) }).collect();
    let tracks: Vec<Vec<TrackSample>> = track_points(&frames, 0, &specs, PointOptions::default()).into_iter().map(|t| t.expect("starts")).collect();
    // the incremental form gives the same samples as the batch run
    let mut multi = MultiPointTracker::new(&frames[0], &specs, PointOptions::default());
    assert_eq!(multi.first_samples(), tracks.iter().map(|t| t[0]).collect::<Vec<_>>());
    for (k, f) in frames.iter().enumerate().skip(1).take(8) {
        assert_eq!(multi.step(f), tracks.iter().map(|t| t[k]).collect::<Vec<_>>());
    }
    let radius = 10u32;
    let stab = stabilise(&tracks, 0, TrackMethod::PositionScaleRotation, radius);
    let mut worst = 0.0f64;
    for t in radius as usize..n - radius as usize {
        let s = stab[t].expect("stabilised");
        for c in [Vec2::new(20.0, 20.0), Vec2::new(300.0, 220.0)] {
            let seen = c + pan(t as f64) + jitter(t as f64) - jitter(0.0);
            let expect = c + pan(t as f64) - jitter(0.0);
            worst = worst.max((s.apply(seen) - expect).length());
        }
    }
    eprintln!("smooth stabilise (radius {radius}): max departure from the pure pan {worst:.4} px (shake amplitude 2.5 px)");
    assert!(worst <= 0.5, "max {worst}");
    // the ends (window cut short): the local line keeps the pan's speed instead of averaging it
    // toward the interior (a plain truncated mean was ~5 px off here)
    let mut ends = 0.0f64;
    for t in (0..radius as usize).chain(n - radius as usize..n) {
        let s = stab[t].expect("stabilised");
        for c in [Vec2::new(20.0, 20.0), Vec2::new(300.0, 220.0)] {
            let seen = c + pan(t as f64) + jitter(t as f64) - jitter(0.0);
            let expect = c + pan(t as f64) - jitter(0.0);
            ends = ends.max((s.apply(seen) - expect).length());
        }
    }
    eprintln!("smooth stabilise ends: max departure from the pure pan {ends:.4} px");
    assert!(ends <= 1.5, "ends max {ends}");
}

#[test]
fn corner_pin_from_four_points_follows_a_perspective_plane() {
    let n = 24;
    let hs: Vec<Homography> = (0..n).map(|t| plane_h(t as f64)).collect();
    let frames: Vec<Prepared> = hs.iter().map(plane_frame).collect();
    let obj = [Vec2::new(-45.0, -45.0), Vec2::new(45.0, -45.0), Vec2::new(45.0, 45.0), Vec2::new(-45.0, 45.0)];
    let specs: Vec<PointSpec> =
        obj.iter().map(|o| PointSpec { at: hs[0].apply(*o).unwrap(), pattern: Vec2::new(12.0, 12.0), search: Vec2::new(10.0, 10.0) }).collect();
    let tracks: Vec<Vec<TrackSample>> = track_points(&frames, 0, &specs, PointOptions::default()).into_iter().map(|t| t.expect("starts")).collect();
    let mut pt_worst = 0.0f64;
    for (tr, o) in tracks.iter().zip(obj) {
        for (t, s) in tr.iter().enumerate() {
            let e = (s.pos - hs[t].apply(o).unwrap()).length();
            pt_worst = pt_worst.max(e);
        }
    }
    let pins = corner_pin(&tracks, 0).expect("four tracks");
    let quad = [Vec2::new(-80.0, -80.0), Vec2::new(80.0, -80.0), Vec2::new(80.0, 80.0), Vec2::new(-80.0, 80.0)].map(|o| hs[0].apply(o).unwrap());
    let mut worst = 0.0f64;
    for t in 0..n {
        let got = map_quad(&pins[t].expect("pinned"), &quad).unwrap();
        for (g, o) in got.iter().zip([Vec2::new(-80.0, -80.0), Vec2::new(80.0, -80.0), Vec2::new(80.0, 80.0), Vec2::new(-80.0, 80.0)]) {
            worst = worst.max((*g - hs[t].apply(o).unwrap()).length());
        }
    }
    eprintln!("4-point corner pin: max point error {pt_worst:.4} px, max quad corner error {worst:.4} px");
    assert!(pt_worst <= 0.3, "point max {pt_worst}");
    assert!(worst <= 0.5, "max {worst}");
    assert!(corner_pin(&tracks[..3], 0).is_none());
}

// ---- (c) planar tracking -------------------------------------------------------------------

/// The plane's pose at `t`: object coordinates (side 160, centred) → frame pixels, with a
/// changing perspective tilt, rotation, scale and a drifting position.
fn plane_h(t: f64) -> Homography {
    let a = Affine::motion(
        Vec2::new(200.0 + 35.0 * (t / 11.0).sin() + 0.3 * t, 150.0 + 18.0 * (t / 8.0).sin()),
        Vec2::new(1.0 + 0.06 * (t / 14.0).sin(), 1.0 + 0.06 * (t / 14.0).sin()),
        5.0 * (t / 10.0).sin(),
        Vec2::ZERO,
    );
    let affine = Homography { m: [a.a, a.c, a.e, a.b, a.d, a.f, 0.0, 0.0, 1.0] };
    let tilt = Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0009 * (t / 9.0).sin(), 0.0007 * (t / 12.0).cos() - 0.0003, 1.0] };
    affine.after(&tilt)
}

fn plane_frame(h: &Homography) -> Prepared {
    let inv = h.inverse().unwrap();
    render(400, 300, move |p| match inv.apply(p) {
        Some(o) if o.x.abs() < 80.0 && o.y.abs() < 80.0 => tex(o + Vec2::new(300.0, 200.0)),
        _ => bg(p),
    })
}

const OBJ_QUAD: [Vec2; 4] = [Vec2::new(-80.0, -80.0), Vec2::new(80.0, -80.0), Vec2::new(80.0, 80.0), Vec2::new(-80.0, 80.0)];

fn plane_setup(h0: &Homography) -> (Vec<Vec2>, [Vec2; 4]) {
    let region =
        [Vec2::new(-64.0, -64.0), Vec2::new(64.0, -64.0), Vec2::new(64.0, 64.0), Vec2::new(-64.0, 64.0)].iter().map(|o| h0.apply(*o).unwrap()).collect();
    (region, OBJ_QUAD.map(|o| h0.apply(o).unwrap()))
}

#[test]
fn planar_tracker_keeps_a_corner_pin_locked_without_drift() {
    let n = 72;
    let hs: Vec<Homography> = (0..n).map(|t| plane_h(t as f64)).collect();
    let mut frames: Vec<Arc<Prepared>> = hs.iter().enumerate().map(|(t, h)| Arc::new(noisy(&plane_frame(h), t))).collect();
    let (region, quad) = plane_setup(&hs[0]);
    let track = track_plane(&frames, 0, &region, quad).expect("starts");
    let mut errs = Vec::new();
    let mut last = 0.0f64;
    for (t, s) in track.iter().enumerate() {
        assert!(!s.lost, "lost at {t} (conf {}, inliers {})", s.confidence, s.inliers);
        for (c, o) in s.corners.iter().zip(OBJ_QUAD) {
            let e = (*c - hs[t].apply(o).unwrap()).length();
            errs.push(e);
            if t == n - 1 {
                last = last.max(e);
            }
        }
    }
    let (mean, max) = stats(&errs);
    // the chain alone (no re-anchoring), for comparison
    let mut chain = Homography::IDENTITY;
    for k in 1..n {
        let seen: Vec<Vec2> = region.iter().filter_map(|p| chain.apply(*p)).collect();
        if let Some(st) = track_step_perspective(&frames[k - 1], &frames[k], &seen) {
            chain = st.transform.after(&chain);
        }
    }
    let chain_last = map_quad(&chain, &quad).unwrap().iter().zip(OBJ_QUAD).map(|(c, o)| (*c - hs[n - 1].apply(o).unwrap()).length()).fold(0.0, f64::max);
    eprintln!(
        "planar tracker over {n} frames: corner error mean {mean:.4} px, max {max:.4} px, last-frame drift {last:.4} px (frame-to-frame chain alone: {chain_last:.4} px)"
    );
    assert!(mean <= 0.5, "mean {mean}");
    assert!(max <= 1.5, "max {max}");
    assert!(last <= 1.0, "drift {last}");

    // backward from the last frame, too
    let back = track_plane(&frames, n - 1, &plane_setup(&hs[n - 1]).0, plane_setup(&hs[n - 1]).1).expect("starts");
    let back_max = back
        .iter()
        .enumerate()
        .flat_map(|(t, s)| s.corners.iter().zip(OBJ_QUAD).map(move |(c, o)| (*c - plane_h(t as f64).apply(o).unwrap()).length()))
        .fold(0.0, f64::max);
    eprintln!("planar tracker backward: max corner error {back_max:.4} px");
    assert!(back_max <= 1.5, "backward max {back_max}");

    // (d) an occluded (blank) frame is reported lost, then the plane is re-acquired
    frames[30] = Arc::new(flat(400, 300));
    let occl = track_plane(&frames, 0, &region, quad).expect("starts");
    assert!(occl[30].lost, "blank frame lost");
    let after = occl[31..]
        .iter()
        .enumerate()
        .flat_map(|(k, s)| s.corners.iter().zip(OBJ_QUAD).map(move |(c, o)| (*c - plane_h((k + 31) as f64).apply(o).unwrap()).length()))
        .fold(0.0, f64::max);
    assert!(occl[31..].iter().all(|s| !s.lost), "re-acquired after the blank frame");
    eprintln!("planar tracker after a blank frame: max corner error {after:.4} px");
    assert!(after <= 1.5, "after occlusion {after}");
}

// ---- (d) occlusion / blank frames for points -----------------------------------------------

#[test]
fn point_blank_frame_is_lost_then_stops_or_holds() {
    let mut frames = pan_frames(30);
    frames[20] = flat(320, 240);
    let at = POINTS[0];
    let stop = track_point(&frames, 0, spec(at), PointOptions::default()).expect("starts");
    assert!(stop[..20].iter().all(|s| !s.lost));
    assert!(stop[20..].iter().all(|s| s.lost), "stop: lost from the blank frame on");
    let hold = track_point(&frames, 0, spec(at), PointOptions { on_lost: OnLost::Hold, ..PointOptions::default() }).expect("starts");
    assert!(hold[20].lost);
    assert!(hold[20].pos == hold[19].pos, "held at the last good position");
    let mut worst = 0.0f64;
    for (t, s) in hold.iter().enumerate().skip(21) {
        assert!(!s.lost, "re-found at {t}");
        worst = worst.max((s.pos - (at + pan(t as f64))).length());
    }
    eprintln!("point tracker after a blank frame (hold): max error {worst:.4} px");
    assert!(worst <= 0.3);
    // a frame where the pattern is gone (different picture) is lost too
    let other = render(320, 240, |p| bg(p * 1.7));
    let mut t = PointTracker::new(&frames[0], spec(at), PointOptions::default()).unwrap();
    let s = t.step(&other);
    assert!(s.lost, "different picture: conf {}", s.confidence);
}

// ---- (e) hostile input ---------------------------------------------------------------------

#[test]
fn hostile_input_is_refused_never_panics() {
    let good = render(64, 48, tex);
    let zero = Prepared::new(Gray { w: 0, h: 0, px: Vec::new() });
    let one = Prepared::new(Gray { w: 1, h: 1, px: vec![0.5] });
    let short = Prepared::new(Gray { w: 100, h: 100, px: vec![0.5; 3] });
    let huge_claim = Prepared::new(Gray { w: usize::MAX, h: 2, px: vec![0.5; 4] });
    let c = Vec2::new(32.0, 24.0);
    let ok = spec(c);
    for f in [&zero, &one, &short, &huge_claim] {
        assert!(PointTracker::new(f, ok, PointOptions::default()).is_none());
        assert!(PlanarTracker::new(f, &[Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)], OBJ_QUAD).is_none());
        assert!(crate::track::track_step(f, f, &[Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)], TrackMethod::Position).is_none());
    }
    let nan = Vec2::new(f64::NAN, 1.0);
    let inf = Vec2::new(f64::INFINITY, 1.0);
    for s in [
        PointSpec { at: nan, ..ok },
        PointSpec { pattern: nan, ..ok },
        PointSpec { search: inf, ..ok },
        PointSpec { at: Vec2::new(-5.0, 10.0), ..ok },
        PointSpec { at: Vec2::new(1e300, 10.0), ..ok },
    ] {
        assert!(PointTracker::new(&good, s, PointOptions::default()).is_none(), "{s:?}");
    }
    assert!(PointTracker::new(&flat(64, 48), ok, PointOptions::default()).is_none(), "flat pattern");
    // huge boxes are clamped and still track; a NaN threshold falls back to the default
    let mut big = PointTracker::new(
        &good,
        PointSpec { pattern: Vec2::new(1e12, 1e12), search: Vec2::new(1e12, 1e12), ..ok },
        PointOptions { lost_below: f64::NAN, ..PointOptions::default() },
    )
    .expect("clamped");
    let s = big.step(&good);
    assert!(!s.lost && (s.pos - c).length() < 0.1, "{s:?}");
    // frames of the wrong size or unusable are lost
    let mut t = PointTracker::new(&good, ok, PointOptions { on_lost: OnLost::Hold, ..PointOptions::default() }).unwrap();
    for f in [&zero, &one, &short, &huge_claim, &render(32, 32, tex)] {
        assert!(t.step(f).lost);
    }
    assert!(!t.step(&good).lost, "hold recovers");
    // batch entry points
    assert!(track_point(&[], 0, ok, PointOptions::default()).is_none());
    assert!(track_point(std::slice::from_ref(&good), 5, ok, PointOptions::default()).is_none());
    assert_eq!(track_points(std::slice::from_ref(&good), 0, &vec![ok; MAX_POINTS + 10], PointOptions::default()).len(), MAX_POINTS);
    let m = MultiPointTracker::new(&good, &[ok, PointSpec { at: nan, ..ok }], PointOptions::default());
    assert_eq!(m.len(), 2);
    assert!(m.first_samples()[1].lost);
    // planar regions
    let quad = [Vec2::new(10.0, 10.0), Vec2::new(50.0, 10.0), Vec2::new(50.0, 40.0), Vec2::new(10.0, 40.0)];
    assert!(PlanarTracker::new(&good, &[], quad).is_none());
    assert!(PlanarTracker::new(&good, &quad[..2], quad).is_none());
    assert!(PlanarTracker::new(&good, &[nan, quad[1], quad[2]], quad).is_none());
    assert!(PlanarTracker::new(&good, &quad, [nan, quad[1], quad[2], quad[3]]).is_none());
    assert!(PlanarTracker::new(&good, &quad.map(|p| p + Vec2::new(-500.0, -500.0)), quad).is_none(), "region outside the frame");
    assert!(PlanarTracker::new(&good, &vec![quad[0]; MAX_REGION_POINTS + 1], quad).is_none());
    assert!(track_plane(&[], 0, &quad, quad).is_none());
    // solves
    assert!(motion_path(&[], 0, TrackMethod::Position).is_empty());
    let tr = vec![vec![TrackSample { pos: c, confidence: 1.0, lost: false }, TrackSample { pos: nan, confidence: 1.0, lost: false }]; 4];
    assert!(motion_path(&tr, 99, TrackMethod::PositionScaleRotation).iter().all(Option::is_none));
    let mp = motion_path(&tr, 0, TrackMethod::PositionScaleRotation);
    assert!(mp[0].is_some() && mp[1].is_none(), "{mp:?}");
    assert_eq!(stabilise(&tr, 0, TrackMethod::PositionScaleRotation, u32::MAX).len(), 2);
    assert!(stabilise(&tr, 7, TrackMethod::PositionScaleRotation, 3).iter().all(Option::is_none));
    // four identical points: no homography, no panic
    assert!(corner_pin(&tr, 0).unwrap().iter().all(Option::is_none));
    assert!(corner_pin(&[], 0).is_none());
    // low-level helpers
    assert!(crate::track::lk_from(&good.levels, &good.levels, [f32::NAN, 1.0], [0.0, 0.0]).is_none());
    assert!(crate::track::lk_from(&good.levels, &good.levels, [3.0, 1.0], [f32::INFINITY, 0.0]).is_none());
    assert!(Homography { m: [0.0; 9] }.inverse().is_none());
    assert!(Homography { m: [f64::NAN; 9] }.inverse().is_none());
    assert!(good.base().sample(f32::MAX, -f32::MAX).is_finite());
}

#[test]
fn homography_inverse_roundtrips() {
    let h = plane_h(17.0);
    let inv = h.inverse().unwrap();
    for o in OBJ_QUAD {
        let back = inv.apply(h.apply(o).unwrap()).unwrap();
        assert!((back - o).length() < 1e-9);
    }
}

/// C3.8 review regression: a strong-perspective corner pin of a 6240 × 4160 frame (a floor plane
/// receding to a narrow far edge) is a regular map; the singularity test compared its
/// determinant with the largest entry cubed (a pixel offset) and refused it, so the warp drew
/// nothing. Truly singular maps are still refused.
#[test]
fn large_frame_perspective_pins_invert() {
    let (w, h) = (6240.0, 4160.0);
    let src = [Vec2::new(0.0, 0.0), Vec2::new(w, 0.0), Vec2::new(w, h), Vec2::new(0.0, h)];
    for far in [[(2900.0, 1500.0), (3340.0, 1500.0)], [(3100.0, 2000.0), (3140.0, 2000.0)], [(3115.0, 2000.0), (3125.0, 2000.0)]] {
        let dst = [Vec2::new(far[0].0, far[0].1), Vec2::new(far[1].0, far[1].1), Vec2::new(w, h), Vec2::new(0.0, h)];
        let hm = fit_homography(&src, &dst).unwrap();
        let inv = hm.inverse().unwrap_or_else(|| panic!("{far:?}: refused as singular"));
        for (s, d) in src.iter().zip(&dst) {
            let back = inv.apply(*d).unwrap();
            assert!((back - *s).length() < 1e-6 * w, "{far:?}: {back:?} vs {s:?}");
        }
    }
    assert!(Homography { m: [1.0, 2.0, 3.0, 2.0, 4.0, 6.0, 0.0, 0.0, 1.0] }.inverse().is_none());
    assert!(Homography { m: [1.0, 0.0, 3000.0, 2.0, 0.0, 6000.0, 0.0, 0.0, 1.0] }.inverse().is_none());
    assert!(Homography { m: [f64::MAX, 0.0, 0.0, 0.0, f64::MAX, 0.0, 0.0, 0.0, f64::MAX] }.inverse().is_none());
}
