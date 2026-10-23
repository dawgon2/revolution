//! Track-assisted roto (fork C3.7): "draw once, it follows the object".
//!
//! Two ways a Roto node's shape follows what it outlines:
//!
//! - **Follow a tracker** (`comp.roto.linkTracker` / `comp.roto.unlinkTracker`): the shape's
//!   [`TrackLink`] names a Tracker or Planar Tracker node of the same comp. At render time the
//!   shape (as drawn, keyframes included, in the tracker's reference frame) is moved by that
//!   tracker's measured motion (`filmcraft_render::comp::track::linked_roto`), CPU and GPU alike
//!   (the GPU only consumes the CPU-rasterised coverage). Linking and unlinking re-express the
//!   stored outlines so nothing jumps on screen: each keyframe through the motion at its own
//!   time, an unkeyframed outline through the motion at `at` (the playhead in the UI). A link whose tracker was deleted (or is
//!   no longer a tracker) leaves the shape where it was drawn; `comp.explain` says so.
//! - **Track the shape with Magic Mask** (`comp.roto.trackShape`): a background job prompts a
//!   video segmenter (SAM 2.1 with its memory of earlier frames; MobileSAM + colour follow as the
//!   fallback; a factory installed on [`crate::magic_mask::MagicMasks::video_segmenter`] in tests)
//!   with the shape at `from` (its box and up to three interior points: SAM 2.1's tracker takes no
//!   mask prompt on a first frame), follows the subject frame by frame over the Roto node's input
//!   (the comp graph above it, rendered at comp time at most [`ANALYSIS_MAX_SIDE`] on the long
//!   side), turns each frame's matte into an outline with the previous frame's vertex count, start
//!   and orientation (`filmcraft_render::contour::matte_to_shape`) and writes every `every`-th
//!   frame (and the last) as a shape keyframe, per-point feathers copied from the start shape by
//!   vertex index. Everything lands in **one** undo step when the job ends (done, stopped or
//!   lost), and only if the shape still is what was tracked (`setup`); otherwise nothing is
//!   written and a toast says why.
//!
//! Coordinates: the node stores comp pixels (continuous, pixel centres at +0.5); the segmenter's
//! frames and the contour fit use analysis pixels with the same convention (`p · k`). Time is comp
//! time (`Tick`); frame indices exist only inside the job.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use filmcraft_geom::Vec2;
use filmcraft_mask::video::Direction;
use filmcraft_mask::{Point, Prompt, Rect, RgbImage};
use filmcraft_project::comp::{Comp, CompKind};
use filmcraft_project::dag::NodeId;
use filmcraft_project::roto::{MAX_KEYFRAMES, MAX_LINK_ID, Roto, RotoKind, RotoShape, TrackLink};
use filmcraft_project::{ItemId, Keyframe, MaskPath, MaskVertex, ParamValue};
use filmcraft_render::comp::track::{TrackWarp, path_through};
use filmcraft_render::contour;
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p, u64_p};
use crate::comp::{comp_at, comp_of, edit_comp, group_p, target};
use crate::comp_track::{lock, ticks_p};
use crate::magic_mask::MaskModel;
use crate::{EngineError, Result, Session};

/// Longest side of the frames the segmenter sees (comp pixels are scaled down to it).
pub const ANALYSIS_MAX_SIDE: f64 = 1024.0;
/// Most frames one track follows after its first.
pub const MAX_FRAMES: i64 = 100_000;
/// Largest keyframe spacing (frames).
pub const MAX_EVERY: u64 = 1_000;

const LINK: &str = "comp.roto.linkTracker";
const UNLINK: &str = "comp.roto.unlinkTracker";
const TRACK: &str = "comp.roto.trackShape";
const STOP: &str = "comp.roto.stopTrack";

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            LINK,
            "Follow Tracker",
            r#"{"comp":id?,"group":[id]?,"node":id (Roto),"shape":n,"tracker":id (a Tracker or Planar Tracker node of the comp),"output":"transform"|point name? (default transform: the node's whole solve; a Tracker point's name follows that point's position only),"at":ticks? (comp time the shape is seen at, e.g. the playhead: it stays where it is on screen there; keyframes stay where they are at their own times; default the tracker's reference frame)}"#,
            link,
        ),
        spec(
            UNLINK,
            "Stop Following Tracker",
            r#"{"comp":id?,"group":[id]?,"node":id,"shape":n,"at":ticks? (comp time the shape is seen at: an unkeyframed shape stays where it is on screen there; keyframes stay where they are at their own times; default the tracker's reference frame)}"#,
            unlink,
        ),
        spec(
            TRACK,
            "Track Roto Shape",
            r#"{"comp":id?,"group":[id]?,"node":id (Roto),"shape":n,"from":ticks? (comp time; default the shape's first keyframe, else 0),"to":ticks? (default the comp's last frame, or 0 backward),"direction":"forward"|"backward"?,"every":frames? (keyframe spacing, 1..=1000, default 1),"model":"auto"|"better"|"faster"|"mobileSam"?,"clip":id?,"wait":bool?}"#,
            track_shape,
        ),
        CommandSpec { journal: false, ..spec(STOP, "Stop Roto Shape Track", r#"{"comp":id?,"group":[id]?,"node":id?}"#, stop) },
    ]
}

/// A running shape track; its keyframes are written when it ends ([`poll`]).
pub struct Pending {
    pub job: u64,
    pub comp: ItemId,
    /// The group path the node is inside (LO.4; empty: the comp's own nodes).
    pub group: Vec<NodeId>,
    pub node: NodeId,
    pub shape: usize,
    /// The tracked span (comp time, low ..= high): shape keyframes in it are replaced.
    span: (Tick, Tick),
    /// What the track started from ([`setup_of`]); a different shape at the end is not written.
    setup: Value,
    found: Arc<Mutex<Vec<(Tick, MaskPath)>>>,
}

/// The parts of a shape (and its comp) a track result depends on: the comp size and the shape's
/// name, kind, link, vertex count and outline at the start time.
fn setup_of(c: &Comp, node: NodeId, shape: usize, at: Tick) -> Value {
    let sh = match c.node(node).map(|n| &n.kind) {
        Some(CompKind::Roto { roto, .. }) => roto.shapes.get(shape).map(|s| json!([s.name, s.kind, s.track_link, s.path_at(at)])),
        _ => None,
    };
    json!({"size": [c.width, c.height], "shape": sh})
}

fn node_p(p: &Value, cmd: &str) -> Result<NodeId> {
    u64_p(p, "node").ok_or_else(|| bad(cmd, "need `node` (a Roto node id)"))
}

fn shape_p(p: &Value, cmd: &str) -> Result<usize> {
    p.get("shape").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(cmd, "need `shape` (a shape's list index)"))
}

fn roto_in<'a>(c: &'a Comp, node: NodeId, cmd: &str) -> Result<&'a Roto> {
    match c.node(node).map(|n| &n.kind) {
        Some(CompKind::Roto { roto, .. }) => Ok(roto),
        Some(_) => Err(bad(cmd, format!("node {node} is not a Roto node"))),
        None => Err(bad(cmd, format!("no node {node}"))),
    }
}

fn shape_mut<'a>(c: &'a mut Comp, node: NodeId, i: usize, cmd: &str) -> Result<&'a mut RotoShape> {
    let n = c.node_mut(node).ok_or_else(|| bad(cmd, format!("no node {node}")))?;
    let CompKind::Roto { roto, .. } = &mut n.kind else { return Err(bad(cmd, format!("node {node} is not a Roto node"))) };
    roto.shapes.get_mut(i).ok_or_else(|| bad(cmd, format!("no shape {i}")))
}

fn link(s: &mut Session, p: &Value) -> Result<Value> {
    let node = node_p(p, LINK)?;
    let i = shape_p(p, LINK)?;
    let tracker = match p.get("tracker") {
        Some(Value::String(t)) => t.trim().parse::<NodeId>().ok(),
        Some(v) => v.as_u64(),
        None => None,
    }
    .ok_or_else(|| bad(LINK, "need `tracker` (a Tracker or Planar Tracker node id)"))?;
    let output = match p.get("output") {
        None | Some(Value::Null) => "transform".to_string(),
        Some(Value::String(o)) if o.chars().count() <= MAX_LINK_ID => o.clone(),
        Some(_) => return Err(bad(LINK, format!("`output` is \"transform\" or a point's name (at most {MAX_LINK_ID} characters)"))),
    };
    let c = comp_at(s, p, LINK)?.1;
    roto_in(c, node, LINK)?.shapes.get(i).ok_or_else(|| bad(LINK, format!("no shape {i}")))?;
    let whole = output.is_empty() || output.eq_ignore_ascii_case("transform");
    match c.node(tracker).map(|n| &n.kind) {
        None => return Err(bad(LINK, format!("no node {tracker}"))),
        Some(CompKind::Tracker { points, .. }) => {
            if !whole && !points.iter().any(|q| q.name == output) {
                return Err(bad(LINK, format!("the Tracker has no point named `{output}`")));
            }
        }
        Some(CompKind::PlanarTracker { .. }) => {
            if !whole {
                return Err(bad(LINK, "a Planar Tracker has one output: \"transform\" (the plane)"));
            }
        }
        Some(_) => return Err(bad(LINK, format!("node {tracker} is not a Tracker or Planar Tracker node"))),
    }
    let output = if whole { "transform".to_string() } else { output };
    let at = ticks_p(p, "at", LINK)?;
    let new_link = TrackLink { tracker: tracker.to_string(), output };
    // a shape following another tracker first comes back to the screen through that one
    let old = roto_in(c, node, LINK)?.shapes.get(i).and_then(|sh| sh.track_link.clone()).and_then(|l| Follow::of(c, &l));
    let new = Follow::of(c, &new_link);
    let moved = edit_comp(s, p, LINK, "Follow Tracker", None, |c| {
        let sh = shape_mut(c, node, i, LINK)?;
        let mut moved = 0;
        if let Some(f) = &old {
            moved += rebase(sh, at, |t| f.warp(t));
        }
        if let Some(f) = &new {
            moved += rebase(sh, at, |t| f.warp(t)?.inverse());
        }
        sh.track_link = Some(new_link);
        Ok(moved)
    })?;
    Ok(json!({"rebased": moved}))
}

fn unlink(s: &mut Session, p: &Value) -> Result<Value> {
    let node = node_p(p, UNLINK)?;
    let i = shape_p(p, UNLINK)?;
    let at = ticks_p(p, "at", UNLINK)?;
    let c = comp_at(s, p, UNLINK)?.1;
    let old = roto_in(c, node, UNLINK)?.shapes.get(i).and_then(|sh| sh.track_link.clone()).and_then(|l| Follow::of(c, &l));
    let moved = edit_comp(s, p, UNLINK, "Stop Following Tracker", None, |c| {
        let sh = shape_mut(c, node, i, UNLINK)?;
        if sh.track_link.is_none() {
            return Err(bad(UNLINK, format!("shape {i} follows no tracker")));
        }
        // bake the motion in so the shape stays where it is on screen
        let moved = old.as_ref().map_or(0, |f| rebase(sh, at, |t| f.warp(t)));
        sh.track_link = None;
        Ok(moved)
    })?;
    Ok(json!({"rebased": moved}))
}

/// The tracker a link follows, copied out of its comp (fork C3.7 link / unlink review fix).
struct Follow {
    kind: CompKind,
    output: String,
    rate: filmcraft_time::FrameRate,
    size: (f64, f64),
}

impl Follow {
    /// None when the link dangles (the shape is drawn as stored).
    fn of(c: &Comp, l: &TrackLink) -> Option<Self> {
        let kind = filmcraft_render::comp::track::link_target(&l.tracker, |id| c.node(id).map(|n| &n.kind))?.clone();
        Some(Self { kind, output: l.output.clone(), rate: c.rate, size: (f64::from(c.width.max(1)), f64::from(c.height.max(1))) })
    }
    /// The motion from the tracker's reference frame to `t` (what the renderer applies).
    fn warp(&self, t: Tick) -> Option<TrackWarp> {
        filmcraft_render::comp::track::follow_warp(&self.kind, self.rate, t, self.size, &self.output)
    }
}

/// LO.5: `path` as seen on screen at comp time `t`, in the coordinates a shape following
/// `link` stores (the tracker's reference frame): through the inverse of the tracker's motion at
/// `t`. None when the link dangles or the motion is unknown or singular there.
pub(crate) fn screen_to_stored(c: &Comp, link: &TrackLink, t: Tick, path: &MaskPath) -> Option<MaskPath> {
    let inverse = Follow::of(c, link)?.warp(t)?.inverse()?;
    let moved = path_through(path, inverse);
    Some(MaskPath { vertices: moved.vertices.iter().map(|v| v.cleaned()).collect(), closed: moved.closed })
}

/// Re-expresses a shape's outlines through `warp_at(t)` so that what is on screen does not jump
/// when the shape starts or stops following a tracker: each keyframe at its own time; the static
/// outline (no keyframes) at `at` only (None: left alone). Outlines whose map is unknown there
/// (not analysed, singular) stay. Returns how many outlines changed.
fn rebase(sh: &mut RotoShape, at: Option<Tick>, warp_at: impl Fn(Tick) -> Option<TrackWarp>) -> usize {
    let through = |path: &MaskPath, t: Tick| -> Option<MaskPath> {
        let moved = path_through(path, warp_at(t)?);
        let cleaned = MaskPath { vertices: moved.vertices.iter().map(|v| v.cleaned()).collect(), closed: moved.closed };
        (cleaned != *path).then_some(cleaned)
    };
    let mut n = 0usize;
    if sh.shape.keyframes.is_empty() {
        if let (Some(t), ParamValue::Path(path)) = (at, &sh.shape.value)
            && let Some(q) = through(path, t)
        {
            sh.shape.value = ParamValue::Path(q);
            n = 1;
        }
        return n;
    }
    for k in &mut sh.shape.keyframes {
        if let ParamValue::Path(path) = &k.value
            && let Some(q) = through(path, k.time)
        {
            k.value = ParamValue::Path(q);
            n = n.saturating_add(1);
        }
    }
    n
}

fn stop(s: &mut Session, p: &Value) -> Result<Value> {
    let comp = if p.get("comp").is_some() || p.get("node").is_some() { Some(target(s, p, STOP)?) } else { None };
    let node = u64_p(p, "node");
    // (a node is named with its group path, LO.4)
    let group = group_p(p, STOP)?;
    let same_group = |j: &Pending| (node.is_none() && p.get("group").is_none()) || j.group == group;
    let mut stopped = 0u64;
    for pj in s.roto_track_jobs.iter().filter(|j| comp.is_none_or(|c| c == j.comp) && node.is_none_or(|n| n == j.node) && same_group(j)) {
        if let Some(j) = s.jobs.iter().find(|j| j.id == pj.job) {
            j.progress.cancel.store(true, Ordering::Relaxed);
            stopped += 1;
        }
    }
    if stopped == 0 {
        return Err(bad(STOP, "no roto shape is being tracked there"));
    }
    Ok(json!({"stopped": stopped}))
}

/// Comp pixels ↔ analysis pixels (both continuous with pixel centres at +0.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AnalysisMap {
    pub size: (usize, usize),
    kx: f64,
    ky: f64,
}

impl AnalysisMap {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        let (cw, ch) = (f64::from(width.max(1)), f64::from(height.max(1)));
        let k = (ANALYSIS_MAX_SIDE / cw.max(ch)).min(1.0);
        let size = (((cw * k).round() as usize).max(1), ((ch * k).round() as usize).max(1));
        Self { size, kx: size.0 as f64 / cw, ky: size.1 as f64 / ch }
    }
    fn path(self, p: &MaskPath, kx: f64, ky: f64) -> MaskPath {
        let s = |v: Vec2| Vec2::new(v.x * kx, v.y * ky);
        MaskPath { vertices: p.vertices.iter().map(|v| MaskVertex { p: s(v.p), t_in: s(v.t_in), t_out: s(v.t_out), ..*v }).collect(), closed: p.closed }
    }
    /// A comp-pixel path in analysis pixels (feathers untouched: they are not fitted).
    pub(crate) fn to_analysis(self, p: &MaskPath) -> MaskPath {
        self.path(p, self.kx, self.ky)
    }
    /// An analysis-pixel path in comp pixels.
    pub(crate) fn to_comp(self, p: &MaskPath) -> MaskPath {
        self.path(p, 1.0 / self.kx, 1.0 / self.ky)
    }
}

/// The prompt for an outline's coverage (`w × h`, ≥ 0.5 inside): its box and up to three
/// interior positive points, each as far from the edge and from the others as the shape allows
/// (a chamfer distance transform). None when nothing is inside.
pub(crate) fn prompt_of(cov: &[f32], w: usize, h: usize) -> Option<Prompt> {
    let n = w.checked_mul(h)?;
    if cov.len() < n || n == 0 {
        return None;
    }
    let inside = |i: usize| cov.get(i).is_some_and(|v| *v >= 0.5);
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut any = false;
    for i in 0..n {
        if inside(i) {
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            any = true;
        }
    }
    if !any {
        return None;
    }
    // chamfer (3-4) distance to the nearest outside pixel (or the frame edge)
    let big = u32::MAX / 4;
    let mut d: Vec<u32> = (0..n).map(|i| if inside(i) { big } else { 0 }).collect();
    let at = |d: &[u32], x: isize, y: isize| -> u32 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize { 0 } else { d.get(y as usize * w + x as usize).copied().unwrap_or(0) }
    };
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            if d.get(i).copied().unwrap_or(0) == 0 {
                continue;
            }
            let m = [at(&d, x - 1, y) + 3, at(&d, x, y - 1) + 3, at(&d, x - 1, y - 1) + 4, at(&d, x + 1, y - 1) + 4].into_iter().min().unwrap_or(0);
            if let Some(v) = d.get_mut(i) {
                *v = (*v).min(m);
            }
        }
    }
    for y in (0..h as isize).rev() {
        for x in (0..w as isize).rev() {
            let i = y as usize * w + x as usize;
            if d.get(i).copied().unwrap_or(0) == 0 {
                continue;
            }
            let m = [at(&d, x + 1, y) + 3, at(&d, x, y + 1) + 3, at(&d, x + 1, y + 1) + 4, at(&d, x - 1, y + 1) + 4].into_iter().min().unwrap_or(0);
            if let Some(v) = d.get_mut(i) {
                *v = (*v).min(m);
            }
        }
    }
    let mut chosen: Vec<(usize, usize)> = Vec::new();
    for _ in 0..3 {
        let mut best: Option<(f64, usize)> = None;
        for (i, dv) in d.iter().enumerate() {
            if *dv == 0 {
                continue;
            }
            let (x, y) = ((i % w) as f64, (i / w) as f64);
            let edge = f64::from(*dv) / 3.0;
            let apart = chosen.iter().map(|(cx, cy)| (x - *cx as f64).hypot(y - *cy as f64)).fold(f64::INFINITY, f64::min);
            let score = edge.min(apart);
            if best.is_none_or(|(b, _)| score > b) {
                best = Some((score, i));
            }
        }
        // a later point too close to the edge or the others adds nothing
        match best {
            Some((score, i)) if chosen.is_empty() || score >= 2.0 => chosen.push((i % w, i / w)),
            _ => break,
        }
    }
    let (fw, fh) = (w as f32, h as f32);
    let points = chosen.iter().map(|(x, y)| Point { x: (*x as f32 + 0.5) / fw, y: (*y as f32 + 0.5) / fh, positive: true }).collect();
    let rect = Rect { x0: x0 as f32 / fw, y0: y0 as f32 / fh, x1: (x1 as f32 + 1.0) / fw, y1: (y1 as f32 + 1.0) / fh };
    Some(Prompt { points, rect: Some(rect), prior: None, prefer_whole: false })
}

/// An analysis-size coverage of `path` alone (no feather, expansion or opacity).
fn coverage(path: &MaskPath, map: AnalysisMap) -> Vec<f32> {
    let roto = Roto { shapes: vec![RotoShape::new("", RotoKind::Bezier, path.clone())] };
    let prepared = filmcraft_render::roto::prepare(&roto, Tick::ZERO, map.size.0, map.size.1, map.kx as f32);
    filmcraft_render::roto::rasterize(&prepared, map.size.0, map.size.1)
}

/// `fitted` (comp pixels) with each vertex's feathers taken from `start` by index.
fn with_feathers(mut fitted: MaskPath, start: &MaskPath) -> MaskPath {
    for (v, s) in fitted.vertices.iter_mut().zip(&start.vertices) {
        v.feather_in = s.feather_in;
        v.feather_out = s.feather_out;
    }
    fitted
}

fn track_shape(s: &mut Session, p: &Value) -> Result<Value> {
    // LO.4: inside a group (`group`), the node's input as it is evaluated in place
    let group = group_p(p, TRACK)?;
    let (comp_id, c) = comp_at(s, p, TRACK)?;
    let c = c.clone();
    let node = node_p(p, TRACK)?;
    let i = shape_p(p, TRACK)?;
    let shape = roto_in(&c, node, TRACK)?.shapes.get(i).cloned().ok_or_else(|| bad(TRACK, format!("no shape {i}")))?;
    if shape.track_link.is_some() {
        return Err(bad(TRACK, "the shape follows a tracker: stop following it first (comp.roto.unlinkTracker), or track the tracker instead"));
    }
    if shape.kind != RotoKind::Bezier {
        return Err(bad(TRACK, "only Bézier shapes can be tracked (a B-spline's points are not on its outline)"));
    }
    if s.roto_track_jobs.iter().any(|j| j.comp == comp_id && j.group == group && j.node == node) {
        return Err(bad(TRACK, "a shape of this node is already being tracked"));
    }
    let input = c
        .edges
        .iter()
        .find(|e| e.to.node == node && e.to.index == 0)
        .map(|e| e.from.node)
        .ok_or_else(|| bad(TRACK, "connect the picture to track to the Roto node's input"))?;
    let rate = c.rate;
    let from = ticks_p(p, "from", TRACK)?.unwrap_or_else(|| shape.shape.keyframes.first().map_or(Tick::ZERO, |k| k.time));
    let backward = match str_p(p, "direction") {
        None if p.get("direction").is_none() => false,
        Some("forward") => false,
        Some("backward") => true,
        _ => return Err(bad(TRACK, "`direction` is forward or backward")),
    };
    let every = match p.get("every") {
        None | Some(Value::Null) => 1,
        Some(v) => {
            v.as_u64().filter(|e| (1..=MAX_EVERY).contains(e)).ok_or_else(|| bad(TRACK, format!("`every` is a whole number of frames, 1..={MAX_EVERY}")))?
        }
    };
    let model = match p.get("model") {
        None | Some(Value::Null) => s.magic_mask.model,
        Some(v) => v.as_str().and_then(MaskModel::parse).ok_or_else(|| bad(TRACK, "`model` is auto, better, faster or mobileSam"))?,
    };
    let f0 = rate.frame_at(from);
    let last = rate.frame_at(c.duration).saturating_sub(1);
    let f1 = match ticks_p(p, "to", TRACK)? {
        Some(t) => rate.frame_at(t),
        None if backward => 0,
        None => last,
    };
    let steps = if backward { f0.saturating_sub(f1) } else { f1.saturating_sub(f0) };
    if steps <= 0 {
        return Err(bad(
            TRACK,
            if backward {
                "nothing to track: `to` must be before `from`"
            } else {
                "nothing to track: `to` must be after `from` (and the comp needs a duration)"
            },
        ));
    }
    // every keyframe of the track fits the shape's keyframe cap
    let max_keys = i64::try_from(MAX_KEYFRAMES).unwrap_or(i64::MAX).saturating_sub(2);
    let steps = steps.min(MAX_FRAMES).min(max_keys.saturating_mul(i64::try_from(every).unwrap_or(1)));
    let frame_t = move |k: i64| rate.tick_of(if backward { f0.saturating_sub(k) } else { f0.saturating_add(k) });
    let (t_first, t_last) = (frame_t(0), frame_t(steps));
    let span = (t_first.min(t_last), t_first.max(t_last));
    let start = shape.path_at(t_first);
    if !start.closed || start.vertices.len() < 3 || start.vertices.len() > contour::MAX_VERTICES {
        return Err(bad(TRACK, format!("the shape needs a closed outline of 3..={} points at the start time", contour::MAX_VERTICES)));
    }
    let map = AnalysisMap::new(c.width, c.height);
    let (aw, ah) = map.size;
    let start_a = map.to_analysis(&start);
    let cov = coverage(&start_a, map);
    let prompt = prompt_of(&cov, aw, ah).ok_or_else(|| bad(TRACK, "the shape covers nothing at the start time (draw it over the subject)"))?;
    // the model is chosen here (a missing model or a build without it is an error now); it loads
    // on the job's thread
    let choice = s.magic_mask.video_choice(model, TRACK)?;
    let clip = crate::comp_track::context(s, p, comp_id, TRACK)?;
    let project = s.project.clone();
    let provider = s.media.provider(project.clone(), s.services.clone());
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).saturating_add(1);
    let label = format!("Track roto shape \"{}\" ({})", shape.name.chars().take(32).collect::<String>(), if backward { "backward" } else { "forward" });
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    job.progress.total.store(u64::try_from(steps).unwrap_or(0), Ordering::Relaxed);
    let found: Arc<Mutex<Vec<(Tick, MaskPath)>>> = Arc::default();
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), found.clone());
    let frames = usize::try_from(steps).unwrap_or(0).saturating_add(1);
    // SAM 2.1's frame numbers run in time order: backward tracks count down from the last
    let index = move |k: i64| usize::try_from(if backward { steps.saturating_sub(k) } else { k }).unwrap_or(0);
    let direction = if backward { Direction::Backward } else { Direction::Forward };
    let path = group.clone();
    let run = move || {
        let started = web_time::Instant::now();
        let picture = |t: Tick| -> Option<RgbImage> {
            let img = filmcraft_render::render_comp_group_node_at(&project, comp_id, &path, input, clip, t, (aw, ah), &provider)?;
            if (img.w, img.h) != (aw, ah) {
                return None;
            }
            let rgba = img.over_black_rgba8();
            let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|q| [q[0], q[1], q[2]]).collect();
            RgbImage::new(aw, ah, rgb).ok()
        };
        let progress = |done: u64, status: String| {
            prog.done.store(done, Ordering::Relaxed);
            *lock(&prog.status) = status;
        };
        let mut done = 0u64;
        let mut err: Option<String> = None;
        let mut used = String::new();
        'track: {
            let (mut seg, name) = match choice.open(frames) {
                Ok(v) => v,
                Err(e) => {
                    err = Some(e);
                    break 'track;
                }
            };
            used = name;
            progress(0, format!("Tracking with {used}"));
            let Some(first) = picture(t_first) else {
                err = Some("can't render the Roto node's input".into());
                break 'track;
            };
            let seed = match seg.add_prompt(index(0), &first, &prompt) {
                Ok(m) => m,
                Err(e) => {
                    if matches!(e, filmcraft_mask::MaskError::Failed(_)) {
                        choice.gpu_failed();
                    }
                    err = Some(e.to_string());
                    break 'track;
                }
            };
            let matte = seed.segment.matte(aw, ah).ok();
            if !seed.visible || matte.as_ref().is_none_or(|m| m.bounds().is_none()) {
                err = Some("the model found nothing inside the shape at the start: draw it over the subject".into());
                break 'track;
            }
            lock(&out).push((t_first, start.clone()));
            let mut prev = start_a.clone();
            let mut pending: Option<(Tick, MaskPath)> = None;
            for step in 1..=steps {
                if prog.cancel.load(Ordering::Relaxed) {
                    err = Some("stopped".into());
                    break;
                }
                let t = frame_t(step);
                let secs = t.seconds();
                let Some(img) = picture(t) else {
                    err = Some(format!("can't render the input at {secs:.2} s"));
                    break;
                };
                let vm = match seg.propagate(index(step), &img, direction) {
                    Ok(v) => v,
                    Err(e) => {
                        // a lost GPU: the next track loads SAM 2.1 again
                        if matches!(e, filmcraft_mask::MaskError::Failed(_)) {
                            choice.gpu_failed();
                        }
                        err = Some(e.to_string());
                        break;
                    }
                };
                let fitted = vm
                    .visible
                    .then(|| vm.segment.matte(aw, ah).ok())
                    .flatten()
                    .map(|m| m.alpha.iter().map(|a| f32::from(*a) / 255.0).collect::<Vec<f32>>())
                    .and_then(|m| contour::matte_to_shape(&m, aw, ah, Some(&prev), 0.5))
                    .filter(|q| q.vertices.len() == prev.vertices.len());
                let Some(fitted) = fitted else {
                    err = Some(format!("lost the subject at {secs:.2} s: track again from the last good frame, or fix the shape there"));
                    break;
                };
                let comp_path = with_feathers(map.to_comp(&fitted), &start);
                prev = fitted;
                let key = u64::try_from(step).unwrap_or(0) % every == 0 || step == steps;
                if key {
                    lock(&out).push((t, comp_path));
                    pending = None;
                } else {
                    pending = Some((t, comp_path));
                }
                done += 1;
                progress(done, format!("Frame {step} of {steps} · {used}"));
            }
            // a track that ended early keeps its last good frame as a keyframe
            if let Some(k) = pending {
                lock(&out).push(k);
            }
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
        let with = if used.is_empty() { String::new() } else { format!(" with {used}") };
        *lock(&prog.status) = match &err {
            Some(e) => format!("{e} ({done} frame(s) tracked{with})"),
            None => format!("Tracked {done} frame(s) in {secs:.1}s{with}"),
        };
        prog.finished.store(true, Ordering::Relaxed);
        *lock(&res) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    let setup = setup_of(&c, node, i, t_first);
    s.jobs.push(job);
    s.roto_track_jobs.push(Pending { job: id, comp: comp_id, group, node, shape: i, span, setup, found });
    if bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
    } else if let Err(e) = std::thread::Builder::new().name("filmcraft-roto-track".into()).spawn(run) {
        s.roto_track_jobs.retain(|j| j.job != id);
        s.jobs.retain(|j| j.id != id);
        return Err(EngineError::Other(e.to_string()));
    }
    let st = s.jobs.iter().find(|j| j.id == id).map(crate::Job::to_json).unwrap_or(Value::Null);
    Ok(json!({"job": id, "frames": steps, "status": st}))
}

/// `keys` written as shape keyframes of shape `i`, replacing those in `lo ..= hi`.
fn write(c: &mut Comp, node: NodeId, i: usize, (lo, hi): (Tick, Tick), keys: &[(Tick, MaskPath)]) -> Result<()> {
    let sh = shape_mut(c, node, i, TRACK)?;
    let kept = sh.shape.keyframes.iter().filter(|k| k.time < lo || k.time > hi).count();
    if kept.saturating_add(keys.len()) > MAX_KEYFRAMES {
        return Err(bad(TRACK, format!("the shape would have more than {MAX_KEYFRAMES} keyframes: track a shorter span or set a larger keyframe spacing")));
    }
    if sh.shape.keyframes.is_empty()
        && let Some((_, first)) = keys.first()
    {
        sh.shape.value = ParamValue::Path(first.clone());
    }
    sh.shape.keyframes.retain(|k| k.time < lo || k.time > hi);
    sh.shape.keyframes.extend(keys.iter().map(|(t, path)| Keyframe::new(*t, ParamValue::Path(path.clone()))));
    sh.shape.keyframes.sort_by_key(|k| k.time);
    sh.shape.keyframes.dedup_by_key(|k| k.time);
    Ok(())
}

/// Writes the keyframes of finished shape tracks (one undo step each) and drops them. Called from
/// [`Session::poll_persistence`] and after synchronous runs.
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while let Some(pj) = s.roto_track_jobs.get(i) {
        let finished = s.jobs.iter().find(|j| j.id == pj.job).is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.roto_track_jobs.remove(i);
        let found = std::mem::take(&mut *lock(&pj.found));
        // (the start frame alone changes nothing)
        if found.len() < 2 {
            continue;
        }
        let at = found.first().map_or(Tick::ZERO, |(t, _)| *t);
        // (LO.4: inside its group; a comp or group deleted meanwhile: nothing is written)
        let now = comp_of(&s.project, pj.comp)
            .and_then(|c| if pj.group.is_empty() { Some(c) } else { filmcraft_project::comp::group::inner(c, &pj.group).ok() })
            .map(|c| setup_of(c, pj.node, pj.shape, at));
        if now.as_ref().is_none_or(|v| *v != pj.setup) {
            s.error_toast(TRACK, "the roto shape was changed (or deleted) while it was being tracked: nothing was written; track it again".to_string());
            continue;
        }
        let mut p = json!({"comp": pj.comp.0});
        if !pj.group.is_empty() {
            p["group"] = json!(pj.group);
        }
        if let Err(e) = edit_comp(s, &p, TRACK, "Track Roto Shape", None, |c| write(c, pj.node, pj.shape, pj.span, &found)) {
            s.error_toast(TRACK, e.to_string());
        }
    }
}
