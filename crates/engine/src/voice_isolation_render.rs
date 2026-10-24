//! Rendered voice isolation (`audio.voiceIsolate.render`, fork AUDIO-PLAN A3.7).
//!
//! The live Voice Isolation effect runs DeepFilterNet3 ("Faster": real time, 30 ms latency). The
//! "Better" model, MossFormer2_SE_48K, listens to seconds of audio at once and costs a few tenths of
//! a second of CPU per second of audio, so it can't run live: it renders. The command takes the
//! selected audio clips, reads the part of each clip's source it plays (plus a second either side,
//! at 48 kHz), runs the chosen model over it in a background job and writes
//! `<clip> Voice Isolated.wav` next to the project; when the job ends ([`poll`]) the files are
//! imported into the clips' bins and the clips switch to them, all as one undo step (Undo brings the
//! originals back; the files stay on disk). A stopped or failed job changes nothing and deletes the
//! files it wrote. Without the MossFormer2 model (missing, or a file that won't load) "Better"
//! falls back to DeepFilterNet3 and the job's status says why; with neither model it refuses in
//! plain words. Clips with more than 8 channels, or longer than [`MAX_SAMPLE_CHANNELS`] samples over
//! all channels (about 46 minutes of stereo), are refused with the reason.
//!
//! | param | meaning |
//! |---|---|
//! | `clips` | timeline clips (default: the selection, linked audio included) |
//! | `model` | `"better"` (MossFormer2) or `"faster"` (DeepFilterNet3); default [`DEFAULT_MODEL`] |
//! | `replace` | switch the clips to the rendered files (default `true`); `false` only imports them |
//! | `gate` | adapt the strength to the clip's noise level (default `true`, below); `false` = the full model |
//! | `wait` | run in place and apply at once (control channel / CLI / tests) |
//!
//! Noise-level gating (fork FORK-PLAN §1a.125 (b), `filmcraft_voice::gate`): after the model runs,
//! the clip's speech-to-noise ratio is measured from what the model kept and removed, and the
//! input is mixed back in by the attenuation limit Clean Up Voice would pick for that level: a
//! noisy clip (up to 25 dB) keeps the model's full output, a mildly noisy one is lightened (limit
//! 18 dB falling to 6 dB at 35 dB), and an already clean one (40 dB or more) is written unchanged,
//! because on clean speech whatever a model changes is damage. The job's status names the level
//! and what was done per clip.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use filmcraft_project::{BinId, ClipId, ItemId};
use filmcraft_render::SourceProvider;
use filmcraft_time::Tick;

use crate::commands::{CommandSpec, bad, bool_p, has_seq, str_p};
use crate::{EngineError, Result, Session};

const CMD: &str = "audio.voiceIsolate.render";
pub(crate) const LABEL: &str = "Render Voice Isolation";
const RATE: u32 = 48_000;
/// Longest stretch of source rendered per clip (2 h).
const MAX_SECONDS: i64 = 2 * 3600;
/// Most samples over all channels rendered per clip (2^28: about 46 min of stereo, 93 min of
/// mono): bounds the job's memory (about 10 bytes per sample: input, result, WAV) to under 3 GB
/// and keeps the WAV far below its 4 GB limit.
pub const MAX_SAMPLE_CHANNELS: u64 = 1 << 28;
/// Most channels per clip (the models' limit).
const MAX_CHANNELS: usize = 8;
/// Longest file-name stem, in bytes.
const MAX_STEM: usize = 200;
/// The model a render uses when the caller doesn't say (fork AUDIO-PLAN A3.7 step 3: "better"
/// only if MossFormer2 beats DeepFilterNet3 at both bars; see `voice_isolation_better_eval`).
/// "better" since the Dell run of 2026-10-22 (`~/fork-logs/a37-gate.md`): with noise-level gating,
/// clean speech (VoiceBank-DEMAND, 824 utterances) 58.8 dB mean SI-SDR (DFN3 40.8; bar ≥ 20) and a
/// 4.4 dB margin over DeNoise on non-steady noise at 10 dB SNR (DFN3 0.8; bar ≥ 3). Without the
/// MossFormer2 model a render falls back to "faster" and says so.
pub const DEFAULT_MODEL: &str = "better";

/// One clip's finished render.
pub struct Rendered {
    clip: ClipId,
    seq: ItemId,
    path: String,
    bytes: Arc<[u8]>,
    bin: Option<BinId>,
    /// Source time of the file's first sample.
    offset: Tick,
    old_item: ItemId,
    old_in: Tick,
}

/// A render running as a background job: its files are imported by [`poll`] if it succeeds.
pub struct Pending {
    pub job: u64,
    replace: bool,
    /// The file names this job writes (reserved so a second job never picks the same one).
    paths: Vec<String>,
    out: Arc<Mutex<Vec<Rendered>>>,
}

#[cfg(test)]
thread_local! {
    /// Tests: stop the job (as Cancel would) after this many clips.
    pub(crate) static CANCEL_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Which model runs: (`"better"` | `"faster"`, why the other one was used if it fell back).
fn pick(model: &str) -> std::result::Result<(&'static str, Option<String>), String> {
    if !cfg!(feature = "voice-isolation") || cfg!(target_arch = "wasm32") {
        return Err("voice isolation is not available in this build (built without the `voice-isolation` feature)".into());
    }
    let (better, faster) = (crate::voice_isolation::better_ready(), crate::voice_isolation::ready());
    match (model, better, faster) {
        ("better", true, _) => Ok(("better", None)),
        ("better", false, true) => Ok(("faster", Some("the MossFormer2 model isn't installed, so DeepFilterNet3 was used".into()))),
        (_, _, true) => Ok(("faster", None)),
        ("better", false, false) => Err("no voice isolation model is installed: install MossFormer2 (\"Better\") or DeepFilterNet3 (\"Faster\") first".into()),
        _ => Err("the DeepFilterNet3 voice isolation model isn't installed (download it in Settings, or `audio.voiceIsolate.download`)".into()),
    }
}

/// Run `model` over `chans` (48 kHz): the result, the model that ran, and why it isn't the one
/// asked for (MossFormer2 that won't load falls back to DeepFilterNet3 when that is installed).
#[cfg(feature = "voice-isolation")]
fn isolate(
    model: &str,
    chans: &[Vec<f32>],
    progress: &mut dyn FnMut(f32) -> bool,
) -> std::result::Result<(Vec<Vec<f32>>, &'static str, Option<String>), String> {
    let faster = |progress: &mut dyn FnMut(f32) -> bool| filmcraft_voice::effect::isolate_offline(chans, progress).map_err(|e| e.0);
    if model != "better" {
        return faster(progress).map(|y| (y, "faster", None));
    }
    match filmcraft_voice::mossformer::shared() {
        Ok(m) => m.enhance(chans, progress).map(|y| (y, "better", None)).map_err(|e| e.0),
        Err(e) if crate::voice_isolation::ready() => {
            faster(progress).map(|y| (y, "faster", Some(format!("MossFormer2 didn't load ({e}), so DeepFilterNet3 was used"))))
        }
        Err(e) => Err(format!("MossFormer2 didn't load: {e}")),
    }
}

#[cfg(not(feature = "voice-isolation"))]
fn isolate(_: &str, _: &[Vec<f32>], _: &mut dyn FnMut(f32) -> bool) -> std::result::Result<(Vec<Vec<f32>>, &'static str, Option<String>), String> {
    Err("voice isolation is not available in this build".into())
}

/// What to render for one clip.
struct Work {
    clip: ClipId,
    item: ItemId,
    name: String,
    stream: usize,
    channels: usize,
    from: Tick,
    to: Tick,
    old_in: Tick,
    bin: Option<BinId>,
    path: String,
}

/// A clip name made safe as a file-name stem: no extension, path separators, characters Windows
/// refuses (`<>:"/\\|?*`) or control characters, at most [`MAX_STEM`] bytes, never empty.
pub(crate) fn safe_stem(name: &str) -> String {
    let base = name.rsplit_once('.').map_or(name, |(a, _)| a);
    let mut out = String::new();
    for c in base.chars().filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        if out.len() + c.len_utf8() > MAX_STEM {
            break;
        }
        out.push(c);
    }
    let t = out.trim().trim_end_matches('.').to_string();
    if t.is_empty() { "Clip".into() } else { t }
}

/// A free `<stem> Voice Isolated[ n].wav` in `dir`: not on disk and not reserved by a running job.
fn reserve_path(s: &Session, dir: &str, stem: &str, taken: &[String]) -> std::result::Result<String, String> {
    for k in 1..=999 {
        let name = if k == 1 { format!("{stem} Voice Isolated.wav") } else { format!("{stem} Voice Isolated {k}.wav") };
        let path = std::path::Path::new(dir).join(name).to_string_lossy().into_owned();
        let reserved = taken.contains(&path) || s.isolate_jobs.iter().any(|j| j.paths.contains(&path));
        if !reserved && s.services.file_size(&path).is_err() {
            return Ok(path);
        }
    }
    Err(format!("999 files named \"{stem} Voice Isolated\" already exist next to the project; move or rename some"))
}

/// Delete files a stopped or failed job wrote (best effort; the host's file system).
fn discard(paths: &[String]) {
    #[cfg(not(target_arch = "wasm32"))]
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = paths;
}

fn render(s: &mut Session, p: &Value) -> Result<Value> {
    let model = str_p(p, "model").unwrap_or(DEFAULT_MODEL);
    if model != "better" && model != "faster" {
        return Err(bad(CMD, format!("unknown model `{model}`: use \"better\" (MossFormer2) or \"faster\" (DeepFilterNet3)")));
    }
    let replace = bool_p(p, "replace").unwrap_or(true);
    let gate = bool_p(p, "gate").unwrap_or(true);
    let wait = bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32");
    let clips = crate::essential_sound::targets(s, p);
    if clips.is_empty() {
        return Err(bad(CMD, "select one or more audio clips"));
    }
    let (used, note) = pick(model).map_err(|e| bad(CMD, e))?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let dir = s
        .path
        .as_deref()
        .and_then(|x| std::path::Path::new(x).parent())
        .map(|d| d.to_string_lossy().into_owned())
        .ok_or_else(|| bad(CMD, "save the project first: the rendered audio is written next to it"))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut work = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    for it in seq.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| clips.contains(&i.id)) {
        let Some((_, m, _)) = s.project.resolve_media(it.item) else { continue };
        let Some(a) = m.info.audio() else { continue };
        let channels = usize::try_from(a.channels).unwrap_or(usize::MAX).max(1);
        if channels > MAX_CHANNELS {
            skipped.push(format!("{}: {channels} channels, the voice models take up to {MAX_CHANNELS}", it.name));
            continue;
        }
        let (x, y) = (it.source_time_at(it.start), it.source_time_at((it.end() - Tick(1)).max(it.start)));
        let second = Tick::from_units(1, 1);
        let from = (x.min(y) - second).max(Tick::ZERO);
        let to = (x.max(y) + second).min(m.info.duration.max(Tick::ZERO)).min(from + Tick::from_units(MAX_SECONDS, 1));
        if to <= from {
            continue;
        }
        let frames = u64::try_from(to.to_units_floor(i64::from(RATE)).saturating_sub(from.to_units_floor(i64::from(RATE)))).unwrap_or(u64::MAX);
        if frames.saturating_mul(channels as u64) > MAX_SAMPLE_CHANNELS {
            let mins = MAX_SAMPLE_CHANNELS / channels as u64 / u64::from(RATE) / 60;
            skipped.push(format!("{}: too long to render in one go (up to {mins} min with {channels} channel(s)); split it first", it.name));
            continue;
        }
        let path = match reserve_path(s, &dir, &safe_stem(&it.name), &taken) {
            Ok(p) => p,
            Err(e) => {
                skipped.push(format!("{}: {e}", it.name));
                continue;
            }
        };
        taken.push(path.clone());
        work.push(Work {
            clip: it.id,
            item: it.item,
            name: it.name.clone(),
            stream: it.audio_stream,
            channels,
            from,
            to,
            old_in: it.source_in,
            bin: s.project.root.parent_of(it.item).filter(|b| *b != s.project.root.id),
            path,
        });
    }
    if work.is_empty() {
        let why = if skipped.is_empty() { "the selected clips have no audio".to_string() } else { skipped.join("; ") };
        return Err(bad(CMD, why));
    }
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let services = s.services.clone();
    let n = work.len();
    let job = crate::narration::new_job(s, format!("{LABEL} ({n} clip{})", if n == 1 { "" } else { "s" }));
    let id = job.id;
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let out: Arc<Mutex<Vec<Rendered>>> = Arc::default();
    let out2 = out.clone();
    let model_name = if used == "better" { "MossFormer2" } else { "DeepFilterNet3" };
    let skipped2 = skipped.clone();
    #[cfg(test)]
    let cancel_after = CANCEL_AFTER.with(std::cell::Cell::get);
    let run = move || {
        let t0 = web_time::Instant::now();
        let mut errors = skipped2;
        let mut notes: Vec<String> = note.into_iter().collect();
        let mut written: Vec<String> = Vec::new();
        prog.total.store(1000 * n as u64, Ordering::Relaxed);
        for (k, w) in work.iter().enumerate() {
            *lock(&prog.status) = format!("{}: {model_name}, clip {} of {n}", w.name, k + 1);
            match render_one(w, used, gate, &provider, &*services, &mut |f| {
                prog.done.store(1000 * k as u64 + (f.clamp(0.0, 1.0) * 1000.0) as u64, Ordering::Relaxed);
                !prog.cancel.load(Ordering::Relaxed)
            }) {
                Ok((bytes, nt, level)) => {
                    if let Some(nt) = nt.filter(|nt| !notes.contains(nt)) {
                        notes.push(nt);
                    }
                    if let Some(level) = level {
                        notes.push(format!("{}: {level}", w.name));
                    }
                    written.push(w.path.clone());
                    lock(&out2).push(Rendered {
                        clip: w.clip,
                        seq: seq_id,
                        path: w.path.clone(),
                        bytes,
                        bin: w.bin,
                        offset: w.from,
                        old_item: w.item,
                        old_in: w.old_in,
                    })
                }
                Err(e) => errors.push(format!("{}: {e}", w.name)),
            }
            #[cfg(test)]
            if cancel_after == Some(k + 1) {
                prog.cancel.store(true, Ordering::Relaxed);
            }
            if prog.cancel.load(Ordering::Relaxed) {
                break;
            }
        }
        let done = lock(&out2).len();
        let r = if prog.cancel.load(Ordering::Relaxed) {
            // nothing is applied: forget the renders and delete their files
            lock(&out2).clear();
            discard(&written);
            Err("stopped: nothing was changed (the rendered files were deleted)".to_string())
        } else if done == 0 {
            Err(errors.join("; "))
        } else {
            let mut msg = format!("Rendered voice isolation ({model_name}) for {done} clip(s) in {:.1} s", t0.elapsed().as_secs_f64());
            for nt in &notes {
                msg.push_str(&format!(". {nt}"));
            }
            if !errors.is_empty() {
                msg.push_str(&format!(". Skipped: {}", errors.join("; ")));
            }
            Ok(msg)
        };
        crate::narration::finish(&prog, &res, r, t0);
    };
    let paths: Vec<String> = taken;
    crate::narration::spawn(s, job, wait, "filmcraft-voice-render", run)?;
    s.isolate_jobs.push(Pending { job: id, replace, paths, out });
    if wait {
        // a failed or stopped job has nothing to apply ([`apply_finished`] checks its result)
        let applied = apply_finished(s);
        let j = s.jobs.iter().find(|j| j.id == id);
        if let Some(Err(e)) = j.and_then(|j| lock(&j.result).clone()) {
            return Err(bad(CMD, e));
        }
        return Ok(json!({"job": id, "model": used, "applied": applied, "skipped": skipped}));
    }
    Ok(json!({"job": id, "model": used, "clips": n, "skipped": skipped}))
}

/// What noise-level gating did, in plain words, for the job's status.
pub(crate) fn gate_note(snr: Option<f64>, limit: f32) -> String {
    use filmcraft_voice::gate::{BYPASS, FULL};
    let level = snr.filter(|v| v.is_finite()).map_or("noise level not measurable".to_string(), |v| format!("speech {v:.0} dB over the noise"));
    if limit.is_nan() || limit >= FULL {
        format!("{level}, full voice isolation")
    } else if limit <= BYPASS {
        format!("{level}, already clean: left unchanged")
    } else {
        format!("{level}, light voice isolation (at most {limit:.0} dB taken away)")
    }
}

/// Render one clip: the WAV bytes, why another model ran (if it did), and what noise-level gating
/// did (when on).
fn render_one(
    w: &Work,
    model: &str,
    gate: bool,
    provider: &dyn SourceProvider,
    services: &dyn crate::Services,
    progress: &mut dyn FnMut(f32) -> bool,
) -> std::result::Result<(Arc<[u8]>, Option<String>, Option<String>), String> {
    let src = provider.source(w.item).ok_or("the media is offline")?;
    let start = w.from.to_units_floor(i64::from(RATE));
    let frames = usize::try_from(w.to.to_units_floor(i64::from(RATE)).saturating_sub(start)).map_err(|_| "bad range")?;
    if (frames as u64).saturating_mul(w.channels as u64) > MAX_SAMPLE_CHANNELS {
        return Err("too long to render in one go; split it first".into());
    }
    // read in 10 s pieces so a long source never decodes in one call
    let mut chans: Vec<Vec<f32>> = (0..w.channels).map(|_| Vec::new()).collect();
    for c in chans.iter_mut() {
        c.try_reserve_exact(frames).map_err(|_| "not enough memory for this clip")?;
    }
    let mut pos = 0usize;
    while pos < frames {
        let len = (frames - pos).min(10 * RATE as usize);
        let buf = src.audio_stream(w.stream, start + pos as i64, len, RATE).map_err(|e| e.to_string())?;
        for (c, out) in chans.iter_mut().enumerate() {
            let ch = buf.channels.get(c).or_else(|| buf.channels.first());
            out.extend((0..len).map(|i| ch.and_then(|x| x.get(i)).copied().unwrap_or(0.0)));
        }
        pos += len;
    }
    let (mut y, _, note) = isolate(model, &chans, progress)?;
    let level = gate.then(|| {
        let (snr, limit) = filmcraft_voice::gate::gate(&chans, &mut y, RATE);
        gate_note(snr, limit)
    });
    drop(chans);
    let views: Vec<&[f32]> = y.iter().map(Vec::as_slice).collect();
    let bytes: Arc<[u8]> = filmcraft_media::wav::try_write_wav16_planar(&views, RATE)?.into();
    drop(views);
    drop(y);
    if services.file_size(&w.path).is_ok() {
        return Err(format!("{} appeared while rendering; nothing was overwritten", w.path));
    }
    services.write_file(&w.path, &bytes).map_err(|e| format!("{}: {e}", w.path))?;
    Ok((bytes, note, level))
}

/// Import finished renders and switch their clips: one undo step per job. Called once per UI frame
/// from [`Session::poll_persistence`].
pub fn poll(s: &mut Session) {
    apply_finished(s);
}

fn apply_finished(s: &mut Session) -> Vec<Value> {
    let mut applied = Vec::new();
    let mut i = 0;
    while let Some(pj) = s.isolate_jobs.get(i) {
        let job = s.jobs.iter().find(|j| j.id == pj.job);
        if !job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed)) {
            i += 1;
            continue;
        }
        let ok = job.is_some_and(|j| matches!(*lock(&j.result), Some(Ok(_))) && !j.progress.cancel.load(Ordering::Relaxed));
        let pj = s.isolate_jobs.remove(i);
        let done = std::mem::take(&mut *lock(&pj.out));
        if !ok {
            // a stopped or failed job changes nothing (its files are already gone)
            discard(&done.iter().map(|r| r.path.clone()).collect::<Vec<_>>());
            continue;
        }
        let n0 = s.history.undo.len();
        for r in done {
            match apply_one(s, &r, pj.replace) {
                Ok(v) => applied.push(v),
                Err(e) => s.error_toast(CMD, format!("{LABEL}: {e}")),
            }
        }
        crate::clip_ops::collapse_history(s, n0, LABEL);
    }
    applied
}

fn apply_one(s: &mut Session, r: &Rendered, replace: bool) -> Result<Value> {
    let item = crate::commands::import_bytes(s, &r.path, r.bytes.clone(), r.bin)?;
    let mut switched = false;
    if replace {
        let (clip, seq, old_item, old_in, offset) = (r.clip, r.seq, r.old_item, r.old_in, r.offset);
        switched = s.edit(LABEL, |p, _| {
            let Some(seq) = p.sequence_mut(seq) else { return Ok(false) };
            // only a clip still showing the same source moves to the rendered file
            let Some(it) = seq.audio_tracks.iter_mut().flat_map(|t| t.items.iter_mut()).find(|i| i.id == clip && i.item == old_item && i.source_in == old_in)
            else {
                return Ok(false);
            };
            it.item = item;
            it.audio_stream = 0;
            it.source_in = (old_in - offset).max(Tick::ZERO);
            Ok(true)
        })?;
    }
    Ok(json!({"clip": r.clip.0, "item": item.0, "path": r.path, "replaced": switched}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: LABEL,
        menu: &[],
        shortcut: None,
        params: r#"{"clips":[id]?,"model":str?,"replace":bool?,"gate":bool?,"wait":bool?}"#,
        enabled: has_seq,
        run: render,
        journal: true,
    }]
}
