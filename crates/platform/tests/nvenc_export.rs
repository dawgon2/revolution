//! Export with Hardware encoding (NVENC) against the software encoder, through the real export
//! pipeline: the same frames, a file both decode, counters showing which encoder ran, and what NVENC
//! declines (hardware encoding Off, two-pass, MXF...) going to the software encoder. Its own test
//! binary: the encoder registry is process-wide. Skips without an NVIDIA GPU with NVENC.
#![cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]

use std::sync::Arc;

use filmcraft_export::{BitrateMode, ExportSettings, Format, HardwareEncoding, Progress, export, hw_encode_stats};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

/// The encoder counters are process-wide: one test at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn project(w: u32, h: u32, frames: i64) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("x");
    let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), w, h, rate, rate.tick_of(frames));
    let info = g.info().clone();
    let clip = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(clip, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-nvenc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

fn decode(path: &str) -> Vec<Arc<filmcraft_frame::VideoFrame>> {
    let bytes: Arc<[u8]> = std::fs::read(path).unwrap().into();
    let src = filmcraft_codecs::open_bytes(path, bytes).unwrap();
    let rate = src.info().frame_rate();
    let n = rate.frame_at(src.info().duration);
    (0..n).map(|i| src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap()).collect()
}

fn luma_psnr(a: &filmcraft_frame::VideoFrame, b: &filmcraft_frame::VideoFrame) -> f64 {
    let (x, y) = (a.luma8(), b.luma8());
    let mse = x.iter().zip(&y).map(|(p, q)| (f64::from(*p) - f64::from(*q)).powi(2)).sum::<f64>() / x.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

fn settings(path: String, hw: HardwareEncoding) -> ExportSettings {
    ExportSettings {
        format: Format::H264,
        path,
        include_audio: false,
        bitrate_kbps: 4000,
        bitrate_mode: BitrateMode::Vbr1Pass,
        hardware_encoding: hw,
        ..Default::default()
    }
}

#[test]
fn hardware_export_matches_the_software_export() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    let (p, seq, m) = project(1280, 720, 72);
    let soft_path = tmp("soft.mp4");
    let before_soft = hw_encode_stats();
    export(&p, seq, &settings(soft_path.clone(), HardwareEncoding::Off), &m, &Progress::default()).unwrap();
    assert_eq!(hw_encode_stats(), before_soft, "Off: no hardware encoder attempt");

    let hw_path = tmp("hw.mp4");
    let before = hw_encode_stats();
    let report = export(&p, seq, &settings(hw_path.clone(), HardwareEncoding::Auto), &m, &Progress::default()).unwrap();
    let after = hw_encode_stats();
    if after.sessions == before.sessions {
        assert_eq!(after.declined - before.declined, 1, "Auto attempted hardware and reported the fallback");
        assert_eq!(after.frames, before.frames);
        assert_eq!(std::fs::read(&soft_path).unwrap(), std::fs::read(&hw_path).unwrap(), "fallback preserves software determinism");
        eprintln!("no NVENC here: byte-identical software fallback verified");
        return;
    }
    assert_eq!(report.frames, 72);
    assert_eq!(after.frames - before.frames, 72, "every picture went through NVENC");

    let (soft, hard) = (decode(&soft_path), decode(&hw_path));
    assert_eq!((soft.len(), hard.len()), (72, 72), "frame counts");
    let worst = soft.iter().zip(&hard).map(|(a, b)| luma_psnr(a, b)).fold(99.0, f64::min);
    assert!(worst > 30.0, "hardware export differs from the software one: {worst:.1} dB");
    let size = |p: &str| std::fs::metadata(p).unwrap().len();
    eprintln!("software {} bytes, hardware {} bytes, worst luma PSNR between them {worst:.1} dB", size(&soft_path), size(&hw_path));

    // the file plays: ffmpeg decodes it without errors (external oracle only)
    if let Some(ff) = filmcraft_testkit::ffmpeg() {
        let out = std::process::Command::new(ff).args(["-v", "error", "-i", &hw_path, "-f", "null", "-"]).output().unwrap();
        assert!(out.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn what_nvenc_does_not_take_goes_to_the_software_encoder() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    filmcraft_platform::register();
    let (p, seq, m) = project(640, 360, 24);
    let run = |name: &str, f: &dyn Fn(&mut ExportSettings)| {
        let mut s = settings(tmp(name), HardwareEncoding::Auto);
        f(&mut s);
        let before = hw_encode_stats();
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let after = hw_encode_stats();
        (after.frames - before.frames, after.declined - before.declined)
    };
    // two-pass VBR is a software feature (both passes ask, both are declined)
    assert_eq!(run("two_pass.mp4", &|s| s.bitrate_mode = BitrateMode::Vbr2Pass), (0, 2));
    // MXF H.264 is an Annex B stream with in-band parameter sets: the software encoder's
    assert_eq!(
        run("mxf.mxf", &|s| {
            s.format = Format::MxfOp1a;
            s.mxf_video_codec = filmcraft_export::MxfVideoCodec::H264;
        }),
        (0, 1)
    );
    // an odd frame size is rounded to even by the pipeline, so NVENC still takes it
    let hardware_available = filmcraft_platform::nvenc::available();
    let odd = run("odd.mp4", &|s| s.frame_size = Some((641, 359)));
    assert_eq!(odd, if hardware_available { (24, 0) } else { (0, 1) });
    // Off never touches the hardware encoder
    assert_eq!(run("off.mp4", &|s| s.hardware_encoding = HardwareEncoding::Off), (0, 0));
    // the decoded file of a declined export is fine
    assert_eq!(decode(&tmp("odd.mp4")).len(), 24);
}

/// Fork XPS 6 / 7: the Linux H.265 export renders each frame on the GPU and hands it over as
/// planar 4:2:0 (`EncoderFrame::yuv`, `rgba` empty). NVENC took only RGBA there, so every NVENC
/// H.265 export on Linux failed at its first frame ("the picture is smaller than the encoder's
/// size"). Main (8-bit) and Main 10 (10-bit SDR, PQ, HLG) from such frames now decode with our own
/// HEVC decoder to pictures close to the source.
#[cfg(target_os = "linux")]
#[test]
fn gpu_rendered_planar_frames_encode_on_linux() {
    use filmcraft_export::{ColorSignal, EncoderFrame};
    use filmcraft_frame::{Chroma, PixelData};
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !filmcraft_platform::nvenc::hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC encoder");
        return;
    }
    let (w, h, n) = (640usize, 360usize, 24usize);
    let (cw, ch) = (w / 2, h / 2);
    // a moving gradient in limited-range codes (`max`: 235 8-bit, 940 10-bit)
    let plane = |i: usize, ten: bool| -> Vec<u8> {
        let s = if ten { 4 } else { 1 };
        let mut out = Vec::new();
        let mut push = |v: usize| {
            if ten {
                out.extend_from_slice(&(v as u16).to_le_bytes());
            } else {
                out.push(v as u8);
            }
        };
        for y in 0..h {
            for x in 0..w {
                push((16 + (x * 200 / w + y * 19 / h + i * 3) % 219) * s);
            }
        }
        for c in 0..2 {
            for y in 0..ch {
                for x in 0..cw {
                    push((128 + (x * 40 / cw) + c * 20 + y * 10 / ch - 30) * s);
                }
            }
        }
        out
    };
    for (name, depth, signal) in
        [("main", 8, ColorSignal::default()), ("main10", 10, ColorSignal::default()), ("pq", 10, ColorSignal::PQ), ("hlg", 10, ColorSignal::HLG)]
    {
        let ten = depth == 10;
        let s = ExportSettings {
            format: Format::Hevc,
            hevc_bit_depth: depth,
            signal,
            bitrate_kbps: 8000,
            bitrate_mode: BitrateMode::Vbr1Pass,
            ..Default::default()
        };
        let mut enc =
            filmcraft_platform::nvenc::export::linux_hevc_encoder(w as u32, h as u32, FrameRate::FPS_24, &s).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut packets = Vec::new();
        let sources: Vec<Vec<u8>> = (0..n).map(|i| plane(i, ten)).collect();
        for (i, yuv) in sources.iter().enumerate() {
            let f = EncoderFrame { width: w as u32, height: h as u32, rgba: &[], hdr: None, yuv: Some(yuv), gpu: None, index: i as u64 };
            packets.extend(enc.encode(&f).unwrap_or_else(|e| panic!("{name} frame {i}: {e}")));
        }
        packets.extend(enc.flush().expect("flush"));
        assert_eq!(packets.len(), n, "{name}: one packet per picture");
        let entry = enc.sample_entry();
        let den = FrameRate::FPS_24.den;
        let samples: Vec<(Vec<u8>, i64)> =
            packets.iter().enumerate().map(|(k, p)| (p.data.clone(), k as i64 * den + i64::from(p.composition_offset))).collect();
        let mut dec = filmcraft_codecs::software_video_decoder(&entry).expect("an HEVC decoder");
        let mut out = Vec::new();
        for (data, pts) in &samples {
            out.extend(dec.decode(data, *pts).expect("decodes"));
        }
        out.extend(dec.flush());
        out.sort_by_key(|f| f.pts);
        assert_eq!(out.len(), n, "{name}: every picture decodes");
        let mut worst = f64::MAX;
        for (f, src) in out.iter().zip(&sources) {
            let p = f.frame.to_planar().unwrap_or_else(|| f.frame.clone());
            let (got, max): (Vec<f64>, f64) = match &p.data {
                PixelData::Yuv16 { planes, chroma, bits, .. } if ten => {
                    assert_eq!((*chroma, *bits), (Chroma::C420, 10), "{name}: 10-bit 4:2:0");
                    (planes[0].iter().map(|v| f64::from(*v)).collect(), 1023.0)
                }
                PixelData::Yuv8 { planes, .. } if !ten => (planes[0].iter().map(|v| f64::from(*v)).collect(), 255.0),
                _ => panic!("{name}: decoded as {}", p.format_label()),
            };
            let want: Vec<f64> = if ten {
                src[..w * h * 2].as_chunks::<2>().0.iter().map(|b| f64::from(u16::from_le_bytes(*b))).collect()
            } else {
                src[..w * h].iter().map(|v| f64::from(*v)).collect()
            };
            let mse = got.iter().zip(&want).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / want.len() as f64;
            worst = worst.min(if mse == 0.0 { 99.0 } else { 10.0 * (max * max / mse).log10() });
        }
        eprintln!("{name}: {} packets, worst luma PSNR {worst:.1} dB", packets.len());
        assert!(worst > 38.0, "{name}: worst luma PSNR {worst:.1} dB");
    }
}

/// Fork XPS 8: NVENC failing part-way through an export (the GPU lost, the driver reset; simulated
/// with `nvenc::lose_device_after`, no sudo needed) ends the encode with an error that names NVENC;
/// nothing panics, the stream's packets so far are intact, and the session is released (a new
/// encoder opens and encodes afterwards).
#[cfg(target_os = "linux")]
#[test]
fn nvenc_lost_mid_export_is_an_error_naming_nvenc() {
    use filmcraft_export::EncoderFrame;
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !filmcraft_platform::nvenc::hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC encoder");
        return;
    }
    let (w, h) = (640usize, 360usize);
    let yuv: Vec<u8> = (0..w * h * 3 / 2).map(|i| (i % 200 + 16) as u8).collect();
    let s = ExportSettings { format: Format::Hevc, bitrate_kbps: 4000, bitrate_mode: BitrateMode::Vbr1Pass, ..Default::default() };
    let frame = |i: u64| EncoderFrame { width: w as u32, height: h as u32, rgba: &[], hdr: None, yuv: Some(&yuv), gpu: None, index: i };
    filmcraft_platform::nvenc::lose_device_after(12);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut enc = filmcraft_platform::nvenc::export::linux_hevc_encoder(w as u32, h as u32, FrameRate::FPS_24, &s).expect("opens");
        let mut packets = 0;
        for i in 0..48u64 {
            match enc.encode(&frame(i)) {
                Ok(p) => packets += p.len(),
                Err(e) => return (i, packets, e.to_string()),
            }
        }
        (48, packets, String::new())
    }));
    filmcraft_platform::nvenc::lose_device_after(u64::MAX);
    let (at, packets, err) = result.expect("no panic");
    assert_eq!(at, 12, "fails at the lost picture");
    assert!(err.contains("NVENC"), "{err}");
    eprintln!("lost at picture {at} after {packets} packets: {err}");
    // the session was released: a new one opens and encodes
    let mut enc = filmcraft_platform::nvenc::export::linux_hevc_encoder(w as u32, h as u32, FrameRate::FPS_24, &s).expect("reopens");
    let mut n = 0;
    for i in 0..8 {
        n += enc.encode(&frame(i)).expect("encodes").len();
    }
    n += enc.flush().expect("flush").len();
    assert_eq!(n, 8);
}

/// Planar 4:2:0 8-bit test pictures: a moving gradient, limited range.
#[cfg(target_os = "linux")]
fn moving_yuv(w: usize, h: usize, i: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    for y in 0..h {
        for x in 0..w {
            out.push((16 + (x * 200 / w + y * 19 / h + i * 3) % 219) as u8);
        }
    }
    for c in 0..2 {
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                out.push((98 + (x * 40 / (w / 2)) + c * 20 + y * 10 / (h / 2)) as u8);
            }
        }
    }
    out
}

/// Decode `packets` (decoding order, one frame each) with our HEVC decoder from `entry`; the
/// pictures in presentation order with their worst luma PSNR against `sources`.
#[cfg(target_os = "linux")]
fn decode_against(
    entry: &filmcraft_isobmff::SampleEntry,
    packets: &[filmcraft_export::EncodedPacket],
    sources: &[Vec<u8>],
    w: usize,
    h: usize,
) -> (usize, f64) {
    use filmcraft_frame::PixelData;
    let den = FrameRate::FPS_24.den;
    let mut dec = filmcraft_codecs::software_video_decoder(entry).expect("an HEVC decoder");
    let mut out = Vec::new();
    for (k, p) in packets.iter().enumerate() {
        out.extend(dec.decode(&p.data, k as i64 * den + i64::from(p.composition_offset)).expect("decodes"));
    }
    out.extend(dec.flush());
    out.sort_by_key(|f| f.pts);
    let mut worst = f64::MAX;
    for (f, src) in out.iter().zip(sources) {
        let p = f.frame.to_planar().unwrap_or_else(|| f.frame.clone());
        let PixelData::Yuv8 { planes, .. } = &p.data else { panic!("decoded as {}", p.format_label()) };
        let mse = planes[0].iter().zip(&src[..w * h]).map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2)).sum::<f64>() / (w * h) as f64;
        worst = worst.min(if mse == 0.0 { 99.0 } else { 10.0 * (255.0 * 255.0 / mse).log10() });
    }
    (out.len(), worst)
}

/// Fork 8.6d (XPS item 8's open half): NVENC lost part-way through an H.265 export no longer ends
/// it: the pictures not written yet, and every one after, go to VA-API in process, which starts
/// with an IDR picture and its own parameter sets in band. Every picture is written once, the
/// timing stays NVENC's (B-frame delay kept), and the whole stream decodes close to the source.
#[cfg(target_os = "linux")]
#[test]
fn nvenc_lost_mid_export_carries_on_in_va_api() {
    use filmcraft_export::EncoderFrame;
    use filmcraft_platform::hevc_export::{HevcEncoder, LaneTarget};
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !filmcraft_platform::nvenc::hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC encoder");
        return;
    }
    let (w, h, n) = (640usize, 360usize, 48usize);
    let s = ExportSettings { format: Format::Hevc, bitrate_kbps: 6000, bitrate_mode: BitrateMode::Vbr1Pass, ..Default::default() };
    if HevcEncoder::for_lane(w as u32, h as u32, FrameRate::FPS_24, &s, LaneTarget { node: None, stream: None, in_band: true }).is_err() {
        eprintln!("SKIPPED: no in-process VA-API HEVC encoder");
        return;
    }
    let sources: Vec<Vec<u8>> = (0..n).map(|i| moving_yuv(w, h, i)).collect();
    for lost_at in [12u64, 13, 40] {
        let settings = s.clone();
        let then = Box::new(move || {
            HevcEncoder::for_lane(640, 360, FrameRate::FPS_24, &settings, LaneTarget { node: None, stream: None, in_band: true })
                .map(|e| Box::new(e) as Box<dyn filmcraft_export::VideoEncoder>)
        });
        let mut enc = filmcraft_platform::nvenc::export::linux_hevc_encoder_with_fallback(w as u32, h as u32, FrameRate::FPS_24, &s, then).expect("opens");
        filmcraft_platform::nvenc::lose_device_after(lost_at);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut packets = Vec::new();
            for (i, yuv) in sources.iter().enumerate() {
                let f = EncoderFrame { width: w as u32, height: h as u32, rgba: &[], hdr: None, yuv: Some(yuv), gpu: None, index: i as u64 };
                packets.extend(enc.encode(&f).unwrap_or_else(|e| panic!("frame {i}: {e}")));
            }
            packets.extend(enc.flush().expect("flush"));
            packets
        }));
        filmcraft_platform::nvenc::lose_device_after(u64::MAX);
        let packets = result.expect("no panic, no error");
        assert_eq!(packets.len(), n, "lost at {lost_at}: one packet per picture");
        // presentation times: every frame exactly once (decode index + offset, NVENC's delay kept)
        let den = FrameRate::FPS_24.den;
        let start = enc.media_start().unwrap_or(0);
        let mut shown: Vec<i64> = packets.iter().enumerate().map(|(k, p)| (k as i64 * den + i64::from(p.composition_offset) - start) / den).collect();
        shown.sort_unstable();
        assert_eq!(shown, (0..n as i64).collect::<Vec<_>>(), "lost at {lost_at}");
        let (decoded, worst) = decode_against(&enc.sample_entry(), &packets, &sources, w, h);
        eprintln!("lost at {lost_at}: {decoded} pictures, worst luma PSNR {worst:.1} dB");
        assert_eq!(decoded, n, "lost at {lost_at}: every picture decodes");
        assert!(worst > 35.0, "lost at {lost_at}: worst luma PSNR {worst:.1} dB");
    }
}

/// Fork 8.6d: an NVENC lane of a two-GPU export writes chunks VA-API's can sit next to: no
/// B-frames (the same timing), VPS / SPS / PPS in every IDR picture and a `hev1` entry, so a
/// chunk decodes from its own parameter sets alone.
#[cfg(target_os = "linux")]
#[test]
fn nvenc_lane_chunks_carry_their_parameter_sets() {
    use filmcraft_export::EncoderFrame;
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !filmcraft_platform::nvenc::hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC encoder");
        return;
    }
    let (w, h, n) = (640usize, 360usize, 30usize);
    let s = ExportSettings { format: Format::Hevc, bitrate_kbps: 6000, bitrate_mode: BitrateMode::Vbr1Pass, keyframe_distance: Some(10), ..Default::default() };
    let sources: Vec<Vec<u8>> = (0..n).map(|i| moving_yuv(w, h, i)).collect();
    let mut enc = filmcraft_platform::nvenc::export::linux_hevc_lane_encoder(w as u32, h as u32, FrameRate::FPS_24, &s).expect("opens");
    let mut packets = Vec::new();
    for (i, yuv) in sources.iter().enumerate() {
        let f = EncoderFrame { width: w as u32, height: h as u32, rgba: &[], hdr: None, yuv: Some(yuv), gpu: None, index: i as u64 };
        packets.extend(enc.encode(&f).expect("encodes"));
    }
    packets.extend(enc.flush().expect("flush"));
    assert_eq!(packets.len(), n);
    assert_eq!(enc.media_start(), None, "no reordering");
    assert!(packets.iter().all(|p| p.composition_offset == 0), "no B-frames");
    let entry = enc.sample_entry();
    assert_eq!(&entry.format.0, b"hev1");
    let nal_types = |data: &[u8]| {
        let mut t = Vec::new();
        let mut at = 0usize;
        while let Some(len) = data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize) {
            t.push(data.get(at + 4).map_or(255, |b| (b >> 1) & 0x3f));
            at += 4 + len;
        }
        t
    };
    let keys: Vec<_> = packets.iter().enumerate().filter(|(_, p)| p.key).map(|(k, _)| k).collect();
    assert_eq!(keys, vec![0, 10, 20], "an IDR every 10");
    for k in keys {
        assert_eq!(nal_types(&packets[k].data)[..3], [32, 33, 34], "IDR {k}: VPS, SPS, PPS first");
    }
    assert!(!nal_types(&packets[1].data).contains(&33), "no parameter sets outside IDR pictures");
    // the entry's sets stripped: the chunk decodes from its in-band ones
    let mut bare = entry.clone();
    if let filmcraft_isobmff::CodecConfig::Hevc(c) = &mut bare.codec {
        c.arrays.clear();
    }
    let (decoded, worst) = decode_against(&bare, &packets, &sources, w, h);
    eprintln!("lane chunk: {decoded} pictures, worst luma PSNR {worst:.1} dB");
    assert_eq!(decoded, n);
    assert!(worst > 35.0, "worst luma PSNR {worst:.1} dB");
}
