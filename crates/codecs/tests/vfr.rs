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

/// Soft telecine like DGPulldown (LO.7): a progressive 23.976 MPEG-2 stream re-flagged as 29.97
/// interlaced with 3:2 pulldown (`repeat_first_field` / `top_field_first` on every picture).
fn soft_telecine(es: &[u8]) -> Vec<u8> {
    let mut out = es.to_vec();
    let mut picture = 0usize;
    let mut i = 0;
    while i + 8 < out.len() {
        if out[i] != 0 || out[i + 1] != 0 || out[i + 2] != 1 {
            i += 1;
            continue;
        }
        match (out[i + 3], out[i + 4] >> 4) {
            // sequence header: frame_rate_code 4 = 30000/1001
            (0xB3, _) => out[i + 7] = (out[i + 7] & 0xF0) | 4,
            // sequence extension: progressive_sequence = 0
            (0xB5, 1) => out[i + 5] &= !0x08,
            // picture coding extension: the 3:2 cadence (TFF+RFF, BFF, BFF+RFF, TFF), progressive frames
            (0xB5, 8) => {
                let (tff, rff) = [(true, true), (false, false), (false, true), (true, false)][picture % 4];
                let b3 = (out[i + 7] & !0x82) | if tff { 0x80 } else { 0 } | if rff { 0x02 } else { 0 };
                out[i + 7] = b3;
                out[i + 8] |= 0x80;
                picture += 1;
            }
            _ => {}
        }
        i += 4;
    }
    out
}

#[test]
fn a_soft_telecined_mpeg2_clip_is_constant_film_rate() {
    use filmcraft_media::timing::FrameClock;
    let ff = filmcraft_testkit::require_ffmpeg!();
    let es = fixture(
        &ff,
        "film_23976.m2v",
        &["-f", "lavfi", "-i", "testsrc2=size=128x72:rate=24000/1001:duration=4", "-c:v", "mpeg2video", "-g", "1", "-bf", "0", "-q:v", "4", "-f", "mpeg2video"],
    )
    .expect("ffmpeg fixture");
    let patched = common::dir().join("film_pulldown.m2v");
    std::fs::write(&patched, soft_telecine(&bytes(&es))).expect("write");
    let ts = fixture(&ff, "film_pulldown.ts", &["-fflags", "+genpts", "-f", "mpegvideo", "-i", patched.to_str().unwrap(), "-c", "copy", "-f", "mpegts"])
        .expect("ffmpeg remux");
    let src = filmcraft_codecs::open_bytes("film_pulldown.ts", bytes(&ts)).expect("open");
    let v = src.info().video.as_ref().expect("video");
    assert_eq!(v.vfr, None, "soft telecine is not variable: {:?}", v.vfr);
    assert_eq!((v.frame_rate.num, v.frame_rate.den), (24_000, 1001), "the film rate");
    assert!(matches!(src.frame_clock(), FrameClock::Grid(_)));
    let secs = src.info().duration.seconds();
    assert!((secs - 4.0).abs() < 0.1, "{secs}");
}
