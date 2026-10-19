//! Fork 8.6d D2: NVENC encodes export frames straight from GPU memory (the export renderer copies
//! each converted frame into memory shared with CUDA; no read-back), on an NVIDIA GPU.
#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

use std::sync::{Arc, Mutex};

use filmcraft_export::{BitrateMode, EncoderFrame, ExportSettings, Format, VideoEncoder};
use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_render::plan::{FramePlan, PlanLayer};
use filmcraft_time::FrameRate;

static SERIAL: Mutex<()> = Mutex::new(());

/// The NVIDIA GPU's render node, if any.
fn nvidia_node() -> Option<String> {
    let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri").ok()?.flatten().map(|e| e.path().to_string_lossy().into_owned()).collect();
    nodes.sort();
    nodes.into_iter().filter(|n| n.contains("renderD")).find(|n| filmcraft_platform::pci_ids(n).is_some_and(|(v, _)| v == 0x10de))
}

fn plan(w: u32, h: u32, i: u32) -> FramePlan {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[((x * 255 / w + i * 7) % 256) as u8, ((y * 255 / h) % 256) as u8, ((x + y + i * 5) % 256) as u8, 255]);
        }
    }
    let frame = Arc::new(VideoFrame::rgba8(w, h, px));
    FramePlan::Layers {
        width: w as usize,
        height: h as usize,
        layers: vec![PlanLayer::new(frame, filmcraft_geom::Affine::IDENTITY, 1.0, filmcraft_render::blend::Blend::Normal)],
        output: None,
    }
}

/// Worst luma PSNR of decoded `packets` against `refs` (planar 4:2:0 bytes in the encoder's depth).
fn worst_luma(
    entry: &filmcraft_isobmff::SampleEntry,
    packets: &[filmcraft_export::EncodedPacket],
    refs: &[Vec<u8>],
    w: usize,
    h: usize,
    ten: bool,
) -> (usize, f64) {
    let den = FrameRate::FPS_24.den;
    let mut dec = filmcraft_codecs::software_video_decoder(entry).expect("an HEVC decoder");
    let mut out = Vec::new();
    for (k, p) in packets.iter().enumerate() {
        out.extend(dec.decode(&p.data, k as i64 * den + i64::from(p.composition_offset)).expect("decodes"));
    }
    out.extend(dec.flush());
    out.sort_by_key(|f| f.pts);
    let mut worst = f64::MAX;
    for (f, r) in out.iter().zip(refs) {
        let p = f.frame.to_planar().unwrap_or_else(|| f.frame.clone());
        let (got, want, max): (Vec<f64>, Vec<f64>, f64) = match &p.data {
            PixelData::Yuv16 { planes, .. } if ten => (
                planes[0].iter().map(|v| f64::from(*v)).collect(),
                r[..w * h * 2].as_chunks::<2>().0.iter().map(|b| f64::from(u16::from_le_bytes(*b))).collect(),
                1023.0,
            ),
            PixelData::Yuv8 { planes, .. } if !ten => {
                (planes[0].iter().map(|v| f64::from(*v)).collect(), r[..w * h].iter().map(|v| f64::from(*v)).collect(), 255.0)
            }
            _ => panic!("decoded as {}", p.format_label()),
        };
        let mse = got.iter().zip(&want).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / want.len() as f64;
        worst = worst.min(if mse == 0.0 { 99.0 } else { 10.0 * (max * max / mse).log10() });
    }
    (out.len(), worst)
}

/// Every frame is rendered into the encoder's GPU memory and encoded in place (8-bit NV12 and
/// 10-bit P010); the stream decodes to what the read-back path converts; a frame read back from the
/// shared memory equals the read-back path's within a code value.
#[test]
fn nvenc_encodes_export_frames_from_gpu_memory() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(node) = nvidia_node().filter(|_| filmcraft_platform::nvenc::hevc_available()) else {
        eprintln!("SKIPPED: no NVIDIA GPU with an NVENC HEVC encoder");
        return;
    };
    let gpu = filmcraft_platform::frames_on(&node).expect("an export renderer on the NVIDIA GPU");
    let (w, h, n) = (640u32, 360u32, 24u32);
    let (kr, kb) = (0.2126, 0.0722);
    for depth in [8u8, 10] {
        let ten = depth == 10;
        let s = ExportSettings { format: Format::Hevc, hevc_bit_depth: depth, bitrate_kbps: 8000, bitrate_mode: BitrateMode::Vbr1Pass, ..Default::default() };
        let mut enc = filmcraft_platform::nvenc::export::linux_hevc_encoder(w, h, FrameRate::FPS_24, &s).expect("opens");
        let target = enc.gpu_target().expect("an NVENC encoder offers GPU memory");
        let before = filmcraft_platform::nvenc::gpu_input::frames_in_place();
        let (mut packets, mut refs) = (Vec::new(), Vec::new());
        for i in 0..n {
            let p = plan(w, h, i);
            let reference = gpu.yuv420(&p, kr, kb, ten, false).expect("read-back path");
            let pic =
                gpu.render_into(&p, kr, kb, ten, false, target.as_ref()).unwrap_or_else(|| panic!("{depth}-bit frame {i}: not rendered into NVENC's memory"));
            if i == 3 {
                let back = pic.read_yuv420().expect("reads back");
                assert_eq!(back.len(), reference.len());
                let step = if ten { 2 } else { 1 };
                let worst = back.chunks(step).zip(reference.chunks(step)).map(|(a, b)| {
                    let v = |c: &[u8]| if ten { i32::from(u16::from_le_bytes([c[0], c[1]])) } else { i32::from(c[0]) };
                    (v(a) - v(b)).abs()
                });
                assert!(worst.max().unwrap_or(0) <= 1, "{depth}-bit: in-place frame differs from the read-back one by more than a code");
            }
            refs.push(reference);
            let f = EncoderFrame { width: w, height: h, rgba: &[], hdr: None, yuv: None, gpu: Some(pic), index: u64::from(i) };
            packets.extend(enc.encode(&f).unwrap_or_else(|e| panic!("{depth}-bit frame {i}: {e}")));
        }
        packets.extend(enc.flush().expect("flush"));
        let in_place = filmcraft_platform::nvenc::gpu_input::frames_in_place() - before;
        assert_eq!(in_place, u64::from(n), "{depth}-bit: every frame encoded in place");
        let (decoded, worst) = worst_luma(&enc.sample_entry(), &packets, &refs, w as usize, h as usize, ten);
        eprintln!("{depth}-bit: {decoded} pictures from GPU memory, worst luma PSNR {worst:.1} dB");
        assert_eq!(decoded, n as usize);
        assert!(worst > 38.0, "{depth}-bit: worst luma PSNR {worst:.1} dB");
    }
}

/// With frames in GPU memory, NVENC lost part-way still hands its unwritten frames (read back from
/// that memory) to VA-API, and the whole stream decodes.
#[test]
fn a_lost_nvenc_replays_frames_from_gpu_memory_into_va_api() {
    use filmcraft_platform::hevc_export::{HevcEncoder, LaneTarget};
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(node) = nvidia_node().filter(|_| filmcraft_platform::nvenc::hevc_available()) else {
        eprintln!("SKIPPED: no NVIDIA GPU with an NVENC HEVC encoder");
        return;
    };
    let (w, h, n) = (640u32, 360u32, 36u32);
    let s = ExportSettings { format: Format::Hevc, bitrate_kbps: 8000, bitrate_mode: BitrateMode::Vbr1Pass, ..Default::default() };
    if HevcEncoder::for_lane(w, h, FrameRate::FPS_24, &s, LaneTarget { node: None, stream: None, in_band: true }).is_err() {
        eprintln!("SKIPPED: no in-process VA-API HEVC encoder");
        return;
    }
    let gpu = filmcraft_platform::frames_on(&node).expect("an export renderer on the NVIDIA GPU");
    let settings = s.clone();
    let then = Box::new(move || {
        HevcEncoder::for_lane(640, 360, FrameRate::FPS_24, &settings, LaneTarget { node: None, stream: None, in_band: true })
            .map(|e| Box::new(e) as Box<dyn VideoEncoder>)
    });
    let mut enc = filmcraft_platform::nvenc::export::linux_hevc_encoder_with_fallback(w, h, FrameRate::FPS_24, &s, then).expect("opens");
    assert!(enc.gpu_target().is_some(), "GPU memory");
    filmcraft_platform::nvenc::lose_device_after(20);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (mut packets, mut refs) = (Vec::new(), Vec::new());
        for i in 0..n {
            let p = plan(w, h, i);
            refs.push(gpu.yuv420(&p, 0.2126, 0.0722, false, false).expect("read-back path"));
            // as the exporter does: asked for the encoder's target each batch, else the read-back path
            let into = enc.gpu_target().and_then(|t| gpu.render_into(&p, 0.2126, 0.0722, false, false, t.as_ref()));
            assert_eq!(into.is_some(), i < 20 || enc.gpu_target().is_some(), "frame {i}: NVENC's memory until VA-API takes over");
            let bytes = refs.last().cloned().unwrap_or_default();
            let f = match into {
                Some(pic) => EncoderFrame { width: w, height: h, rgba: &[], hdr: None, yuv: None, gpu: Some(pic), index: u64::from(i) },
                None => EncoderFrame { width: w, height: h, rgba: &[], hdr: None, yuv: Some(&bytes), gpu: None, index: u64::from(i) },
            };
            packets.extend(enc.encode(&f).unwrap_or_else(|e| panic!("frame {i}: {e}")));
        }
        packets.extend(enc.flush().expect("flush"));
        (packets, refs)
    }));
    filmcraft_platform::nvenc::lose_device_after(u64::MAX);
    let (packets, refs) = result.expect("no panic, no error");
    assert_eq!(packets.len(), n as usize);
    assert!(enc.gpu_target().is_none(), "VA-API carrying on: no NVENC memory offered");
    let (decoded, worst) = worst_luma(&enc.sample_entry(), &packets, &refs, w as usize, h as usize, false);
    eprintln!("lost at 20 with GPU frames: {decoded} pictures, worst luma PSNR {worst:.1} dB");
    assert_eq!(decoded, n as usize);
    assert!(worst > 35.0, "worst luma PSNR {worst:.1} dB");
}
