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
fn better_model_is_reported_and_rendering_refuses_cleanly() {
    let mut s = Session::default();
    let st = s.execute("audio.voiceIsolate.status", json!({})).unwrap();
    assert_eq!(st["better"]["license"], "Apache-2.0");
    assert_eq!(st["better"]["live"], false);
    assert_eq!(st["renderDefault"], crate::voice_isolation_render::DEFAULT_MODEL);
    // bad model names are refused before anything else
    s.execute("file.newSequence", json!({})).unwrap();
    let e = s.execute("audio.voiceIsolate.render", json!({"model": "best"})).unwrap_err().to_string();
    assert!(e.contains("unknown model"), "{e}");
    let e = s.execute("audio.voiceIsolate.remove", json!({"model": "best"})).unwrap_err().to_string();
    assert!(e.contains("unknown model"), "{e}");
    // (installFile checks the build first: without the feature it refuses before the model name)
    let e = s.execute("audio.voiceIsolate.installFile", json!({"path": "/nonexistent.pt", "model": "best"})).unwrap_err().to_string();
    let want = if cfg!(feature = "voice-isolation") { "unknown model" } else { "voice-isolation" };
    assert!(e.contains(want), "{e}");
    // nothing selected
    let e = s.execute("audio.voiceIsolate.render", json!({"model": "better", "clips": []})).unwrap_err().to_string();
    assert!(e.contains("select one or more audio clips"), "{e}");
}

#[cfg(feature = "voice-isolation")]
#[test]
fn a_corrupt_better_model_is_refused_before_install() {
    let mut s = Session::default();
    let dir = std::env::temp_dir().join(format!("fc-engine-moss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let junk = dir.join("last_best_checkpoint.pt");
    std::fs::write(&junk, b"PK\x03\x04 not a checkpoint").unwrap();
    let before = s.execute("audio.voiceIsolate.status", json!({})).unwrap()["better"]["installed"].clone();
    let e = s.execute("audio.voiceIsolate.installFile", json!({"path": junk, "model": "better"})).unwrap_err().to_string();
    assert!(e.contains("not a readable MossFormer2_SE_48K checkpoint"), "{e}");
    assert_eq!(s.execute("audio.voiceIsolate.status", json!({})).unwrap()["better"]["installed"], before);
    std::fs::remove_dir_all(&dir).ok();
}

/// The real MossFormer2 checkpoint, when present (`FILMCRAFT_MOSSFORMER2_CKPT`, else the fork's
/// `models/` folder): a rendered "Better" isolation writes the file, switches the clip to it at the
/// same source time, cleans the noise, and one Undo brings the original back.
#[cfg(feature = "voice-isolation")]
#[test]
fn rendering_with_the_better_model_switches_the_clip() {
    use filmcraft_voice::testset::{Noise, mix_at_snr, noise, rms};
    let ckpt = std::env::var_os("FILMCRAFT_MOSSFORMER2_CKPT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "~/Projects/filmcraft-main/models/mossformer2-se-48k/last_best_checkpoint.pt".into());
    if !ckpt.is_file() {
        eprintln!("MossFormer2 checkpoint absent: skipped");
        return;
    }
    filmcraft_voice::mossformer::set_model_path(&ckpt);
    let (clean, _) = eval_speech();
    let clean = &clean[..clean.len().min(6 * 48_000)];
    let noisy = mix_at_snr(clean, &noise(Noise::Traffic, clean.len(), 48_000, 3), 0.0);
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "vi", "audio": 2, "video": 1})).unwrap();
    let inter: Vec<f32> = noisy.iter().flat_map(|v| [*v, *v]).collect();
    let bytes: std::sync::Arc<[u8]> = crate::previews::write_wav_f32(&inter, 48_000).into();
    let item = crate::commands::import_bytes(&mut s, "/noisy.wav", bytes, None).unwrap();
    let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let clip = r["clips"][0].as_u64().unwrap();
    let dir = std::env::temp_dir().join(format!("fc-engine-vi-render-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // unsaved: refused in plain words
    let e = s.execute("audio.voiceIsolate.render", json!({"clips": [clip], "model": "better", "wait": true})).unwrap_err().to_string();
    assert!(e.contains("save the project first"), "{e}");
    s.path = Some(dir.join("p.fcproj").to_string_lossy().into_owned());
    let undo0 = s.history.undo.len();
    let r = s.execute("audio.voiceIsolate.render", json!({"clips": [clip], "model": "better", "wait": true})).unwrap();
    assert_eq!(r["model"], "better", "{r}");
    let a = &r["applied"][0];
    assert_eq!(a["replaced"], true, "{r}");
    let path = a["path"].as_str().unwrap();
    assert!(path.ends_with("Voice Isolated.wav") && std::path::Path::new(path).is_file(), "{path}");
    let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.clone();
    assert_ne!(it.item, item);
    assert_eq!(it.source_in, filmcraft_time::Tick::ZERO);
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    // the rendered file: same length, and the noise is gone from the pauses
    let out = read_wav_16_stereo_left(std::path::Path::new(path)).unwrap();
    assert!(out.len().abs_diff(noisy.len()) <= 1, "{} vs {}", out.len(), noisy.len());
    let si = filmcraft_voice::testset::si_sdr(&out[..clean.len().min(out.len())], &clean[..clean.len().min(out.len())]);
    let before = filmcraft_voice::testset::si_sdr(&noisy, clean);
    eprintln!("SI-SDR {before:.1} → {si:.1} dB; level {:.3} → {:.3}", rms(&noisy), rms(&out));
    assert!(si > before + 3.0, "{before:.1} → {si:.1}");
    s.execute("edit.undo", json!({})).unwrap();
    let back = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.clone();
    assert_eq!(back.item, item);
    std::fs::remove_dir_all(&dir).ok();
}

/// Two clips of the same name: stopping after the first changes nothing and leaves no file; a full
/// run gives them distinct files and switches both in one undo step.
#[cfg(feature = "voice-isolation")]
#[test]
fn a_stopped_render_changes_nothing_and_a_finished_one_is_one_undo_step() {
    let ckpt = std::env::var_os("FILMCRAFT_MOSSFORMER2_CKPT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "~/Projects/filmcraft-main/models/mossformer2-se-48k/last_best_checkpoint.pt".into());
    if !ckpt.is_file() {
        eprintln!("MossFormer2 checkpoint absent: skipped");
        return;
    }
    filmcraft_voice::mossformer::set_model_path(&ckpt);
    let x: Vec<f32> = (0..96_000).map(|i| ((i as f32) * 0.05).sin() * 0.1).collect();
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "vi2", "audio": 2, "video": 1})).unwrap();
    let inter: Vec<f32> = x.iter().flat_map(|v| [*v, *v]).collect();
    let bytes: std::sync::Arc<[u8]> = crate::previews::write_wav_f32(&inter, 48_000).into();
    let item = crate::commands::import_bytes(&mut s, "/take.wav", bytes, None).unwrap();
    let mut clips = Vec::new();
    for t in [0.0, 5.0] {
        let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": t})).unwrap();
        clips.push(r["clips"][0].as_u64().unwrap());
    }
    let dir = std::env::temp_dir().join(format!("fc-engine-vi-stop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    s.path = Some(dir.join("p.fcproj").to_string_lossy().into_owned());
    let wavs = || std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "wav")).count();
    let items = |s: &Session| -> Vec<filmcraft_project::ItemId> {
        clips.iter().map(|c| s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(*c)).unwrap().1.item).collect()
    };
    let undo0 = s.history.undo.len();
    crate::voice_isolation_render::CANCEL_AFTER.with(|c| c.set(Some(1)));
    let e = s.execute("audio.voiceIsolate.render", json!({"clips": clips, "model": "better", "wait": true})).unwrap_err().to_string();
    crate::voice_isolation_render::CANCEL_AFTER.with(|c| c.set(None));
    assert!(e.contains("nothing was changed"), "{e}");
    assert_eq!(items(&s), [item, item]);
    assert_eq!(s.history.undo.len(), undo0);
    assert_eq!(wavs(), 0, "the stopped job's file was deleted");
    // a full run: distinct names, both switched, one undo step
    let r = s.execute("audio.voiceIsolate.render", json!({"clips": clips, "model": "better", "wait": true})).unwrap();
    let paths: Vec<&str> = r["applied"].as_array().unwrap().iter().map(|a| a["path"].as_str().unwrap()).collect();
    assert_eq!(paths.len(), 2, "{r}");
    assert_ne!(paths[0], paths[1]);
    assert!(items(&s).iter().all(|i| *i != item));
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step for the job");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(items(&s), [item, item]);
    assert_eq!(wavs(), 2, "undo leaves the files on disk");
    std::fs::remove_dir_all(&dir).ok();
}

/// Noise-level gating's status line says, in plain words, what it did.
#[test]
fn rendered_gating_is_reported_plainly() {
    use crate::voice_isolation_render::gate_note;
    assert_eq!(gate_note(Some(8.4), 100.0), "speech 8 dB over the noise, full voice isolation");
    assert_eq!(gate_note(Some(45.0), 0.0), "speech 45 dB over the noise, already clean: left unchanged");
    assert_eq!(gate_note(Some(30.0), 12.0), "speech 30 dB over the noise, light voice isolation (at most 12 dB taken away)");
    assert_eq!(gate_note(None, 100.0), "noise level not measurable, full voice isolation");
    assert_eq!(gate_note(Some(f64::NAN), f32::NAN), "noise level not measurable, full voice isolation");
}

#[test]
fn rendered_file_names_are_safe() {
    use crate::voice_isolation_render::safe_stem;
    assert_eq!(safe_stem("Interview A.mov"), "Interview A");
    assert_eq!(safe_stem("a/b\\c:d*e?f\"g<h>i|j\u{7}k.wav"), "abcdefghijk");
    assert_eq!(safe_stem("...."), "Clip");
    assert_eq!(safe_stem(""), "Clip");
    let long = "é".repeat(400);
    let s = safe_stem(&long);
    assert!(s.len() <= 200 && s.chars().all(|c| c == 'é'), "{}", s.len());
}

/// The left channel of a 16-bit stereo WAV as written by the render.
#[cfg(feature = "voice-isolation")]
fn read_wav_16_stereo_left(path: &std::path::Path) -> Option<Vec<f32>> {
    let b = std::fs::read(path).ok()?;
    let data = b.windows(4).position(|w| w == b"data")?;
    let body = b.get(data + 8..)?;
    Some(body.as_chunks::<4>().0.iter().map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0).collect())
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

/// Fork AUDIO-PLAN A3.7: the "Better" model (MossFormer2_SE_48K) against DeepFilterNet3 (when
/// `FILMCRAFT_DFN3_CKPT` is set) and DeNoise (best of Amount 40 / 100 %), each model also with
/// noise-level gating (`filmcraft_voice::gate`, FORK-PLAN §1a.125 (b): the model's output mixed
/// with the input by the clip's measured noise level, as Clean Up Voice and the render do).
///
/// Noisy mixes (`FILMCRAFT_EVAL_SPEECH`, LibriSpeech on the Dell; skipped with
/// `FILMCRAFT_EVAL_SKIP_NOISY=1`): mean SI-SDR over the five noises at 0 / 5 / 10 dB SNR and the
/// mean margin over DeNoise on the non-steady noises (bar ≥ 3 dB at 10 dB SNR), plus that speech
/// run through as recorded (reported, but not the clean bar: LibriSpeech's recordings carry their
/// own hiss and room tone, which a model rightly removes).
///
/// Clean speech (`FILMCRAFT_EVAL_CLEAN`: a folder of truly clean 48 kHz mono WAV, the
/// VoiceBank-DEMAND clean test set; `FILMCRAFT_EVAL_CLEAN_N` = use only the first n files): each
/// utterance on its own, as its own clip, scored against itself (no noise was added, so the output
/// should match the input): SI-SDR (bar ≥ 20 dB on the mean), the share that matches the input
/// (SI-SDR ≥ 20 dB and level within ±1 dB), the worst utterance and the level change.
///
/// Also seconds of processing per second of mono audio. Reports only (no assertions).
/// `FILMCRAFT_MOSSFORMER2_CKPT=…/last_best_checkpoint.pt FILMCRAFT_DFN3_CKPT=… FILMCRAFT_EVAL_SPEECH=… FILMCRAFT_EVAL_CLEAN=…/clean_testset_wav cargo test --release -p filmcraft-engine --features voice-isolation voice_isolation_better_eval -- --ignored --nocapture`
#[cfg(feature = "voice-isolation")]
#[test]
#[ignore]
fn voice_isolation_better_eval() {
    use filmcraft_audio_dsp::AudioEffect;
    use filmcraft_voice::gate;
    use filmcraft_voice::testset::{Noise, mix_at_snr, noise, rms, si_sdr};
    let Some(moss) = std::env::var_os("FILMCRAFT_MOSSFORMER2_CKPT") else {
        eprintln!("FILMCRAFT_MOSSFORMER2_CKPT unset: skipped");
        return;
    };
    let t0 = std::time::Instant::now();
    let moss = filmcraft_voice::mossformer::MossFormer2::load(std::path::Path::new(&moss)).unwrap();
    eprintln!("MossFormer2 loaded in {:.1} s", t0.elapsed().as_secs_f64());
    let dfn = std::env::var_os("FILMCRAFT_DFN3_CKPT").map(|p| filmcraft_voice::weights::Weights::load(std::path::Path::new(&p)).unwrap());
    fn run(fx: &mut dyn AudioEffect, x: &[f32]) -> Vec<f32> {
        let lat = fx.latency();
        let mut buf: Vec<f32> = x.iter().copied().chain(std::iter::repeat_n(0.0, lat)).collect();
        for block in buf.chunks_mut(512) {
            fx.process(&mut [block]);
        }
        buf[lat..lat + x.len()].to_vec()
    }
    let gated = |x: &[f32], y: &[f32]| -> (Vec<f32>, f32) {
        let mut out = vec![y.to_vec()];
        let (_, limit) = gate::gate(&[x.to_vec()], &mut out, 48_000);
        (out.remove(0), limit)
    };
    // seconds of audio, seconds of DFN3, seconds of MossFormer2
    let (mut audio_s, mut dfn_s, mut moss_s) = (0.0f64, 0.0f64, 0.0f64);
    // (DFN3 full, DFN3 gated + its limit, MossFormer2 full, MossFormer2 gated + its limit)
    #[allow(clippy::type_complexity)]
    let mut process = |x: &[f32]| -> (Option<(Vec<f32>, (Vec<f32>, f32))>, Vec<f32>, (Vec<f32>, f32)) {
        audio_s += x.len() as f64 / 48_000.0;
        let t = std::time::Instant::now();
        let df = dfn.as_ref().map(|w| {
            let mut fx = filmcraft_voice::effect::VoiceIsolate::new(w, 1).unwrap();
            let y = run(&mut fx, x);
            let g = gated(x, &y);
            (y, g)
        });
        dfn_s += t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let y = moss.enhance(&[x.to_vec()], &mut |_| true).unwrap().remove(0);
        moss_s += t.elapsed().as_secs_f64();
        let g = gated(x, &y);
        (df, y, g)
    };
    let fmt = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{v:.1}"));
    let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let mut table = String::new();

    if std::env::var_os("FILMCRAFT_EVAL_SKIP_NOISY").is_none() {
        let (clean, real) = eval_speech();
        let n = clean.len();
        let source = if real.is_some() { "recorded speech" } else { "built-in formant voices (synthetic: not valid for acceptance)" };
        table.push_str(&format!(
            "## Noisy mixes\n\nSpeech: {source} (`FILMCRAFT_EVAL_SPEECH`), {:.0} s. SI-SDR (dB); \"gated\" = with noise-level gating (limit chosen in brackets, 100 = full model).\n\n| noise | SNR | noisy | DeNoise | DFN3 | DFN3 gated | MossFormer2 | MossFormer2 gated |\n|---|---|---|---|---|---|---|---|\n",
            n as f64 / 48_000.0
        ));
        let denoise = |x: &[f32]| -> f64 {
            [40.0f32, 100.0]
                .map(|amount| {
                    let mut fx = filmcraft_audio_dsp::create_effect("denoise", 48_000.0, 1).unwrap();
                    fx.set_param("reduction", amount * 0.4);
                    fx.reset();
                    si_sdr(&run(fx.as_mut(), x), &clean)
                })
                .into_iter()
                .fold(f64::MIN, f64::max)
        };
        // (snr, steady, DeNoise, DFN3, DFN3 gated, MossFormer2, MossFormer2 gated)
        let mut rows: Vec<(f64, bool, f64, Option<f64>, Option<f64>, f64, f64)> = Vec::new();
        for kind in Noise::ALL {
            let nz = noise(kind, n, 48_000, 11);
            for snr in [0.0, 5.0, 10.0] {
                let noisy = mix_at_snr(&clean, &nz, snr);
                let base = si_sdr(&noisy, &clean);
                let dn = denoise(&noisy);
                let (df, mf, (mg, ml)) = process(&noisy);
                let dff = df.as_ref().map(|(y, _)| si_sdr(y, &clean));
                let dfg = df.as_ref().map(|(_, (g, _))| si_sdr(g, &clean));
                let dfl = df.as_ref().map(|(_, (_, l))| f64::from(*l));
                let (mf, mg) = (si_sdr(&mf, &clean), si_sdr(&mg, &clean));
                let line = format!(
                    "| {} | {snr:.0} dB | {base:.1} | {dn:.1} | {} | {} ({}) | {mf:.1} | {mg:.1} ({ml:.0}) |\n",
                    kind.name(),
                    fmt(dff),
                    fmt(dfg),
                    dfl.map_or("–".into(), |l| format!("{l:.0}"))
                );
                eprint!("{line}");
                table.push_str(&line);
                rows.push((snr, kind.steady(), dn, dff, dfg, mf, mg));
            }
        }
        table.push_str(
            "\n| SNR | mean DeNoise | mean DFN3 | mean DFN3 gated | mean MossFormer2 | mean MossFormer2 gated | non-steady margin DFN3 | DFN3 gated | MossFormer2 | MossFormer2 gated |\n|---|---|---|---|---|---|---|---|---|---|\n",
        );
        for snr in [0.0, 5.0, 10.0] {
            let r: Vec<_> = rows.iter().filter(|r| r.0 == snr).collect();
            let ns: Vec<_> = r.iter().filter(|r| !r.1).collect();
            let opt_mean = |f: &dyn Fn(&(f64, bool, f64, Option<f64>, Option<f64>, f64, f64)) -> Option<f64>,
                            v: &[&&(f64, bool, f64, Option<f64>, Option<f64>, f64, f64)]| {
                v.iter().map(|r| f(r)).collect::<Option<Vec<f64>>>().map(|v| avg(&v))
            };
            let all: Vec<_> = r.iter().collect();
            table.push_str(&format!(
                "| {snr:.0} dB | {:.1} | {} | {} | {:.1} | {:.1} | {} | {} | {:.1} | {:.1} |\n",
                avg(&r.iter().map(|r| r.2).collect::<Vec<_>>()),
                fmt(opt_mean(&|r| r.3, &all)),
                fmt(opt_mean(&|r| r.4, &all)),
                avg(&r.iter().map(|r| r.5).collect::<Vec<_>>()),
                avg(&r.iter().map(|r| r.6).collect::<Vec<_>>()),
                fmt(opt_mean(&|r| r.3.map(|d| d - r.2), &ns)),
                fmt(opt_mean(&|r| r.4.map(|d| d - r.2), &ns)),
                avg(&ns.iter().map(|r| r.5 - r.2).collect::<Vec<_>>()),
                avg(&ns.iter().map(|r| r.6 - r.2).collect::<Vec<_>>()),
            ));
        }
        let (df, mf, (mg, ml)) = process(&clean);
        table.push_str(&format!(
            "\nThis speech as recorded (no noise added; LibriSpeech as-recorded has its own noise, so this is not the clean-speech bar): DFN3 {}, DFN3 gated {} (limit {}), MossFormer2 {:.1}, MossFormer2 gated {:.1} (limit {ml:.0}) dB SI-SDR.\n",
            fmt(df.as_ref().map(|(y, _)| si_sdr(y, &clean))),
            fmt(df.as_ref().map(|(_, (g, _))| si_sdr(g, &clean))),
            df.as_ref().map_or("–".into(), |(_, (_, l))| format!("{l:.0}")),
            si_sdr(&mf, &clean),
            si_sdr(&mg, &clean),
        ));
    }

    if let Some(dir) = std::env::var_os("FILMCRAFT_EVAL_CLEAN").map(std::path::PathBuf::from) {
        let limit_n: usize = std::env::var("FILMCRAFT_EVAL_CLEAN_N").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
        files.sort();
        files.truncate(limit_n);
        /// Per variant: per-utterance SI-SDR, level change (dB), limit, and pooled sums (dot, rr, ee).
        #[derive(Default)]
        struct Stat {
            sdr: Vec<f64>,
            level: Vec<f64>,
            limits: Vec<f64>,
            dot: f64,
            rr: f64,
            ee: f64,
        }
        impl Stat {
            fn add(&mut self, y: &[f32], x: &[f32], limit: Option<f32>) {
                self.sdr.push(si_sdr(y, x).min(CAP));
                self.level.push(20.0 * (rms(y).max(1e-12) / rms(x).max(1e-12)).log10());
                if let Some(l) = limit {
                    self.limits.push(f64::from(l));
                }
                let mx = x.iter().map(|v| f64::from(*v)).sum::<f64>() / x.len().max(1) as f64;
                let my = y.iter().map(|v| f64::from(*v)).sum::<f64>() / y.len().max(1) as f64;
                for (a, b) in y.iter().zip(x) {
                    let (a, b) = (f64::from(*a) - my, f64::from(*b) - mx);
                    self.dot += a * b;
                    self.rr += b * b;
                    self.ee += a * a;
                }
            }
            fn pooled(&self) -> f64 {
                let a = self.dot / self.rr.max(1e-30);
                let target = a * a * self.rr;
                let resid = (self.ee - 2.0 * a * self.dot + a * a * self.rr).max(0.0);
                (10.0 * (target / resid.max(1e-30)).log10()).min(CAP)
            }
        }
        /// Per-utterance SI-SDR is capped here (an untouched utterance scores infinity).
        const CAP: f64 = 60.0;
        const MATCH_SDR: f64 = 20.0;
        const MATCH_LEVEL: f64 = 1.0;
        let names = ["DFN3", "DFN3 gated", "MossFormer2", "MossFormer2 gated"];
        let mut stats: [Stat; 4] = Default::default();
        let (mut used, mut secs) = (0usize, 0.0f64);
        for f in &files {
            let Some(x) = read_wav_48k_mono(f) else {
                eprintln!("{}: not 48 kHz mono 16-bit / float WAV, skipped", f.display());
                continue;
            };
            if x.len() < 24_000 || rms(&x) <= 0.0 {
                continue;
            }
            used += 1;
            secs += x.len() as f64 / 48_000.0;
            let (df, mf, (mg, ml)) = process(&x);
            if let Some((y, (g, l))) = &df {
                stats[0].add(y, &x, None);
                stats[1].add(g, &x, Some(*l));
            }
            stats[2].add(&mf, &x, None);
            stats[3].add(&mg, &x, Some(ml));
            if used.is_multiple_of(50) {
                eprintln!("clean: {used} / {} utterances", files.len());
            }
        }
        table.push_str(&format!(
            "\n## Clean speech (no noise added)\n\n{used} utterances, {secs:.0} s, from {} (each its own clip). SI-SDR of the output against the input (dB, per utterance capped at {CAP:.0}); \"matches\" = SI-SDR ≥ {MATCH_SDR:.0} dB and level within ±{MATCH_LEVEL:.0} dB. Bar: mean ≥ 20 dB.\n\n| model | mean SI-SDR | median | worst | pooled | matches input | mean level change | worst level change | bypassed / mean limit |\n|---|---|---|---|---|---|---|---|---|\n",
            dir.display()
        ));
        for (name, st) in names.iter().zip(&stats) {
            if st.sdr.is_empty() {
                table.push_str(&format!("| {name} | – | – | – | – | – | – | – | – |\n"));
                continue;
            }
            let mut sorted = st.sdr.clone();
            sorted.sort_by(f64::total_cmp);
            let matches = st.sdr.iter().zip(&st.level).filter(|(s, l)| **s >= MATCH_SDR && l.abs() <= MATCH_LEVEL).count();
            let worst_level = st.level.iter().copied().fold(0.0f64, |a, b| if b.abs() > a.abs() { b } else { a });
            let gate_col = if st.limits.is_empty() {
                "–".to_string()
            } else {
                let by = st.limits.iter().filter(|l| **l <= f64::from(gate::BYPASS)).count();
                format!("{:.0} % / {:.0} dB", 100.0 * by as f64 / st.limits.len() as f64, avg(&st.limits))
            };
            table.push_str(&format!(
                "| {name} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} % ({matches} / {}) | {:+.2} dB | {worst_level:+.1} dB | {gate_col} |\n",
                avg(&st.sdr),
                sorted.get(sorted.len() / 2).copied().unwrap_or(f64::NAN),
                sorted.first().copied().unwrap_or(f64::NAN),
                st.pooled(),
                100.0 * matches as f64 / st.sdr.len() as f64,
                st.sdr.len(),
                avg(&st.level),
            ));
        }
    }

    let per = |t: f64| t / audio_s.max(1e-9);
    table.push_str(&format!(
        "\nProcessing time per second of mono audio ({}, {} threads): MossFormer2 {:.3} s, DeepFilterNet3 {}.\n",
        std::env::consts::ARCH,
        std::thread::available_parallelism().map_or(1, |n| n.get()),
        per(moss_s),
        if dfn.is_some() { format!("{:.3} s", per(dfn_s)) } else { "–".into() },
    ));
    eprintln!("{table}");
    if let Some(out) = std::env::var_os("FILMCRAFT_EVAL_OUT") {
        std::fs::write(out, &table).unwrap();
    }
}
