//! Voice isolation model commands (`audio.voiceIsolate.*`, fork AUDIO-PLAN A2).
//!
//! The `voice_isolate` audio effect runs DeepFilterNet3 (`filmcraft-voice`) once its model is
//! installed under `<data dir>/models/deepfilternet3/`, and DeNoise until then. The model is
//! never bundled: it is downloaded only on [`download`] (the user's explicit choice), from a pinned
//! URL, checked against its SHA-256, then the checkpoint is extracted and loaded. Sessions point
//! the effect at an installed checkpoint when they start ([`init`]); it loads on first use.
//!
//! | command | does |
//! |---|---|
//! | `audio.voiceIsolate.status` | available in this build, installed, loaded, load error, size, licence, source, running download |
//! | `audio.voiceIsolate.download` | download, verify, extract and load the model in the background (`wait` for tests / CLI) |
//! | `audio.voiceIsolate.cancelDownload` | stop a running download |
//! | `audio.voiceIsolate.installFile` | install from a local file: the release zip (checksum checked) or the checkpoint itself |
//! | `audio.voiceIsolate.remove` | unload and delete the model; the effect falls back to DeNoise |
//!
//! Fork AUDIO-PLAN A3.7 adds a second, "Better" model for rendered voice isolation
//! (`audio.voiceIsolate.render`, [`crate::voice_isolation_render`]): MossFormer2_SE_48K
//! (Apache-2.0), installed under `<data dir>/models/mossformer2-se-48k/`. `status` reports it under
//! `better`; `installFile` / `remove` take `"model": "better"` for it (their default stays the live
//! effect's DeepFilterNet3, "faster"; a render's default is "better"). The app never downloads it (221.6 MB; the official file is at
//! `filmcraft_voice::mossformer::SOURCE_URL`).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{Result, Session};

const JOB_LABEL: &str = "Downloading the voice isolation model";

fn model_dir() -> Option<PathBuf> {
    crate::transcript::models_dir().map(|d| d.join("deepfilternet3"))
}

/// The installed checkpoint, if any.
fn checkpoint() -> Option<PathBuf> {
    model_dir().map(|d| d.join("model_120.ckpt.best")).filter(|p| p.is_file())
}

fn better_dir() -> Option<PathBuf> {
    crate::transcript::models_dir().map(|d| d.join("mossformer2-se-48k"))
}

/// The installed MossFormer2 checkpoint, if any.
fn better_checkpoint() -> Option<PathBuf> {
    better_dir().map(|d| d.join("last_best_checkpoint.pt")).filter(|p| p.is_file())
}

/// Whether a rendered "Better" isolation would run MossFormer2 (built in and installed).
/// (Installed in the data directory, or a checkpoint path already given to `filmcraft-voice`.)
pub(crate) fn better_ready() -> bool {
    #[cfg(feature = "voice-isolation")]
    let set = filmcraft_voice::mossformer::status().0.is_some();
    #[cfg(not(feature = "voice-isolation"))]
    let set = false;
    available() && (better_checkpoint().is_some() || set)
}

/// Point the effect (and the rendered "Better" model) at installed models (cheap: they load on
/// first use). Called when a session starts; does nothing in builds without the model code.
pub fn init() {
    #[cfg(feature = "voice-isolation")]
    {
        if let Some(p) = checkpoint() {
            filmcraft_voice::effect::set_model_path(&p);
        }
        if let Some(p) = better_checkpoint() {
            filmcraft_voice::mossformer::set_model_path(&p);
        }
    }
}

fn better_status() -> Value {
    #[cfg(feature = "voice-isolation")]
    let (path, loaded, error) = filmcraft_voice::mossformer::status();
    #[cfg(not(feature = "voice-isolation"))]
    let (path, loaded, error): (Option<PathBuf>, bool, Option<String>) = (None, false, None);
    json!({
        "available": available(),
        "installed": better_checkpoint().is_some(),
        "loaded": loaded,
        "path": path.map(|p| p.display().to_string()),
        "error": error,
        "name": "MossFormer2_SE_48K (rendered voice isolation, \"Better\")",
        "size": 221_552_019u64,
        "license": "Apache-2.0",
        "source": "https://huggingface.co/alibabasglab/MossFormer2_SE_48K",
        "sampleRates": [48_000],
        "live": false,
    })
}

fn available() -> bool {
    cfg!(feature = "voice-isolation") && !cfg!(target_arch = "wasm32")
}

/// Whether Voice Isolation would run the model now (built in and installed), not DeNoise.
pub(crate) fn ready() -> bool {
    available() && checkpoint().is_some()
}

fn running(s: &Session) -> Option<u64> {
    s.jobs.iter().find(|j| j.label == JOB_LABEL && !j.progress.finished.load(Ordering::Relaxed)).map(|j| j.id)
}

fn status(s: &mut Session, _: &Value) -> Result<Value> {
    #[cfg(feature = "voice-isolation")]
    let (path, loaded, error) = filmcraft_voice::effect::status();
    #[cfg(not(feature = "voice-isolation"))]
    let (path, loaded, error): (Option<PathBuf>, bool, Option<String>) = (None, false, None);
    Ok(json!({
        "available": available(),
        "installed": checkpoint().is_some(),
        "loaded": loaded,
        "path": path.map(|p| p.display().to_string()),
        "error": error,
        "name": "DeepFilterNet3 (voice isolation)",
        "size": 7_986_207u64,
        "license": "MIT OR Apache-2.0",
        "source": "https://github.com/Rikorose/DeepFilterNet",
        "sampleRates": [48_000],
        "download": running(s),
        "better": better_status(),
        "renderDefault": crate::voice_isolation_render::DEFAULT_MODEL,
    }))
}

fn need_build(cmd: &str) -> Result<()> {
    if available() { Ok(()) } else { Err(bad(cmd, "voice isolation is not available in this build (built without the `voice-isolation` feature)")) }
}

fn download(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "audio.voiceIsolate.download";
    need_build(CMD)?;
    if let Some(job) = running(s) {
        return Err(bad(CMD, format!("the model is already downloading (job {job})")));
    }
    let dir = model_dir().ok_or_else(|| bad(CMD, "no data directory for models"))?;
    if checkpoint().is_some() {
        install_checkpoint(CMD, &dir)?;
        return Ok(json!({"job": null, "installed": true}));
    }
    let wait = bool_p(p, "wait").unwrap_or(false);
    let job = crate::narration::new_job(s, JOB_LABEL.into());
    job.progress.total.store(7_986_207, Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let id = crate::narration::spawn(s, job, wait, "filmcraft-voice-download", move || {
        let t0 = web_time::Instant::now();
        let r = fetch(&dir, &prog).and_then(|()| {
            *prog.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = "Checking the model…".into();
            finish_install(&dir)
        });
        crate::narration::finish(&prog, &res, r.map(|()| "Voice isolation model installed".to_string()), t0);
    })?;
    Ok(json!({"job": id, "installed": false}))
}

#[cfg(feature = "voice-isolation")]
fn fetch(dir: &Path, prog: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    use filmcraft_voice::package as pkg;
    let files = [filmcraft_speech::models::ModelFile { name: pkg::ZIP_NAME, url: pkg::ZIP_URL, sha256: pkg::ZIP_SHA256, size: pkg::ZIP_SIZE }];
    let mut progress = |done: u64, total: u64, file: &str| {
        prog.done.store(done, Ordering::Relaxed);
        prog.total.store(total, Ordering::Relaxed);
        *prog.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("Downloading {file}");
        !prog.cancel.load(Ordering::Relaxed)
    };
    filmcraft_speech::models::download_files(dir, &files, &mut progress).map_err(|e| e.to_string())
}

#[cfg(not(feature = "voice-isolation"))]
fn fetch(_: &Path, _: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    Err("voice isolation is not available in this build".into())
}

/// Extract the checkpoint from the downloaded zip (if not done yet) and load it.
#[cfg(feature = "voice-isolation")]
fn finish_install(dir: &Path) -> std::result::Result<(), String> {
    use filmcraft_voice::package as pkg;
    let ckpt = dir.join(pkg::CHECKPOINT);
    if !ckpt.is_file() {
        pkg::extract_checkpoint(&dir.join(pkg::ZIP_NAME), &ckpt).map_err(|e| e.0)?;
    }
    if let Err(e) = filmcraft_voice::effect::install(&ckpt) {
        // A checkpoint that doesn't load is useless: remove it so a new download can replace it.
        let _ = std::fs::remove_file(&ckpt);
        return Err(e.0);
    }
    Ok(())
}

#[cfg(not(feature = "voice-isolation"))]
fn finish_install(_: &Path) -> std::result::Result<(), String> {
    Err("voice isolation is not available in this build".into())
}

fn install_checkpoint(cmd: &str, dir: &Path) -> Result<()> {
    finish_install(dir).map_err(|e| bad(cmd, e))
}

fn cancel_download(s: &mut Session, _: &Value) -> Result<Value> {
    let Some(job) = running(s) else { return Ok(json!({"cancelled": false})) };
    if let Some(j) = s.jobs.iter().find(|j| j.id == job) {
        j.progress.cancel.store(true, Ordering::Relaxed);
    }
    Ok(json!({"cancelled": true, "job": job}))
}

/// SHA-256 of a file, lowercase hex.
#[cfg(feature = "voice-isolation")]
fn sha256(path: &Path) -> std::result::Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or_default());
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Runs on the command thread, unlike `download`: the work is bounded (the release zip is checked by
/// its size before it is hashed, a checkpoint is capped at `weights::MAX_FILE_BYTES`), it's a
/// one-off the user asks for, and no UI control calls it (the panels offer `download`).
fn install_file(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "audio.voiceIsolate.installFile";
    need_build(CMD)?;
    if running(s).is_some() {
        return Err(bad(CMD, "the model is downloading; cancel the download first"));
    }
    let src = PathBuf::from(str_p(p, "path").ok_or_else(|| bad(CMD, "missing `path`"))?);
    if is_better(CMD, p)? {
        let dir = better_dir().ok_or_else(|| bad(CMD, "no data directory for models"))?;
        let warning = install_better(CMD, &src, &dir)?;
        let mut st = status(s, &Value::Null)?;
        if let Some(o) = st.as_object_mut() {
            o.insert("warning".into(), json!(warning));
        }
        return Ok(st);
    }
    let dir = model_dir().ok_or_else(|| bad(CMD, "no data directory for models"))?;
    install_from(CMD, &src, &dir)?;
    status(s, &Value::Null)
}

#[cfg(feature = "voice-isolation")]
fn install_from(cmd: &str, src: &Path, dir: &Path) -> Result<()> {
    use filmcraft_voice::package as pkg;
    let ckpt = dir.join(pkg::CHECKPOINT);
    let is_zip = src.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    // Check the file before anything is written.
    if is_zip {
        // the release's size first, so a huge file is never hashed on the command thread
        let len = std::fs::metadata(src).map_err(|e| bad(cmd, e.to_string()))?.len();
        if len != pkg::ZIP_SIZE {
            return Err(bad(cmd, format!("{} is not the DeepFilterNet3 release ({len} bytes, not {})", src.display(), pkg::ZIP_SIZE)));
        }
        let sha = sha256(src).map_err(|e| bad(cmd, e))?;
        if sha != pkg::ZIP_SHA256 {
            return Err(bad(cmd, format!("{} is not the DeepFilterNet3 release (SHA-256 {sha})", src.display())));
        }
    } else {
        filmcraft_voice::weights::Weights::load(src).map_err(|e| bad(cmd, e.0))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| bad(cmd, e.to_string()))?;
    if is_zip {
        pkg::extract_checkpoint(src, &ckpt).map_err(|e| bad(cmd, e.0))?;
    } else {
        std::fs::copy(src, &ckpt).map_err(|e| bad(cmd, e.to_string()))?;
    }
    install_checkpoint(cmd, dir)
}

#[cfg(not(feature = "voice-isolation"))]
fn install_from(cmd: &str, _: &Path, _: &Path) -> Result<()> {
    need_build(cmd)
}

/// `"model": "better"` (MossFormer2) or `"faster"` / absent (DeepFilterNet3).
fn is_better(cmd: &str, p: &Value) -> Result<bool> {
    match str_p(p, "model") {
        None | Some("faster") => Ok(false),
        Some("better") => Ok(true),
        Some(m) => Err(bad(cmd, format!("unknown model `{m}`: use \"better\" (MossFormer2) or \"faster\" (DeepFilterNet3)"))),
    }
}

/// Check a MossFormer2 checkpoint by loading it (about half a second, size-capped), then copy it in
/// place. Runs on the command thread like `install_from`. A file that loads but isn't the published
/// release (size / SHA-256) is installed with a warning (a fine-tuned copy of the same network
/// works), which is returned.
#[cfg(feature = "voice-isolation")]
fn install_better(cmd: &str, src: &Path, dir: &Path) -> Result<Option<String>> {
    use filmcraft_voice::mossformer as mf;
    let len = std::fs::metadata(src).map_err(|e| bad(cmd, format!("{}: {e}", src.display())))?.len();
    if len > mf::MAX_FILE_BYTES {
        return Err(bad(cmd, format!("{} is not the MossFormer2_SE_48K checkpoint ({len} bytes)", src.display())));
    }
    mf::MossFormer2::load(src).map_err(|e| bad(cmd, e.0))?;
    let warning = if len != mf::CHECKPOINT_SIZE {
        Some(format!(
            "{} isn't the published MossFormer2_SE_48K file ({len} bytes, not {}); installed anyway since it loads",
            src.display(),
            mf::CHECKPOINT_SIZE
        ))
    } else {
        match sha256(src) {
            Ok(h) if h == mf::CHECKPOINT_SHA256 => None,
            Ok(h) => Some(format!("{} isn't the published MossFormer2_SE_48K file (SHA-256 {h}); installed anyway since it loads", src.display())),
            Err(e) => Some(format!("couldn't check {}'s SHA-256 ({e})", src.display())),
        }
    };
    std::fs::create_dir_all(dir).map_err(|e| bad(cmd, e.to_string()))?;
    let dst = dir.join(mf::CHECKPOINT);
    if std::fs::canonicalize(src).ok() != std::fs::canonicalize(&dst).ok() {
        std::fs::copy(src, &dst).map_err(|e| bad(cmd, e.to_string()))?;
    }
    mf::unload();
    mf::set_model_path(&dst);
    Ok(warning)
}

#[cfg(not(feature = "voice-isolation"))]
fn install_better(cmd: &str, _: &Path, _: &Path) -> Result<Option<String>> {
    need_build(cmd).map(|()| None)
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "audio.voiceIsolate.remove";
    if is_better(CMD, p)? {
        #[cfg(feature = "voice-isolation")]
        filmcraft_voice::mossformer::unload();
        let mut freed = 0u64;
        if let Some(dir) = better_dir().filter(|d| d.is_dir()) {
            freed = std::fs::read_dir(&dir).map(|it| it.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0);
            std::fs::remove_dir_all(&dir).map_err(|e| bad(CMD, e.to_string()))?;
        }
        return Ok(json!({"removed": freed > 0, "freed": freed, "model": "better"}));
    }
    if running(s).is_some() {
        return Err(bad(CMD, "the model is downloading; cancel the download first"));
    }
    #[cfg(feature = "voice-isolation")]
    filmcraft_voice::effect::uninstall();
    let mut freed = 0u64;
    if let Some(dir) = model_dir().filter(|d| d.is_dir()) {
        freed = std::fs::read_dir(&dir).map(|it| it.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0);
        std::fs::remove_dir_all(&dir).map_err(|e| bad(CMD, e.to_string()))?;
    }
    Ok(json!({"removed": freed > 0, "freed": freed}))
}

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("audio.voiceIsolate.status", "Voice Isolation Model Status", "{}", status),
        spec("audio.voiceIsolate.download", "Download Voice Isolation Model", r#"{"wait":bool?}"#, download),
        spec("audio.voiceIsolate.cancelDownload", "Cancel Voice Isolation Model Download", "{}", cancel_download),
        spec("audio.voiceIsolate.installFile", "Install Voice Isolation Model from File", r#"{"path":str,"model":str?}"#, install_file),
        spec("audio.voiceIsolate.remove", "Remove Voice Isolation Model", r#"{"model":str?}"#, remove),
    ]
}
