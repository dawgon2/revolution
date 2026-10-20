//! The background media worker (fork Phase 6.2 / 6.3): automatic proxies and the render cache.
//!
//! **One worker, one job at a time.** Jobs are *stepped* ([`BackgroundJob::step`] does one batch,
//! e.g. one `Exporter::step`), and the worker picks the job to step next from a priority queue
//! every time:
//! 1. proxies before render-cache parts ([`JobKind`]);
//! 2. 4:2:2 sources first ([`JobSpec::chroma_422`]: they are what playback cannot decode fast);
//! 3. a job already started before one that has not (no pile of half-written files);
//! 4. the caller's [`JobSpec::rank`] (smaller first: the cache uses the distance to the playhead);
//! 5. first come, first served.
//!
//! A higher-priority job submitted while another runs takes over at the next batch boundary; the
//! other keeps its state and continues later.
//!
//! **Never in the way.** While [`Activity`] says the user plays, scrubs (a playhead move within
//! [`WorkerConfig::scrub_window`], 300 ms) or exports, the worker finishes the batch in hand and
//! then waits; it resumes [`WorkerConfig::resume_delay`] (500 ms) after the activity clears. A
//! burst of hardware decoding while playing made compositing wait 40–100 ms per frame (HANDOFF:
//! "GPU stall with the video engine flat out"), so each batch also runs inside the decode hints
//! for work nobody waits on: [`filmcraft_media::cancel::with_background`], a hardware-decode pace
//! ([`filmcraft_media::cancel::with_pace`]) and no deadline, which `filmcraft_codecs::hwgate`
//! serves last. The hints are thread-local: work a step hands to other threads (an export renders
//! its batch on the rayon pool) does not see them.
//!
//! The worker also waits while the user has paused it ([`BackgroundWorker::pause`]), and it never
//! busy-waits (a condition variable, with a timeout only while a resume delay runs).
//!
//! **Never crash.** Every step, every cancel clean-up and every drop of a job runs under
//! `catch_unwind`: a job that panics is reported as failed and the worker carries on. Outcomes are
//! collected by [`BackgroundWorker::take_finished`].
//!
//! The thread starts with the first job ([`BackgroundWorker::submit`]); a host without threads
//! (web) gets an error from `submit`. The worker stops when the last handle is dropped (queued
//! jobs are cancelled then).

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use filmcraft_time::Tick;
use serde::Serialize;
use web_time::Instant;

/// What a job produces; the order is the priority (proxies first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    /// A proxy transcode (6.2).
    Proxy,
    /// A render-cache part (6.3).
    Cache,
}

/// How a job is queued.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSpec {
    pub kind: JobKind,
    /// Shown in status lists (already translated by the caller).
    pub label: String,
    /// The source is 4:2:2 (served before other jobs of its kind).
    pub chroma_422: bool,
    /// Smaller first within a kind (e.g. the distance of a cache part from the playhead, in
    /// frames); 0 when the caller has no order.
    pub rank: u64,
}

impl JobSpec {
    pub fn new(kind: JobKind, label: impl Into<String>) -> Self {
        Self { kind, label: label.into(), chroma_422: false, rank: 0 }
    }
}

/// What one step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStep {
    /// A batch is done; step again.
    More,
    /// The job is complete.
    Done,
}

/// A job the worker advances one batch at a time. Runs on the worker thread.
pub trait BackgroundJob: Send {
    /// Do one batch (keep it short: the worker only pauses between batches). `Err` ends the job
    /// as failed. Long work inside a step may poll [`filmcraft_media::cancel::cancelled`].
    fn step(&mut self) -> Result<JobStep, String>;
    /// Progress 0–1 (shown in the status).
    fn fraction(&self) -> f32 {
        0.0
    }
    /// The job was cancelled (or the worker stopped) before it finished: clean up (e.g. delete a
    /// partial output file).
    fn cancelled(&mut self) {}
}

/// A job's id, unique within its worker.
pub type JobId = u64;

/// What the user is doing that background work must not compete with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    /// Program or Source playback.
    pub playing: bool,
    /// The playhead was moved by hand (counts for [`WorkerConfig::scrub_window`] after the move).
    pub scrubbing: bool,
    /// An export (or another foreground media job) is running.
    pub export_running: bool,
}

/// Timing of the worker.
#[derive(Clone, Copy, Debug)]
pub struct WorkerConfig {
    /// How long the worker keeps waiting after the activity clears.
    pub resume_delay: Duration,
    /// How long a playhead move counts as scrubbing.
    pub scrub_window: Duration,
    /// Hardware-decode pace inside a step ([`filmcraft_media::cancel::with_pace`]).
    pub pace: Option<Duration>,
    /// The machine's memory (total, available bytes) before each step; the worker waits while it
    /// is low ([`memory_low`]). None: never checked (tests, platforms without the number).
    pub memory: Option<fn() -> Option<(u64, u64)>>,
}

/// Free memory under which the worker waits (owner, fork XPS sign-off: on the 16 GB XPS 16 the
/// render cache's 6.2K parts and proxies ran next to playback and an export until the machine
/// swapped and the Arc GPU ran out of memory): less than 3 GB available, or less than a quarter
/// of the machine's memory (4 GB on 16 GB). Smaller machines back off sooner.
pub fn memory_low(total: u64, available: u64) -> bool {
    available < (3u64 << 30).max(total / 4)
}

/// Whether this machine's memory is low now ([`memory_low`] on `/proc/meminfo`; false where the
/// numbers aren't known). For other background work that should wait too (the Project
/// Manager's posters).
pub fn memory_low_now() -> bool {
    #[cfg(target_os = "linux")]
    {
        meminfo().is_some_and(|(total, available)| memory_low(total, available))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// How long the worker waits before it looks at the memory again.
const MEMORY_RECHECK: Duration = Duration::from_secs(2);

/// `MemTotal` and `MemAvailable` of `/proc/meminfo`, in bytes.
#[cfg(target_os = "linux")]
fn meminfo() -> Option<(u64, u64)> {
    parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

fn parse_meminfo(s: &str) -> Option<(u64, u64)> {
    let field = |name: &str| -> Option<u64> {
        let line = s.lines().find(|l| l.starts_with(name))?;
        let kb: u64 = line.get(name.len()..)?.trim().trim_end_matches("kB").trim().parse().ok()?;
        kb.checked_mul(1024)
    };
    Some((field("MemTotal:")?, field("MemAvailable:")?))
}

impl Default for WorkerConfig {
    fn default() -> Self {
        // pace: at most 250 pictures/s submitted by the worker's own thread (6.2K decodes at
        // ~45-60 fps, so it only spreads the bursts of small sources). Provisional (fork 6.2).
        Self {
            resume_delay: Duration::from_millis(500),
            scrub_window: Duration::from_millis(300),
            pace: Some(Duration::from_millis(4)),
            #[cfg(target_os = "linux")]
            memory: Some(meminfo),
            #[cfg(not(target_os = "linux"))]
            memory: None,
        }
    }
}

/// How a job ended.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome", content = "error")]
pub enum JobOutcome {
    Done,
    Failed(String),
    Cancelled,
}

/// A job that ended (see [`BackgroundWorker::take_finished`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finished {
    pub id: JobId,
    pub spec: JobSpec,
    #[serde(flatten)]
    pub outcome: JobOutcome,
}

/// A queued or running job.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: JobId,
    #[serde(flatten)]
    pub spec: JobSpec,
    /// At least one step ran.
    pub started: bool,
    /// Being stepped right now.
    pub running: bool,
    pub fraction: f32,
}

/// What the worker is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkerState {
    /// No jobs.
    Idle,
    /// Stepping jobs.
    Working,
    /// Paused by the user.
    Paused,
    /// Waiting for the user's activity (playing, scrubbing, exporting) to clear.
    Waiting,
    /// The activity cleared; resuming after the resume delay.
    Resuming,
    /// Waiting for free memory ([`memory_low`]).
    LowMemory,
}

/// A snapshot for status displays and commands.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatus {
    pub state: WorkerState,
    pub paused: bool,
    pub activity: Activity,
    /// In the order they will run (the running job first).
    pub jobs: Vec<JobInfo>,
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
}

/// Times the worker loop is restarted after an internal error before its thread gives up.
const MAX_RESTARTS: usize = 3;

/// Outcomes kept for [`BackgroundWorker::take_finished`] (oldest dropped first).
const MAX_FINISHED: usize = 256;

struct Entry {
    id: JobId,
    spec: JobSpec,
    seq: u64,
    started: bool,
    fraction: f32,
    /// None while the worker steps it.
    job: Option<Box<dyn BackgroundJob>>,
    cancel: Arc<AtomicBool>,
}

impl Entry {
    fn key(&self) -> (JobKind, bool, bool, u64, u64) {
        (self.spec.kind, !self.spec.chroma_422, !self.started, self.spec.rank, self.seq)
    }
}

enum Hold {
    No,
    Indefinite,
    Until(Instant),
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    running: Option<JobId>,
    /// The worker holds a job (or cancelled jobs) outside the lock: stepping, or cleaning up.
    in_hand: bool,
    paused: bool,
    playing: bool,
    export_running: bool,
    /// When playing / exporting last cleared.
    cleared_at: Option<Instant>,
    last_scrub: Option<Instant>,
    last_playhead: Option<Tick>,
    /// Jobs taken off the queue by `cancel`, for the worker to clean up outside the lock.
    graveyard: Vec<(JobId, JobSpec, Box<dyn BackgroundJob>)>,
    finished: VecDeque<Finished>,
    completed: u64,
    failed: u64,
    cancelled: u64,
    next_id: JobId,
    next_seq: u64,
    started: bool,
    shutdown: bool,
    /// The last memory check found it low (the worker waits).
    low_memory: bool,
}

impl State {
    fn scrubbing(&self, now: Instant, cfg: &WorkerConfig) -> bool {
        self.last_scrub.is_some_and(|t| now.saturating_duration_since(t) < cfg.scrub_window)
    }

    fn hold(&self, now: Instant, cfg: &WorkerConfig) -> Hold {
        if self.paused || self.playing || self.export_running {
            return Hold::Indefinite;
        }
        let after_clear = self.cleared_at.and_then(|t| t.checked_add(cfg.resume_delay));
        let after_scrub = self.last_scrub.and_then(|t| t.checked_add(cfg.scrub_window.saturating_add(cfg.resume_delay)));
        match after_clear.into_iter().chain(after_scrub).max() {
            Some(t) if t > now => Hold::Until(t),
            _ => Hold::No,
        }
    }

    /// The entry to step next.
    fn pick(&self) -> Option<usize> {
        self.entries.iter().enumerate().filter(|(_, e)| e.job.is_some() && !e.cancel.load(Ordering::Relaxed)).min_by_key(|(_, e)| e.key()).map(|(i, _)| i)
    }

    fn finish(&mut self, id: JobId, spec: JobSpec, outcome: JobOutcome) {
        match outcome {
            JobOutcome::Done => self.completed += 1,
            JobOutcome::Failed(_) => self.failed += 1,
            JobOutcome::Cancelled => self.cancelled += 1,
        }
        if self.finished.len() >= MAX_FINISHED {
            self.finished.pop_front();
        }
        self.finished.push_back(Finished { id, spec, outcome });
    }

    fn set_busy(&mut self, playing: bool, export_running: bool, now: Instant) {
        let was = self.playing || self.export_running;
        self.playing = playing;
        self.export_running = export_running;
        if was && !(playing || export_running) {
            self.cleared_at = Some(now);
        }
    }
}

struct Shared {
    state: Mutex<State>,
    /// Wakes the worker (new job, activity cleared, resume, cancel, shutdown).
    wake: Condvar,
    /// Signalled after every step and every finished job (waiters in [`BackgroundWorker::wait_idle`]).
    changed: Condvar,
    cfg: WorkerConfig,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Stops the worker when the last handle goes.
struct Handle {
    shared: Arc<Shared>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.stop();
    }
}

impl Shared {
    /// Stop the worker: nothing new starts, queued jobs are cleaned up, and the running job is
    /// cancelled, so it stops at its next cancellation check instead of finishing its step (a
    /// render-cache batch is seconds of work nobody is left to want).
    fn stop(&self) {
        let mut st = self.lock();
        st.shutdown = true;
        for e in &st.entries {
            e.cancel.store(true, Ordering::Relaxed);
        }
        drop(st);
        self.wake.notify_all();
        self.changed.notify_all();
    }
}

/// The background media worker. Cheap to clone (all clones drive the same worker).
#[derive(Clone)]
pub struct BackgroundWorker {
    handle: Arc<Handle>,
}

impl Default for BackgroundWorker {
    fn default() -> Self {
        Self::new(WorkerConfig::default())
    }
}

impl std::fmt::Debug for BackgroundWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundWorker").field("status", &self.status()).finish()
    }
}

impl BackgroundWorker {
    /// A worker with `cfg` (its thread starts with the first job).
    pub fn new(cfg: WorkerConfig) -> Self {
        let shared = Arc::new(Shared { state: Mutex::new(State { next_id: 1, ..Default::default() }), wake: Condvar::new(), changed: Condvar::new(), cfg });
        Self { handle: Arc::new(Handle { shared }) }
    }

    fn shared(&self) -> &Shared {
        &self.handle.shared
    }

    /// Queue a job. Errors when the worker thread cannot start (a host without threads).
    pub fn submit(&self, spec: JobSpec, job: Box<dyn BackgroundJob>) -> Result<JobId, String> {
        let mut st = self.shared().lock();
        if st.shutdown {
            return Err("the background worker has stopped".into());
        }
        if !st.started {
            let shared = self.handle.shared.clone();
            std::thread::Builder::new()
                .name("filmcraft-background".into())
                .spawn(move || {
                    // jobs are guarded one by one; this only catches a bug in the loop itself:
                    // the job in hand is failed and the loop starts again (a few times; then the
                    // next `submit` starts a new thread)
                    for _ in 0..MAX_RESTARTS {
                        if guarded(|| run(shared.clone())).is_some() {
                            // stopped: [`BackgroundWorker::stop_and_wait`] waits for this
                            shared.lock().started = false;
                            shared.changed.notify_all();
                            return;
                        }
                        log::error!("the background media worker hit an internal error; restarting it");
                        recover(&shared);
                    }
                    shared.lock().started = false;
                    shared.changed.notify_all();
                })
                .map_err(|e| format!("background worker: {e}"))?;
            st.started = true;
        }
        let id = st.next_id;
        st.next_id = st.next_id.saturating_add(1);
        let seq = st.next_seq;
        st.next_seq = st.next_seq.saturating_add(1);
        st.entries.push(Entry { id, spec, seq, started: false, fraction: 0.0, job: Some(job), cancel: Arc::new(AtomicBool::new(false)) });
        drop(st);
        self.shared().wake.notify_all();
        Ok(id)
    }

    /// Cancel a queued or running job (a running one stops after its batch). Returns whether the
    /// job was known.
    pub fn cancel(&self, id: JobId) -> bool {
        self.cancel_where(|e| e.id == id) > 0
    }

    /// Cancel every job of `kind`; returns how many.
    pub fn cancel_kind(&self, kind: JobKind) -> usize {
        self.cancel_where(|e| e.spec.kind == kind)
    }

    /// Cancel every job; returns how many.
    pub fn cancel_all(&self) -> usize {
        self.cancel_where(|_| true)
    }

    fn cancel_where(&self, f: impl Fn(&Entry) -> bool) -> usize {
        let mut st = self.shared().lock();
        let mut n = 0;
        let mut i = 0;
        while i < st.entries.len() {
            let Some(e) = st.entries.get(i) else { break };
            if !f(e) || e.cancel.load(Ordering::Relaxed) {
                i += 1;
                continue;
            }
            e.cancel.store(true, Ordering::Relaxed);
            n += 1;
            if e.job.is_some() {
                // not running: off the queue now, cleaned up by the worker
                let e = st.entries.remove(i);
                if let Some(job) = e.job {
                    st.graveyard.push((e.id, e.spec, job));
                }
            } else {
                i += 1;
            }
        }
        drop(st);
        if n > 0 {
            self.shared().wake.notify_all();
        }
        n
    }

    /// Stop starting batches until [`Self::resume`] (the batch in hand finishes).
    pub fn pause(&self) {
        self.shared().lock().paused = true;
    }

    /// Undo [`Self::pause`].
    pub fn resume(&self) {
        self.shared().lock().paused = false;
        self.shared().wake.notify_all();
    }

    /// Report what the user is doing (call whenever it may have changed; cheap when it did not).
    /// `scrubbing` counts as a playhead move now.
    pub fn set_activity(&self, a: Activity) {
        let now = Instant::now();
        let mut st = self.shared().lock();
        if a.scrubbing {
            st.last_scrub = Some(now);
        }
        let cleared = (st.playing || st.export_running) && !(a.playing || a.export_running);
        st.set_busy(a.playing, a.export_running, now);
        drop(st);
        if cleared {
            self.shared().wake.notify_all();
        }
    }

    /// The UI's once-a-frame hook: playing, exporting, and the playhead (a move while not playing
    /// is a scrub).
    pub fn observe(&self, playing: bool, export_running: bool, playhead: Option<Tick>) {
        let mut st = self.shared().lock();
        let moved = playhead.is_some() && st.last_playhead.is_some() && st.last_playhead != playhead;
        st.last_playhead = playhead;
        let changed = (moved && !playing) || st.playing != playing || st.export_running != export_running;
        drop(st);
        if changed {
            self.set_activity(Activity { playing, scrubbing: moved && !playing, export_running });
        }
    }

    /// The playhead was moved by hand.
    pub fn note_scrub(&self) {
        self.shared().lock().last_scrub = Some(Instant::now());
    }

    /// Outcomes of the jobs that ended since the last call (oldest first).
    pub fn take_finished(&self) -> Vec<Finished> {
        self.shared().lock().finished.drain(..).collect()
    }

    /// What the user is doing as the worker sees it (cheap: no job list; call every frame).
    pub fn activity(&self) -> Activity {
        let cfg = self.shared().cfg;
        let st = self.shared().lock();
        Activity { playing: st.playing, scrubbing: st.scrubbing(Instant::now(), &cfg), export_running: st.export_running }
    }

    /// The worker's state and its jobs, in the order they will run.
    pub fn status(&self) -> WorkerStatus {
        let cfg = self.shared().cfg;
        let st = self.shared().lock();
        let now = Instant::now();
        let mut order: Vec<&Entry> = st.entries.iter().filter(|e| !e.cancel.load(Ordering::Relaxed)).collect();
        order.sort_by_key(|e| (st.running != Some(e.id), e.key()));
        let jobs: Vec<JobInfo> = order
            .iter()
            .map(|e| JobInfo { id: e.id, spec: e.spec.clone(), started: e.started, running: st.running == Some(e.id), fraction: e.fraction })
            .collect();
        let state = if st.running.is_some() {
            WorkerState::Working
        } else if jobs.is_empty() {
            WorkerState::Idle
        } else if st.paused {
            WorkerState::Paused
        } else {
            match st.hold(now, &cfg) {
                Hold::Indefinite => WorkerState::Waiting,
                Hold::Until(_) if st.scrubbing(now, &cfg) => WorkerState::Waiting,
                Hold::Until(_) => WorkerState::Resuming,
                Hold::No if st.low_memory => WorkerState::LowMemory,
                Hold::No => WorkerState::Working,
            }
        };
        WorkerStatus {
            state,
            paused: st.paused,
            activity: Activity { playing: st.playing, scrubbing: st.scrubbing(now, &cfg), export_running: st.export_running },
            jobs,
            completed: st.completed,
            failed: st.failed,
            cancelled: st.cancelled,
        }
    }

    /// Whether no job is queued or running.
    pub fn is_idle(&self) -> bool {
        let st = self.shared().lock();
        st.entries.is_empty() && st.graveyard.is_empty() && !st.in_hand
    }

    /// Stop the worker for good (its session is closing; every clone stops with it): queued jobs
    /// are cancelled and cleaned up, the running one is cancelled, and this waits at most
    /// `timeout` for the worker thread to finish. Returns whether it did (or never started).
    pub fn stop_and_wait(&self, timeout: Duration) -> bool {
        self.shared().stop();
        let start = Instant::now();
        let end = start.checked_add(timeout).unwrap_or(start);
        let mut st = self.shared().lock();
        loop {
            if !st.started {
                return true;
            }
            let now = Instant::now();
            if now >= end {
                return false;
            }
            st = self.shared().changed.wait_timeout(st, end.saturating_duration_since(now)).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Block until no job is queued or running, at most `timeout` (CLI `wait`, tests). Returns
    /// whether the worker is idle.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        let end = start.checked_add(timeout).or_else(|| start.checked_add(Duration::from_secs(86_400))).unwrap_or(start);
        let mut st = self.shared().lock();
        loop {
            if st.entries.is_empty() && st.graveyard.is_empty() && !st.in_hand {
                return true;
            }
            let now = Instant::now();
            if now >= end || st.shutdown {
                return false;
            }
            st = self.shared().changed.wait_timeout(st, end.saturating_duration_since(now)).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// Whether a foreground media job runs in the session (an export, a render of previews, a proxy
/// job started from the Clip menu…): [`Activity::export_running`] for the UI hook. Jobs of the
/// background worker itself must not be listed unfinished in `Session::jobs`, or they would hold
/// the worker.
pub fn foreground_jobs_running(s: &crate::Session) -> bool {
    !s.stepped.is_empty() || s.jobs.iter().any(|j| !j.progress.finished.load(Ordering::Relaxed))
}

/// Run `f` and swallow a panic (the panic hook has logged it).
fn guarded<R>(f: impl FnOnce() -> R) -> Option<R> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

/// Clean up a job that will not finish, outside the lock.
fn retire(mut job: Box<dyn BackgroundJob>) {
    let _ = guarded(|| job.cancelled());
    let _ = guarded(move || drop(job));
}

/// After the worker loop itself panicked: the job it held is lost, so fail it, and clear the
/// running / in-hand marks (or `wait_idle` would never return and `status` would say Working).
fn recover(shared: &Shared) {
    let mut st = shared.lock();
    st.running = None;
    st.in_hand = false;
    let mut i = 0;
    while i < st.entries.len() {
        if st.entries.get(i).is_some_and(|e| e.job.is_none()) {
            let e = st.entries.remove(i);
            st.finish(e.id, e.spec, JobOutcome::Failed("internal error (see the crash log); the job was stopped".into()));
        } else {
            i += 1;
        }
    }
    drop(st);
    shared.changed.notify_all();
}

fn run(shared: Arc<Shared>) {
    let cfg = shared.cfg;
    let mut st = shared.lock();
    loop {
        if !st.graveyard.is_empty() {
            let dead = std::mem::take(&mut st.graveyard);
            st.in_hand = true;
            drop(st);
            let mut ended = Vec::new();
            for (id, spec, job) in dead {
                retire(job);
                ended.push((id, spec));
            }
            st = shared.lock();
            for (id, spec) in ended {
                st.finish(id, spec, JobOutcome::Cancelled);
            }
            st.in_hand = false;
            shared.changed.notify_all();
            continue;
        }
        if st.shutdown {
            break;
        }
        let now = Instant::now();
        match st.hold(now, &cfg) {
            Hold::Indefinite => {
                st = shared.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            Hold::Until(t) => {
                st = shared.wake.wait_timeout(st, t.saturating_duration_since(now)).unwrap_or_else(PoisonError::into_inner).0;
                continue;
            }
            Hold::No => {}
        }
        // free memory first: a step would add a decoder and an encoder of the source's size
        let low = cfg.memory.and_then(|f| f()).is_some_and(|(total, available)| memory_low(total, available));
        if low != st.low_memory {
            st.low_memory = low;
            shared.changed.notify_all();
        }
        if low && st.pick().is_some() {
            st = shared.wake.wait_timeout(st, MEMORY_RECHECK).unwrap_or_else(PoisonError::into_inner).0;
            continue;
        }
        let Some(i) = st.pick() else {
            st = shared.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let Some(e) = st.entries.get_mut(i) else { continue };
        let Some(mut job) = e.job.take() else { continue };
        e.started = true;
        let (id, cancel) = (e.id, e.cancel.clone());
        st.running = Some(id);
        st.in_hand = true;
        drop(st);

        let stepped = guarded(|| {
            filmcraft_media::cancel::with_cancel(&cancel, || {
                filmcraft_media::cancel::with_background(true, || filmcraft_media::cancel::with_pace(cfg.pace, || job.step()))
            })
        });
        let fraction = guarded(|| job.fraction()).filter(|f| f.is_finite()).map_or(0.0, |f| f.clamp(0.0, 1.0));

        st = shared.lock();
        st.running = None;
        let cancelled = cancel.load(Ordering::Relaxed);
        let Some(pos) = st.entries.iter().position(|e| e.id == id) else {
            // cannot happen (a running entry stays listed); clean up anyway
            drop(st);
            retire(job);
            st = shared.lock();
            st.in_hand = false;
            continue;
        };
        match stepped {
            Some(Ok(JobStep::More)) if !cancelled => {
                if let Some(e) = st.entries.get_mut(pos) {
                    e.job = Some(job);
                    e.fraction = fraction;
                }
            }
            Some(Ok(JobStep::Done)) => {
                let e = st.entries.remove(pos);
                st.finish(id, e.spec, JobOutcome::Done);
                drop(st);
                let _ = guarded(move || drop(job));
                st = shared.lock();
            }
            Some(Ok(JobStep::More)) | Some(Err(_)) | None => {
                let e = st.entries.remove(pos);
                let outcome = match stepped {
                    _ if cancelled => JobOutcome::Cancelled,
                    Some(Err(why)) => JobOutcome::Failed(why),
                    _ => JobOutcome::Failed("internal error (see the crash log); the job was stopped".into()),
                };
                drop(st);
                // a failed or panicked job may have left partial output too
                retire(job);
                st = shared.lock();
                st.finish(id, e.spec, outcome);
            }
        }
        st.in_hand = false;
        shared.changed.notify_all();
    }
    // stopped: cancel what is left
    let entries = std::mem::take(&mut st.entries);
    let graveyard = std::mem::take(&mut st.graveyard);
    let left: Vec<Box<dyn BackgroundJob>> = entries.into_iter().filter_map(|e| e.job).chain(graveyard.into_iter().map(|(_, _, j)| j)).collect();
    drop(st);
    for job in left {
        retire(job);
    }
    shared.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nop;
    impl BackgroundJob for Nop {
        fn step(&mut self) -> Result<JobStep, String> {
            Ok(JobStep::Done)
        }
    }

    /// The loop's own panic path (a job cannot reach it: steps are guarded) leaves the state as if
    /// the job in hand had failed, and the worker reads idle again.
    #[test]
    fn recovery_fails_the_lost_job_and_clears_the_marks() {
        let w = BackgroundWorker::new(WorkerConfig::default());
        let shared = w.handle.shared.clone();
        {
            let mut st = shared.lock();
            for (id, job) in [(7, None), (8, Some(Box::new(Nop) as Box<dyn BackgroundJob>))] {
                st.entries.push(Entry { id, spec: JobSpec::new(JobKind::Proxy, "x"), seq: id, started: true, fraction: 0.0, job, cancel: Arc::default() });
            }
            st.running = Some(7);
            st.in_hand = true;
        }
        assert_eq!(w.status().state, WorkerState::Working);
        assert!(!w.wait_idle(Duration::from_millis(10)));
        recover(&shared);
        let st = w.status();
        assert_eq!(st.jobs.iter().map(|j| j.id).collect::<Vec<_>>(), [8], "the queued job is kept");
        assert!(!st.jobs.iter().any(|j| j.running));
        assert_eq!(st.failed, 1);
        assert!(matches!(w.take_finished().as_slice(), [Finished { id: 7, outcome: JobOutcome::Failed(_), .. }]));
        shared.lock().entries.clear();
        assert!(w.wait_idle(Duration::from_millis(10)));
        assert_eq!(w.status().state, WorkerState::Idle);
    }
}

#[cfg(test)]
mod meminfo_tests {
    #[test]
    fn meminfo_parses_and_refuses_junk() {
        let m = "MemTotal:       16179140 kB\nMemFree:  100 kB\nMemAvailable:    4000000 kB\n";
        assert_eq!(super::parse_meminfo(m), Some((16_179_140 * 1024, 4_000_000 * 1024)));
        assert_eq!(super::parse_meminfo("MemTotal: lots kB\nMemAvailable: 1 kB"), None);
        assert_eq!(super::parse_meminfo(""), None);
    }
}
