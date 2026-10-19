//! The Intel → AMD hand-off of hardware-decoded HEVC 4:2:2 pictures (fork Phase 6.1b, owner
//! §1a.30): the media engine copies each picture into a linear surface, exports it as a DMA-BUF,
//! and the compositing GPU imports it, unpacks it once into VRAM planes and draws it. A handed-off
//! picture renders exactly as the same picture read back to memory and uploaded; its samples are
//! the reference decoder's; without the hand-off (turned off, a failed copy / export, a refused
//! import) pictures are read back, never a crash. Skips (with SKIPPED) without a display with
//! HEVC Main 4:2:2 10, a GPU device with DMA-BUF import, or the fixtures. Since fork U0.2.3 the
//! decoder is upstream's front end on the zero-copy accelerator (`vaapi::VaDecoder::zero_copy`).
#![cfg(target_os = "linux")]

mod common;

use std::sync::Arc;

use common::*;
use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_frame::{Chroma, GpuFormat, PixelData, VideoFrame};
use filmcraft_platform::vaapi::{VaDecoder, decode_annexb_with};

/// The media engine, one test at a time (as `va_hevc_422`).
static ENGINE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn engine() -> std::sync::MutexGuard<'static, ()> {
    ENGINE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn has_422() -> bool {
    match filmcraft_platform::va::Display::for_hevc_422() {
        Ok(d) => {
            eprintln!("HEVC 4:2:2 on {} ({})", d.node, d.vendor);
            true
        }
        Err(e) => {
            eprintln!("SKIPPED: no VA-API display with HEVC Main 4:2:2 10: {e}");
            false
        }
    }
}

/// The compositing device (the high-performance adapter: AMD here) with DMA-BUF import.
/// The device is made as the app makes its own (`filmcraft_ui_egui::device_descriptor`: the
/// compositor's wanted features only, no adapter-specific format features).
fn gpu() -> Option<filmcraft_gpu::GpuCompositor> {
    let instance = wgpu::Instance::default();
    let opts = wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() };
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&opts)) else {
        eprintln!("SKIPPED: no GPU");
        return None;
    };
    let desc = wgpu::DeviceDescriptor {
        required_features: adapter.features() & filmcraft_gpu::wanted_features(),
        required_limits: wgpu::Limits { max_texture_dimension_2d: 8192, ..wgpu::Limits::default() },
        ..Default::default()
    };
    match filmcraft_gpu::interop::create_device(&adapter, &desc) {
        Some(Ok((dev, q))) => {
            eprintln!("compositing on {}", adapter.get_info().name);
            Some(filmcraft_gpu::GpuCompositor::new(&dev, &q))
        }
        _ => {
            eprintln!("SKIPPED: no GPU device with DMA-BUF import");
            None
        }
    }
}

/// `frame` drawn by the compositor (its effect stage, no effects): linear RGBA f32.
fn render(c: &mut filmcraft_gpu::GpuCompositor, frame: &VideoFrame) -> Vec<f32> {
    let fx = filmcraft_render::plan::LayerFx { size: (frame.width, frame.height), decimation: 1, ops: Vec::new() };
    c.effect_image(frame, &fx).expect("rendered").2
}

/// The handed-off GPU frame drawn in place, and the same picture read back and uploaded: the
/// largest difference.
fn in_place_vs_read_back(c: &mut filmcraft_gpu::GpuCompositor, frame: &VideoFrame) -> f32 {
    let PixelData::Gpu(s) = &frame.data else { panic!("a GPU frame") };
    assert_eq!(s.format(), GpuFormat::Y210, "packed 4:2:2");
    let in_place = render(c, frame);
    let read = VideoFrame { data: s.download(frame.width, frame.height).expect("read back"), ..frame.clone() };
    assert!(matches!(read.data, PixelData::Yuv16Bi { chroma: Chroma::C422, bits: 10, .. }));
    let uploaded = render(c, &read);
    assert_eq!(in_place.len(), uploaded.len());
    in_place.iter().zip(&uploaded).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max)
}

/// Every picture's Y, U, V planes back to back as 16-bit little-endian codes (the `.yuv` layout).
fn planar_bytes(frames: &[VideoFrame]) -> Vec<u8> {
    let mut got = Vec::new();
    for f in frames {
        let p = f.to_planar().expect("planar");
        let PixelData::Yuv16 { planes, chroma: Chroma::C422, bits: 10, .. } = &p.data else { panic!("4:2:2 10-bit") };
        planes.iter().for_each(|p| p.iter().for_each(|v| got.extend_from_slice(&v.to_le_bytes())));
    }
    got
}

/// The 4:2:2 conformance fixtures through the hand-off: every picture's samples are the
/// reference's (CPU read-back, from the decoded surface), and the first pictures of each, drawn from
/// the linear copy the GPU imports, render exactly as read back.
#[test]
fn handed_off_fixtures_match_the_reference_and_render_as_read_back() {
    let _engine = engine();
    if !has_422() {
        return;
    }
    let Some(mut c) = gpu() else { return };
    let dir = filmcraft_testkit::fixtures::fixtures_root().join("hevc");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "hevc") && p.to_string_lossy().contains("_422")).collect())
        .unwrap_or_default();
    names.sort();
    if names.is_empty() {
        eprintln!("SKIPPED: no 4:2:2 fixtures in {} (cargo xtask fixtures hevc)", dir.display());
        return;
    }
    let (mut checked, mut drawn) = (0, 0);
    for path in names {
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let Ok(reference) = std::fs::read(path.with_extension("yuv")) else { continue };
        let frames = decode_annexb_with(&std::fs::read(&path).expect("fixture"), true).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(frames.iter().all(|f| matches!(&f.data, PixelData::Gpu(s) if s.format() == GpuFormat::Y210)), "{name}: handed off");
        assert!(planar_bytes(&frames) == reference, "{name}: the read-back differs from the reference");
        for f in frames.iter().take(2) {
            let worst = in_place_vs_read_back(&mut c, f);
            assert!(worst < 1e-6, "{name}: in place differs from read back by {worst}");
            drawn += 1;
        }
        checked += 1;
        eprintln!("{name}: {} pictures bit-exact; first 2 drawn from the linear copy exactly as read back", frames.len());
    }
    assert!(c.gpu_frames_in_place >= drawn, "drawn in place: {} of {drawn} ({} read back)", c.gpu_frames_in_place, c.gpu_frames_read_back);
    eprintln!("{checked} fixtures, {drawn} pictures drawn in place");
}

/// The first `n` samples of the video track of a large MOV, without reading the rest.
fn first_samples(path: &std::path::Path, n: usize) -> Option<Stream> {
    let bytes = std::fs::read(path).ok()?;
    let file = filmcraft_isobmff::open(bytes.clone()).ok()?;
    let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video)?;
    let track = file.tracks.get(t)?;
    let n = n.min(track.samples.len());
    let samples = (0..n).map(|i| Some((file.read_sample(&bytes, t, i).ok()?, track.samples[i].pts))).collect::<Option<Vec<_>>>()?;
    Some(Stream { entry: track.entries.first()?.clone(), samples, sync: track.samples.iter().take(n).map(|s| s.is_sync).collect() })
}

/// The first pictures of the 6.2K xm5 4:2:2 clip through the media stack's decoder: handed off
/// by default, the software decoder's samples, drawn exactly as read back.
#[test]
fn xm5_422_pictures_are_handed_off_and_render_as_read_back() {
    let _engine = engine();
    if !has_422() {
        return;
    }
    let dir = std::env::var_os("FILMCRAFT_XM5_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| filmcraft_testkit::fixtures::fixtures_root().join("xm5"));
    let path = dir.join("xm5_422_2997_20s.mov");
    if !path.exists() {
        eprintln!("SKIPPED: no {}", path.display());
        return;
    }
    let Some(mut c) = gpu() else { return };
    let s = first_samples(&path, 4).expect("readable clip");
    let info = NalStreamInfo::from_entry(&s.entry).expect("HEVC").expect("hvcC");
    let mut hw = VaDecoder::zero_copy(info).expect("taken in hardware");
    let got = decode_all(&mut hw, &s.samples);
    let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).expect("software").as_mut(), &s.samples);
    assert_same("xm5 4:2:2 handed off", &got, &reference);
    for f in got.iter().take(3) {
        let worst = in_place_vs_read_back(&mut c, &f.frame);
        assert!(worst < 1e-6, "xm5 pts {}: in place differs from read back by {worst}", f.pts);
    }
    assert!(c.gpu_frames_in_place >= 3, "drawn in place ({} read back)", c.gpu_frames_read_back);
    eprintln!("xm5 4:2:2 {}x{}: {} pictures handed off bit-exact, 3 drawn identically", got[0].frame.width, got[0].frame.height, got.len());
}

/// A small 4:2:2 MP4 (ffmpeg-made, cached; the one `va_hevc_422` uses).
fn mp4_422(ff: &std::path::Path) -> Option<std::path::PathBuf> {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=640x360:r=24:d=2,noise=alls=12:allf=t,fade=in:0:20",
        "-c:v",
        "libx265",
        "-preset",
        "fast",
        "-profile:v",
        "main422-10",
        "-tag:v",
        "hvc1",
        "-x265-params",
        "log-level=error:bframes=3:keyint=24:min-keyint=24:scenecut=0:weightp=1",
        "-pix_fmt",
        "yuv422p10le",
    ];
    fixture(ff, "hevc_422_10_wp.mp4", &args)
}

/// XPS 16: two streams of the same size handed off one after the other each read back as their
/// own pictures. With linear copy surfaces shared between copiers, the second stream's copies
/// (`intra_422` after `tu4_422`, iHD 26.3.5) were never written and read back as the first
/// stream's pictures.
#[test]
fn consecutive_streams_of_one_size_are_each_handed_off_right() {
    let _engine = engine();
    if !has_422() {
        return;
    }
    let dir = filmcraft_testkit::fixtures::fixtures_root().join("hevc");
    let order = ["tu4_422", "intra_422", "tu4_422", "intra_nosao_nodbk_422"];
    if order.iter().any(|n| !dir.join(format!("{n}.hevc")).exists()) {
        eprintln!("SKIPPED: no 4:2:2 fixtures in {} (cargo xtask fixtures hevc)", dir.display());
        return;
    }
    for name in order {
        let reference = std::fs::read(dir.join(format!("{name}.yuv"))).expect("reference");
        let frames = decode_annexb_with(&std::fs::read(dir.join(format!("{name}.hevc"))).expect("fixture"), true).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(frames.iter().all(|f| matches!(&f.data, PixelData::Gpu(s) if s.format() == GpuFormat::Y210)), "{name}: handed off");
        assert!(planar_bytes(&frames) == reference, "{name}: the read-back differs from the reference (after another stream)");
    }
}

/// Hand-off off (as with `FILMCRAFT_NO_GPU_IMPORT`), or failing at its first picture (a refused
/// copy or export): the pictures are read back to memory, with the reference's samples.
#[test]
fn without_the_handoff_pictures_are_read_back() {
    let _engine = engine();
    let ff = filmcraft_testkit::require_ffmpeg!();
    if !has_422() {
        return;
    }
    let Some(path) = mp4_422(&ff) else { return };
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
    let read_back = |f: &filmcraft_codecs::DecodedFrame| matches!(f.frame.data, PixelData::Yuv16Bi { chroma: Chroma::C422, .. });
    // on (the default): GPU frames
    let mut on = VaDecoder::zero_copy(info.clone()).unwrap();
    let out = decode_all(&mut on, &s.samples);
    assert_same("hand-off on", &out, &reference);
    assert!(out.iter().all(|f| matches!(&f.frame.data, PixelData::Gpu(g) if g.format() == GpuFormat::Y210)), "handed off");
    // off
    let mut off = VaDecoder::zero_copy(info.clone()).unwrap();
    off.set_handoff(false);
    let out = decode_all(&mut off, &s.samples);
    assert_same("hand-off off", &out, &reference);
    assert!(out.iter().all(read_back), "read back");
    // failing at the first picture: read back from then on
    let mut failing = VaDecoder::zero_copy(info).unwrap();
    failing.fail_next_handoff();
    let out = decode_all(&mut failing, &s.samples);
    assert_same("hand-off failed", &out, &reference);
    assert!(out.iter().all(read_back), "read back after the failure");
}

/// A real handed-off picture whose export description is altered.
#[derive(Debug)]
struct Altered {
    inner: Arc<dyn filmcraft_frame::GpuSurface>,
    id: u64,
    alter: fn(&mut filmcraft_frame::DmaBuf),
}

impl filmcraft_frame::GpuSurface for Altered {
    fn memory_id(&self) -> u64 {
        self.id
    }
    fn liveness(&self) -> std::sync::Weak<()> {
        self.inner.liveness()
    }
    fn format(&self) -> GpuFormat {
        self.inner.format()
    }
    fn coded_size(&self) -> (u32, u32) {
        self.inner.coded_size()
    }
    fn crop_origin(&self) -> (u32, u32) {
        self.inner.crop_origin()
    }
    fn dmabuf(&self) -> Option<filmcraft_frame::DmaBuf> {
        let mut d = self.inner.dmabuf()?;
        (self.alter)(&mut d);
        Some(d)
    }
    fn download(&self, width: u32, height: u32) -> Result<PixelData, String> {
        self.inner.download(width, height)
    }
    fn gpu_bytes(&self) -> usize {
        self.inner.gpu_bytes()
    }
}

/// Hostile export descriptions of a packed 4:2:2 picture (planar layer formats, a second layer,
/// missing or out-of-range buffer objects, several memory planes, a pitch or size beyond the
/// buffer, a tiled modifier) are refused before Vulkan sees them or by the driver's checks, and
/// the compositor draws the picture read back instead: the same image, no crash.
#[test]
fn refused_imports_fall_back_to_read_back() {
    let _engine = engine();
    let ff = filmcraft_testkit::require_ffmpeg!();
    if !has_422() {
        return;
    }
    let Some(mut c) = gpu() else { return };
    let Some(path) = mp4_422(&ff) else { return };
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut dec = VaDecoder::zero_copy(info).unwrap();
    let frames = decode_all(&mut dec, &s.samples[..4]);
    let f = frames.first().expect("a picture").frame.clone();
    let PixelData::Gpu(real) = &f.data else { panic!("a GPU frame") };
    let expected = render(&mut c, &VideoFrame { data: real.download(f.width, f.height).unwrap(), ..f.clone() });
    let alterations: [fn(&mut filmcraft_frame::DmaBuf); 9] = [
        |d| d.layer_formats = [u32::from_le_bytes(*b"R16 "), u32::from_le_bytes(*b"GR32")],
        |d| d.layers[1] = d.layers[0].clone(),
        |d| d.layers.iter_mut().flatten().for_each(|p| p.object = 99),
        |d| d.objects = Arc::from(Vec::new()),
        |d| d.layers[0] = vec![d.layers[0][0]; 2],
        |d| d.layers[0][0].pitch = u32::MAX,
        |d| d.height = 1 << 20,
        |d| d.layers[0][0].offset = u32::MAX - 16,
        |d| d.modifier = (0x01 << 56) | 2,
    ];
    for (i, alter) in alterations.into_iter().enumerate() {
        let bad = VideoFrame { data: PixelData::Gpu(Arc::new(Altered { inner: real.clone(), id: u64::MAX - i as u64, alter })), ..f.clone() };
        let before = c.gpu_frames_read_back;
        let got = render(&mut c, &bad);
        assert_eq!(c.gpu_frames_read_back, before + 1, "alteration {i} was read back");
        let worst = got.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-6, "alteration {i}: differs by {worst}");
    }
    // a refusal stops the hand-off for every stream (no copy only to be read back) ...
    assert!(filmcraft_frame::packed_422_refused(), "the refusal is recorded");
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut after = VaDecoder::zero_copy(info).unwrap();
    let out = decode_all(&mut after, &s.samples[..4]);
    assert!(out.iter().all(|f| matches!(f.frame.data, PixelData::Yuv16Bi { .. })), "read back once refused");
    // ... (tests: forget it again); the unaltered picture imports
    filmcraft_frame::reset_packed_422_refusal();
    let before = c.gpu_frames_in_place;
    render(&mut c, &f);
    assert_eq!(c.gpu_frames_in_place, before + 1, "the real picture is drawn in place");
}

/// Pool reuse under load (review of 6.1b): many handed-off pictures in flight at once (held, and
/// drawn by the compositor while the decoder goes on) never share memory, keep their samples (no
/// surface is recycled while a frame holds it: every held picture still reads back as the
/// reference), and repeated passes do not grow the number of live surfaces.
#[test]
fn pool_reuse_under_load_keeps_held_pictures_and_does_not_leak() {
    let _engine = engine();
    let ff = filmcraft_testkit::require_ffmpeg!();
    if !has_422() {
        return;
    }
    let Some(mut c) = gpu() else { return };
    let Some(path) = mp4_422(&ff) else { return };
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
    let mut live = Vec::new();
    for pass in 0..4 {
        let mut dec = VaDecoder::zero_copy(info.clone()).unwrap();
        let mut held = Vec::new();
        for (sample, pts) in &s.samples {
            for f in dec.decode(sample, *pts).expect("decode") {
                // drawn as it comes (the GPU reads the copy while the decoder goes on)
                render(&mut c, &f.frame);
                held.push(f);
            }
        }
        held.extend(dec.flush());
        assert_eq!(held.len(), reference.len(), "pass {pass}: every picture");
        let mut ids: Vec<u64> = held
            .iter()
            .map(|f| match &f.frame.data {
                PixelData::Gpu(g) => g.memory_id(),
                _ => panic!("pass {pass}: handed off"),
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), held.len(), "pass {pass}: pictures in flight share memory");
        // every held picture still holds its own samples (read back by mapping its copy)
        assert_same(&format!("pass {pass}"), &held, &reference);
        drop(held);
        drop(dec);
        let _ = c.device().poll(wgpu::PollType::wait_indefinitely());
        live.push(filmcraft_platform::va::live_surfaces());
    }
    eprintln!("live VA surfaces after each pass: {live:?}");
    assert!(live.windows(2).skip(1).all(|w| w[1] <= w[0]), "live surfaces grow pass after pass: {live:?}");
}
