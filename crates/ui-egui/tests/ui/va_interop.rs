//! Zero-copy hardware-decoded frames (Linux, VA-API + Vulkan DMA-BUF import, ADR 0002): a frame
//! decoded in process and drawn in place by the GPU compositor gives exactly the image of the same
//! picture read back to memory and uploaded. Skipped without VA-API, a Vulkan adapter with the
//! import extensions, or the fixtures (`target/fixtures/platform/hevc_main*.mp4`, made by the
//! platform tests).
#![cfg(target_os = "linux")]

use std::sync::Arc;

use eframe::wgpu;

use filmcraft_codecs::VideoDecoder;
use filmcraft_frame::{PixelData, VideoFrame};

/// A device on the GPU VA-API decodes on (a hybrid laptop's high-performance GPU, NVIDIA, imports
/// none of Intel's tiled pictures: XPS 16), else the high-performance one.
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let decode_gpu = filmcraft_platform::va::Display::get().ok().and_then(|d| filmcraft_platform::pci_ids(&d.node));
    let on_decode_gpu = decode_gpu.and_then(|(vendor, device)| {
        pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN)).into_iter().find(|a| {
            let i = a.get_info();
            (i.vendor, i.device) == (vendor, device)
        })
    });
    let adapter = match on_decode_gpu {
        Some(a) => a,
        None => {
            let opts = wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() };
            pollster::block_on(instance.request_adapter(&opts)).ok()?
        }
    };
    let desc = wgpu::DeviceDescriptor { required_features: adapter.features() & filmcraft_gpu::wanted_features(), ..Default::default() };
    match filmcraft_gpu::interop::create_device(&adapter, &desc)? {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("interop device: {e}");
            None
        }
    }
}

#[test]
fn imported_frames_draw_exactly_as_read_back_frames() {
    let Some((dev, q)) = device() else {
        eprintln!("no Vulkan adapter with DMA-BUF import; skipped");
        return;
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/platform");
    let mut checked = 0;
    for name in ["hevc_main10.mp4", "hevc_main.mp4"] {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("{name}: fixture missing; skipped");
            continue;
        };
        let file = filmcraft_isobmff::open(bytes.clone()).expect("mp4");
        let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).expect("video");
        let track = &file.tracks[t];
        let entry = track.entries[0].clone();
        let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(&entry).expect("hvcC").expect("info");
        let mut dec = match filmcraft_platform::vaapi::VaDecoder::zero_copy(info) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{name}: no in-process VA-API ({e}); skipped");
                return;
            }
        };
        let mut frames = Vec::new();
        for i in 0..track.samples.len().min(12) {
            let s = file.read_sample(&bytes, t, i).expect("sample");
            frames.extend(dec.decode(&s, track.samples[i].pts).expect("decode"));
        }
        frames.extend(dec.flush());
        assert!(!frames.is_empty(), "{name}: no frames");
        let mut c = filmcraft_gpu::GpuCompositor::new(&dev, &q);
        for f in &frames {
            assert!(matches!(f.frame.data, PixelData::Gpu(_)), "{name}: the decoder hands out GPU frames");
            let (w, h) = (f.frame.width, f.frame.height);
            let fx = filmcraft_render::plan::LayerFx { size: (w, h), decimation: 1, ops: Vec::new() };
            let gpu = Arc::new(f.frame.clone());
            let (_, _, in_place) = c.effect_image(&gpu, &fx).expect("draw in place");
            let PixelData::Gpu(s) = &f.frame.data else { unreachable!() };
            let read = VideoFrame { data: s.download(w, h).expect("read back"), ..f.frame.clone() };
            let (_, _, uploaded) = c.effect_image(&read, &fx).expect("draw read-back");
            let worst = in_place.iter().zip(&uploaded).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            assert!(worst < 1e-6, "{name} pts {}: in place differs from read back by {worst}", f.pts);
            checked += 1;
        }
        assert!(c.gpu_frames_in_place > 0, "{name}: frames were drawn in place, not read back ({} read back)", c.gpu_frames_read_back);
        eprintln!("{name}: {} frames identical in place and read back", frames.len());
    }
    eprintln!("{checked} frames checked");
}

/// A real picture's export, altered.
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
    fn format(&self) -> filmcraft_frame::GpuFormat {
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

/// Unsafe-code review (FORK-PLAN §1a.5): a DMA-BUF description that names a missing buffer object,
/// has several memory planes in a layer, or does not match the picture's format is refused (the
/// frame is read back instead) rather than handed to Vulkan. Descriptions own their fds (round 2),
/// so a closed or foreign fd can no longer be expressed; an fd of -1 panicked before round 1.
#[test]
fn hostile_dmabuf_descriptions_are_refused() {
    let Some((dev, _q)) = device() else {
        eprintln!("no Vulkan adapter with DMA-BUF import; skipped");
        return;
    };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/platform/hevc_main10.mp4");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("fixture missing; skipped");
        return;
    };
    let file = filmcraft_isobmff::open(bytes.clone()).expect("mp4");
    let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).expect("video");
    let track = &file.tracks[t];
    let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(&track.entries[0]).expect("hvcC").expect("info");
    let Ok(mut dec) = filmcraft_platform::vaapi::VaDecoder::zero_copy(info) else {
        eprintln!("no in-process VA-API; skipped");
        return;
    };
    let mut frames = Vec::new();
    for i in 0..track.samples.len().min(4) {
        let s = file.read_sample(&bytes, t, i).expect("sample");
        frames.extend(dec.decode(&s, track.samples[i].pts).expect("decode"));
    }
    frames.extend(dec.flush());
    let Some(PixelData::Gpu(real)) = frames.first().map(|f| f.frame.data.clone()) else { panic!("no GPU frame") };
    let mut importer = filmcraft_gpu::interop::Importer::new(&dev).expect("importer");
    let alterations: [fn(&mut filmcraft_frame::DmaBuf); 5] = [
        |d| d.layers.iter_mut().flatten().for_each(|p| p.object = 99),
        |d| d.objects = std::sync::Arc::from(Vec::new()),
        |d| d.layer_formats.swap(0, 1),
        |d| d.layers[1].clear(),
        |d| d.layers[0] = vec![d.layers[0][0]; 2],
    ];
    for (i, alter) in alterations.into_iter().enumerate() {
        let bad = Altered { inner: real.clone(), id: u64::MAX - i as u64, alter };
        assert!(importer.import(&bad).is_none(), "alteration {i} was imported");
        assert_eq!(importer.failures, i as u64 + 1);
    }
    assert!(importer.import(&*real).is_some(), "the unaltered picture imports");
}
