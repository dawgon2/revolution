//! Background frame rendering for monitors, thumbnails and playback prefetch.
//!
//! A small pool of worker threads pulls prioritised jobs (the frame on screen first, then the
//! frames playback will need next, then thumbnails). Results land in a byte-budgeted cache keyed
//! by (target, frame, scale, revision); the UI shows the exact frame when ready and otherwise holds
//! the nearest frame it already has, so scrubbing never flashes black and never blocks the UI.
//! Sequence frames inside a rendered segment come from its render preview instead of the live
//! render (see `filmcraft_engine::previews`).

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use web_time::Instant;

use filmcraft_engine::previews::PreviewStore;
use filmcraft_engine::{MediaPool, Services};
use filmcraft_project::{ItemId, Project};
use filmcraft_time::Tick;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Composite of a sequence at a timeline time.
    Sequence(ItemId),
    /// A single project item at a media time (Source monitor, thumbnails).
    Item(ItemId),
    /// A GPU frame plan of a sequence (decoded layers + transforms), composited on the GPU.
    SequencePlan(ItemId),
    /// The Multi-Camera view's angle grid of a multi-camera source at its time: one page (grid
    /// side, 0 = automatic; page) of the shown angles, each at the job's scale.
    MulticamGrid(ItemId, u8, u16),
    /// One angle of a multi-camera source (Edit Cameras thumbnails).
    MulticamAngle(ItemId, u32),
    /// One node's output of a comp clip (sequence, clip id, node id) at a timeline time: the
    /// Compositor's node viewer in the Program monitor (fork C1 step 10b).
    CompNode(ItemId, u64, u64),
}

/// Item jobs at this priority or a larger number are background work: thumbnails of the Project
/// panel and the timeline (50) and the Media Browser (40). Monitors and playback use 0..=3. Their
/// decoders are small and are not kept (`filmcraft_media::cancel::with_background`).
pub const BACKGROUND_PRIO: u32 = 40;

/// Whether a job is a thumbnail: a single item at a background priority. Sequence frames are never
/// background, whatever their priority (the scopes ask for the program frame at 40 while playing,
/// and must not turn the program's decoders into small ones).
fn is_background(job: &Job) -> bool {
    background_work(job.prio, job.key.target)
}

fn background_work(prio: u32, target: Target) -> bool {
    prio >= BACKGROUND_PRIO && matches!(target, Target::Item(_))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub target: Target,
    pub frame: i64,
    /// Output width in pixels (thumbnails) or scale ×1000 (monitors).
    pub size: u32,
    pub revision: u64,
    /// Draft decoding (reduced-resolution playback with Settings ▸ Playback ▸ Draft decoding):
    /// sources may decode approximate non-reference pictures
    /// ([`filmcraft_media::cancel::with_draft`]). Draft frames have their own keys, so a paused
    /// monitor, an export or a render never shows one.
    pub draft: bool,
    /// A temporal render (fork 6.7, `RenderOptions::temporal`): clips with temporal noise
    /// reduction blend their neighbouring frames. Only the paused Program frame asks for one
    /// ([`temporal_frame`]), never playback or scrubbing; it has its own key, so a frame rendered
    /// while scrubbing is never shown as the paused one.
    pub temporal: bool,
}

/// Whether the Program monitor asks for a temporal render of its frame ([`FrameKey::temporal`]):
/// paused, not being scrubbed or dragged (`pointer_down`), and a clip at the frame uses temporal
/// noise reduction (`uses`), so every other paused frame keeps its single cache entry.
pub fn temporal_frame(playing: bool, pointer_down: bool, uses: bool) -> bool {
    !playing && !pointer_down && uses
}

/// Whether a monitor frame is requested with draft decoding ([`FrameKey::draft`]): only while
/// playing, at 1/2 resolution or lower, with the preference on.
pub fn draft_playback(playing: bool, scale: f32, enabled: bool) -> bool {
    enabled && playing && scale <= 0.5
}

pub struct Rgba {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

struct Job {
    key: FrameKey,
    queued: Instant,
    /// Set when the job is no longer wanted; sources poll it (`filmcraft_media::cancel`).
    cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Queued by playback prefetch (dropped when playback stops).
    prefetch: bool,
    /// While playing: how far before this frame the frames are late (the playhead's distance;
    /// zero for the frame due now), so a decoder catching up may skip pictures only late frames
    /// need (`filmcraft_media::cancel::with_catch_up`).
    catch_up: Option<Tick>,
    /// While playback steps over frames: any frame within this much of `time` will do
    /// (`filmcraft_media::cancel::with_skim`).
    skim: Option<Tick>,
    /// Hardware-decode pace while playing backwards (`filmcraft_media::cancel::with_pace`).
    pace: Option<Duration>,
    /// When the frame is due on screen ([`now_us`] clock): hardware decoders serve the most
    /// urgent job first (`filmcraft_codecs::hwgate`).
    deadline: u64,
    time: Tick,
    /// Output scale relative to the target's frame size.
    scale: f32,
    project: Arc<Project>,
    prio: u32,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    done: Mutex<Cache>,
    plans: Mutex<PlanCache>,
    /// Running jobs: key, cancel flag, prefetch.
    in_flight: Mutex<Vec<(FrameKey, Arc<std::sync::atomic::AtomicBool>, bool)>>,
    /// Per-job timings, collected while profiling is on (benchmarks, `ui.inspect`).
    profiling: AtomicBool,
    records: Mutex<Vec<JobRecord>>,
    /// Running estimate (s) of the per-frame work of playback jobs that is not fetching source
    /// frames: compositing, CPU effects. Frames cannot be ready sooner than this after they are
    /// started, and the workers can finish at most `workers / cost` of them per second.
    render_cost: Mutex<f64>,
    workers: usize,
    /// Always-on counters for `perf.stats`.
    stats: Mutex<FrameStats>,
    /// The server was dropped: workers exit (else they would keep the caches, and the decoded
    /// frames and GPU surfaces in them, alive for the life of the process).
    stop: AtomicBool,
    /// Fault injection (`FrameServer::inject_job_panics`): the next n jobs of this server panic.
    inject_panics: std::sync::atomic::AtomicU32,
    /// Which frames playback faster than real time shows ([`FrameServer::playback_frame`]).
    skim: Mutex<HashMap<Target, SkimControl>>,
}

/// Display steps (timeline frames between shown frames) playback faster than real time may use.
const SKIM_STEPS: [i64; 18] = [1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 24, 30, 40, 48, 60, 90, 120];
/// Grid frames judged at a time: the step goes up when [`SKIM_LATE`] of the last
/// [`SKIM_WINDOW`] were not on screen while due, and down after [`SKIM_CALM`] grid frames in a
/// row were, with the frames ahead already ready.
const SKIM_WINDOW: usize = 8;
const SKIM_LATE: usize = 3;
/// This many late of the window: two rungs at once (far behind; climbing one rung at a time
/// would stall the picture for seconds).
const SKIM_FAR_BEHIND: usize = 6;
const SKIM_CALM: usize = 48;
/// After a change of step, wait this long before judging again (the new grid's frames need
/// time to come in).
const SKIM_SETTLE: Duration = Duration::from_millis(500);

/// Fast-forward / fast-reverse frame skipping (JKL above 1×), like Resolve: the playhead shows
/// every `step`-th timeline frame (a fixed grid, so the frames prefetched are the frames shown),
/// starting at the speed's natural step (2 at 2×) and stepping up the ladder when the decoders
/// fall behind and back down when they have room, so the picture keeps an even cadence instead
/// of stalling. The step chosen for a speed is remembered for the next time it is played.
#[derive(Debug, Default)]
pub struct SkimControl {
    speed: f64,
    step: i64,
    /// Grid frame due and whether it was ready at some refresh while due.
    due: Option<(i64, bool)>,
    /// Whether each recent grid frame was ready in time (newest last).
    history: VecDeque<bool>,
    settled: Option<Instant>,
    /// Step last used per speed (speed × 1000).
    remembered: HashMap<i64, i64>,
    /// The last change was a step down, and how many calm grid frames a step down needs now:
    /// doubled each time a step down has to be undone (the decoders were only just keeping up).
    stepped_down: bool,
    calm: usize,
}

impl SkimControl {
    /// The natural step at `speed`: the frames a refresh at the timeline's rate passes over.
    pub fn base_step(speed: f64) -> i64 {
        if speed.abs() <= 1.0 + 1e-9 { 1 } else { (speed.abs().round() as i64).clamp(1, SKIM_STEPS[SKIM_STEPS.len() - 1]) }
    }

    fn speed_key(speed: f64) -> i64 {
        (speed * 1000.0).round() as i64
    }

    /// The step in force at `speed` (switching to it if playback changed speed).
    pub fn step(&mut self, speed: f64) -> i64 {
        if self.step == 0 || (self.speed - speed).abs() > 1e-9 {
            let base = Self::base_step(speed);
            self.speed = speed;
            self.step = self.remembered.get(&Self::speed_key(speed)).copied().unwrap_or(base).max(base);
            self.due = None;
            self.history.clear();
            self.settled = None;
        }
        self.step
    }

    /// Playback started or stopped: judge afresh (the remembered steps stay).
    pub fn restart(&mut self) {
        self.due = None;
        self.history.clear();
        self.settled = None;
    }

    /// One refresh while playing at `speed`: grid frame `frame` is due and `ready` says whether
    /// it is ready; `ahead_ready` whether frames well ahead are ready already (room to spare).
    /// Returns the step to use from now on.
    pub fn refresh(&mut self, speed: f64, frame: i64, ready: bool, ahead_ready: bool, now: Instant) -> i64 {
        let step = self.step(speed);
        if step == 1 && Self::base_step(speed) == 1 {
            // normal speed and slower: every frame is shown, never skipped on purpose
            return step;
        }
        match self.due {
            Some((f, seen)) if f == frame => self.due = Some((f, seen || ready)),
            Some((_, seen)) => {
                self.history.push_back(seen);
                if self.history.len() > self.calm.max(SKIM_CALM) {
                    self.history.pop_front();
                }
                self.due = Some((frame, ready));
            }
            None => self.due = Some((frame, ready)),
        }
        let settled = self.settled.is_none_or(|t| now.saturating_duration_since(t) >= SKIM_SETTLE);
        if !settled {
            return step;
        }
        let recent_late = self.history.iter().rev().take(SKIM_WINDOW).filter(|ok| !**ok).count();
        let base = Self::base_step(speed);
        // (three late in a row climb at once: waiting for a full window at a large step would
        // stall the picture for seconds)
        let new = if recent_late >= SKIM_LATE {
            let rungs = if recent_late >= SKIM_FAR_BEHIND { 2 } else { 1 };
            SKIM_STEPS.iter().copied().filter(|s| *s > step).take(rungs).last().unwrap_or(step)
        } else if self.history.len() >= self.calm.max(SKIM_CALM) && recent_late == 0 && self.history.iter().all(|ok| *ok) && ahead_ready {
            SKIM_STEPS.iter().rev().copied().find(|s| *s < step && *s >= base).unwrap_or(step)
        } else {
            step
        };
        if new != step {
            if new > step && self.stepped_down {
                self.calm = (self.calm.max(SKIM_CALM) * 2).min(SKIM_CALM * 8);
            }
            self.stepped_down = new < step;
            self.step = new;
            self.remembered.insert(Self::speed_key(speed), new);
            self.history.clear();
            self.due = None;
            self.settled = Some(now);
        }
        self.step
    }

    /// The grid frame shown while `frame` is under the playhead.
    pub fn snap(frame: i64, step: i64) -> i64 {
        if step <= 1 { frame } else { frame.div_euclid(step) * step }
    }

    /// How far from the wanted time a skimming frame may be (frames): half the step, so any
    /// frame of the grid cell will do (a sync sample in it costs one picture, where a frame deep
    /// in a GOP costs every reference picture before it). The picture still changes once per
    /// shown frame, at an even cadence.
    pub fn tolerance(step: i64) -> i64 {
        if step <= 1 { 0 } else { step / 2 }
    }
}

/// Cumulative frame-job statistics (`perf.stats`): cheap enough to keep on all the time.
#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    /// Jobs finished (including cancelled ones).
    pub jobs: u64,
    /// Jobs whose decode/render panicked (the worker survives; the frame is not cached).
    pub failed: u64,
    pub cancelled: u64,
    /// Jobs served from a render preview.
    pub preview: u64,
    /// Playback prefetch jobs.
    pub prefetch: u64,
    /// Requests answered by a cached frame or one already queued / running.
    pub request_hits: u64,
    /// Requests that queued a new job.
    pub request_misses: u64,
    /// Totals over all jobs (ms): getting source frames (decode), the rest (render), whole job.
    pub source_ms: f64,
    pub render_ms: f64,
    pub job_ms: f64,
    /// The most recent jobs: (source ms, render ms).
    pub recent: VecDeque<(f32, f32)>,
}

impl FrameStats {
    const RECENT: usize = 256;

    /// JSON for `perf.stats`.
    pub fn to_json(&self) -> serde_json::Value {
        let pct = |f: &dyn Fn(&(f32, f32)) -> f32, p: f64| -> f64 {
            let mut v: Vec<f32> = self.recent.iter().map(f).collect();
            if v.is_empty() {
                return 0.0;
            }
            v.sort_by(|a, b| a.total_cmp(b));
            v[((v.len() - 1) as f64 * p).round() as usize] as f64
        };
        let n = self.jobs.max(1) as f64;
        let req = self.request_hits + self.request_misses;
        serde_json::json!({
            "jobs": self.jobs, "cancelled": self.cancelled, "failedJobs": self.failed, "previewJobs": self.preview, "prefetchJobs": self.prefetch,
            "requestHitRate": if req == 0 { 0.0 } else { self.request_hits as f64 / req as f64 },
            "decodeMs": {"mean": self.source_ms / n, "p50": pct(&|r| r.0, 0.5), "p95": pct(&|r| r.0, 0.95)},
            "renderMs": {"mean": self.render_ms / n, "p50": pct(&|r| r.1, 0.5), "p95": pct(&|r| r.1, 0.95)},
            "jobMsMean": self.job_ms / n,
        })
    }
}

/// Timing of one finished frame job (collected while [`FrameServer::set_profiling`] is on).
#[derive(Clone, Copy, Debug)]
pub struct JobRecord {
    pub key: FrameKey,
    pub prio: u32,
    pub queued: Instant,
    pub started: Instant,
    pub finished: Instant,
    /// Thread-CPU time of the whole job (robust to machine load; zero where unsupported).
    pub cpu: Duration,
    /// Wall and thread-CPU time spent getting source frames (decode, including waits on a
    /// source's decoder lock; the decoder's own worker threads are not included in `source_cpu`).
    pub source_wall: Duration,
    pub source_cpu: Duration,
    /// The frame came from a render preview.
    pub preview: bool,
    /// The job was cancelled while running (its result was discarded).
    pub cancelled: bool,
}

/// CPU time consumed by the calling thread (None where the platform has no thread clock).
pub fn thread_cpu_time() -> Option<Duration> {
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
        Some(Duration::new(t.tv_sec as u64, t.tv_nsec as u32))
    }
    #[cfg(windows)]
    {
        cpu_time::ThreadTime::try_now().ok().map(|t| t.as_duration())
    }
    #[cfg(not(any(windows, all(unix, not(target_arch = "wasm32")))))]
    {
        None
    }
}

/// CPU time consumed by the whole process, all threads (None where unsupported).
pub fn process_cpu_time() -> Option<Duration> {
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
        Some(Duration::new(t.tv_sec as u64, t.tv_nsec as u32))
    }
    #[cfg(windows)]
    {
        cpu_time::ProcessTime::try_now().ok().map(|t| t.as_duration())
    }
    #[cfg(not(any(windows, all(unix, not(target_arch = "wasm32")))))]
    {
        None
    }
}

thread_local! {
    /// (wall, cpu) spent in source `video_frame` calls by this worker during the current job.
    static SOURCE_TIME: Cell<(Duration, Duration)> = const { Cell::new((Duration::ZERO, Duration::ZERO)) };
    /// This worker is profiling the current job (thread CPU of source calls is measured).
    static PROFILING: Cell<bool> = const { Cell::new(false) };
}

/// Run `f`, adding its wall/CPU time to this thread's source-time accumulator.
fn timed_source<R>(f: impl FnOnce() -> R) -> R {
    let cpu = || if PROFILING.with(Cell::get) { thread_cpu_time().unwrap_or_default() } else { Duration::ZERO };
    let (w0, c0) = (Instant::now(), cpu());
    let r = f();
    let (dw, dc) = (w0.elapsed(), cpu().saturating_sub(c0));
    SOURCE_TIME.with(|s| {
        let (w, c) = s.get();
        s.set((w + dw, c + dc));
    });
    r
}

/// A media source that times its `video_frame` calls (see [`JobRecord::source_wall`]).
struct TimedSource(filmcraft_media::SharedSource);

impl filmcraft_media::MediaSource for TimedSource {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        self.0.info()
    }
    fn set_handoff_scale(&self, scale: f32) {
        self.0.set_handoff_scale(scale);
    }
    fn decoder_name(&self) -> Option<String> {
        self.0.decoder_name()
    }
    // (a variable-frame-rate source's own frame times, Y2)
    fn frame_clock(&self) -> filmcraft_media::timing::FrameClock {
        self.0.frame_clock()
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        timed_source(|| self.0.video_frame(req))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.0.audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.0.audio_stream(stream, start, frames, sample_rate)
    }
}

/// The job's source provider: the pool, with source fetches timed (the time feeds
/// [`FrameServer::render_cost`] and, while profiling, [`JobRecord`]).
struct JobProvider {
    inner: filmcraft_engine::media_pool::PoolProvider,
}

impl filmcraft_render::SourceProvider for JobProvider {
    fn source(&self, item: ItemId) -> Option<filmcraft_media::SharedSource> {
        let s = self.inner.source(item)?;
        Some(Arc::new(TimedSource(s)))
    }
}

/// A GPU frame plan with its layers' texel conversions already done (off the UI thread).
pub struct GpuPlan {
    pub plan: filmcraft_render::plan::FramePlan,
    pub prepared: filmcraft_gpu::PreparedPlan,
}

/// Plans for frames playback has passed that are kept (stepping back a few frames after a stop
/// stays instant); see `PlanCache::retire_passed`.
const KEEP_PASSED: usize = 8;

/// What a cached plan is charged per GPU-resident layer frame (see [`GpuPlan::bytes`]).
const GPU_PLAN_CHARGE: usize = 16 << 20;

/// What a plan's GPU-resident layer frames (hardware decoder surfaces) are charged: mostly the
/// GOP caches' own frames (charged to their VRAM budget there), so a plan is charged a share,
/// which still bounds how many surfaces plans can keep alive (768 MB / 16 MB = 48).
fn gpu_plan_charge(plan: &filmcraft_render::plan::FramePlan) -> usize {
    use filmcraft_render::plan::{FramePlan, PlanLayer, PlanStep};
    let layers = |ls: &[PlanLayer]| ls.iter().map(|l| l.frame.gpu_bytes().min(GPU_PLAN_CHARGE)).sum::<usize>();
    match plan {
        FramePlan::Image(_) => 0,
        FramePlan::Layers { layers: ls, .. } => layers(ls),
        FramePlan::Composite { steps, .. } => steps
            .iter()
            .map(|s| match s {
                PlanStep::Layers(ls) => layers(ls),
                PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| layers(ls)).sum(),
                PlanStep::Output(_) => 0,
            })
            .sum(),
    }
}

impl GpuPlan {
    /// Bytes this plan keeps alive (layer frames + converted texels).
    fn bytes(&self) -> usize {
        let frames = self.plan.source_bytes() + gpu_plan_charge(&self.plan);
        frames + self.prepared.bytes()
    }
}

/// Finished plans, bounded by count and bytes (oldest use evicted first).
#[derive(Default)]
struct PlanCache {
    map: HashMap<FrameKey, (Arc<GpuPlan>, u64, usize)>,
    clock: u64,
    bytes: usize,
}

impl PlanCache {
    const MAX: usize = 96;
    const BUDGET: usize = 1 << 30;

    fn get(&mut self, k: &FrameKey) -> Option<Arc<GpuPlan>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(k).map(|(p, used, _)| {
            *used = clock;
            p.clone()
        })
    }

    /// Playback of `target` is at `frame` moving in `dir`: plans it has passed (more than
    /// `margin` frames behind) are evicted before any other (least recently used first would
    /// evict the frames prefetched ahead, which were made before the ones just shown).
    ///
    /// Beyond the [`KEEP_PASSED`] nearest, they are dropped at once: each can keep a decoded 6.2K
    /// surface (78 MB of GPU memory) alive, and a timeline cutting between four 6.2K clips came
    /// within 0.1 GB of a 12 GB card's memory with the plan cache full of frames already shown.
    fn retire_passed(&mut self, target: Target, frame: i64, dir: i64, margin: i64) {
        let mut passed: Vec<(i64, FrameKey)> = Vec::new();
        for (k, (_, used, _)) in self.map.iter_mut() {
            let behind = (frame - k.frame).saturating_mul(dir);
            if k.target == target && behind > margin {
                *used = 0;
                passed.push((behind, *k));
            }
        }
        if passed.len() > KEEP_PASSED {
            passed.sort_unstable_by_key(|(behind, _)| *behind);
            for (_, k) in passed.get(KEEP_PASSED..).unwrap_or_default() {
                if let Some((_, _, b)) = self.map.remove(k) {
                    self.bytes = self.bytes.saturating_sub(b);
                }
            }
        }
    }

    fn insert(&mut self, k: FrameKey, p: GpuPlan) {
        self.clock += 1;
        let b = p.bytes();
        self.bytes += b;
        if let Some((_, _, old)) = self.map.insert(k, (Arc::new(p), self.clock, b)) {
            self.bytes -= old;
        }
        if self.map.len() > Self::MAX || self.bytes > Self::BUDGET {
            let mut v: Vec<(u64, FrameKey, usize)> = self.map.iter().map(|(k, v)| (v.1, *k, v.2)).collect();
            v.sort_unstable_by_key(|x| x.0);
            for (_, k, b) in v {
                if self.map.len() <= Self::MAX && self.bytes <= Self::BUDGET {
                    break;
                }
                self.map.remove(&k);
                self.bytes -= b;
            }
        }
    }
}

struct Cache {
    map: HashMap<FrameKey, (Arc<Rgba>, u64)>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl Cache {
    fn insert(&mut self, k: FrameKey, v: Arc<Rgba>) {
        self.clock += 1;
        self.bytes += v.px.len();
        if let Some((old, _)) = self.map.insert(k, (v, self.clock)) {
            self.bytes -= old.px.len();
        }
        if self.bytes > self.budget {
            let mut all: Vec<(u64, FrameKey, usize)> = self.map.iter().map(|(k, (v, s))| (*s, *k, v.px.len())).collect();
            all.sort_unstable_by_key(|x| x.0);
            for (_, k, b) in all {
                if self.bytes <= self.budget * 8 / 10 {
                    break;
                }
                self.map.remove(&k);
                self.bytes -= b;
            }
        }
    }
}

pub struct FrameServer {
    shared: Arc<Shared>,
    pub pool: Arc<MediaPool>,
    pub services: Arc<dyn Services>,
    pub previews: Arc<PreviewStore>,
    repaint: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
}

/// Frames queued ahead of the playhead while playing (and how far ahead a queued job may be
/// before it is dropped as stale), in shown frames of `step` timeline frames each.
///
/// Backwards a decoder has to decode a whole GOP forward before the first frame of it (its last
/// one) can be shown, so reverse playback asks for frames more than a GOP (of up to ~1 s) ahead.
/// With large steps a dozen shown frames are already seconds of the timeline.
pub fn prefetch_depth(speed: f64, step: i64) -> i64 {
    if speed < 0.0 && step <= 2 {
        // (timeline frames: a GOP and more)
        REVERSE_PREFETCH / step.max(1)
    } else if step >= 8 {
        12
    } else if speed.abs() > 1.5 {
        24
    } else {
        14
    }
}

/// Frames reverse playback asks for ahead of the playhead (see [`prefetch_depth`]).
pub const REVERSE_PREFETCH: i64 = 40;

/// Pictures per frame interval reverse playback lets a hardware decoder submit (an open GOP
/// needs ~1.13 per frame: the GOP and the leading pictures of the next): measured on 6.2K, 1.2
/// (28 ms per picture at 29.97) played backwards with 1 frame of 300 dropped, unpaced ~25.
pub const REVERSE_PACE: f64 = 1.25;

/// Microseconds since the first call (the clock of job deadlines).
fn now_us() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros().min(u64::MAX as u128) as u64
}

/// Playback starts its clock once this many frames from the playhead are ready, or after
/// [`PREROLL_TIMEOUT_S`]: otherwise the first frames after Play are due before any decoder had a
/// chance to produce them (a seek decodes from the previous keyframe).
pub const PREROLL_FRAMES: i64 = 6;

/// Seconds before a scrubbed-to frame whose frames a seek still decodes in full (see
/// [`FrameServer::request`]); earlier non-reference pictures are skipped on the way.
pub const SCRUB_KEEP_S: f64 = 2.0;
pub const PREROLL_TIMEOUT_S: f64 = 0.5;

/// Backwards, a decoder can only show a GOP once it has decoded all of it, and the GOP before it
/// is decoded while that one plays: playback starts once [`REVERSE_PREROLL_FRAMES`] (about a
/// second) are ready, or after [`REVERSE_PREROLL_TIMEOUT_S`].
pub const REVERSE_PREROLL_FRAMES: i64 = 30;
pub const REVERSE_PREROLL_TIMEOUT_S: f64 = 2.0;

/// How long playback at `speed` waits for its first frames (see [`PREROLL_FRAMES`]).
pub fn preroll_timeout(speed: f64) -> f64 {
    if speed < 0.0 && SkimControl::base_step(speed) == 1 { REVERSE_PREROLL_TIMEOUT_S } else { PREROLL_TIMEOUT_S }
}

/// Which frames playback asks for, given the per-frame render cost (s), the displayed frame rate
/// and the worker count: `(lead, stride)`. While the workers keep up, every frame from the
/// playhead on (`(1, 1)`). When they cannot (CPU effects slower than real time), `lead` is the
/// first frame ahead of the playhead worth starting (nearer ones would be late however soon they
/// start) and `stride` the spacing the workers can finish in real time: the frames in between
/// are dropped on purpose and evenly, instead of every frame arriving late.
pub fn playback_plan(render_cost: f64, fps: f64, workers: usize) -> (i64, i64) {
    let frames_per_job = render_cost * fps;
    let load = frames_per_job * 1.15 / workers.max(1) as f64;
    if load <= 1.0 {
        return (1, 1);
    }
    (frames_per_job.ceil() as i64, load.ceil() as i64)
}

impl FrameServer {
    /// Settings ▸ Memory ▸ frame cache budget (bytes); evicts down to it right away.
    pub fn set_cache_budget(&self, bytes: usize) {
        let mut c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
        c.budget = bytes.max(16 << 20);
        if c.bytes > c.budget
            && let Some((k, v)) = c.map.iter().next().map(|(k, (v, _))| (*k, v.clone()))
        {
            // re-inserting an entry runs the eviction pass
            c.insert(k, v);
        }
    }

    /// (bytes cached, budget).
    pub fn cache_usage(&self) -> (usize, usize) {
        let c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
        (c.bytes, c.budget)
    }

    pub fn new(pool: Arc<MediaPool>, services: Arc<dyn Services>, previews: Arc<PreviewStore>, workers: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            done: Mutex::new(Cache { map: HashMap::new(), bytes: 0, budget: 768 << 20, clock: 0 }),
            plans: Mutex::new(PlanCache::default()),
            in_flight: Mutex::new(Vec::new()),
            profiling: AtomicBool::new(false),
            records: Mutex::new(Vec::new()),
            skim: Mutex::new(HashMap::new()),
            render_cost: Mutex::new(0.0),
            workers: workers.max(1),
            stats: Mutex::new(FrameStats::default()),
            stop: AtomicBool::new(false),
            inject_panics: std::sync::atomic::AtomicU32::new(0),
        });
        let repaint: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>> = Arc::new(Mutex::new(None));
        #[cfg(not(target_arch = "wasm32"))]
        for i in 0..workers.max(1) {
            let sh = shared.clone();
            let pool = pool.clone();
            let services = services.clone();
            let rp = repaint.clone();
            let pv = previews.clone();
            std::thread::Builder::new().name(format!("filmcraft-frames-{i}")).spawn(move || worker(sh, pool, services, pv, rp)).ok();
        }
        #[cfg(target_arch = "wasm32")]
        let _ = workers;
        Self { shared, pool, services, previews, repaint }
    }

    /// The default number of frame workers for this machine.
    pub fn default_workers() -> usize {
        std::thread::available_parallelism().map(|n| n.get().clamp(2, 6)).unwrap_or(3)
    }

    pub fn set_context(&self, ctx: &egui::Context) {
        let mut g = self.repaint.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            let ctx = ctx.clone();
            *g = Some(Box::new(move || ctx.request_repaint()));
        }
    }

    /// Collect a [`JobRecord`] per finished job (benchmarks); `take_records` drains them.
    pub fn set_profiling(&self, on: bool) {
        self.shared.profiling.store(on, Ordering::Relaxed);
    }

    pub fn take_records(&self) -> Vec<JobRecord> {
        std::mem::take(&mut *self.shared.records.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Cumulative job statistics (`perf.stats`).
    pub fn stats(&self) -> FrameStats {
        self.shared.stats.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Worker threads.
    pub fn workers(&self) -> usize {
        self.shared.workers
    }

    /// Worker threads that have not exited yet (each holds the shared state until it does); unlike
    /// [`Self::stop_and_wait`] this only looks.
    pub fn live_workers(&self) -> usize {
        Arc::strong_count(&self.shared).saturating_sub(1)
    }

    /// (cached images, their bytes, cached GPU plans, their bytes).
    pub fn cache_entries(&self) -> (usize, usize, usize, usize) {
        let (n, b) = {
            let c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
            (c.map.len(), c.bytes)
        };
        let p = self.shared.plans.lock().unwrap_or_else(|e| e.into_inner());
        (n, b, p.map.len(), p.bytes)
    }

    pub fn get(&self, k: &FrameKey) -> Option<Arc<Rgba>> {
        let mut c = self.shared.done.lock().unwrap_or_else(|e| e.into_inner());
        c.clock += 1;
        let clock = c.clock;
        c.map.get_mut(k).map(|(v, s)| {
            *s = clock;
            v.clone()
        })
    }

    pub fn get_plan(&self, k: &FrameKey) -> Option<Arc<GpuPlan>> {
        self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).get(k)
    }

    /// Whether the exact frame (image or plan) is ready.
    pub fn is_ready(&self, k: &FrameKey) -> bool {
        match k.target {
            Target::SequencePlan(_) => self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(k),
            _ => self.shared.done.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(k),
        }
    }

    /// Nearest cached plan at or before `frame`.
    pub fn nearest_plan(&self, key: FrameKey, max_back: i64) -> Option<(FrameKey, Arc<GpuPlan>)> {
        let mut g = self.shared.plans.lock().unwrap_or_else(|e| e.into_inner());
        (0..=max_back).find_map(|d| {
            let k = FrameKey { frame: key.frame - d, ..key };
            g.get(&k).map(|p| (k, p))
        })
    }

    /// Queue a job unless it is cached, queued or in flight.
    ///
    /// The frame on screen (`prio` 0: scrubbing, a seek) treats frames more than
    /// [`SCRUB_KEEP_S`] before it as not wanted: a decoder seeking from a keyframe skips their
    /// non-reference pictures, while nearby frames stay decoded (and cached) for scrubbing back.
    pub fn request(&self, key: FrameKey, time: Tick, scale: f32, project: &Arc<Project>, prio: u32) {
        let late = (prio == 0).then(|| Tick::from_seconds_f64(SCRUB_KEEP_S));
        // wanted now (the frame on screen), thumbnails and the like after it
        self.request_job(key, time, scale, project, prio, false, late, None, None, now_us().saturating_add(prio as u64 * 1_000));
    }

    #[allow(clippy::too_many_arguments)]
    fn request_job(
        &self,
        key: FrameKey,
        time: Tick,
        scale: f32,
        project: &Arc<Project>,
        prio: u32,
        prefetch: bool,
        catch_up: Option<Tick>,
        skim: Option<Tick>,
        pace: Option<Duration>,
        deadline: u64,
    ) {
        let hit = || self.shared.stats.lock().unwrap_or_else(|e| e.into_inner()).request_hits += 1;
        if self.shared.done.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(&key)
            || self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).map.contains_key(&key)
        {
            hit();
            return;
        }
        if self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(k, c, _)| *k == key && !c.load(Ordering::Relaxed)) {
            hit();
            return;
        }
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(j) = q.iter_mut().find(|j| j.key == key) {
            j.prio = j.prio.min(prio);
            j.deadline = j.deadline.min(deadline);
            j.prefetch &= prefetch;
            // the playhead only moves on: the newest (smallest) margin
            j.catch_up = match (j.catch_up, catch_up) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            // an exact request for the frame wins over a skimming one
            j.skim = j.skim.zip(skim).map(|(a, b)| a.min(b));
            j.pace = j.pace.zip(pace).map(|(a, b)| a.min(b));
            drop(q);
            hit();
            return;
        }
        self.shared.stats.lock().unwrap_or_else(|e| e.into_inner()).request_misses += 1;
        if prio == 0 {
            // A new frame on screen for this view replaces the one asked for before: scrubbing
            // asks for one per refresh, and without this the oldest position would decode
            // first (or keep a decoder busy seeking to it) while the newest waits.
            //
            // A job already running for the same frame at an older revision (a value being
            // dragged asks for a new revision per refresh) is left to finish: it needs the same
            // source frames, so it holds no decoder back, and when frames take longer than a
            // refresh, cancelling each one for the next would show nothing until the mouse rests.
            let same_view = |k: &FrameKey| k.target == key.target && k.size == key.size;
            q.retain(|j| !(j.prio == 0 && !j.prefetch && same_view(&j.key)));
            for (k, c, prefetch) in self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                if !prefetch && same_view(k) && k.frame != key.frame {
                    c.store(true, Ordering::Relaxed);
                }
            }
        }
        q.push_back(Job {
            key,
            queued: Instant::now(),
            cancel: Default::default(),
            prefetch,
            catch_up,
            skim,
            pace,
            deadline,
            time,
            scale,
            project: project.clone(),
            prio,
        });
        drop(q);
        self.shared.cv.notify_one();
    }

    /// Playback scheduling for the monitor showing `key` (the frame due now): request it first,
    /// then the next frames in display order, and drop queued jobs for this target that are
    /// stale (old revision, behind the playhead or too far ahead).
    ///
    /// When frames cost more to render than the frame interval (CPU effects), frames that could
    /// not be ready in time are not started and the workers get evenly spaced frames they can
    /// finish ([`playback_plan`]). While `preroll` (clock not started yet) every frame is wanted.
    pub fn schedule_playback(&self, key: FrameKey, rate: filmcraft_time::FrameRate, scale: f32, project: &Arc<Project>, speed: f64, preroll: bool) {
        let dir = if speed < 0.0 { -1 } else { 1 };
        let step = {
            let mut all = self.shared.skim.lock().unwrap_or_else(|e| e.into_inner());
            let sk = all.entry(key.target).or_default();
            let step = sk.step(speed);
            if preroll {
                sk.restart();
                step
            } else {
                let ready = self.is_ready(&key);
                // room to spare: a frame half the prefetch window ahead is ready already
                let probe = key.frame + prefetch_depth(speed, step) / 2 * dir * step;
                let ahead_ready = self.is_ready(&FrameKey { frame: probe, ..key });
                sk.refresh(speed, key.frame, ready, ahead_ready, Instant::now())
            }
        };
        let skim = (step > 1).then(|| rate.tick_of(SkimControl::tolerance(step)));
        // Backwards at normal speed a GOP is decoded at a time, well ahead (see
        // `prefetch_depth`): pace it to REVERSE_PACE pictures per frame interval.
        let pace = (dir < 0 && step == 1).then(|| Duration::from_secs_f64(1.0 / (rate.as_f64().max(1.0) * REVERSE_PACE)));
        let fps = rate.as_f64() * speed.abs().max(1.0) / step as f64;
        let (lead, stride) = if preroll { (1, 1) } else { playback_plan(self.render_cost(), fps, self.shared.workers) };
        // when a frame `ahead` display frames from the playhead is due
        let now = now_us();
        let frame_us = 1e6 / (rate.as_f64() * speed.abs().max(1e-3));
        let due = |ahead: i64| now.saturating_add((ahead.max(0) as f64 * frame_us) as u64);
        if lead <= 1 {
            // the frame due now; once the clock runs, the frames before it are late
            self.request_job(key, rate.tick_of(key.frame), scale, project, 0, false, (!preroll && dir > 0).then_some(Tick::ZERO), skim, pace, now);
        }
        let ahead = prefetch_depth(speed, step);
        for i in 0..ahead {
            let f = key.frame + (lead + i * stride) * dir * step;
            if f >= 0 {
                // forward play: frames before the playhead are late (reverse play skips nothing)
                let late = (!preroll && dir > 0).then(|| rate.tick_of(f) - rate.tick_of(key.frame));
                self.request_job(
                    FrameKey { frame: f, ..key },
                    rate.tick_of(f),
                    scale,
                    project,
                    i as u32 + 1,
                    true,
                    late,
                    skim,
                    pace,
                    due((f - key.frame).abs()),
                );
            }
        }
        let (target, rev, cur) = (key.target, key.revision, key.frame);
        if !preroll {
            self.shared.plans.lock().unwrap_or_else(|e| e.into_inner()).retire_passed(target, cur, dir, 2 * step);
        }
        let span = (lead + ahead * stride) * step + 26;
        // (skimming: frames of another grid, queued before the step changed, are not shown)
        self.retain_queue(|k| {
            k.target != target
                || (k.revision == rev && (k.frame - cur) * dir >= 0 && (k.frame - cur).abs() < span && (step == 1 || k.frame.rem_euclid(step) == 0))
        });
    }

    /// Whether playback can start at `key`: the next [`PREROLL_FRAMES`] frames (up to `last`)
    /// are ready ([`REVERSE_PREROLL_FRAMES`] backwards at normal speed).
    pub fn preroll_ready(&self, key: FrameKey, speed: f64, last: i64) -> bool {
        let step = self.playback_step(key.target, speed) * if speed < 0.0 { -1 } else { 1 };
        let n = if speed < 0.0 && step == -1 { REVERSE_PREROLL_FRAMES } else { PREROLL_FRAMES };
        (0..n).map(|i| key.frame + i * step).filter(|f| *f >= 0 && *f <= last).all(|f| self.is_ready(&FrameKey { frame: f, ..key }))
    }

    /// Timeline frames between the frames playback of `target` at `speed` shows
    /// ([`SkimControl`]).
    pub fn playback_step(&self, target: Target, speed: f64) -> i64 {
        self.shared.skim.lock().unwrap_or_else(|e| e.into_inner()).entry(target).or_default().step(speed)
    }

    /// The frame playback of `target` at `speed` shows while `frame` is under the playhead:
    /// every frame up to 1×, the frames of the skimming grid above (and backwards above 1×).
    pub fn playback_frame(&self, target: Target, frame: i64, speed: f64) -> i64 {
        SkimControl::snap(frame, self.playback_step(target, speed))
    }

    /// The current per-frame render cost estimate (s), see [`playback_plan`].
    pub fn render_cost(&self) -> f64 {
        *self.shared.render_cost.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop queued jobs that fail `keep` (e.g. stale revisions or frames far from the playhead).
    /// Jobs already running for such frames are cancelled: a worker blocked behind a decoder
    /// would otherwise go on to decode a frame playback has passed, possibly from its keyframe.
    pub fn retain_queue(&self, keep: impl Fn(&FrameKey) -> bool) {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).retain(|j| keep(&j.key));
        for (k, c, _) in self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if !keep(k) {
                c.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Playback stopped: drop its queued prefetch jobs and cancel the running ones (left alone,
    /// they would keep the workers busy, e.g. rendering effects for frames nobody will see).
    pub fn stop_prefetch(&self) {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).retain(|j| !j.prefetch);
        for (_, c, prefetch) in self.shared.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if *prefetch {
                c.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Fault injection for robustness tests (and agents checking recovery): the next `n` frame
    /// jobs of this server panic as if a decoder had hit a bug. Per server, so tests running in
    /// parallel in one process don't consume each other's faults.
    #[doc(hidden)]
    pub fn inject_job_panics(&self, n: u32) {
        self.shared.inject_panics.store(n, Ordering::Relaxed);
    }

    pub fn queue_len(&self) -> usize {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The most recent cached frame for `target` at or before `frame` within `max_back` frames.
    pub fn nearest(&self, target: Target, frame: i64, size: u32, revision: u64, max_back: i64) -> Option<Arc<Rgba>> {
        for d in 0..=max_back {
            if let Some(v) = self.get(&FrameKey { target, frame: frame - d, size, revision, draft: false, temporal: false }) {
                return Some(v);
            }
        }
        None
    }

    /// Without worker threads (wasm32): run queued jobs on the calling (UI) thread, highest
    /// priority first, until `budget` has passed (at least one job). Jobs whose media is still
    /// loading go back to the queue. Returns the number of jobs finished. A no-op with workers.
    pub fn pump(&self, budget: Duration) -> usize {
        if cfg!(not(target_arch = "wasm32")) {
            return 0;
        }
        let t0 = Instant::now();
        let mut done = 0;
        let mut retry = Vec::new();
        loop {
            let job = {
                let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
                if q.is_empty() {
                    break;
                }
                let best = q.iter().enumerate().min_by_key(|(i, j)| (j.prio, *i)).map(|(i, _)| i).unwrap_or(0);
                match q.remove(best) {
                    Some(job) => job,
                    None => break,
                }
            };
            if run_job(&self.shared, &job, &self.pool, &self.services, &self.previews) {
                done += 1;
            } else {
                retry.push(job);
            }
            if t0.elapsed() >= budget {
                break;
            }
        }
        if !retry.is_empty() {
            let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
            for j in retry.into_iter().rev() {
                if !q.iter().any(|x| x.key == j.key) {
                    q.push_front(j);
                }
            }
        }
        done
    }
}

/// Counts shown and dropped frames while playing.
///
/// A frame of the timeline is *shown* when its exact picture was on screen at some refresh while
/// it was the frame due, and *dropped* otherwise — including frames the playhead skipped because
/// no refresh happened during their interval. Frames left out on purpose (fast-forward speeds
/// step over frames) are not counted.
#[derive(Clone, Debug, Default)]
pub struct PlaybackMeter {
    pub shown: u64,
    pub dropped: u64,
    /// Frame due at the last refresh, and whether its exact picture has been shown.
    current: Option<(i64, bool)>,
    /// Frames advanced per displayed frame (speed), and direction.
    step: i64,
}

impl PlaybackMeter {
    pub fn start(&mut self, speed: f64) {
        *self = Self { step: (speed.abs().max(1.0) as i64) * if speed < 0.0 { -1 } else { 1 }, ..Default::default() };
    }

    /// Playback now shows every `step`-th frame (skimming, [`SkimControl`]); the direction stays.
    pub fn set_step(&mut self, step: i64) {
        self.step = step.max(1) * if self.step < 0 { -1 } else { 1 };
    }

    /// Record one display refresh: `frame` is due now and `exact` says whether it is on screen.
    pub fn refresh(&mut self, frame: i64, exact: bool) {
        let step = if self.step == 0 { 1 } else { self.step };
        match self.current {
            Some((f, seen)) if f == frame => self.current = Some((f, seen || exact)),
            Some((f, seen)) => {
                self.close(seen);
                let moved = (frame - f) / step;
                // Forward progress: count frames passed over without a refresh. A jump backwards
                // (loop restart) or a huge jump is a discontinuity, not a drop.
                if moved > 1 && moved < 1000 {
                    self.dropped += (moved - 1) as u64;
                }
                self.current = Some((frame, exact));
            }
            None => self.current = Some((frame, exact)),
        }
    }

    fn close(&mut self, seen: bool) {
        if seen {
            self.shown += 1;
        } else {
            self.dropped += 1;
        }
    }

    /// Continue from `frame` without counting what happened since the last refresh (the window
    /// was hidden, so nothing could be shown).
    pub fn resync(&mut self, frame: i64, exact: bool) {
        self.current = Some((frame, exact));
    }

    /// Playback stopped: account for the frame on screen.
    pub fn finish(&mut self) {
        if let Some((_, seen)) = self.current.take() {
            self.close(seen);
        }
    }

    /// Shown and dropped so far, counting the frame currently due as shown when it is.
    pub fn counts(&self) -> (u64, u64) {
        match self.current {
            Some((_, true)) => (self.shown + 1, self.dropped),
            _ => (self.shown, self.dropped),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn worker(
    sh: Arc<Shared>,
    pool: Arc<MediaPool>,
    services: Arc<dyn Services>,
    previews: Arc<PreviewStore>,
    repaint: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
) {
    loop {
        let job = {
            let mut q = sh.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if sh.stop.load(Ordering::Relaxed) {
                    return;
                }
                if !q.is_empty() {
                    // highest priority (lowest number) first; FIFO among equals
                    let best = q.iter().enumerate().min_by_key(|(i, j)| (j.prio, *i)).map(|(i, _)| i).unwrap_or(0);
                    if let Some(job) = q.remove(best) {
                        break job;
                    }
                }
                q = sh.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        // A panic while decoding or rendering one frame (bad media, a decoder bug) must not take
        // the worker down with it: a dead worker leaves its frame "in flight" forever and the
        // monitors blank. Log it, forget the job and keep serving.
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(&sh, &job, &pool, &services, &previews)));
        if ran.is_err() {
            sh.in_flight.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, c, _)| !(*k == job.key && Arc::ptr_eq(c, &job.cancel)));
            sh.stats.lock().unwrap_or_else(|e| e.into_inner()).failed += 1;
            log::error!("frame job {:?} panicked; the worker continues", job.key);
        }
        if let Some(f) = repaint.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            f();
        }
    }
}

impl Drop for FrameServer {
    fn drop(&mut self) {
        self.stop();
    }
}

impl FrameServer {
    /// Stop the workers and wait at most `timeout` for them to exit (the app is closing; dropping
    /// the server only tells them to stop). The frames they are rendering are cancelled, so this
    /// is quick unless a source ignores cancellation. Returns whether every worker exited.
    pub fn stop_and_wait(&self, timeout: Duration) -> bool {
        self.stop();
        let start = Instant::now();
        let end = start.checked_add(timeout).unwrap_or(start);
        // each worker holds a reference to the shared state until it exits
        while Arc::strong_count(&self.shared) > 1 {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    /// Tell the workers to stop: the queue is dropped, frames being rendered are cancelled, and the
    /// caches are emptied.
    fn stop(&self) {
        let sh = &self.shared;
        sh.stop.store(true, Ordering::Relaxed);
        sh.queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
        for (_, c, _) in sh.in_flight.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            c.store(true, Ordering::Relaxed);
        }
        {
            let mut d = sh.done.lock().unwrap_or_else(|e| e.into_inner());
            d.map.clear();
            d.bytes = 0;
        }
        {
            let mut p = sh.plans.lock().unwrap_or_else(|e| e.into_inner());
            p.map.clear();
            p.bytes = 0;
        }
        sh.cv.notify_all();
    }
}

/// Run one job and cache its result. Returns false when the job's media was still loading (an
/// asynchronous web read): nothing was cached and the job should be retried.
fn run_job(sh: &Shared, job: &Job, pool: &Arc<MediaPool>, services: &Arc<dyn Services>, previews: &PreviewStore) -> bool {
    sh.in_flight.lock().unwrap_or_else(|e| e.into_inner()).push((job.key, job.cancel.clone(), job.prefetch));
    let inject = &sh.inject_panics;
    if inject.load(Ordering::Relaxed) > 0 && inject.fetch_sub(1, Ordering::Relaxed) > 0 {
        crate::crash::injected_fault("injected frame-job fault");
    }
    let profiling = sh.profiling.load(Ordering::Relaxed);
    let (started, cpu0) = (Instant::now(), if profiling { thread_cpu_time().unwrap_or_default() } else { Duration::ZERO });
    SOURCE_TIME.with(|s| s.set((Duration::ZERO, Duration::ZERO)));
    PROFILING.with(|p| p.set(profiling));
    // A cancelled job may have missed layers (its source gave up): its result is not cached.
    // A job whose media bytes were still loading (web) rendered with missing layers: like a
    // cancelled one, its result is not cached (the caller retries it).
    let _ = filmcraft_media::pending::take();
    let mut loading = false;
    let preview = filmcraft_media::cancel::with_cancel(&job.cancel, || {
        filmcraft_media::cancel::with_catch_up(job.catch_up, || {
            filmcraft_media::cancel::with_skim(job.skim, || {
                filmcraft_media::cancel::with_pace(job.pace, || {
                    filmcraft_media::cancel::with_deadline(job.deadline, || {
                        filmcraft_media::cancel::with_draft(job.key.draft, || {
                            filmcraft_media::cancel::with_background(is_background(job), || {
                                if let Target::SequencePlan(seq) = job.key.target {
                                    let (plan, pv) = plan_job(job, seq, pool, services, previews);
                                    loading = filmcraft_media::pending::take();
                                    if !job.cancel.load(Ordering::Relaxed) && !loading {
                                        // Convert texels for upload here, not on the UI thread when the frame is shown.
                                        let prepared = filmcraft_gpu::prepare(&plan);
                                        sh.plans.lock().unwrap_or_else(|e| e.into_inner()).insert(job.key, GpuPlan { plan, prepared });
                                    }
                                    pv
                                } else {
                                    let (img, pv) = render_job(job, pool, services, previews);
                                    loading = filmcraft_media::pending::take();
                                    if !job.cancel.load(Ordering::Relaxed) && !loading {
                                        sh.done.lock().unwrap_or_else(|e| e.into_inner()).insert(job.key, Arc::new(img));
                                    }
                                    pv
                                }
                            })
                        })
                    })
                })
            })
        })
    });
    let cancelled = job.cancel.load(Ordering::Relaxed);
    sh.in_flight.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, c, _)| !(*k == job.key && Arc::ptr_eq(c, &job.cancel)));
    if job.prefetch {
        // Work beyond fetching source frames (decoding is sequential per source and is
        // not saved by skipping frames, so it is left out of the estimate). A cancelled job
        // only tells that the work takes at least this long.
        let cost = started.elapsed().saturating_sub(SOURCE_TIME.with(|s| s.get()).0).as_secs_f64();
        let mut c = sh.render_cost.lock().unwrap_or_else(|e| e.into_inner());
        if !cancelled || cost > *c {
            *c = if *c == 0.0 { cost } else { *c * 0.8 + cost * 0.2 };
        }
    }
    {
        let job_wall = started.elapsed();
        let source = SOURCE_TIME.with(|s| s.get()).0;
        let (src_ms, render_ms) = (source.as_secs_f64() * 1e3, job_wall.saturating_sub(source).as_secs_f64() * 1e3);
        let mut st = sh.stats.lock().unwrap_or_else(|e| e.into_inner());
        st.jobs += 1;
        st.cancelled += cancelled as u64;
        st.preview += preview as u64;
        st.prefetch += job.prefetch as u64;
        st.source_ms += src_ms;
        st.render_ms += render_ms;
        st.job_ms += job_wall.as_secs_f64() * 1e3;
        if st.recent.len() >= FrameStats::RECENT {
            st.recent.pop_front();
        }
        st.recent.push_back((src_ms as f32, render_ms as f32));
    }
    if profiling {
        let (source_wall, source_cpu) = SOURCE_TIME.with(|s| s.get());
        let rec = JobRecord {
            key: job.key,
            prio: job.prio,
            queued: job.queued,
            started,
            finished: Instant::now(),
            cpu: thread_cpu_time().unwrap_or_default().saturating_sub(cpu0),
            source_wall,
            source_cpu,
            preview,
            cancelled,
        };
        sh.records.lock().unwrap_or_else(|e| e.into_inner()).push(rec);
    }
    !loading
}

/// The preview frame for a sequence job, when its segment has been rendered.
fn preview_frame(job: &Job, seq: ItemId, pool: &MediaPool, previews: &PreviewStore) -> Option<Arc<filmcraft_frame::VideoFrame>> {
    let rate = job.project.sequence(seq)?.settings.frame_rate;
    let f = rate.frame_at(job.time);
    let out = timed_source(|| previews.frame(pool, &job.project, seq, f, job.scale));
    if job.prefetch {
        // fork 6.3: open the next cached part now, so a part boundary or a cut does not wait for
        // its file to open
        previews.preopen_next(pool, &job.project, seq, f);
    }
    out
}

/// Whether a job renders with temporal access: its key asks for it and it is not playback
/// prefetch (the flows run here, on a frame-server worker, never on the UI thread).
fn job_temporal(job: &Job) -> bool {
    job.key.temporal && !job.prefetch && matches!(job.key.target, Target::Sequence(_) | Target::SequencePlan(_))
}

fn provider(job: &Job, pool: &Arc<MediaPool>, services: &Arc<dyn Services>) -> JobProvider {
    JobProvider { inner: pool.provider(job.project.clone(), services.clone()) }
}

fn plan_job(job: &Job, seq: ItemId, pool: &Arc<MediaPool>, services: &Arc<dyn Services>, previews: &PreviewStore) -> (filmcraft_render::plan::FramePlan, bool) {
    let opts = filmcraft_render::RenderOptions { scale: job.scale, captions: true, temporal: job_temporal(job), ..Default::default() };
    if let Some(frame) = preview_frame(job, seq, pool, previews)
        && let Some(q) = job.project.sequence(seq)
    {
        // One full-frame layer: the preview scaled to the output size. Previews never contain
        // captions (they are not part of the preview hash), so captions are layered on live.
        let (w, h) = filmcraft_render::output_size(q, job.scale);
        let matrix = filmcraft_geom::Affine::scale(w as f64 / frame.width.max(1) as f64, h as f64 / frame.height.max(1) as f64);
        let mut layers = vec![filmcraft_render::plan::PlanLayer { frame, matrix, opacity: 1.0, blend: filmcraft_render::Blend::Normal, fx: None, comp: None }];
        for o in filmcraft_render::caption_overlays(q, job.time, w, h) {
            layers.push(filmcraft_render::plan::PlanLayer {
                frame: Arc::new(filmcraft_frame::VideoFrame::rgba_f32(o.w as u32, o.h as u32, o.px)),
                matrix: filmcraft_geom::Affine::translate(o.x as f64, o.y as f64),
                opacity: 1.0,
                blend: filmcraft_render::Blend::Normal,
                fx: None,
                comp: None,
            });
        }
        // (a render preview is display-referred already: no output transform)
        return (filmcraft_render::plan::FramePlan::Layers { width: w, height: h, layers, output: None }, true);
    }
    let provider = provider(job, pool, services);
    (filmcraft_render::plan::plan_frame(&job.project, seq, job.time, opts, &provider), false)
}

/// Render a job to RGBA8; the flag says whether it came from a render preview.
fn render_job(job: &Job, pool: &Arc<MediaPool>, services: &Arc<dyn Services>, previews: &PreviewStore) -> (Rgba, bool) {
    if let Target::Sequence(seq) | Target::SequencePlan(seq) = job.key.target
        && let Some(f) = preview_frame(job, seq, pool, previews)
        && let Some(q) = job.project.sequence(seq)
    {
        let (w, h) = filmcraft_render::output_size(q, job.scale);
        let img = filmcraft_render::Image { w: f.width as usize, h: f.height as usize, px: f.to_linear_f32() };
        let mut img = if img.w == w && img.h == h {
            img
        } else {
            img.transformed(w, h, &filmcraft_geom::Affine::scale(w as f64 / img.w.max(1) as f64, h as f64 / img.h.max(1) as f64))
        };
        if matches!(job.key.target, Target::Sequence(_)) {
            for o in filmcraft_render::caption_overlays(q, job.time, w, h) {
                o.composite_onto(&mut img.px, w, h);
            }
        }
        return (Rgba { w: img.w, h: img.h, px: img.over_black_rgba8() }, true);
    }
    let provider = provider(job, pool, services);
    let opts = filmcraft_render::RenderOptions { scale: job.scale, captions: true, temporal: job_temporal(job), ..Default::default() };
    let img = match job.key.target {
        Target::Sequence(seq) | Target::SequencePlan(seq) => Some(filmcraft_render::render_sequence(&job.project, seq, job.time, opts, &provider)),
        Target::Item(item) => filmcraft_render::render_item(&job.project, item, job.time, job.scale, &provider),
        Target::MulticamGrid(item, side, page) => {
            let side = (side > 0).then_some(side as usize);
            filmcraft_render::multicam::render_grid_page(&job.project, item, job.time, job.scale, side, page as usize, &provider).map(|(img, _)| img)
        }
        Target::MulticamAngle(item, angle) => {
            filmcraft_render::multicam::render_angle_thumbnail(&job.project, item, angle as usize, job.time, job.scale, &provider)
        }
        Target::CompNode(seq, clip, node) => {
            filmcraft_render::render_comp_node(&job.project, seq, filmcraft_project::ClipId(clip), node, job.time, opts, &provider)
        }
    };
    let rgba = match img {
        Some(img) => Rgba { w: img.w, h: img.h, px: img.over_black_rgba8() },
        None => Rgba { w: 1, h: 1, px: vec![0, 0, 0, 255] },
    };
    (rgba, false)
}

#[cfg(test)]
mod tests {
    use super::{PlaybackMeter, SkimControl, Target, background_work, playback_plan};

    /// Y2 review: the job wrapper must pass a variable-frame-rate source's own frame times on
    /// (it gave the typical rate's grid, so the app's reverse snap / blends / TNR ignored them).
    #[test]
    fn timed_sources_keep_a_variable_frame_rate_clock() {
        use filmcraft_media::MediaSource;
        use filmcraft_media::timing::FrameClock;
        use filmcraft_time::{FrameRate, Tick};
        struct Vfr(filmcraft_media::MediaInfo);
        impl MediaSource for Vfr {
            fn info(&self) -> &filmcraft_media::MediaInfo {
                &self.0
            }
            fn frame_clock(&self) -> FrameClock {
                FrameClock::from_starts([Tick(0), Tick(10), Tick(30)], FrameRate::FPS_30)
            }
            fn video_frame(&self, _: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
                Err(filmcraft_media::MediaError::NoStream("video"))
            }
            fn audio(&self, _: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
                Ok(filmcraft_frame::AudioBuffer::silence(sample_rate, 2, frames))
            }
        }
        let info = filmcraft_media::MediaInfo {
            name: "vfr".into(),
            kind: filmcraft_media::MediaKind::Movie,
            duration: Tick(30),
            video: None,
            audio_streams: Vec::new(),
            container: String::new(),
            start_timecode: None,
            file_size: None,
        };
        let inner: filmcraft_media::SharedSource = std::sync::Arc::new(Vfr(info));
        let want = inner.frame_clock();
        assert!(matches!(want, FrameClock::Times(_)));
        assert_eq!(super::TimedSource(inner).frame_clock(), want);
    }
    use filmcraft_project::ItemId;
    use web_time::Instant;

    /// Feed `n` refreshes of consecutive grid frames from `from`, each `ready` or not, 20 ms
    /// apart from `t0`; returns the step in force after.
    fn feed(c: &mut SkimControl, speed: f64, from: i64, n: i64, ready: bool, ahead: bool, t0: Instant) -> i64 {
        let mut step = c.step(speed);
        let mut f = from;
        for i in 0..n {
            step = c.refresh(speed, f, ready, ahead, t0 + std::time::Duration::from_millis(20 * i as u64));
            f += step;
        }
        step
    }

    #[test]
    fn skimming_starts_at_the_natural_step_and_snaps_to_its_grid() {
        assert_eq!([0.5, 1.0, -1.0, 2.0, -2.0, 4.0, 8.0].map(SkimControl::base_step), [1, 1, 1, 2, 2, 4, 8]);
        assert_eq!([SkimControl::snap(7, 1), SkimControl::snap(7, 2), SkimControl::snap(29, 15), SkimControl::snap(-3, 4)], [7, 6, 15, -4]);
        assert_eq!([1, 2, 3, 4, 15, 30].map(SkimControl::tolerance), [0, 1, 1, 2, 7, 15]);
        // normal speed and slower never skip, whatever happens
        let mut c = SkimControl::default();
        assert_eq!(feed(&mut c, 1.0, 0, 40, false, false, Instant::now()), 1);
        assert_eq!(feed(&mut c, -1.0, 100, 40, false, false, Instant::now()), 1);
    }

    #[test]
    fn skimming_steps_up_when_frames_are_late_and_back_down_when_calm() {
        let mut c = SkimControl::default();
        let t0 = Instant::now();
        assert_eq!(c.step(4.0), 4);
        // three late grid frames in a row: one rung up
        let up = feed(&mut c, 4.0, 0, 4, false, false, t0);
        assert_eq!(up, 5);
        // still late while the new step settles: far behind (6 of 8 late) once it has settled,
        // two rungs at once
        assert_eq!(feed(&mut c, 4.0, 100, 9, false, false, t0 + std::time::Duration::from_millis(100)), 5);
        let t1 = t0 + super::SKIM_SETTLE + std::time::Duration::from_millis(200);
        let far = feed(&mut c, 4.0, 200, 1, false, false, t1);
        assert_eq!(far, 8);
        // calm with frames ready well ahead: one rung down (not below the natural step)
        let t2 = t1 + std::time::Duration::from_secs(1);
        let calm = feed(&mut c, 4.0, 1000, super::SKIM_CALM as i64 + 2, true, true, t2);
        assert!(calm < far && calm >= 4, "{calm} after {far}");
        // remembered for the next time the speed is played
        let mut again = SkimControl { remembered: c.remembered.clone(), ..Default::default() };
        assert_eq!(again.step(4.0), calm);
        // a step down that has to be undone makes the next one wait twice as long
        let t3 = t2 + std::time::Duration::from_secs(5);
        feed(&mut c, 4.0, 5000, 4, false, false, t3);
        assert_eq!(c.calm, super::SKIM_CALM * 2);
    }

    #[test]
    fn reverse_playback_prefetches_more_than_a_gop() {
        assert!(super::prefetch_depth(-1.0, 1) >= 35);
        assert_eq!(super::prefetch_depth(-2.0, 2) * 2, super::REVERSE_PREFETCH);
        assert_eq!(super::prefetch_depth(1.0, 1), 14);
        assert_eq!(super::prefetch_depth(4.0, 15), 12);
        assert!(super::preroll_timeout(-1.0) > super::preroll_timeout(1.0));
    }

    #[test]
    fn plans_playback_has_passed_are_evicted_first() {
        let mut c = super::PlanCache::default();
        let key = |frame| super::FrameKey {
            target: super::Target::SequencePlan(filmcraft_project::ItemId(1)),
            frame,
            size: 1000,
            revision: 0,
            draft: false,
            temporal: false,
        };
        let plan = || super::GpuPlan {
            plan: filmcraft_render::plan::FramePlan::Image(filmcraft_render::Image::new(1, 1)),
            prepared: filmcraft_gpu::PreparedPlan::default(),
        };
        // reverse playback at frame 100: 40 frames prefetched below it, made first
        for f in (60..100).rev() {
            c.insert(key(f), plan());
        }
        // then the frames shown, playing down from 160 (each used as it is shown)
        for f in (100..160).rev() {
            c.insert(key(f), plan());
            let _ = c.get(&key(f));
            c.retire_passed(key(0).target, f, -1, 2);
        }
        c.insert(key(59), plan());
        assert!((59..100).all(|f| c.map.contains_key(&key(f))), "the frames still to be shown stay");
        assert!(c.map.len() <= super::PlanCache::MAX);
        // of the frames shown, only the few nearest the playhead are kept
        let passed = (101..160).filter(|f| c.map.contains_key(&key(*f))).count();
        assert!(passed <= super::KEEP_PASSED + 2, "{passed} passed plans kept");
    }

    #[test]
    fn meter_follows_the_skimming_step() {
        let mut m = PlaybackMeter::default();
        m.start(-4.0);
        m.set_step(15);
        for f in [150, 135, 120, 90] {
            m.refresh(f, true);
        }
        m.finish();
        // 105 was passed over without a refresh
        assert_eq!((m.shown, m.dropped), (4, 1));
    }

    /// A dropped frame server stops its workers and empties its caches (they used to run for the
    /// life of the process, keeping every decoded frame and GPU surface they had cached).
    /// Each worker holds a reference to the server's shared state until it exits, so the test
    /// counts this server's workers only (counting threads by name raced with other tests'
    /// frame servers when the suite runs in parallel).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dropping_the_server_stops_its_workers() {
        let s = filmcraft_engine::Session::default();
        let server = super::FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), 3);
        let shared = std::sync::Arc::downgrade(&server.shared);
        // the server's own reference plus one per worker
        assert_eq!(shared.strong_count(), 4, "three workers started");
        drop(server);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while shared.strong_count() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(shared.strong_count(), 0, "the workers exited");
    }

    /// The app waits for its frame workers when it closes (fork 2026-10-13: a dropped app's
    /// workers kept rendering for seconds, and the UI test binary, one app per test, piled them up
    /// until it ran out of memory).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn stop_and_wait_returns_once_the_workers_exited() {
        let s = filmcraft_engine::Session::default();
        let server = super::FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), 3);
        let shared = std::sync::Arc::downgrade(&server.shared);
        assert!(server.stop_and_wait(std::time::Duration::from_secs(5)), "the workers exited in time");
        assert_eq!(shared.strong_count(), 1, "only the server itself still holds the shared state");
    }

    #[test]
    fn only_item_thumbnails_are_background_work() {
        let item = Target::Item(ItemId(1));
        // thumbnails (50) and the Media Browser (40)
        assert!(background_work(50, item));
        assert!(background_work(40, item));
        // the Source monitor shows an item at a foreground priority
        assert!(!background_work(1, item));
        // the scopes ask for the program frame at 40 while playing: never background
        assert!(!background_work(40, Target::Sequence(ItemId(1))));
        assert!(!background_work(50, Target::SequencePlan(ItemId(1))));
    }

    #[test]
    fn draft_decoding_only_for_reduced_resolution_playback() {
        use super::draft_playback;
        assert!(draft_playback(true, 0.5, true) && draft_playback(true, 0.25, true));
        assert!(!draft_playback(true, 1.0, true), "full resolution is never draft");
        assert!(!draft_playback(false, 0.5, true), "a paused frame is never draft");
        assert!(!draft_playback(true, 0.5, false), "off unless enabled");
        assert!(!filmcraft_engine::autosave::Preferences::default().playback.draft_decode, "off by default");
    }

    #[test]
    fn temporal_renders_only_for_the_paused_unscrubbed_frame() {
        use super::temporal_frame;
        assert!(temporal_frame(false, false, true));
        assert!(!temporal_frame(true, false, true), "never while playing");
        assert!(!temporal_frame(false, true, true), "never while scrubbing / dragging");
        assert!(!temporal_frame(false, false, false), "only where a clip uses it");
    }

    #[test]
    fn playback_plan_skips_frames_only_when_workers_cannot_keep_up() {
        // decode-only / light frames: every frame from the playhead on
        assert_eq!(playback_plan(0.0, 24.0, 6), (1, 1));
        assert_eq!(playback_plan(0.15, 24.0, 6), (1, 1));
        // 0.5 s of effects per frame at 24 fps on 6 workers: 12 frames pass while one renders,
        // so start 12 ahead, and every 3rd frame is what 6 workers can finish
        assert_eq!(playback_plan(0.5, 24.0, 6), (12, 3));
    }

    #[test]
    fn meter_resync_does_not_count_hidden_time() {
        let mut m = PlaybackMeter::default();
        m.start(1.0);
        m.refresh(0, true);
        m.resync(60, false); // window hidden for 2.5 s
        m.refresh(60, true);
        m.refresh(61, true);
        m.finish();
        assert_eq!((m.shown, m.dropped), (2, 0));
    }

    #[test]
    fn meter_counts_frames_not_refreshes() {
        // 24 fps on a 60 Hz display: each frame is due for 2–3 refreshes.
        let mut m = PlaybackMeter::default();
        m.start(1.0);
        for (f, exact) in [(0, true), (0, true), (0, true), (1, false), (1, true), (2, false), (2, false), (3, true)] {
            m.refresh(f, exact);
        }
        m.finish();
        // frame 1 arrived late but was on screen while due → shown; frame 2 never → dropped.
        assert_eq!((m.shown, m.dropped), (3, 1));
    }

    #[test]
    fn meter_counts_skipped_frames_but_not_speed_steps_or_loops() {
        let mut m = PlaybackMeter::default();
        m.start(1.0);
        m.refresh(10, true);
        m.refresh(14, true); // no refresh during 11..13 (UI stalled)
        m.refresh(0, true); // loop restart
        m.finish();
        assert_eq!((m.shown, m.dropped), (3, 3));
        let mut m = PlaybackMeter::default();
        m.start(2.0); // 2× steps over every other frame on purpose
        for f in [0, 2, 4, 6] {
            m.refresh(f, true);
        }
        m.finish();
        assert_eq!((m.shown, m.dropped), (4, 0));
    }
}
