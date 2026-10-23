//! Tracker and Planar Tracker comp nodes (fork C3.5a): placing what to track, analysing it in the
//! background and writing the result as keyframes the comp renders from
//! (`filmcraft_render::comp::track`).
//!
//! `comp.track.analyse` renders the node's input 0 (the comp graph above it, with the tracks below
//! the comp clip as Layer sources) frame by frame at comp time, one frame at a time (no shot is
//! held in memory), and feeds it to the point trackers (`MultiPointTracker`) or the planar tracker
//! (`PlanarTracker`, frames shared by `Arc`). When the job ends (done, stopped or lost) every
//! tracked frame becomes keyframes in **one** undo step, replacing the analysed span.
//!
//! Coordinates: the node stores comp pixels (continuous, pixel centres at +0.5, as Transform and
//! the masks); the trackers see frames of at most [`TRACK_MAX_SIDE`] on the long side with pixel
//! centres at integers. The conversion is here and only here. Time is comp time (`Tick`); frame
//! indices exist only inside the job.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use filmcraft_geom::Vec2;
use filmcraft_project::comp::{self, Comp, CompKind, TrackOperation, TrackerPoint};
use filmcraft_project::dag::NodeId;
use filmcraft_project::{ClipId, ItemId, Keyframe, MaskPath, MaskVertex, Param, ParamValue, TrackMethod};
use filmcraft_render::comp::track::reference_quad;
use filmcraft_render::track::{Gray, Prepared, fit_homography};
use filmcraft_render::track_run::{DEFAULT_LOST_BELOW, MultiPointTracker, OnLost, PlanarTracker, PointOptions, PointSpec};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p, u64_p};
use crate::comp::{comp_of, edit_comp, target, top_level_only};
use crate::{EngineError, Result, Session};

/// Most frames one analysis tracks after its first (the keyframe cap of a track).
const MAX_FRAMES: i64 = comp::MAX_TRACK_KEYS as i64 - 1;
/// Longest side of the frames the trackers see (comp pixels are scaled down to it).
pub const TRACK_MAX_SIDE: f64 = 1920.0;
/// Consecutive lost frames after which a planar analysis stops.
const PLANAR_MAX_LOST: usize = 48;
/// Largest coordinate accepted from a command (comp pixels).
const MAX_COORD: f64 = 1e7;

const ANALYSE: &str = "comp.track.analyse";

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            ANALYSE,
            "Analyse Tracker",
            r#"{"comp":id?,"node":id,"from":ticks? (comp time; default: the node's reference),"to":ticks? (default: the comp's last frame, or 0 backward),"direction":"forward"|"backward"?,"clip":id? (the comp clip whose lower tracks Layer sources read; default: the selected comp clip),"wait":bool?}"#,
            analyse,
        ),
        CommandSpec { journal: false, ..spec("comp.track.cancel", "Stop Tracker Analysis", r#"{"comp":id?,"node":id?}"#, cancel) },
        spec(
            "comp.track.setPoints",
            "Set Tracker Points",
            r#"{"comp":id?,"group":[id]?,"node":id,"points":[{"name":str?,"at":[x,y],"pattern":[hw,hh]|n?,"search":[hw,hh]|n?} | a point from comp.get],"gesture":n?}"#,
            set_points,
        ),
        spec(
            "comp.track.setOptions",
            "Tracker Settings",
            r#"{"comp":id?,"group":[id]?,"node":id,"operation":"matchMove"|"stabilise"|"cornerPin"?,"smoothRadius":n? (frames),"reference":ticks?,"method":"position"|"positionRotation"|"positionScaleRotation"?}"#,
            set_options,
        ),
        spec(
            "comp.planar.setRegion",
            "Set Planar Region",
            r#"{"comp":id?,"group":[id]?,"node":id,"region":[[x,y]…] | mask path,"quad":[[x,y],[x,y],[x,y],[x,y]]?,"reference":ticks?,"gesture":n?}"#,
            set_region,
        ),
        spec("comp.track.clear", "Clear Track", r#"{"comp":id?,"group":[id]?,"node":id,"from":ticks?,"to":ticks?}"#, clear),
    ]
}

/// A running analysis; its keyframes are written when it ends ([`poll`]).
pub struct Pending {
    pub job: u64,
    pub comp: ItemId,
    pub node: NodeId,
    /// The requested span (comp time, low ..= high): keyframes in it are replaced.
    span: (Tick, Tick),
    /// What the analysis was set up from ([`setup_of`]): when the node no longer matches it at
    /// the end (points added / removed / re-boxed, region or quad redrawn, comp resized), the
    /// result belongs to a different setup and is not written.
    setup: Value,
    found: Arc<Mutex<Found>>,
}

/// The parts of a tracker node (and its comp) an analysis result depends on: the comp size, and
/// the points' names and boxes in order (Tracker) or the region and quad (Planar Tracker). Not
/// the analysed keyframes (an analysis replaces those) nor the operation / reference.
fn setup_of(c: &Comp, node: NodeId) -> Value {
    let kind = match c.node(node).map(|n| &n.kind) {
        Some(CompKind::Tracker { points, .. }) => {
            json!(points.iter().map(|p| json!([p.name, [p.pattern.x, p.pattern.y], [p.search.x, p.search.y]])).collect::<Vec<_>>())
        }
        Some(CompKind::PlanarTracker { region, quad, .. }) => json!({"region": region, "quad": quad.map(|q| [q.x, q.y])}),
        _ => Value::Null,
    };
    json!({"size": [c.width, c.height], "node": kind})
}

/// Comp pixels (continuous, pixel centres at +0.5) ↔ the trackers' frame pixels (at most
/// [`TRACK_MAX_SIDE`] on the long side, pixel centres at integers).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FrameMap {
    /// The trackers' frame size.
    pub size: (usize, usize),
    kx: f64,
    ky: f64,
}

impl FrameMap {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        let (cw, ch) = (f64::from(width.max(1)), f64::from(height.max(1)));
        let k = (TRACK_MAX_SIDE / cw.max(ch)).min(1.0);
        let size = (((cw * k).round() as usize).max(1), ((ch * k).round() as usize).max(1));
        Self { size, kx: size.0 as f64 / cw, ky: size.1 as f64 / ch }
    }
    /// A comp point in frame pixels.
    pub(crate) fn to_frame(self, p: Vec2) -> Vec2 {
        Vec2::new(p.x * self.kx - 0.5, p.y * self.ky - 0.5)
    }
    /// A frame point in comp pixels.
    pub(crate) fn to_comp(self, p: Vec2) -> Vec2 {
        Vec2::new((p.x + 0.5) / self.kx, (p.y + 0.5) / self.ky)
    }
    /// A comp length (box half-size) in frame pixels.
    pub(crate) fn scale(self, v: Vec2) -> Vec2 {
        Vec2::new(v.x * self.kx, v.y * self.ky)
    }
}

/// What a job tracked so far (comp time, comp pixels, confidence 0..1).
#[derive(Default)]
struct Found {
    points: Vec<Vec<(Tick, Vec2, f32)>>,
    plane: Vec<(Tick, [Vec2; 4], f32)>,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn node_p(p: &Value, cmd: &str) -> Result<NodeId> {
    u64_p(p, "node").ok_or_else(|| bad(cmd, "need `node` (a Tracker or Planar Tracker node id)"))
}

/// A comp time parameter in whole ticks, within a day of zero.
pub(crate) fn ticks_p(p: &Value, key: &str, cmd: &str) -> Result<Option<Tick>> {
    let day = TICKS_PER_SECOND.saturating_mul(86_400);
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .filter(|t| (-day..=day).contains(t))
            .map(|t| Some(Tick(t)))
            .ok_or_else(|| bad(cmd, format!("`{key}` must be whole ticks ({TICKS_PER_SECOND}/s) within a day"))),
    }
}

fn finite(v: Vec2) -> bool {
    v.x.is_finite() && v.y.is_finite() && v.x.abs() <= MAX_COORD && v.y.abs() <= MAX_COORD
}

/// `[x, y]` (or `{"x":…,"y":…}`) as a finite point.
fn vec2_of(v: &Value) -> Option<Vec2> {
    let p = match v {
        Value::Array(a) if a.len() == 2 => Vec2::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?),
        Value::Object(_) => Vec2::new(v.get("x")?.as_f64()?, v.get("y")?.as_f64()?),
        _ => return None,
    };
    finite(p).then_some(p)
}

/// A box half-size: `[hw, hh]` or one number for both.
fn half_of(v: &Value) -> Option<Vec2> {
    let h = match v.as_f64() {
        Some(n) => Vec2::new(n, n),
        None => vec2_of(v)?,
    };
    (finite(h) && h.x > 0.0 && h.y > 0.0).then_some(h)
}

/// The addressed node's kind, or why not.
fn tracker_kind<'a>(c: &'a Comp, node: NodeId, cmd: &str) -> Result<&'a CompKind> {
    let n = c.node(node).ok_or_else(|| bad(cmd, format!("no node {node}")))?;
    match &n.kind {
        k @ (CompKind::Tracker { .. } | CompKind::PlanarTracker { .. }) => Ok(k),
        _ => Err(bad(cmd, format!("node {node} is not a Tracker or Planar Tracker node"))),
    }
}

/// Undo merging for drags on the monitor: the overlay sends the same `gesture` while it drags.
fn gesture_key(s: &Session, p: &Value, cmd: &str, node: NodeId) -> Result<Option<String>> {
    match p.get("gesture").and_then(Value::as_u64) {
        Some(g) => Ok(Some(format!("{cmd}/{}/{node}/{g}", target(s, p, cmd)?.0))),
        None => Ok(None),
    }
}

fn set_points(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.track.setPoints";
    let node = node_p(p, CMD)?;
    let raw = p.get("points").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `points` (a list of points)"))?;
    if raw.len() > comp::MAX_TRACK_POINTS {
        return Err(bad(CMD, format!("a Tracker holds at most {} points", comp::MAX_TRACK_POINTS)));
    }
    let mut list = Vec::with_capacity(raw.len());
    for (i, v) in raw.iter().enumerate() {
        let pt = if v.get("track").is_some() {
            // a point as comp.get returns it (its analysed path kept)
            serde_json::from_value::<TrackerPoint>(v.clone()).map_err(|e| bad(CMD, format!("point {i}: {e}")))?
        } else {
            let at = v.get("at").and_then(vec2_of).ok_or_else(|| bad(CMD, format!("point {i}: need `at` [x, y] (finite comp pixels)")))?;
            let mut pt = TrackerPoint::at(str_p(v, "name").unwrap_or_default().chars().take(comp::MAX_LABEL).collect::<String>(), at);
            for (key, slot) in [("pattern", &mut pt.pattern), ("search", &mut pt.search)] {
                if let Some(h) = v.get(key) {
                    *slot = half_of(h).ok_or_else(|| bad(CMD, format!("point {i}: `{key}` is a positive half-size [w, h] or a number")))?;
                }
            }
            pt
        };
        list.push(pt);
    }
    let merge = gesture_key(s, p, CMD, node)?;
    edit_comp(s, p, CMD, "Set Tracker Points", merge, |c| {
        let n = c.node_mut(node).ok_or_else(|| bad(CMD, format!("no node {node}")))?;
        let CompKind::Tracker { points, .. } = &mut n.kind else { return Err(bad(CMD, format!("node {node} is not a Tracker node"))) };
        *points = list;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_options(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.track.setOptions";
    let node = node_p(p, CMD)?;
    let op = match str_p(p, "operation") {
        Some(k) => Some(TrackOperation::from_key(k).ok_or_else(|| bad(CMD, format!("operation `{k}`: matchMove, stabilise or cornerPin")))?),
        None if p.get("operation").is_some() => return Err(bad(CMD, "`operation` is a name")),
        None => None,
    };
    let radius = match p.get("smoothRadius") {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|r| u32::try_from(r).ok())
                .filter(|r| *r <= comp::MAX_SMOOTH_RADIUS)
                .ok_or_else(|| bad(CMD, format!("`smoothRadius` is 0..={} frames", comp::MAX_SMOOTH_RADIUS)))?,
        ),
    };
    let reference = ticks_p(p, "reference", CMD)?;
    let method = match str_p(p, "method") {
        Some(m) => Some(TrackMethod::from_name(m).ok_or_else(|| bad(CMD, format!("unknown method `{m}`")))?),
        None if p.get("method").is_some() => return Err(bad(CMD, "`method` is a name")),
        None => None,
    };
    let merge = gesture_key(s, p, CMD, node)?;
    edit_comp(s, p, CMD, "Tracker Settings", merge, |c| {
        let n = c.node_mut(node).ok_or_else(|| bad(CMD, format!("no node {node}")))?;
        match &mut n.kind {
            CompKind::Tracker { operation, smooth_radius, reference: r, method: m, .. } => {
                *operation = op.unwrap_or(*operation);
                *smooth_radius = radius.unwrap_or(*smooth_radius);
                *r = reference.unwrap_or(*r);
                *m = method.unwrap_or(*m);
            }
            CompKind::PlanarTracker { operation, reference: r, .. } => {
                if radius.is_some() || method.is_some() {
                    return Err(bad(CMD, "a Planar Tracker has no smoothing radius or method (it tracks a whole plane)"));
                }
                *operation = op.unwrap_or(*operation);
                *r = reference.unwrap_or(*r);
            }
            _ => return Err(bad(CMD, format!("node {node} is not a Tracker or Planar Tracker node"))),
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_region(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.planar.setRegion";
    let node = node_p(p, CMD)?;
    let raw = p.get("region").ok_or_else(|| bad(CMD, "need `region` (a list of [x, y] points or a mask path)"))?;
    let region: MaskPath = match raw {
        Value::Array(pts) => {
            if pts.len() > comp::MAX_REGION_POINTS {
                return Err(bad(CMD, format!("a region has at most {} points", comp::MAX_REGION_POINTS)));
            }
            let vertices = pts
                .iter()
                .map(|q| vec2_of(q).map(MaskVertex::corner))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| bad(CMD, "every region point is a finite [x, y]"))?;
            MaskPath { vertices, closed: true }
        }
        v => serde_json::from_value(v.clone()).map_err(|e| bad(CMD, format!("not a mask path: {e}")))?,
    };
    if region.vertices.len() < 3 || region.vertices.len() > comp::MAX_REGION_POINTS || !region.vertices.iter().all(|v| finite(v.p)) {
        return Err(bad(CMD, format!("a region needs 3..={} finite points", comp::MAX_REGION_POINTS)));
    }
    let quad = match p.get("quad") {
        None => None,
        Some(Value::Array(q)) if q.len() == 4 => {
            let pts: Vec<Vec2> = q.iter().map(vec2_of).collect::<Option<_>>().ok_or_else(|| bad(CMD, "`quad` is four finite [x, y] points"))?;
            <[Vec2; 4]>::try_from(pts).ok()
        }
        Some(_) => return Err(bad(CMD, "`quad` is four [x, y] points, clockwise from the top left")),
    };
    let reference = ticks_p(p, "reference", CMD)?;
    let merge = gesture_key(s, p, CMD, node)?;
    edit_comp(s, p, CMD, "Set Planar Region", merge, |c| {
        let n = c.node_mut(node).ok_or_else(|| bad(CMD, format!("no node {node}")))?;
        let CompKind::PlanarTracker { region: r, quad: q, reference: rf, .. } = &mut n.kind else {
            return Err(bad(CMD, format!("node {node} is not a Planar Tracker node")));
        };
        *r = region;
        if let Some(quad) = quad {
            *q = quad;
        }
        *rf = reference.unwrap_or(*rf);
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Keyframes (and confidences) of `track` within `lo ..= hi` removed.
fn clear_span(track: &mut Param, conf: &mut Vec<(Tick, f32)>, lo: Tick, hi: Tick) {
    let all = track.keyframes.iter().all(|k| lo <= k.time && k.time <= hi);
    if all && !track.keyframes.is_empty() {
        // the point stays where it was first tracked
        if let Some(k) = track.keyframes.first() {
            track.value = k.value.clone();
        }
    }
    track.keyframes.retain(|k| k.time < lo || k.time > hi);
    conf.retain(|(t, _)| *t < lo || *t > hi);
}

fn clear(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.track.clear";
    let node = node_p(p, CMD)?;
    let lo = ticks_p(p, "from", CMD)?.unwrap_or(Tick(i64::MIN));
    let hi = ticks_p(p, "to", CMD)?.unwrap_or(Tick(i64::MAX));
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    edit_comp(s, p, CMD, "Clear Track", None, |c| {
        let n = c.node_mut(node).ok_or_else(|| bad(CMD, format!("no node {node}")))?;
        match &mut n.kind {
            CompKind::Tracker { points, .. } => {
                for pt in points {
                    clear_span(&mut pt.track, &mut pt.confidence, lo, hi);
                }
            }
            CompKind::PlanarTracker { corners, confidence, .. } => {
                let mut none = Vec::new();
                for c in corners.iter_mut() {
                    clear_span(c, &mut none, lo, hi);
                }
                confidence.retain(|(t, _)| *t < lo || *t > hi);
                if corners.iter().all(|c| c.keyframes.is_empty()) {
                    corners.clear();
                }
            }
            _ => return Err(bad(CMD, format!("node {node} is not a Tracker or Planar Tracker node"))),
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.track.cancel";
    let comp = if p.get("comp").is_some() || p.get("node").is_some() { Some(target(s, p, CMD)?) } else { None };
    let node = u64_p(p, "node");
    let mut stopped = 0u64;
    for pj in s.comp_track_jobs.iter().filter(|j| comp.is_none_or(|c| c == j.comp) && node.is_none_or(|n| n == j.node)) {
        if let Some(j) = s.jobs.iter().find(|j| j.id == pj.job) {
            j.progress.cancel.store(true, Ordering::Relaxed);
            stopped += 1;
        }
    }
    if stopped == 0 {
        return Err(bad(CMD, "no tracker analysis is running there"));
    }
    Ok(json!({"stopped": stopped}))
}

/// What one analysis tracks, in the trackers' frame pixels.
enum Work {
    Points(Vec<PointSpec>),
    Plane { region: Vec<Vec2>, quad: [Vec2; 4] },
}

/// The comp clip whose lower tracks Layer sources read: `clip`, else the first selected clip of
/// the comp, else its first clip in the active sequence (None: Layer sources are transparent).
pub(crate) fn context(s: &Session, p: &Value, comp: ItemId, cmd: &str) -> Result<Option<(ItemId, ClipId)>> {
    let (Some(seq_id), Some(q)) = (s.state.active_sequence, s.active_sequence()) else {
        return if p.get("clip").is_some() { Err(bad(cmd, "`clip` needs an active sequence holding the comp clip")) } else { Ok(None) };
    };
    let of_comp = |c: ClipId| q.find_item(c).is_some_and(|(_, it)| it.item == comp);
    if p.get("clip").is_some() {
        let c = u64_p(p, "clip").map(ClipId).filter(|c| of_comp(*c)).ok_or_else(|| bad(cmd, "`clip` must be a clip of this comp in the active sequence"))?;
        return Ok(Some((seq_id, c)));
    }
    let chosen =
        s.state.selection.iter().copied().find(|c| of_comp(*c)).or_else(|| q.video_tracks.iter().flat_map(|t| &t.items).find(|i| i.item == comp).map(|i| i.id));
    Ok(chosen.map(|c| (seq_id, c)))
}

fn analyse(s: &mut Session, p: &Value) -> Result<Value> {
    top_level_only(p, ANALYSE, "Tracker analysis")?;
    let comp_id = target(s, p, ANALYSE)?;
    let node = node_p(p, ANALYSE)?;
    let c = comp_of(&s.project, comp_id).ok_or_else(|| bad(ANALYSE, "the comp is gone"))?.clone();
    let kind = tracker_kind(&c, node, ANALYSE)?.clone();
    if s.comp_track_jobs.iter().any(|j| j.comp == comp_id && j.node == node) {
        return Err(bad(ANALYSE, "this node is already being analysed"));
    }
    // what is analysed: whatever feeds input 0
    let input = c
        .edges
        .iter()
        .find(|e| e.to.node == node && e.to.index == 0)
        .map(|e| e.from.node)
        .ok_or_else(|| bad(ANALYSE, "connect the picture to track to the node's first input"))?;
    let rate = c.rate;
    let reference = match &kind {
        CompKind::Tracker { reference, .. } | CompKind::PlanarTracker { reference, .. } => *reference,
        _ => Tick::ZERO,
    };
    let from = ticks_p(p, "from", ANALYSE)?.unwrap_or(reference);
    let backward = match str_p(p, "direction") {
        None if p.get("direction").is_none() => false,
        Some("forward") => false,
        Some("backward") => true,
        _ => return Err(bad(ANALYSE, "`direction` is forward or backward")),
    };
    let f0 = rate.frame_at(from);
    let last = rate.frame_at(c.duration).saturating_sub(1);
    let f1 = match ticks_p(p, "to", ANALYSE)? {
        Some(t) => rate.frame_at(t),
        None if backward => 0,
        None => last,
    };
    let steps = if backward { f0.saturating_sub(f1) } else { f1.saturating_sub(f0) };
    if steps <= 0 {
        return Err(bad(
            ANALYSE,
            if backward {
                "nothing to track: `to` must be before `from`"
            } else {
                "nothing to track: `to` must be after `from` (and the comp needs a duration)"
            },
        ));
    }
    let steps = steps.min(MAX_FRAMES);
    let frame_t = move |k: i64| rate.tick_of(if backward { f0.saturating_sub(k) } else { f0.saturating_add(k) });
    let (t_first, t_last) = (frame_t(0), frame_t(steps));
    let span = (t_first.min(t_last), t_first.max(t_last));
    // the trackers' frame size and the comp → frame pixel map
    let (cw, ch) = (f64::from(c.width.max(1)), f64::from(c.height.max(1)));
    let map = FrameMap::new(c.width, c.height);
    let size = map.size;
    let to_frame = move |p: Vec2| map.to_frame(p);
    let to_comp = move |p: Vec2| map.to_comp(p);
    let work = match &kind {
        CompKind::Tracker { points, .. } => {
            if points.is_empty() {
                return Err(bad(ANALYSE, "the Tracker has no points: add one first (comp.track.setPoints)"));
            }
            let specs = points
                .iter()
                .map(|pt| {
                    let at = pt.track.value_at(t_first).as_vec2().filter(|v| finite(*v)).unwrap_or(Vec2::new(f64::NAN, f64::NAN));
                    PointSpec { at: to_frame(at), pattern: map.scale(pt.pattern), search: map.scale(pt.search) }
                })
                .collect();
            Work::Points(specs)
        }
        CompKind::PlanarTracker { region, quad, corners, .. } => {
            let poly: Vec<Vec2> = region.vertices.iter().map(|v| v.p).collect();
            if poly.len() < 3 {
                return Err(bad(ANALYSE, "draw the region to track first (comp.planar.setRegion)"));
            }
            let q = reference_quad(quad, &poly, (cw, ch));
            // start from the analysed plane at `from` when there is one (the region was drawn at
            // the reference)
            let at_from: Option<Vec<Vec2>> = (corners.len() == 4 && corners.iter().all(|c| !c.keyframes.is_empty()))
                .then(|| corners.iter().map(|c| c.value_at(t_first).as_vec2().filter(|v| finite(*v))).collect())
                .flatten();
            let (poly, q) = match at_from.as_deref().and_then(|now| fit_homography(&q, now).map(|h| (h, now))) {
                Some((h, now)) => {
                    let moved: Option<Vec<Vec2>> = poly.iter().map(|p| h.apply(*p)).collect();
                    match (moved, <[Vec2; 4]>::try_from(now.to_vec())) {
                        (Some(m), Ok(now)) => (m, now),
                        _ => (poly, q),
                    }
                }
                None => (poly, q),
            };
            Work::Plane { region: poly.into_iter().map(to_frame).collect(), quad: q.map(to_frame) }
        }
        _ => return Err(bad(ANALYSE, "not a tracker node")),
    };
    let clip = context(s, p, comp_id, ANALYSE)?;
    let project = s.project.clone();
    let provider = s.media.provider(project.clone(), s.services.clone());
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).saturating_add(1);
    let label =
        format!("Analyse {} ({})", if matches!(work, Work::Points(_)) { "Tracker" } else { "Planar Tracker" }, if backward { "backward" } else { "forward" });
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    job.progress.total.store(steps as u64, Ordering::Relaxed);
    let found: Arc<Mutex<Found>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), found.clone());
    let run = move || {
        let started = web_time::Instant::now();
        let frame = |t: Tick| -> Option<Arc<Prepared>> {
            let img = filmcraft_render::render_comp_node_at(&project, comp_id, input, clip, t, size, &provider)?;
            Some(Arc::new(Prepared::new(Gray::from_rgba8(img.w, img.h, &img.to_rgba8()))))
        };
        let mut done = 0u64;
        let mut err: Option<String> = None;
        let progress = |done: u64, status: String| {
            prog.done.store(done, Ordering::Relaxed);
            *lock(&prog.status) = status;
        };
        match work {
            Work::Points(specs) => match frame(t_first) {
                None => err = Some("can't render the tracker's input".into()),
                Some(first) => {
                    let opts = PointOptions { lost_below: DEFAULT_LOST_BELOW, on_lost: OnLost::Stop };
                    let mut multi = MultiPointTracker::new(&first, &specs, opts);
                    drop(first);
                    let samples = multi.first_samples();
                    if samples.iter().all(|s| !s.valid()) {
                        err = Some("no point can be tracked: each needs detail (not a flat area) inside the picture".into());
                    } else {
                        let mut f = lock(&out);
                        f.points = samples.iter().map(|s| if s.valid() { vec![(t_first, to_comp(s.pos), s.confidence as f32)] } else { Vec::new() }).collect();
                    }
                    for step in 1..=steps {
                        if err.is_some() {
                            break;
                        }
                        if prog.cancel.load(Ordering::Relaxed) {
                            err = Some("stopped".into());
                            break;
                        }
                        let t = frame_t(step);
                        let Some(next) = frame(t) else {
                            err = Some(format!("can't render frame {step}"));
                            break;
                        };
                        let samples = multi.step(&next);
                        let mut alive = 0;
                        {
                            let mut f = lock(&out);
                            for (track, s) in f.points.iter_mut().zip(&samples) {
                                if s.valid() {
                                    track.push((t, to_comp(s.pos), s.confidence as f32));
                                    alive += 1;
                                }
                            }
                        }
                        if alive == 0 {
                            err = Some(format!("lost every point after {} frame(s)", step - 1));
                            break;
                        }
                        done += 1;
                        progress(done, format!("Frame {step} of {steps} · {alive} point(s)"));
                    }
                }
            },
            Work::Plane { region, quad } => match frame(t_first).and_then(|f| PlanarTracker::new_shared(f, &region, quad)) {
                None => err = Some("the region can't be tracked: it needs detail inside the picture".into()),
                Some(mut tracker) => {
                    let s0 = tracker.first_sample();
                    lock(&out).plane.push((t_first, s0.corners.map(to_comp), 1.0));
                    let mut lost = 0usize;
                    for step in 1..=steps {
                        if prog.cancel.load(Ordering::Relaxed) {
                            err = Some("stopped".into());
                            break;
                        }
                        let t = frame_t(step);
                        let Some(next) = frame(t) else {
                            err = Some(format!("can't render frame {step}"));
                            break;
                        };
                        let sm = tracker.step(&next);
                        if sm.lost {
                            lost += 1;
                            if lost >= PLANAR_MAX_LOST {
                                err = Some(format!("lost the plane at frame {}", step.saturating_sub(lost as i64) + 1));
                                break;
                            }
                        } else {
                            lost = 0;
                            lock(&out).plane.push((t, sm.corners.map(to_comp), sm.confidence as f32));
                        }
                        done += 1;
                        progress(done, format!("Frame {step} of {steps} · {} features", sm.inliers));
                    }
                }
            },
        }
        let secs = started.elapsed().as_secs_f64();
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
    let setup = setup_of(&c, node);
    s.jobs.push(job);
    s.comp_track_jobs.push(Pending { job: id, comp: comp_id, node, span, setup, found });
    if bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
    } else if let Err(e) = std::thread::Builder::new().name("filmcraft-comp-track".into()).spawn(run) {
        // no thread: forget the job, or the node would stay "being analysed" for good
        s.comp_track_jobs.retain(|j| j.job != id);
        s.jobs.retain(|j| j.id != id);
        return Err(EngineError::Other(e.to_string()));
    }
    let st = s.jobs.iter().find(|j| j.id == id).map(crate::Job::to_json).unwrap_or(Value::Null);
    Ok(json!({"job": id, "frames": steps, "status": st}))
}

/// `keys` written into `track` (and their confidences into `conf`), replacing what was in
/// `lo ..= hi`.
fn replace(track: &mut Param, conf: &mut Vec<(Tick, f32)>, (lo, hi): (Tick, Tick), keys: &[(Tick, Vec2, f32)]) {
    if keys.is_empty() {
        return;
    }
    if track.keyframes.is_empty()
        && let Some((_, v, _)) = keys.first()
    {
        track.value = ParamValue::Vec2(*v);
    }
    track.keyframes.retain(|k| k.time < lo || k.time > hi);
    track.keyframes.extend(keys.iter().map(|(t, v, _)| Keyframe::new(*t, ParamValue::Vec2(*v))));
    track.keyframes.sort_by_key(|k| k.time);
    track.keyframes.dedup_by_key(|k| k.time);
    conf.retain(|(t, _)| *t < lo || *t > hi);
    conf.extend(keys.iter().map(|(t, _, c)| (*t, if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 })));
    conf.sort_by_key(|(t, _)| *t);
}

fn write(c: &mut Comp, node: NodeId, span: (Tick, Tick), found: &Found) -> Result<()> {
    let (cw, ch) = (f64::from(c.width.max(1)), f64::from(c.height.max(1)));
    let n = c.node_mut(node).ok_or_else(|| bad(ANALYSE, "the tracker node is gone"))?;
    match &mut n.kind {
        CompKind::Tracker { points, .. } => {
            for (pt, keys) in points.iter_mut().zip(&found.points) {
                replace(&mut pt.track, &mut pt.confidence, span, keys);
            }
        }
        CompKind::PlanarTracker { region, quad, corners, confidence, .. } => {
            if found.plane.is_empty() {
                return Ok(());
            }
            if corners.len() != 4 {
                let poly: Vec<Vec2> = region.vertices.iter().map(|v| v.p).collect();
                *corners = reference_quad(quad, &poly, (cw, ch)).iter().map(|q| Param::new(ParamValue::Vec2(*q))).collect();
            }
            let mut none = Vec::new();
            for (i, corner) in corners.iter_mut().enumerate() {
                let keys: Vec<(Tick, Vec2, f32)> = found.plane.iter().filter_map(|(t, q, cf)| q.get(i).map(|p| (*t, *p, *cf))).collect();
                replace(corner, &mut none, span, &keys);
            }
            let keys: Vec<(Tick, Vec2, f32)> = found.plane.iter().map(|(t, _, cf)| (*t, Vec2::ZERO, *cf)).collect();
            let mut scratch = Param::new(ParamValue::Vec2(Vec2::ZERO));
            replace(&mut scratch, confidence, span, &keys);
        }
        _ => return Err(bad(ANALYSE, "the node is no longer a tracker")),
    }
    Ok(())
}

/// Writes the keyframes of finished analyses (one undo step each) and drops them. Called from
/// [`Session::poll_persistence`] and after synchronous runs.
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while let Some(pj) = s.comp_track_jobs.get(i) {
        let finished = s.jobs.iter().find(|j| j.id == pj.job).is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.comp_track_jobs.remove(i);
        let found = std::mem::take(&mut *lock(&pj.found));
        if found.points.iter().all(Vec::is_empty) && found.plane.is_empty() {
            continue;
        }
        let now = comp_of(&s.project, pj.comp).map(|c| setup_of(c, pj.node));
        if now.as_ref().is_some_and(|v| *v != pj.setup) {
            s.error_toast(ANALYSE, "the tracker was changed while it was being analysed: nothing was written; analyse it again".to_string());
            continue;
        }
        let p = json!({"comp": pj.comp.0});
        if let Err(e) = edit_comp(s, &p, ANALYSE, "Analyse Tracker", None, |c| write(c, pj.node, pj.span, &found)) {
            s.error_toast(ANALYSE, e.to_string());
        }
    }
}
