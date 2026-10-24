//! One-click voice clean-up (`audio.cleanupVoice`, AUDIO-PLAN A2 item 2).
//!
//! Analyses a voice clip and sets the ordinary, visible Essential Sound controls a person would set
//! by hand following the written process in `docs/guide/audio.md` ("Cleaning up a voice"):
//! Reduce Rumble (high-pass), Voice Isolation or Reduce Noise, DeEss, Dynamics, and Loudness matched
//! to the delivery target with a true-peak limit. Nothing is hidden: the result is a Dialogue clip
//! whose sliders show what was done, one undo step, every value adjustable afterwards. It runs as a
//! background job (several renders of the clip) applied by [`poll`] when done; `"wait": true` runs it
//! in place and returns the report. With `"apply": false` it only reports the suggestion ("Suggest
//! settings"), always in place. Both in-place forms block the caller for the renders, so they are for
//! the control channel, CLI and MCP only (whoever asks reads the report); the UI's Clean Up Voice
//! button sends neither and never waits.
//!
//! | measured (on the clip without its Essential Sound effects) | sets |
//! |---|---|
//! | speech-to-noise ratio: what Voice Isolation keeps vs removes (model installed), else the noise floor vs speech level (10th / 95th percentile of 20 ms frame levels) | Voice Isolation (model installed) or Reduce Noise, amount by the ratio |
//! | energy below 80 Hz | Reduce Rumble 66 Hz, or 78 Hz when the low end is heavy |
//! | 5–9 kHz vs 1–4 kHz energy in loud frames | DeEss, amount by the excess |
//! | loudness range (EBU R128 LRA) | Dynamics amount |
//! | integrated loudness and true peak after all of the above | match gain to the target, Hard Limiter under the ceiling |

use serde_json::{Value, json};

use filmcraft_audio_dsp::LoudnessMeter;
use filmcraft_audio_dsp::biquad::{Biquad, Coeffs, FilterType};
use filmcraft_project::essential::{self as es, AudioType, EssentialSound, Slot};
use filmcraft_project::{ClipId, TrackItem};
use filmcraft_render::SourceProvider;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use filmcraft_project::ItemId;

use crate::commands::{CommandSpec, bad, bool_p, f64_p, has_seq};
use crate::essential_sound::{mt_of, targets, update};
use crate::{EngineError, Result, Session};

/// Delivery defaults: YouTube / most streaming services (AUDIO-PLAN A2).
pub const DEFAULT_TARGET_LUFS: f64 = -14.0;
pub const DEFAULT_CEILING_DBTP: f64 = -1.0;
/// How close the integrated loudness must land (LU); the gate allows ±0.5.
const LOUDNESS_TOLERANCE: f64 = 0.1;
/// Measurement passes for the loudness match (the limiter moves the result a little each time).
const PASSES: usize = 4;
/// Frames quieter than this are digital silence, not the noise floor.
const SILENCE_DB: f64 = -100.0;
/// At least this many 20 ms frames (0.5 s) of sound to analyse.
const MIN_FRAMES: usize = 25;

/// What the analysis measured on the clip's own sound.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    /// 10th percentile of the 20 ms frame levels (dBFS).
    pub noise_db: f64,
    /// 95th percentile of the frame levels (dBFS).
    pub speech_db: f64,
    /// Energy below 80 Hz relative to the whole signal (dB).
    pub low_db: f64,
    /// 5–9 kHz relative to 1–4 kHz energy in the loud frames (dB).
    pub sibilance_db: f64,
    /// Loudness range (LU), `None` when too short to measure.
    pub lra: Option<f64>,
    /// Speech-to-noise ratio measured with Voice Isolation itself (what it keeps vs what it
    /// removes, dB), when Voice Isolation is used: it hears impulsive and gusty noise (keyboard,
    /// wind) that the frame-level percentiles miss between the clicks and gusts.
    pub model_snr_db: Option<f64>,
}

impl Analysis {
    /// Speech level over the noise floor (frame-level percentiles).
    pub fn snr_db(&self) -> f64 {
        self.speech_db - self.noise_db
    }

    /// The speech-to-noise ratio the noise reduction is chosen by: the model's when measured.
    pub fn noise_snr_db(&self) -> f64 {
        self.model_snr_db.filter(|v| v.is_finite()).unwrap_or_else(|| self.snr_db())
    }

    fn to_value(&self) -> Value {
        json!({
            "noiseFloorDb": self.noise_db,
            "speechDb": self.speech_db,
            "snrDb": self.snr_db(),
            "lowDb": self.low_db,
            "sibilanceDb": self.sibilance_db,
            "lra": self.lra,
            "modelSnrDb": self.model_snr_db,
        })
    }
}

fn db10(e: f64) -> f64 {
    10.0 * e.max(1e-30).log10()
}

/// `p`-th percentile (0…1) of sorted values.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    let last = sorted.len().checked_sub(1)?;
    let i = ((last as f64) * p.clamp(0.0, 1.0)).round() as usize;
    sorted.get(i.min(last)).copied()
}

/// Analyse `item`'s signal (clip gain + clip effects, sequence rate `sr`). `None` when the clip has
/// no audio source, less than half a second of sound, or `stop` was set (checked every second of
/// audio, like [`model_snr`] and [`measure`]).
pub fn analyse(item: &TrackItem, sr: u32, sources: &dyn SourceProvider, stop: &AtomicBool) -> Option<Analysis> {
    let mono = filmcraft_render::audio::clip_is_mono(item, sources)?;
    let srf = f64::from(sr.max(1));
    let frame = (sr / 50).max(1) as usize;
    let lp = Coeffs::design(FilterType::LowPass, srf, 80.0, std::f64::consts::FRAC_1_SQRT_2, 0.0);
    let sib = Coeffs::design(FilterType::BandPass, srf, 7000.0, 1.75, 0.0);
    let pres = Coeffs::design(FilterType::BandPass, srf, 2000.0, 0.67, 0.0);
    let (mut f_lp, mut f_sib, mut f_pres) = (Biquad::default(), Biquad::default(), Biquad::default());
    let mut meter = LoudnessMeter::new(srf, if mono { 1 } else { 2 });
    // per frame: (level dB, sibilant energy, presence energy); totals for the low-end ratio
    let mut frames: Vec<(f64, f64, f64)> = Vec::new();
    let (mut e_all, mut e_low) = (0.0f64, 0.0f64);
    let (mut acc, mut acc_sib, mut acc_pres, mut acc_n) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    let a0 = item.start.to_units_floor(i64::from(sr));
    let a1 = item.end().to_units_floor(i64::from(sr));
    let mut pos = a0;
    while pos < a1 {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let n = a1.saturating_sub(pos).min(i64::from(sr)) as usize;
        let [l, r] = filmcraft_render::audio::clip_signal(item, pos, n, sr, sources)?;
        let m: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
        if mono {
            meter.process(&[&m]);
        } else {
            meter.process(&[&l, &r]);
        }
        for &x in &m {
            let x = if x.is_finite() { f64::from(x) } else { 0.0 };
            let lo = f_lp.tick(&lp, x);
            let s = f_sib.tick(&sib, x);
            let p = f_pres.tick(&pres, x);
            e_all += x * x;
            e_low += lo * lo;
            acc += x * x;
            acc_sib += s * s;
            acc_pres += p * p;
            acc_n += 1;
            if acc_n == frame {
                frames.push((db10(acc / frame as f64), acc_sib, acc_pres));
                (acc, acc_sib, acc_pres, acc_n) = (0.0, 0.0, 0.0, 0);
            }
        }
        pos += n as i64;
    }
    let mut levels: Vec<f64> = frames.iter().map(|f| f.0).filter(|l| *l > SILENCE_DB).collect();
    if levels.len() < MIN_FRAMES {
        return None;
    }
    levels.sort_by(f64::total_cmp);
    let noise_db = percentile(&levels, 0.10)?;
    let speech_db = percentile(&levels, 0.95)?;
    let (mut s_sum, mut p_sum) = (0.0f64, 0.0f64);
    for (lvl, s, p) in &frames {
        if *lvl >= speech_db - 10.0 {
            s_sum += s;
            p_sum += p;
        }
    }
    let lra = meter.loudness_range();
    Some(Analysis {
        noise_db,
        speech_db,
        low_db: db10(e_low) - db10(e_all),
        sibilance_db: db10(s_sum) - db10(p_sum),
        lra: lra.is_finite().then_some(lra),
        model_snr_db: None,
    })
}

/// Speech-to-noise ratio of `raw` (the clip without Essential Sound) as Voice Isolation hears it:
/// the energy it keeps over the energy it removes, at full amount (dB). `None` when silent.
pub fn model_snr(raw: &TrackItem, mt: filmcraft_time::Tick, sr: u32, sources: &dyn SourceProvider, stop: &AtomicBool) -> Option<f64> {
    let mut st = EssentialSound::new(AudioType::Dialogue);
    st.repair.isolate = Slot { on: true, amount: 10.0 };
    let mut vi = raw.clone();
    vi.essential = Some(st);
    es::apply(&mut vi, None, mt);
    let (mut kept, mut removed) = (0.0f64, 0.0f64);
    let a0 = raw.start.to_units_floor(i64::from(sr));
    let a1 = raw.end().to_units_floor(i64::from(sr));
    let mut pos = a0;
    while pos < a1 {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let n = a1.saturating_sub(pos).min(i64::from(sr)) as usize;
        let [l, r] = filmcraft_render::audio::clip_signal(raw, pos, n, sr, sources)?;
        let [vl, vr] = filmcraft_render::audio::clip_signal(&vi, pos, n, sr, sources)?;
        for (((a, b), c), d) in l.iter().zip(&r).zip(&vl).zip(&vr) {
            let x = 0.5 * f64::from(a + b);
            let y = 0.5 * f64::from(c + d);
            if x.is_finite() && y.is_finite() {
                kept += y * y;
                removed += (x - y) * (x - y);
            }
        }
        pos += n as i64;
    }
    (kept > 1e-20).then(|| db10(kept) - db10(removed))
}

/// The settings the written manual process arrives at for this analysis, on top of `prev` (the
/// clip's current Dialogue settings, so DeHum, Reduce Reverb, EQ, Enhance and Creative stay as they
/// are). `isolate`: use Voice Isolation rather than Reduce Noise.
pub fn suggest(a: &Analysis, isolate: bool, prev: Option<&EssentialSound>, ceiling_dbtp: f64) -> EssentialSound {
    let mut st = prev.filter(|p| p.kind == AudioType::Dialogue).cloned().unwrap_or_else(|| EssentialSound::new(AudioType::Dialogue));
    st.preset.clear();
    st.repair.enabled = true;
    st.clarity.enabled = true;
    st.loudness.enabled = true;
    // 1. rumble: a voice has nothing useful below ~70 Hz
    st.repair.rumble = Slot { on: true, amount: if a.low_db > -15.0 { 4.0 } else { 3.0 } };
    // 2. noise: the further the floor sits under the voice, the less to take away
    let snr = a.noise_snr_db();
    let level = if snr < 15.0 {
        3
    } else if snr < 25.0 {
        2
    } else if snr < 40.0 {
        1
    } else {
        0
    };
    let (iso, nr) = match level {
        3 => (10.0, 7.0),
        2 => (10.0, 5.0),
        1 => (8.0, 3.0),
        _ => (0.0, 0.0),
    };
    st.repair.isolate = Slot { on: isolate && level > 0, amount: if iso > 0.0 { iso } else { st.repair.isolate.amount } };
    // fork A3.6 / A3.7 (FORK-PLAN §1a.125 (b)): Voice Isolation's attenuation limit follows the
    // noise level (`filmcraft_voice::gate`, shared with the rendered models): full strength up to
    // 25 dB SNR, then lighter as the clip gets cleaner (18 dB falling to 6 dB at 35 dB SNR; 12 dB
    // at 30 dB, the limit the A3.6 LibriSpeech sweep found best for mild noise), and from 40 dB
    // Voice Isolation stays off. Louder noise keeps no limit: in `cleanup_gate` 24 / 18 dB limits
    // let loud keyboard clicks through (−1.1 / −0.6 dB against the manual process at 0 / 5 dB
    // SNR), and without a limit every case passes.
    if level > 0 {
        let limit = f64::from(filmcraft_voice::gate::limit_for_snr(snr));
        st.repair.isolate_limit_db = if limit >= 100.0 { 100.0 } else { limit.clamp(filmcraft_voice::gate::MIN_LIMIT_DB, 100.0) };
    }
    st.repair.noise = Slot { on: !isolate && level > 0, amount: if nr > 0.0 { nr } else { st.repair.noise.amount } };
    // 3. sibilance
    let sib = a.sibilance_db;
    st.repair.deess = if sib.is_finite() && sib > -8.0 {
        Slot { on: true, amount: (4.0 + (sib + 8.0) * 0.5).clamp(2.0, 8.0) }
    } else {
        Slot { on: false, amount: st.repair.deess.amount }
    };
    // 4. dynamics: even out a wide range, always a little
    let amount = a.lra.map(|l| ((l - 2.0) * 0.6).clamp(2.0, 6.0)).unwrap_or(3.0);
    st.clarity.dynamics = Slot { on: true, amount: (amount * 2.0).round() / 2.0 };
    // 5. loudness: matched by the caller; peaks held under the ceiling
    st.loudness.limit = true;
    st.loudness.ceiling_dbtp = if ceiling_dbtp.is_finite() { ceiling_dbtp.clamp(-20.0, 0.0) } else { DEFAULT_CEILING_DBTP };
    st
}

/// Integrated loudness (LUFS; a mono clip as one channel) and true peak (dBTP) of `item`'s signal.
pub fn measure(item: &TrackItem, sr: u32, sources: &dyn SourceProvider, stop: &AtomicBool) -> Option<(f64, f64)> {
    let mono = filmcraft_render::audio::clip_is_mono(item, sources)?;
    let mut meter = LoudnessMeter::new(f64::from(sr.max(1)), if mono { 1 } else { 2 });
    let a0 = item.start.to_units_floor(i64::from(sr));
    let a1 = item.end().to_units_floor(i64::from(sr));
    let mut pos = a0;
    while pos < a1 {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let n = a1.saturating_sub(pos).min(i64::from(sr)) as usize;
        let [l, r] = filmcraft_render::audio::clip_signal(item, pos, n, sr, sources)?;
        if mono {
            let m: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
            meter.process(&[&m]);
        } else {
            meter.process(&[&l, &r]);
        }
        pos += n as i64;
    }
    let (l, tp) = (meter.integrated(), meter.true_peak_dbtp());
    (l.is_finite()).then_some((l, if tp.is_finite() { tp } else { -200.0 }))
}

/// One clip's clean-up: the settings, the analysis and the measured result.
struct Plan {
    clip: ClipId,
    settings: EssentialSound,
    report: Value,
}

#[allow(clippy::too_many_arguments)]
fn plan_clip(
    it: &TrackItem,
    mt: filmcraft_time::Tick,
    sr: u32,
    sources: &dyn SourceProvider,
    isolate: bool,
    target: f64,
    ceiling: f64,
    stop: &AtomicBool,
) -> std::result::Result<Plan, &'static str> {
    let stopped = |why: &'static str| if stop.load(Ordering::Relaxed) { "stopped" } else { why };
    // the clip as it sounds without any Essential Sound processing
    let mut raw = it.clone();
    let old = raw.essential.take();
    es::apply(&mut raw, old.as_ref(), mt);
    let mut a = analyse(&raw, sr, sources, stop).ok_or_else(|| stopped("silent or shorter than half a second"))?;
    if isolate {
        a.model_snr_db = model_snr(&raw, mt, sr, sources, stop);
    }
    let mut st = suggest(&a, isolate, old.as_ref(), ceiling);
    let mut gain = 0.0f64;
    // the limiter can't sit lower than its floor: past this gain the peaks would no longer be held
    let max_gain = st.loudness.ceiling_dbtp - es::LIMITER_FLOOR_DB - 0.5;
    let mut result = None;
    for pass in 0..PASSES {
        st.loudness.gain_db = gain;
        let mut cand = raw.clone();
        cand.essential = Some(st.clone());
        es::apply(&mut cand, None, mt);
        let Some((l, tp)) = measure(&cand, sr, sources, stop) else { break };
        result = Some((l, tp));
        let err = target - l;
        if err.abs() <= LOUDNESS_TOLERANCE || pass + 1 == PASSES {
            break;
        }
        let next = (gain + err).clamp(-60.0, max_gain);
        if (next - gain).abs() < 1e-9 {
            break;
        }
        gain = next;
    }
    if stop.load(Ordering::Relaxed) {
        return Err("stopped");
    }
    let (l, tp) = result.ok_or("silent after processing")?;
    st.loudness.measured_lufs = Some(l - gain);
    st.loudness.target_lufs = Some(target);
    let report = json!({
        "clip": it.id.0,
        "analysis": a.to_value(),
        "voiceIsolation": st.repair.isolate.on,
        "settings": st.to_value(),
        "lufs": l,
        "truePeakDbtp": tp,
        "gainDb": gain,
        "targetLufs": target,
        "ceilingDbtp": st.loudness.ceiling_dbtp,
        "meetsTarget": (l - target).abs() <= 0.5 && tp <= st.loudness.ceiling_dbtp + 1e-6,
    });
    Ok(Plan { clip: it.id, settings: st, report })
}

fn finite_or(v: Option<f64>, def: f64) -> f64 {
    v.filter(|v| v.is_finite()).unwrap_or(def)
}

/// A clean-up running as a background job: its plans are applied by [`poll`] when it finishes.
pub struct Pending {
    pub job: u64,
    seq: ItemId,
    plans: Arc<Mutex<Option<Vec<Plan>>>>,
}

const CMD: &str = "audio.cleanupVoice";
pub(crate) const LABEL: &str = "Clean Up Voice";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Write `plans` into their clips as one undo step.
fn apply_plans(s: &mut Session, plans: &[Plan]) -> Result<usize> {
    let ids: Vec<ClipId> = plans.iter().map(|pl| pl.clip).collect();
    update(s, CMD, LABEL, None, &ids, |it, e| {
        if let Some(pl) = plans.iter().find(|pl| pl.clip == it.id) {
            *e = Some(pl.settings.clone());
        }
        Ok(())
    })
}

fn cleanup_voice(s: &mut Session, p: &Value) -> Result<Value> {
    let clips = targets(s, p);
    if clips.is_empty() {
        return Err(bad(CMD, "select one or more voice clips"));
    }
    let target = finite_or(f64_p(p, "target"), DEFAULT_TARGET_LUFS).clamp(-40.0, -5.0);
    let ceiling = finite_or(f64_p(p, "ceiling"), DEFAULT_CEILING_DBTP).clamp(-20.0, 0.0);
    let isolate = bool_p(p, "isolate").unwrap_or_else(crate::voice_isolation::ready);
    let apply = bool_p(p, "apply").unwrap_or(true);
    // suggestions are read by whoever asked: they always wait
    let wait = !apply || bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32");
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    if s.cleanup_jobs.iter().any(|j| j.seq == seq_id) {
        return Err(EngineError::Other("Clean Up Voice is already running on this sequence".into()));
    }
    let ph = s.playhead();
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let sr = seq.settings.sample_rate.max(1);
    let work: Vec<(TrackItem, filmcraft_time::Tick)> =
        seq.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| clips.contains(&i.id)).map(|it| (it.clone(), mt_of(it, ph))).collect();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let n = work.len();
    let job =
        crate::Job { id, label: format!("{LABEL} ({n} clip{})", if n == 1 { "" } else { "s" }), progress: Default::default(), result: Default::default() };
    job.progress.total.store(n as u64, Ordering::Relaxed);
    let plans: Arc<Mutex<Option<Vec<Plan>>>> = Arc::default();
    let report: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (prog, res, out, rep) = (job.progress.clone(), job.result.clone(), plans.clone(), report.clone());
    let run = move || {
        let t0 = web_time::Instant::now();
        let mut done = Vec::new();
        for (k, (it, mt)) in work.iter().enumerate() {
            *lock(&prog.status) = format!("{}: clip {} of {n}", it.name, k + 1);
            match plan_clip(it, *mt, sr, &provider, isolate, target, ceiling, &prog.cancel) {
                Ok(pl) => {
                    lock(&rep).push(pl.report.clone());
                    done.push(pl);
                }
                Err(why) => lock(&rep).push(json!({"clip": it.id.0, "skipped": why})),
            }
            prog.done.store(k as u64 + 1, Ordering::Relaxed);
            if prog.cancel.load(Ordering::Relaxed) {
                break;
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        let r = if prog.cancel.load(Ordering::Relaxed) {
            *lock(&prog.status) = "Stopped: nothing was changed".into();
            Err("stopped".to_string())
        } else if done.is_empty() {
            let e = "no audible audio in the selected clips".to_string();
            *lock(&prog.status) = e.clone();
            Err(e)
        } else {
            *lock(&prog.status) = format!("Cleaned up {} clip(s) ({secs:.1}s)", done.len());
            let frames = done.len() as u64;
            *lock(&out) = Some(done);
            Ok(filmcraft_export::Report { path: String::new(), frames, seconds: secs, bytes: 0, render_fps: 0.0, extra_files: Vec::new() })
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    let result = job.result.clone();
    s.jobs.push(job);
    if !wait {
        // spawn first: a failed spawn must not leave the sequence "already running" forever
        if let Err(e) = std::thread::Builder::new().name("filmcraft-cleanup-voice".into()).spawn(run) {
            s.jobs.retain(|j| j.id != id);
            return Err(EngineError::Other(e.to_string()));
        }
        s.cleanup_jobs.push(Pending { job: id, seq: seq_id, plans });
        return Ok(json!({"job": id, "clips": n}));
    }
    run();
    s.jobs.retain(|j| j.id != id);
    if let Some(Err(e)) = lock(&result).clone() {
        return Err(bad(CMD, e));
    }
    let done = lock(&plans).take().unwrap_or_default();
    if apply {
        apply_plans(s, &done)?;
    }
    let clips = lock(&report).clone();
    Ok(json!({"applied": apply, "clips": clips}))
}

/// Apply finished clean-ups (one undo step each) and drop finished / stopped ones. Called once per
/// UI frame from [`Session::poll_persistence`].
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while let Some(pj) = s.cleanup_jobs.get(i) {
        let job = s.jobs.iter().find(|j| j.id == pj.job);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.cleanup_jobs.remove(i);
        let Some(plans) = lock(&pj.plans).take().filter(|_| !cancelled) else { continue };
        if s.state.active_sequence != Some(pj.seq) {
            s.error_toast(CMD, "Clean Up Voice: the sequence changed while it ran; nothing was changed".to_string());
            continue;
        }
        match apply_plans(s, &plans) {
            Ok(_) => s.toast(format!("Clean Up Voice: set {} clip(s)", plans.len())),
            Err(e) => s.error_toast(CMD, format!("Clean Up Voice: {e}")),
        }
    }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "audio.cleanupVoice",
        label: "Clean Up Voice",
        menu: &[],
        shortcut: None,
        params: r#"{"clips":[id]?,"target":lufs?,"ceiling":dbtp?,"isolate":bool?,"apply":bool?,"wait":bool?}"#,
        enabled: has_seq,
        run: cleanup_voice,
        journal: true,
    }]
}
