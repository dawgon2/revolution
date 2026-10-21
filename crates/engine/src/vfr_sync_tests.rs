//! Y2 acceptance: a 20-minute variable-frame-rate clip stays in sync with its sound, in playback
//! (program render + sequence mix) and in export. The clip (ffmpeg-made, MP4 and MKV) flashes
//! white and clicks at every 10 s; its frames average ~29.85 fps while its typical gap says 30
//! (one doubled gap every 200 frames, ±1 ms jitter), so a frame index × 1/30 would be ~6 s late
//! by the end. Synthetic.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

use crate::media_test_util::session_with;

const SECONDS: u32 = 1205;

fn fixture(ext: &str) -> Option<PathBuf> {
    let ff = filmcraft_testkit::oracle::ffmpeg_or_skip("vfr sync")?;
    let out = filmcraft_testkit::fixtures_dir("engine/vfr").join(format!("sync20.{ext}"));
    let vf = "settb=1/90000,setpts='((N+floor(N/200))/30+(mod(N,3)-1)*0.001)/TB',\
              drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='lt(mod(t+0.002,10),0.1)'";
    let a = format!("aevalsrc='if(lt(mod(t,10),0.02),0.8*sin(2*PI*1000*t),0)':s=48000:d={SECONDS}");
    let v = format!("color=black:s=64x36:r=30:d={SECONDS}");
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let mut c = Command::new(&ff);
        c.args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &v, "-f", "lavfi", "-i", &a, "-vf", vf]);
        c.args(["-fps_mode", "passthrough", "-enc_time_base:v", "1/90000", "-c:v", "libx264", "-preset", "ultrafast", "-g", "60", "-pix_fmt", "yuv420p"]);
        c.args(["-c:a", "aac", "-shortest"]);
        if ext == "mp4" {
            c.args(["-video_track_timescale", "90000"]);
        }
        c.arg(tmp).stdin(Stdio::null()).status().is_ok_and(|s| s.success())
    })
}

fn secs(t: Tick) -> f64 {
    t.0 as f64 / TICKS_PER_SECOND as f64
}

/// First sample at or after `from` seconds whose level passes 0.1, as seconds (sequence mix).
fn click_after(s: &crate::Session, seq: filmcraft_project::ItemId, from: f64) -> Option<f64> {
    let q = s.project.sequence(seq)?;
    let sr = q.settings.sample_rate.max(1);
    let start = (from * sr as f64) as i64;
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let mix = filmcraft_render::audio::mix_sequence(&s.project, q, start, sr as usize, &provider);
    let n = mix.channels.first()?.len();
    (0..n).find(|&i| mix.channels.iter().any(|c| c.get(i).is_some_and(|v| v.abs() > 0.1))).map(|i| (start + i as i64) as f64 / sr as f64)
}

/// The first sequence frame at or after `from` seconds that the program shows lit, as seconds.
fn flash_after(s: &crate::Session, from: f64) -> Option<f64> {
    let rate = s.sequence_rate();
    let f0 = rate.frame_at(Tick((from * TICKS_PER_SECOND as f64) as i64));
    (f0..f0 + 15).map(|f| rate.tick_of(f)).find(|&t| s.render_program_at(1.0, t).is_some_and(|img| lit(&img.px))).map(secs)
}

fn lit(rgba: &[f32]) -> bool {
    let r: Vec<f32> = rgba.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
    !r.is_empty() && r.iter().sum::<f32>() / r.len() as f32 > 0.5
}

#[test]
fn a_twenty_minute_variable_frame_rate_clip_stays_in_sync_in_playback() {
    for ext in ["mp4", "mkv"] {
        let Some(path) = fixture(ext) else { return };
        let (s, items, seq) = session_with(&[path.as_path()]);
        assert!(s.project.item(items[0]).and_then(|i| i.as_media()).and_then(|m| m.vfr()).is_some(), "{ext}: detected as VFR");
        // its typical rate, not the jitter-skewed median sample duration (29.13)
        assert_eq!(s.sequence_rate(), filmcraft_time::FrameRate::FPS_30, "{ext}");
        let frame = secs(s.sequence_rate().frame_duration());
        for mark in [10.0, 600.0, 1190.0] {
            let a = click_after(&s, seq, mark - 0.5).unwrap_or_else(|| panic!("{ext}: no click near {mark}"));
            let v = flash_after(&s, mark - 0.2).unwrap_or_else(|| panic!("{ext}: no flash near {mark}"));
            assert!((a - mark).abs() < 0.005, "{ext}: click at {a} for {mark}");
            assert!((v - a).abs() <= frame + 0.002, "{ext} at {mark}: picture {v} vs sound {a} (frame {frame})");
        }
    }
}

/// ffmpeg's view of an exported file: (first lit frame, first click) after `from` seconds of it.
fn measure_export(ff: &Path, file: &Path, from: f64) -> (Option<f64>, Option<f64>) {
    let run =
        |args: &[&str]| Command::new(ff).args(["-v", "error", "-i"]).arg(file).args(args).stdin(Stdio::null()).output().map(|o| o.stdout).unwrap_or_default();
    let gray = run(&["-map", "0:v:0", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "gray", "-"]);
    // each picture's own time, in presentation order
    let probe = Command::new(ff.with_file_name("ffprobe"))
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let mut times: Vec<f64> = String::from_utf8_lossy(&probe).lines().filter_map(|l| l.trim().trim_end_matches(',').parse().ok()).collect();
    times.sort_by(f64::total_cmp);
    let px = 64 * 36;
    let v = gray.chunks_exact(px).zip(times).find(|(f, t)| *t >= from && f.iter().map(|&b| b as u32).sum::<u32>() / px as u32 > 128).map(|(_, t)| t);
    let pcm = run(&["-map", "0:a:0", "-ac", "1", "-ar", "48000", "-f", "f32le", "-"]);
    let a = pcm
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .enumerate()
        .find(|(i, x)| *i as f64 / 48000.0 >= from && x.abs() > 0.1)
        .map(|(i, _)| i as f64 / 48000.0);
    (v, a)
}

#[test]
fn a_twenty_minute_variable_frame_rate_clip_stays_in_sync_in_export() {
    let Some(ff) = filmcraft_testkit::oracle::ffmpeg_or_skip("vfr sync export") else { return };
    let Some(path) = fixture("mp4") else { return };
    let (s, _, seq) = session_with(&[path.as_path()]);
    let dir = crate::media_test_util::tmp_dir("vfr-sync-export");
    let out = dir.join("end.mov");
    // the last 15 s before the 20-minute mark: the click / flash at 1190 s is 10 s in
    let range = TimeRange::new(Tick(1180 * TICKS_PER_SECOND), Tick(15 * TICKS_PER_SECOND));
    let settings = filmcraft_export::ExportSettings {
        format: filmcraft_export::Format::ProRes,
        path: out.to_string_lossy().into_owned(),
        range: Some(range),
        ..Default::default()
    };
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    filmcraft_export::export(&Arc::new((*s.project).clone()), seq, &settings, &provider, &Default::default()).expect("export");
    let (v, a) = measure_export(&ff, &out, 9.5);
    let (v, a) = (v.expect("a flash in the export"), a.expect("a click in the export"));
    assert!((a - 10.0).abs() < 0.005, "click at {a}");
    assert!((v - a).abs() <= 1.0 / 30.0 + 0.002, "picture {v} vs sound {a}");
    let _ = std::fs::remove_dir_all(dir);
}

fn media(s: &crate::Session, it: filmcraft_project::ItemId) -> filmcraft_project::MediaClip {
    s.project.item(it).and_then(|i| i.as_media()).cloned().expect("a media item")
}

/// Interpret as CFR (Y2): frame k plays at k / 30 whatever its recorded time, so the clip is
/// shorter (it has fewer frames than 30 a second) and the picture drifts from its sound: the
/// flash recorded with the click at 1190 s shows about 6 s early. Undo, Interpret Footage's
/// `frameRate` and hostile values.
#[test]
fn interpret_as_cfr_plays_every_frame_evenly_and_lets_the_sound_drift() {
    use filmcraft_media::timing::FrameClock;
    let Some(path) = fixture("mp4") else { return };
    let (mut s, items, _) = session_with(&[path.as_path()]);
    let it = items[0];
    let before = media(&s, it).duration();
    let r = s.execute("clip.interpretAsCfr", serde_json::json!({"items": [it.0]})).unwrap();
    assert_eq!(r["clips"][0]["frameRate"], serde_json::json!([30, 1]), "{r}");
    let m = media(&s, it);
    let n = m.frame_count().expect("frames");
    assert_eq!(m.duration(), filmcraft_time::FrameRate::FPS_30.tick_of(n));
    assert!(secs(before) - secs(m.duration()) > 5.0, "{} → {}", secs(before), secs(m.duration()));
    assert!(m.vfr().is_none(), "treated as constant-rate now");
    // edits and trims use that length (review finding: they read the file's)
    let range = crate::clip_ops::item_range(&s.project, it, Tick::ZERO).expect("range");
    assert_eq!(range.duration, m.duration());
    assert_eq!(crate::media_duration(&s.project, &s.media, it), Some(m.duration()));

    let src = s.source(it).expect("source");
    assert_eq!(src.frame_clock(), FrameClock::Grid(filmcraft_time::FrameRate::FPS_30));
    assert_eq!(src.info().duration, m.duration());
    let lit_at = |t: Tick| {
        src.video_frame(filmcraft_media::FrameRequest::full(t)).is_ok_and(|f| lit(&f.to_rgba8().iter().map(|&b| f32::from(b) / 255.0).collect::<Vec<_>>()))
    };
    let rate = filmcraft_time::FrameRate::FPS_30;
    let first = (rate.frame_at(Tick(1182 * TICKS_PER_SECOND))..rate.frame_at(Tick(1189 * TICKS_PER_SECOND))).map(|k| rate.tick_of(k)).find(|&t| lit_at(t));
    let t = secs(first.expect("the 1190 s flash, early"));
    assert!(t > 1183.0 && t < 1186.0, "flash at {t}");

    s.execute("edit.undo", serde_json::json!({})).unwrap();
    assert_eq!(media(&s, it).duration(), before);
    assert!(media(&s, it).vfr().is_some());

    s.execute("clip.interpretFootage", serde_json::json!({"items": [it.0], "frameRate": [25, 1]})).unwrap();
    assert_eq!(media(&s, it).duration(), filmcraft_time::FrameRate::FPS_25.tick_of(n));
    let r = s.execute("clip.interpretFootage", serde_json::json!({"items": [it.0], "frameRate": "file"})).unwrap();
    assert_eq!(r["frameRate"], "file");
    assert_eq!(media(&s, it).duration(), before);
    for bad in [
        serde_json::json!(0),
        serde_json::json!("x"),
        serde_json::json!([1, 0]),
        serde_json::json!([-30, 1]),
        serde_json::json!(1e9),
        serde_json::json!("30/0"),
    ] {
        assert!(s.execute("clip.interpretFootage", serde_json::json!({"items": [it.0], "frameRate": bad})).is_err(), "{bad}");
    }
    assert_eq!(media(&s, it).duration(), before);
}
