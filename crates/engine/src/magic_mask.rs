//! Magic Mask in the engine (fork GRADE-PLAN G3.2): the `grade.magicMask.*` commands, the
//! propagation job and the matte cache.
//!
//! **Strokes are the document, mattes are a cache.** A node's [`MagicMask`] holds only the
//! strokes (pinned to the clip time they were drawn on, the time grade keyframes use) and the
//! refine settings. `grade.magicMask.track` queues a [`TrackJob`] on the background worker that
//! segments the clip's frames with a [`Segmenter`] and puts the mattes in the [`MatteCache`].
//!
//! **Who owns a frame.** Every frame belongs to the stroke time nearest to it (ties: the earlier
//! one). The job segments each stroke frame from its strokes (a frame's own strokes always win),
//! then follows the subject frame by frame forward and back with [`filmcraft_mask::track::follow`]
//! (a colour model of the subject and the rest of the frame kept per direction, interior positive
//! points, leaks into differently coloured neighbours carved off) across the frames that stroke
//! owns, i.e. up to half way to the neighbouring stroke frames. A cached matte
//! is keyed by its owner's strokes and the clip's picture ([`owner_hash`], [`picture_fingerprint`]),
//! so a new stroke only changes (and the next track only re-runs) the frames between its
//! neighbours, and a Motion / speed / media change hides the mattes until the clip is tracked
//! again. Stale frames are not deleted eagerly: an undo that brings old strokes back finds their
//! mattes again; the byte cap (least recently used first out) clears them. When the subject is
//! lost (an empty matte), the last good matte is held for at most [`MAX_HELD_FRAMES`] more frames
//! of that direction (sharing one matte, charged a few bytes each) and the job says where in
//! [`TrackStatus::notes`]. Editing the strokes (or removing the mask) stops a running track of
//! that node ([`TrackState::Superseded`]): each (clip, node) has an edit generation the job checks
//! before each model run and each insert.
//!
//! **What is segmented.** The clip's own picture (its input transform and intrinsic motion, no
//! grade and no other effects) at a preview size of at most [`MAX_PREVIEW_SIDE`] on the long side,
//! so editing earlier nodes never invalidates a matte, and the matte lines up with the picture
//! in the sequence frame.
//!
//! **Refine at use.** The cache keeps the model's matte; [`MatteCache::matte`] applies the
//! node's refine settings when asked (and keeps the last refined result per frame), so moving a
//! Refine slider never runs the model again. The renderer gets the mattes through the session's
//! registered [`filmcraft_render::matte::MatteSource`] ([`MagicMasks::token`], set on the
//! `RenderOptions` of the Program monitor, scopes, stills, the render cache and exports; frames
//! not tracked yet render without the limit). [`magic_matte`] reads one for the active sequence.
//!
//! **The model.** Tests and hosts may install a segmenter ([`MagicMasks::segmenter`]); otherwise
//! [`MagicMasks::model`] (or the track's `model` parameter) picks it (feature `magic-mask`):
//! **Better** = SAM 2.1 Hiera Large, **Faster** = SAM 2.1 Hiera Base+ (`<data dir>/models/sam2/`,
//! on the GPU, with a memory of earlier frames: [`crate::magic_mask_sam2`], fork G3.4), or
//! MobileSAM (`<data dir>/models/mobilesam/mobile_sam.pt`, frame by frame with the colour-guided
//! follow). `Auto` (the default) is Better on a discrete GPU when SAM 2.1 Large is installed, else
//! MobileSAM (owner question Q66). SAM 2.1 failing to load falls back to MobileSAM with a note. Models load on the worker thread. A missing model is
//! a command error in plain words.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use filmcraft_mask::refine::{Refine, refine};
use filmcraft_mask::sam2::Sam2Size;
use filmcraft_mask::track::{FollowOptions, Track, follow};
use filmcraft_mask::{LOW_RES, MAX_POINTS, MaskError, Matte, Point, Prompt, Rect, RgbImage, Segment, Segmenter};
use filmcraft_project::magic_mask::{MAX_STROKE_POINTS, MAX_STROKES, MagicMask, MagicMaskMode, MaskRefine, MaskStroke, StrokePoint};
use filmcraft_project::{ClipId, ItemId, ItemKind, Project, TrackItem};
use filmcraft_render::matte::{BitmapMatte, MatteSource, MatteToken, Registration};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::background::{BackgroundJob, JobId, JobKind, JobSpec, JobStep};
use crate::commands::{CommandSpec, always, bad, has_seq, str_p, time_p};
use crate::grade::{current_for, edit_grade_merged, is_video, media_time, node_arg, pick};
use crate::{EngineError, Result, Session};

/// Default byte cap of the matte cache.
pub const DEFAULT_CACHE_BYTES: usize = 512 << 20;
/// Long side of the picture the model sees (and of the cached mattes).
pub const MAX_PREVIEW_SIDE: usize = 1024;
/// Most frames one clip may be tracked over.
pub const MAX_TRACK_FRAMES: i64 = 100_000;
/// Track statuses kept for `grade.magicMask.status`.
const MAX_STATUSES: usize = 16;
/// Notes kept per track.
const MAX_NOTES: usize = 32;
/// Frames that hold the last good matte after the subject was lost, per direction of a run.
pub const MAX_HELD_FRAMES: usize = 120;
/// Bytes a held frame is charged (it shares the last good frame's matte).
const HELD_FRAME_BYTES: usize = 64;

// ---------------------------------------------------------------------------------------------
// Owners
// ---------------------------------------------------------------------------------------------

/// The distinct stroke times of `mask`, sorted.
pub fn stroke_times(mask: &MagicMask) -> Vec<Tick> {
    let mut v: Vec<Tick> = mask.strokes.iter().map(|s| s.time).collect();
    v.sort();
    v.dedup();
    v
}

/// The stroke time that owns clip time `t`: the nearest of `times` (sorted), ties to the earlier.
pub fn owner(times: &[Tick], t: Tick) -> Option<Tick> {
    let i = times.partition_point(|x| *x <= t);
    let before = i.checked_sub(1).and_then(|j| times.get(j)).copied();
    let after = times.get(i).copied();
    match (before, after) {
        (Some(b), Some(a)) => Some(if a.0.saturating_sub(t.0) < t.0.saturating_sub(b.0) { a } else { b }),
        (Some(b), None) => Some(b),
        (None, a) => a,
    }
}

/// The intrinsic effects that move the clip's picture (all but Opacity).
fn moving_effects(item: &TrackItem) -> impl Iterator<Item = &filmcraft_project::EffectInstance> {
    item.effects.iter().filter(|e| e.def().is_some_and(|d| d.intrinsic) && e.effect != "opacity")
}

fn effects_hash(item: &TrackItem) -> u64 {
    let mut h = DefaultHasher::new();
    for e in moving_effects(item) {
        serde_json::to_string(e).unwrap_or_default().hash(&mut h);
    }
    h.finish()
}

/// [`picture_fingerprint`] with the hash of the moving effects given.
fn fingerprint_with(project: &Project, item: &TrackItem, effects: u64) -> u64 {
    let mut h = DefaultHasher::new();
    item.item.0.hash(&mut h);
    match project.item(item.item).map(|p| &p.kind) {
        Some(ItemKind::Media(m)) => {
            serde_json::to_string(&m.media).unwrap_or_default().hash(&mut h);
            serde_json::to_string(&m.interpret).unwrap_or_default().hash(&mut h);
        }
        Some(k @ ItemKind::Subclip { .. }) => serde_json::to_string(k).unwrap_or_default().hash(&mut h),
        Some(ItemKind::Sequence(seq)) => nested_picture_hash(project, seq, 0, &mut h),
        _ => {}
    }
    item.speed.to_bits().hash(&mut h);
    (item.reverse, item.hold_filters, item.scale_to_frame, item.frame_hold.map(|t| t.0)).hash(&mut h);
    serde_json::to_string(&(&item.time_interpolation, &item.field_options)).unwrap_or_default().hash(&mut h);
    effects.hash(&mut h);
    h.finish()
}

/// How deep [`nested_picture_hash`] follows sequences nested in nested sequences.
const MAX_NEST_DEPTH: usize = 8;

/// A nested sequence's picture (LO.6): its settings and video tracks (items, their effects and
/// grades), and the media or sequences those items show, [`MAX_NEST_DEPTH`] levels down.
fn nested_picture_hash(project: &Project, seq: &filmcraft_project::Sequence, depth: usize, h: &mut DefaultHasher) {
    serde_json::to_string(&(&seq.settings, &seq.video_tracks)).unwrap_or_default().hash(h);
    if depth >= MAX_NEST_DEPTH {
        return;
    }
    for it in seq.video_tracks.iter().flat_map(|t| &t.items) {
        match project.item(it.item).map(|p| &p.kind) {
            Some(ItemKind::Media(m)) => {
                serde_json::to_string(&m.media).unwrap_or_default().hash(h);
                serde_json::to_string(&m.interpret).unwrap_or_default().hash(h);
            }
            Some(k @ ItemKind::Subclip { .. }) => serde_json::to_string(k).unwrap_or_default().hash(h),
            Some(ItemKind::Sequence(inner)) => nested_picture_hash(project, inner, depth + 1, h),
            _ => {}
        }
    }
}

/// What the clip's own picture at a clip time depends on, as far as a matte is concerned: the
/// media (and its interpretation), the intrinsic effects that move it (Motion, crop, time
/// remapping…; not Opacity), speed, reverse, holds and frame scaling. A change hides the mattes
/// until the clip is tracked again. A nested sequence counts with what it shows (edits inside it
/// invalidate, LO.6).
pub fn picture_fingerprint(project: &Project, item: &TrackItem) -> u64 {
    fingerprint_with(project, item, effects_hash(item))
}

/// What a matte owned by stroke time `at` depends on: that time, the mode, its strokes and the
/// clip's picture ([`picture_fingerprint`]).
pub fn owner_hash(mask: &MagicMask, at: Tick, picture: u64) -> u64 {
    let mut h = DefaultHasher::new();
    at.0.hash(&mut h);
    picture.hash(&mut h);
    matches!(mask.mode, MagicMaskMode::Person).hash(&mut h);
    for s in mask.strokes_at(at) {
        s.points.len().hash(&mut h);
        for p in &s.points {
            (p.x.to_bits(), p.y.to_bits(), p.positive).hash(&mut h);
        }
        s.rect.map(|r| r.map(f32::to_bits)).hash(&mut h);
    }
    h.finish()
}

/// The prompt of the strokes drawn at `at`: their points (the newest [`MAX_POINTS`]) and the
/// newest box; Person mode asks for the whole figure when the strokes are points only
/// ([`Prompt::prefer_whole`]).
fn stroke_prompt(mask: &MagicMask, at: Tick) -> Prompt {
    let mut points: Vec<Point> = Vec::new();
    let mut rect = None;
    for s in mask.strokes_at(at) {
        points.extend(s.points.iter().map(|p| Point { x: p.x, y: p.y, positive: p.positive }));
        if let Some(r) = s.rect {
            rect = Some(Rect { x0: r[0], y0: r[1], x1: r[2], y1: r[3] });
        }
    }
    let extra = points.len().saturating_sub(MAX_POINTS);
    points.drain(..extra);
    Prompt { points, rect, prior: None, prefer_whole: matches!(mask.mode, MagicMaskMode::Person) }
}

/// A segment in the model's grid rebuilt from a cached matte (the prior for continuing a run
/// from a cached frame).
fn segment_from(m: &Matte, cover: (f32, f32)) -> Segment {
    let mut s = Segment::empty(cover);
    let (cw, ch) = (s.cover.0 * LOW_RES as f32, s.cover.1 * LOW_RES as f32);
    if m.width == 0 || m.height == 0 || cw <= 0.0 || ch <= 0.0 {
        return s;
    }
    for gy in 0..LOW_RES {
        let v = (gy as f32 + 0.5) / ch;
        if v >= 1.0 {
            break;
        }
        let y = ((v * m.height as f32) as usize).min(m.height - 1);
        for gx in 0..LOW_RES {
            let u = (gx as f32 + 0.5) / cw;
            if u >= 1.0 {
                break;
            }
            let x = ((u * m.width as f32) as usize).min(m.width - 1);
            let inside = m.alpha.get(y * m.width + x).is_some_and(|a| *a >= 128);
            if let Some(l) = s.logits.get_mut(gy * LOW_RES + gx) {
                *l = if inside { 10.0 } else { -10.0 };
            }
        }
    }
    s.score = 1.0;
    s
}

fn to_refine(r: MaskRefine) -> Refine {
    let r = r.cleaned();
    Refine { grow: r.grow, soften: r.soften, clean_black: r.clean_black, clean_white: r.clean_white, invert: r.invert }
}

// ---------------------------------------------------------------------------------------------
// Matte cache
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Frame {
    owner_hash: u64,
    matte: Arc<Matte>,
    /// The model grid's cover of the frame (to rebuild a prior).
    cover: (f32, f32),
    refined: Option<(MaskRefine, Arc<Matte>)>,
    /// The matte handed to the renderer last (refined or not) and its conversion.
    bitmap: Option<(Arc<Matte>, Arc<BitmapMatte>)>,
    used: u64,
    /// Holds the last good matte of a lost subject (shared with that frame: not charged again).
    held: bool,
}

impl Frame {
    fn bytes(&self) -> usize {
        let own = if self.held { HELD_FRAME_BYTES } else { 64usize.saturating_add(self.matte.alpha.len()) };
        own.saturating_add(self.refined.as_ref().map_or(0, |(_, m)| m.alpha.len())).saturating_add(self.bitmap.as_ref().map_or(0, |(_, b)| b.alpha.len()))
    }
}

#[derive(Clone, Default)]
struct NodeFrames {
    frames: BTreeMap<Tick, Frame>,
    /// Clip time between two tracked frames (a lookup matches within half of it).
    step: i64,
}

/// Edit generations and matte epochs come from one process-wide counter, so a forgotten entry
/// never matches an old value again.
static NEXT_STAMP: AtomicU64 = AtomicU64::new(1);

fn next_stamp() -> u64 {
    NEXT_STAMP.fetch_add(1, Ordering::Relaxed).max(1)
}

#[derive(Default)]
struct Inner {
    nodes: HashMap<(u64, u64), NodeFrames>,
    bytes: usize,
    clock: u64,
    /// Edit generation per (clip, node): a running track stops when it moves.
    gens: HashMap<(u64, u64), u64>,
    /// Matte epoch per (clip, node): moves when its mattes change as a set (a track ended,
    /// frames evicted or forgotten). Salts the render-cache hashes of segments showing the node.
    epochs: HashMap<(u64, u64), u64>,
    /// Moves with any epoch.
    epoch: u64,
    /// Moves when a track ends or a node's mattes are dropped as a set, not when frames are
    /// evicted ([`MatteCache::shown_epoch`]).
    shown: u64,
}

impl Inner {
    fn touch(&mut self, key: (u64, u64)) {
        self.epochs.insert(key, next_stamp());
        self.epoch = self.epoch.wrapping_add(1);
    }

    fn remove_node(&mut self, key: (u64, u64)) {
        if let Some(n) = self.nodes.remove(&key) {
            let b: usize = n.frames.values().map(Frame::bytes).sum();
            self.bytes = self.bytes.saturating_sub(b);
            self.touch(key);
            self.shown = self.shown.wrapping_add(1);
        }
    }

    /// Drop the least recently used frames until the cache is at 90 % of `cap`.
    fn evict(&mut self, cap: usize) {
        if self.bytes <= cap {
            return;
        }
        let target = cap / 10 * 9;
        let mut all: Vec<(u64, (u64, u64), Tick)> = self.nodes.iter().flat_map(|(k, n)| n.frames.iter().map(move |(t, f)| (f.used, *k, *t))).collect();
        all.sort_unstable_by_key(|e| e.0);
        let mut touched: Vec<(u64, u64)> = Vec::new();
        for (_, k, t) in all {
            if self.bytes <= target {
                break;
            }
            if let Some(f) = self.nodes.get_mut(&k).and_then(|n| n.frames.remove(&t)) {
                self.bytes = self.bytes.saturating_sub(f.bytes());
                if !touched.contains(&k) {
                    touched.push(k);
                }
            }
        }
        for k in touched {
            self.touch(k);
        }
        // (a node keeps its frame step while a track may still add to it)
        self.nodes.retain(|_, n| !n.frames.is_empty() || n.step > 0);
    }
}

/// The in-memory cache of Magic Mask mattes: per (clip, node, clip time), the model's matte
/// (unrefined) and the stroke owner it was made from. Byte-capped, least recently used first out.
pub struct MatteCache {
    inner: Mutex<Inner>,
    cap: usize,
    /// Mixed into nonzero matte epochs, so render-cache files of another session never match.
    salt: u64,
}

impl Default for MatteCache {
    fn default() -> Self {
        Self::with_cap(DEFAULT_CACHE_BYTES)
    }
}

impl std::fmt::Debug for MatteCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatteCache").field("bytes", &self.bytes()).field("cap", &self.cap).finish()
    }
}

impl MatteCache {
    /// A cache holding at most `cap` bytes of mattes.
    pub fn with_cap(cap: usize) -> Self {
        let mut h = DefaultHasher::new();
        web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).hash(&mut h);
        next_stamp().hash(&mut h);
        Self { inner: Mutex::new(Inner::default()), cap, salt: h.finish() }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    /// Frames held for a node.
    pub fn frames(&self, clip: ClipId, node: u64) -> usize {
        self.lock().nodes.get(&(clip.0, node)).map_or(0, |n| n.frames.len())
    }

    /// A frozen copy of every matte (shared, not copied), for a render that must not see a
    /// running track's changes (an export). Never evicts.
    pub fn snapshot(&self) -> MatteCache {
        let g = self.lock();
        let copy = MatteCache::with_cap(usize::MAX);
        {
            let mut c = copy.lock();
            c.nodes = g.nodes.clone();
            c.bytes = g.bytes;
        }
        copy
    }

    fn set_step(&self, clip: ClipId, node: u64, step: i64) {
        self.lock().nodes.entry((clip.0, node)).or_default().step = step.max(0);
    }

    /// The matte epoch of a node: 0 before anything was tracked or forgotten, else a value that
    /// changes whenever its mattes change as a set (a track ended, frames evicted or forgotten).
    pub fn matte_epoch(&self, clip: ClipId, node: u64) -> u64 {
        match self.lock().epochs.get(&(clip.0, node)) {
            None => 0,
            Some(e) => {
                let mut h = DefaultHasher::new();
                (self.salt, *e).hash(&mut h);
                h.finish().max(1)
            }
        }
    }

    /// Moves whenever any node's matte epoch does.
    pub fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    /// Moves when a track ends or a node's mattes are dropped (mask removed, clip invalidated),
    /// but not when the byte cap evicts frames: what the app's frame cache follows (fork G3.3),
    /// so evictions during a long track don't re-render the Program monitor. 0: never moved.
    pub fn shown_epoch(&self) -> u64 {
        self.lock().shown
    }

    /// The node's mattes changed as a set (a track of it ended).
    fn touch(&self, clip: ClipId, node: u64) {
        let mut g = self.lock();
        g.touch((clip.0, node));
        g.shown = g.shown.wrapping_add(1);
    }

    /// The edit generation of a node ([`Self::bump`]; 0: none yet).
    pub fn generation(&self, clip: ClipId, node: u64) -> u64 {
        self.lock().gens.get(&(clip.0, node)).copied().unwrap_or(0)
    }

    /// The node's generation, given one if it has none (a track records it).
    fn claim_generation(&self, clip: ClipId, node: u64) -> u64 {
        *self.lock().gens.entry((clip.0, node)).or_insert_with(next_stamp)
    }

    /// The node's strokes changed: a running track of it stops.
    pub fn bump(&self, clip: ClipId, node: u64) {
        self.lock().gens.insert((clip.0, node), next_stamp());
    }

    /// Insert a matte unless the node's generation moved past `generation` (false: not inserted).
    #[allow(clippy::too_many_arguments)]
    fn insert(&self, clip: ClipId, node: u64, t: Tick, owner_hash: u64, matte: Arc<Matte>, cover: (f32, f32), held: bool, generation: u64) -> bool {
        let mut g = self.lock();
        if g.gens.get(&(clip.0, node)).copied() != Some(generation) {
            return false;
        }
        g.clock = g.clock.saturating_add(1);
        let used = g.clock;
        let f = Frame { owner_hash, matte, cover, refined: None, bitmap: None, used, held };
        let add = f.bytes();
        let old = g.nodes.entry((clip.0, node)).or_default().frames.insert(t, f);
        g.bytes = g.bytes.saturating_add(add).saturating_sub(old.map_or(0, |o| o.bytes()));
        let cap = self.cap;
        g.evict(cap);
        true
    }

    /// The cached matte at exactly `t` when it was made for `owner_hash`.
    fn get_valid(&self, clip: ClipId, node: u64, t: Tick, owner_hash: u64) -> Option<(Arc<Matte>, (f32, f32))> {
        let mut g = self.lock();
        g.clock = g.clock.saturating_add(1);
        let now = g.clock;
        let f = g.nodes.get_mut(&(clip.0, node))?.frames.get_mut(&t).filter(|f| f.owner_hash == owner_hash)?;
        f.used = now;
        Some((f.matte.clone(), f.cover))
    }

    /// The tracked frame time nearest `t` (within half a frame) whose matte is still valid for
    /// `mask`.
    fn nearest_valid(g: &Inner, key: (u64, u64), mask: &MagicMask, picture: u64, t: Tick) -> Option<Tick> {
        let n = g.nodes.get(&key)?;
        let below = n.frames.range(..=t).next_back().map(|(k, _)| *k);
        let above = n.frames.range(t..).next().map(|(k, _)| *k);
        let near = match (below, above) {
            (Some(b), Some(a)) => {
                if a.0.saturating_sub(t.0) < t.0.saturating_sub(b.0) {
                    a
                } else {
                    b
                }
            }
            (Some(b), None) => b,
            (None, a) => a?,
        };
        if near.0.abs_diff(t.0) > (n.step / 2).unsigned_abs() {
            return None;
        }
        let times = stroke_times(mask);
        let at = owner(&times, near)?;
        let f = n.frames.get(&near)?;
        (f.owner_hash == owner_hash(mask, at, picture)).then_some(near)
    }

    /// The model's matte (unrefined) for `mask` on (`clip`, `node`) at clip time `t`, if tracked
    /// for the clip's current `picture` ([`picture_fingerprint`]).
    pub fn raw(&self, clip: ClipId, node: u64, mask: &MagicMask, picture: u64, t: Tick) -> Option<Arc<Matte>> {
        let mut g = self.lock();
        let key = (clip.0, node);
        let near = Self::nearest_valid(&g, key, mask, picture, t)?;
        g.clock = g.clock.saturating_add(1);
        let now = g.clock;
        let f = g.nodes.get_mut(&key)?.frames.get_mut(&near)?;
        f.used = now;
        Some(f.matte.clone())
    }

    /// The matte for `mask` on (`clip`, `node`) at clip time `t`, refined with the mask's
    /// refine settings (the last refined matte per frame is kept). None when the frame has not
    /// been tracked for the current strokes and `picture` ([`picture_fingerprint`]), or was evicted.
    pub fn matte(&self, clip: ClipId, node: u64, mask: &MagicMask, picture: u64, t: Tick) -> Option<Arc<Matte>> {
        self.matte_near(clip, node, mask, picture, t).map(|(_, m)| m)
    }

    /// [`Self::matte`] and the time of the frame it belongs to.
    fn matte_near(&self, clip: ClipId, node: u64, mask: &MagicMask, picture: u64, t: Tick) -> Option<(Tick, Arc<Matte>)> {
        let want = mask.refine.cleaned();
        let r = to_refine(want);
        let (near, raw) = {
            let mut g = self.lock();
            let key = (clip.0, node);
            let near = Self::nearest_valid(&g, key, mask, picture, t)?;
            g.clock = g.clock.saturating_add(1);
            let now = g.clock;
            let f = g.nodes.get_mut(&key)?.frames.get_mut(&near)?;
            f.used = now;
            if r.is_identity() {
                return Some((near, f.matte.clone()));
            }
            if let Some((have, m)) = &f.refined
                && *have == want
            {
                return Some((near, m.clone()));
            }
            (near, f.matte.clone())
        };
        // (refined outside the lock: a large soften takes a few milliseconds)
        let refined = Arc::new(refine(&raw, &r).ok()?);
        let mut g = self.lock();
        let mut grew = 0usize;
        if let Some(f) = g.nodes.get_mut(&(clip.0, node)).and_then(|n| n.frames.get_mut(&near)).filter(|f| Arc::ptr_eq(&f.matte, &raw)) {
            let before = f.bytes();
            f.refined = Some((want, refined.clone()));
            grew = f.bytes().saturating_sub(before);
        }
        g.bytes = g.bytes.saturating_add(grew);
        let cap = self.cap;
        g.evict(cap);
        Some((near, refined))
    }

    /// [`Self::matte`] as the renderer takes it, converted once per frame and refine setting
    /// (kept with the frame, counted in the byte cap).
    pub fn bitmap(&self, clip: ClipId, node: u64, mask: &MagicMask, picture: u64, t: Tick) -> Option<Arc<BitmapMatte>> {
        let (near, m) = self.matte_near(clip, node, mask, picture, t)?;
        let key = (clip.0, node);
        {
            let g = self.lock();
            if let Some((src, b)) = g.nodes.get(&key).and_then(|n| n.frames.get(&near)).and_then(|f| f.bitmap.as_ref())
                && Arc::ptr_eq(src, &m)
            {
                return Some(b.clone());
            }
        }
        let b = Arc::new(BitmapMatte::new(u32::try_from(m.width).ok()?, u32::try_from(m.height).ok()?, m.alpha.clone())?);
        let mut g = self.lock();
        let mut delta: Option<(usize, usize)> = None;
        if let Some(f) = g.nodes.get_mut(&key).and_then(|n| n.frames.get_mut(&near)) {
            let before = f.bytes();
            f.bitmap = Some((m, b.clone()));
            delta = Some((before, f.bytes()));
        }
        if let Some((before, after)) = delta {
            g.bytes = g.bytes.saturating_add(after).saturating_sub(before);
        }
        let cap = self.cap;
        g.evict(cap);
        Some(b)
    }

    /// Forget every matte of a node.
    pub fn invalidate_node(&self, clip: ClipId, node: u64) {
        self.lock().remove_node((clip.0, node));
    }

    /// The node's mask is gone: forget its mattes, its edit generation (a track of it stops: no
    /// generation matches) and its epoch.
    fn forget_node(&self, clip: ClipId, node: u64) {
        let mut g = self.lock();
        let key = (clip.0, node);
        g.remove_node(key);
        g.gens.remove(&key);
        g.epochs.remove(&key);
        g.epoch = g.epoch.wrapping_add(1);
    }

    /// Forget every matte of a clip (e.g. it was relinked to other media).
    pub fn invalidate_clip(&self, clip: ClipId) {
        let mut g = self.lock();
        let keys: Vec<(u64, u64)> = g.nodes.keys().filter(|k| k.0 == clip.0).copied().collect();
        for k in keys {
            g.remove_node(k);
        }
    }
}

/// The refined Magic Mask matte of node `node` of clip `clip` (active sequence) at clip time
/// `source_time` (the time grade keyframes use: `TrackItem::effect_time_at`), or None when the
/// node has no Magic Mask or that frame has not been tracked for its current strokes. The matte
/// is at the tracked preview size (scale it to the frame).
pub fn magic_matte(s: &Session, clip: ClipId, node: u64, source_time: Tick) -> Option<Arc<Matte>> {
    let q = s.active_sequence()?;
    let (_, it) = q.find_item(clip)?;
    let mask = it.grade.as_ref()?.node(node)?.magic_mask.as_ref()?;
    s.magic_mask.cache.matte(clip, node, mask, picture_fingerprint(&s.project, it), source_time)
}

/// Clips whose moving-effects hash [`CacheSource`] keeps.
const MAX_MEMO: usize = 256;

/// The renderer's view of a matte cache ([`MagicMasks::token`]).
struct CacheSource {
    cache: Arc<MatteCache>,
    /// Per clip: its moving effects as last seen and their hash (a frame then compares instead
    /// of serialising them).
    effects: Mutex<HashMap<u64, (Vec<filmcraft_project::EffectInstance>, u64)>>,
}

impl CacheSource {
    fn new(cache: Arc<MatteCache>) -> Self {
        Self { cache, effects: Mutex::default() }
    }

    fn fingerprint(&self, project: &Project, item: &TrackItem) -> u64 {
        let mut memo = self.effects.lock().unwrap_or_else(PoisonError::into_inner);
        let hash = match memo.get(&item.id.0) {
            Some((seen, h)) if seen.iter().eq(moving_effects(item)) => *h,
            _ => {
                let h = effects_hash(item);
                if memo.len() >= MAX_MEMO {
                    memo.clear();
                }
                memo.insert(item.id.0, (moving_effects(item).cloned().collect(), h));
                h
            }
        };
        drop(memo);
        fingerprint_with(project, item, hash)
    }
}

impl MatteSource for CacheSource {
    fn matte(&self, project: &Project, item: &TrackItem, node: &filmcraft_project::grade::GradeNode, source_t: Tick) -> Option<Arc<BitmapMatte>> {
        let mask = node.magic_mask.as_ref()?;
        self.cache.bitmap(item.id, node.id, mask, self.fingerprint(project, item), source_t)
    }
}

/// A registration of the session's cache as the renderer's matte source.
struct Registered {
    reg: Registration,
    cache: Arc<MatteCache>,
}

// ---------------------------------------------------------------------------------------------
// Session state, segmenter
// ---------------------------------------------------------------------------------------------

/// How a track ended (or that it hasn't).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackState {
    #[default]
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
    /// The node's strokes changed (or its mask was removed) while it ran: track again.
    Superseded,
}

/// Progress and result of one `grade.magicMask.track`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackStatus {
    pub job: JobId,
    pub clip: u64,
    pub node: u64,
    pub state: TrackState,
    /// Frames the job visits.
    pub frames: usize,
    pub done: usize,
    /// Frames the model segmented (the rest were already cached, or held the last good matte).
    pub segmented: usize,
    /// Frames taken from the cache.
    pub reused: usize,
    /// Where the subject was lost, frames without a picture… in plain words.
    pub notes: Vec<String>,
    pub error: Option<String>,
    /// The model actually tracking, in plain words ("Better (SAM 2.1 Hiera Large) on …",
    /// "MobileSAM"); changes when SAM 2.1 falls back.
    pub used: String,
}

/// A track still queued or running ([`MagicMasks::running`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunningTrack {
    pub job: JobId,
    pub clip: u64,
    pub node: u64,
    pub done: usize,
    pub frames: usize,
}

type SharedStatus = Arc<Mutex<TrackStatus>>;

fn lock_status(s: &SharedStatus) -> MutexGuard<'_, TrackStatus> {
    s.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Which model tracks Magic Masks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaskModel {
    /// Better on a discrete GPU when SAM 2.1 Large is installed, else MobileSAM.
    #[default]
    Auto,
    /// SAM 2.1 Hiera Large (GPU).
    Better,
    /// SAM 2.1 Hiera Base+ (GPU).
    Faster,
    /// MobileSAM, frame by frame (CPU, or its image encoder on the GPU).
    MobileSam,
}

impl MaskModel {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "auto" => MaskModel::Auto,
            "better" => MaskModel::Better,
            "faster" => MaskModel::Faster,
            "mobileSam" | "mobilesam" => MaskModel::MobileSam,
            _ => return None,
        })
    }
}

/// MobileSAM's name in a track's status.
const MOBILE_SAM: &str = "MobileSAM";

/// SAM 2.1 for a track: the size, its files, and whether `Auto` chose it (then a non-discrete
/// GPU or a failed load falls back to MobileSAM).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VideoPlan {
    size: Sam2Size,
    files: (PathBuf, PathBuf),
    auto: bool,
}

/// The session's Magic Mask state (`Session::magic_mask`).
#[derive(Clone)]
pub struct MagicMasks {
    /// Which model tracks (a track's `model` parameter overrides it).
    pub model: MaskModel,
    /// Override of SAM 2.1's folder (tests); None: `<data dir>/models/sam2`.
    pub sam2_dir: Option<PathBuf>,
    /// The model each node was last tracked with (a change re-tracks every frame).
    used: Arc<Mutex<HashMap<(u64, u64), String>>>,
    /// The mattes (shared with the jobs; the renderer reads it).
    pub cache: Arc<MatteCache>,
    /// A segmenter installed by the host or a test; None: MobileSAM from [`Self::model_path`].
    pub segmenter: Option<Arc<dyn Segmenter>>,
    /// Override of the MobileSAM weights' path (tests); None: `<data dir>/models/mobilesam/mobile_sam.pt`.
    pub model_path: Option<PathBuf>,
    /// A video segmenter factory installed by the host or a test (track-assisted roto, fork
    /// C3.7, uses it before any model); None: SAM 2.1 / MobileSAM as [`Self::model`] picks.
    pub video_segmenter: Option<VideoFactory>,
    tracks: Arc<Mutex<VecDeque<SharedStatus>>>,
    registered: Arc<Mutex<Option<Registered>>>,
}

impl Default for MagicMasks {
    fn default() -> Self {
        Self {
            model: MaskModel::Auto,
            sam2_dir: None,
            used: Arc::default(),
            cache: Arc::new(MatteCache::default()),
            segmenter: None,
            model_path: None,
            video_segmenter: None,
            tracks: Arc::default(),
            registered: Arc::default(),
        }
    }
}

impl std::fmt::Debug for MagicMasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MagicMasks").field("cache", &self.cache).field("model_path", &self.model_path).finish()
    }
}

/// Makes a fresh [`VideoSegmenter`](filmcraft_mask::video::VideoSegmenter) (one per follow).
pub type VideoFactory = Arc<dyn Fn() -> Box<dyn filmcraft_mask::video::VideoSegmenter> + Send + Sync>;

/// The video segmenter a one-off follow uses (track-assisted roto, fork C3.7), chosen cheaply on
/// the command's thread ([`MagicMasks::video_choice`]; errors in plain words there) and opened on
/// the worker ([`VideoChoice::open`]: model loads are slow).
#[derive(Clone)]
pub(crate) enum VideoChoice {
    Factory(VideoFactory),
    /// MobileSAM (or an installed image segmenter) with the colour-guided follow.
    Image(SegSource),
    /// SAM 2.1, with MobileSAM as the fallback when there is one.
    #[cfg_attr(not(feature = "magic-mask"), allow(dead_code))]
    Sam2(VideoPlan, Option<SegSource>),
}

impl VideoChoice {
    /// The segmenter for a follow over `frames` frames and its name (for the status).
    pub(crate) fn open(&self, frames: usize) -> std::result::Result<(Box<dyn filmcraft_mask::video::VideoSegmenter>, String), String> {
        use filmcraft_mask::video::{ImageTracker, VideoSegmenter};
        let image = |src: &SegSource| -> std::result::Result<(Box<dyn VideoSegmenter>, String), String> {
            let seg = match src {
                SegSource::Ready(s) => s.clone(),
                SegSource::Load(p) => load(p).map_err(|e| e.to_string())?,
            };
            let t = ImageTracker::new(seg);
            let name = t.name();
            Ok((Box::new(t), name))
        };
        match self {
            VideoChoice::Factory(f) => {
                let v = f();
                let name = v.name();
                Ok((v, name))
            }
            VideoChoice::Image(src) => image(src),
            #[cfg(feature = "magic-mask")]
            VideoChoice::Sam2(plan, fallback) => {
                let tried = crate::magic_mask_sam2::load(plan.size, &plan.files).map_err(|e| e.to_string()).and_then(|m| {
                    if plan.auto && !m.discrete {
                        return Err("SAM 2.1 needs a discrete GPU to run at a usable speed".to_string());
                    }
                    let name = m.name();
                    m.tracker(frames).map(|t| (Box::new(t) as Box<dyn VideoSegmenter>, name)).map_err(|e| e.to_string())
                });
                match (tried, fallback) {
                    (Ok(v), _) => Ok(v),
                    (Err(why), Some(src)) => image(src).map(|(v, name)| (v, format!("{name} ({why})"))),
                    (Err(why), None) => Err(format!("SAM 2.1 can't run: {why}")),
                }
            }
            #[cfg(not(feature = "magic-mask"))]
            VideoChoice::Sam2(_, fallback) => {
                let _ = frames;
                match fallback {
                    Some(src) => image(src),
                    None => Err("SAM 2.1 can't run in this build (it was built without the `magic-mask` feature)".into()),
                }
            }
        }
    }

    /// The segmenter [`Self::open`] gave failed as a lost GPU does: a SAM 2.1 model is forgotten
    /// (the next track loads it again, as Magic Mask's own tracks do).
    pub(crate) fn gpu_failed(&self) {
        #[cfg(feature = "magic-mask")]
        if let VideoChoice::Sam2(plan, _) = self {
            crate::magic_mask_sam2::forget(plan.size);
        }
    }
}

/// Where MobileSAM's weights live in the data folder.
pub fn default_model_path() -> Option<PathBuf> {
    crate::transcript::models_dir().map(|d| d.join("mobilesam").join("mobile_sam.pt"))
}

/// The segmenter a job uses: ready, or MobileSAM to load on the worker thread.
#[derive(Clone)]
pub(crate) enum SegSource {
    Ready(Arc<dyn Segmenter>),
    #[cfg_attr(not(feature = "magic-mask"), allow(dead_code))]
    Load(PathBuf),
}

/// MobileSAM with its image encoder on the GPU when one works (fork G3, FORK-PLAN §1a.122):
/// [`filmcraft_gpu::tinyvit`] makes the embedding (≈ 30–50 ms on a discrete GPU instead of
/// ≈ 4 s on the CPU), candle on the CPU decodes it.
///
/// The GPU encoder (weights + activations, ≈ 200 MB of VRAM) is opened on the first frame,
/// released after [`gpu_sam::IDLE_RELEASE`] without use (a background reaper; the next frame
/// opens it again, ≈ 1 s) and with the model. No usable GPU, `FILMCRAFT_NO_GPU_MASK`, a wasm
/// build, or a GPU failure while encoding: the CPU encoder, from then on.
#[cfg(feature = "magic-mask")]
pub(crate) mod gpu_sam {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use web_time::Instant;

    use filmcraft_gpu::tinyvit::TinyVitGpu;
    use filmcraft_mask::sam::MobileSam;
    use filmcraft_mask::{Embedding, MaskError, Prompt, RgbImage, Segment, Segmenter};

    /// Unused this long, the GPU encoder is released.
    pub(crate) const IDLE_RELEASE: Duration = Duration::from_secs(120);
    /// How often the reaper looks.
    #[cfg(not(target_arch = "wasm32"))]
    const REAP_EVERY: Duration = Duration::from_secs(15);

    enum Gpu {
        /// Not opened yet, or released while idle: opened on the next frame.
        Closed,
        Ready(TinyVitGpu, Instant),
        /// Unavailable or failed: the CPU from now on.
        Off,
    }

    pub(crate) struct GpuSam {
        sam: MobileSam,
        gpu: Mutex<Gpu>,
        /// The idle reaper is running.
        #[cfg(not(target_arch = "wasm32"))]
        reaper: std::sync::atomic::AtomicBool,
    }

    impl GpuSam {
        pub(crate) fn new(sam: MobileSam) -> Arc<Self> {
            let off = cfg!(target_arch = "wasm32") || std::env::var_os("FILMCRAFT_NO_GPU_MASK").is_some();
            Arc::new(Self {
                sam,
                gpu: Mutex::new(if off { Gpu::Off } else { Gpu::Closed }),
                #[cfg(not(target_arch = "wasm32"))]
                reaper: std::sync::atomic::AtomicBool::new(false),
            })
        }

        fn lock(&self) -> MutexGuard<'_, Gpu> {
            self.gpu.lock().unwrap_or_else(PoisonError::into_inner)
        }

        #[cfg(not(target_arch = "wasm32"))]
        fn open(&self) -> Option<TinyVitGpu> {
            use std::collections::HashMap;

            use filmcraft_gpu::tinyvit::Weight;
            let opened = catch_unwind(AssertUnwindSafe(|| -> Result<TinyVitGpu, String> {
                let w: HashMap<String, Weight> =
                    self.sam.encoder_weights().map_err(|e| e.to_string())?.into_iter().map(|t| (t.name, Weight { shape: t.shape, data: t.data })).collect();
                TinyVitGpu::open(&w).map_err(|e| e.to_string())
            }));
            match opened {
                Ok(Ok(g)) => {
                    log::info!("Magic Mask: image encoder on {}", g.name());
                    Some(g)
                }
                Ok(Err(e)) => {
                    log::info!("Magic Mask: image encoder on the CPU ({e})");
                    None
                }
                Err(_) => {
                    log::warn!("Magic Mask: the GPU encoder stopped unexpectedly while loading; using the CPU");
                    None
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        fn open(&self) -> Option<TinyVitGpu> {
            None
        }

        /// Starts the thread that releases the GPU encoder when idle (once; it ends with `this`).
        #[cfg(not(target_arch = "wasm32"))]
        fn start_reaper(this: &Arc<Self>) {
            use std::sync::atomic::Ordering;
            if this.reaper.swap(true, Ordering::SeqCst) {
                return;
            }
            let weak = Arc::downgrade(this);
            let spawned = std::thread::Builder::new().name("magic-mask-gpu-reaper".into()).spawn(move || {
                loop {
                    std::thread::sleep(REAP_EVERY);
                    let Some(s) = weak.upgrade() else { break };
                    s.release_if_idle(IDLE_RELEASE);
                }
            });
            if spawned.is_err() {
                this.reaper.store(false, Ordering::SeqCst);
            }
        }

        /// Releases the GPU encoder when it has not been used for `idle`.
        pub(crate) fn release_if_idle(&self, idle: Duration) {
            let mut g = self.lock();
            if matches!(&*g, Gpu::Ready(_, last) if last.elapsed() >= idle) {
                *g = Gpu::Closed;
                log::info!("Magic Mask: GPU encoder released (idle)");
            }
        }

        /// Whether the GPU encoder is open.
        #[cfg(test)]
        pub(crate) fn on_gpu(&self) -> bool {
            matches!(&*self.lock(), Gpu::Ready(..))
        }
    }

    /// The segmenter the engine uses: [`GpuSam`] behind an `Arc` (the reaper holds a weak one).
    pub(crate) struct Shared(pub(crate) Arc<GpuSam>);

    impl Segmenter for Shared {
        fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError> {
            let me = &self.0;
            let mut g = me.lock();
            if matches!(&*g, Gpu::Closed) {
                *g = match me.open() {
                    Some(gpu) => Gpu::Ready(gpu, Instant::now()),
                    None => Gpu::Off,
                };
                #[cfg(not(target_arch = "wasm32"))]
                if matches!(&*g, Gpu::Ready(..)) {
                    GpuSam::start_reaper(me);
                }
            }
            if let Gpu::Ready(gpu, last) = &mut *g {
                // a bad frame is the caller's error, not the GPU's
                let (input, fitted) = me.sam.prepare_input(image)?;
                match catch_unwind(AssertUnwindSafe(|| gpu.encode(&input))) {
                    Ok(Ok(features)) => {
                        *last = Instant::now();
                        drop(g);
                        return me.sam.embedding_from_features(features, image.width(), image.height(), fitted);
                    }
                    Ok(Err(e)) => log::warn!("Magic Mask: the GPU encoder failed ({e}); using the CPU from now on"),
                    Err(_) => log::warn!("Magic Mask: the GPU encoder stopped unexpectedly; using the CPU from now on"),
                }
                *g = Gpu::Off;
            }
            drop(g);
            me.sam.embed(image)
        }

        fn segment(&self, embedding: &Embedding, prompt: &Prompt) -> Result<Segment, MaskError> {
            self.0.sam.segment(embedding, prompt)
        }

        fn candidates(&self, embedding: &Embedding, prompt: &Prompt, multimask: bool) -> Result<Vec<Segment>, MaskError> {
            self.0.sam.candidates(embedding, prompt, multimask)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use filmcraft_mask::{Point, Rect};

        /// GPU and CPU embeddings give the same mask; idle release reopens on the next frame;
        /// the CPU takes over when the GPU is off.
        /// Skips without the model (`$FILMCRAFT_MOBILESAM`, else the checkout's `models/`).
        #[test]
        fn gpu_and_cpu_encoders_agree_and_the_cpu_takes_over() {
            let path = std::env::var_os("FILMCRAFT_MOBILESAM")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/mobilesam/mobile_sam.pt"));
            if !path.is_file() {
                eprintln!("skipped: {} not installed", path.display());
                return;
            }
            let shared = Shared(GpuSam::new(MobileSam::load(&path).unwrap()));
            let cpu = MobileSam::load(&path).unwrap();
            let (w, h) = (320, 200);
            let inside = |x: usize, y: usize| (110..210).contains(&x) && (50..150).contains(&y);
            let img = RgbImage::new(w, h, (0..w * h).flat_map(|i| if inside(i % w, i / w) { [220, 60, 40] } else { [40, 90, 160] }).collect()).unwrap();
            let p = Prompt {
                points: vec![Point { x: 0.5, y: 0.5, positive: true }],
                rect: Some(Rect { x0: 0.3, y0: 0.2, x1: 0.7, y1: 0.8 }),
                prior: None,
                prefer_whole: false,
            };
            let a = shared.segment(&shared.embed(&img).unwrap(), &p).unwrap().matte(w, h).unwrap();
            let b = cpu.segment(&cpu.embed(&img).unwrap(), &p).unwrap().matte(w, h).unwrap();
            let iou = a.iou(&b).unwrap();
            let on_gpu = shared.0.on_gpu();
            eprintln!("on GPU: {on_gpu}; IoU vs CPU {iou}");
            assert!(iou >= 0.99, "IoU {iou}");
            // idle release, then the next frame opens the GPU encoder again
            shared.0.release_if_idle(Duration::ZERO);
            assert!(!shared.0.on_gpu());
            let a2 = shared.segment(&shared.embed(&img).unwrap(), &p).unwrap().matte(w, h).unwrap();
            assert_eq!(shared.0.on_gpu(), on_gpu);
            assert!(a2.iou(&b).unwrap() >= 0.99);
            // the GPU off: the CPU path gives the CPU's mask
            *shared.0.lock() = Gpu::Off;
            let c = shared.segment(&shared.embed(&img).unwrap(), &p).unwrap().matte(w, h).unwrap();
            assert_eq!(c, b);
        }
    }
}

/// MobileSAM, loaded once per path.
#[cfg(feature = "magic-mask")]
static LOADED: Mutex<Option<(PathBuf, Arc<dyn Segmenter>)>> = Mutex::new(None);

fn loaded(path: &std::path::Path) -> Option<Arc<dyn Segmenter>> {
    #[cfg(feature = "magic-mask")]
    {
        let g = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
        g.as_ref().filter(|(p, _)| p == path).map(|(_, s)| s.clone())
    }
    #[cfg(not(feature = "magic-mask"))]
    {
        let _ = path;
        None
    }
}

/// Release the loaded models (MobileSAM and SAM 2; `ai.stop`): running tracks keep their copies
/// until they end, the next track loads them again.
pub(crate) fn release_models() {
    #[cfg(feature = "magic-mask")]
    {
        *LOADED.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    #[cfg(feature = "magic-mask")]
    crate::magic_mask_sam2::release();
}

/// Whether a Magic Mask model is in memory.
pub(crate) fn models_loaded() -> bool {
    #[cfg(feature = "magic-mask")]
    let mobile = LOADED.lock().unwrap_or_else(PoisonError::into_inner).is_some();
    #[cfg(not(feature = "magic-mask"))]
    let mobile = false;
    #[cfg(feature = "magic-mask")]
    let sam2 = crate::magic_mask_sam2::loaded();
    #[cfg(not(feature = "magic-mask"))]
    let sam2 = false;
    mobile || sam2
}

/// Held while a model loads, so two first loads don't load it (and open a GPU encoder) twice.
#[cfg(feature = "magic-mask")]
static LOADING: Mutex<()> = Mutex::new(());

fn load(path: &std::path::Path) -> std::result::Result<Arc<dyn Segmenter>, MaskError> {
    #[cfg(feature = "magic-mask")]
    let _loading = LOADING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(s) = loaded(path) {
        return Ok(s);
    }
    #[cfg(feature = "magic-mask")]
    {
        // (replacing another path's model drops it, and its GPU encoder with it)
        let sam: Arc<dyn Segmenter> = Arc::new(gpu_sam::Shared(gpu_sam::GpuSam::new(filmcraft_mask::sam::MobileSam::load(path)?)));
        *LOADED.lock().unwrap_or_else(PoisonError::into_inner) = Some((path.to_path_buf(), sam.clone()));
        Ok(sam)
    }
    #[cfg(not(feature = "magic-mask"))]
    Err(MaskError::ModelMissing(format!("{} (this build has no Magic Mask model support)", path.display())))
}

impl MagicMasks {
    /// The token that shows this session's mattes in a render (`RenderOptions::mattes`): the
    /// cache is registered as a [`MatteSource`] on first use (again if [`Self::cache`] was
    /// replaced). The registration lives as long as this state (and its clones).
    pub fn token(&self) -> MatteToken {
        let mut g = self.registered.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(r) = g.as_ref()
            && Arc::ptr_eq(&r.cache, &self.cache)
        {
            return r.reg.token();
        }
        // (the old registration unregisters as it drops)
        let reg = Registration::new(Arc::new(CacheSource::new(self.cache.clone())));
        let token = reg.token();
        *g = Some(Registered { reg, cache: self.cache.clone() });
        token
    }

    /// A frozen copy of the mattes registered for one render job (an export): a track still
    /// running doesn't change what it shows. The token works while the handle lives.
    pub fn snapshot_registration(&self) -> Registration {
        Registration::new(Arc::new(CacheSource::new(Arc::new(self.cache.snapshot()))))
    }

    /// The MobileSAM weights this session would load.
    pub fn model_path(&self) -> Option<PathBuf> {
        self.model_path.clone().or_else(default_model_path)
    }

    /// SAM 2.1's folder.
    pub fn sam2_dir(&self) -> Option<PathBuf> {
        #[cfg(feature = "magic-mask")]
        {
            self.sam2_dir.clone().or_else(crate::magic_mask_sam2::default_dir)
        }
        #[cfg(not(feature = "magic-mask"))]
        {
            self.sam2_dir.clone()
        }
    }

    /// SAM 2.1 for a track with `model`, or None (MobileSAM). An installed segmenter (tests)
    /// always wins. Better / Faster without their files is an error in plain words.
    fn video_plan(&self, model: MaskModel, cmd: &str) -> Result<Option<VideoPlan>> {
        if self.segmenter.is_some() || model == MaskModel::MobileSam {
            return Ok(None);
        }
        let dir = self.sam2_dir();
        #[cfg(feature = "magic-mask")]
        let find = |size: Sam2Size| dir.as_deref().and_then(|d| crate::magic_mask_sam2::files_in(d, size));
        #[cfg(not(feature = "magic-mask"))]
        let find = |_: Sam2Size| -> Option<(PathBuf, PathBuf)> { None };
        let want = |size: Sam2Size| -> Result<Option<VideoPlan>> {
            if !cfg!(feature = "magic-mask") {
                return Err(bad(cmd, "SAM 2.1 can't run in this build (it was built without the `magic-mask` feature)"));
            }
            match find(size) {
                Some(files) => Ok(Some(VideoPlan { size, files, auto: false })),
                None => {
                    let (pt, yaml) = size.files();
                    let d = dir.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "the models folder".into());
                    Err(bad(cmd, format!("{} is not installed: put {pt} and {yaml} in {d}", size.label())))
                }
            }
        };
        match model {
            MaskModel::Better => want(Sam2Size::Large),
            MaskModel::Faster => want(Sam2Size::BasePlus),
            // Auto = Better when SAM 2.1 Large is installed (it falls back to MobileSAM without a
            // usable discrete GPU); owner question Q66, resolved once SAM 2.1 passed the shadow scene
            _ => Ok(find(Sam2Size::Large).map(|files| VideoPlan { size: Sam2Size::Large, files, auto: true })),
        }
    }

    /// What [`MaskModel::Auto`] picks now: Better when SAM 2.1 Large is installed (it still
    /// falls back to MobileSAM without a usable discrete GPU), else MobileSAM. Cheap (two file
    /// checks), for the model selector's label.
    pub fn auto_choice(&self) -> MaskModel {
        match self.video_plan(MaskModel::Auto, "grade.magicMask.track") {
            Ok(Some(_)) => MaskModel::Better,
            _ => MaskModel::MobileSam,
        }
    }

    /// The segmenter for a track, checked cheaply (the model file is there, the build can run
    /// it); loading happens on the worker.
    fn source(&self, cmd: &str) -> Result<SegSource> {
        if let Some(s) = &self.segmenter {
            return Ok(SegSource::Ready(s.clone()));
        }
        let path = self.model_path().ok_or_else(|| bad(cmd, MaskError::ModelMissing("there is no data folder for models".into()).to_string()))?;
        if let Some(s) = loaded(&path) {
            return Ok(SegSource::Ready(s));
        }
        if !path.is_file() {
            let dir = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
            return Err(bad(cmd, format!("{}: put mobile_sam.pt in {dir}", MaskError::ModelMissing(path.display().to_string()))));
        }
        if !cfg!(feature = "magic-mask") {
            return Err(bad(cmd, "Magic Mask can't run in this build (it was built without the `magic-mask` feature)"));
        }
        Ok(SegSource::Load(path))
    }

    /// The video segmenter for a one-off follow with `model` (track-assisted roto, fork C3.7):
    /// an installed factory, else SAM 2.1 (Better / Faster, or Auto when Large is installed; with
    /// MobileSAM as its fallback when that is there), else MobileSAM. Nothing usable: an error
    /// in plain words (which file to put where, or that the build can't run it).
    pub(crate) fn video_choice(&self, model: MaskModel, cmd: &str) -> Result<VideoChoice> {
        if let Some(f) = &self.video_segmenter {
            return Ok(VideoChoice::Factory(f.clone()));
        }
        match self.video_plan(model, cmd)? {
            Some(plan) => Ok(VideoChoice::Sam2(plan, self.source(cmd).ok())),
            None => Ok(VideoChoice::Image(self.source(cmd)?)),
        }
    }

    fn add_track(&self, st: SharedStatus) {
        let mut g = self.tracks.lock().unwrap_or_else(PoisonError::into_inner);
        if g.len() >= MAX_STATUSES {
            g.pop_front();
        }
        g.push_back(st);
    }

    /// The statuses of recent tracks (all, or one job's), oldest first.
    pub fn statuses(&self, job: Option<JobId>) -> Vec<TrackStatus> {
        let g = self.tracks.lock().unwrap_or_else(PoisonError::into_inner);
        g.iter().map(|s| lock_status(s).clone()).filter(|s| job.is_none_or(|j| s.job == j)).collect()
    }

    /// The tracks still queued or running, cheaply (no notes copied; called every frame by the
    /// app).
    pub fn running(&self) -> Vec<RunningTrack> {
        let g = self.tracks.lock().unwrap_or_else(PoisonError::into_inner);
        g.iter()
            .filter_map(|s| {
                let s = lock_status(s);
                matches!(s.state, TrackState::Queued | TrackState::Running).then_some(RunningTrack {
                    job: s.job,
                    clip: s.clip,
                    node: s.node,
                    done: s.done,
                    frames: s.frames,
                })
            })
            .collect()
    }

    /// The newest track of a node (one status copied).
    pub fn latest(&self, clip: ClipId, node: u64) -> Option<TrackStatus> {
        let g = self.tracks.lock().unwrap_or_else(PoisonError::into_inner);
        g.iter().rev().find_map(|s| {
            let s = lock_status(s);
            (s.clip == clip.0 && s.node == node).then(|| s.clone())
        })
    }

    /// Jobs still queued or running (for one node, or all).
    pub(crate) fn live_jobs(&self, node: Option<(ClipId, u64)>) -> Vec<JobId> {
        self.statuses(None)
            .into_iter()
            .filter(|s| matches!(s.state, TrackState::Queued | TrackState::Running) && node.is_none_or(|(c, n)| s.clip == c.0 && s.node == n))
            .map(|s| s.job)
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// The propagation job
// ---------------------------------------------------------------------------------------------

/// One stroke time's frames: its seed frame and the frames it owns after / before it.
struct Run {
    owner: Tick,
    hash: u64,
    prompt: Prompt,
    seed: usize,
    /// The seed frame is owned by this run (another stroke time could be nearer to it).
    seed_owned: bool,
    fwd: Vec<usize>,
    back: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Seed,
    Fwd(usize),
    Back(usize),
}

#[derive(Default)]
struct Walk {
    prev: Option<Segment>,
    /// The frame `prev` is from.
    prev_frame: Option<usize>,
    /// What following carries from frame to frame (colour models + the last mask); `None` after
    /// a cached frame (started again from `prev` on `prev_frame`'s picture when needed). Such a
    /// restart learns its colour models from that one frame: the history learnt before the
    /// cached frames is gone, so a run continued from cache can follow a little differently
    /// from an uncached run of the same frames (each frame is still keyed by its owner's strokes).
    track: Option<Track>,
    seed_track: Option<Track>,
    seed_seg: Option<Segment>,
    seed_matte: Option<Arc<Matte>>,
    last_good: Option<Arc<Matte>>,
    /// The subject was lost in this direction: the rest holds `last_good` (at most
    /// [`MAX_HELD_FRAMES`] frames, counted in `held`).
    lost: bool,
    seed_lost: bool,
    held: usize,
}

struct TrackJob {
    project: Arc<Project>,
    seq: ItemId,
    clip: ClipId,
    node: u64,
    provider: crate::media_pool::PoolProvider,
    scale: f32,
    /// Timeline and clip time of each frame of the clip.
    tl: Vec<Tick>,
    src: Vec<Tick>,
    runs: Vec<Run>,
    run: usize,
    phase: Phase,
    walk: Walk,
    cache: Arc<MatteCache>,
    /// MobileSAM (or an installed segmenter); None when only SAM 2.1 is installed.
    source: Option<SegSource>,
    segmenter: Option<Arc<dyn Segmenter>>,
    /// SAM 2.1 for this track, until it fails to load (then MobileSAM when there is one).
    video: Option<VideoPlan>,
    #[cfg(feature = "magic-mask")]
    tracker: Option<filmcraft_mask::sam2::tracker::Sam2Tracker>,
    status: SharedStatus,
    ended: bool,
    /// The node's edit generation when the track was queued ([`MatteCache::bump`]).
    generation: u64,
}

/// What computing one frame gave.
enum Got {
    /// The segment, its matte, and the follow state from it (when it could be started).
    Matte(Segment, Arc<Matte>, Option<Track>),
    /// Nothing found (empty matte) or no picture.
    Lost(String),
}

impl TrackJob {
    #[cfg_attr(not(feature = "magic-mask"), allow(dead_code))]
    fn set_used(&self, used: String) {
        lock_status(&self.status).used = used;
    }

    fn note(&self, n: String) {
        let mut st = lock_status(&self.status);
        if st.notes.len() < MAX_NOTES {
            st.notes.push(n);
        }
    }

    fn at(&self, frame: usize) -> String {
        let t = self.tl.get(frame).copied().unwrap_or(Tick::ZERO);
        format!("{:.2} s", t.seconds())
    }

    fn picture(&self, frame: usize) -> Option<RgbImage> {
        let t = *self.tl.get(frame)?;
        let opts = filmcraft_render::RenderOptions { scale: self.scale, ..Default::default() };
        let img = filmcraft_render::render_clip(&self.project, self.seq, self.clip, t, opts, &self.provider)?;
        let rgba = img.over_black_rgba8();
        let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        RgbImage::new(img.w, img.h, rgb).ok()
    }

    fn segmenter(&mut self) -> std::result::Result<Arc<dyn Segmenter>, String> {
        if let Some(s) = &self.segmenter {
            return Ok(s.clone());
        }
        let s = match &self.source {
            Some(SegSource::Ready(s)) => s.clone(),
            Some(SegSource::Load(p)) => load(p).map_err(|e| e.to_string())?,
            None => return Err(MaskError::ModelMissing("MobileSAM (mobile_sam.pt), the fallback when SAM 2.1 can't run".into()).to_string()),
        };
        self.segmenter = Some(s.clone());
        Ok(s)
    }

    /// SAM 2.1: the seed frame's prompt enters the memory, other frames are propagated
    /// (`backward`: towards earlier frames). Ok(None): no SAM 2.1 for this track (MobileSAM).
    #[cfg(feature = "magic-mask")]
    fn compute_video(&mut self, frame: usize, prompt: &Prompt, seed: bool, backward: bool) -> std::result::Result<Option<Got>, String> {
        use filmcraft_mask::video::{Direction, VideoSegmenter};
        let Some(plan) = self.video.clone() else { return Ok(None) };
        if self.tracker.is_none() {
            let fallback = |job: &mut Self, why: String| -> std::result::Result<Option<Got>, String> {
                if job.source.is_some() {
                    job.note(format!("tracking with MobileSAM: {why}"));
                    job.set_used(MOBILE_SAM.into());
                    job.video = None;
                    Ok(None)
                } else {
                    Err(why)
                }
            };
            match crate::magic_mask_sam2::load(plan.size, &plan.files) {
                Ok(m) if plan.auto && !m.discrete => return fallback(self, "SAM 2.1 needs a discrete GPU to run at a usable speed".into()),
                Ok(m) => match m.tracker(self.tl.len()) {
                    Ok(t) => {
                        self.tracker = Some(t);
                        self.note(format!("tracking with {}", m.name()));
                        self.set_used(m.name());
                    }
                    Err(e) => return fallback(self, format!("SAM 2.1 can't run ({e})")),
                },
                Err(e) => return fallback(self, format!("SAM 2.1 can't run ({e})")),
            }
        }
        let Some(img) = self.picture(frame) else { return Ok(Some(Got::Lost(format!("the clip has no picture at {}", self.at(frame))))) };
        let Some(tracker) = self.tracker.as_mut() else { return Ok(None) };
        let r = match if seed {
            tracker.add_prompt(frame, &img, prompt)
        } else {
            tracker.propagate(frame, &img, if backward { Direction::Backward } else { Direction::Forward })
        } {
            Ok(r) => r,
            // the GPU failed (lost, timed out): forget that model; MobileSAM goes on when there is one
            Err(MaskError::Failed(e)) => {
                crate::magic_mask_sam2::forget(plan.size);
                self.tracker = None;
                if self.source.is_some() {
                    self.note(format!("SAM 2.1 stopped ({e}); tracking with MobileSAM from {} on", self.at(frame)));
                    self.set_used(MOBILE_SAM.into());
                    self.video = None;
                    return Ok(None);
                }
                return Err(MaskError::Failed(e).to_string());
            }
            Err(e) => return Err(e.to_string()),
        };
        lock_status(&self.status).segmented += 1;
        let m = r.segment.matte(img.width(), img.height()).map_err(|e| e.to_string())?;
        // an empty matte (the subject is hidden) is a result: SAM 2.1 finds it again later
        Ok(Some(Got::Matte(r.segment, Arc::new(m), None)))
    }

    fn lost_at(&self, frame: usize) -> Got {
        Got::Lost(format!("lost the subject at {}: add a stroke there", self.at(frame)))
    }

    /// A stroke frame: segment it from its strokes.
    fn compute_seed(&mut self, frame: usize, prompt: &Prompt) -> std::result::Result<Got, String> {
        let seg = self.segmenter()?;
        let Some(img) = self.picture(frame) else { return Ok(Got::Lost(format!("the clip has no picture at {}", self.at(frame)))) };
        let e = seg.embed(&img).map_err(|e| e.to_string())?;
        let s = seg.segment(&e, prompt).map_err(|e| e.to_string())?;
        lock_status(&self.status).segmented += 1;
        let m = s.matte(img.width(), img.height()).map_err(|e| e.to_string())?;
        if m.bounds().is_none() {
            return Ok(self.lost_at(frame));
        }
        let track = Track::start(s.clone(), &img);
        Ok(Got::Matte(s, Arc::new(m), track))
    }

    /// The next frame of a direction: follow the subject from the previous frame.
    fn compute_follow(&mut self, frame: usize) -> std::result::Result<Got, String> {
        let seg = self.segmenter()?;
        let track = match self.walk.track.take() {
            Some(t) => t,
            // after a cached frame: learn the colours again on that frame's picture
            None => {
                let started = match (self.walk.prev.clone(), self.walk.prev_frame) {
                    (Some(prev), Some(f)) => self.picture(f).and_then(|img| Track::start(prev, &img)),
                    _ => None,
                };
                match started {
                    Some(t) => t,
                    None => return Ok(self.lost_at(frame)),
                }
            }
        };
        let Some(img) = self.picture(frame) else { return Ok(Got::Lost(format!("the clip has no picture at {}", self.at(frame)))) };
        let e = seg.embed(&img).map_err(|e| e.to_string())?;
        let followed = follow(&*seg, &e, &img, &track, &FollowOptions::default()).map_err(|e| e.to_string())?;
        lock_status(&self.status).segmented += 1;
        let Some((track, _)) = followed else { return Ok(self.lost_at(frame)) };
        let s = track.segment().clone();
        let m = s.matte(img.width(), img.height()).map_err(|e| e.to_string())?;
        if m.bounds().is_none() {
            return Ok(self.lost_at(frame));
        }
        Ok(Got::Matte(s, Arc::new(m), Some(track)))
    }

    fn next_phase(&mut self) {
        self.phase = match self.phase {
            Phase::Seed => Phase::Fwd(0),
            Phase::Fwd(i) => Phase::Fwd(i + 1),
            Phase::Back(i) => Phase::Back(i + 1),
        };
    }

    fn done_one(&self) {
        lock_status(&self.status).done += 1;
    }

    /// Advance until one frame was segmented (cached frames and held frames don't count).
    fn advance(&mut self) -> std::result::Result<JobStep, String> {
        loop {
            let Some(run) = self.runs.get(self.run) else { return Ok(JobStep::Done) };
            let pos = match self.phase {
                Phase::Seed => Some((run.seed, true)),
                Phase::Fwd(i) => run.fwd.get(i).map(|f| (*f, false)),
                Phase::Back(i) => run.back.get(i).map(|f| (*f, false)),
            };
            let (hash, owner_t, seed_owned, seed_frame) = (run.hash, run.owner, run.seed_owned, run.seed);
            let Some((frame, seed)) = pos else {
                if let Phase::Fwd(_) = self.phase {
                    // back from the seed
                    self.phase = Phase::Back(0);
                    self.walk.prev = self.walk.seed_seg.clone();
                    self.walk.prev_frame = Some(seed_frame);
                    self.walk.track = self.walk.seed_track.clone();
                    self.walk.last_good = self.walk.seed_matte.clone();
                    self.walk.lost = self.walk.seed_lost;
                    self.walk.held = 0;
                } else {
                    self.run += 1;
                    self.phase = Phase::Seed;
                    self.walk = Walk::default();
                }
                continue;
            };
            if self.superseded() {
                return Ok(JobStep::Done);
            }
            let cacheable = !seed || seed_owned;
            let t = self.src.get(frame).copied().unwrap_or(Tick::ZERO);
            if self.walk.lost {
                if self.walk.held < MAX_HELD_FRAMES {
                    if let Some(m) = self.walk.last_good.clone() {
                        let cover = self.walk.prev.as_ref().map_or((1.0, 1.0), |s| s.cover);
                        if !self.cache.insert(self.clip, self.node, t, hash, m, cover, true, self.generation) {
                            self.supersede();
                            return Ok(JobStep::Done);
                        }
                    }
                } else if self.walk.held == MAX_HELD_FRAMES {
                    self.note(format!("held the last good matte for {MAX_HELD_FRAMES} frames; no matte from {} on: add a stroke there", self.at(frame)));
                }
                self.walk.held = self.walk.held.saturating_add(1);
                self.done_one();
                self.next_phase();
                continue;
            }
            // (SAM 2.1 tracks every frame again: each one's memory carries the subject on; a
            // cached matte would leave a hole in it)
            if cacheable
                && self.video.is_none()
                && let Some((m, cover)) = self.cache.get_valid(self.clip, self.node, t, hash)
            {
                self.walk.prev = Some(segment_from(&m, cover));
                self.walk.prev_frame = Some(frame);
                self.walk.track = None;
                if seed {
                    self.walk.seed_seg = self.walk.prev.clone();
                    self.walk.seed_matte = Some(m.clone());
                    self.walk.seed_track = None;
                }
                self.walk.last_good = Some(m);
                {
                    let mut st = lock_status(&self.status);
                    st.done += 1;
                    st.reused += 1;
                }
                self.next_phase();
                continue;
            }
            let backward = matches!(self.phase, Phase::Back(_));
            #[cfg(feature = "magic-mask")]
            let video_got = if self.video.is_some() {
                // SAM 2.1: the seed's strokes enter its memory, other frames follow from it
                let prompt = if seed { self.runs.get(self.run).map(|r| r.prompt.clone()).unwrap_or_default() } else { Prompt::default() };
                self.compute_video(frame, &prompt, seed, backward)?
            } else {
                None
            };
            #[cfg(not(feature = "magic-mask"))]
            let video_got: Option<Got> = {
                let _ = backward;
                None
            };
            let got = if let Some(g) = video_got {
                g
            } else if seed {
                let prompt = self.runs.get(self.run).map(|r| r.prompt.clone()).unwrap_or_default();
                self.compute_seed(frame, &prompt)?
            } else if self.walk.prev.is_none() && self.walk.track.is_none() {
                self.note(format!("lost the subject at {}: add a stroke there", self.at(frame)));
                self.walk.lost = true;
                continue;
            } else {
                self.compute_follow(frame)?
            };
            match got {
                Got::Matte(s, m, track) => {
                    if cacheable && !self.cache.insert(self.clip, self.node, t, hash, m.clone(), s.cover, false, self.generation) {
                        self.supersede();
                        return Ok(JobStep::Done);
                    }
                    if seed {
                        self.walk.seed_seg = Some(s.clone());
                        self.walk.seed_matte = Some(m.clone());
                        self.walk.seed_track = track.clone();
                    }
                    self.walk.track = track;
                    self.walk.prev_frame = Some(frame);
                    self.walk.prev = Some(s);
                    self.walk.last_good = Some(m);
                    self.done_one();
                    self.next_phase();
                }
                Got::Lost(why) => {
                    if seed {
                        self.note(format!("the stroke at {:.2} s found nothing: {why}", owner_t.seconds()));
                        self.walk.seed_lost = true;
                    } else {
                        self.note(why);
                    }
                    // (this frame and the rest of the direction hold the last good matte)
                    self.walk.lost = true;
                    continue;
                }
            }
            return Ok(JobStep::More);
        }
    }

    /// The node was edited since the track was queued: end as [`TrackState::Superseded`].
    fn superseded(&mut self) -> bool {
        if self.cache.generation(self.clip, self.node) == self.generation {
            return false;
        }
        self.supersede();
        true
    }

    fn supersede(&mut self) {
        if !self.ended {
            self.note("the strokes changed while tracking: track again".into());
        }
        self.end(TrackState::Superseded, None);
    }

    fn end(&mut self, state: TrackState, error: Option<String>) {
        if self.ended {
            return;
        }
        self.ended = true;
        // (render-cache parts made before or during the track are stale now)
        self.cache.touch(self.clip, self.node);
        let mut st = lock_status(&self.status);
        st.state = state;
        st.error = error;
    }
}

impl BackgroundJob for TrackJob {
    fn step(&mut self) -> std::result::Result<JobStep, String> {
        lock_status(&self.status).state = TrackState::Running;
        let r = match catch_unwind(AssertUnwindSafe(|| self.advance())) {
            Ok(r) => r,
            Err(_) => Err("Magic Mask stopped on an internal error".to_string()),
        };
        match &r {
            Ok(JobStep::Done) => self.end(TrackState::Done, None),
            Ok(JobStep::More) => {}
            Err(e) => self.end(TrackState::Failed, Some(e.clone())),
        }
        r
    }

    fn fraction(&self) -> f32 {
        let st = lock_status(&self.status);
        if st.frames == 0 { 0.0 } else { (st.done as f32 / st.frames as f32).clamp(0.0, 1.0) }
    }

    fn cancelled(&mut self) {
        self.end(TrackState::Cancelled, None);
    }
}

impl Drop for TrackJob {
    fn drop(&mut self) {
        self.end(TrackState::Cancelled, None);
    }
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

type RunFn = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: RunFn) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_node, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("grade.magicMask.add", "Add Magic Mask", r#"{"clip":id?,"node":id?,"mode":"object"|"person"?}"#, add),
        spec(
            "grade.magicMask.stroke",
            "Magic Mask Stroke",
            r#"{"clip":id?,"node":id?,"time":ticks?,"points":[{"x":0..1,"y":0..1,"positive":bool?}],"rect":[x0,y0,x1,y1]?}"#,
            stroke,
        ),
        spec("grade.magicMask.clearStrokes", "Clear Magic Mask Strokes", r#"{"clip":id?,"node":id?,"time":ticks?}"#, clear_strokes),
        spec(
            "grade.magicMask.setRefine",
            "Refine Magic Mask",
            r#"{"clip":id?,"node":id?,"grow":num?,"soften":num?,"cleanBlack":0..1?,"cleanWhite":0..1?,"invert":bool?,"merge":str?}"#,
            set_refine,
        ),
        spec("grade.magicMask.remove", "Remove Magic Mask", r#"{"clip":id?,"node":id?}"#, remove),
        spec(
            "grade.magicMask.track",
            "Track Magic Mask",
            r#"{"clip":id?,"node":id?,"direction":"forward"|"backward"|"both"?,"fromTime":ticks?,"toTime":ticks?,"model":"auto"|"better"|"faster"|"mobileSam"?}"#,
            track,
        ),
        CommandSpec {
            id: "grade.magicMask.cancel",
            label: "Stop Magic Mask Tracking",
            menu: &[],
            shortcut: None,
            params: r#"{"job":id?}"#,
            enabled: has_seq,
            run: cancel,
            journal: false,
        },
        CommandSpec {
            id: "grade.magicMask.status",
            label: "Magic Mask Status",
            menu: &[],
            shortcut: None,
            params: r#"{"job":id?}"#,
            enabled: always,
            run: status,
            journal: false,
        },
    ]
}

/// A video clip with at least one grade node is selected (or named).
fn has_node(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    let c = s.state.selection.iter().copied().find(|c| is_video(q, *c)).or_else(|| s.state.grade_node.map(|g| g.clip)).ok_or("select a video clip")?;
    let has = q.find_item(c).and_then(|(_, it)| it.grade.as_ref()).is_some_and(|g| !g.nodes.is_empty());
    if has { Ok(()) } else { Err("the clip has no grade node: add one first".into()) }
}

/// Edit the addressed node's Magic Mask slot as one undo step (merged by `merge`). A change of
/// the strokes or mode stops a running track of the node; stale mattes stay cached (an undo shows
/// them again) until the byte cap drops them; a removed mask drops them. Returns the clip, the
/// node and `f`'s result.
fn edit_mask<R>(
    s: &mut Session,
    p: &Value,
    cmd: &'static str,
    label: &str,
    merge: Option<&str>,
    f: impl FnOnce(&mut Option<MagicMask>) -> Result<R>,
) -> Result<(ClipId, u64, R)> {
    let want = node_arg(p, cmd)?;
    let (c, current) = current_for(s, p, cmd)?;
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.insert("clip".into(), json!(c.0));
    }
    let (id, r, after) = edit_grade_merged(s, &q, cmd, label, false, merge, |g| {
        let id = pick(g, want, current, cmd)?;
        let n = g.node_mut(id).ok_or_else(|| bad(cmd, format!("no node {id}")))?;
        let key = |m: &Option<MagicMask>| m.as_ref().map(|m| (m.mode, m.strokes.clone()));
        let before = key(&n.magic_mask);
        let r = f(&mut n.magic_mask)?;
        if let Some(m) = n.magic_mask.take() {
            n.magic_mask = Some(m.cleaned());
        }
        let changed = key(&n.magic_mask) != before;
        Ok((id, r, (n.magic_mask.is_some(), changed)))
    })?;
    let (has, changed) = after;
    if changed {
        s.magic_mask.cache.bump(c, id);
    }
    if !has {
        s.magic_mask.cache.forget_node(c, id);
    }
    Ok((c, id, r))
}

fn need_mask<'a>(m: &'a mut Option<MagicMask>, cmd: &str) -> Result<&'a mut MagicMask> {
    m.as_mut().ok_or_else(|| bad(cmd, "the node has no Magic Mask: add one first (grade.magicMask.add)"))
}

fn mode_p(p: &Value, cmd: &str) -> Result<Option<MagicMaskMode>> {
    match p.get("mode") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_str() {
            Some("object") => Ok(Some(MagicMaskMode::Object)),
            Some("person") => Ok(Some(MagicMaskMode::Person)),
            _ => Err(bad(cmd, "`mode` must be \"object\" or \"person\"")),
        },
    }
}

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.magicMask.add";
    let mode = mode_p(p, CMD)?;
    let (c, id, _) = edit_mask(s, p, CMD, "Add Magic Mask", None, |m| {
        if m.is_some() {
            return Err(bad(CMD, "the node already has a Magic Mask"));
        }
        *m = Some(MagicMask { mode: mode.unwrap_or_default(), ..MagicMask::default() });
        Ok(())
    })?;
    Ok(json!({"clip": c.0, "node": id}))
}

/// A coordinate in 0…1 of the frame.
fn unit(v: Option<&Value>, what: &str, cmd: &str) -> Result<f32> {
    let x = v.and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("{what} must be a number (0…1 of the frame)")))?;
    if !(0.0..=1.0).contains(&x) {
        return Err(bad(cmd, format!("{what} is outside the frame: use 0…1 of the picture")));
    }
    Ok(x as f32)
}

fn stroke(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.magicMask.stroke";
    let points: Vec<StrokePoint> = match p.get("points") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => {
            if a.len() > MAX_STROKE_POINTS {
                return Err(bad(CMD, format!("a stroke has at most {MAX_STROKE_POINTS} points: send fewer (thin the stroke)")));
            }
            let mut v = Vec::with_capacity(a.len());
            for pt in a {
                if !pt.is_object() {
                    return Err(bad(CMD, "each point must be {\"x\": 0…1, \"y\": 0…1, \"positive\": true|false}"));
                }
                let positive = match pt.get("positive") {
                    None | Some(Value::Null) => true,
                    Some(b) => b.as_bool().ok_or_else(|| bad(CMD, "`positive` must be true or false"))?,
                };
                v.push(StrokePoint { x: unit(pt.get("x"), "`x`", CMD)?, y: unit(pt.get("y"), "`y`", CMD)?, positive });
            }
            v
        }
        Some(_) => return Err(bad(CMD, "`points` must be a list of {x, y} points")),
    };
    let rect = match p.get("rect") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) if a.len() == 4 => {
            let mut r = [0f32; 4];
            for (slot, v) in r.iter_mut().zip(a) {
                *slot = unit(Some(v), "a `rect` corner", CMD)?;
            }
            Some(r)
        }
        Some(_) => return Err(bad(CMD, "`rect` must be [x0, y0, x1, y1] (0…1 of the frame)")),
    };
    if points.is_empty() && rect.is_none() {
        return Err(bad(CMD, "need `points` (on the subject) or a `rect` around it"));
    }
    let (c, _) = current_for(s, p, CMD)?;
    let tl = time_p(s, p, "").unwrap_or_else(|| s.playhead());
    let inside = s.active_sequence().and_then(|q| q.find_item(c)).is_some_and(|(_, it)| tl >= it.start && tl < it.end());
    if !inside {
        return Err(bad(CMD, "that time is outside the clip: draw the stroke on a frame of the clip (move the playhead onto it)"));
    }
    let t = media_time(s, p, c);
    let (c, id, n) = edit_mask(s, p, CMD, "Magic Mask Stroke", None, |m| {
        let mask = m.get_or_insert_with(MagicMask::default);
        if mask.strokes.len() >= MAX_STROKES {
            return Err(bad(CMD, format!("the Magic Mask has {MAX_STROKES} strokes, the most it keeps: clear some first")));
        }
        mask.strokes.push(MaskStroke { time: t, points, rect });
        Ok(mask.strokes.len())
    })?;
    Ok(json!({"clip": c.0, "node": id, "time": t.0, "strokes": n}))
}

fn has_time(p: &Value) -> bool {
    ["time", "frame", "seconds", "timecode"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()))
}

fn clear_strokes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.magicMask.clearStrokes";
    let at = if has_time(p) {
        let (c, _) = current_for(s, p, CMD)?;
        if time_p(s, p, "").is_none() {
            return Err(bad(CMD, "the time must be ticks (`time`), a `frame`, `seconds` or a `timecode`"));
        }
        Some(media_time(s, p, c))
    } else {
        None
    };
    let (c, id, removed) = edit_mask(s, p, CMD, "Clear Magic Mask Strokes", None, |m| {
        let mask = need_mask(m, CMD)?;
        let before = mask.strokes.len();
        match at {
            Some(t) => mask.strokes.retain(|st| st.time != t),
            None => mask.strokes.clear(),
        }
        Ok(before - mask.strokes.len())
    })?;
    Ok(json!({"clip": c.0, "node": id, "removed": removed}))
}

fn set_refine(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.magicMask.setRefine";
    let num = |k: &str| -> Result<Option<f32>> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_f64().filter(|f| f.is_finite()).map(|f| Some(f as f32)).ok_or_else(|| bad(CMD, format!("`{k}` must be a number"))),
        }
    };
    let (grow, soften, cb, cw) = (num("grow")?, num("soften")?, num("cleanBlack")?, num("cleanWhite")?);
    let invert = match p.get("invert") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| bad(CMD, "`invert` must be true or false"))?),
    };
    if grow.is_none() && soften.is_none() && cb.is_none() && cw.is_none() && invert.is_none() {
        return Err(bad(CMD, "need one of `grow`, `soften`, `cleanBlack`, `cleanWhite`, `invert`"));
    }
    let merge = str_p(p, "merge").map(str::to_string);
    let (c, id, r) = edit_mask(s, p, CMD, "Refine Magic Mask", merge.as_deref(), |m| {
        let mask = need_mask(m, CMD)?;
        let mut r = mask.refine;
        if let Some(v) = grow {
            r.grow = v;
        }
        if let Some(v) = soften {
            r.soften = v;
        }
        if let Some(v) = cb {
            r.clean_black = v;
        }
        if let Some(v) = cw {
            r.clean_white = v;
        }
        if let Some(v) = invert {
            r.invert = v;
        }
        mask.refine = r.cleaned();
        Ok(mask.refine)
    })?;
    Ok(json!({"clip": c.0, "node": id, "refine": r}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.magicMask.remove";
    let (c, id, _) = edit_mask(s, p, CMD, "Remove Magic Mask", None, |m| m.take().map(|_| ()).ok_or_else(|| bad(CMD, "the node has no Magic Mask")))?;
    for j in s.magic_mask.live_jobs(Some((c, id))) {
        s.background.cancel(j);
    }
    s.magic_mask.cache.invalidate_node(c, id);
    Ok(json!({"clip": c.0, "node": id}))
}

/// Timeline / clip time of the clip's frames a track of `lo..hi` (timeline) needs: the range and
/// the frames up to the stroke frames that own its frames (a run starts at its stroke frame;
/// `strokes`: every stroke time, sorted), consecutive repeats of a clip time dropped (frame
/// holds). Only the range and [`MAX_TRACK_FRAMES`] frames either side of it are scanned (cheaply,
/// no picture); a track needing more than [`MAX_TRACK_FRAMES`] frames is refused.
fn clip_frames(
    it: &filmcraft_project::TrackItem,
    rate: filmcraft_time::FrameRate,
    lo: Tick,
    hi: Tick,
    strokes: &[Tick],
    cmd: &str,
) -> Result<(Vec<Tick>, Vec<Tick>)> {
    let (start, end) = (it.start, it.end());
    let (lo, hi) = (lo.max(start), hi.min(end));
    if hi <= lo {
        return Ok((Vec::new(), Vec::new()));
    }
    let too_long = || bad(cmd, format!("the clip is too long to track in one go (at most {MAX_TRACK_FRAMES} frames between the strokes and the range)"));
    let (f0, f1) = (rate.frame_at(start), rate.frame_at(Tick(end.0.saturating_sub(1))));
    let (r0, r1) = (rate.frame_at(lo).max(f0), rate.frame_at(Tick(hi.0.saturating_sub(1))).min(f1));
    if r1.saturating_sub(r0) >= MAX_TRACK_FRAMES {
        return Err(too_long());
    }
    // the range and up to MAX_TRACK_FRAMES either side: (timeline, clip time)
    let (s0, s1) = (r0.saturating_sub(MAX_TRACK_FRAMES).max(f0), r1.saturating_add(MAX_TRACK_FRAMES).min(f1));
    let mut scan: Vec<(Tick, Tick)> = Vec::new();
    for f in s0..=s1 {
        let t = rate.tick_of(f).max(start);
        if t >= end {
            break;
        }
        scan.push((t, it.effect_time_at(t)));
    }
    // the stroke times owning the range's frames, and their stroke frames
    let mut need: Vec<Tick> = scan.iter().filter(|(t, _)| *t >= lo && *t < hi).filter_map(|(_, m)| owner(strokes, *m)).collect();
    need.sort();
    need.dedup();
    let (mut a, mut b) = (lo, Tick(hi.0.saturating_sub(1)));
    for st in need {
        if let Some((t, _)) = scan.iter().min_by_key(|(_, m)| m.0.abs_diff(st.0)) {
            a = a.min(*t);
            b = b.max(*t);
        }
    }
    let (mut tl, mut src) = (Vec::new(), Vec::new());
    for (t, m) in scan.into_iter().filter(|(t, _)| *t >= a && *t <= b) {
        if src.last() == Some(&m) {
            continue;
        }
        if tl.len() as i64 >= MAX_TRACK_FRAMES {
            return Err(too_long());
        }
        tl.push(t);
        src.push(m);
    }
    Ok((tl, src))
}

/// The runs of `mask` over frames with clip times `src`, limited to the frames `wanted` and the
/// `fwd` / `back` directions.
fn plan(mask: &MagicMask, picture: u64, src: &[Tick], wanted: &[bool], fwd: bool, back: bool) -> Vec<Run> {
    let times = stroke_times(mask);
    let owners: Vec<Option<Tick>> = src.iter().map(|t| owner(&times, *t)).collect();
    let mut runs = Vec::new();
    for o in times {
        let Some(seed) = src.iter().enumerate().min_by_key(|(_, t)| t.0.abs_diff(o.0)).map(|(i, _)| i) else { continue };
        let owned = |i: usize| owners.get(i).copied().flatten() == Some(o);
        let want = |i: usize| wanted.get(i).copied().unwrap_or(false);
        let mut f: Vec<usize> = Vec::new();
        if fwd {
            f = (seed + 1..src.len()).take_while(|i| owned(*i)).collect();
            let last = f.iter().rposition(|i| want(*i)).map_or(0, |p| p + 1);
            f.truncate(last);
        }
        let mut b: Vec<usize> = Vec::new();
        if back {
            b = (0..seed).rev().take_while(|i| owned(*i)).collect();
            let last = b.iter().rposition(|i| want(*i)).map_or(0, |p| p + 1);
            b.truncate(last);
        }
        if !(want(seed) && owned(seed)) && f.is_empty() && b.is_empty() {
            continue;
        }
        runs.push(Run { owner: o, hash: owner_hash(mask, o, picture), prompt: stroke_prompt(mask, o), seed, seed_owned: owned(seed), fwd: f, back: b });
    }
    runs
}

fn track(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.magicMask.track";
    let (fwd, back) = match str_p(p, "direction") {
        None | Some("both") => (true, true),
        Some("forward") => (true, false),
        Some("backward") => (false, true),
        Some(d) => return Err(bad(CMD, format!("unknown direction `{d}` (forward, backward or both)"))),
    };
    if p.get("direction").is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Err(bad(CMD, "`direction` must be \"forward\", \"backward\" or \"both\""));
    }
    let want = node_arg(p, CMD)?;
    let (c, current) = current_for(s, p, CMD)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let from = time_p(s, p, "from");
    let to = time_p(s, p, "to");
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if !is_video(q, c) {
        return Err(bad(CMD, "Magic Mask needs a clip on a video track"));
    }
    let rate = q.settings.frame_rate;
    let (w, h) = (q.settings.width as usize, q.settings.height as usize);
    let (_, it) = q.find_item(c).ok_or_else(|| bad(CMD, "no such clip"))?;
    if s.project.item(it.item).is_some_and(|pi| matches!(pi.kind, filmcraft_project::ItemKind::AdjustmentLayer { .. })) {
        return Err(bad(CMD, "Magic Mask doesn't work on adjustment layers"));
    }
    let g = it.grade.as_ref().ok_or_else(|| bad(CMD, "the clip has no grade"))?;
    let id = pick(g, want, current, CMD)?;
    let node = g.node(id).ok_or_else(|| bad(CMD, format!("no node {id}")))?;
    let mask = node.magic_mask.clone().ok_or_else(|| bad(CMD, "the node has no Magic Mask: add one first (grade.magicMask.add)"))?;
    if mask.strokes.is_empty() {
        return Err(bad(CMD, "draw a stroke on the subject first (grade.magicMask.stroke)"));
    }
    let (lo, hi) = (from.unwrap_or(it.start), to.unwrap_or(it.end()));
    if hi <= lo {
        return Err(bad(CMD, "the range is empty: `to` must be after `from`"));
    }
    let (tl, src) = clip_frames(it, rate, lo, hi, &stroke_times(&mask), CMD)?;
    let wanted: Vec<bool> = tl.iter().map(|t| *t >= lo && *t < hi).collect();
    if !wanted.iter().any(|w| *w) {
        return Err(bad(CMD, "the range doesn't overlap the clip"));
    }
    let picture = picture_fingerprint(&s.project, it);
    let runs = plan(&mask, picture, &src, &wanted, fwd, back);
    let frames: usize = runs.iter().map(|r| 1 + r.fwd.len() + r.back.len()).sum();
    let model = match p.get("model") {
        None | Some(Value::Null) => s.magic_mask.model,
        Some(Value::String(m)) => MaskModel::parse(m).ok_or_else(|| bad(CMD, format!("unknown model `{m}` (auto, better, faster or mobileSam)")))?,
        Some(_) => return Err(bad(CMD, "`model` must be \"auto\", \"better\", \"faster\" or \"mobileSam\"")),
    };
    let video = s.magic_mask.video_plan(model, CMD)?;
    let source = match &video {
        // SAM 2.1 chosen: MobileSAM only as its fallback, when installed
        Some(_) => s.magic_mask.source(CMD).ok(),
        None => Some(s.magic_mask.source(CMD)?),
    };
    // a different model than this node's mattes came from: track every frame again
    let tag = video.as_ref().map_or_else(|| "mobileSam".to_string(), |v| format!("{:?}", v.size));
    {
        let mut used = s.magic_mask.used.lock().unwrap_or_else(PoisonError::into_inner);
        if used.insert((c.0, id), tag.clone()).is_some_and(|old| old != tag) {
            s.magic_mask.cache.invalidate_node(c, id);
        }
    }
    // the clip's own picture: no grade, no effects but the intrinsic ones (motion)
    let mut probe = (*s.project).clone();
    if let Some((_, pi)) = probe.sequence_mut(seq_id).and_then(|q| q.find_item_mut(c)) {
        pi.grade = None;
        for e in pi.effects.iter_mut().filter(|e| !e.def().is_some_and(|d| d.intrinsic)) {
            e.enabled = false;
        }
    }
    let long = w.max(h).max(1);
    let scale = (MAX_PREVIEW_SIDE as f32 / long as f32).min(1.0);
    let step = match (src.first(), src.get(1)) {
        (Some(a), Some(b)) => b.0.saturating_sub(a.0).saturating_abs(),
        _ => 0,
    };
    // one track per node at a time
    for j in s.magic_mask.live_jobs(Some((c, id))) {
        s.background.cancel(j);
    }
    let cache = s.magic_mask.cache.clone();
    cache.set_step(c, id, step);
    let generation = cache.claim_generation(c, id);
    s.previews.set_mattes(cache.clone());
    let used = video.as_ref().map_or_else(|| MOBILE_SAM.to_string(), |v| v.size.label().to_string());
    let status: SharedStatus = Arc::new(Mutex::new(TrackStatus { clip: c.0, node: id, frames, used, ..TrackStatus::default() }));
    let job = TrackJob {
        project: Arc::new(probe),
        seq: seq_id,
        clip: c,
        node: id,
        provider: s.media.provider(s.project.clone(), s.services.clone()),
        scale,
        tl,
        src,
        runs,
        run: 0,
        phase: Phase::Seed,
        walk: Walk::default(),
        cache,
        source,
        segmenter: None,
        video,
        #[cfg(feature = "magic-mask")]
        tracker: None,
        status: status.clone(),
        ended: false,
        generation,
    };
    let spec = JobSpec::new(JobKind::Mask, format!("Magic Mask: clip {} node {id}", c.0));
    // (registered before the job can run, so its status is never missing)
    let mut st = lock_status(&status);
    let job_id = s.background.submit(spec, Box::new(job)).map_err(EngineError::Other)?;
    st.job = job_id;
    drop(st);
    s.magic_mask.add_track(status);
    Ok(json!({"clip": c.0, "node": id, "job": job_id, "frames": frames}))
}

pub(crate) fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.magicMask.cancel";
    let jobs = match p.get("job") {
        None | Some(Value::Null) => s.magic_mask.live_jobs(None),
        Some(v) => {
            let j = v.as_u64().ok_or_else(|| bad(CMD, "`job` must be a job id"))?;
            if !s.magic_mask.live_jobs(None).contains(&j) {
                return Err(bad(CMD, format!("no Magic Mask track {j} is running")));
            }
            vec![j]
        }
    };
    let n = jobs.into_iter().filter(|j| s.background.cancel(*j)).count();
    Ok(json!({"cancelled": n}))
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.magicMask.status";
    let job = match p.get("job") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or_else(|| bad(CMD, "`job` must be a job id"))?),
    };
    let tracks = s.magic_mask.statuses(job);
    Ok(json!({
        "tracks": tracks,
        "cacheBytes": s.magic_mask.cache.bytes(),
        "model": s.magic_mask.model_path().map(|p| p.display().to_string()),
        "tracker": s.magic_mask.model,
        "sam2": s.magic_mask.sam2_dir().map(|p| p.display().to_string()),
        "available": s.magic_mask.segmenter.is_some() || cfg!(feature = "magic-mask"),
    }))
}
