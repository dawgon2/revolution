//! NVDEC pictures kept on the GPU (fork 8.6c / XPS item 18): with the compositor on an NVIDIA GPU
//! registered as the CUDA target, NVDEC copies each picture on the GPU into memory the compositor's
//! Vulkan device shares with CUDA, and the compositor copies its planes into textures on the GPU.
//! Every picture is bit-exact with our software decoder (read back through the shared memory),
//! renders exactly as the same picture read back and uploaded, and counts as drawn in place.
//! Its own test binary: the CUDA target is process-wide. Skips without ffmpeg (fixtures), an
//! NVIDIA GPU with NVDEC, or a Vulkan device with external memory.
#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

mod common;

use common::*;
use filmcraft_frame::{PixelData, VideoFrame};

fn nvidia_compositor() -> Option<(filmcraft_gpu::GpuCompositor, wgpu::Device)> {
    let instance = wgpu::Instance::default();
    let opts = wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() };
    let adapter = pollster::block_on(instance.request_adapter(&opts)).ok()?;
    if adapter.get_info().vendor != 0x10de {
        eprintln!("SKIPPED: the high-performance GPU is not NVIDIA's ({})", adapter.get_info().name);
        return None;
    }
    let desc = wgpu::DeviceDescriptor {
        required_features: adapter.features() & filmcraft_gpu::wanted_features(),
        required_limits: wgpu::Limits { max_texture_dimension_2d: 8192, ..wgpu::Limits::default() },
        ..Default::default()
    };
    match filmcraft_gpu::interop::create_device(&adapter, &desc) {
        Some(Ok((dev, q))) => Some((filmcraft_gpu::GpuCompositor::new(&dev, &q), dev)),
        _ => {
            eprintln!("SKIPPED: no Vulkan device with external memory");
            None
        }
    }
}

/// The decoder's reasons (why a picture was read back) on stderr.
struct Stderr;
impl log::Log for Stderr {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.target().starts_with("filmcraft")
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) && r.level() <= log::Level::Info {
            eprintln!("{} {}: {}", r.level(), r.target(), r.args());
        }
    }
    fn flush(&self) {}
}
static STDERR: Stderr = Stderr;

fn render(c: &mut filmcraft_gpu::GpuCompositor, frame: &VideoFrame) -> Vec<f32> {
    let fx = filmcraft_render::plan::LayerFx { size: (frame.width, frame.height), decimation: 1, ops: Vec::new() };
    c.effect_image(frame, &fx).expect("rendered").2
}

#[test]
fn nvdec_pictures_stay_on_the_gpu_bit_exact() {
    let _ = log::set_logger(&STDERR).map(|()| log::set_max_level(log::LevelFilter::Info));
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some((mut c, dev)) = nvidia_compositor() else { return };
    filmcraft_gpu::interop::set_cuda_target(Some(dev));
    // the export renderer's own device on the same GPU: imports the window device's memory
    let Some((ed, eq)) = filmcraft_gpu::GpuCompositor::headless_device_on(Some((0x10de, 0x28a1))).or_else(filmcraft_gpu::GpuCompositor::headless_device) else {
        eprintln!("SKIPPED: no export device");
        return;
    };
    let mut export = filmcraft_gpu::GpuCompositor::new(&ed, &eq);
    let src = "testsrc2=s=1920x1080:r=24:d=1,noise=alls=12:allf=t";
    let cases: [(&str, Vec<&str>); 2] = [
        ("gpu_h264_1080.mp4", vec!["-c:v", "libx264", "-profile:v", "high", "-preset", "veryfast", "-bf", "3", "-pix_fmt", "yuv420p"]),
        (
            "gpu_hevc_main10_1080.mp4",
            vec!["-c:v", "libx265", "-preset", "ultrafast", "-tag:v", "hvc1", "-x265-params", "log-level=error:bframes=3", "-pix_fmt", "yuv420p10le"],
        ),
    ];
    let mut checked = 0;
    for (name, enc) in cases {
        let args: Vec<&str> = ["-f", "lavfi", "-i", src].into_iter().chain(enc).collect();
        let Some(path) = fixture(&ff, name, &args) else { continue };
        let s = read_stream(&path);
        let mut hw = match filmcraft_platform::nvdec_factory(&s.entry) {
            Some(Ok(d)) => d,
            Some(Err(e)) => panic!("{name}: {e}"),
            None => {
                eprintln!("SKIPPED: no NVDEC for {name}");
                continue;
            }
        };
        let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
        // picture by picture, as playback holds them (a few at a time), each against the reference
        let before = c.gpu_frames_in_place;
        let mut n = 0;
        let before_export = export.gpu_frames_in_place;
        let mut check = |f: filmcraft_codecs::DecodedFrame, c: &mut filmcraft_gpu::GpuCompositor, e: &mut filmcraft_gpu::GpuCompositor| {
            let PixelData::Gpu(g) = &f.frame.data else { panic!("{name}: picture {n} not on the GPU") };
            assert!(g.as_any().is_some_and(|a| a.is::<filmcraft_gpu::shared_picture::SharedPicture>()), "{name}: picture {n} in shared memory");
            let want = reference.get(n).unwrap_or_else(|| panic!("{name}: more pictures than the reference"));
            assert_same(name, std::slice::from_ref(&f), std::slice::from_ref(want));
            if n < 3 {
                let in_place = render(c, &f.frame);
                let read = VideoFrame { data: g.download(f.frame.width, f.frame.height).expect("read back"), ..f.frame.clone() };
                let uploaded = render(c, &read);
                let worst = in_place.iter().zip(&uploaded).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
                assert!(worst < 1e-6, "{name}: in place differs from read back by {worst}");
                let on_export = render(e, &f.frame);
                let worst = on_export.iter().zip(&uploaded).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
                assert!(worst < 1e-6, "{name}: on the export device it differs from read back by {worst}");
            }
            n += 1;
        };
        for (sample, pts) in &s.samples {
            for f in hw.decode(sample, *pts).expect("decode") {
                check(f, &mut c, &mut export);
            }
        }
        for f in hw.flush() {
            check(f, &mut c, &mut export);
        }
        assert_eq!(n, reference.len(), "{name}: every picture");
        assert_eq!(c.gpu_frames_in_place - before, 3, "{name}: drawn in place ({} read back)", c.gpu_frames_read_back);
        assert_eq!(export.gpu_frames_in_place - before_export, 3, "{name}: drawn in place on the export device ({} read back)", export.gpu_frames_read_back);
        let out = reference;
        eprintln!("{name}: {} pictures on the GPU, bit-exact; 3 drawn in place exactly as read back", out.len());
        checked += 1;
    }
    filmcraft_gpu::interop::set_cuda_target(None);
    eprintln!("{checked} streams");
}
