//! Temporal noise reduction in the renderer (fork 6.7, integration): which neighbouring frames a
//! clip's [`FxOp::TemporalDenoise`] blends, their motion, and attaching them to the op.
//!
//! The op comes from the clip (`grade::temporal`: `TrackItem::temporal_nr`, fork 6.7b; it was the
//! first active grade node's `tnr.*` before schema v17) with no inputs. A render with temporal access
//! ([`crate::RenderOptions::temporal`]: export and so the render cache, the paused Program frame)
//! attaches them here; every other render strips the op ([`crate::grade::strip_temporal`]).
//!
//! **Neighbours.** The clip's frames at timeline `t ± k·frame` (k ≤ radius, the sequence's frame),
//! inside the item only (never across a cut or past a trim), each mapped through the item's
//! speed / reverse / hold (`video_source_time`) and snapped to its source frame; a source frame
//! already taken (the current one, or an earlier neighbour: holds, slow motion) is skipped, so a
//! hold collapses to no neighbour at all (identity). Offsets are ranks: the nearest distinct frame
//! on a side is ±1, the next ±2. Media and subclip items only.
//!
//! **Motion.** Dense pyramidal Lucas–Kanade ([`crate::flow`]) on the source frames' flow luma at
//! ≤ [`flow::WORK_W`] px, estimated both ways for each adjacent pair (the current frame and ±1,
//! ±1 and ±2) and cached ([`FlowCache`]: bounded LRU, keyed by the media's identity, the two source
//! frames and the frame size); ±2 chains the two steps (`flow::chain`). The fields are relabelled
//! for the working image (`FlowField::for_image`). The work runs wherever the frame renders: an
//! export / render-cache worker, a frame-server worker; never the UI thread.
//!
//! **Pictures.** The GPU path ([`attach_frames`]) hands over the neighbours' source frames
//! ([`NeighbourPicture::Frame`]: the GPU decodes and replays the layer's earlier ops on them); the
//! CPU path ([`attach_images`]) their decoded, colour-managed images (`base_layer`, the state the
//! op sees: it runs first in the grade, before any other grade op).

use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use filmcraft_frame::VideoFrame;
use filmcraft_media::timing::FrameTimes;
use filmcraft_media::{MediaSource, SharedSource};
use filmcraft_project::{ItemKind, Project, Sequence, TrackItem};
use filmcraft_time::{FrameRate, Tick};

use crate::flow::{self, FlowField};
use crate::gpufx::{FxOp, NeighbourPicture, TemporalInputs, TemporalNeighbour};
use crate::track::Gray;
use crate::{Image, RenderOptions, SourceProvider};

/// One neighbouring frame of a clip at a timeline time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeighbourTime {
    /// ±rank (the nearest distinct source frame on a side is ±1).
    pub offset: i8,
    /// Timeline time of the neighbour.
    pub timeline: Tick,
    /// Media time to ask the source for (`video_source_time`).
    pub source: Tick,
    /// Its source frame (at the media rate).
    pub frame: i64,
}

/// The source frame a clip shows at timeline `t` (None: the media has no usable frame rate).
/// `media_rate` is the source's frame clock (a frame rate, or a variable-frame-rate source's
/// [`filmcraft_media::timing::FrameClock`]), so frames are told apart by their own times.
pub fn source_frame_at(item: &TrackItem, t: Tick, media_rate: impl FrameTimes + Copy) -> Option<(Tick, i64)> {
    if !media_rate.usable() {
        return None;
    }
    let ft = crate::video_source_time(item, t, media_rate);
    Some((ft, media_rate.frame_at(ft)))
}

/// The neighbours of `item` at timeline `t` for a temporal radius (frames each side, at most
/// [`crate::tnr::MAX_RADIUS`]): see the module docs. Ordered by distance (±1 before ±2), at most
/// [`crate::tnr::MAX_NEIGHBOURS`]. Empty for a hold, a clip one frame long, a sequence or media
/// without a usable frame rate.
pub fn neighbour_times(item: &TrackItem, t: Tick, seq_rate: FrameRate, media_rate: impl FrameTimes + Copy, radius: u8) -> Vec<NeighbourTime> {
    neighbour_times_in(item, (item.start, item.end()), t, seq_rate, media_rate, radius)
}

/// The timeline span whose frames are `item`'s at `t`: the item, extended by a transition it is
/// part of when `t` lies inside that transition (an outgoing clip shows its handle frames after
/// its end, an incoming one before its start: the same media, so they are neighbours too).
pub fn clip_bounds(seq: &Sequence, item: &TrackItem, t: Tick) -> (Tick, Tick) {
    let (mut a, mut b) = (item.start, item.end());
    for tr in &seq.video_tracks {
        for x in tr.transitions.iter().filter(|x| (x.from == Some(item.id) || x.to == Some(item.id)) && x.start <= t && t < x.end()) {
            a = a.min(x.start);
            b = b.max(x.end());
        }
    }
    (a, b)
}

/// [`neighbour_times`] within the timeline span `bounds` (start inclusive, end exclusive; see
/// [`clip_bounds`]) instead of the item's own range.
pub fn neighbour_times_in(
    item: &TrackItem,
    bounds: (Tick, Tick),
    t: Tick,
    seq_rate: FrameRate,
    media_rate: impl FrameTimes + Copy,
    radius: u8,
) -> Vec<NeighbourTime> {
    let radius = crate::tnr::radius(radius);
    let step = seq_rate.frame_duration().0;
    // (a time outside the span has no frame of the clip to start from)
    if step <= 0 || t < bounds.0 || t >= bounds.1 {
        return Vec::new();
    }
    let Some((_, current)) = source_frame_at(item, t, media_rate) else { return Vec::new() };
    let mut seen = vec![current];
    let mut sides: [Vec<NeighbourTime>; 2] = [Vec::new(), Vec::new()];
    for (side, out) in [1i64, -1].into_iter().zip(sides.iter_mut()) {
        let mut rank: i8 = 0;
        for k in 1..=i64::from(radius) {
            let Some(tk) = step.checked_mul(k * side).and_then(|d| t.0.checked_add(d)).map(Tick) else { break };
            // inside the item only: never across a cut, never past the clip's own frames
            if tk < bounds.0 || tk >= bounds.1 {
                break;
            }
            let Some((ft, f)) = source_frame_at(item, tk, media_rate) else { break };
            if seen.contains(&f) {
                continue;
            }
            seen.push(f);
            rank = rank.saturating_add(1);
            out.push(NeighbourTime { offset: rank.saturating_mul(side as i8), timeline: tk, source: ft, frame: f });
        }
    }
    let [fwd, back] = sides;
    let mut all: Vec<NeighbourTime> = fwd.into_iter().chain(back).collect();
    all.sort_by_key(|n| (n.offset.unsigned_abs(), n.offset < 0));
    all.truncate(crate::tnr::MAX_NEIGHBOURS);
    all
}

// ------------------------------------------------------------------------------- flow cache

/// The flow luma of one source frame and what it was taken from.
#[derive(Debug)]
struct Luma {
    gray: Gray,
    /// Size of the (decimated) image the luma was taken from: the flows' image-pixel frame.
    size: (usize, usize),
    /// Image pixels per luma pixel.
    scale: f32,
}

/// A tiny least-recently-used map (linear scans: a few dozen entries).
struct Lru<K, V> {
    cap: usize,
    clock: u64,
    entries: Vec<(K, V, u64)>,
}

impl<K: PartialEq, V: Clone> Lru<K, V> {
    const fn new(cap: usize) -> Self {
        Self { cap, clock: 0, entries: Vec::new() }
    }

    fn get(&mut self, k: &K) -> Option<V> {
        self.clock = self.clock.wrapping_add(1);
        let now = self.clock;
        self.entries.iter_mut().find(|(key, _, _)| key == k).map(|(_, v, used)| {
            *used = now;
            v.clone()
        })
    }

    fn insert(&mut self, k: K, v: V) {
        self.clock = self.clock.wrapping_add(1);
        if let Some(e) = self.entries.iter_mut().find(|(key, _, _)| *key == k) {
            e.1 = v;
            e.2 = self.clock;
            return;
        }
        if self.entries.len() >= self.cap.max(1)
            && let Some(oldest) = self.entries.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i)
        {
            self.entries.swap_remove(oldest);
        }
        self.entries.push((k, v, self.clock));
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

/// (media identity, source frame, frame width, frame height)
type LumaKey = (u64, i64, u32, u32);
/// (media identity, from frame, to frame, frame width, frame height)
type PairKey = (u64, i64, i64, u32, u32);
type Pair = (Arc<FlowField>, Arc<FlowField>);

/// The flow cache: lumas per source frame, flows per adjacent pair (both directions), and the
/// pairs being estimated right now (another worker wanting one waits for it instead of
/// estimating it again). Lumas are at most 640 px wide, so a luma is up to ~1.1 MB and a pair up
/// to ~4.4 MB (two fields of 8 bytes per luma pixel; ~1.6 MB at 6.2K, 389 px wide): with the
/// largest bounds ([`cache_bounds`]) at most ~45 MB of lumas and ~140 MB of pairs.
struct FlowCache {
    lumas: Lru<LumaKey, Arc<Luma>>,
    pairs: Lru<PairKey, Pair>,
    in_flight: Vec<PairKey>,
}

/// (lumas, pairs) the cache keeps: enough for ±2 around a frame per concurrent render (5 lumas,
/// 4 pairs each) for the machine's threads, within 10…40 lumas and 8…32 pairs.
pub fn cache_bounds() -> (usize, usize) {
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);
    ((5 * workers).clamp(10, 40), (4 * workers).clamp(8, 32))
}

static CACHE: LazyLock<Mutex<FlowCache>> = LazyLock::new(|| {
    let (lumas, pairs) = cache_bounds();
    Mutex::new(FlowCache { lumas: Lru::new(lumas), pairs: Lru::new(pairs), in_flight: Vec::new() })
});

/// Signalled when an estimation in flight ends.
static ESTIMATION_DONE: std::sync::Condvar = std::sync::Condvar::new();

fn cache() -> std::sync::MutexGuard<'static, FlowCache> {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Empty the flow cache (tests, memory pressure).
pub fn clear_flow_cache() {
    let mut c = cache();
    c.lumas.clear();
    c.pairs.clear();
}

/// Entries in the flow cache: (lumas, pairs).
pub fn flow_cache_len() -> (usize, usize) {
    let c = cache();
    (c.lumas.entries.len(), c.pairs.entries.len())
}

thread_local! {
    /// Pairs this thread estimated (both directions count once): tests prove a cache hit.
    static ESTIMATED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Pairs the calling thread has estimated so far (not taken from the cache).
pub fn pairs_estimated() -> u64 {
    ESTIMATED.with(|e| e.get())
}

/// Whether the flows between source frames `a` and `b` (either way) of `media` at a frame size
/// are cached.
pub fn pair_cached(media: u64, a: i64, b: i64, size: (u32, u32)) -> bool {
    let c = cache();
    c.pairs.entries.iter().any(|(k, _, _)| *k == (media, a, b, size.0, size.1) || *k == (media, b, a, size.0, size.1))
}

/// What identifies a clip's media for the cache: the project item, the media it resolves to
/// (through subclips) with its content identity (`MediaClip::identity`, the fingerprint) and, for
/// a file, its size and modification time on disk (`preview::file_identity`: a file re-exported
/// in place is another picture), and what the source says about itself.
pub fn media_key(project: &Project, item: &TrackItem, src: &dyn MediaSource) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    item.item.hash(&mut h);
    let mut id = item.item;
    for _ in 0..crate::MAX_NEST_DEPTH {
        match project.item(id).map(|p| &p.kind) {
            Some(ItemKind::Subclip { parent, .. }) => id = *parent,
            Some(ItemKind::Media(m)) => {
                serde_json::to_string(&m.media).unwrap_or_default().hash(&mut h);
                serde_json::to_string(&m.identity).unwrap_or_default().hash(&mut h);
                if let filmcraft_project::MediaRef::File { path } = &m.media {
                    crate::preview::file_identity(path).hash(&mut h);
                }
                break;
            }
            _ => break,
        }
    }
    let info = src.info();
    info.name.hash(&mut h);
    info.file_size.hash(&mut h);
    info.duration.0.hash(&mut h);
    info.container.hash(&mut h);
    if let Some(v) = &info.video {
        (v.width, v.height).hash(&mut h);
    }
    h.finish()
}

/// The flow luma of source frame `f` (cached).
fn frame_luma(media: u64, f: i64, frame: &VideoFrame) -> Option<Arc<Luma>> {
    let key = (media, f, frame.width, frame.height);
    if let Some(l) = cache().lumas.get(&key) {
        return Some(l);
    }
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    // (a GPU picture is read back here: once per frame, then the luma is cached)
    let d = crate::decimation(frame.width as f32, flow::WORK_W as f32);
    let (w, h, px) = frame.to_linear_f32_decimated(d);
    let img = Image { w, h, px };
    let (gray, s) = flow::flow_luma(&img);
    filmcraft_frame::pool::recycle_f32(img.px);
    let l = Arc::new(Luma { gray, size: (w, h), scale: s as f32 });
    cache().lumas.insert(key, l.clone());
    Some(l)
}

/// Flows `a → b` and `b → a` between two source frames (cached both ways).
fn pair(media: u64, size: (u32, u32), (a, la): (i64, &Luma), (b, lb): (i64, &Luma)) -> Option<Pair> {
    let key = (media, a, b, size.0, size.1);
    let swapped = (media, b, a, size.0, size.1);
    {
        let mut c = cache();
        loop {
            if let Some(p) = c.pairs.get(&key) {
                return Some(p);
            }
            if let Some((f, g)) = c.pairs.get(&swapped) {
                return Some((g, f));
            }
            if !c.in_flight.iter().any(|k| *k == key || *k == swapped) {
                break;
            }
            if filmcraft_media::cancel::cancelled() {
                return None;
            }
            // another worker is estimating this pair: wait for it (its result is then cached;
            // if it gave up, this one estimates it). The timeout re-checks cancellation.
            c = ESTIMATION_DONE.wait_timeout(c, std::time::Duration::from_millis(50)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
        c.in_flight.push(key);
    }
    // (taken out of the in-flight list on every exit, waking the waiters)
    struct InFlight(PairKey);
    impl Drop for InFlight {
        fn drop(&mut self) {
            cache().in_flight.retain(|k| *k != self.0);
            ESTIMATION_DONE.notify_all();
        }
    }
    let _guard = InFlight(key);
    if (la.gray.w, la.gray.h) != (lb.gray.w, lb.gray.h) || la.size != lb.size {
        return None;
    }
    let f = flow::estimate_luma(la.gray.clone(), lb.gray.clone(), la.scale)?;
    if filmcraft_media::cancel::cancelled() {
        return None;
    }
    let g = flow::estimate_luma(lb.gray.clone(), la.gray.clone(), lb.scale)?;
    ESTIMATED.with(|e| e.set(e.get().saturating_add(1)));
    let p = (Arc::new(f), Arc::new(g));
    cache().pairs.insert(key, p.clone());
    Some(p)
}

/// One fetched neighbour: when it is and its source frame.
struct Fetched<P> {
    time: NeighbourTime,
    frame: Arc<VideoFrame>,
    picture: P,
}

/// Flows for every neighbour (current → neighbour, neighbour → current), relabelled for a
/// working image of `working` pixels; a neighbour whose flow can't be made (and the farther ones
/// on its side) is left out.
fn neighbours<P: Clone>(
    media: u64,
    current: (i64, &Arc<VideoFrame>),
    fetched: Vec<Fetched<P>>,
    working: (usize, usize),
) -> Vec<(i8, P, Arc<FlowField>, Arc<FlowField>)> {
    let (cur_f, cur) = current;
    let size = (cur.width, cur.height);
    let Some(cur_luma) = frame_luma(media, cur_f, cur) else { return Vec::new() };
    let mut out = Vec::new();
    for side in [1i8, -1] {
        let mut on_side: Vec<&Fetched<P>> = fetched.iter().filter(|n| n.time.offset.signum() == side).collect();
        on_side.sort_by_key(|n| n.time.offset.unsigned_abs());
        let (mut prev_f, mut prev_luma) = (cur_f, cur_luma.clone());
        let mut acc: Option<(FlowField, FlowField)> = None;
        for n in on_side {
            if filmcraft_media::cancel::cancelled() || (n.frame.width, n.frame.height) != size {
                break;
            }
            let Some(luma) = frame_luma(media, n.time.frame, &n.frame) else { break };
            let Some((fwd, bwd)) = pair(media, size, (prev_f, &prev_luma), (n.time.frame, &luma)) else { break };
            let next = match &acc {
                None => Some((FlowField::clone(&fwd), FlowField::clone(&bwd))),
                // t → t+2 = (t → t+1) then (t+1 → t+2); back: (t+2 → t+1) then (t+1 → t)
                Some((f1, b1)) => flow::chain(f1, &fwd).zip(flow::chain(&bwd, b1)),
            };
            let Some((flow_, back)) = next else { break };
            let relabel = |f: &FlowField| Arc::new(f.for_image(luma.size, working));
            out.push((n.time.offset, n.picture.clone(), relabel(&flow_), relabel(&back)));
            acc = Some((flow_, back));
            prev_f = n.time.frame;
            prev_luma = luma;
        }
    }
    out.sort_by_key(|n| (n.0.unsigned_abs(), n.0 < 0));
    out
}

/// The neighbours of `item` at `t` in `seq` whose pictures are source frames: a frame that Frame
/// Blending / Optical Flow makes between two source frames (a retimed clip) is left out, as the
/// flows are estimated between source frames; when the frame at `t` itself is such a frame, none
/// (temporal NR doesn't run on interpolated frames).
fn timed_neighbours(seq: &Sequence, item: &TrackItem, t: Tick, rate: impl FrameTimes + Copy, radius: u8) -> Vec<NeighbourTime> {
    if crate::interpolation_blend(item, t, rate).is_some() {
        return Vec::new();
    }
    neighbour_times_in(item, clip_bounds(seq, item, t), t, seq.settings.frame_rate, rate, radius)
        .into_iter()
        .filter(|n| crate::interpolation_blend(item, n.timeline, rate).is_none())
        .collect()
}

/// The radius of the temporal op in `ops` (also inside masked steps), if there is one.
fn temporal_radius(ops: &[FxOp]) -> Option<u8> {
    ops.iter().find_map(|o| match o {
        FxOp::TemporalDenoise { radius, .. } => Some(*radius),
        FxOp::Masked { ops, .. } => temporal_radius(ops),
        _ => None,
    })
}

/// Give every temporal op in `ops` these inputs.
fn set_inputs(ops: &mut [FxOp], inputs: &Arc<TemporalInputs>) {
    for o in ops {
        match o {
            FxOp::TemporalDenoise { inputs: i, .. } => *i = Some(inputs.clone()),
            FxOp::Masked { ops, .. } => set_inputs(ops, inputs),
            _ => {}
        }
    }
}

/// Whether `item` at `t` may run temporal NR in a render with `opts` (media / subclip, effects
/// on, a temporal render); also the radius asked for.
fn wanted(ops: &[FxOp], project: &Project, item: &TrackItem, opts: RenderOptions) -> Option<u8> {
    let media = project.item(item.item).is_some_and(|p| matches!(p.kind, ItemKind::Media(_) | ItemKind::Subclip { .. }));
    if !(opts.temporal && opts.effects && media) || filmcraft_media::cancel::cancelled() {
        return None;
    }
    temporal_radius(ops)
}

/// Finish the ops of a clip's GPU effect stage: with a temporal render, attach the neighbouring
/// source frames (`prep` turns a fetched frame into what the layer draws, e.g. its signal for an
/// input transform) and their flows for a working image of `working` pixels, decimated by
/// `decimation`; otherwise (or with no usable neighbour) remove the temporal op.
#[allow(clippy::too_many_arguments)]
pub(crate) fn attach_frames(
    ops: &mut Vec<FxOp>,
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    src: &SharedSource,
    current: &Arc<VideoFrame>,
    want: f32,
    decimation: u32,
    working: (usize, usize),
    prep: &dyn Fn(Arc<VideoFrame>) -> Arc<VideoFrame>,
) {
    let Some(radius) = wanted(ops, project, item, opts) else {
        crate::grade::strip_temporal(ops);
        return;
    };
    let clock = src.frame_clock();
    let rate = &clock;
    let Some((_, cur_f)) = source_frame_at(item, t, rate) else {
        crate::grade::strip_temporal(ops);
        return;
    };
    let fetched: Vec<Fetched<()>> = timed_neighbours(seq, item, t, rate, radius)
        .into_iter()
        .filter_map(|time| crate::clip_frame(src.as_ref(), item, time.source, want).ok().map(|frame| Fetched { time, frame, picture: () }))
        .collect();
    let media = media_key(project, item, src.as_ref());
    let list = neighbours(media, (cur_f, current), fetched_with_frames(fetched), working);
    if list.is_empty() {
        crate::grade::strip_temporal(ops);
        return;
    }
    let neighbours = list
        .into_iter()
        .map(|(offset, frame, flow_, back)| TemporalNeighbour {
            offset,
            picture: NeighbourPicture::Frame { frame: prep(frame), decimation: decimation.max(1) },
            flow: flow_,
            back,
        })
        .collect();
    let inputs = Arc::new(TemporalInputs { source_size: (current.width, current.height), neighbours });
    set_inputs(ops, &inputs);
}

/// The GPU path's pictures are the fetched frames themselves.
fn fetched_with_frames(v: Vec<Fetched<()>>) -> Vec<Fetched<Arc<VideoFrame>>> {
    v.into_iter().map(|n| Fetched { time: n.time, picture: n.frame.clone(), frame: n.frame }).collect()
}

/// Finish the ops of a clip's CPU layer (`item_layer_ex`; the grade's temporal op runs first on
/// the `working`-sized base layer): with a temporal render, attach the neighbours' base layers at
/// the same `want` and their flows; otherwise (or with no usable neighbour) remove the op.
#[allow(clippy::too_many_arguments)]
pub(crate) fn attach_images(
    ops: &mut Vec<FxOp>,
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
    want: f32,
    working: (usize, usize),
) {
    let Some(radius) = wanted(ops, project, item, opts) else {
        crate::grade::strip_temporal(ops);
        return;
    };
    let Some(src) = sources.source(item.item) else {
        crate::grade::strip_temporal(ops);
        return;
    };
    let clock = src.frame_clock();
    let rate = &clock;
    let current = source_frame_at(item, t, rate).and_then(|(ft, f)| crate::clip_frame(src.as_ref(), item, ft, want).ok().map(|fr| (f, fr)));
    let Some((cur_f, current)) = current else {
        crate::grade::strip_temporal(ops);
        return;
    };
    // (a neighbour's own base layer never runs temporal NR: it isn't graded here at all)
    let plain = RenderOptions { temporal: false, ..opts };
    let fetched: Vec<Fetched<Arc<Image>>> = timed_neighbours(seq, item, t, rate, radius)
        .into_iter()
        .filter_map(|time| {
            let frame = crate::clip_frame(src.as_ref(), item, time.source, want).ok()?;
            let img = crate::base_layer(project, seq, item, time.timeline, plain, sources, want)?;
            ((img.w, img.h) == working).then(|| Fetched { time, frame, picture: Arc::new(img) })
        })
        .collect();
    let media = media_key(project, item, src.as_ref());
    let list = neighbours(media, (cur_f, &current), fetched, working);
    if list.is_empty() {
        crate::grade::strip_temporal(ops);
        return;
    }
    let neighbours =
        list.into_iter().map(|(offset, img, flow_, back)| TemporalNeighbour { offset, picture: NeighbourPicture::Image(img), flow: flow_, back }).collect();
    let inputs = Arc::new(TemporalInputs { source_size: (current.width, current.height), neighbours });
    set_inputs(ops, &inputs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lru_keeps_the_most_recently_used() {
        let mut l: Lru<u32, u32> = Lru::new(2);
        l.insert(1, 10);
        l.insert(2, 20);
        assert_eq!(l.get(&1), Some(10));
        l.insert(3, 30);
        assert_eq!((l.get(&1), l.get(&2), l.get(&3)), (Some(10), None, Some(30)));
        l.insert(3, 31);
        assert_eq!((l.entries.len(), l.get(&3)), (2, Some(31)));
        let mut z: Lru<u32, u32> = Lru::new(0);
        z.insert(1, 1);
        z.insert(2, 2);
        assert_eq!(z.entries.len(), 1, "a zero capacity still keeps one");
    }
}
