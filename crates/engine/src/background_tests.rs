//! The background media worker (fork 6.2 / 6.3) with fake jobs: order, pausing for the user's
//! activity, resuming, cancelling, and surviving a job that panics.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use filmcraft_time::Tick;
use web_time::Instant;

use crate::background::{Activity, BackgroundJob, BackgroundWorker, JobKind, JobOutcome, JobSpec, JobStep, WorkerConfig, WorkerState};

const WAIT: Duration = Duration::from_secs(10);

fn cfg() -> WorkerConfig {
    WorkerConfig { resume_delay: Duration::from_millis(60), scrub_window: Duration::from_millis(40), pace: None, memory: None }
}

#[derive(Default)]
struct Probe {
    steps: AtomicU32,
    cancelled: AtomicBool,
    log: Mutex<Vec<String>>,
}

struct Fake {
    name: String,
    steps: u32,
    done: u32,
    sleep: Duration,
    panic_at: Option<u32>,
    fail_at: Option<u32>,
    probe: Arc<Probe>,
}

impl Fake {
    fn new(name: &str, steps: u32, probe: &Arc<Probe>) -> Self {
        Self { name: name.into(), steps, done: 0, sleep: Duration::ZERO, panic_at: None, fail_at: None, probe: probe.clone() }
    }
    fn boxed(self) -> Box<dyn BackgroundJob> {
        Box::new(self)
    }
}

impl BackgroundJob for Fake {
    fn step(&mut self) -> Result<JobStep, String> {
        assert!(filmcraft_media::cancel::background(), "steps run as background work");
        self.done += 1;
        if self.panic_at == Some(self.done) {
            panic!("fake job panics (expected in this test)");
        }
        if self.fail_at == Some(self.done) {
            return Err(format!("{} failed", self.name));
        }
        std::thread::sleep(self.sleep);
        self.probe.steps.fetch_add(1, Ordering::SeqCst);
        self.probe.log.lock().unwrap().push(format!("{}{}", self.name, self.done));
        Ok(if self.done >= self.steps { JobStep::Done } else { JobStep::More })
    }
    fn fraction(&self) -> f32 {
        self.done as f32 / self.steps.max(1) as f32
    }
    fn cancelled(&mut self) {
        self.probe.cancelled.store(true, Ordering::SeqCst);
    }
}

fn spec(kind: JobKind, label: &str, chroma_422: bool, rank: u64) -> JobSpec {
    JobSpec { kind, label: label.into(), chroma_422, rank }
}

/// Wait (bounded) until `f` holds.
fn until(f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

#[test]
fn proxies_before_cache_422_first_one_job_at_a_time() {
    let w = BackgroundWorker::new(cfg());
    let p = Arc::new(Probe::default());
    w.pause();
    w.submit(spec(JobKind::Cache, "far", false, 50), Fake::new("A", 2, &p).boxed()).unwrap();
    w.submit(spec(JobKind::Cache, "near", false, 1), Fake::new("B", 2, &p).boxed()).unwrap();
    w.submit(spec(JobKind::Proxy, "420", false, 0), Fake::new("C", 2, &p).boxed()).unwrap();
    w.submit(spec(JobKind::Proxy, "422", true, 0), Fake::new("D", 2, &p).boxed()).unwrap();
    let st = w.status();
    assert_eq!(st.state, WorkerState::Paused);
    assert_eq!(st.jobs.iter().map(|j| j.spec.label.as_str()).collect::<Vec<_>>(), ["422", "420", "near", "far"]);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(p.steps.load(Ordering::SeqCst), 0, "paused: nothing runs");
    w.resume();
    assert!(w.wait_idle(WAIT));
    assert_eq!(*p.log.lock().unwrap(), ["D1", "D2", "C1", "C2", "B1", "B2", "A1", "A2"]);
    let fin = w.take_finished();
    assert_eq!(fin.len(), 4);
    assert!(fin.iter().all(|f| f.outcome == JobOutcome::Done));
    assert_eq!(w.status().completed, 4);
    assert_eq!(w.status().state, WorkerState::Idle);
}

#[test]
fn a_proxy_submitted_later_takes_over_at_the_next_batch() {
    let w = BackgroundWorker::new(cfg());
    let p = Arc::new(Probe::default());
    // long enough that it cannot finish while the test thread is descheduled
    let mut cache = Fake::new("K", 100_000, &p);
    cache.sleep = Duration::from_millis(1);
    let k = w.submit(spec(JobKind::Cache, "cache", false, 0), cache.boxed()).unwrap();
    assert!(until(|| p.steps.load(Ordering::SeqCst) >= 2));
    // hold the worker at a batch boundary, queue the proxy, let it go
    w.pause();
    assert!(until(|| w.status().state == WorkerState::Paused));
    let held = usize::try_from(p.steps.load(Ordering::SeqCst)).unwrap();
    w.submit(spec(JobKind::Proxy, "proxy", false, 0), Fake::new("P", 2, &p).boxed()).unwrap();
    w.resume();
    // the cache job continues after the proxy, from where it was
    assert!(until(|| p.steps.load(Ordering::SeqCst) as usize >= held + 4));
    assert!(w.cancel(k));
    assert!(w.wait_idle(WAIT));
    let log = p.log.lock().unwrap().clone();
    assert_eq!(log.get(held).map(String::as_str), Some("P1"), "the proxy takes over at the next batch: {log:?}");
    assert_eq!(log.get(held + 1).map(String::as_str), Some("P2"), "and runs to the end");
    assert_eq!(log.get(held + 2), Some(&format!("K{}", held + 1)), "the cache job kept its state");
    assert!(log.iter().filter(|s| s.starts_with('K')).enumerate().all(|(i, s)| *s == format!("K{}", i + 1)));
}

#[test]
fn waits_while_playing_and_resumes_after_the_delay() {
    let c = WorkerConfig { resume_delay: Duration::from_millis(300), ..cfg() };
    let w = BackgroundWorker::new(c);
    let p = Arc::new(Probe::default());
    w.set_activity(Activity { playing: true, ..Default::default() });
    w.submit(spec(JobKind::Proxy, "p", false, 0), Fake::new("P", 3, &p).boxed()).unwrap();
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(p.steps.load(Ordering::SeqCst), 0, "nothing runs while playing");
    assert_eq!(w.status().state, WorkerState::Waiting);
    assert!(w.status().activity.playing);
    let stopped = Instant::now();
    w.set_activity(Activity::default());
    assert_eq!(w.status().state, WorkerState::Resuming);
    assert!(until(|| p.steps.load(Ordering::SeqCst) > 0));
    assert!(stopped.elapsed() >= c.resume_delay, "resumed only after the resume delay");
    assert!(w.wait_idle(WAIT));
    assert_eq!(p.steps.load(Ordering::SeqCst), 3);
}

#[test]
fn pauses_between_batches_when_playback_or_an_export_starts() {
    for activity in [Activity { playing: true, ..Default::default() }, Activity { export_running: true, ..Default::default() }] {
        let w = BackgroundWorker::new(cfg());
        let p = Arc::new(Probe::default());
        let mut job = Fake::new("J", 400, &p);
        job.sleep = Duration::from_millis(3);
        w.submit(spec(JobKind::Cache, "c", false, 0), job.boxed()).unwrap();
        assert!(until(|| p.steps.load(Ordering::SeqCst) >= 3));
        w.set_activity(activity);
        // the batch in hand finishes, then nothing more
        assert!(until(|| w.status().state == WorkerState::Waiting));
        let held = p.steps.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(p.steps.load(Ordering::SeqCst), held, "no batch while {activity:?}");
        let info = w.status().jobs;
        assert!(info[0].started && !info[0].running && info[0].fraction > 0.0);
        w.set_activity(Activity::default());
        assert!(until(|| p.steps.load(Ordering::SeqCst) > held), "resumes");
        w.cancel_all();
        assert!(w.wait_idle(WAIT));
    }
}

#[test]
fn scrubbing_holds_the_worker_for_the_window_plus_the_delay() {
    let c = WorkerConfig { scrub_window: Duration::from_millis(300), ..cfg() };
    let w = BackgroundWorker::new(c);
    let p = Arc::new(Probe::default());
    let moved = Instant::now();
    w.observe(false, false, Some(Tick(0)));
    w.observe(false, false, Some(Tick(1000)));
    assert!(w.status().activity.scrubbing);
    w.submit(spec(JobKind::Proxy, "p", false, 0), Fake::new("P", 1, &p).boxed()).unwrap();
    assert!(w.wait_idle(WAIT));
    assert!(moved.elapsed() >= c.scrub_window + c.resume_delay);
    // the same playhead again is not a scrub
    w.observe(false, false, Some(Tick(1000)));
    std::thread::sleep(c.scrub_window + c.resume_delay);
    w.observe(false, false, Some(Tick(1000)));
    assert!(!w.status().activity.scrubbing);
    // playing moves the playhead too: that is playing, not scrubbing
    w.observe(true, false, Some(Tick(2000)));
    let st = w.status();
    assert!(st.activity.playing && !st.activity.scrubbing);
}

#[test]
fn user_pause_and_resume() {
    let w = BackgroundWorker::new(cfg());
    let p = Arc::new(Probe::default());
    let mut job = Fake::new("J", 400, &p);
    job.sleep = Duration::from_millis(2);
    w.submit(spec(JobKind::Cache, "c", false, 0), job.boxed()).unwrap();
    assert!(until(|| p.steps.load(Ordering::SeqCst) >= 2));
    w.pause();
    assert!(until(|| w.status().state == WorkerState::Paused));
    let held = p.steps.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(p.steps.load(Ordering::SeqCst), held);
    assert!(w.status().paused);
    w.resume();
    assert!(until(|| p.steps.load(Ordering::SeqCst) > held));
    w.cancel_all();
    assert!(w.wait_idle(WAIT));
}

#[test]
fn cancel_a_queued_and_a_running_job() {
    let w = BackgroundWorker::new(cfg());
    let (pr, pq) = (Arc::new(Probe::default()), Arc::new(Probe::default()));
    let mut running = Fake::new("R", 10_000, &pr);
    running.sleep = Duration::from_millis(2);
    let r = w.submit(spec(JobKind::Proxy, "running", false, 0), running.boxed()).unwrap();
    let q = w.submit(spec(JobKind::Cache, "queued", false, 0), Fake::new("Q", 5, &pq).boxed()).unwrap();
    assert!(until(|| pr.steps.load(Ordering::SeqCst) >= 2));
    assert!(w.cancel(q));
    assert!(w.cancel(r));
    assert!(!w.cancel(r), "already cancelled");
    assert!(!w.cancel(9999));
    assert!(w.wait_idle(WAIT));
    assert!(pr.cancelled.load(Ordering::SeqCst) && pq.cancelled.load(Ordering::SeqCst), "both cleaned up");
    assert_eq!(pq.steps.load(Ordering::SeqCst), 0);
    assert!(pr.steps.load(Ordering::SeqCst) < 10_000);
    let fin = w.take_finished();
    assert_eq!(fin.len(), 2);
    assert!(fin.iter().all(|f| f.outcome == JobOutcome::Cancelled));
    assert_eq!(w.status().cancelled, 2);
}

#[test]
fn cancel_works_while_the_worker_waits() {
    let w = BackgroundWorker::new(cfg());
    let p = Arc::new(Probe::default());
    w.set_activity(Activity { playing: true, ..Default::default() });
    w.submit(spec(JobKind::Proxy, "p", false, 0), Fake::new("P", 3, &p).boxed()).unwrap();
    assert_eq!(w.cancel_kind(JobKind::Proxy), 1);
    assert!(w.wait_idle(WAIT), "cleaned up while playing");
    assert!(p.cancelled.load(Ordering::SeqCst));
    assert_eq!(p.steps.load(Ordering::SeqCst), 0);
}

#[test]
fn a_panicking_job_does_not_kill_the_worker() {
    let w = BackgroundWorker::new(cfg());
    let (pa, pb, pc) = (Arc::new(Probe::default()), Arc::new(Probe::default()), Arc::new(Probe::default()));
    let mut bad = Fake::new("X", 5, &pa);
    bad.panic_at = Some(2);
    let mut failing = Fake::new("F", 5, &pc);
    failing.fail_at = Some(1);
    let x = w.submit(spec(JobKind::Proxy, "bad", true, 0), bad.boxed()).unwrap();
    let f = w.submit(spec(JobKind::Proxy, "failing", false, 0), failing.boxed()).unwrap();
    let ok = w.submit(spec(JobKind::Cache, "ok", false, 0), Fake::new("B", 3, &pb).boxed()).unwrap();
    assert!(w.wait_idle(WAIT));
    assert_eq!(pb.steps.load(Ordering::SeqCst), 3, "the next job still ran");
    let fin = w.take_finished();
    let outcome = |id| fin.iter().find(|f| f.id == id).map(|f| f.outcome.clone());
    assert!(matches!(outcome(x), Some(JobOutcome::Failed(_))));
    assert_eq!(outcome(f), Some(JobOutcome::Failed("F failed".into())));
    assert_eq!(outcome(ok), Some(JobOutcome::Done));
    assert!(pa.cancelled.load(Ordering::SeqCst), "the panicked job's partial output is cleaned up");
    assert_eq!(w.status().failed, 2);
    assert!(!filmcraft_media::cancel::background(), "the test thread is not marked");
    // and it keeps taking work
    let pd = Arc::new(Probe::default());
    w.submit(spec(JobKind::Proxy, "after", false, 0), Fake::new("D", 1, &pd).boxed()).unwrap();
    assert!(w.wait_idle(WAIT));
    assert_eq!(pd.steps.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_the_last_handle_cancels_queued_jobs() {
    let p = Arc::new(Probe::default());
    {
        let w = BackgroundWorker::new(cfg());
        w.set_activity(Activity { export_running: true, ..Default::default() });
        w.submit(spec(JobKind::Cache, "c", false, 0), Fake::new("C", 3, &p).boxed()).unwrap();
        let clone = w.clone();
        drop(w);
        assert!(!clone.is_idle(), "a clone keeps the worker");
    }
    assert!(until(|| p.cancelled.load(Ordering::SeqCst)), "the stopped worker cleaned up");
    assert_eq!(p.steps.load(Ordering::SeqCst), 0);
}

/// A step that runs until its job is cancelled (bounded, so a regression fails, not hangs).
struct UntilCancelled {
    started: Arc<AtomicBool>,
    saw_cancel: Arc<AtomicBool>,
}

impl BackgroundJob for UntilCancelled {
    fn step(&mut self) -> Result<JobStep, String> {
        self.started.store(true, Ordering::SeqCst);
        let end = Instant::now() + WAIT;
        while Instant::now() < end {
            if filmcraft_media::cancel::cancelled() {
                self.saw_cancel.store(true, Ordering::SeqCst);
                return Ok(JobStep::More);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(JobStep::Done)
    }
    fn fraction(&self) -> f32 {
        0.0
    }
    fn cancelled(&mut self) {}
}

#[test]
fn dropping_the_last_handle_cancels_the_running_step() {
    // Fork 2026-10-13: a closed app's render-cache batch kept rendering for seconds after its
    // session was gone (the UI test binary piled them up until it ran out of memory).
    let (started, saw_cancel) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let w = BackgroundWorker::new(cfg());
    w.submit(spec(JobKind::Cache, "c", false, 0), Box::new(UntilCancelled { started: started.clone(), saw_cancel: saw_cancel.clone() })).unwrap();
    assert!(until(|| started.load(Ordering::SeqCst)), "the step started");
    drop(w);
    assert!(until(|| saw_cancel.load(Ordering::SeqCst)), "the running step saw the cancellation");
}

#[test]
fn stop_and_wait_cancels_the_running_step_and_waits_for_the_thread() {
    assert!(BackgroundWorker::new(cfg()).stop_and_wait(Duration::ZERO), "a worker that never started has nothing to wait for");
    let (started, saw_cancel) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let p = Arc::new(Probe::default());
    let w = BackgroundWorker::new(cfg());
    w.submit(spec(JobKind::Cache, "c", false, 0), Box::new(UntilCancelled { started: started.clone(), saw_cancel: saw_cancel.clone() })).unwrap();
    w.submit(spec(JobKind::Cache, "q", false, 1), Fake::new("Q", 3, &p).boxed()).unwrap();
    assert!(until(|| started.load(Ordering::SeqCst)), "the step started");
    let t = Instant::now();
    assert!(w.stop_and_wait(WAIT), "the worker thread finished");
    assert!(t.elapsed() < WAIT / 2, "the running step stopped early ({:?})", t.elapsed());
    assert!(saw_cancel.load(Ordering::SeqCst), "the running step saw the cancellation");
    assert!(p.cancelled.load(Ordering::SeqCst), "the queued job was cleaned up");
    assert_eq!(p.steps.load(Ordering::SeqCst), 0);
    assert!(w.submit(spec(JobKind::Cache, "late", false, 0), Fake::new("L", 1, &p).boxed()).is_err(), "a stopped worker takes no more jobs");
}

#[test]
fn status_serializes_for_commands() {
    let w = BackgroundWorker::new(cfg());
    w.pause();
    let p = Arc::new(Probe::default());
    w.submit(spec(JobKind::Proxy, "clip.mov", true, 0), Fake::new("P", 1, &p).boxed()).unwrap();
    let v = serde_json::to_value(w.status()).unwrap();
    assert_eq!(v["state"], "paused");
    assert_eq!(v["jobs"][0]["kind"], "proxy");
    assert_eq!(v["jobs"][0]["chroma422"], true);
    assert_eq!(v["jobs"][0]["label"], "clip.mov");
    w.cancel_all();
    assert!(w.wait_idle(WAIT));
    let fin = serde_json::to_value(w.take_finished()).unwrap();
    assert_eq!(fin[0]["outcome"], "cancelled");
    // a default session owns an idle worker that has started no thread
    let s = crate::Session::default();
    assert!(s.background.is_idle());
    assert!(!crate::background::foreground_jobs_running(&s));
}

/// Owner (fork XPS sign-off): the render cache and proxies back off while memory is low — on the
/// 16 GB XPS 16 they ran next to 6.2K playback and an export until the machine swapped and the
/// Arc GPU ran out of memory. A job waits (state `LowMemory`) and runs once memory is free.
#[test]
fn waits_while_memory_is_low_and_runs_once_it_is_free() {
    static LOW: AtomicBool = AtomicBool::new(true);
    fn memory() -> Option<(u64, u64)> {
        let total = 16u64 << 30;
        Some((total, if LOW.load(Ordering::SeqCst) { 2 << 30 } else { 10 << 30 }))
    }
    let w = BackgroundWorker::new(WorkerConfig { memory: Some(memory), ..cfg() });
    let p = Arc::new(Probe::default());
    w.submit(spec(JobKind::Cache, "c", false, 0), Fake::new("C", 2, &p).boxed()).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(p.steps.load(Ordering::SeqCst), 0, "nothing runs while memory is low");
    assert!(until(|| w.status().state == WorkerState::LowMemory));
    LOW.store(false, Ordering::SeqCst);
    assert!(w.wait_idle(Duration::from_secs(10)), "runs once memory is free");
    assert_eq!(p.steps.load(Ordering::SeqCst), 2);
}

#[test]
fn memory_is_low_under_3_gb_or_a_quarter_of_the_machine() {
    use crate::background::memory_low;
    const GB: u64 = 1 << 30;
    assert!(memory_low(16 * GB, 3 * GB + GB / 2), "16 GB: under 4 GB free");
    assert!(!memory_low(16 * GB, 5 * GB));
    assert!(memory_low(8 * GB, 2 * GB + GB / 2), "8 GB: under 3 GB free");
    assert!(!memory_low(64 * GB, 17 * GB));
    assert!(memory_low(64 * GB, 15 * GB), "64 GB: under 16 GB free");
}
