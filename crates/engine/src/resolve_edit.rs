//! DaVinci Resolve's edit set (fork RS4.1): `edit.insert` / `overwrite` / `replace` / `placeOnTop` /
//! `append` / `rippleOverwrite` / `fitToFill` and the Cut page's `smartInsert` / `closeUp` /
//! `sourceOverwrite`.
//!
//! Resolve vocabulary: the **source** is the clip in the Source monitor with its In / Out; every
//! command also takes an optional `clip` (a project item id) that is used instead, with its saved
//! In / Out (or its whole range), so the Cut page can edit a media pool clip without loading it
//! first. The **destination** tracks are the patched ones ([`Session::targeting`]); the **record
//! point** is the timeline In mark when set, else the playhead. Every command is one undo step
//! (multi-step ones are folded into one).

use filmcraft_edit as edit;
use filmcraft_project::{ClipId, ItemId, Param, ParamValue, Project, TrackId};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, edit_item_at, has_seq, item_p, place_item, source_range};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

/// The Close Up zoom (Motion scale multiplier).
const CLOSE_UP_ZOOM: f64 = 1.5;
/// Fit to Fill limits: the speed it may set (a hostile source / fill length cannot ask for more).
const MIN_FIT_SPEED: f64 = 0.01;
const MAX_FIT_SPEED: f64 = 100.0;

fn spec(id: &'static str, label: &'static str, shortcut: Option<&'static str>, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &["Sequence", "Resolve Edit"], shortcut, params: r#"{"clip":itemId?}"#, enabled, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("edit.insert", "Insert", None, can_edit, |s, p| edit_insert(s, p, true)),
        spec("edit.overwrite", "Overwrite", None, can_edit, |s, p| edit_insert(s, p, false)),
        spec("edit.replace", "Replace", None, can_edit, |s, p| replace(s, p, false)),
        spec("edit.placeOnTop", "Place on Top", None, can_edit, place_on_top),
        spec("edit.append", "Append at End", None, can_edit, append),
        spec("edit.rippleOverwrite", "Ripple Overwrite", None, can_edit, |s, p| replace(s, p, true)),
        spec("edit.fitToFill", "Fit to Fill", None, can_fit_to_fill, fit_to_fill),
        spec("edit.smartInsert", "Smart Insert", None, can_edit, smart_insert),
        spec("edit.closeUp", "Close Up", None, can_edit, close_up),
        spec("edit.sourceOverwrite", "Source Overwrite", None, can_edit, source_overwrite),
    ]
}

// ---------------------------------------------------------------------------------------------
// enablement
// ---------------------------------------------------------------------------------------------

fn selected_clip(s: &Session) -> Option<ItemId> {
    s.state.project_selection.iter().copied().find(|i| crate::clip_ops::is_clip_item(&s.project, *i))
}

fn can_edit(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    // the `clip` param (Cut page) needs no Source monitor clip: any clip in the project will do
    if s.state.source_item.is_some() || selected_clip(s).is_some() || s.project.items.keys().any(|i| crate::clip_ops::is_clip_item(&s.project, *i)) {
        Ok(())
    } else {
        Err("the project has no clip to edit in".into())
    }
}

fn can_fit_to_fill(s: &Session) -> std::result::Result<(), String> {
    can_edit(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if q.mark_in.is_some() && q.mark_out.is_some() { Ok(()) } else { Err("mark an In and an Out point on the timeline first".into()) }
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

struct Src {
    item: ItemId,
    range: TimeRange,
    /// Taken from the Source monitor (so its playhead means something).
    from_viewer: bool,
}

/// The clip to edit in: the `clip` param, else the Source monitor's, else the media pool selection.
fn resolve_src(s: &Session, p: &Value, cmd: &str) -> Result<Src> {
    let still = s.prefs.timeline.still_duration(s.sequence_rate());
    let src = if p.get("clip").is_some_and(|v| !v.is_null()) {
        let id = item_p(p, "clip").ok_or_else(|| bad(cmd, "`clip` must be a project item id"))?;
        if !crate::clip_ops::is_clip_item(&s.project, id) {
            return Err(bad(cmd, "`clip` is not a clip in the project"));
        }
        let range = crate::clip_ops::item_range(&s.project, id, still).ok_or_else(|| bad(cmd, "`clip` is not a clip in the project"))?;
        Src { item: id, range, from_viewer: false }
    } else if let Some((item, range)) = source_range(s) {
        Src { item, range, from_viewer: true }
    } else if let Some(id) = selected_clip(s) {
        let range = crate::clip_ops::item_range(&s.project, id, still).ok_or_else(|| bad(cmd, "no source clip"))?;
        Src { item: id, range, from_viewer: false }
    } else {
        return Err(bad(cmd, "no source clip: open one in the Source monitor or pass `clip`"));
    };
    if src.range.duration.0 <= 0 {
        return Err(bad(cmd, "the source has no length between its In and Out"));
    }
    Ok(src)
}

/// Timeline In mark when set, else the playhead.
fn record_point(s: &Session) -> Result<Tick> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    Ok(q.mark_in.unwrap_or(s.playhead()))
}

fn clips_json(ids: &[ClipId]) -> Vec<u64> {
    ids.iter().map(|c| c.0).collect()
}

/// Run a multi-step edit as one undo step; when it fails, undo what it did.
fn atomic<R>(s: &mut Session, label: &str, f: impl FnOnce(&mut Session) -> Result<R>) -> Result<R> {
    let n0 = s.history.undo.len();
    match f(s) {
        Ok(r) => {
            crate::clip_ops::collapse_history(s, n0, label);
            Ok(r)
        }
        Err(e) => {
            for _ in 0..8 {
                if s.history.undo.len() <= n0 || s.undo().is_none() {
                    break;
                }
            }
            s.history.redo.clear();
            Err(e)
        }
    }
}

/// The video track at index `idx` (0 = lowest), adding one when `idx` is just past the top.
fn video_track_at(s: &mut Session, idx: usize, cmd: &str) -> Result<TrackId> {
    let have = s.active_sequence().ok_or(EngineError::NoSequence)?.video_tracks.len();
    if idx > have {
        return Err(bad(cmd, "no video track there"));
    }
    if idx == have {
        crate::sequence_tools::add_tracks(s, &json!({"video": 1}))?;
    }
    s.active_sequence().and_then(|q| q.video_tracks.get(idx)).map(|t| t.id).ok_or_else(|| bad(cmd, "no video track there"))
}

/// Index of the video track above the highest one that has a clip at `at` (0 when none has).
fn first_free_above(s: &Session, at: Tick) -> Result<usize> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    Ok(q.video_tracks.iter().rposition(|t| t.item_at(at).is_some()).map_or(0, |i| i.saturating_add(1)))
}

fn require_video(s: &Session, item: ItemId, cmd: &str) -> Result<()> {
    if s.project.item(item).is_some_and(|i| i.has_video()) { Ok(()) } else { Err(bad(cmd, "the source has no picture")) }
}

/// The clip under the playhead on the destination track.
struct Under {
    track: TrackId,
    start: Tick,
    duration: Tick,
    item: ItemId,
    source_in: Tick,
    speed: f64,
    frame_hold: Option<Tick>,
}

/// The clip under `ph` on the destination track for `src_item` (the video destination when the
/// source has picture, else the audio destination). The track must not be locked.
fn clip_under_playhead(s: &Session, src_item: ItemId, ph: Tick, cmd: &str) -> Result<Under> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let tg = s.targeting();
    let has_v = s.project.item(src_item).is_some_and(|i| i.has_video());
    let tid = if has_v { tg.video_dest } else { tg.audio_dest }.ok_or_else(|| bad(cmd, "no destination track (check source patching)"))?;
    let track = q.track(tid).ok_or_else(|| bad(cmd, "no destination track"))?;
    if track.locked {
        return Err(bad(cmd, "the destination track is locked"));
    }
    let it = track.item_at(ph).ok_or_else(|| bad(cmd, "no clip under the playhead on the destination track"))?;
    Ok(Under { track: tid, start: it.start, duration: it.duration, item: it.item, source_in: it.source_in, speed: it.speed, frame_hold: it.frame_hold })
}

/// Floor `t` to whole frames (at least zero).
fn floor_frames(s: &Session, t: Tick) -> Tick {
    let fd = s.sequence_rate().frame_duration().0.max(1);
    Tick((t.0.max(0) / fd).saturating_mul(fd))
}

// ---------------------------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------------------------

fn edit_insert(s: &mut Session, p: &Value, insert: bool) -> Result<Value> {
    let cmd = if insert { "edit.insert" } else { "edit.overwrite" };
    let src = resolve_src(s, p, cmd)?;
    let at = record_point(s)?;
    edit_item_at(s, src.item, src.range, at, insert)
}

/// `edit.replace` / `edit.rippleOverwrite`.
fn replace(s: &mut Session, p: &Value, ripple: bool) -> Result<Value> {
    let cmd = if ripple { "edit.rippleOverwrite" } else { "edit.replace" };
    let src = resolve_src(s, p, cmd)?;
    let ph = s.playhead();
    let under = clip_under_playhead(s, src.item, ph, cmd)?;
    let tg = s.targeting();
    let rate = s.sequence_rate();
    if ripple {
        // the source's own length; the rest of the timeline moves by the difference
        let len = rate.snap_nearest(src.range.duration).max(rate.frame_duration());
        let old = TimeRange::new(under.start, under.duration);
        let mut tracks = vec![under.track];
        tracks.extend(tg.video_dest.into_iter().chain(tg.audio_dest));
        let ids = atomic(s, "Ripple Overwrite", |s| {
            s.edit_sequence("Ripple Overwrite", |q, ctx, _| {
                edit::extract(q, &tracks, old, ctx);
                Ok(())
            })?;
            place_item(s, src.item, TimeRange::new(src.range.start, len), under.start, tg.video_dest, tg.audio_dest, true, "Ripple Overwrite", None)
        })?;
        s.set_playhead(under.start + len);
        return Ok(json!({"clips": clips_json(&ids), "rippled": (len - under.duration).0}));
    }
    // line up the source frame at the Source monitor playhead with the timeline playhead
    let in_media = if src.from_viewer { Tick(s.state.source_playhead.0.saturating_sub((ph - under.start).0)) } else { src.range.start };
    if in_media.0 < 0 {
        return Err(bad(cmd, "not enough media before the Source playhead to line it up with the timeline playhead"));
    }
    let mut len = under.duration;
    if let Some(end) = crate::media_duration(&s.project, &s.media, src.item) {
        len = len.min(Tick(end.0.saturating_sub(in_media.0)));
    }
    let len = floor_frames(s, len);
    if len.0 <= 0 {
        return Err(bad(cmd, "no media left after the Source playhead to replace with"));
    }
    let ids = place_item(s, src.item, TimeRange::new(in_media, len), under.start, tg.video_dest, tg.audio_dest, false, "Replace", None)?;
    Ok(json!({"clips": clips_json(&ids), "duration": len.0, "short": len < under.duration}))
}

fn place_on_top(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.placeOnTop";
    let src = resolve_src(s, p, cmd)?;
    require_video(s, src.item, cmd)?;
    let at = record_point(s)?;
    let idx = first_free_above(s, at)?;
    let tg = s.targeting();
    let ids = atomic(s, "Place on Top", |s| {
        let vtrack = video_track_at(s, idx, cmd)?;
        place_item(s, src.item, src.range, at, Some(vtrack), tg.audio_dest, false, "Place on Top", None)
    })?;
    s.set_playhead(at + s.sequence_rate().snap_nearest(src.range.duration));
    Ok(json!({"clips": clips_json(&ids), "track": idx}))
}

fn append(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.append";
    let src = resolve_src(s, p, cmd)?;
    let tg = s.targeting();
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let at = s.sequence_rate().snap_nearest(q.duration());
    let ids = place_item(s, src.item, src.range, at, tg.video_dest, tg.audio_dest, false, "Append at End", None)?;
    s.set_playhead(at + s.sequence_rate().snap_nearest(src.range.duration));
    Ok(json!({"clips": clips_json(&ids), "at": at.0}))
}

fn fit_to_fill(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.fitToFill";
    let src = resolve_src(s, p, cmd)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let rate = q.settings.frame_rate;
    let (Some(a), Some(b)) = (q.mark_in, q.mark_out) else {
        return Err(bad(cmd, "mark an In and an Out point on the timeline first"));
    };
    let fill = TimeRange::from_bounds(a, b + rate.frame_duration());
    if fill.duration.0 <= 0 {
        return Err(bad(cmd, "the In point is after the Out point"));
    }
    let speed = src.range.duration.0 as f64 / fill.duration.0 as f64;
    if !speed.is_finite() || !(MIN_FIT_SPEED..=MAX_FIT_SPEED).contains(&speed) {
        return Err(bad(cmd, format!("the source would need {:.1}% speed; fit to fill allows 1% to 10000%", speed * 100.0)));
    }
    let tg = s.targeting();
    let ids = atomic(s, "Fit to Fill", |s| {
        // place exactly the filled span (so nothing outside In..Out is overwritten), then stretch it
        // to show the whole marked source
        let ids =
            place_item(s, src.item, TimeRange::new(src.range.start, fill.duration), fill.start, tg.video_dest, tg.audio_dest, false, "Fit to Fill", None)?;
        let placed = ids.clone();
        s.edit_sequence("Fit to Fill", move |q, _, _| {
            for id in &placed {
                if let Some((_, it)) = q.find_item_mut(*id) {
                    it.speed = speed;
                }
            }
            Ok(())
        })?;
        Ok(ids)
    })?;
    s.set_playhead(fill.end());
    Ok(json!({"clips": clips_json(&ids), "speed": speed}))
}

fn smart_insert(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.smartInsert";
    let src = resolve_src(s, p, cmd)?;
    let ph = s.playhead();
    let tg = s.targeting();
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // the edit points (clip starts and ends) of the destination tracks; the earlier one on a tie
    let nearest = tg
        .video_dest
        .into_iter()
        .chain(tg.audio_dest)
        .filter_map(|t| q.track(t))
        .flat_map(|t| t.items.iter().flat_map(|i| [i.start, i.end()]))
        .min_by_key(|e| ((e.0 - ph.0).unsigned_abs(), e.0));
    let at = nearest.unwrap_or(ph);
    edit_item_at(s, src.item, src.range, at, true)
}

/// Multiply the clip's Motion scale (percent) by `zoom`.
fn zoom_clip(p: &mut Project, seq: ItemId, clip: ClipId, zoom: f64) -> Result<()> {
    let it = p.sequence_mut(seq).and_then(|q| q.find_item_mut(clip)).map(|(_, it)| it).ok_or_else(|| bad("edit.closeUp", "the placed clip is gone"))?;
    let m = it.effects.iter_mut().find(|e| e.effect == "motion").ok_or_else(|| bad("edit.closeUp", "the clip has no Motion effect"))?;
    let cur = match m.params.get("scale").map(|p| &p.value) {
        Some(ParamValue::Float(f)) if f.is_finite() => *f,
        _ => 100.0,
    };
    m.params.insert("scale".into(), Param::new(ParamValue::Float(cur * zoom)));
    Ok(())
}

fn close_up(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.closeUp";
    let src = resolve_src(s, p, cmd)?;
    require_video(s, src.item, cmd)?;
    let at = s.playhead();
    let idx = first_free_above(s, at)?;
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let ids = atomic(s, "Close Up", |s| {
        let vtrack = video_track_at(s, idx, cmd)?;
        // picture only: the close-up is a cutaway over what is already playing
        let ids = place_item(s, src.item, src.range, at, Some(vtrack), None, false, "Close Up", None)?;
        let placed = ids.clone();
        s.edit("Close Up", move |pr, _| {
            for id in &placed {
                zoom_clip(pr, seq, *id, CLOSE_UP_ZOOM)?;
            }
            Ok(())
        })?;
        Ok(ids)
    })?;
    s.set_playhead(at + s.sequence_rate().snap_nearest(src.range.duration));
    Ok(json!({"clips": clips_json(&ids), "track": idx, "zoom": CLOSE_UP_ZOOM}))
}

/// The timecode origin of an item's media, in ticks (the media's start timecode frames).
fn timecode_origin(p: &Project, item: ItemId) -> Option<Tick> {
    let (_, media, _) = p.resolve_media(item)?;
    let frames = media.info.start_timecode.unwrap_or(0);
    let fd = media.frame_rate().frame_duration().0;
    fd.checked_mul(frames).map(Tick)
}

fn source_overwrite(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.sourceOverwrite";
    let src = resolve_src(s, p, cmd)?;
    require_video(s, src.item, cmd)?;
    let ph = s.playhead();
    let under = clip_under_playhead(s, src.item, ph, cmd)?;
    let (Some(dest_origin), Some(src_origin)) = (timecode_origin(&s.project, under.item), timecode_origin(&s.project, src.item)) else {
        return Err(bad(cmd, "the clips have no source timecode"));
    };
    // media time of the destination clip at the playhead (a held frame has one time)
    let dest_media = match under.frame_hold {
        Some(h) => h,
        None => Tick(under.source_in.0.saturating_add(((ph - under.start).0 as f64 * under.speed.abs()).round() as i64)),
    };
    let dest_tc = dest_origin.0.saturating_add(dest_media.0);
    let src_tc_in = src_origin.0.saturating_add(src.range.start.0);
    // the timeline position of the source's In point so that both timecodes meet at the playhead
    let at = s.sequence_rate().snap_nearest(Tick(ph.0.saturating_add(src_tc_in).saturating_sub(dest_tc)));
    let len = s.sequence_rate().snap_nearest(src.range.duration).max(s.sequence_rate().frame_duration());
    let placed = TimeRange::new(at, len);
    if at.0 < 0 || !placed.overlaps(&TimeRange::new(under.start, under.duration)) || !(placed.start <= ph && ph < placed.end()) {
        return Err(bad(cmd, "the source timecode does not overlap the clip under the playhead"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let dest_idx = q.video_tracks.iter().position(|t| t.id == under.track).ok_or_else(|| bad(cmd, "no destination track"))?;
    let ids = atomic(s, "Source Overwrite", |s| {
        let vtrack = video_track_at(s, dest_idx.saturating_add(1), cmd)?;
        place_item(s, src.item, src.range, at, Some(vtrack), None, false, "Source Overwrite", None)
    })?;
    Ok(json!({"clips": clips_json(&ids), "at": at.0}))
}
