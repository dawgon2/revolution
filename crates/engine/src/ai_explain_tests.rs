//! Local AI explanations (fork GRADE-PLAN G2.11): routing through an injected backend (checked
//! text shown as `ai`, failing text / panic / timeout / error → template with a reason), the
//! asynchronous `aiKey` flow, preference leniency, the `ai.*` commands in a build without the
//! model, and (with `FILMCRAFT_LLM_DIR`) a real model.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::Session;
use crate::ai_explain::{AiPrefs, ExplainBackend, Fallback};
use filmcraft_llm::{GenOptions, ModelInfo};
use filmcraft_project::ClipId;

/// A backend that answers with `f(user prompt)`.
struct Fake<F: Fn(&str) -> Result<String, String> + Send + Sync>(F);

impl<F: Fn(&str) -> Result<String, String> + Send + Sync> ExplainBackend for Fake<F> {
    fn ready(&self, _: &ModelInfo) -> Result<(), (Fallback, String)> {
        Ok(())
    }
    fn generate(&self, _: &ModelInfo, _: &str, user: &str, _: GenOptions) -> Result<String, String> {
        (self.0)(user)
    }
}

/// The template body the prompt carries ("Reference sentence:\n…\n\n").
fn reference(user: &str) -> String {
    user.split("Reference sentence:\n").nth(1).and_then(|r| r.split("\n\n").next()).unwrap_or_default().to_string()
}

/// The demo project with one node lifting the gain on the first clip.
fn graded() -> (Session, ClipId, u64) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let c = s.active_sequence().unwrap().video_tracks[0].item_at(s.playhead()).unwrap().id;
    s.state.selection = vec![c];
    let n = s.execute("grade.addNode", json!({"clip": c.0})).unwrap()["node"].as_u64().unwrap();
    s.execute("grade.setParam", json!({"clip": c.0, "node": n, "param": "gain", "value": [0.0, 0.0, 0.0, 0.3]})).unwrap();
    (s, c, n)
}

fn with_backend(s: &mut Session, b: impl ExplainBackend + 'static) {
    s.ai.set_backend(Arc::new(b));
    s.prefs.color.ai = AiPrefs { enabled: true, model: filmcraft_llm::DEFAULT_MODEL.into() };
}

fn explain(s: &mut Session, c: ClipId, n: u64, wait: bool) -> Value {
    s.execute("grade.explainNode", json!({"clip": c.0, "node": n, "wait": wait})).unwrap()
}

#[test]
fn off_by_default_shows_the_template() {
    let (mut s, c, n) = graded();
    assert!(!s.prefs.color.ai.enabled, "opt-in");
    let r = explain(&mut s, c, n, false);
    assert_eq!(r["source"], "template", "{r}");
    assert_eq!(r["reasonCode"], "off", "{r}");
    assert_eq!(r["text"], r["sentence"], "the existing fields stay");
    assert!(r["summary"].is_string() && r["facts"].is_object() && r.get("aiKey").is_none(), "{r}");
}

#[test]
fn checked_text_is_shown_as_ai_and_cached() {
    let (mut s, c, n) = graded();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let k = calls.clone();
    with_backend(
        &mut s,
        Fake(move |u: &str| {
            k.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(format!("  {}  ", reference(u)))
        }),
    );
    // the UI path: the template at once with a key, the checked text when polled
    let r = explain(&mut s, c, n, false);
    assert_eq!(r["source"], "template", "{r}");
    assert_eq!(r["pending"], true, "{r}");
    let key = r["aiKey"].as_str().unwrap().to_string();
    let a = s.execute("ai.explanation", json!({"key": key, "wait": true})).unwrap();
    assert_eq!(a["state"], "done", "{a}");
    assert_eq!(a["source"], "ai", "the template body passes its own fact check: {a}");
    assert!(a["reason"].is_null(), "{a}");
    assert!(a["text"].as_str().unwrap().starts_with("Node 1: "), "shown with the node's label: {a}");
    // asked again: answered from the cache, no second generation
    let r = explain(&mut s, c, n, false);
    assert_eq!(r["source"], "ai", "{r}");
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(s.execute("ai.explanation", json!({"key": "nope"})).unwrap()["state"], "unknown");
}

#[test]
fn wrong_text_falls_back_with_the_reason() {
    let (mut s, c, n) = graded();
    with_backend(&mut s, Fake(|_: &str| Ok("It makes the picture much darker.".into())));
    let r = explain(&mut s, c, n, true);
    assert_eq!(r["source"], "template", "{r}");
    assert_eq!(r["reasonCode"], "rejected", "{r}");
    assert_eq!(r["text"], r["sentence"]);
    assert!(r["reason"].as_str().unwrap().starts_with("the AI text"), "{r}");
    // hostile output: huge, markdown, other scripts, control characters
    for bad in ["x".repeat(1 << 20), "**Node 1**: brighter\n- more".into(), "画面更亮。".into(), "\u{0}\u{202e}".into(), String::new()] {
        let (mut s, c, n) = graded();
        with_backend(&mut s, Fake(move |_: &str| Ok(bad.clone())));
        let r = explain(&mut s, c, n, true);
        assert_eq!(r["source"], "template", "{r}");
    }
}

#[test]
fn panic_error_and_timeout_fall_back() {
    let (mut s, c, n) = graded();
    with_backend(&mut s, Fake(|_: &str| -> Result<String, String> { panic!("model exploded") }));
    let r = explain(&mut s, c, n, true);
    assert_eq!((r["source"].as_str(), r["reasonCode"].as_str()), (Some("template"), Some("error")), "{r}");

    let (mut s, c, n) = graded();
    with_backend(&mut s, Fake(|_: &str| Err("out of tokens".into())));
    let r = explain(&mut s, c, n, true);
    assert_eq!(r["reasonCode"], "error", "{r}");
    assert!(!r["reason"].as_str().unwrap().contains("out of tokens"), "detail goes to the log: {r}");

    let (mut s, c, n) = graded();
    with_backend(
        &mut s,
        Fake(|u: &str| {
            std::thread::sleep(Duration::from_millis(1500));
            Ok(reference(u))
        }),
    );
    s.ai.timeout = Duration::from_millis(150);
    let t0 = std::time::Instant::now();
    let r = explain(&mut s, c, n, true);
    assert_eq!((r["source"].as_str(), r["reasonCode"].as_str()), (Some("template"), Some("timeout")), "{r}");
    assert!(t0.elapsed() < Duration::from_millis(1400), "the caller doesn't wait for a hung model");
    // a late valid answer replaces the timeout
    std::thread::sleep(Duration::from_millis(1600));
    assert_eq!(explain(&mut s, c, n, false)["source"], "ai");
}

#[test]
fn unknown_or_missing_model_falls_back() {
    let (mut s, c, n) = graded();
    with_backend(&mut s, Fake(|u: &str| Ok(reference(u))));
    s.prefs.color.ai.model = "no-such-model".into();
    assert_eq!(explain(&mut s, c, n, true)["reasonCode"], "noModel");
    // not chosen: the automatic model (an installed tune, else the default)
    s.prefs.color.ai.model = String::new();
    assert_eq!(explain(&mut s, c, n, true)["source"], "ai");
    // the real backend over an empty models folder (never the user's real models)
    let empty = std::env::temp_dir().join(format!("fc-ai-empty-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let (mut s, c, n) = graded();
    s.ai.set_backend(Arc::new(crate::ai_explain::LlmBackend::with_dir(empty.clone())));
    s.prefs.color.ai.enabled = true;
    let r = explain(&mut s, c, n, true);
    assert_eq!(r["source"], "template", "{r}");
    let code = r["reasonCode"].as_str().unwrap();
    assert_eq!(code, if filmcraft_llm::AVAILABLE { "notInstalled" } else { "unavailable" }, "{r}");
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn rejection_reasons_never_echo_model_text() {
    for bad in ["zzqx \u{202e}evil", "Node 1 'zzqx': brighter.", "**zzqx**", "It makes the picture much darker zzqx."] {
        let (mut s, c, n) = graded();
        let text = bad.to_string();
        with_backend(&mut s, Fake(move |_: &str| Ok(text.clone())));
        let r = explain(&mut s, c, n, true);
        assert_eq!(r["reasonCode"], "rejected", "{r}");
        let reason = r["reason"].as_str().unwrap();
        assert!(!reason.contains("zzqx") && !reason.contains('\u{202e}') && !reason.contains("evil"), "{reason}");
        assert!(!r.to_string().contains("zzqx"), "no unchecked text anywhere in the answer: {r}");
    }
    // backend errors are not shown either (they go to the log)
    let (mut s, c, n) = graded();
    with_backend(&mut s, Fake(|_: &str| Err("zzqx \u{202e}evil".into())));
    let r = explain(&mut s, c, n, true);
    assert_eq!(r["reasonCode"], "error");
    assert!(!r.to_string().contains("zzqx"), "{r}");
}

#[test]
fn timeouts_and_errors_are_asked_again() {
    // an error, then a good answer: the second request runs the model again
    let (mut s, c, n) = graded();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let k = calls.clone();
    with_backend(&mut s, Fake(move |u: &str| if k.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 { Err("first".into()) } else { Ok(reference(u)) }));
    assert_eq!(explain(&mut s, c, n, true)["reasonCode"], "error");
    assert_eq!(explain(&mut s, c, n, true)["source"], "ai");
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    // a rejection is cached
    let (mut s, c, n) = graded();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let k = calls.clone();
    with_backend(
        &mut s,
        Fake(move |_: &str| {
            k.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok("It makes the picture much darker.".into())
        }),
    );
    assert_eq!(explain(&mut s, c, n, true)["reasonCode"], "rejected");
    assert_eq!(explain(&mut s, c, n, true)["reasonCode"], "rejected");
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn prefs_are_lenient_per_field() {
    let p: crate::settings::ColorPrefs = serde_json::from_value(json!({"ai": {"enabled": "yes", "model": 7}})).unwrap();
    assert_eq!(p.ai, AiPrefs::default());
    assert!(!p.ai.enabled);
    let p: crate::settings::ColorPrefs = serde_json::from_value(json!({"ai": {"enabled": true, "model": "x".repeat(10_000)}})).unwrap();
    assert!(p.ai.enabled);
    assert_eq!(p.ai.model.len(), 128);
    let p: crate::settings::ColorPrefs = serde_json::from_value(json!({"ai": {"model": null}})).unwrap();
    assert_eq!(p.ai.model, "");
    let p: crate::settings::ColorPrefs = serde_json::from_value(json!({})).unwrap();
    assert_eq!(p.ai.model, "", "not chosen: resolved per request");
    // the whole preferences file (deserialized directly: no process-wide switches touched)
    let p: crate::autosave::Preferences = serde_json::from_value(json!({"color": {"ai": {"enabled": "garbage"}, "hdrGraphicsWhite": "300"}})).unwrap();
    assert!(!p.color.ai.enabled, "garbage turns it off, never on");
    assert_eq!(p.color.hdr_graphics_white, "300", "the other fields survive");
    let p: crate::autosave::Preferences = serde_json::from_value(json!({"color": {"ai": {"enabled": true}}})).unwrap();
    assert!(p.color.ai.enabled);
}

#[test]
fn ai_commands_list_select_and_switch() {
    let mut s = Session::default();
    let m = s.execute("ai.models", json!({})).unwrap();
    let list = m["models"].as_array().unwrap();
    assert_eq!(list.len(), filmcraft_llm::models::catalogue().len());
    for e in list {
        assert!(e["size"].as_u64().unwrap() > 0 && e["license"].is_string() && e["source"].is_string() && e["installed"].is_boolean(), "{e}");
    }
    assert_eq!(m["inference"], json!(filmcraft_llm::AVAILABLE), "{m}");
    assert_eq!(m["slowBuild"], json!(cfg!(target_arch = "x86_64") && !cfg!(target_feature = "avx2")));
    assert_eq!(m["enabled"], false);
    assert!(list.iter().any(|e| e["selected"] == true));
    assert_eq!(m["automatic"], true, "nothing chosen yet");
    for e in list {
        let id = e["id"].as_str().unwrap();
        let local = filmcraft_llm::models::find(id).unwrap().is_local();
        assert_eq!(e["local"], json!(local), "{e}");
        assert_eq!(e["tuned"], json!(filmcraft_llm::models::find(id).is_some_and(filmcraft_llm::ModelInfo::is_tuned)), "{e}");
    }
    assert!(list.iter().any(|e| e["local"] == true), "the tuned explainers are local for now");

    let other = filmcraft_llm::models::catalogue().last().unwrap().id;
    s.execute("ai.select", json!({"model": other})).unwrap();
    assert_eq!(s.prefs.color.ai.model, other);
    let m = s.execute("ai.models", json!({})).unwrap();
    assert_eq!((m["selected"].as_str(), &m["automatic"]), (Some(other), &json!(false)), "a local entry is selected normally");
    assert!(s.execute("ai.select", json!({"model": "../../etc"})).is_err());
    assert!(s.execute("ai.select", json!({})).is_err());
    s.execute("ai.setEnabled", json!({"on": true})).unwrap();
    assert!(s.prefs.color.ai.enabled);
    assert!(s.execute("ai.setEnabled", json!({"on": "x"})).is_err());
    assert!(s.execute("ai.remove", json!({"model": "nope"})).is_err());
    assert_eq!(s.execute("ai.cancelDownload", json!({})).unwrap()["cancelled"], false);
}

#[test]
fn downloads_need_the_feature() {
    let mut s = Session::default();
    let id = filmcraft_llm::DEFAULT_MODEL;
    let m = s.execute("ai.models", json!({})).unwrap();
    assert_eq!(m["canDownload"], json!(cfg!(feature = "llm-download")));
    if !cfg!(feature = "llm-download") {
        let e = s.execute("ai.download", json!({"model": id})).unwrap_err().to_string();
        assert!(e.contains("not available"), "{e}");
        assert!(!s.is_enabled("ai.download"));
    }
    // a model built on this computer is never downloaded
    let local = filmcraft_llm::models::catalogue().iter().find(|m| m.is_local()).unwrap();
    assert!(s.execute("ai.download", json!({"model": local.id})).is_err());
}

/// A backend that records the options it is given.
struct Recording(Arc<std::sync::Mutex<Vec<(String, GenOptions)>>>);

impl ExplainBackend for Recording {
    fn ready(&self, _: &ModelInfo) -> Result<(), (Fallback, String)> {
        Ok(())
    }
    fn generate(&self, m: &ModelInfo, _: &str, user: &str, opts: GenOptions) -> Result<String, String> {
        self.0.lock().unwrap().push((m.id.to_string(), opts));
        Ok(reference(user))
    }
}

/// Each model runs with its own recommended settings (the tuned explainers without a repeat
/// penalty) within the app's token cap and deadline.
#[test]
fn generation_uses_the_models_recommended_options() {
    let (mut s, c, n) = graded();
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    with_backend(&mut s, Recording(log.clone()));
    let tuned = filmcraft_llm::models::catalogue().iter().find(|m| m.is_tuned()).unwrap();
    for id in [tuned.id, filmcraft_llm::DEFAULT_MODEL] {
        s.prefs.color.ai.model = id.into();
        assert_eq!(explain(&mut s, c, n, true)["source"], "ai");
    }
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2);
    let (tid, t) = &log[0];
    assert_eq!(tid, tuned.id);
    assert_eq!(t.repeat_penalty, 1.0, "tuned: no repeat penalty");
    assert_eq!((t.max_tokens, t.deadline), (crate::ai_explain::MAX_TOKENS, crate::ai_explain::GEN_DEADLINE));
    let (_, d) = &log[1];
    assert_eq!(d.repeat_penalty, GenOptions::default().repeat_penalty, "stock models keep the default");
    assert_eq!(*t, crate::ai_explain::gen_options(tuned));
}

/// A backend that counts how often it is asked to free its model.
struct Releasing(Arc<std::sync::atomic::AtomicUsize>);

impl ExplainBackend for Releasing {
    fn ready(&self, _: &ModelInfo) -> Result<(), (Fallback, String)> {
        Ok(())
    }
    fn generate(&self, _: &ModelInfo, _: &str, _: &str, _: GenOptions) -> Result<String, String> {
        Err("unused".into())
    }
    fn release(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Switching AI explanations off frees the model on every path: `ai.setEnabled` and the
/// Settings dialog's OK (`prefs.set`); turning them on, or an unrelated change, doesn't.
#[test]
fn switching_off_through_prefs_set_frees_the_model() {
    let mut s = Session::default();
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    s.ai.set_backend(Arc::new(Releasing(n.clone())));
    let count = || n.load(std::sync::atomic::Ordering::Relaxed);
    s.execute("prefs.set", json!({"values": {"color.ai.enabled": true}})).unwrap();
    assert_eq!(count(), 0, "turning on frees nothing");
    s.execute("prefs.set", json!({"key": "autoSave.intervalMinutes", "value": 9})).unwrap();
    assert_eq!(count(), 0, "an unrelated change frees nothing");
    s.execute("prefs.set", json!({"values": {"color.ai.enabled": false}})).unwrap();
    assert_eq!(count(), 1, "the Settings dialog's OK path frees the model");
    s.execute("ai.setEnabled", json!({"on": true})).unwrap();
    s.execute("ai.setEnabled", json!({"on": false})).unwrap();
    assert_eq!(count(), 2, "and ai.setEnabled still does");
    assert!(!s.prefs.color.ai.enabled);
}

/// A real model: `FILMCRAFT_LLM_DIR=<folder holding <model id>/<files>>`, built with `--features
/// llm`. Skipped otherwise; never downloads.
#[test]
fn real_model_answers_or_falls_back_cleanly() {
    let Some(dir) = std::env::var_os("FILMCRAFT_LLM_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: FILMCRAFT_LLM_DIR is not set");
        return;
    };
    if !filmcraft_llm::AVAILABLE {
        eprintln!("skipped: built without the `llm` feature");
        return;
    }
    let Some(model) = filmcraft_llm::models::catalogue().iter().find(|m| filmcraft_llm::models::installed(&dir, m)) else {
        eprintln!("skipped: no catalogue model in {}", dir.display());
        return;
    };
    let (mut s, c, n) = graded();
    s.ai.set_backend(Arc::new(crate::ai_explain::LlmBackend::with_dir(dir)));
    s.prefs.color.ai = AiPrefs { enabled: true, model: model.id.into() };
    s.ai.timeout = Duration::from_secs(180);
    let r = explain(&mut s, c, n, true);
    eprintln!("{}: {r}", model.id);
    match r["source"].as_str() {
        Some("ai") => assert!(r["reason"].is_null()),
        _ => assert!(["rejected", "timeout"].contains(&r["reasonCode"].as_str().unwrap_or("")), "a working model answers: {r}"),
    }
}

/// A backend that streams the template body, then a wrong sentence once `go` is set.
struct Streaming(Arc<std::sync::atomic::AtomicBool>);

impl ExplainBackend for Streaming {
    fn ready(&self, _: &ModelInfo) -> Result<(), (Fallback, String)> {
        Ok(())
    }
    fn generate(&self, _: &ModelInfo, _: &str, _: &str, _: GenOptions) -> Result<String, String> {
        Err("streaming only".into())
    }
    fn generate_streaming(&self, _: &ModelInfo, _: &str, user: &str, _: GenOptions, on_text: &mut dyn FnMut(&str) -> bool) -> Result<String, String> {
        let first = reference(user);
        on_text(&first);
        on_text(&format!("{first} It makes"));
        let t0 = std::time::Instant::now();
        while !self.0.load(std::sync::atomic::Ordering::Relaxed) && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(format!("{first} It makes the picture much darker."))
    }
}

/// While the model writes, its checked complete sentences are `partial`; the final answer is
/// still checked whole (here a wrong second sentence → the template).
#[test]
fn checked_sentences_stream_before_the_answer() {
    let (mut s, c, n) = graded();
    let go = Arc::new(std::sync::atomic::AtomicBool::new(false));
    with_backend(&mut s, Streaming(go.clone()));
    let r = explain(&mut s, c, n, false);
    let key = r["aiKey"].as_str().unwrap().to_string();
    let t0 = std::time::Instant::now();
    let partial = loop {
        let a = s.execute("ai.explanation", json!({"key": key})).unwrap();
        assert_eq!(a["state"], "pending", "{a}");
        if let Some(p) = a["partial"].as_str() {
            break p.to_string();
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "no partial text: {a}");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(partial.starts_with("Node 1: "), "{partial}");
    assert!(!partial.contains("It makes"), "an unfinished sentence is never shown: {partial}");
    go.store(true, std::sync::atomic::Ordering::Relaxed);
    let a = s.execute("ai.explanation", json!({"key": key, "wait": true})).unwrap();
    assert_eq!(a["state"], "done", "{a}");
    assert_eq!(a["reasonCode"], "rejected", "{a}");
}

/// Background pre-generation (AI1.3): a newer prefetch drops an older one nobody asked for; an
/// explicit request for the same facts adopts the pending prefetch and gets its answer.
#[test]
fn a_newer_prefetch_supersedes_and_a_request_adopts() {
    let (mut s, c, n) = graded();
    let go = Arc::new(std::sync::atomic::AtomicBool::new(false));
    with_backend(&mut s, Streaming(go.clone()));
    let prefetch = |s: &mut Session| s.execute("grade.explainNode", json!({"clip": c.0, "node": n, "prefetch": true})).unwrap();
    let rev = s.revision;
    let first = prefetch(&mut s)["aiKey"].as_str().unwrap().to_string();
    assert_eq!(s.revision, rev, "a prefetch is not an edit (the UI keys its debounce on the revision)");
    s.execute("grade.setParam", json!({"clip": c.0, "node": n, "param": "gain", "value": [0.0, 0.0, 0.0, 0.5]})).unwrap();
    let second = prefetch(&mut s)["aiKey"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    assert_eq!(s.execute("ai.explanation", json!({"key": first})).unwrap()["state"], "unknown", "superseded");
    // the user clicks Explain node: same facts, so the same pending answer (adopted)
    let r = explain(&mut s, c, n, false);
    assert_eq!(r["aiKey"].as_str(), Some(second.as_str()), "{r}");
    // a prefetch elsewhere no longer drops it
    s.execute("grade.setParam", json!({"clip": c.0, "node": n, "param": "gain", "value": [0.0, 0.0, 0.0, 0.7]})).unwrap();
    prefetch(&mut s);
    assert_eq!(s.execute("ai.explanation", json!({"key": second})).unwrap()["state"], "pending");
    go.store(true, std::sync::atomic::Ordering::Relaxed);
    let a = s.execute("ai.explanation", json!({"key": second, "wait": true})).unwrap();
    assert_eq!(a["state"], "done", "{a}");
}

/// AI4 acceptance: a broken picture-model folder (missing, garbage or truncated files) makes the
/// look fail with an error, so the assistant answers without it; it never panics.
#[cfg(feature = "llm")]
#[test]
fn a_corrupt_picture_model_fails_the_look_without_a_crash() {
    use crate::coach_assist::PictureInput;
    const CONFIG: &str = r#"{"architectures":["Qwen3VLForConditionalGeneration"],"tie_word_embeddings":true,
        "text_config":{"hidden_size":64,"intermediate_size":128,"num_hidden_layers":1,"num_attention_heads":2,"num_key_value_heads":1,
        "head_dim":32,"rms_norm_eps":1e-6,"rope_theta":5000000,"vocab_size":32,"rope_scaling":{"mrope_interleaved":true,"mrope_section":[6,5,5]}},
        "vision_config":{"deepstack_visual_indexes":[0],"depth":1,"hidden_act":"gelu_pytorch_tanh","hidden_size":32,"in_channels":3,
        "intermediate_size":64,"num_heads":2,"num_position_embeddings":16,"out_hidden_size":64,"patch_size":16,"spatial_merge_size":2,"temporal_patch_size":2}}"#;
    let shard = |header_len: u64, rest: &[u8]| {
        let mut v = header_len.to_le_bytes().to_vec();
        v.extend_from_slice(rest);
        v
    };
    let index = r#"{"weight_map":{"model.visual.patch_embed.proj.weight":"model-00001-of-00001.safetensors","model.language_model.embed_tokens.weight":"model-00001-of-00001.safetensors"}}"#;
    let cases: Vec<(&str, Vec<(&str, Vec<u8>)>)> = vec![
        ("empty", vec![]),
        ("garbage config", vec![("config.json", b"\x00\xff not json".to_vec())]),
        ("garbage index", vec![("config.json", CONFIG.into()), ("model.safetensors.index.json", b"{\"weight_map\": 7".to_vec())]),
        (
            "index escapes the folder",
            vec![("config.json", CONFIG.into()), ("model.safetensors.index.json", index.replace("model-00001-of-00001", "../../etc/passwd").into_bytes())],
        ),
        (
            "header longer than the file",
            vec![("config.json", CONFIG.into()), ("model.safetensors.index.json", index.into()), ("model-00001-of-00001.safetensors", shard(1 << 40, b"{}"))],
        ),
        (
            "garbage header",
            vec![("config.json", CONFIG.into()), ("model.safetensors.index.json", index.into()), ("model-00001-of-00001.safetensors", shard(9, b"not json!"))],
        ),
        (
            "garbage text model",
            vec![
                ("config.json", CONFIG.into()),
                ("model.safetensors.index.json", index.into()),
                ("model-00001-of-00001.safetensors", shard(2, b"{}")),
                ("text-own-q8_0.gguf", b"GGUF\x03\x00\x00\x00 truncated".to_vec()),
            ],
        ),
    ];
    let picture = PictureInput { rgb: vec![0.5; 32 * 32 * 3], w: 32, h: 32 };
    for (name, files) in cases {
        let dir = std::env::temp_dir().join(format!("filmcraft-vl-corrupt-{}-{}", std::process::id(), name.replace(' ', "-")));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        for (f, bytes) in files {
            std::fs::write(dir.join(f), bytes).expect("write");
        }
        let looker = crate::ai_explain::looker_at(dir.clone()).expect("a looker in llm builds");
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| looker.look(&picture, "is it too dark?")));
        assert!(matches!(r, Ok(Err(_))), "{name}: {r:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Fork AI2: a mixer's explanation is its branch note; the model's text is shown only when it is
/// one of the note's fixed phrasings, and a correction node's own text is unchanged.
#[test]
fn a_mixer_is_explained_by_its_branch_note_under_the_fixed_check() {
    let (mut s, c, n) = graded();
    let r = s.execute("grade.addParallel", json!({"clip": c.0, "node": n})).unwrap();
    let mixer = r["nodes"][1].as_u64().unwrap();
    // off: the note's template
    let t = explain(&mut s, c, mixer, false);
    let note = t["branch"]["sentence"].as_str().unwrap().to_string();
    assert!(note.contains("blended 50 / 50"), "{t}");
    assert_eq!(t["sentence"], json!(note), "{t}");
    assert_eq!(t["text"], json!(note), "{t}");
    assert_eq!(t["branch"]["facts"]["kind"], "parallel", "{t}");
    // on: a true rephrasing passes, shown as written
    let accept =
        filmcraft_render::grade_explain::branch_facts(s.active_sequence().unwrap().find_item(c).unwrap().1.grade.as_ref().unwrap(), filmcraft_time::Tick(0))[0]
            .phrasings();
    let other = accept[1].clone();
    let o = other.clone();
    with_backend(&mut s, Fake(move |_: &str| Ok(o.clone())));
    let a = explain(&mut s, c, mixer, true);
    assert_eq!((a["source"].clone(), a["text"].clone()), (json!("ai"), json!(other)), "{a}");
    // a wrong share falls back to the template with the reason
    let mut s2 = s;
    // (a fresh cache: the passing answer above is cached per facts and model)
    s2.ai = Default::default();
    let wrong = note.replace("50 / 50", "70 / 30");
    with_backend(&mut s2, Fake(move |_: &str| Ok(wrong.clone())));
    let b = explain(&mut s2, c, mixer, true);
    assert_eq!((b["source"].clone(), b["text"].clone(), b["reasonCode"].clone()), (json!("template"), json!(note), json!("rejected")), "{b}");
    // the correction node keeps its measured explanation (no branch note: nothing limits it)
    let own = explain(&mut s2, c, n, false);
    assert!(own.get("branch").is_none() && own["facts"]["ranges"].is_array(), "{own}");
}
