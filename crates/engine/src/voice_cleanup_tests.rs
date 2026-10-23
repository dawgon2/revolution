//! `audio.cleanupVoice`: analysis → visible Essential Sound settings, loudness to the delivery
//! target under the true-peak ceiling, suggest-only mode, one undo step, hostile input.

use std::sync::Arc;

use filmcraft_audio_dsp::LoudnessMeter;
use filmcraft_project::{AudioType, ClipId, TrackItem};
use serde_json::{Value, json};

use super::*;

const SR: u32 = 48_000;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// Speech-like: gliding f0 with harmonics up to 4 kHz under a syllable envelope, pauses every 2 s.
fn speech(secs: f64, amp: f32) -> Vec<f32> {
    let n = (secs * SR as f64) as usize;
    let mut phase = 0.0f64;
    (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            if t % 2.0 > 1.6 {
                return 0.0;
            }
            let f0 = 135.0 + 25.0 * (2.0 * std::f64::consts::PI * 0.7 * t).sin();
            phase += f0 / SR as f64;
            let mut v = 0.0;
            let mut k = 1;
            while k as f64 * f0 < 4000.0 {
                v += (2.0 * std::f64::consts::PI * phase * k as f64).sin() / k as f64;
                k += 1;
            }
            let syl = 0.6 - 0.4 * (2.0 * std::f64::consts::PI * 4.5 * t).cos();
            (v * syl * 0.5) as f32 * amp
        })
        .collect()
}

/// Steady fan-like noise (one-pole low-passed white noise) plus a 40 Hz rumble.
fn fan_and_rumble(secs: f64, amp: f32, rumble: f32) -> Vec<f32> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut y = 0.0f32;
    (0..(secs * SR as f64) as usize)
        .map(|i| {
            y = 0.8 * y + 0.2 * rng.next();
            amp * y * 2.5 + rumble * (2.0 * std::f64::consts::PI * 40.0 * i as f64 / SR as f64).sin() as f32
        })
        .collect()
}

/// Sibilant bursts: 7 kHz band noise in the first 80 ms of every syllable.
fn esses(secs: f64, amp: f32) -> Vec<f32> {
    let mut rng = Rng(7);
    let (mut a, mut b) = (0.0f32, 0.0f32);
    (0..(secs * SR as f64) as usize)
        .map(|i| {
            let t = i as f64 / SR as f64;
            // crude band-pass around 7 kHz: a high-passed then resonated noise
            let x = rng.next();
            let hp = x - a;
            a = x;
            b = 0.3 * b - 0.6 * hp;
            if t % 2.0 <= 1.6 && (t * 4.5) % 1.0 < 0.18 { amp * (hp + b) } else { 0.0 }
        })
        .collect()
}

fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

fn session_with(x: &[f32]) -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "vc", "audio": 2, "video": 1})).unwrap();
    let inter: Vec<f32> = x.iter().flat_map(|v| [*v, *v]).collect();
    let bytes: Arc<[u8]> = crate::previews::write_wav_f32(&inter, SR).into();
    let item = crate::commands::import_bytes(&mut s, "/voice.wav", bytes, None).unwrap();
    let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    s.execute("edit.deselectAll", json!({})).unwrap();
    (s, ClipId(r["clips"][0].as_u64().unwrap()))
}

fn item(s: &Session, c: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(c).unwrap().1.clone()
}

fn es_ids(s: &Session, c: ClipId) -> Vec<String> {
    item(s, c).effects.iter().filter(|e| e.essential).map(|e| e.effect.clone()).collect()
}

/// Integrated loudness and true peak of the clip as it now renders.
fn loudness_of(s: &Session, c: ClipId) -> (f64, f64) {
    let it = item(s, c);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let a0 = it.start.to_units_floor(SR as i64);
    let a1 = it.end().to_units_floor(SR as i64);
    let mut m = LoudnessMeter::new(SR as f64, 2);
    let mut pos = a0;
    while pos < a1 {
        let n = 4800.min((a1 - pos) as usize);
        let b = filmcraft_render::audio::clip_signal(&it, pos, n, SR, &provider).unwrap();
        m.process(&[&b[0], &b[1]]);
        pos += n as i64;
    }
    (m.integrated(), m.true_peak_dbtp())
}

fn run(s: &mut Session, c: ClipId, extra: Value) -> Value {
    let mut p = json!({"clips": [c.0], "isolate": false, "wait": true});
    if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    s.execute("audio.cleanupVoice", p).unwrap()
}

#[test]
fn cleans_a_noisy_voice_to_the_delivery_target() {
    let sig = add(&speech(8.0, 0.08), &fan_and_rumble(8.0, 0.004, 0.02));
    let (mut s, c) = session_with(&sig);
    assert!(commands::find("audio.cleanupVoice").is_some());
    let r = run(&mut s, c, json!({}));
    let rep = &r["clips"][0];
    let it = item(&s, c);
    let st = it.essential.clone().unwrap();
    assert_eq!(st.kind, AudioType::Dialogue);
    assert!(st.repair.rumble.on, "{rep}");
    assert!(st.repair.noise.on && !st.repair.isolate.on, "{rep}");
    assert!(st.clarity.dynamics.on && st.loudness.limit, "{rep}");
    assert_eq!(es_ids(&s, c), ["highpass", "denoise", "dynamics", "hard_limiter"], "{rep}");
    // the gate's loudness half: ±0.5 LU of −14 LUFS, ≤ −1 dBTP, measured on the rendered clip
    let (l, tp) = loudness_of(&s, c);
    assert!((l + 14.0).abs() <= 0.5, "integrated {l:.2} LUFS; {rep}");
    assert!(tp <= -1.0, "true peak {tp:.2} dBTP; {rep}");
    assert_eq!(rep["meetsTarget"], json!(true), "{rep}");
    // one undo step back to the untouched clip
    s.execute("edit.undo", json!({})).unwrap();
    let back = item(&s, c);
    assert!(back.essential.is_none() && back.gain_db == 0.0 && es_ids(&s, c).is_empty());
}

#[test]
fn clean_speech_gets_no_noise_reduction_and_esses_get_deessed() {
    // a quiet room: the pauses sit ~70 dB under the voice
    let clean = add(&speech(6.0, 0.2), &fan_and_rumble(6.0, 0.0001, 0.0));
    let (mut s, c) = session_with(&clean);
    let r = run(&mut s, c, json!({}));
    let st = item(&s, c).essential.unwrap();
    assert!(!st.repair.noise.on && !st.repair.isolate.on, "{r}");
    assert!(!st.repair.deess.on, "{r}");
    let (mut s, c) = session_with(&add(&clean, &esses(6.0, 0.15)));
    let r = run(&mut s, c, json!({}));
    assert!(item(&s, c).essential.unwrap().repair.deess.on, "{r}");
}

#[test]
fn voice_isolation_replaces_reduce_noise_when_asked() {
    let sig = add(&speech(3.0, 0.08), &fan_and_rumble(3.0, 0.004, 0.0));
    let (mut s, c) = session_with(&sig);
    run(&mut s, c, json!({"isolate": true}));
    let ids = es_ids(&s, c);
    assert!(ids.contains(&"voice_isolate".to_string()) && !ids.contains(&"denoise".to_string()), "{ids:?}");
}

#[test]
fn suggest_only_reports_and_changes_nothing() {
    let sig = add(&speech(4.0, 0.08), &fan_and_rumble(4.0, 0.004, 0.02));
    let (mut s, c) = session_with(&sig);
    let before = item(&s, c);
    let r = run(&mut s, c, json!({"apply": false}));
    assert_eq!(r["applied"], json!(false));
    assert_eq!(r["clips"][0]["settings"]["repair"]["rumble"]["on"], json!(true), "{r}");
    assert_eq!(item(&s, c), before);
}

#[test]
fn a_loud_target_is_held_under_the_ceiling() {
    let (mut s, c) = session_with(&speech(5.0, 0.3));
    // −6 LUFS from a quiet voice needs more limiting than the limiter allows: the peaks win and the
    // report says the target wasn't met
    let r = run(&mut s, c, json!({"target": -6.0, "ceiling": -2.0}));
    let (_, tp) = loudness_of(&s, c);
    assert!(tp <= -2.0, "true peak {tp:.2} dBTP; {r}");
    assert!(r["clips"][0]["gainDb"].as_f64().unwrap() <= 21.5 + 1e-9, "{r}");
}

/// Poll the session until no clean-up job is pending (at most 60 s).
fn wait_jobs(s: &mut Session) {
    let t0 = std::time::Instant::now();
    while !s.cleanup_jobs.is_empty() {
        assert!(t0.elapsed().as_secs() < 60, "clean-up job never finished");
        std::thread::sleep(std::time::Duration::from_millis(10));
        s.poll_persistence();
    }
}

#[test]
fn runs_as_a_background_job_applied_in_one_undo_step() {
    let sig = add(&speech(4.0, 0.08), &fan_and_rumble(4.0, 0.004, 0.02));
    let (mut s, c) = session_with(&sig);
    let undo0 = s.execute("history.list", json!({})).unwrap()["undo"].as_array().unwrap().len();
    let r = s.execute("audio.cleanupVoice", json!({"clips": [c.0], "isolate": false})).unwrap();
    assert!(r["job"].as_u64().is_some(), "{r}");
    // one at a time per sequence
    // (it stays pending until a poll applies it, so a second run is refused until then)
    assert!(s.execute("audio.cleanupVoice", json!({"clips": [c.0], "isolate": false})).is_err());
    wait_jobs(&mut s);
    let st = item(&s, c).essential.expect("applied when the job finished");
    assert!(st.repair.rumble.on && st.loudness.limit);
    assert_eq!(s.execute("history.list", json!({})).unwrap()["undo"].as_array().unwrap().len(), undo0 + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(item(&s, c).essential.is_none());
}

#[test]
fn a_stopped_job_changes_nothing() {
    let sig = add(&speech(6.0, 0.08), &fan_and_rumble(6.0, 0.004, 0.02));
    let (mut s, c) = session_with(&sig);
    let before = item(&s, c);
    let r = s.execute("audio.cleanupVoice", json!({"clips": [c.0], "isolate": false})).unwrap();
    let id = r["job"].as_u64().unwrap();
    s.execute("jobs.cancel", json!({"job": id})).unwrap();
    wait_jobs(&mut s);
    assert_eq!(item(&s, c), before);
}

#[test]
fn hostile_input_is_refused_or_clamped_never_a_crash() {
    // silence, no clips, nothing selected
    let (mut s, c) = session_with(&vec![0.0; SR as usize * 2]);
    assert!(s.execute("audio.cleanupVoice", json!({"clips": [c.0], "wait": true})).is_err());
    assert!(s.execute("audio.cleanupVoice", json!({})).is_err());
    assert!(s.execute("audio.cleanupVoice", json!({"clips": [999_999]})).is_err());
    // a 0.2 s blip is too short to analyse
    let (mut s, c) = session_with(&speech(0.2, 0.2));
    assert!(s.execute("audio.cleanupVoice", json!({"clips": [c.0], "wait": true})).is_err());
    // absurd targets and ceilings clamp
    let (mut s, c) = session_with(&speech(3.0, 0.1));
    let r = run(&mut s, c, json!({"target": 1e300, "ceiling": -1e300}));
    assert_eq!(r["clips"][0]["targetLufs"], json!(-5.0));
    assert_eq!(r["clips"][0]["ceilingDbtp"], json!(-20.0));
    let r = run(&mut s, c, json!({"target": "loud", "ceiling": null}));
    assert_eq!(r["clips"][0]["targetLufs"], json!(-14.0));
    // NaN / Inf samples in the source
    let mut x = speech(3.0, 0.1);
    for v in x.iter_mut().step_by(997) {
        *v = f32::NAN;
    }
    if let Some(v) = x.get_mut(5) {
        *v = f32::INFINITY;
    }
    let (mut s, c) = session_with(&x);
    let _ = s.execute("audio.cleanupVoice", json!({"clips": [c.0], "isolate": false, "wait": true}));
}

/// The one-click gate (fork AUDIO-PLAN A2 item 2, A2.8b): on recorded speech mixed with each test
/// noise at 0 / 5 / 10 dB SNR, Clean Up Voice against the written manual process of
/// `docs/guide/audio.md` ("Cleaning up a voice by hand"), applied by someone who knows what they
/// hear: the noise kind picks Reduce Noise (steady) or Voice Isolation (changing), and of the
/// process's suggested amounts (and DeEss off / on) the one with the best SI-SDR stands in for
/// tuning by ear. Both chains end with Auto-Match to −14 LUFS and Limit True Peak at −1 dBTP.
/// SI-SDR scores the Repair stage against the clean speech through the same non-noise Repair
/// settings (see `repair_score`); loudness and true peak are of the whole chain.
///
/// Pass (decided before the first measurement, PREP-A2 §1): per case, one-click SI-SDR ≥ manual
/// − 0.5 dB, loudness within ±0.5 LU of −14 LUFS, true peak ≤ −1 dBTP. A noise kind that fails at
/// any SNR ships only as "Suggest settings" (`apply: false`).
///
/// `FILMCRAFT_EVAL_SPEECH=<dir of 48 kHz mono WAVs> FILMCRAFT_DFN3_CKPT=<ckpt> cargo test --release
/// -p filmcraft-engine --features voice-isolation cleanup_gate -- --ignored --nocapture`
/// (`FILMCRAFT_EVAL_OUT=<file>` writes the table; `FILMCRAFT_GATE_SECONDS` sets the speech length,
/// default 45).
#[cfg(feature = "voice-isolation")]
#[test]
#[ignore]
fn cleanup_gate() {
    use filmcraft_voice::testset::{Noise, mix_at_snr, noise, rms, si_sdr};
    let (Some(dir), Some(ckpt)) = (std::env::var_os("FILMCRAFT_EVAL_SPEECH"), std::env::var_os("FILMCRAFT_DFN3_CKPT")) else {
        eprintln!("FILMCRAFT_EVAL_SPEECH / FILMCRAFT_DFN3_CKPT unset: skipped");
        return;
    };
    assert!(filmcraft_voice::effect::set_model_path(std::path::Path::new(&ckpt)), "model at {ckpt:?}");
    let secs: f64 = std::env::var("FILMCRAFT_GATE_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(45.0);
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
    files.sort();
    let mut clean = Vec::new();
    for f in files {
        if clean.len() as f64 >= secs * SR as f64 {
            break;
        }
        if let Some(x) = crate::voice_isolation_tests::read_wav_48k_mono(&f) {
            clean.extend(x);
            clean.extend(std::iter::repeat_n(0.0, 24_000));
        }
    }
    assert!(clean.len() > SR as usize * 10, "need at least 10 s of speech");
    let k = 0.1 / rms(&clean);
    clean.iter_mut().for_each(|v| *v = (*v as f64 * k) as f32);
    let n = clean.len();

    // the clip's rendered output (channel 0) and its loudness / true peak
    let output = |s: &Session, c: ClipId| -> (Vec<f32>, f64, f64) {
        let it = item(s, c);
        let provider = s.media.provider(s.project.clone(), s.services.clone());
        let a0 = it.start.to_units_floor(SR as i64);
        let a1 = it.end().to_units_floor(SR as i64);
        let mut m = LoudnessMeter::new(SR as f64, 2);
        let mut out = Vec::with_capacity(n);
        let mut pos = a0;
        while pos < a1 {
            let len = 48_000.min((a1 - pos) as usize);
            let b = filmcraft_render::audio::clip_signal(&it, pos, len, SR, &provider).unwrap();
            m.process(&[&b[0], &b[1]]);
            out.extend_from_slice(&b[0]);
            pos += len as i64;
        }
        out.truncate(n);
        (out, m.integrated(), m.true_peak_dbtp())
    };
    // SI-SDR of the Repair stage (Clarity, Creative and Loudness off) against the clean speech through
    // the same Repair settings without noise reduction: what the noise reduction leaves (residual
    // noise + artefacts), not the intended tone / level changes of high-pass, DeEss, Dynamics
    let repair_score = |s: &mut Session, c: ClipId| -> f64 {
        let st = item(s, c).essential.unwrap();
        let off = json!({"clarity.enabled": false, "creative.enabled": false, "loudness.enabled": false});
        s.execute("essentialSound.set", json!({"clips": [c.0], "values": off})).unwrap();
        let (out, _, _) = output(s, c);
        let (mut r, rc) = session_with(&clean);
        r.execute("essentialSound.setType", json!({"clips": [rc.0], "type": "dialogue"})).unwrap();
        let rp = &st.repair;
        let mut values = off.as_object().unwrap().clone();
        for (k, v) in [
            ("repair.rumble.on", json!(rp.rumble.on)),
            ("repair.rumble.amount", json!(rp.rumble.amount)),
            ("repair.deess.on", json!(rp.deess.on)),
            ("repair.deess.amount", json!(rp.deess.amount)),
            ("repair.dehum.on", json!(rp.dehum.on)),
            ("repair.reverb.on", json!(rp.reverb.on)),
        ] {
            values.insert(k.into(), v);
        }
        r.execute("essentialSound.set", json!({"clips": [rc.0], "values": values})).unwrap();
        let (reference, _, _) = output(&r, rc);
        si_sdr(&out, &reference)
    };
    let mut table = format!(
        "Speech: {:.0} s of recorded speech.\n\n| noise | SNR | noisy | manual (settings) | one-click (settings) | Δ SI-SDR | LUFS manual / one-click | dBTP manual / one-click | pass |\n|---|---|---|---|---|---|---|---|---|\n",
        n as f64 / SR as f64
    );
    let mut failed: Vec<String> = Vec::new();
    let describe = |v: &Value| -> String {
        let r = &v["repair"];
        let mut parts = Vec::new();
        for (key, name) in [("isolate", "VI"), ("noise", "NR"), ("rumble", "HP"), ("deess", "DS")] {
            if r[key]["on"] == json!(true) {
                parts.push(format!("{name} {}", r[key]["amount"].as_f64().unwrap_or(0.0)));
            }
        }
        if v["clarity"]["dynamics"]["on"] == json!(true) {
            parts.push(format!("Dyn {}", v["clarity"]["dynamics"]["amount"].as_f64().unwrap_or(0.0)));
        }
        parts.join(", ")
    };
    for kind in Noise::ALL {
        let nz = noise(kind, n, SR, 11);
        for snr in [0.0, 5.0, 10.0] {
            let noisy = mix_at_snr(&clean, &nz, snr);
            let base = si_sdr(&noisy, &clean);
            // manual: the process's choices for what the person hears
            let heavy_low = matches!(kind, Noise::Traffic | Noise::Wind);
            let amounts: &[f64] = if kind.steady() { if snr < 5.0 { &[5.0, 7.0] } else { &[3.0, 5.0] } } else { &[7.0, 10.0] };
            let slot = if kind.steady() { "repair.noise" } else { "repair.isolate" };
            let mut best: Option<(f64, f64, f64, String)> = None;
            for &amt in amounts {
                for deess in [false, true] {
                    let (mut s, c) = session_with(&noisy);
                    s.execute("essentialSound.setType", json!({"clips": [c.0], "type": "dialogue"})).unwrap();
                    let mut values = serde_json::Map::new();
                    values.insert("repair.rumble.on".into(), json!(true));
                    values.insert("repair.rumble.amount".into(), json!(if heavy_low { 4.0 } else { 3.0 }));
                    values.insert(format!("{slot}.on"), json!(true));
                    values.insert(format!("{slot}.amount"), json!(amt));
                    values.insert("repair.deess.on".into(), json!(deess));
                    values.insert("repair.deess.amount".into(), json!(4.0));
                    values.insert("clarity.dynamics.on".into(), json!(true));
                    values.insert("clarity.dynamics.amount".into(), json!(3.0));
                    values.insert("loudness.limit".into(), json!(true));
                    s.execute("essentialSound.set", json!({"clips": [c.0], "values": values})).unwrap();
                    s.execute("essentialSound.autoMatch", json!({"clips": [c.0], "target": -14.0})).unwrap();
                    let (_, l, tp) = output(&s, c);
                    let settings = describe(&item(&s, c).essential.unwrap().to_value());
                    let score = repair_score(&mut s, c);
                    if best.as_ref().is_none_or(|b| score > b.0) {
                        best = Some((score, l, tp, settings));
                    }
                }
            }
            let (manual, ml, mtp, mset) = best.unwrap();
            // one click
            let (mut s, c) = session_with(&noisy);
            // (the model is installed for this test: one-click uses Voice Isolation, as in the app)
            let r = s.execute("audio.cleanupVoice", json!({"clips": [c.0], "isolate": true, "wait": true})).unwrap();
            let (_, l, tp) = output(&s, c);
            let one = repair_score(&mut s, c);
            let oset = describe(&r["clips"][0]["settings"]);
            let pass = one >= manual - 0.5 && (l + 14.0).abs() <= 0.5 && tp <= -1.0;
            if !pass {
                failed.push(format!("{} at {snr} dB", kind.name()));
            }
            table.push_str(&format!(
                "| {} | {snr:.0} dB | {base:.1} | {manual:.1} ({mset}) | {one:.1} ({oset}) | {:+.1} | {ml:.1} / {l:.1} | {mtp:.1} / {tp:.1} | {} |\n",
                kind.name(),
                one - manual,
                if pass { "yes" } else { "**no**" }
            ));
        }
    }
    eprintln!("{table}");
    if let Some(out) = std::env::var_os("FILMCRAFT_EVAL_OUT") {
        std::fs::write(out, &table).unwrap();
    }
    assert!(failed.is_empty(), "one-click loses to the manual process: {failed:?}");
}

#[test]
fn voice_isolation_limit_follows_the_noise_level() {
    use crate::voice_cleanup::{Analysis, suggest};
    let at =
        |snr: f64, model: Option<f64>| Analysis { noise_db: -20.0 - snr, speech_db: -20.0, low_db: -30.0, sibilance_db: -20.0, lra: None, model_snr_db: model };
    let limit = |a: &Analysis| suggest(a, true, None, -1.0).repair.isolate_limit_db;
    assert_eq!(limit(&at(8.0, None)), 100.0, "heavy noise: no limit");
    assert_eq!(limit(&at(20.0, None)), 100.0, "medium noise: no limit");
    assert_eq!(limit(&at(30.0, None)), 12.0, "mild noise");
    // fork A3.7 (FORK-PLAN §1a.125 (b)): the limit follows the level smoothly (shared with the
    // rendered models in `filmcraft_voice::gate`), lightest just before Voice Isolation turns off
    let l26 = limit(&at(26.0, None));
    assert!(l26 > 12.0 && l26 < 18.0, "{l26}");
    assert_eq!(limit(&at(38.0, None)), 6.0, "nearly clean: lightest");
    assert!(!suggest(&at(40.0, None), true, None, -1.0).repair.isolate.on, "clean from 40 dB");
    // clean: Voice Isolation stays off and the limit at its default
    let clean = suggest(&at(50.0, None), true, None, -1.0);
    assert!(!clean.repair.isolate.on);
    assert_eq!(clean.repair.isolate_limit_db, 100.0);
    // the model's own measurement wins; a NaN one is ignored
    assert_eq!(limit(&at(30.0, Some(8.0))), 100.0);
    assert_eq!(limit(&at(30.0, Some(f64::NAN))), 12.0);
    // the chosen limit reaches the effect
    let st = suggest(&at(30.0, None), true, None, -1.0);
    let fx = st.effects();
    let vi = fx.iter().find(|e| e.effect == "voice_isolate").expect("voice isolation on");
    assert_eq!(vi.f64_at("limit", filmcraft_time::Tick::ZERO), 12.0);
}
