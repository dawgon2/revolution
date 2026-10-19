//! Export frames rendered on the GPU (fork Phase 5.1): the [`filmcraft_export::GpuFrames`] hook
//! backed by a headless compositor (`filmcraft_gpu::GpuCompositor` on its own device, with
//! DMA-BUF import so hardware-decoded frames never leave VRAM). Created on the first exported
//! frame; a device error or a panic in the compositor turns it off for the rest of the process
//! and frames are rendered on the CPU again (logged once).
//!
//! Zero-copy (fork Phase 5.2c): when the encoder is the in-process VA-API one, frames are written
//! straight into its input surfaces ([`filmcraft_export::GpuFrames::render_into`]): the surface
//! is exported as DMA-BUF for writing, imported as two render targets, and the compositor's
//! Y'CbCr pass renders into it. Nothing is read back or uploaded and nothing waits here: the
//! [`VaPicture`] carries the GPU submission, which the encoder thread waits for before encoding.
//! If the driver cannot render into the surfaces, this is turned off (logged once) and frames
//! are read back as before. `FILMCRAFT_NO_ZERO_COPY` turns it off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_gpu::GpuCompositor;
use filmcraft_render::plan::FramePlan;

#[derive(Default)]
struct GpuExport {
    /// None: not created yet; Some(None): no usable device.
    compositor: Mutex<Option<Option<GpuCompositor>>>,
    broken: Arc<AtomicBool>,
    /// Rendering into encoder surfaces failed once: frames are read back from then on.
    no_targets: AtomicBool,
    /// The GPU's PCI ids (vendor, device); None: the high-performance adapter (fork 6E.3).
    pci: Option<(u32, u32)>,
}

/// A frame rendered into an encoder input surface ([`crate::va_enc::EncodeTarget`]).
pub struct VaPicture {
    pub surface: Arc<crate::va::Surface>,
    device: wgpu::Device,
    /// The submission writing the surface: `Pending`, then `Done`, or `Lost` (sticky: a frame
    /// whose write never finished is never encoded).
    submission: Mutex<Submission>,
    /// The frame's plan, kept until the GPU has finished with it: its decoded pictures are
    /// sampled in place, and a decoder surface released early would be decoded into again while
    /// the GPU still reads it.
    plan: Mutex<Option<FramePlan>>,
    ten_bit: bool,
    size: (u32, u32),
    broken: Arc<AtomicBool>,
}

impl VaPicture {
    /// Wait until the GPU has written the surface.
    pub fn ready(&self) -> Result<(), String> {
        let mut slot = self.submission.lock().unwrap_or_else(PoisonError::into_inner);
        if let Submission::Pending(sub) = &*slot {
            *slot = if GpuCompositor::wait_for(&self.device, sub.clone()) { Submission::Done } else { Submission::Lost };
        }
        if matches!(*slot, Submission::Lost) {
            return Err("the export GPU was lost while writing a frame".into());
        }
        drop(slot);
        drop(self.plan.lock().unwrap_or_else(PoisonError::into_inner).take());
        if self.broken.load(Ordering::Relaxed) {
            return Err("the export GPU reported an error".into());
        }
        Ok(())
    }
}

enum Submission {
    Pending(wgpu::SubmissionIndex),
    Done,
    Lost,
}

impl Drop for VaPicture {
    /// A frame dropped unencoded (export cancelled, encoder gone) still waits for its GPU work:
    /// its surface goes back to the encoder's pool and its plan's decoder surfaces to theirs.
    fn drop(&mut self) {
        let _ = self.ready();
    }
}

impl filmcraft_export::GpuPicture for VaPicture {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn read_yuv420(&self) -> Option<Vec<u8>> {
        self.ready().ok()?;
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let bps = if self.ten_bit { 2 } else { 1 };
        let mut out = vec![0u8; (w * h + 2 * cw * ch) * bps];
        let (luma, chroma) = out.split_at_mut(w * h * bps);
        let (cb, cr) = chroma.split_at_mut(cw * ch * bps);
        let ten = self.ten_bit;
        let mut ok = true;
        self.surface
            .read((0, 0, self.size.0, self.size.1), |plane, row, bytes| {
                let put = |dst: &mut [u8], i: usize, s: &[u8]| {
                    if ten {
                        let v = u16::from_le_bytes([s[0], s[1]]) >> 6;
                        if let Some(d) = dst.get_mut(i * 2..i * 2 + 2) {
                            d.copy_from_slice(&v.to_le_bytes());
                        }
                    } else if let Some(d) = dst.get_mut(i) {
                        *d = s[0];
                    }
                };
                if plane == 0 {
                    for (x, s) in bytes.chunks_exact(bps).take(w).enumerate() {
                        put(luma, row * w + x, s);
                    }
                } else if row < ch {
                    for (x, s) in bytes.chunks_exact(2 * bps).take(cw).enumerate() {
                        put(cb, row * cw + x, &s[..bps]);
                        put(cr, row * cw + x, &s[bps..]);
                    }
                } else {
                    ok = false;
                }
            })
            .ok()?;
        ok.then_some(out)
    }
}

impl GpuExport {
    fn create(&self) -> Option<GpuCompositor> {
        let (device, queue) = GpuCompositor::headless_device_on(self.pci)?;
        let flag = self.broken.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            log::error!("export GPU error, rendering export frames on the CPU from now on: {e}");
            flag.store(true, Ordering::Relaxed);
        }));
        let c = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| GpuCompositor::for_export(&device, &queue))).ok()?;
        log::info!("export frames render on the GPU");
        Some(c)
    }
}

impl GpuExport {
    /// Composite `plan` and read it back as 4:2:0 or (`chroma_422`, fork X422) 4:2:2 Y'CbCr.
    fn read_back(&self, plan: &FramePlan, kr: f32, kb: f32, ten_bit: bool, high_quality: bool, chroma_422: bool) -> Option<Vec<u8>> {
        let clock = filmcraft_export::trace::start();
        let out = self.read_back_untimed(plan, kr, kb, ten_bit, high_quality, chroma_422);
        filmcraft_export::trace::add(filmcraft_export::trace::GPU_READ_BACK, clock);
        out
    }

    fn read_back_untimed(&self, plan: &FramePlan, kr: f32, kb: f32, ten_bit: bool, high_quality: bool, chroma_422: bool) -> Option<Vec<u8>> {
        // composited and submitted under the lock; waited for and copied out after it (fork
        // X422: the read-back of a 6.2K frame was most of its time, and serial)
        let pending = {
            let mut slot = self.compositor()?;
            let c = slot.as_mut()?.as_mut()?;
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                c.set_high_quality(high_quality);
                c.composite(plan);
                c.submit_yuv(kr, kb, ten_bit, chroma_422)
            }));
            match out {
                Ok(v) if !self.broken.load(Ordering::Relaxed) => v,
                Ok(_) => None,
                Err(_) => {
                    log::error!("export compositor panicked; rendering export frames on the CPU from now on");
                    self.broken.store(true, Ordering::Relaxed);
                    *slot = Some(None);
                    None
                }
            }
        }?;
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pending.wait())).ok().flatten();
        // a GPU error raised meanwhile (uncaptured error handler) makes the frame unusable
        out.filter(|_| !self.broken.load(Ordering::Relaxed))
    }
}

impl GpuExport {
    /// Fork 8.6d D2: composite `plan` and copy it, converted to NV12 / P010, into one of NVENC's
    /// input buffers on this GPU (memory shared with CUDA). None: the frame is read back instead.
    #[cfg(target_pointer_width = "64")]
    fn render_into_nvenc(
        &self,
        plan: &FramePlan,
        kr: f32,
        kb: f32,
        ten_bit: bool,
        high_quality: bool,
        t: &crate::nvenc::gpu_input::NvencTarget,
    ) -> Option<Arc<dyn filmcraft_export::GpuPicture>> {
        if t.ten_bit != ten_bit || std::env::var_os("FILMCRAFT_NO_ZERO_COPY").is_some() {
            return None;
        }
        let clock = filmcraft_export::trace::start();
        let mut slot = self.compositor()?;
        filmcraft_export::trace::add(filmcraft_export::trace::GPU_LOCK, clock);
        let clock = filmcraft_export::trace::start();
        let c = slot.as_mut()?.as_mut()?;
        let buffer = t.buffer_on(c.device()).ok()?;
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.set_high_quality(high_quality);
            let (_, size) = c.composite(plan);
            if size != t.size {
                return None;
            }
            c.write_yuv420_into_buffer(buffer.memory.buffer(), t.pitch, kr, kb, ten_bit)
        }));
        filmcraft_export::trace::add(filmcraft_export::trace::GPU_RECORD, clock);
        match out {
            Ok(Some(sub)) if !self.broken.load(Ordering::Relaxed) => {
                Some(Arc::new(crate::nvenc::gpu_input::NvencPicture::new(buffer, sub, t, self.broken.clone())))
            }
            Ok(_) => None,
            Err(_) => {
                log::error!("export compositor panicked; rendering export frames on the CPU from now on");
                self.broken.store(true, Ordering::Relaxed);
                *slot = Some(None);
                None
            }
        }
    }
}

impl filmcraft_export::GpuFrames for GpuExport {
    fn yuv420(&self, plan: &FramePlan, kr: f32, kb: f32, ten_bit: bool, high_quality: bool) -> Option<Vec<u8>> {
        self.read_back(plan, kr, kb, ten_bit, high_quality, false)
    }

    fn yuv422(&self, plan: &FramePlan, kr: f32, kb: f32, ten_bit: bool, high_quality: bool) -> Option<Vec<u8>> {
        self.read_back(plan, kr, kb, ten_bit, high_quality, true)
    }

    fn render_into(
        &self,
        plan: &FramePlan,
        kr: f32,
        kb: f32,
        ten_bit: bool,
        high_quality: bool,
        target: &(dyn std::any::Any + Send + Sync),
    ) -> Option<Arc<dyn filmcraft_export::GpuPicture>> {
        #[cfg(target_pointer_width = "64")]
        if let Some(t) = target.downcast_ref::<crate::nvenc::gpu_input::NvencTarget>() {
            return self.render_into_nvenc(plan, kr, kb, ten_bit, high_quality, t);
        }
        let t = target.downcast_ref::<crate::va_enc::EncodeTarget>()?;
        if t.ten_bit != ten_bit || t.chroma_422 || self.no_targets.load(Ordering::Relaxed) || std::env::var_os("FILMCRAFT_NO_ZERO_COPY").is_some() {
            return None;
        }
        // the surface first: it is exported outside the compositor lock
        let surface = match t.pool.get().and_then(|s| s.export_writable().map(|e| (s, e))) {
            Ok(v) => v,
            Err(e) => {
                self.give_up_targets(&e);
                return None;
            }
        };
        let (surface, export) = surface;
        let clock = filmcraft_export::trace::start();
        let mut slot = self.compositor()?;
        filmcraft_export::trace::add(filmcraft_export::trace::GPU_LOCK, clock);
        let clock = filmcraft_export::trace::start();
        let c = slot.as_mut()?.as_mut()?;
        let mem = surface.mem().clone();
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.set_high_quality(high_quality);
            let (_, size) = c.composite(plan);
            if size != t.size {
                return Err(format!("the frame is {}x{}, the encoder {}x{}", size.0, size.1, t.size.0, t.size.1));
            }
            let sub = c
                .write_yuv420_into(mem.memory_id, mem.liveness(), &export.dmabuf, export.writable, kr, kb, ten_bit)
                .ok_or("the GPU cannot render into the encoder's surfaces")?;
            Ok((sub, c.device().clone()))
        }));
        filmcraft_export::trace::add(filmcraft_export::trace::GPU_RECORD, clock);
        // diagnostic (6E.3 graded lanes): wait for each frame's GPU work and print how long it took
        if let (Ok(Ok((sub, device))), Some(_)) = (&out, std::env::var_os("FILMCRAFT_EXPORT_GPU_PROBE")) {
            let clock = std::time::Instant::now();
            let done = GpuCompositor::wait_for(device, sub.clone());
            eprintln!("gpu-probe pci={:04x?} gpu_ms={:.1} done={done}", self.pci, clock.elapsed().as_secs_f64() * 1e3);
        }
        match out {
            Ok(Ok((sub, device))) if !self.broken.load(Ordering::Relaxed) => Some(Arc::new(VaPicture {
                surface: Arc::new(surface),
                device,
                submission: Mutex::new(Submission::Pending(sub)),
                plan: Mutex::new(Some(plan.clone())),
                ten_bit,
                size: t.size,
                broken: self.broken.clone(),
            })),
            Ok(Ok(_)) => None,
            Ok(Err(e)) => {
                self.give_up_targets(&e);
                None
            }
            Err(_) => {
                log::error!("export compositor panicked; rendering export frames on the CPU from now on");
                self.broken.store(true, Ordering::Relaxed);
                *slot = Some(None);
                None
            }
        }
    }
}

impl GpuExport {
    fn give_up_targets(&self, why: &str) {
        if !self.no_targets.swap(true, Ordering::Relaxed) {
            log::warn!("export frames are read back and uploaded to the encoder: {why}");
        }
    }

    fn compositor(&self) -> Option<std::sync::MutexGuard<'_, Option<Option<GpuCompositor>>>> {
        if self.broken.load(Ordering::Relaxed) {
            return None;
        }
        let mut slot = self.compositor.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(self.create());
        }
        Some(slot)
    }
}

/// Register the GPU export renderer (once).
pub fn register() {
    filmcraft_export::register_gpu_frames(Box::new(GpuExport::default()));
}

/// An export renderer of its own on the GPU behind render node `node` (fork 6E.3: one pipeline
/// per GPU); `None` when the node's PCI ids can't be read. Its device is made on first use, and
/// a GPU without a usable adapter renders nothing (the caller falls back to the CPU).
pub fn frames_on(node: &str) -> Option<Box<dyn filmcraft_export::GpuFrames>> {
    let pci = pci_ids(node)?;
    Some(Box::new(GpuExport { pci: Some(pci), ..Default::default() }))
}

/// The PCI (vendor, device) ids of a DRM render node (`/dev/dri/renderD128`), from sysfs.
pub fn pci_ids(node: &str) -> Option<(u32, u32)> {
    let name = std::path::Path::new(node).file_name()?.to_str()?;
    if !name.starts_with("renderD") || name.len() > 16 || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let read = |f: &str| {
        let t = std::fs::read_to_string(format!("/sys/class/drm/{name}/device/{f}")).ok()?;
        u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok()
    };
    Some((read("vendor")?, read("device")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::va_enc::{EncodeParams, RateControl, VaHevcEncoder};
    use filmcraft_export::GpuFrames;
    use filmcraft_frame::VideoFrame;
    use filmcraft_render::plan::PlanLayer;

    fn plan(w: usize, h: usize) -> FramePlan {
        let (lw, lh) = (w as u32 / 2 + 7, h as u32 / 2 + 5);
        let mut px = Vec::with_capacity((lw * lh * 4) as usize);
        for y in 0..lh {
            for x in 0..lw {
                let (fx, fy) = (x as f32 / lw as f32, y as f32 / lh as f32);
                px.extend_from_slice(&[fx, fy, (fx + fy) * 0.5, 1.0]);
            }
        }
        let layer = std::sync::Arc::new(VideoFrame::rgba_f32(lw, lh, px));
        let blend = filmcraft_render::blend::Blend::Normal;
        FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![
                PlanLayer::new(layer.clone(), filmcraft_geom::Affine::scale(2.1, 2.05), 1.0, blend),
                PlanLayer::new(layer, filmcraft_geom::Affine::translate(w as f64 / 4.0, h as f64 / 5.0), 0.6, blend),
            ],
            output: None,
        }
    }

    /// Fork Phase 5.2c: a frame rendered straight into an encoder input surface holds exactly the
    /// samples the read-back path uploads (10- and 8-bit, a picture smaller than the coded size
    /// with its padding repeating the last row), and encodes to the same bitstream. Run on AMD and
    /// Intel (`FILMCRAFT_VAAPI_DEVICE` and `MESA_VK_DEVICE_SELECT` on the same GPU). Skipped
    /// without VA-API HEVC encode or a GPU; reports when the driver cannot render into surfaces.
    /// One export renderer per GPU (fork 6E.3): each render node's GPU renders the same plan, and
    /// the GPUs agree within a code or two (10-bit). Hostile node names give `None`.
    #[test]
    fn a_renderer_on_each_gpu() {
        assert!(frames_on("/dev/dri/../../etc/passwd").is_none() && frames_on("renderD999").is_none() && pci_ids("").is_none());
        let p = plan(320, 176);
        let mut frames = Vec::new();
        for node in crate::va::render_nodes() {
            let Some(g) = frames_on(&node) else { continue };
            match g.yuv420(&p, 0.2126, 0.0722, true, false) {
                Some(f) => {
                    // fork X422: the 4:2:2 read-back, same luma, full-height chroma
                    let c422 = g.yuv422(&p, 0.2126, 0.0722, true, false).expect("4:2:2 where 4:2:0 renders");
                    let luma = 320 * 176 * 2;
                    assert_eq!(c422.len(), luma + 2 * 160 * 176 * 2, "{node}");
                    assert_eq!(c422.get(..luma), f.get(..luma), "{node}: luma differs between 4:2:0 and 4:2:2");
                    frames.push((node, f));
                }
                None => eprintln!("{node}: no usable GPU"),
            }
        }
        eprintln!("rendered on {:?}", frames.iter().map(|f| &f.0).collect::<Vec<_>>());
        if let [(_, a), rest @ ..] = frames.as_slice() {
            for (node, b) in rest {
                assert_eq!(a.len(), b.len(), "{node}");
                let at = |v: &[u8], i: usize| u16::from_le_bytes([v[i * 2], v[i * 2 + 1]]) as i32;
                let worst = (0..a.len() / 2).map(|i| (at(a, i) - at(b, i)).abs()).max().unwrap_or(0);
                assert!(worst <= 4, "{node}: up to {worst} codes from the first GPU");
            }
        }
    }

    /// Export frames render effects in full precision on every GPU, the integrated one too
    /// (FORK-PLAN §1a.64: half floats are preview-only).
    #[test]
    fn export_compositors_never_use_half_floats() {
        let mut made = 0;
        for node in crate::va::render_nodes() {
            let Some(pci) = pci_ids(&node) else { continue };
            let g = GpuExport { pci: Some(pci), ..Default::default() };
            let Some(c) = g.create() else { continue };
            assert!(!c.effects_half_precision(), "{node}: export compositor with half-float effects");
            made += 1;
        }
        eprintln!("checked {made} export compositors");
    }

    #[test]
    fn frames_rendered_into_encoder_surfaces_match_the_read_back() {
        let g = GpuExport::default();
        let (kr, kb) = (0.2126, 0.0722);
        for (w, h, ten) in [(320u32, 176u32, true), (320, 176, false), (322, 182, true)] {
            if !VaHevcEncoder::supported(ten) {
                eprintln!("no VA-API HEVC encode ({:?}); skipped", crate::va::Display::get().map(|d| d.vendor.clone()));
                return;
            }
            let p = plan(w as usize, h as usize);
            let Some(want) = g.yuv420(&p, kr, kb, ten, false) else {
                eprintln!("no GPU; skipped");
                return;
            };
            let params = EncodeParams {
                width: w,
                height: h,
                rate_num: 24,
                rate_den: 1,
                ten_bit: ten,
                keyint: 30,
                rc: RateControl::Cqp(24),
                colour: (1, 1, 1),
                speed: Default::default(),
                chroma_422: false,
                low_power: false,
            };
            let mut a = VaHevcEncoder::new(params.clone()).expect("encoder");
            let mut b = VaHevcEncoder::new(params).expect("encoder");
            let t = a.target();
            let Some(pic) = g.render_into(&p, kr, kb, ten, false, &t) else {
                eprintln!("the GPU cannot render into the encoder's surfaces (other device?); skipped");
                return;
            };
            let got = pic.read_yuv420().expect("read back");
            assert_eq!(got.len(), want.len());
            let diff = got.iter().zip(&want).filter(|(x, y)| x != y).count();
            assert_eq!(diff, 0, "{w}x{h} ten {ten}: {diff} bytes differ");
            let v = pic.as_any().downcast_ref::<VaPicture>().expect("a VA picture");
            // padding below the picture repeats the last row
            let (cw, chh) = v.surface.pool().size;
            if chh > h {
                let mut rows: Vec<Vec<u8>> = Vec::new();
                v.surface
                    .read((0, 0, cw, chh), |plane, r, bytes| {
                        if plane == 0 && (r + 1 == h as usize || r == h as usize) {
                            rows.push(bytes[..w as usize * if ten { 2 } else { 1 }].to_vec());
                        }
                    })
                    .expect("read padded");
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0], rows[1], "padding repeats the last row");
            }
            if (cw, chh) == (w, h) {
                // the same samples encode to the same stream
                let mut sa = a.encode_surface(v.surface.clone()).expect("encode surface");
                sa.extend(a.flush().expect("flush"));
                let mut sb = b.encode(&want).expect("encode bytes");
                sb.extend(b.flush().expect("flush"));
                assert_eq!(sa, sb, "{w}x{h} ten {ten}: bitstreams differ");
            }
            eprintln!("{w}x{h} ten {ten}: identical (surface {cw}x{chh})");
        }
    }
}
