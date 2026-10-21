//! Local AI explanations for the node explainer (fork GRADE-PLAN G2.11): `ai.*` commands and the
//! routing behind `grade.explainNode`.
//!
//! An optional, offline small language model ([`filmcraft_llm`]) rewrites a node's measured facts
//! (G2.10) in plain words. It is **opt-in** (Settings ▸ Color ▸ AI explanations, `color.ai.enabled`
//! off by default) and the weights are **never bundled**: the user picks a catalogue model and
//! starts its download (`ai.download`), the only network use.
//!
//! Every answer is guarded:
//! - generation runs on a worker thread under `catch_unwind`, one at a time, with a deadline and a
//!   token cap (and an overall [`AiExplain::timeout`] that also covers loading); the UI thread
//!   never waits: `grade.explainNode` returns the template sentence at once with a `aiKey`, and
//!   `ai.explanation` (or [`AiExplain::poll`]) reports the checked text when it is ready;
//! - the text must pass [`filmcraft_render::grade_explain::check_explanation_labeled`] (every
//!   number, direction and range agrees with the facts); otherwise, and on any error, panic,
//!   timeout, missing model or low memory, the template sentence is shown with the reason;
//! - model files are hostile: the llm crate bounds-checks them on load, and their SHA-256 is
//!   verified once (after the download or on first use) and remembered for the process.
//!
//! Results are cached per (node facts, model). Tests inject text with [`AiExplain::set_backend`].

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_llm::models as catalogue;
/// (re-exported for [`ExplainBackend`] implementations outside the engine)
pub use filmcraft_llm::{GenOptions, ModelInfo};
use filmcraft_render::grade_explain::{FactMismatch, NodeFacts, check_explanation_labeled};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{EngineError, Result, Session};

/// Wall-clock budget of one generation (prompt + answer).
pub const GEN_DEADLINE: Duration = Duration::from_secs(15);
/// Most new tokens of one explanation (two short sentences).
pub const MAX_TOKENS: usize = 96;
/// Default overall cap of one request: verifying and loading the model on first use plus
/// [`GEN_DEADLINE`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Worker threads alive at once (running or queued for the model); more requests fall back to
/// the template ("busy").
const MAX_PENDING: usize = 4;
/// Answers kept; when full, an arbitrary half of the finished ones is dropped (cheap, and a
/// dropped answer is only asked again).
const MAX_CACHE: usize = 256;
/// The longest a `"wait": true` call blocks; still pending after this, it returns the template
/// with `aiKey` to poll.
pub const MAX_WAIT: Duration = Duration::from_secs(20);
/// Head room a loaded model needs beyond its file size (context, tokenizer, buffers).
const LOAD_HEADROOM: u64 = 512 << 20;

// ---------- preferences ----------

/// Settings ▸ Color ▸ AI explanations, kept in `preferences.json` (`color.ai.*`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiPrefs {
    /// Opt-in: off until the user turns it on.
    #[serde(deserialize_with = "lenient_enabled")]
    pub enabled: bool,
    /// The catalogue model to use (`ai.models`); empty = not chosen: the best fine-tuned
    /// explainer installed, else the default ([`resolve_model`]).
    #[serde(deserialize_with = "lenient_model")]
    pub model: String,
}

/// A malformed `enabled` (hand-edited file) turns the feature off instead of failing the file.
fn lenient_enabled<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(Value::deserialize(d)?.as_bool().unwrap_or(false))
}

/// A malformed `model` goes back to "not chosen"; a long one is cut (ids are short).
fn lenient_model<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s.chars().take(128).collect(),
        _ => String::new(),
    })
}

/// The model preference `pref` names: the catalogue entry, or when empty (the user hasn't
/// chosen) [`catalogue::automatic`] over `models_dir`. None for an unknown id.
pub fn resolve_model(pref: &str, models_dir: Option<&Path>) -> Option<&'static ModelInfo> {
    if pref.is_empty() { catalogue::automatic(models_dir) } else { catalogue::find(pref) }
}

/// The generation settings of one explanation: the model's own ([`ModelInfo::recommended_options`],
/// e.g. no repeat penalty for the fine-tuned explainers) within the app's token cap and deadline.
pub fn gen_options(m: &ModelInfo) -> GenOptions {
    GenOptions { max_tokens: MAX_TOKENS, deadline: GEN_DEADLINE, ..m.recommended_options() }
}

// ---------- outcomes ----------

/// Why the template sentence is shown instead of an AI text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Fallback {
    /// AI explanations are switched off (the default).
    Off,
    /// This build can't run a model (no `llm` feature, or wasm).
    Unavailable,
    /// No model selected, or the selected id isn't in the catalogue.
    NoModel,
    /// The selected model isn't downloaded (or is damaged).
    NotInstalled,
    /// Not enough free memory to load the model.
    Memory,
    /// Too many requests are waiting for the model.
    Busy,
    /// The model answered, but the text failed the fact check.
    Rejected,
    /// No answer within the time cap.
    Timeout,
    /// Loading or generating failed (including a caught panic).
    Error,
}

impl Fallback {
    /// Whether the model was asked and its answer couldn't be used (the UI explains those).
    pub fn attempted(self) -> bool {
        matches!(self, Fallback::Rejected | Fallback::Timeout | Fallback::Error)
    }
}

/// What to show for one node.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Outcome {
    pub text: String,
    /// `"ai"` (checked model text) or `"template"`.
    pub source: &'static str,
    /// Why the template is shown (None for `ai`).
    pub reason: Option<String>,
    pub code: Option<Fallback>,
}

impl Outcome {
    fn template(text: &str, code: Fallback, reason: impl Into<String>) -> Self {
        Outcome { text: text.to_string(), source: "template", reason: Some(reason.into()), code: Some(code) }
    }

    pub fn to_json(&self) -> Value {
        json!({"text": self.text, "source": self.source, "reason": self.reason, "reasonCode": self.code})
    }
}

/// The answer to a request: final, or pending with the template to show meanwhile and, while the
/// model writes, `partial`: its complete sentences so far that already passed the fact check
/// (shown with the node's label, like a final answer). The final answer may still be the template
/// (a later sentence rejected).
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Ready(Outcome),
    Pending { key: String, template: String, partial: Option<String> },
}

// ---------- the backend seam ----------

/// Something that writes an explanation: the local model, or a test's injected text.
pub trait ExplainBackend: Send + Sync {
    /// Whether `model` can run now (checked on the caller's thread, so keep it cheap).
    fn ready(&self, model: &ModelInfo) -> std::result::Result<(), (Fallback, String)>;
    /// Generate an answer to the prompt (on a worker thread; may be slow, may fail).
    fn generate(&self, model: &ModelInfo, system: &str, user: &str, opts: GenOptions) -> std::result::Result<String, String>;
    /// [`ExplainBackend::generate`], calling `on_text` with the answer so far as it grows;
    /// `on_text` returning false asks to stop early (the result is then not used). The default
    /// does not stream.
    fn generate_streaming(
        &self,
        model: &ModelInfo,
        system: &str,
        user: &str,
        opts: GenOptions,
        on_text: &mut dyn FnMut(&str) -> bool,
    ) -> std::result::Result<String, String> {
        let _ = on_text;
        self.generate(model, system, user, opts)
    }
    /// Free a loaded model's memory (AI explanations switched off, another model chosen). Must
    /// never wait: called on the UI thread; a running generation may keep its model.
    fn release(&self) {}
    /// Where this backend's models live (for picking the automatic model).
    fn models_dir(&self) -> Option<PathBuf> {
        models_dir()
    }
}

/// The real backend: one cached [`filmcraft_llm::Model`] for the process, loaded lazily on the
/// first request and reloaded when another model is selected.
pub struct LlmBackend {
    dir: Option<PathBuf>,
    loaded: Mutex<Option<filmcraft_llm::Model>>,
    /// A release asked for while a generation held the model: it drops the model when done.
    release_pending: AtomicBool,
    /// The last generation's numbers (its text cleared), for timing reports.
    last: Mutex<Option<filmcraft_llm::Generation>>,
}

impl LlmBackend {
    fn new(dir: Option<PathBuf>) -> Self {
        LlmBackend { dir, loaded: Mutex::new(None), release_pending: AtomicBool::new(false), last: Mutex::new(None) }
    }

    /// A backend reading models from `dir` (`<dir>/<model id>/`) instead of the data folder.
    pub fn with_dir(dir: PathBuf) -> Self {
        Self::new(Some(dir))
    }

    fn lock(&self) -> MutexGuard<'_, Option<filmcraft_llm::Model>> {
        match self.loaded.lock() {
            Ok(g) => g,
            Err(p) => {
                // (a panic mid-generation may have left the model half-updated: drop it)
                let mut g = p.into_inner();
                *g = None;
                g
            }
        }
    }

    /// The last generation's token counts and times (its text left out).
    pub fn last_generation(&self) -> Option<filmcraft_llm::Generation> {
        self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// The GPU adapter the loaded model runs on (`None`: the CPU, or nothing loaded). Never
    /// waits: `None` while a generation holds the model.
    pub fn gpu_adapter(&self) -> Option<String> {
        let g = self.loaded.try_lock().ok()?;
        g.as_ref().and_then(|m| m.gpu_adapter().map(str::to_string))
    }

    /// Drop the loaded model if it is `id` (or any with `None`). Never waits: false while a
    /// generation holds the model.
    pub fn unload(&self, id: Option<&str>) -> bool {
        let mut g = match self.loaded.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        if id.is_none_or(|id| g.as_ref().is_some_and(|m| m.info().id == id)) {
            *g = None;
        }
        true
    }

    /// A generation is done with the model: drop it when a release was asked for meanwhile.
    fn finish(&self, g: &mut Option<filmcraft_llm::Model>) {
        if self.release_pending.swap(false, Ordering::SeqCst) {
            *g = None;
        }
    }

    /// Whether `m` is the loaded model, without waiting (a running generation counts as loaded:
    /// the worker checks memory again before it loads another model).
    fn is_loaded(&self, m: &ModelInfo) -> bool {
        match self.loaded.try_lock() {
            Ok(g) => g.as_ref().is_some_and(|x| x.info().id == m.id),
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Poisoned(_)) => false,
        }
    }
}

/// Enough free memory to load `m` (Err with the plain reason otherwise; Ok where unknown).
fn memory_for(m: &ModelInfo) -> std::result::Result<(), String> {
    let Some(free) = available_memory() else { return Ok(()) };
    let need = m.size().saturating_add(LOAD_HEADROOM);
    if free < need {
        return Err(format!("not enough free memory to load {} ({} MB free, {} MB needed)", m.name, free >> 20, need >> 20));
    }
    Ok(())
}

impl ExplainBackend for LlmBackend {
    fn models_dir(&self) -> Option<PathBuf> {
        self.dir.clone()
    }

    fn release(&self) {
        if self.unload(None) {
            self.release_pending.store(false, Ordering::SeqCst);
            return;
        }
        // a generation holds the model: it drops it when it finishes (try once more in case it
        // finished in between)
        self.release_pending.store(true, Ordering::SeqCst);
        if self.unload(None) {
            self.release_pending.store(false, Ordering::SeqCst);
        }
    }

    fn ready(&self, m: &ModelInfo) -> std::result::Result<(), (Fallback, String)> {
        if !filmcraft_llm::AVAILABLE || cfg!(target_arch = "wasm32") {
            return Err((Fallback::Unavailable, "AI explanations are not available in this build (built without the `llm` feature)".into()));
        }
        let Some(dir) = &self.dir else { return Err((Fallback::NotInstalled, "there is no data folder for models".into())) };
        if !catalogue::installed(dir, m) {
            return Err((Fallback::NotInstalled, format!("{} is not downloaded (Settings ▸ Color ▸ AI explanations)", m.name)));
        }
        if let Some(Err(e)) = verified(m.id) {
            return Err((Fallback::NotInstalled, format!("{} is damaged ({e}); remove it and download it again", m.name)));
        }
        if !self.is_loaded(m) {
            memory_for(m).map_err(|e| (Fallback::Memory, e))?;
        }
        Ok(())
    }

    fn generate(&self, m: &ModelInfo, system: &str, user: &str, opts: GenOptions) -> std::result::Result<String, String> {
        self.generate_streaming(m, system, user, opts, &mut |_| true)
    }

    fn generate_streaming(
        &self,
        m: &ModelInfo,
        system: &str,
        user: &str,
        opts: GenOptions,
        on_text: &mut dyn FnMut(&str) -> bool,
    ) -> std::result::Result<String, String> {
        let dir = self.dir.as_ref().map(|d| catalogue::model_dir(d, m)).ok_or("there is no data folder for models")?;
        // (one verify / load / generation at a time: the lock is held until the answer is done)
        let mut g = self.lock();
        let verify = match verified(m.id) {
            Some(r) => r,
            None => {
                let r = filmcraft_llm::Model::verify(&dir, m).map_err(|e| e.to_string());
                remember_verified(m.id, r.clone());
                r
            }
        };
        verify?;
        if g.as_ref().is_none_or(|x| x.info().id != m.id) {
            *g = None;
            memory_for(m)?;
            *g = Some(filmcraft_llm::Model::load(&dir, m).map_err(|e| e.to_string())?);
        }
        let model = g.as_mut().ok_or("the model could not be loaded")?;
        let r = model
            .generate_streaming(system, user, opts, on_text)
            .map(|mut g| {
                let text = std::mem::take(&mut g.text);
                *self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(g);
                text
            })
            .map_err(|e| e.to_string());
        self.finish(&mut g);
        r
    }
}

/// The process-wide real backend (None when this build can't run a model).
pub fn real_backend() -> Option<Arc<LlmBackend>> {
    static REAL: OnceLock<Arc<LlmBackend>> = OnceLock::new();
    if !filmcraft_llm::AVAILABLE || cfg!(target_arch = "wasm32") {
        return None;
    }
    Some(REAL.get_or_init(|| Arc::new(LlmBackend::new(models_dir()))).clone())
}

/// AI4.2: folder of the Qwen3-VL 4B release inside [`models_dir`] (the Hugging Face files as
/// downloaded; the text model's GGUF is written next to them on first use).
pub const VISION_MODEL_DIR: &str = "qwen3-vl-4b-instruct";
/// The text model converted by [`filmcraft_llm::convert`] (Q8_0; provisional, HANDOFF AI4.2).
#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
const VISION_TEXT_GGUF: &str = "text-own-q8_0.gguf";

/// AI4.2: the installed vision model as a [`crate::coach_assist::PictureLooker`], when this build
/// can run it and its files are there. Cheap (file-size checks): the model loads on the first look.
pub fn real_looker() -> Option<Arc<dyn crate::coach_assist::PictureLooker>> {
    #[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
    {
        let dir = models_dir()?.join(VISION_MODEL_DIR);
        let models = dir.parent()?;
        if filmcraft_llm::models::VISION_MODEL.installed(models) {
            return Some(Arc::new(VisionLooker { dir }));
        }
    }
    None
}

/// AI4 item 3 (tune evaluation): a looker for the Qwen3-VL release in `dir` (e.g. a tuned copy),
/// without the install check [`real_looker`] makes. Not used by the app.
pub fn looker_at(dir: PathBuf) -> Option<Arc<dyn crate::coach_assist::PictureLooker>> {
    #[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
    {
        Some(Arc::new(VisionLooker { dir }))
    }
    #[cfg(not(all(feature = "llm", not(target_arch = "wasm32"))))]
    {
        let _ = dir;
        None
    }
}

/// Qwen3-VL on FilmCraft's own runtime (text half on a discrete GPU when there is one): loaded on
/// the first look and kept for [`VISION_KEEP`] after the last one, so a follow-up question doesn't
/// load it again, then dropped so editing gets the memory back (AI-PLAN AI4 item 2).
#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
struct VisionLooker {
    dir: PathBuf,
}

/// How long the picture model stays loaded after a look.
#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
const VISION_KEEP: std::time::Duration = std::time::Duration::from_secs(120);
/// Free memory a load needs (the Q8_0 text model ≈ 4.3 GB + the encoder ≈ 1.7 GB + work buffers).
#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
const VISION_LOAD_MEMORY: u64 = 7 << 30;

/// The loaded picture model and the look count when it was put back (the idle drop only drops
/// it when no look came after).
#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
struct KeptVision {
    vl: filmcraft_llm::vl::VisionLanguage,
    dir: PathBuf,
    looks: u64,
}

#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
static KEPT_VISION: std::sync::Mutex<Option<KeptVision>> = std::sync::Mutex::new(None);
/// One look at a time (two loads at once would need twice the memory).
#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
static LOOKING: std::sync::Mutex<u64> = std::sync::Mutex::new(0);

#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
impl crate::coach_assist::PictureLooker for VisionLooker {
    fn look(&self, picture: &crate::coach_assist::PictureInput, question: &str) -> std::result::Result<String, String> {
        self.look_with(picture, &crate::coach_assist::look_prompt(question))
    }

    fn look_with(&self, picture: &crate::coach_assist::PictureInput, prompt: &str) -> std::result::Result<String, String> {
        let mut count = LOOKING.lock().unwrap_or_else(PoisonError::into_inner);
        *count = count.wrapping_add(1);
        let looks = *count;
        // taken out while it works: a look that panics doesn't put a half-used model back (one that
        // returns an error does: every ask starts from a reset cache)
        let kept = KEPT_VISION.lock().unwrap_or_else(PoisonError::into_inner).take();
        let mut vl = match kept {
            Some(k) if k.dir == self.dir => k.vl,
            other => {
                // (a model kept for another folder: dropped here, outside the lock)
                if let Some(k) = other {
                    k.vl.drop_detached();
                }
                // the first look converts the release's text model (~25 s, one time)
                if !self.dir.join(VISION_TEXT_GGUF).is_file() {
                    filmcraft_llm::convert::qwen3vl_text_to_gguf(&self.dir, &self.dir.join(VISION_TEXT_GGUF), filmcraft_llm::convert::Quant::Q8_0, None)
                        .map_err(|e| e.to_string())?;
                }
                if let Some(free) = available_memory()
                    && free < VISION_LOAD_MEMORY
                {
                    return Err(format!(
                        "not enough free memory for the picture model ({:.1} GB free, about {:.0} GB needed)",
                        free as f64 / f64::from(1u32 << 30),
                        VISION_LOAD_MEMORY as f64 / f64::from(1u32 << 30)
                    ));
                }
                filmcraft_llm::vl::VisionLanguage::load(&self.dir, VISION_TEXT_GGUF).map_err(|e| e.to_string())?
            }
        };
        let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            vl.ask(&picture.rgb, picture.w, picture.h, crate::coach_assist::LOOK_PIXELS, prompt, 120, None)
        }));
        let r = match asked {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => {
                // never drop a GPU that may have failed on this (the assistant's) thread
                vl.drop_detached();
                return Err("the picture model stopped unexpectedly".into());
            }
        };
        *KEPT_VISION.lock().unwrap_or_else(PoisonError::into_inner) = Some(KeptVision { vl, dir: self.dir.clone(), looks });
        drop(count);
        drop_vision_when_idle(looks);
        r
    }
}

/// After [`VISION_KEEP`], drops the kept picture model unless another look used it since (a
/// detached thread; when none can be spawned it is dropped now).
#[cfg(all(feature = "llm", not(target_arch = "wasm32")))]
fn drop_vision_when_idle(looks: u64) {
    let idle_drop = move || {
        let stale = {
            let mut g = KEPT_VISION.lock().unwrap_or_else(PoisonError::into_inner);
            if g.as_ref().is_some_and(|k| k.looks == looks) { g.take() } else { None }
        };
        // (dropped outside the lock: freeing the GPU can wait on the device)
        if let Some(k) = stale {
            k.vl.drop_detached();
        }
    };
    let spawned = std::thread::Builder::new().name("filmcraft-vision-idle".into()).spawn(move || {
        std::thread::sleep(VISION_KEEP);
        let _ = std::panic::catch_unwind(idle_drop);
    });
    if spawned.is_err() {
        let kept = KEPT_VISION.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(k) = kept {
            k.vl.drop_detached();
        }
    }
}

/// Where AI models live: `<data dir>/models/<id>/` (the speech models' folder; ids differ).
pub fn models_dir() -> Option<PathBuf> {
    crate::transcript::models_dir()
}

/// SHA-256 results per model id, remembered for the process (verifying reads the whole file).
fn verified_map() -> MutexGuard<'static, HashMap<String, std::result::Result<(), String>>> {
    static V: OnceLock<Mutex<HashMap<String, std::result::Result<(), String>>>> = OnceLock::new();
    V.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner)
}

fn verified(id: &str) -> Option<std::result::Result<(), String>> {
    verified_map().get(id).cloned()
}

fn remember_verified(id: &str, r: std::result::Result<(), String>) {
    verified_map().insert(id.to_string(), r);
}

fn forget_verified(id: &str) {
    verified_map().remove(id);
}

/// Free memory the OS reports (Linux `MemAvailable`); None where unknown (no check then).
fn available_memory() -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = s.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb.saturating_mul(1024))
}

// ---------- the request cache ----------

enum Entry {
    /// `partial`: the checked complete sentences so far (see [`Answer::Pending`]); `speculative`:
    /// a background pre-generation nobody has asked for yet ([`AiExplain::prefetch`]).
    Pending {
        since: web_time::Instant,
        template: String,
        partial: Option<String>,
        speculative: bool,
    },
    Done(Outcome),
}

/// The session's AI explanation state: an optional injected backend, the answer cache and the
/// running model download.
#[derive(Clone)]
pub struct AiExplain {
    backend: Option<Arc<dyn ExplainBackend>>,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    /// Overall cap of one request (load + generate); a pending request older than this is
    /// answered with the template ("timeout").
    pub timeout: Duration,
    /// The model download in progress: (model id, job id).
    pub download: Option<(String, u64)>,
    /// Worker threads alive (see [`MAX_PENDING`]).
    live: Arc<AtomicUsize>,
    /// One job generates at a time; queued jobs whose request is no longer wanted skip.
    gate: Arc<Mutex<()>>,
}

impl Default for AiExplain {
    fn default() -> Self {
        AiExplain { backend: None, entries: Default::default(), timeout: DEFAULT_TIMEOUT, download: None, live: Default::default(), gate: Default::default() }
    }
}

/// Counts a live worker thread; dropped (thread exit, or a thread that never started) it
/// gives the slot back.
struct LiveGuard(Arc<AtomicUsize>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Whether a finished answer is reused: checked texts and rejections are; a timeout or error
/// is asked again next time.
fn cacheable(o: &Outcome) -> bool {
    !matches!(o.code, Some(Fallback::Timeout | Fallback::Error))
}

/// A fixed, plain reason for a failed fact check: never the model's words (they are unchecked
/// text).
fn mismatch_reason(e: &FactMismatch) -> &'static str {
    match e {
        FactMismatch::Empty => "the AI text was empty",
        FactMismatch::TooLong(_) | FactMismatch::TooManySentences(_) => "the AI text was too long",
        FactMismatch::Formatting(_) => "the AI text used formatting",
        FactMismatch::NotEnglish(_) => "the AI text wasn't plain English",
        FactMismatch::Numeral(_) | FactMismatch::Number(_) => "the AI text stated a number the measured facts don't support",
        FactMismatch::Direction(_) => "the AI text stated a direction the measured facts don't support",
        FactMismatch::Range(_) => "the AI text named a tonal range the measured facts don't support",
        FactMismatch::NoChange(_) => "the AI text got wrong whether the node changes the picture",
        FactMismatch::Unsupported(_) => "the AI text claimed something the measured facts don't contain",
        FactMismatch::Intensity(_) => "the AI text got the strength of the change wrong",
        FactMismatch::UnknownWord(_) | FactMismatch::Ungrammatical(..) => "the AI text used wording the fact check can't verify",
        FactMismatch::NoClaim(_) => "the AI text said nothing that can be verified",
        FactMismatch::Selection(_) => "the AI text got the selected area wrong",
        FactMismatch::Omission(_) => "the AI text left out the node's main change",
        FactMismatch::Label(_) => "the AI text named the wrong node",
    }
}

/// `s` safe for a log line: printable ASCII only, at most 200 characters.
fn for_log(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(200).collect()
}

impl std::fmt::Debug for AiExplain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiExplain").field("injected", &self.backend.is_some()).field("timeout", &self.timeout).field("download", &self.download).finish()
    }
}

fn hash_key(model: &str, facts: &str, label: Option<&str>, index: usize) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (model, facts, label, index).hash(&mut h);
    format!("{:016x}", h.finish())
}

fn timeout_reason(t: Duration) -> String {
    format!("the AI model didn't answer within {:.1} s", t.as_secs_f64())
}

/// The template body without its leading "Label: " (the model writes no label).
fn without_label<'a>(sentence: &'a str, label: &str) -> &'a str {
    sentence.strip_prefix(label).and_then(|r| r.strip_prefix(':')).map_or(sentence, str::trim_start)
}

impl AiExplain {
    /// Use `backend` instead of the local model (tests, or a host's own model).
    pub fn set_backend(&mut self, backend: Arc<dyn ExplainBackend>) {
        self.backend = Some(backend);
        self.clear();
    }

    fn backend(&self) -> Option<Arc<dyn ExplainBackend>> {
        self.backend.clone().or_else(|| real_backend().map(|b| b as Arc<dyn ExplainBackend>))
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Free the backend's loaded model (no wait; see [`ExplainBackend::release`]).
    pub fn release(&self) {
        if let Some(b) = self.backend() {
            b.release();
        }
    }

    /// Forget every cached answer (after a model is removed or replaced).
    pub fn clear(&self) {
        self.entries().retain(|_, e| matches!(e, Entry::Pending { .. }));
    }

    /// The explanation of one node: the template at once, or a cached / pending AI answer.
    /// `label` is the node's own name (None when unnamed), `index` its 1-based position and
    /// `display` the label the template sentence starts with.
    pub fn request(&self, prefs: &AiPrefs, facts: &NodeFacts, label: Option<&str>, index: usize, display: &str, template: &str) -> Answer {
        self.request_as(prefs, facts, label, index, display, template, false)
    }

    /// Whether a [`AiExplain::prefetch`] would run the model now (on, a backend, a model, ready):
    /// cheap, so the UI asks before computing a node's facts.
    pub fn can_prefetch(&self, prefs: &AiPrefs) -> bool {
        let Some(backend) = self.backend().filter(|_| prefs.enabled) else { return false };
        resolve_model(&prefs.model, backend.models_dir().as_deref()).is_some_and(|m| backend.ready(m).is_ok())
            && self.live.load(Ordering::Relaxed) < MAX_PENDING
    }

    /// Start writing the explanation in the background before anyone asks (AI1.3: the UI calls
    /// this once the user pauses adjusting a node), so **Explain node** is usually answered from
    /// the cache. Only one pre-generation is wanted at a time: a newer one makes older ones that
    /// nobody asked for yet unwanted (a queued one skips, a running one stops at its next token).
    /// [`AiExplain::request`] for the same facts adopts a pending pre-generation.
    pub fn prefetch(&self, prefs: &AiPrefs, facts: &NodeFacts, label: Option<&str>, index: usize, display: &str, template: &str) -> Answer {
        self.request_as(prefs, facts, label, index, display, template, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn request_as(&self, prefs: &AiPrefs, facts: &NodeFacts, label: Option<&str>, index: usize, display: &str, template: &str, speculative: bool) -> Answer {
        if !prefs.enabled {
            return Answer::Ready(Outcome::template(template, Fallback::Off, "AI explanations are off (Settings ▸ Color ▸ AI explanations)"));
        }
        let Some(backend) = self.backend() else {
            return Answer::Ready(Outcome::template(
                template,
                Fallback::Unavailable,
                "AI explanations are not available in this build (built without the `llm` feature)",
            ));
        };
        let Some(model) = resolve_model(&prefs.model, backend.models_dir().as_deref()) else {
            return Answer::Ready(Outcome::template(template, Fallback::NoModel, "no AI model is selected (Settings ▸ Color ▸ AI explanations)"));
        };
        let facts_json = serde_json::to_string(facts).unwrap_or_default();
        let key = hash_key(model.id, &facts_json, label, index);
        match self.poll(&key) {
            // (a timeout or error is asked again)
            Some(Answer::Ready(o)) if !cacheable(&o) => {}
            Some(a) => {
                if !speculative && let Some(Entry::Pending { speculative: s, .. }) = self.entries().get_mut(&key) {
                    *s = false;
                }
                return a;
            }
            None => {}
        }
        if let Err((code, reason)) = backend.ready(model) {
            return Answer::Ready(Outcome::template(template, code, reason));
        }
        let mut entries = self.entries();
        // (requests past their time cap are answered, polled or not)
        for e in entries.values_mut() {
            if let Entry::Pending { since, template, .. } = e
                && since.elapsed() > self.timeout
            {
                *e = Entry::Done(Outcome::template(template, Fallback::Timeout, timeout_reason(self.timeout)));
            }
        }
        if self.live.load(Ordering::Relaxed) >= MAX_PENDING {
            return Answer::Ready(Outcome::template(template, Fallback::Busy, "the AI model is busy with other nodes; try again in a moment"));
        }
        if entries.len() >= MAX_CACHE {
            let old: Vec<String> = entries.iter().filter(|(_, e)| matches!(e, Entry::Done(_))).map(|(k, _)| k.clone()).take(MAX_CACHE / 2).collect();
            for k in old {
                entries.remove(&k);
            }
        }
        if speculative {
            entries.retain(|k, e| !matches!(e, Entry::Pending { speculative: true, .. }) || *k == key);
        }
        entries.insert(key.clone(), Entry::Pending { since: web_time::Instant::now(), template: template.to_string(), partial: None, speculative });
        drop(entries);

        let (system, user) = filmcraft_llm::explain_prompt(&facts_json, without_label(template, display));
        let job = Job {
            backend,
            model: *model,
            system,
            user,
            facts: facts.clone(),
            label: label.map(str::to_string),
            index,
            display: display.to_string(),
            template: template.to_string(),
            key: key.clone(),
            entries: self.entries.clone(),
            gate: self.gate.clone(),
            timeout: self.timeout,
        };
        self.live.fetch_add(1, Ordering::Relaxed);
        let live = LiveGuard(self.live.clone());
        let spawned = !cfg!(target_arch = "wasm32")
            && std::thread::Builder::new()
                .name("filmcraft-ai-explain".into())
                .spawn(move || {
                    let _live = live;
                    job.run();
                })
                .is_ok();
        if !spawned {
            self.entries().remove(&key);
            return Answer::Ready(Outcome::template(template, Fallback::Error, "couldn't start the AI model"));
        }
        Answer::Pending { key, template: template.to_string(), partial: None }
    }

    /// The state of request `key`: None when unknown (never asked, or forgotten).
    pub fn poll(&self, key: &str) -> Option<Answer> {
        let mut entries = self.entries();
        let e = entries.get_mut(key)?;
        match e {
            Entry::Done(o) => Some(Answer::Ready(o.clone())),
            Entry::Pending { since, template, partial, .. } => {
                if since.elapsed() <= self.timeout {
                    return Some(Answer::Pending { key: key.to_string(), template: template.clone(), partial: partial.clone() });
                }
                let o = Outcome::template(template, Fallback::Timeout, timeout_reason(self.timeout));
                *e = Entry::Done(o.clone());
                Some(Answer::Ready(o))
            }
        }
    }

    /// Poll until `key` is answered, for at most [`MAX_WAIT`] (agents and tests: `"wait":
    /// true`). None when unknown or still pending then.
    pub fn wait(&self, key: &str) -> Option<Outcome> {
        let t0 = web_time::Instant::now();
        loop {
            match self.poll(key)? {
                Answer::Ready(o) => return Some(o),
                Answer::Pending { .. } if t0.elapsed() >= MAX_WAIT => return None,
                Answer::Pending { .. } => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

/// One generation on the worker thread.
struct Job {
    backend: Arc<dyn ExplainBackend>,
    model: ModelInfo,
    system: String,
    user: String,
    facts: NodeFacts,
    label: Option<String>,
    index: usize,
    display: String,
    template: String,
    key: String,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    gate: Arc<Mutex<()>>,
    timeout: Duration,
}

impl Job {
    /// Whether the request still waits for this answer (pending within its time cap).
    fn wanted(&self) -> bool {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        matches!(entries.get(&self.key), Some(Entry::Pending { since, .. }) if since.elapsed() <= self.timeout)
    }

    fn run(self) {
        // (one generation at a time; a queued job whose request timed out or was forgotten skips)
        let _turn = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.wanted() {
            return;
        }
        let opts = gen_options(&self.model);
        // Stream: publish each longer run of complete sentences that passes the fact check; stop
        // when nobody waits for this answer any more (timed out or forgotten).
        let mut checked_len = 0;
        let mut on_text = |so_far: &str| {
            let done = complete_sentences(so_far);
            if done.len() > checked_len {
                checked_len = done.len();
                if let Some(shown) = self.checked_text(done) {
                    let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(Entry::Pending { partial, .. }) = entries.get_mut(&self.key) {
                        *partial = Some(shown);
                    }
                }
            }
            self.wanted()
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.backend.generate_streaming(&self.model, &self.system, &self.user, opts, &mut on_text)
        }));
        let o = match r {
            Err(_) => Outcome::template(&self.template, Fallback::Error, "the AI model stopped unexpectedly"),
            Ok(Err(e)) => {
                log::warn!("AI explanation failed: {}", for_log(&e));
                Outcome::template(&self.template, Fallback::Error, "the AI model failed (see the log)")
            }
            Ok(Ok(text)) => self.checked(&text),
        };
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        // (a late answer replaces a timeout; a forgotten request stays forgotten)
        let replace = match entries.get(&self.key) {
            Some(Entry::Pending { .. }) => true,
            Some(Entry::Done(d)) => d.code == Some(Fallback::Timeout),
            None => false,
        };
        if replace {
            entries.insert(self.key.clone(), Entry::Done(o));
        }
    }

    /// `text` as shown when it passes the fact check (with the node's label in front, like the
    /// template, unless the model wrote it).
    fn checked_text(&self, text: &str) -> Option<String> {
        let text = text.trim();
        check_explanation_labeled(&self.facts, self.label.as_deref(), Some(self.index), text).ok()?;
        Some(if text.starts_with("Node ") { text.to_string() } else { format!("{}: {text}", self.display) })
    }

    fn checked(&self, text: &str) -> Outcome {
        let text = text.trim();
        match check_explanation_labeled(&self.facts, self.label.as_deref(), Some(self.index), text) {
            Ok(()) => {
                // (shown like the template: with the node's label in front, unless the model wrote it)
                let shown = if text.starts_with("Node ") { text.to_string() } else { format!("{}: {text}", self.display) };
                Outcome { text: shown, source: "ai", reason: None, code: None }
            }
            Err(e) => {
                log::info!("AI explanation rejected: {}", for_log(&e.to_string()));
                Outcome::template(&self.template, Fallback::Rejected, mismatch_reason(&e))
            }
        }
    }
}

/// The leading complete sentences of `text` (up to the last `.`, `!` or `?` followed by
/// whitespace; a final one counts only once more text follows it, since "1." may be "1.5").
fn complete_sentences(text: &str) -> &str {
    let mut end = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?') && chars.peek().is_some_and(|(_, n)| n.is_whitespace()) {
            end = i + c.len_utf8();
        }
    }
    text.get(..end).unwrap_or("")
}

/// `grade.explainNode`'s AI fields for one node: `text`, `source`, `reason`, `reasonCode` and,
/// while the model is writing, `aiKey` (poll `ai.explanation`).
/// `prefetch`: start it in the background only ([`AiExplain::prefetch`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn explain_fields(
    s: &Session,
    facts: &NodeFacts,
    node_name: &str,
    index: usize,
    display: &str,
    template: &str,
    wait: bool,
    prefetch: bool,
) -> Value {
    let label = (!node_name.is_empty()).then_some(node_name);
    let answer = if prefetch {
        s.ai.prefetch(&s.prefs.color.ai, facts, label, index, display, template)
    } else {
        s.ai.request(&s.prefs.color.ai, facts, label, index, display, template)
    };
    match answer {
        Answer::Ready(o) => o.to_json(),
        Answer::Pending { key, template, partial } => {
            if wait && let Some(o) = s.ai.wait(&key) {
                return o.to_json();
            }
            json!({"text": template, "source": "template", "reason": null, "reasonCode": null, "pending": true, "aiKey": key, "partial": partial})
        }
    }
}

// ---------- commands ----------

fn model_p(p: &Value, cmd: &str) -> Result<&'static ModelInfo> {
    let id = str_p(p, "model").ok_or_else(|| bad(cmd, "need `model` (an id from ai.models)"))?;
    catalogue::find(id).ok_or_else(|| bad(cmd, format!("unknown model `{}` (see ai.models)", id.chars().take(64).collect::<String>())))
}

/// Whether this build runs the model without AVX2 on x86-64 (≈ 4× slower).
pub fn slow_build() -> bool {
    cfg!(target_arch = "x86_64") && !cfg!(target_feature = "avx2")
}

/// The model download still running: (model id, job id).
fn running_download(s: &Session) -> Option<(&str, u64)> {
    let (id, job) = s.ai.download.as_ref()?;
    let running = s.jobs.iter().any(|j| j.id == *job && !j.progress.finished.load(std::sync::atomic::Ordering::Relaxed));
    running.then_some((id.as_str(), *job))
}

fn models(s: &mut Session, _: &Value) -> Result<Value> {
    let dir = models_dir();
    let chosen = s.prefs.color.ai.model.clone();
    // (not chosen: the automatic model, shown as selected)
    let selected = resolve_model(&chosen, dir.as_deref()).map_or(chosen.clone(), |m| m.id.to_string());
    let downloading = running_download(s);
    Ok(json!({
        "inference": filmcraft_llm::AVAILABLE && !cfg!(target_arch = "wasm32"),
        "canDownload": cfg!(feature = "llm-download"),
        "slowBuild": slow_build(),
        "enabled": s.prefs.color.ai.enabled,
        "selected": selected,
        "automatic": chosen.is_empty(),
        "default": filmcraft_llm::DEFAULT_MODEL,
        "dir": dir.as_ref().map(|d| d.to_string_lossy().to_string()),
        "downloading": downloading.map(|(m, job)| json!({"model": m, "job": job})),
        "models": catalogue::catalogue().iter().map(|m| json!({
            "id": m.id, "name": m.name, "description": m.description, "author": m.author,
            "license": m.license, "licenseUrl": m.license_url, "source": m.source, "size": m.size(),
            "local": m.is_local(), "tuned": m.is_tuned(),
            "installed": dir.as_ref().is_some_and(|d| catalogue::installed(d, m)),
            "missingBytes": dir.as_ref().map_or(m.size(), |d| catalogue::missing_bytes(d, m)),
            "verified": verified(m.id).map(|r| r.is_ok()),
            "selected": m.id == selected,
        })).collect::<Vec<_>>(),
    }))
}

/// Change `color.ai` and save the preferences file (only this group changes, so none of the
/// process-wide switches `Session::set_prefs` applies are touched).
fn save(s: &mut Session, f: impl FnOnce(&mut AiPrefs)) -> Result<()> {
    let mut ai = s.prefs.color.ai.clone();
    f(&mut ai);
    if ai == s.prefs.color.ai {
        return Ok(());
    }
    s.prefs.color.ai = ai;
    if let Some(path) = s.prefs_path.clone() {
        s.prefs.save(&path).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    }
    Ok(())
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let m = model_p(p, "ai.select")?;
    if resolve_model(&s.prefs.color.ai.model, models_dir().as_deref()).is_none_or(|cur| cur.id != m.id) {
        s.ai.release();
    }
    save(s, |a| a.model = m.id.to_string())?;
    Ok(json!({"selected": m.id}))
}

fn set_enabled(s: &mut Session, p: &Value) -> Result<Value> {
    let on = bool_p(p, "on").ok_or_else(|| bad("ai.setEnabled", "need `on` (true or false)"))?;
    save(s, |a| a.enabled = on)?;
    if !on {
        s.ai.release();
    }
    Ok(json!({"enabled": on}))
}

fn explanation(s: &mut Session, p: &Value) -> Result<Value> {
    let key = str_p(p, "key").ok_or_else(|| bad("ai.explanation", "need `key` (the aiKey from grade.explainNode)"))?;
    let wait = bool_p(p, "wait").unwrap_or(false);
    let a = if wait { s.ai.wait(key).map(Answer::Ready) } else { s.ai.poll(key) };
    Ok(match a {
        None => json!({"state": "unknown"}),
        Some(Answer::Pending { template, partial, .. }) => json!({"state": "pending", "text": template, "source": "template", "partial": partial}),
        Some(Answer::Ready(o)) => {
            let mut v = o.to_json();
            v["state"] = json!("done");
            v
        }
    })
}

/// Remove the files of model `m` (only its catalogue files; the folder when empty).
fn remove_files(dir: &Path, m: &ModelInfo) -> std::result::Result<u64, String> {
    let d = catalogue::model_dir(dir, m);
    let mut freed = 0u64;
    for f in m.files {
        let path = d.join(f.name);
        match std::fs::metadata(&path) {
            Ok(md) => {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                freed = freed.saturating_add(md.len());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        // (a partial download, named by the speech downloader `<name>.part`)
        let _ = std::fs::remove_file(d.join(format!("{}.part", f.name)));
    }
    let _ = std::fs::remove_dir(&d);
    Ok(freed)
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "ai.remove";
    let m = model_p(p, CMD)?;
    if running_download(s).is_some_and(|(id, _)| id == m.id) {
        return Err(bad(CMD, "the model is downloading; cancel the download first"));
    }
    let dir = models_dir().ok_or_else(|| bad(CMD, "no data directory for models"))?;
    if let Some(b) = real_backend()
        && !b.unload(Some(m.id))
    {
        return Err(bad(CMD, "the model is busy explaining a node; try again in a moment"));
    }
    forget_verified(m.id);
    s.ai.clear();
    let freed = remove_files(&dir, m).map_err(|e| bad(CMD, e))?;
    Ok(json!({"model": m.id, "freed": freed}))
}

fn can_download(_: &Session) -> std::result::Result<(), String> {
    if cfg!(feature = "llm-download") && !cfg!(target_arch = "wasm32") {
        Ok(())
    } else {
        Err("AI model downloads are not available in this build (built without the `llm-download` feature)".into())
    }
}

/// Download a catalogue model in the background (only on this explicit command, after the user
/// chose it in Settings ▸ Color ▸ AI explanations), then verify it once.
fn download(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "ai.download";
    can_download(s).map_err(|e| bad(CMD, e))?;
    let m = model_p(p, CMD)?;
    if m.is_local() {
        return Err(bad(CMD, format!("{} is built on this computer and can't be downloaded yet", m.name)));
    }
    let dir = models_dir().ok_or_else(|| bad(CMD, "no data directory for models"))?;
    if catalogue::installed(&dir, m) {
        return Ok(json!({"job": null, "installed": true}));
    }
    if let Some((id, job)) = running_download(s) {
        return Err(bad(CMD, format!("{id} is already downloading (job {job})")));
    }
    let wait = bool_p(p, "wait").unwrap_or(false);
    let job = crate::narration::new_job(s, format!("Downloading {}", m.name));
    job.progress.total.store(catalogue::missing_bytes(&dir, m), std::sync::atomic::Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let info = *m;
    let model = m.id.to_string();
    let id = crate::narration::spawn(s, job, wait, "filmcraft-ai-download", move || {
        let t0 = web_time::Instant::now();
        let r = fetch(&dir, &info, &prog).and_then(|()| {
            *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = "Checking the download…".into();
            let r = filmcraft_llm::Model::verify(&catalogue::model_dir(&dir, &info), &info).map_err(|e| e.to_string());
            remember_verified(info.id, r.clone());
            r.map(|()| format!("{} installed", info.name))
        });
        crate::narration::finish(&prog, &res, r, t0);
    })?;
    s.ai.download = Some((model, id));
    Ok(json!({"job": id, "installed": false}))
}

#[cfg(feature = "llm-download")]
fn fetch(dir: &Path, m: &ModelInfo, prog: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    use std::sync::atomic::Ordering;
    let mut progress = |done: u64, total: u64, file: &str| {
        prog.done.store(done, Ordering::Relaxed);
        prog.total.store(total, Ordering::Relaxed);
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = format!("Downloading {file}");
        !prog.cancel.load(Ordering::Relaxed)
    };
    filmcraft_llm::download(dir, m, &mut progress).map_err(|e| e.to_string())
}

#[cfg(not(feature = "llm-download"))]
fn fetch(_: &Path, _: &ModelInfo, _: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    Err("AI model downloads are not available in this build".into())
}

fn cancel_download(s: &mut Session, _: &Value) -> Result<Value> {
    let Some((model, job)) = s.ai.download.clone() else { return Ok(json!({"cancelled": false})) };
    let Some(j) = s.jobs.iter().find(|j| j.id == job) else { return Ok(json!({"cancelled": false})) };
    j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(json!({"cancelled": true, "model": model, "job": job}))
}

/// The picture model ([`filmcraft_llm::models::VISION_MODEL`]): installed or not, its size, a running
/// download (AI4 item 2's install path, Settings ▸ Color ▸ AI explanations).
fn vision_status(s: &mut Session, _: &Value) -> Result<Value> {
    let m = &filmcraft_llm::models::VISION_MODEL;
    let dir = models_dir();
    let downloading = running_download(s).filter(|(id, _)| *id == m.id).map(|(_, job)| job);
    Ok(json!({
        "id": m.id,
        "name": m.name,
        "description": m.description,
        "license": m.license,
        "licenseUrl": m.license_url,
        "author": m.author,
        "source": m.source,
        "size": m.size(),
        "installed": dir.as_deref().is_some_and(|d| m.installed(d)),
        "missingBytes": dir.as_deref().map_or(m.size(), |d| m.missing_bytes(d)),
        "downloadJob": downloading,
        "canDownload": can_download(s).is_ok(),
    }))
}

/// Download the picture model in the background (only on this explicit command). Shares the one
/// download slot with `ai.download`.
fn vision_download(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "ai.vision.download";
    can_download(s).map_err(|e| bad(CMD, e))?;
    let m = &filmcraft_llm::models::VISION_MODEL;
    let dir = models_dir().ok_or_else(|| bad(CMD, "no data directory for models"))?;
    if m.installed(&dir) {
        return Ok(json!({"job": null, "installed": true}));
    }
    if let Some((id, job)) = running_download(s) {
        return Err(bad(CMD, format!("{id} is already downloading (job {job})")));
    }
    let wait = bool_p(p, "wait").unwrap_or(false);
    let job = crate::narration::new_job(s, format!("Downloading {}", m.name));
    job.progress.total.store(m.missing_bytes(&dir), std::sync::atomic::Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let id = crate::narration::spawn(s, job, wait, "filmcraft-ai-download", move || {
        let t0 = web_time::Instant::now();
        let r = fetch_vision(&dir, &prog).map(|()| format!("{} installed", m.name));
        crate::narration::finish(&prog, &res, r, t0);
    })?;
    s.ai.download = Some((m.id.to_string(), id));
    Ok(json!({"job": id, "installed": false}))
}

#[cfg(feature = "llm-download")]
fn fetch_vision(dir: &Path, prog: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    use std::sync::atomic::Ordering;
    let mut progress = |done: u64, total: u64, file: &str| {
        prog.done.store(done, Ordering::Relaxed);
        prog.total.store(total, Ordering::Relaxed);
        *prog.status.lock().unwrap_or_else(PoisonError::into_inner) = format!("Downloading {file}");
        !prog.cancel.load(Ordering::Relaxed)
    };
    filmcraft_llm::download_vision(dir, &mut progress).map_err(|e| e.to_string())
}

#[cfg(not(feature = "llm-download"))]
fn fetch_vision(_: &Path, _: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    Err("AI model downloads are not available in this build".into())
}

/// Remove the picture model: its pinned files, partial downloads and the text model converted
/// from it on this computer (`*.gguf` in its folder).
fn vision_remove(s: &mut Session, _: &Value) -> Result<Value> {
    const CMD: &str = "ai.vision.remove";
    let m = &filmcraft_llm::models::VISION_MODEL;
    if running_download(s).is_some_and(|(id, _)| id == m.id) {
        return Err(bad(CMD, "the picture model is downloading; cancel the download first"));
    }
    let dir = models_dir().ok_or_else(|| bad(CMD, "no data directory for models"))?;
    let freed = remove_vision_files(&dir.join(m.id), m.files).map_err(|e| bad(CMD, e))?;
    Ok(json!({"model": m.id, "freed": freed}))
}

fn remove_vision_files(d: &Path, files: &[filmcraft_llm::ModelFile]) -> std::result::Result<u64, String> {
    let mut freed = 0u64;
    let mut gone = |path: &Path| -> std::result::Result<(), String> {
        match std::fs::metadata(path) {
            Ok(md) => {
                std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
                freed = freed.saturating_add(md.len());
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    };
    for f in files {
        gone(&d.join(f.name))?;
        gone(&d.join(format!("{}.part", f.name)))?;
    }
    // the converted text model(s) and their partial files
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name.ends_with(".gguf") || name.ends_with(".gguf.part") {
                gone(&p)?;
            }
        }
    }
    // only removed when nothing else is left in it (files the user put there stay)
    let _ = std::fs::remove_dir(d);
    Ok(freed)
}

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal: false }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("ai.models", "List AI Explanation Models", "{}", always, models),
        spec("ai.download", "Download AI Model", r#"{"model":str,"wait":bool?}"#, can_download, download),
        spec("ai.cancelDownload", "Cancel AI Model Download", "{}", always, cancel_download),
        spec("ai.remove", "Remove AI Model", r#"{"model":str}"#, always, remove),
        spec("ai.select", "Select AI Model", r#"{"model":str}"#, always, select),
        spec("ai.setEnabled", "AI Explanations On/Off", r#"{"on":bool}"#, always, set_enabled),
        spec("ai.vision.status", "Picture Model Status", "{}", always, vision_status),
        spec("ai.vision.download", "Download Picture Model", r#"{"wait":bool?}"#, can_download, vision_download),
        spec("ai.vision.remove", "Remove Picture Model", "{}", always, vision_remove),
        spec("ai.explanation", "Get AI Explanation", r#"{"key":str,"wait":bool? (blocks at most 20 s)}"#, always, explanation),
    ]
}

#[cfg(test)]
mod release_tests {
    use super::*;

    /// Switching AI explanations off while a generation holds the model: the release waits for
    /// the generation to finish, which then drops the model.
    #[test]
    fn complete_sentences_stop_at_the_last_finished_one() {
        assert_eq!(complete_sentences(""), "");
        assert_eq!(complete_sentences("It warms"), "");
        assert_eq!(complete_sentences("It warms."), "", "a final stop counts once more text follows");
        assert_eq!(complete_sentences("It warms. It"), "It warms.");
        assert_eq!(complete_sentences("Gain 1.5 up. Less red! Why"), "Gain 1.5 up. Less red!");
        assert_eq!(complete_sentences("Größer. ü"), "Größer.");
    }

    #[test]
    fn a_release_during_a_generation_drops_the_model_when_it_finishes() {
        let b = LlmBackend::with_dir(std::env::temp_dir());
        b.release();
        assert!(!b.release_pending.load(Ordering::SeqCst), "nothing running: released at once");
        let mut g = b.lock();
        // (another thread asks while this "generation" holds the lock)
        std::thread::scope(|sc| {
            sc.spawn(|| b.release());
        });
        assert!(b.release_pending.load(Ordering::SeqCst), "pending while the model is in use");
        b.finish(&mut g);
        assert!(g.is_none() && !b.release_pending.load(Ordering::SeqCst));
        drop(g);
        b.release();
        assert!(!b.release_pending.load(Ordering::SeqCst));
    }
}

#[cfg(test)]
mod vision_install_tests {
    use super::*;

    #[test]
    fn the_looker_folder_is_the_pinned_models_folder() {
        assert_eq!(VISION_MODEL_DIR, filmcraft_llm::models::VISION_MODEL.id);
    }

    #[test]
    fn removing_the_picture_model_takes_its_files_and_converted_text_model_only() {
        let d = std::env::temp_dir().join(format!("fc-vision-remove-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("dir");
        let files = filmcraft_llm::models::VISION_MODEL.files;
        for (name, n) in [("config.json", 3usize), ("tokenizer.json.part", 2), ("text-own-q8_0.gguf", 5), ("notes.txt", 7)] {
            std::fs::write(d.join(name), vec![0u8; n]).expect("write");
        }
        let freed = remove_vision_files(&d, files).expect("removed");
        assert_eq!(freed, 3 + 2 + 5);
        assert!(!d.join("config.json").exists() && !d.join("text-own-q8_0.gguf").exists() && !d.join("tokenizer.json.part").exists());
        assert!(d.join("notes.txt").exists(), "a file the user put there stays");
        // nothing there: nothing freed, no error
        std::fs::remove_file(d.join("notes.txt")).expect("cleanup");
        assert_eq!(remove_vision_files(&d, files), Ok(0));
        let _ = std::fs::remove_dir(&d);
    }

    #[test]
    fn the_status_says_what_is_installed_and_what_a_download_costs() {
        let mut s = Session::new(std::sync::Arc::new(crate::FsServices));
        let v = vision_status(&mut s, &json!({})).expect("status");
        assert_eq!(v["id"], "qwen3-vl-4b-instruct");
        assert_eq!(v["size"].as_u64(), Some(filmcraft_llm::models::VISION_MODEL.size()));
        assert!(v["installed"].is_boolean() && v["missingBytes"].is_u64() && v["downloadJob"].is_null());
        assert!(commands().iter().any(|c| c.id == "ai.vision.download"));
    }
}
