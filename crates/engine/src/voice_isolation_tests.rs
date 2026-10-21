//! Voice isolation model commands (fork AUDIO-PLAN A2.5): status in any build, the commands that
//! need the model code refusing cleanly without it, and hostile files refused before anything is
//! written. (Nothing here downloads, installs or removes a model: those touch the real data
//! directory. They are checked on the Dell with `FILMCRAFT_DATA_DIR` set, see HANDOFF.)

use serde_json::json;

use crate::Session;

#[test]
fn status_reports_the_build_and_the_package() {
    let mut s = Session::default();
    let st = s.execute("audio.voiceIsolate.status", json!({})).unwrap();
    assert_eq!(st["available"], cfg!(feature = "voice-isolation"));
    assert_eq!(st["license"], "MIT OR Apache-2.0");
    assert_eq!(st["size"], 7_986_207);
    assert_eq!(st["sampleRates"], json!([48_000]));
    assert!(st["download"].is_null());
    let ids: Vec<&str> = crate::commands::command_specs().iter().map(|c| c.id).collect();
    for id in ["status", "download", "cancelDownload", "installFile", "remove"] {
        assert!(ids.contains(&format!("audio.voiceIsolate.{id}").as_str()), "{id}");
    }
}

#[test]
fn cancelling_with_no_download_is_harmless() {
    let mut s = Session::default();
    assert_eq!(s.execute("audio.voiceIsolate.cancelDownload", json!({})).unwrap()["cancelled"], false);
}

#[cfg(not(feature = "voice-isolation"))]
#[test]
fn downloads_and_installs_need_the_feature() {
    let mut s = Session::default();
    for (cmd, p) in [("audio.voiceIsolate.download", json!({"wait": true})), ("audio.voiceIsolate.installFile", json!({"path": "/nonexistent.zip"}))] {
        let e = s.execute(cmd, p).unwrap_err().to_string();
        assert!(e.contains("voice-isolation"), "{cmd}: {e}");
    }
}

#[cfg(feature = "voice-isolation")]
#[test]
fn hostile_files_are_refused_before_anything_is_written() {
    let mut s = Session::default();
    let dir = std::env::temp_dir().join(format!("fc-engine-voice-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let zip = dir.join("DeepFilterNet3.zip");
    std::fs::write(&zip, b"PK\x03\x04 not the release").unwrap();
    let ckpt = dir.join("model.ckpt");
    std::fs::write(&ckpt, b"not a checkpoint").unwrap();
    let before = s.execute("audio.voiceIsolate.status", json!({})).unwrap()["installed"].clone();
    let e = s.execute("audio.voiceIsolate.installFile", json!({"path": zip})).unwrap_err().to_string();
    assert!(e.contains("not the DeepFilterNet3 release"), "{e}");
    assert!(s.execute("audio.voiceIsolate.installFile", json!({"path": ckpt})).is_err());
    assert!(s.execute("audio.voiceIsolate.installFile", json!({"path": dir.join("absent.ckpt")})).is_err());
    assert!(s.execute("audio.voiceIsolate.installFile", json!({})).is_err());
    // a huge "release" is refused by its size, before it is read
    let big = dir.join("big").join("DeepFilterNet3.zip");
    std::fs::create_dir_all(big.parent().unwrap()).unwrap();
    std::fs::File::create(&big).unwrap().set_len(8 << 30).unwrap();
    let t0 = std::time::Instant::now();
    let e = s.execute("audio.voiceIsolate.installFile", json!({"path": big})).unwrap_err().to_string();
    assert!(e.contains("not the DeepFilterNet3 release") && e.contains("bytes"), "{e}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(2), "not hashed");
    assert_eq!(s.execute("audio.voiceIsolate.status", json!({})).unwrap()["installed"], before);
    std::fs::remove_dir_all(&dir).ok();
}

/// 48 kHz mono speech from a simple WAV file (16-bit PCM or 32-bit float), `None` otherwise.
#[cfg(feature = "voice-isolation")]
pub(crate) fn read_wav_48k_mono(path: &std::path::Path) -> Option<Vec<f32>> {
    let b = std::fs::read(path).ok()?;
    let u16_at = |i: usize| b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |i: usize| b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    if b.get(0..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut i, mut fmt) = (12usize, None);
    while i + 8 <= b.len() {
        let len = u32_at(i + 4)? as usize;
        let body = i + 8;
        match b.get(i..i + 4)? {
            b"fmt " => fmt = Some((u16_at(body)?, u16_at(body + 2)?, u32_at(body + 4)?, u16_at(body + 14)?)),
            b"data" => {
                let (tag, ch, sr, bits) = fmt?;
                if ch != 1 || sr != 48_000 {
                    return None;
                }
                let data = b.get(body..(body + len).min(b.len()))?;
                return match (tag, bits) {
                    (1, 16) => Some(data.as_chunks::<2>().0.iter().map(|s| f32::from(i16::from_le_bytes(*s)) / 32768.0).collect()),
                    (3, 32) => Some(data.as_chunks::<4>().0.iter().map(|s| f32::from_le_bytes(*s)).collect()),
                    _ => None,
                };
            }
            _ => {}
        }
        i = body + len + (len & 1);
    }
    None
}

/// The eval's speech at −20 dBFS RMS: the recordings in `FILMCRAFT_EVAL_SPEECH` (and that folder),
/// else the built-in voices.
#[cfg(feature = "voice-isolation")]
fn eval_speech() -> (Vec<f32>, Option<std::path::PathBuf>) {
    use filmcraft_voice::testset::{rms, upsample2};
    let mut clean = Vec::new();
    let real = std::env::var_os("FILMCRAFT_EVAL_SPEECH").map(std::path::PathBuf::from);
    if let Some(dir) = &real {
        let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
        files.sort();
        for f in files {
            let Some(x) = read_wav_48k_mono(&f) else {
                eprintln!("{}: not 48 kHz mono 16-bit / float WAV, skipped", f.display());
                continue;
            };
            clean.extend(x);
            clean.extend(std::iter::repeat_n(0.0, 24_000));
        }
        assert!(clean.len() > 48_000 * 10, "need at least 10 s of speech in {}", dir.display());
    } else {
        let lines = [
            "The quick brown fox jumps over the lazy dog, and then it runs away into the forest.",
            "Please remember to save your project before you close the application tonight.",
            "We recorded this voice over in a small room with the window open, so there is some noise.",
        ];
        for v in ["basic-female", "basic-male"] {
            let voice = filmcraft_tts::voice(v).unwrap();
            for l in lines {
                let a = voice.synthesize(l, &filmcraft_tts::Params::default()).unwrap();
                assert_eq!(a.sample_rate, 24_000);
                clean.extend(upsample2(&a.samples));
                clean.extend(std::iter::repeat_n(0.0, 24_000));
            }
        }
    }
    // −20 dBFS RMS.
    let k = 0.1 / rms(&clean);
    clean.iter_mut().for_each(|v| *v = (*v as f64 * k) as f32);
    (clean, real)
}

/// Voice isolation vs DeNoise on the A2 test set (fork AUDIO-PLAN A2.7): speech mixed with five
/// generated noises at 0 / 5 / 10 dB SNR, scored by SI-SDR against the clean speech. DeNoise is
/// scored at Amount 40 % (its default) and 100 %, and its better result counts.
///
/// Speech: the 48 kHz mono WAV files in `FILMCRAFT_EVAL_SPEECH` (real recordings: the only valid
/// input for acceptance), else the built-in formant voices (24 kHz, upsampled), which
/// DeepFilterNet doesn't recognise as speech: those numbers are reported but not asserted.
/// `FILMCRAFT_DFN3_CKPT=… cargo test --release -p filmcraft-engine --features voice-isolation voice_isolation_eval -- --ignored --nocapture`
/// (`FILMCRAFT_EVAL_OUT=<file>` also writes the table as Markdown).
///
/// Acceptance (decided before the first measurement, PREP-A2 §1): a clear margin is a mean SI-SDR
/// at least 3 dB above DeNoise's over traffic, keyboard and wind at every SNR; clean speech is
/// unharmed when Voice Isolation on noise-free speech scores at least 20 dB SI-SDR.
#[cfg(feature = "voice-isolation")]
#[test]
#[ignore]
fn voice_isolation_eval() {
    use filmcraft_audio_dsp::AudioEffect;
    use filmcraft_voice::testset::{Noise, mix_at_snr, noise, rms, si_sdr};
    let Some(ckpt) = std::env::var_os("FILMCRAFT_DFN3_CKPT") else {
        eprintln!("FILMCRAFT_DFN3_CKPT unset: skipped");
        return;
    };
    let w = filmcraft_voice::weights::Weights::load(std::path::Path::new(&ckpt)).unwrap();
    let (clean, real) = eval_speech();
    let n = clean.len();

    fn run(fx: &mut dyn AudioEffect, x: &[f32]) -> Vec<f32> {
        let lat = fx.latency();
        let mut buf: Vec<f32> = x.iter().copied().chain(std::iter::repeat_n(0.0, lat)).collect();
        for block in buf.chunks_mut(512) {
            fx.process(&mut [block]);
        }
        buf[lat..lat + x.len()].to_vec()
    }
    let isolate = |x: &[f32]| {
        let mut fx = filmcraft_voice::effect::VoiceIsolate::new(&w, 1).unwrap();
        run(&mut fx, x)
    };
    let denoise = |x: &[f32], amount: f32| {
        let mut fx = filmcraft_audio_dsp::create_effect("denoise", 48_000.0, 1).unwrap();
        fx.set_param("reduction", amount * 0.4);
        fx.reset();
        run(fx.as_mut(), x)
    };

    let vi_clean = isolate(&clean);
    let clean_vi = si_sdr(&vi_clean, &clean);
    let source = if real.is_some() { "recorded speech" } else { "built-in formant voices (synthetic: not valid for acceptance)" };
    let mut table = format!(
        "Speech: {source}, {:.0} s.\n\n| noise | SNR | noisy | DeNoise (best of 40 / 100 %) | Voice Isolation | margin |\n|---|---|---|---|---|---|\n",
        n as f64 / 48_000.0
    );
    let mut margins: Vec<(Noise, f64, f64)> = Vec::new();
    for kind in Noise::ALL {
        let nz = noise(kind, n, 48_000, 11);
        for snr in [0.0, 5.0, 10.0] {
            let noisy = mix_at_snr(&clean, &nz, snr);
            let base = si_sdr(&noisy, &clean);
            let dn = si_sdr(&denoise(&noisy, 40.0), &clean).max(si_sdr(&denoise(&noisy, 100.0), &clean));
            let vi = si_sdr(&isolate(&noisy), &clean);
            table.push_str(&format!("| {} | {snr:.0} dB | {base:.1} | {dn:.1} | {vi:.1} | {:+.1} |\n", kind.name(), vi - dn));
            margins.push((kind, snr, vi - dn));
        }
    }
    table.push_str(&format!(
        "\nClean speech through Voice Isolation: SI-SDR {clean_vi:.1} dB (≥ 20 required); output level {:+.1} dB.\n",
        20.0 * (rms(&vi_clean) / rms(&clean)).log10()
    ));
    eprintln!("{table}");
    if let Some(out) = std::env::var_os("FILMCRAFT_EVAL_OUT") {
        std::fs::write(out, &table).unwrap();
    }
    if real.is_none() {
        eprintln!("synthetic speech: no assertions (set FILMCRAFT_EVAL_SPEECH to a folder of recorded speech)");
        return;
    }
    for snr in [0.0, 5.0, 10.0] {
        let m: Vec<f64> = margins.iter().filter(|(k, s, _)| !k.steady() && *s == snr).map(|(_, _, m)| *m).collect();
        let mean = m.iter().sum::<f64>() / m.len() as f64;
        assert!(mean >= 3.0, "non-steady noise at {snr} dB: mean margin {mean:.1} dB < 3 dB");
    }
    assert!(clean_vi >= 20.0, "clean speech SI-SDR {clean_vi:.1} dB < 20 dB");
}

/// Fork AUDIO-PLAN A3.6: Voice Isolation at several attenuation limits on the eval's speech, to
/// pick Clean Up Voice's limits by measurement: mean SI-SDR over the five noises at 0 / 5 / 10 dB
/// SNR and on clean speech, for each limit in `FILMCRAFT_EVAL_LIMITS` (dB, comma-separated;
/// default 100,36,30,24,18,12). Reports only (no assertions).
/// `FILMCRAFT_DFN3_CKPT=… FILMCRAFT_EVAL_SPEECH=… cargo test --release -p filmcraft-engine --features voice-isolation voice_isolation_limit_sweep -- --ignored --nocapture`
#[cfg(feature = "voice-isolation")]
#[test]
#[ignore]
fn voice_isolation_limit_sweep() {
    use filmcraft_audio_dsp::AudioEffect;
    use filmcraft_voice::testset::{Noise, mix_at_snr, noise, si_sdr};
    let Some(ckpt) = std::env::var_os("FILMCRAFT_DFN3_CKPT") else {
        eprintln!("FILMCRAFT_DFN3_CKPT unset: skipped");
        return;
    };
    let w = filmcraft_voice::weights::Weights::load(std::path::Path::new(&ckpt)).unwrap();
    let limits: Vec<f32> =
        std::env::var("FILMCRAFT_EVAL_LIMITS").unwrap_or_else(|_| "100,36,30,24,18,12".into()).split(',').filter_map(|v| v.trim().parse().ok()).collect();
    let (clean, real) = eval_speech();
    let n = clean.len();
    let isolate = |x: &[f32], limit: f32| {
        let mut fx = filmcraft_voice::effect::VoiceIsolate::new(&w, 1).unwrap();
        fx.set_param("limit", limit);
        fx.reset();
        let lat = fx.latency();
        let mut buf: Vec<f32> = x.iter().copied().chain(std::iter::repeat_n(0.0, lat)).collect();
        for block in buf.chunks_mut(512) {
            fx.process(&mut [block]);
        }
        buf[lat..lat + x.len()].to_vec()
    };
    let noisy: Vec<(f64, Vec<f32>)> = [0.0, 5.0, 10.0]
        .into_iter()
        .flat_map(|snr| Noise::ALL.into_iter().map(move |k| (snr, k)))
        .map(|(snr, k)| (snr, mix_at_snr(&clean, &noise(k, n, 48_000, 11), snr)))
        .collect();
    let source = if real.is_some() { "recorded speech" } else { "built-in formant voices (synthetic)" };
    let mut table = format!(
        "Speech: {source}, {:.0} s. Mean SI-SDR (dB) over five noises.\n\n| limit | 0 dB | 5 dB | 10 dB | clean |\n|---|---|---|---|---|\n",
        n as f64 / 48_000.0
    );
    for &limit in &limits {
        let mean = |snr: f64| {
            let v: Vec<f64> = noisy.iter().filter(|(s, _)| *s == snr).map(|(_, x)| si_sdr(&isolate(x, limit), &clean)).collect();
            v.iter().sum::<f64>() / v.len().max(1) as f64
        };
        let c = si_sdr(&isolate(&clean, limit), &clean);
        table.push_str(&format!("| {limit:.0} dB | {:.1} | {:.1} | {:.1} | {c:.1} |\n", mean(0.0), mean(5.0), mean(10.0)));
    }
    eprintln!("{table}");
    if let Some(out) = std::env::var_os("FILMCRAFT_EVAL_OUT") {
        std::fs::write(out, &table).unwrap();
    }
}
