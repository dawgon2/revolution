//! Planar-tracked masks and grade windows (fork G4.4): `masks.trackPlanar` / `masks.clearPlanar`.
//!
//! A window on a sign, a screen or a wall should stay glued to that surface while the camera
//! moves, including its perspective. `masks.trackPlanar` runs COMPOSITOR-PLAN C3's planar tracker
//! ([`PlanarTracker`]: a homography per frame, re-anchored on the reference so it doesn't drift)
//! over the clip's own frames, from the playhead forward and / or backward, with the window's
//! outline at the playhead as the region to follow. The result is stored on the mask as a
//! [`PlanarTrack`] (see its doc for why the motion is kept rather than baked into path
//! keyframes); the renderer draws the outline through it (`filmcraft_render::mask::prepare`), on
//! the CPU and the GPU alike.
//!
//! **The job.** Frames are decoded one at a time at most [`TRACK_MAX_SIDE`] on the long side and
//! shared by `Arc` (the tracker keeps the reference and the previous frame; nothing else is held,
//! so a 6.2K clip costs three ~13 MB pyramids, not the shot). It runs in the background
//! (`jobs.cancel` stops it and keeps what was tracked), stops after [`MAX_LOST`] consecutive
//! frames without the plane, and writes everything in **one** undo step when it ends, only if the
//! window is still what was tracked (otherwise a toast says nothing was written).
//!
//! **Coordinates.** Masks store clip pixels (continuous, pixel centres at +0.5); the tracker's
//! frames have pixel centres at integers. `to_frame` / `to_clip` here are the only conversion.
//! A track made on a window that already follows one is composed with it (the new motion starts
//! from where the old one put the window at the playhead), so re-tracking a stretch never moves
//! the window where it was right. On the first track, path keyframes the window already had (from
//! drawing or `masks.track`) are re-expressed in the reference frame so nothing jumps; clearing
//! does the reverse, leaving the window where it is seen at each keyframe (and at the playhead
//! for an unkeyframed path).
//!
//! Time: clip media time ([`Tick`]), as mask keyframes; frame indices exist only inside the job.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use filmcraft_geom::Vec2;
use filmcraft_project::{Mask, MaskPath, ParamValue, PlanarKey, PlanarTrack, mask::MAX_PLANAR_KEYS};
use filmcraft_render::comp::track::{TrackWarp, path_through};
use filmcraft_render::track::{Gray, Homography, Prepared};
use filmcraft_render::track_run::{MAX_REGION_POINTS, PlanarTracker};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq, time_p, u64_p};
use crate::masks::{MaskSel, masks_of, masks_of_mut, owner_for, target_clip};
use crate::{EngineError, Result, Session};

/// Longest side of the frames the planar tracker sees (clip pixels are scaled down to it).
pub const TRACK_MAX_SIDE: f64 = 1920.0;
/// Consecutive frames without the plane after which a direction stops.
pub const MAX_LOST: usize = 48;
/// Most path points of a window tracked (a hostile path can't make the outline huge).
const MAX_SHAPE_VERTICES: usize = 10_000;

const TRACK: &str = "masks.trackPlanar";
const CLEAR: &str = "masks.clearPlanar";

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: can_track, run, journal: true }
}

/// A sequence is open and there is a mask (window) or a clip to address.
fn can_track(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.selected_mask.is_none() && s.state.selection.is_empty() {
        return Err("select a window or a mask to track (or pass `clip` and `mask`)".into());
    }
    Ok(())
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            TRACK,
            "Track Window (Planar)",
            r#"{"clip":id?,"node":id? (a grade node: its windows)|"effect":index|id?,"mask":n? (default: the selected mask),"direction":"both"|"forward"|"backward"? (default both, from the playhead),"frames":n? (per direction; default to the clip's end),"time":ticks? (default the playhead),"wait":bool?}"#,
            track_planar,
        ),
        spec(
            CLEAR,
            "Clear Planar Track",
            r#"{"clip":id?,"node":id?|"effect":index|id?,"mask":n?,"time":ticks? (an unkeyframed window stays where it is seen then; default the playhead)}"#,
            clear_planar,
        ),
    ]
}

/// A running planar track; its keys are written when it ends ([`poll`]).
pub struct Pending {
    pub job: u64,
    pub target: MaskSel,
    seq: filmcraft_project::ItemId,
    /// What the track started from ([`setup_of`]); a different mask at the end is not written.
    setup: Value,
    /// The previous track's map at the start frame and its reference (identity / start without).
    base: (Homography, Tick),
    had_track: bool,
    /// Frame pixels ↔ clip pixels: (kx, ky) frame pixels per clip pixel (measured by the job on
    /// the first decoded frame).
    k: Arc<Mutex<(f64, f64)>>,
    found: Arc<Mutex<Vec<(Tick, Homography)>>>,
}

/// The parts of a mask a planar result depends on.
fn setup_of(m: &Mask) -> Value {
    json!([m.shape, m.path, m.planar])
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clip pixels → frame pixels (pixel centres +0.5 → integers) as a homography, for `k` frame
/// pixels per clip pixel.
fn to_frame(k: (f64, f64)) -> Homography {
    Homography { m: [k.0, 0.0, -0.5, 0.0, k.1, -0.5, 0.0, 0.0, 1.0] }
}

fn to_clip(k: (f64, f64)) -> Homography {
    let (kx, ky) = (if k.0 > 0.0 { k.0 } else { 1.0 }, if k.1 > 0.0 { k.1 } else { 1.0 });
    Homography { m: [1.0 / kx, 0.0, 0.5 / kx, 0.0, 1.0 / ky, 0.5 / ky, 0.0, 0.0, 1.0] }
}

/// The addressed mask: (clip, owner selection, the mask, the clip's track item).
fn addressed(s: &Session, p: &Value, cmd: &str) -> Result<(MaskSel, Mask, filmcraft_project::TrackItem)> {
    let clip = target_clip(s, p)?;
    let sel = s.state.selected_mask;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad(cmd, "no such clip"))?;
    let o = owner_for(it, p, sel, clip).ok_or_else(|| bad(cmd, "no such effect or grade node on clip"))?;
    let mi = u64_p(p, "mask")
        .and_then(|m| usize::try_from(m).ok())
        .or(sel.filter(|x| x.clip == clip && x.owner() == o).map(|x| x.mask))
        .ok_or_else(|| bad(cmd, "need `mask` (or select a window)"))?;
    let mask = masks_of(it, o).and_then(|v| v.get(mi)).cloned().ok_or_else(|| bad(cmd, format!("no mask {mi}")))?;
    Ok((o.sel(clip, mi), mask, it.clone()))
}

fn track_planar(s: &mut Session, p: &Value) -> Result<Value> {
    let (fwd, back) = match p.get("direction") {
        None | Some(Value::Null) => (true, true),
        Some(Value::String(d)) => match d.as_str() {
            "both" => (true, true),
            "forward" => (true, false),
            "backward" => (false, true),
            d => return Err(bad(TRACK, format!("direction `{d}`: both, forward or backward"))),
        },
        Some(_) => return Err(bad(TRACK, "`direction` is \"both\", \"forward\" or \"backward\"")),
    };
    let (target, mask, it) = addressed(s, p, TRACK)?;
    if s.planar_jobs.iter().any(|j| j.target == target) || s.mask_jobs.iter().any(|j| j.target == target) {
        return Err(bad(TRACK, "this window is already being tracked"));
    }
    if it.frame_hold.is_some() {
        return Err(bad(TRACK, "the clip is a frame hold: its picture doesn't move"));
    }
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let rate = s.project.item(it.item).map(|i| i.frame_rate()).unwrap_or(seq.settings.frame_rate);
    let fd = rate.frame_duration().0.max(1);
    let ph = time_p(s, p, "").unwrap_or(s.playhead());
    let mt0 = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let (a, b) = (it.source_time_at(it.start), it.source_time_at(it.end() - Tick(1)));
    let (lo, hi) = (a.min(b), a.max(b));
    let cap = |room: i64| -> i64 {
        let room = room.clamp(0, MAX_PLANAR_KEYS as i64 / 2 - 1);
        match u64_p(p, "frames") {
            Some(n) => i64::try_from(n).unwrap_or(i64::MAX).min(room),
            None => room,
        }
    };
    let steps_f = if fwd { cap((hi - mt0).0 / fd) } else { 0 };
    let steps_b = if back { cap((mt0 - lo).0 / fd) } else { 0 };
    if steps_f + steps_b <= 0 {
        return Err(bad(TRACK, "nothing to track: the playhead is at the clip's only frame in that direction"));
    }
    // the region: the outline where it is seen at the start (through an earlier track)
    let old = mask.planar.as_ref().and_then(|pl| pl.homography_at(mt0).map(|h| (Homography { m: h }, pl.reference)));
    let had_track = mask.planar.as_ref().is_some_and(|pl| !pl.keys.is_empty());
    let base = old.unwrap_or((Homography::IDENTITY, mt0));
    let src = s.source(it.item).ok_or_else(|| EngineError::Other("the clip has no media to track".into()))?;
    let size = filmcraft_render::source_size(&s.project, it.item).ok_or_else(|| bad(TRACK, "the clip has no picture"))?;
    let (sw, sh) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let scale = (TRACK_MAX_SIDE / sw.max(sh)).min(1.0) as f32;
    let shown = path_through(&mask.path_at(mt0), TrackWarp::Projective(base.0));
    let outline: Vec<Vec2> = if mask.shape == filmcraft_project::MaskShape::Linear {
        // a gradient has no area: follow the square its two points span
        match (shown.vertices.first(), shown.vertices.last()) {
            (Some(a), Some(b)) => {
                let (c, d) = (a.p.lerp(b.p, 0.5), (b.p - a.p) * 0.5);
                let n = Vec2::new(-d.y, d.x);
                vec![c - d - n, c + d - n, c + d + n, c - d + n]
            }
            _ => Vec::new(),
        }
    } else if shown.vertices.len() > MAX_SHAPE_VERTICES {
        return Err(bad(TRACK, format!("the window has more than {MAX_SHAPE_VERTICES} points")));
    } else {
        // (the tracker takes at most MAX_REGION_POINTS: a big curved window is thinned out)
        let flat = shown.flatten(0.5);
        let step = flat.len().div_ceil(MAX_REGION_POINTS).max(1);
        flat.into_iter().step_by(step).collect()
    };
    if outline.len() < 3 || !outline.iter().all(|q| q.x.is_finite() && q.y.is_finite()) {
        return Err(bad(TRACK, "the window has no area to track"));
    }
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).saturating_add(1);
    let dir = match (fwd, back) {
        (true, true) => "both ways",
        (true, false) => "forward",
        _ => "backward",
    };
    let job = crate::Job { id, label: format!("Planar track {} ({dir})", mask.name), progress: Default::default(), result: Default::default() };
    let total = (steps_f + steps_b) as u64;
    job.progress.total.store(total, Ordering::Relaxed);
    let found: Arc<Mutex<Vec<(Tick, Homography)>>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), found.clone());
    // (the frame size the decoder gives is measured on the first frame)
    let k_cell: Arc<Mutex<(f64, f64)>> = Arc::new(Mutex::new((f64::from(scale), f64::from(scale))));
    let k_out = k_cell.clone();
    let run = move || {
        let t0 = web_time::Instant::now();
        let frame = |t: Tick| -> Option<(Arc<Prepared>, (f64, f64))> {
            let f = src.video_frame(filmcraft_media::FrameRequest { time: t, scale }).ok()?;
            let g = Gray::from_rgba8(f.width as usize, f.height as usize, &f.to_rgba8());
            Some((Arc::new(Prepared::new(g)), (f64::from(f.width) / sw, f64::from(f.height) / sh)))
        };
        let mut err: Option<String> = None;
        let mut done = 0u64;
        match frame(mt0) {
            None => err = Some("can't decode the clip".into()),
            Some((first, k)) => {
                *lock(&k_out) = k;
                let tf = to_frame(k);
                let region: Vec<Vec2> = outline.iter().filter_map(|q| tf.apply(*q)).collect();
                let lo = region.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |a, q| Vec2::new(a.x.min(q.x), a.y.min(q.y)));
                let hi = region.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, q| Vec2::new(a.x.max(q.x), a.y.max(q.y)));
                let quad = [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
                match PlanarTracker::new_shared(first, &region, quad) {
                    None => err = Some("the window can't be tracked: it needs detail (texture, edges, print) inside it, on the picture".into()),
                    Some(tracker) => {
                        lock(&out).push((mt0, Homography::IDENTITY));
                        for (backward, steps) in [(false, steps_f), (true, steps_b)] {
                            if steps <= 0 || err.is_some() {
                                continue;
                            }
                            let mut tr = tracker.clone();
                            let mut lost = 0usize;
                            for step in 1..=steps {
                                if prog.cancel.load(Ordering::Relaxed) {
                                    err = Some("stopped".into());
                                    break;
                                }
                                let dt = Tick(fd.saturating_mul(step));
                                let t = if backward { mt0 - dt } else { mt0 + dt };
                                let Some((next, _)) = frame(t) else {
                                    err = Some(format!("can't decode frame {step}"));
                                    break;
                                };
                                let sm = tr.step(&next);
                                drop(next);
                                done += 1;
                                prog.done.store(done, Ordering::Relaxed);
                                if sm.lost {
                                    lost += 1;
                                    if lost >= MAX_LOST {
                                        *lock(&prog.status) =
                                            format!("lost the plane {} frame(s) {}", step as usize - lost, if backward { "back" } else { "on" });
                                        break;
                                    }
                                } else {
                                    lost = 0;
                                    lock(&out).push((t, sm.homography));
                                }
                                *lock(&prog.status) = format!("Frame {done} of {total} · {} features", sm.inliers);
                            }
                        }
                    }
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        let r = match &err {
            Some(e) if e != "stopped" => Err(e.clone()),
            _ => Ok(filmcraft_export::Report {
                path: String::new(),
                frames: done,
                seconds: secs,
                bytes: 0,
                render_fps: done as f64 / secs.max(1e-6),
                extra_files: Vec::new(),
            }),
        };
        *lock(&prog.status) = match &err {
            Some(e) => format!("{e} ({done} frame(s) tracked)"),
            None => format!("Tracked {done} frame(s) in {secs:.1}s"),
        };
        prog.finished.store(true, Ordering::Relaxed);
        *lock(&res) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.planar_jobs.push(Pending { job: id, target, seq: seq_id, setup: setup_of(&mask), base, had_track, k: k_cell, found });
    if crate::commands::bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
    } else if let Err(e) = std::thread::Builder::new().name("filmcraft-planar-track".into()).spawn(run) {
        s.planar_jobs.retain(|j| j.job != id);
        s.jobs.retain(|j| j.id != id);
        return Err(EngineError::Other(e.to_string()));
    }
    let st = s.jobs.iter().find(|j| j.id == id).map(crate::Job::to_json).unwrap_or(Value::Null);
    Ok(json!({"job": id, "frames": total, "status": st}))
}

/// Re-express a path keyed in one space through `h` (used to move keyframes in and out of the
/// reference frame). The path is kept where it is when the map can't take it.
fn through(path: &MaskPath, h: &Homography) -> MaskPath {
    path_through(path, TrackWarp::Projective(*h))
}

/// Write a finished track's keys onto its mask (one undo step).
fn write(m: &mut Mask, pj: &Pending, found: &[(Tick, Homography)]) {
    let k = *lock(&pj.k);
    let (from_frame, back) = (to_clip(k), to_frame(k));
    let (base, reference) = pj.base;
    let keys: Vec<PlanarKey> = found
        .iter()
        .filter_map(|(t, g)| {
            // reference → start (the earlier track) → t (this one, in clip pixels)
            let h = from_frame.after(g).after(&back).after(&base);
            filmcraft_project::mask::clean_homography(h.m).map(|h| PlanarKey { time: *t, h })
        })
        .collect();
    let mut track = m.planar.take().filter(|_| pj.had_track).unwrap_or(PlanarTrack { reference, keys: Vec::new() });
    // only the times actually tracked are replaced: a track that stopped early (lost, stopped,
    // a frame it couldn't decode) keeps the earlier track's keys beyond where it got to
    let lo = found.iter().map(|(t, _)| *t).min().unwrap_or(Tick::ZERO);
    let hi = found.iter().map(|(t, _)| *t).max().unwrap_or(Tick::ZERO);
    track.keys.retain(|k| k.time < lo || k.time > hi);
    track.keys.extend(keys);
    let track = track.cleaned();
    if !pj.had_track {
        // keyframes drawn in the picture's own coordinates move into the reference frame
        for k in &mut m.path.keyframes {
            if let (ParamValue::Path(path), Some(h)) = (&k.value, track.homography_at(k.time).and_then(|h| Homography { m: h }.inverse())) {
                k.value = ParamValue::Path(through(path, &h));
            }
        }
    }
    m.planar = Some(track);
}

/// Writes the keys of finished planar tracks (one undo step each) and drops them. Called from
/// [`Session::poll_persistence`] and after synchronous runs.
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while let Some(pj) = s.planar_jobs.get(i) {
        let finished = s.jobs.iter().find(|j| j.id == pj.job).is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.planar_jobs.remove(i);
        let found = std::mem::take(&mut *lock(&pj.found));
        // (the start frame alone moves nothing)
        if found.len() < 2 {
            continue;
        }
        let r = s.edit("Track Window (Planar)", |pr, _| {
            let q = pr.sequence_mut(pj.seq).ok_or(EngineError::NoSequence)?;
            let (_, it) = q.find_item_mut(pj.target.clip).ok_or(filmcraft_edit::EditError::NoItem(pj.target.clip))?;
            let m = masks_of_mut(it, pj.target.owner()).and_then(|v| v.get_mut(pj.target.mask)).ok_or_else(|| bad(TRACK, "the window is gone"))?;
            if setup_of(m) != pj.setup {
                return Err(bad(TRACK, "the window was changed while it was being tracked: nothing was written; track it again"));
            }
            write(m, &pj, &found);
            Ok(())
        });
        if let Err(e) = r {
            s.error_toast(TRACK, e.to_string());
        }
    }
}

fn clear_planar(s: &mut Session, p: &Value) -> Result<Value> {
    let (target, mask, it) = addressed(s, p, CLEAR)?;
    if mask.planar.is_none() {
        return Err(bad(CLEAR, "the window follows no planar track"));
    }
    if s.planar_jobs.iter().any(|j| j.target == target) {
        return Err(bad(CLEAR, "the window is being tracked: stop it first (jobs.cancel)"));
    }
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let ph = time_p(s, p, "").unwrap_or(s.playhead());
    let et = it.effect_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    s.edit("Clear Planar Track", move |pr, _| {
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let (_, it) = q.find_item_mut(target.clip).ok_or(filmcraft_edit::EditError::NoItem(target.clip))?;
        let m = masks_of_mut(it, target.owner()).and_then(|v| v.get_mut(target.mask)).ok_or_else(|| bad(CLEAR, "the window is gone"))?;
        let Some(track) = m.planar.take() else { return Ok(()) };
        let at = |t: Tick| track.homography_at(t).map(|h| Homography { m: h });
        // the window stays where it is seen: each keyframe at its own time, a static path at `et`
        if m.path.keyframes.is_empty() {
            if let (ParamValue::Path(path), Some(h)) = (&m.path.value, at(et)) {
                m.path.value = ParamValue::Path(through(path, &h));
            }
        } else {
            for k in &mut m.path.keyframes {
                if let (ParamValue::Path(path), Some(h)) = (&k.value, at(k.time)) {
                    k.value = ParamValue::Path(through(path, &h));
                }
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

/// JSON summary of a mask's planar track for `masks.list` (None without one).
pub fn planar_json(m: &Mask) -> Option<Value> {
    let p = m.planar.as_ref()?;
    Some(json!({
        "reference": p.reference.0,
        "keys": p.keys.len(),
        "from": p.keys.first().map(|k| k.time.0),
        "to": p.keys.last().map(|k| k.time.0),
    }))
}

/// Largest |coordinate| (clip pixels) an on-screen gesture may map back to: beyond it the
/// inverse is ill-conditioned (the point is near the plane's horizon).
const MAX_BACK: f64 = 1e7;

/// On-screen point `q` back in a planar-tracked mask's reference frame (`inv` = `h`⁻¹), only
/// where that point is in front of the plane (`w` > 0 under `h`, as the renderer requires) and
/// within ±[`MAX_BACK`]; None beyond the horizon.
pub fn to_reference(h: &Homography, inv: &Homography, q: Vec2) -> Option<Vec2> {
    let b = inv.apply(q)?;
    (b.x.abs() <= MAX_BACK && b.y.abs() <= MAX_BACK && filmcraft_render::mask::ahead(h, b).is_some()).then_some(b)
}

/// `masks.translate` / `masks.moveVertex` / `masks.addVertex` take on-screen positions; for a
/// planar-tracked mask they are mapped back into its reference frame here: (map, inverse) at `t`.
pub fn screen_maps(m: &Mask, t: Tick) -> Option<(Homography, Homography)> {
    let h = filmcraft_render::mask::planar_warp(m, t)?;
    Some((h, h.inverse()?))
}

#[cfg(test)]
#[path = "masks_planar_tests.rs"]
mod tests;
