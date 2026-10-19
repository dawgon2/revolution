//! Fork 8.6d D2 (the encode-side twin of 8.6c): NVENC encodes export frames from GPU memory.
//!
//! Before, an H.265 export rendered on the NVIDIA GPU read every finished frame back to the CPU
//! (~39 ms a 4K frame on the XPS 16's RTX 4050) and NVENC uploaded it again. Now the NVENC export
//! encoder offers an [`NvencTarget`] ([`filmcraft_export::VideoEncoder::gpu_target`]): the export
//! renderer copies each converted frame (NV12, or P010 for 10-bit) into one of its buffers, memory
//! its own Vulkan device allocated and CUDA imported ([`crate::nvdec::shared::EncodeBuffer`]), and
//! hands an [`NvencPicture`] over; the encoder registers the buffer's CUDA pointer with NVENC once
//! and encodes it in place. Anything refused (another GPU's device, the budget, a driver error)
//! falls back to the read-back path, frame by frame; nothing here panics.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::nvdec::shared::EncodeBuffer;

static IN_PLACE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Frames NVENC has encoded from GPU memory so far (export trace, tests).
pub fn frames_in_place() -> u64 {
    IN_PLACE.load(Ordering::Relaxed)
}

pub(crate) fn note_in_place() {
    IN_PLACE.fetch_add(1, Ordering::Relaxed);
}

/// Buffers at most: NVENC's ring (8), the batch rendering ahead, pictures a fallback keeps.
const MAX_BUFFERS: usize = 16;

/// Where an export renderer may put frames for one NVENC encoder.
pub struct NvencTarget {
    pub size: (u32, u32),
    pub ten_bit: bool,
    /// Bytes a row of either plane (a multiple of 256, wgpu's copy alignment; NVENC wants 4).
    pub pitch: u32,
    bytes: u64,
    buffers: Mutex<Vec<Arc<TargetBuffer>>>,
    /// A buffer could not be made (another GPU's device, no CUDA import): stop trying.
    refused: AtomicBool,
}

/// One buffer and the device it belongs to.
pub struct TargetBuffer {
    pub(crate) memory: EncodeBuffer,
    device: wgpu::Device,
}

impl NvencTarget {
    /// The target of a `size` encoder (`ten_bit`: P010, else NV12); None for odd or empty sizes.
    pub fn new(size: (u32, u32), ten_bit: bool) -> Option<NvencTarget> {
        let (w, h) = size;
        let row = u64::from(w).checked_mul(if ten_bit { 2 } else { 1 })?;
        let align = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let pitch = u32::try_from(row.div_ceil(align).checked_mul(align)?).ok()?;
        let bytes = u64::from(pitch).checked_mul(u64::from(h).checked_add(u64::from(h / 2))?)?;
        let ok = filmcraft_gpu::nv_buffer_layout_ok(size, pitch, ten_bit, bytes);
        ok.then(|| NvencTarget { size, ten_bit, pitch, bytes, buffers: Mutex::new(Vec::new()), refused: AtomicBool::new(false) })
    }

    /// A buffer on `device` no picture holds, made when there are fewer than [`MAX_BUFFERS`].
    pub fn buffer_on(&self, device: &wgpu::Device) -> Result<Arc<TargetBuffer>, String> {
        if self.refused.load(Ordering::Relaxed) {
            return Err("NVENC input buffers were refused earlier".into());
        }
        let mut buffers = self.buffers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(b) = buffers.iter().find(|b| Arc::strong_count(b) == 1 && b.device == *device) {
            return Ok(Arc::clone(b));
        }
        if buffers.len() >= MAX_BUFFERS {
            return Err("every NVENC input buffer is in use".into());
        }
        match EncodeBuffer::new(device, self.bytes) {
            Ok(memory) => {
                let b = Arc::new(TargetBuffer { memory, device: device.clone() });
                buffers.push(Arc::clone(&b));
                Ok(b)
            }
            Err(e) => {
                log::info!("NVENC: frames from the GPU not used ({e}); reading them back instead");
                self.refused.store(true, Ordering::Relaxed);
                Err(e)
            }
        }
    }
}

/// A frame rendered into a [`TargetBuffer`]; the GPU work may still run until [`NvencPicture::wait`].
pub struct NvencPicture {
    pub buffer: Arc<TargetBuffer>,
    submission: Mutex<Option<wgpu::SubmissionIndex>>,
    pub size: (u32, u32),
    pub ten_bit: bool,
    pub pitch: u32,
    /// The renderer's failure flag: set, the frame is not to be used.
    broken: Arc<AtomicBool>,
}

impl NvencPicture {
    pub fn new(buffer: Arc<TargetBuffer>, submission: wgpu::SubmissionIndex, target: &NvencTarget, broken: Arc<AtomicBool>) -> NvencPicture {
        NvencPicture { buffer, submission: Mutex::new(Some(submission)), size: target.size, ten_bit: target.ten_bit, pitch: target.pitch, broken }
    }

    /// Wait until the GPU has written the frame; false: lost or broken.
    pub fn wait(&self) -> bool {
        let sub = self.submission.lock().unwrap_or_else(PoisonError::into_inner).take();
        let done = match sub {
            Some(index) => self.buffer.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: None }).is_ok(),
            None => true,
        };
        done && !self.broken.load(Ordering::Relaxed)
    }
}

impl filmcraft_export::GpuPicture for NvencPicture {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// The frame as planar `yuv420p` / `yuv420p10le` (the read-back path: VA-API carrying on, CPU uses).
    fn read_yuv420(&self) -> Option<Vec<u8>> {
        if !self.wait() {
            return None;
        }
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let pitch = self.pitch as usize;
        let mut raw = Vec::new();
        raw.try_reserve_exact(pitch.checked_mul(h.checked_add(h / 2)?)?).ok()?;
        raw.resize(pitch * (h + h / 2), 0);
        self.buffer.memory.read(&mut raw).ok()?;
        Some(biplanar_to_planar(&raw, (w, h), pitch, self.ten_bit))
    }
}

/// NV12 / P010 (10 bits at the top of each little-endian 16) at `pitch` bytes a row, chroma after
/// the luma rows, as planar `yuv420p` / `yuv420p10le`.
fn biplanar_to_planar(raw: &[u8], (w, h): (usize, usize), pitch: usize, ten_bit: bool) -> Vec<u8> {
    let bytes = if ten_bit { 2 } else { 1 };
    let (cw, ch) = (w / 2, h / 2);
    let mut out = Vec::with_capacity((w * h + 2 * cw * ch) * bytes);
    let sample = |row: &[u8], i: usize| -> [u8; 2] {
        if ten_bit {
            let v = row.get(i * 2..i * 2 + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) >> 6);
            v.to_le_bytes()
        } else {
            [row.get(i).copied().unwrap_or(0), 0]
        }
    };
    let row_at = |y: usize| raw.get(y * pitch..(y * pitch + pitch).min(raw.len())).unwrap_or(&[]);
    for y in 0..h {
        let row = row_at(y);
        for x in 0..w {
            out.extend_from_slice(&sample(row, x)[..bytes]);
        }
    }
    for plane in 0..2 {
        for y in 0..ch {
            let row = row_at(h + y);
            for x in 0..cw {
                out.extend_from_slice(&sample(row, x * 2 + plane)[..bytes]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_pitched_for_wgpu_and_nvenc() {
        let t = NvencTarget::new((3840, 2160), true).expect("4K P010");
        assert_eq!(t.pitch, 7680);
        assert_eq!(t.bytes, 7680 * 3240);
        let t = NvencTarget::new((1918, 1080), false).expect("odd-ish width rounds the pitch up");
        assert_eq!(t.pitch, 2048);
        assert!(t.pitch.is_multiple_of(4));
        assert!(NvencTarget::new((641, 360), false).is_none(), "odd width");
        assert!(NvencTarget::new((0, 0), false).is_none());
    }

    #[test]
    fn biplanar_frames_unpack_to_planar() {
        // 4×2 NV12 at a pitch of 8: Y rows, then one row of interleaved Cb Cr
        let mut raw = vec![0u8; 8 * 3];
        raw[..4].copy_from_slice(&[1, 2, 3, 4]);
        raw[8..12].copy_from_slice(&[5, 6, 7, 8]);
        raw[16..20].copy_from_slice(&[10, 20, 11, 21]);
        assert_eq!(biplanar_to_planar(&raw, (4, 2), 8, false), vec![1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 20, 21]);
        // P010: 10-bit 940 stored as 940 << 6
        let mut raw = vec![0u8; 8 * 3];
        for (i, v) in [940u16, 64, 512, 513].iter().enumerate() {
            raw[i * 2..i * 2 + 2].copy_from_slice(&(v << 6).to_le_bytes());
        }
        raw[16..20].copy_from_slice(&[(512u16 << 6).to_le_bytes(), (600u16 << 6).to_le_bytes()].concat());
        let p = biplanar_to_planar(&raw, (2, 2), 8, true);
        let words: Vec<u16> = p.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
        assert_eq!(words, vec![940, 64, 0, 0, 512, 600]);
    }
}
