//! `ai.status` / `ai.stop` (owner request 2026-10-24).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::ai_tools::TOOLS;

/// A backend that answers only once `open` is set (at most 20 s).
struct Held {
    open: Arc<AtomicBool>,
}

impl crate::ai_explain::ExplainBackend for Held {
    fn ready(&self, _: &filmcraft_llm::ModelInfo) -> Result<(), (crate::ai_explain::Fallback, String)> {
        Ok(())
    }
    fn generate(&self, _: &filmcraft_llm::ModelInfo, _: &str, _: &str, _: filmcraft_llm::GenOptions) -> Result<String, String> {
        let t = Instant::now();
        while !self.open.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok("Use one node per job.".into())
    }
}

#[test]
fn status_lists_every_tool() {
    let mut s = crate::Session::default();
    let st = s.execute("ai.status", json!({})).expect("status");
    let ids: Vec<&str> = st.as_array().expect("rows").iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids, TOOLS.iter().map(|(id, _)| *id).collect::<Vec<_>>());
    for r in st.as_array().expect("rows") {
        assert_eq!(r["running"], 0, "{r}");
    }
}

#[test]
fn stop_ends_running_and_waiting_assistant_questions() {
    let mut s = crate::Session::default();
    let open = Arc::new(AtomicBool::new(false));
    s.assist.backend = Some(Arc::new(Held { open: open.clone() }));
    // one answer runs, the next waits for it
    let ids: Vec<u64> =
        (0..2).map(|_| s.execute("coach.assist", json!({"question": "nodes?", "style": "free"})).expect("starts")["id"].as_u64().unwrap_or(0)).collect();
    let st = s.execute("ai.status", json!({})).expect("status");
    assert_eq!(st[0]["id"], "assistant");
    assert_eq!(st[0]["running"], 2, "{st}");

    let r = s.execute("ai.stop", json!({"tool": "assistant"})).expect("stop");
    assert_eq!(r["tools"][0]["stopped"], 2, "{r}");
    assert_eq!(r["status"][0]["running"], 0, "{r}");
    for id in &ids {
        let d = s.execute("coach.assistResult", json!({"id": id})).expect("result");
        assert_eq!(d["state"], "done", "{d}");
        assert!(d["fallback"].to_string().contains("stopped"), "{d}");
    }
    // the running answer finishing later doesn't replace "stopped"; the waiting one never runs
    open.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    for id in &ids {
        let d = s.execute("coach.assistResult", json!({"id": id})).expect("result");
        assert!(d["fallback"].to_string().contains("stopped"), "{d}");
    }
    // and a new question works as before
    let id = s.execute("coach.assist", json!({"question": "nodes?", "style": "free"})).expect("starts")["id"].as_u64().unwrap_or(0);
    let d = s.execute("coach.assistResult", json!({"id": id, "wait": true})).expect("done");
    assert_eq!((d["state"].as_str(), d["source"].as_str()), (Some("done"), Some("model")), "{d}");
}

#[test]
fn stop_all_and_hostile_params() {
    let mut s = crate::Session::default();
    let r = s.execute("ai.stop", json!({})).expect("stop all");
    assert_eq!(r["tools"].as_array().map(Vec::len), Some(TOOLS.len()), "{r}");
    // twice is fine (nothing loaded, nothing running)
    s.execute("ai.stop", json!({})).expect("again");
    for (id, _) in TOOLS {
        s.execute("ai.stop", json!({"tool": id})).expect("one tool");
    }
    for bad in [json!({"tool": "nope"}), json!({"tool": 3}), json!({"tool": "x".repeat(10_000)})] {
        assert!(s.execute("ai.stop", bad.clone()).is_err(), "{bad}");
    }
    // a stopped voice job: a job labelled as Clean Up Voice is cancelled
    let job = crate::narration::new_job(&mut s, format!("{} (1 clip)", crate::voice_cleanup::LABEL));
    let progress = job.progress.clone();
    s.jobs.push(job);
    assert_eq!(s.execute("ai.status", json!({})).expect("status")[3]["running"], 1);
    let r = s.execute("ai.stop", json!({"tool": "voice"})).expect("stop voice");
    assert_eq!(r["tools"][0]["stopped"], 1, "{r}");
    assert!(progress.cancel.load(Ordering::Relaxed));
}
