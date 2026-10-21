//! Variable-frame-rate clips (Y2): phone-like timestamp jitter and low-light drops, MP4, MKV and MPEG-TS,
//! made by ffmpeg (external fixture generator only). Info must say VFR; constant-rate clips must not.

mod common;

use common::{bytes, ffmpeg_frames, fixture, max_diff, planes, raw_planes};

/// `setpts` expressions (time in seconds of frame N, for 30 frames a second of `testsrc2`).
const JITTER: &str = "(N/30+(mod(N,3)-1)*0.001)/TB";
const DROP: &str = "(if(lt(N,45),N,45+2*(N-45))/30)/TB";
const CFR: &str = "N/30/TB";

fn clip(ff: &std::path::Path, name: &str, pts: &str) -> std::path::PathBuf {
    let vf = format!("settb=1/90000,setpts='{pts}'");
    // fine filter and encoder time bases, or the 1/30 s default rounds the jitter into repeated times
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=128x72:rate=30:duration=4", "-vf", &vf, "-fps_mode", "passthrough", "-enc_time_base:v", "1/90000"];
    args.extend_from_slice(&["-c:v", "libx264", "-preset", "ultrafast", "-g", "15", "-pix_fmt", "yuv420p"]);
    if name.ends_with(".mp4") {
        args.extend_from_slice(&["-video_track_timescale", "90000"]);
    }
    fixture(ff, name, &args).expect("ffmpeg fixture")
}

fn vfr_of(path: &std::path::Path) -> Option<filmcraft_media::timing::FrameTiming> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let src = filmcraft_codecs::open_bytes(name, bytes(path)).expect("open");
    src.info().video.as_ref().expect("video").vfr
}

#[test]
fn variable_frame_rate_clips_say_so() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for ext in ["mp4", "mkv", "ts"] {
        let cfr = clip(&ff, &format!("vfr_cfr.{ext}"), CFR);
        assert_eq!(vfr_of(&cfr), None, "{ext}: a constant-rate clip isn't VFR");

        let jitter = clip(&ff, &format!("vfr_jitter.{ext}"), JITTER);
        let j = vfr_of(&jitter).unwrap_or_else(|| panic!("{ext}: ±1 ms jitter is VFR"));
        assert!((j.average_fps - 30.0).abs() < 0.2, "{ext}: {j:?}");
        assert!(j.min_fps < 30.0 && j.max_fps > 30.0, "{ext}: {j:?}");

        let drop = clip(&ff, &format!("vfr_drop.{ext}"), DROP);
        let d = vfr_of(&drop).unwrap_or_else(|| panic!("{ext}: a 30 → 15 fps drop is VFR"));
        // MKV keeps whole milliseconds: 33 / 67 ms gaps
        assert!((d.min_fps - 15.0).abs() < 0.5 && (d.max_fps - 30.0).abs() < 0.5, "{ext}: {d:?}");
        assert!(d.average_fps > 15.0 && d.average_fps < 30.0, "{ext}: {d:?}");
        assert!(d.label().starts_with("Variable, "), "{}", d.label());
    }
}

#[test]
fn a_variable_frame_rate_clip_decodes_each_frame_at_its_own_time() {
    use filmcraft_media::timing::{FrameClock, FrameTimes};
    let ff = filmcraft_testkit::require_ffmpeg!();
    for ext in ["mp4", "mkv", "ts"] {
        let cfr = clip(&ff, &format!("vfr_cfr.{ext}"), CFR);
        let src = filmcraft_codecs::open_bytes("c", bytes(&cfr)).expect("open");
        assert!(matches!(src.frame_clock(), FrameClock::Grid(_)), "{ext}: CFR keeps the grid");

        let drop = clip(&ff, &format!("vfr_drop.{ext}"), DROP);
        let name = format!("d.{ext}");
        let src = filmcraft_codecs::open_bytes(&name, bytes(&drop)).expect("open");
        let clock = src.frame_clock();
        let FrameClock::Times(starts) = &clock else { panic!("{ext}: a VFR clip has its own frame times") };
        // 120 frames and the end of the last
        assert_eq!(starts.len(), 121, "{ext}");
        // ffmpeg's pictures in presentation order: the oracle for "which frame is this"
        let raw = ffmpeg_frames(&ff, &drop, "yuv420p");
        let size = 128 * 72 * 3 / 2;
        let theirs: Vec<Vec<Vec<u16>>> = raw.chunks_exact(size).map(|c| raw_planes(c, 128, 72, 64, 36, 1)).collect();
        assert_eq!(theirs.len(), 120, "{ext}");
        let which = |t: filmcraft_time::Tick| {
            let ours = planes(&src.video_frame(filmcraft_media::FrameRequest::full(t)).expect("decode"));
            (0..theirs.len()).min_by_key(|&k| max_diff(&ours, &theirs[k])).unwrap_or(usize::MAX) as i64
        };
        for f in [0, 1, 44, 45, 46, 47, 80, 118] {
            let t = clock.tick_of(f);
            assert_eq!(clock.frame_at(t), f);
            assert_eq!(which(t), f, "{ext}: frame {f}");
            // just before the next frame still shows this one
            let late = filmcraft_time::Tick(clock.tick_of(f + 1).0 - filmcraft_time::Tick::from_rational(2, 1, 1000).0);
            assert_eq!(which(late), f, "{ext}: late in frame {f}");
        }
    }
}
