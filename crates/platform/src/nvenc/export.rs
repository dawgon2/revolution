//! NVENC as an Export encoder: [`factory`] is registered in front of the software H.264 encoder
//! (`filmcraft_export::register_encoder`).
//!
//! - H.264: it takes an export only when Export ▸ Hardware encoding is Auto and NVENC can do what
//!   the settings ask; otherwise it returns `None` and the software encoder runs, exactly as before.
//! - H.265 (HEVC Main 8-bit 4:2:0 SDR BT.709, or Main 10 HDR: 10-bit 4:2:0, BT.2020, PQ or HLG, limited
//!   range; progressive): there is no software encoder, so the Hardware encoding toggle does not apply
//!   (as with VideoToolbox) and choosing the format is the opt-in. The export crate hands HDR pictures
//!   over (`EncoderFrame::hdr`) only where [`super::hevc_hdr_available`] said this GPU has a 10-bit
//!   encoder, otherwise an HDR sequence is tone-mapped to SDR as before. What NVENC cannot do
//!   (interlaced, two-pass, non-square pixels, 10-bit on a GPU without it, sizes outside its
//!   limits...) is counted as declined and ends the export with an error that says why; when this
//!   machine has no HEVC encoder at all, [`filmcraft_export::available`] says so and the factory
//!   returns `None` (the export's own "encoder not available" error).
//!
//! A hardware encoder that fails in the middle of an export (a lost device) ends the export with an
//! error naming it; unlike a decoder it cannot be replayed into the software encoder, because the
//! two write different streams.

use filmcraft_export::{
    BitrateMode, ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, H264Pass, H264Profile, HardwareEncoding, Result,
    VideoEncoder,
};
use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::{Codec, Config, Nvenc, Profile, Sei, Signal};

/// The NVENC export encoder.
struct NvencEncoder {
    enc: Nvenc,
    w: u32,
    h: u32,
    rate: FrameRate,
    /// The stream's colour signal (`colr` / `mdcv` / `clli` on the sample entry for HDR).
    signal: ColorSignal,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    /// Main 10: the 10-bit planes of the current picture.
    y16: Vec<u16>,
    u16: Vec<u16>,
    v16: Vec<u16>,
    /// A GPU-rendered frame's planar 4:2:0 bytes ([`EncoderFrame::yuv`] / [`EncoderFrame::gpu`]).
    planar: Vec<u8>,
    /// A lane of a two-GPU export (fork 8.6d): VPS / SPS / PPS in every IDR picture, `hev1`.
    in_band: bool,
    /// Fork 8.6d: VA-API carries on when NVENC fails mid-export.
    #[cfg(target_os = "linux")]
    fallback: Option<super::fallback::Fallback>,
    /// Fork 8.6d D2: where the export renderer puts frames NVENC encodes in place (after `enc`, so
    /// the session, which registered its buffers, is dropped first), and each buffer's
    /// registration by buffer key; `gpu_refused` once a registration failed (read back from then on).
    #[cfg(target_os = "linux")]
    target: Option<std::sync::Arc<super::gpu_input::NvencTarget>>,
    #[cfg(target_os = "linux")]
    registered: std::collections::HashMap<u64, usize>,
    #[cfg(target_os = "linux")]
    gpu_refused: bool,
}

/// SEI payload types of the HDR10 static metadata (H.265 Annex D): mastering display colour volume
/// and content light level information.
const SEI_MASTERING_DISPLAY: u32 = 137;
const SEI_CONTENT_LIGHT_LEVEL: u32 = 144;

/// The encoder's signal for an export's colour signal: the VUI code points and, for PQ, the HDR10
/// static metadata as SEI messages (the same values and bytes the `mdcv` / `clli` boxes and the software
/// H.264 encoder's SEI carry: [`ColorSignal::static_metadata`]). HLG has no static metadata.
pub fn nvenc_signal(signal: &ColorSignal) -> Signal {
    let mut sei = Vec::new();
    if let Some((md, (max_cll, max_fall))) = signal.static_metadata() {
        sei.push(Sei { payload_type: SEI_MASTERING_DISPLAY, payload: md.to_bytes() });
        let mut cll = max_cll.to_be_bytes().to_vec();
        cll.extend_from_slice(&max_fall.to_be_bytes());
        sei.push(Sei { payload_type: SEI_CONTENT_LIGHT_LEVEL, payload: cll });
    }
    Signal { primaries: signal.primaries, transfer: signal.transfer, matrix: signal.matrix, sei }
}

fn new_encoder(enc: Nvenc, w: u32, h: u32, rate: FrameRate, signal: ColorSignal) -> NvencEncoder {
    NvencEncoder {
        enc,
        w,
        h,
        rate,
        signal,
        y: Vec::new(),
        u: Vec::new(),
        v: Vec::new(),
        y16: Vec::new(),
        u16: Vec::new(),
        v16: Vec::new(),
        planar: Vec::new(),
        in_band: false,
        #[cfg(target_os = "linux")]
        fallback: None,
        #[cfg(target_os = "linux")]
        target: None,
        #[cfg(target_os = "linux")]
        registered: std::collections::HashMap::new(),
        #[cfg(target_os = "linux")]
        gpu_refused: false,
    }
}

impl NvencEncoder {
    /// A frame rendered on the GPU (the Linux H.265 export: [`EncoderFrame::yuv`] or
    /// [`EncoderFrame::gpu`], `rgba` empty) as the planar 4:2:0 of the export's signal, into the
    /// encoder's planes (fork XPS item 6: these frames reached NVENC as an empty RGBA picture and
    /// every NVENC H.265 export on Linux failed at its first frame).
    fn encode_planar(&mut self, f: &EncoderFrame) -> std::result::Result<Vec<super::Packet>, String> {
        self.fill_planar(f)?;
        self.submit_planar(f.index)
    }

    /// The frame as the planar 4:2:0 of the export's signal, into `planar` (checked for size).
    fn fill_planar(&mut self, f: &EncoderFrame) -> std::result::Result<(), String> {
        let size = self.planar_size();
        let (w, h, ten_bit) = (self.w, self.h, self.enc.is_ten_bit());
        // a converted frame of another depth or size would be re-made from the (empty) RGBA picture
        if let Some(yuv) = f.yuv
            && yuv.len() != size
        {
            return Err(format!("a {} byte 4:2:0 frame for a {w}x{h} {}-bit encoder", yuv.len(), if ten_bit { 10 } else { 8 }));
        }
        filmcraft_export::timed(filmcraft_export::Stage::Convert, || filmcraft_export::frame_to_yuv420p(f, self.signal, ten_bit, &mut self.planar));
        if self.planar.len() < size {
            return Err(format!("a {} byte GPU frame for a {w}x{h} encoder", self.planar.len()));
        }
        Ok(())
    }

    fn planar_size(&self) -> usize {
        let (w, h) = (self.w as usize, self.h as usize);
        let (luma, chroma) = (w.saturating_mul(h), (w / 2).saturating_mul(h / 2));
        let bytes = if self.enc.is_ten_bit() { 2 } else { 1 };
        luma.saturating_add(chroma.saturating_mul(2)).saturating_mul(bytes)
    }

    /// Picture `index` from `planar` into NVENC.
    fn submit_planar(&mut self, index: u64) -> std::result::Result<Vec<super::Packet>, String> {
        let ten_bit = self.enc.is_ten_bit();
        let (w, h) = (self.w as usize, self.h as usize);
        let (luma, chroma) = (w.saturating_mul(h), (w / 2).saturating_mul(h / 2));
        let bytes = if ten_bit { 2 } else { 1 };
        let (y, rest) = self.planar.split_at_checked(luma * bytes).ok_or("a short GPU frame")?;
        let (u, rest) = rest.split_at_checked(chroma * bytes).ok_or("a short GPU frame")?;
        let v = rest.get(..chroma * bytes).ok_or("a short GPU frame")?;
        if ten_bit {
            let words = |b: &[u8], out: &mut Vec<u16>| {
                out.clear();
                out.extend(b.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)));
            };
            words(y, &mut self.y16);
            words(u, &mut self.u16);
            words(v, &mut self.v16);
            self.enc.encode_10(&self.y16, &self.u16, &self.v16, index)
        } else {
            self.enc.encode(y, u, v, index)
        }
    }

    /// Fork 8.6d, with a fallback: every frame goes in planar (its copy kept until written); NVENC
    /// failing hands the frames not written yet, and every frame after, to VA-API.
    #[cfg(target_os = "linux")]
    fn encode_with_fallback(&mut self, f: &EncoderFrame) -> Option<Result<Vec<EncodedPacket>>> {
        self.fallback.as_ref()?;
        Some(self.encode_or_take_over(f))
    }

    #[cfg(target_os = "linux")]
    fn encode_or_take_over(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let Some(fb) = self.fallback.as_mut() else { return Err(ExportError::Encode("NVENC: no fallback".into())) };
        if let Some(cont) = fb.continuation() {
            let ps = cont.encode(f)?;
            return Ok(fb.shifted(ps));
        }
        // fork 8.6d D2: a frame in GPU memory is kept as it is (no read-back unless VA-API takes over)
        if let Some(gpu) = self.own_gpu_picture(f) {
            let fb = self.fallback.as_mut().ok_or_else(|| ExportError::Encode("NVENC: no fallback".into()))?;
            fb.submitted(f.index, super::fallback::Kept::Gpu(std::sync::Arc::clone(&gpu))).map_err(|e| ExportError::Encode(format!("NVENC: {e}")))?;
            let r = self.encode_gpu_picture(&gpu, f.index);
            if r.is_ok() {
                filmcraft_export::note_hw_encode_frame();
            }
            return self.release_or_take_over(r);
        }
        self.fill_planar(f).map_err(|e| ExportError::Encode(format!("NVENC: {e}")))?;
        let copy = super::fallback::Kept::Planar(self.planar.clone());
        let fb = self.fallback.as_mut().ok_or_else(|| ExportError::Encode("NVENC: no fallback".into()))?;
        fb.submitted(f.index, copy).map_err(|e| ExportError::Encode(format!("NVENC: {e}")))?;
        let r = self.submit_planar(f.index);
        if r.is_ok() {
            filmcraft_export::note_hw_encode_frame();
        }
        self.release_or_take_over(r)
    }

    /// Fork 8.6d D2: the frame when it is one of this encoder's NVENC pictures (rendered into its
    /// target's memory, the right size and depth) and registration still works.
    #[cfg(target_os = "linux")]
    fn own_gpu_picture(&self, f: &EncoderFrame) -> Option<std::sync::Arc<dyn filmcraft_export::GpuPicture>> {
        let gpu = f.gpu.as_ref()?;
        let pic = gpu.as_any().downcast_ref::<super::gpu_input::NvencPicture>()?;
        let ok = !self.gpu_refused && self.target.is_some() && pic.size == (self.w, self.h) && pic.ten_bit == self.enc.is_ten_bit();
        ok.then(|| std::sync::Arc::clone(gpu))
    }

    /// Encode a frame from GPU memory in place: wait for the GPU, register its buffer once, encode
    /// it. A registration NVENC refuses reads this frame back (and every later one).
    #[cfg(target_os = "linux")]
    fn encode_gpu_picture(&mut self, gpu: &std::sync::Arc<dyn filmcraft_export::GpuPicture>, index: u64) -> std::result::Result<Vec<super::Packet>, String> {
        let pic = gpu.as_any().downcast_ref::<super::gpu_input::NvencPicture>().ok_or("not an NVENC picture")?;
        if !pic.wait() {
            return Err("the frame was lost on the GPU".into());
        }
        let key = pic.buffer.memory.key();
        let reg = match self.registered.get(&key) {
            Some(r) => Ok(*r),
            None => self.enc.register_cuda(pic.buffer.memory.pointer(), pic.pitch).inspect(|r| {
                self.registered.insert(key, *r);
            }),
        };
        match reg {
            Ok(reg) => {
                let keep: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(std::sync::Arc::clone(gpu));
                let r = self.enc.encode_registered(reg, pic.pitch, index, keep);
                if r.is_ok() {
                    super::gpu_input::note_in_place();
                }
                r
            }
            Err(e) => {
                log::info!("NVENC: frames from the GPU not used ({e}); reading them back instead");
                self.gpu_refused = true;
                self.planar = filmcraft_export::GpuPicture::read_yuv420(pic).ok_or("the frame could not be read back")?;
                self.submit_planar(index)
            }
        }
    }

    /// NVENC's answer: the packets that can be written, or VA-API's after it took over.
    #[cfg(target_os = "linux")]
    fn release_or_take_over(&mut self, r: std::result::Result<Vec<super::Packet>, String>) -> Result<Vec<EncodedPacket>> {
        let ps = match r {
            Ok(ps) => ps,
            Err(why) => {
                let fb = self.fallback.as_mut().ok_or_else(|| ExportError::Encode(format!("NVENC: {why}")))?;
                return fb.take_over(&why);
            }
        };
        let order: Vec<u64> = ps.iter().map(|p| u64::try_from(p.pts).unwrap_or(0)).collect();
        let numbered: Vec<(u64, EncodedPacket)> = order.into_iter().zip(self.packets(ps)).collect();
        match self.fallback.as_mut() {
            Some(fb) => Ok(fb.came_out(numbered)),
            None => Ok(numbered.into_iter().map(|(_, p)| p).collect()),
        }
    }

    fn packets(&self, ps: Vec<super::Packet>) -> Vec<EncodedPacket> {
        let den = self.rate.den;
        let duration = u32::try_from(den).unwrap_or(u32::MAX);
        ps.into_iter()
            .map(|p| {
                let data = if self.in_band && p.key { self.with_parameter_sets(p.data) } else { p.data };
                EncodedPacket { data, key: p.key, duration, composition_offset: composition_offset(p.pts, p.dts, den) }
            })
            .collect()
    }

    /// An IDR picture's NAL units behind the stream's VPS / SPS / PPS (length-prefixed, as the
    /// picture's are): each chunk of a two-GPU export carries its own (`hev1`, fork 8.6d).
    fn with_parameter_sets(&self, data: Vec<u8>) -> Vec<u8> {
        let (sps, pps) = self.enc.parameter_sets();
        let sets = [self.enc.vps(), sps, pps];
        let mut out = Vec::with_capacity(data.len().saturating_add(sets.iter().map(|n| n.len().saturating_add(4)).sum()));
        for nal in sets.into_iter().filter(|n| !n.is_empty()) {
            // parameter sets are a few dozen bytes
            out.extend_from_slice(&u32::try_from(nal.len()).unwrap_or(u32::MAX).to_be_bytes());
            out.extend_from_slice(nal);
        }
        out.extend(data);
        out
    }
}

/// `(pts - dts) × den` as an MP4 composition offset. It is a few frames (the B-frame delay); a
/// hostile value saturates instead of wrapping.
fn composition_offset(pts: i64, dts: i64, den: i64) -> i32 {
    let offset = pts.saturating_sub(dts).saturating_mul(den);
    i32::try_from(offset).unwrap_or(if offset < 0 { i32::MIN } else { i32::MAX })
}

impl VideoEncoder for NvencEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let (sps, pps) = self.enc.parameter_sets();
        // `config` declines sizes above u16::MAX
        let (w, h) = (u16::try_from(self.w).unwrap_or(u16::MAX), u16::try_from(self.h).unwrap_or(u16::MAX));
        match self.enc.codec() {
            Codec::H264 => SampleEntry::avc(AvcConfig::new(vec![sps.to_vec()], vec![pps.to_vec()], 4), w, h),
            Codec::Hevc => {
                // `Nvenc::new` built and validated the record, so there is always one for HEVC
                let config = self.enc.hevc_config().cloned().unwrap_or_default();
                let mut e = if self.in_band { SampleEntry::hevc_inband(config, w, h) } else { SampleEntry::hevc(config, w, h) };
                // HDR: `colr` (BT.2020 + PQ / HLG) and, for PQ, `mdcv` / `clli`, as the software H.264 encoder does
                // (nclx, whatever the container: readers take it from either). SDR stays as it was, with the VUI only.
                self.signal.apply_to(&mut e, false);
                e
            }
        }
    }

    fn timescale(&self) -> u32 {
        u32::try_from(self.rate.num).unwrap_or(u32::MAX)
    }

    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        if f.width != self.w || f.height != self.h {
            return Err(ExportError::Encode(format!("NVENC: a {}x{} picture for a {}x{} encoder", f.width, f.height, self.w, self.h)));
        }
        #[cfg(target_os = "linux")]
        if let Some(r) = self.encode_with_fallback(f) {
            return r;
        }
        let (w, h) = (self.w as usize, self.h as usize);
        let hint = if self.enc.codec() == Codec::H264 { " (turn Export ▸ Hardware encoding off to use the software encoder)" } else { "" };
        // fork 8.6d D2: rendered into this encoder's GPU memory: encoded in place
        #[cfg(target_os = "linux")]
        if let Some(gpu) = self.own_gpu_picture(f) {
            let ps = self.encode_gpu_picture(&gpu, f.index).map_err(|e| ExportError::Encode(format!("NVENC: {e}")))?;
            filmcraft_export::note_hw_encode_frame();
            return Ok(self.packets(ps));
        }
        // rendered on the GPU (Linux H.265): planar 4:2:0 as is; a planar-input encoder takes it
        if (f.yuv.is_some() || f.gpu.is_some()) && !self.enc.takes_rgba() {
            let ps = self.encode_planar(f).map_err(|e| ExportError::Encode(format!("NVENC: {e}{hint}")))?;
            filmcraft_export::note_hw_encode_frame();
            return Ok(self.packets(ps));
        }
        let ps = match (self.enc.is_ten_bit(), f.hdr) {
            // Main 10 takes the encoded R'G'B' floats: 10-bit limited-range BT.2020 NCL (or the signal's matrix)
            (true, Some(rgb)) => {
                let (kr, kb) = self.signal.kr_kb();
                filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
                    filmcraft_export::rgbf_to_yuv420_10(rgb, w, h, kr, kb, &mut self.y16, &mut self.u16, &mut self.v16)
                })?;
                self.enc.encode_10(&self.y16, &self.u16, &self.v16, f.index).map_err(|e| ExportError::Encode(format!("NVENC: {e}{hint}")))?
            }
            // 10-bit SDR from an 8-bit picture (XPS item 7): its 10-bit codes, as the other encoders make them
            (true, None) => self.encode_planar(f).map_err(|e| ExportError::Encode(format!("NVENC: {e}")))?,
            (false, Some(_)) => return Err(ExportError::Unsupported("NVENC Main (8-bit) does not take HDR pictures".into())),
            // the GPU converts the RGBA picture itself (BT.709 limited range, the same codes as `rgba_to_yuv420_8`)
            (false, None) if self.enc.takes_rgba() => self.enc.encode_rgba(f.rgba, f.index).map_err(|e| ExportError::Encode(format!("NVENC: {e}{hint}")))?,
            (false, None) => {
                filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
                    filmcraft_export::rgba_to_yuv420_8(f.rgba, w, h, &mut self.y, &mut self.u, &mut self.v)
                });
                self.enc.encode(&self.y, &self.u, &self.v, f.index).map_err(|e| ExportError::Encode(format!("NVENC: {e}{hint}")))?
            }
        };
        filmcraft_export::note_hw_encode_frame();
        Ok(self.packets(ps))
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        #[cfg(target_os = "linux")]
        if let Some(fb) = self.fallback.as_mut() {
            if let Some(cont) = fb.continuation() {
                let ps = cont.flush()?;
                return Ok(fb.shifted(ps));
            }
            let r = self.enc.flush();
            let mut out = self.release_or_take_over(r)?;
            // NVENC failed while finishing: VA-API finishes its replay
            if let Some(fb) = self.fallback.as_mut()
                && let Some(cont) = fb.continuation()
            {
                let ps = cont.flush()?;
                out.extend(fb.shifted(ps));
            }
            return Ok(out);
        }
        let ps = self.enc.flush().map_err(|e| ExportError::Encode(format!("NVENC: {e}")))?;
        Ok(self.packets(ps))
    }

    fn media_start(&self) -> Option<i64> {
        // with B-frames the first DTS is `delay` frames before the first PTS
        (self.enc.delay() > 0).then_some(i64::from(self.enc.delay()).saturating_mul(self.rate.den))
    }

    #[cfg(target_os = "linux")]
    fn gpu_target(&self) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        // (VA-API carrying on reads frames back: NVENC's memory is no use to it)
        if self.fallback.as_ref().is_some_and(|f| f.has_taken_over()) {
            return None;
        }
        let target = self.target.as_ref().filter(|_| !self.gpu_refused)?;
        Some(std::sync::Arc::clone(target) as std::sync::Arc<dyn std::any::Any + Send + Sync>)
    }
}

/// Why NVENC does not take this export (the software encoder does, for H.264), or the encoder's
/// configuration.
fn config(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Config, String> {
    let hevc = format == Format::Hevc;
    if !(format == Format::H264 || hevc) || s.format.is_mxf() {
        return Err("only MP4 / MOV H.264 and H.265 exports".into());
    }
    let hdr = s.signal.is_hdr();
    if hdr && !hevc {
        return Err("HDR (8-bit H.264 here carries it with software signalling)".into());
    }
    // HEVC: BT.709 SDR (Main) or BT.2020 NCL with PQ / HLG (Main 10); any other description is not written
    if hevc && (hdr && (s.signal.primaries != 9 || s.signal.matrix != 9) || !hdr && s.signal != ColorSignal::default()) {
        return Err(format!("the colour signal {:?} (H.265 here is BT.709, or BT.2020 with PQ / HLG)", s.signal));
    }
    if !matches!(s.h264_pass, H264Pass::Single) || (hevc && s.bitrate_mode == BitrateMode::Vbr2Pass) {
        return Err("two-pass VBR".into());
    }
    if s.bitrate_mode == BitrateMode::Crf {
        return Err("CRF (constant quality)".into());
    }
    // the hardware path writes the aspect ratio of H.264 only
    if hevc && s.pixel_aspect.is_some_and(|(n, d)| n != d) {
        return Err("non-square pixels".into());
    }
    if s.field_order != FieldOrder::Progressive {
        return Err("interlaced output".into());
    }
    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err("NVENC needs nonzero even dimensions".into());
    }
    if w > u32::from(u16::MAX) || h > u32::from(u16::MAX) {
        return Err(format!("{w}x{h} does not fit an MP4 sample entry"));
    }
    if rate.num <= 0 || rate.den <= 0 || rate.num > i64::from(u32::MAX) || rate.den > i64::from(u32::MAX) {
        return Err("frame rate".into());
    }
    let (Ok(num), Ok(den)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else {
        return Err("frame rate".into());
    };
    let fps = (num, den);
    let kbps = s.bitrate_kbps.max(100);
    Ok(Config {
        width: w,
        height: h,
        fps,
        bitrate_kbps: kbps,
        max_bitrate_kbps: s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or_else(|| kbps.saturating_add(kbps / 2)),
        cbr: s.bitrate_mode == BitrateMode::Cbr,
        keyint: s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (f64::from(fps.0) / f64::from(fps.1) * 2.0).round().max(1.0) as u32),
        profile: match (hevc, s.h264_profile) {
            // HDR, or 10-bit asked for (XPS item 7: a 10-bit SDR export came out 8-bit)
            (true, _) if hdr || s.hevc_bit_depth == 10 => Profile::HevcMain10,
            (true, _) => Profile::HevcMain,
            (false, H264Profile::Baseline) => Profile::Baseline,
            (false, H264Profile::Main) => Profile::Main,
            (false, H264Profile::High) => Profile::High,
        },
        // the H.264 level setting means nothing for H.265: the encoder picks the level
        level: if hevc { None } else { s.h264_level },
        sar: s.pixel_aspect,
        bframes: true,
        preset: nv_preset(s.hw_encode_speed),
    })
}

/// The NVENC preset for Export ▸ Hardware encoder speed (fork 6E.2 / XPS b): the driver default
/// is P5 (FilmCraft's choice before), Speed P1, Balanced P4, Quality P7, by the same seven-step
/// scale as VA-API's quality levels (1 = best there, so level `l` is preset `8 − l`).
fn nv_preset(speed: filmcraft_export::HwEncodeSpeed) -> u8 {
    speed.level(7).and_then(|l| u8::try_from(8u32.saturating_sub(l)).ok()).filter(|p| (1..=7).contains(p)).unwrap_or(5)
}

/// The Export encoder factory (see the module documentation).
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format == Format::Hevc {
        // H.265 through NVENC is Windows-only for now; on Linux this backend takes H.264 only
        return if cfg!(target_os = "windows") { hevc_factory(w, h, rate, s) } else { None };
    }
    factory_with(format, w, h, rate, s, |cfg| open(cfg, &Signal::default()))
}

/// Open the encoder for `cfg`. An 8-bit one takes the export's RGBA pictures as they are and converts
/// them on the GPU, which keeps the RGB → 4:2:0 conversion off the CPU (where it competes with the
/// render of the next frames); where the driver refuses RGB input, it takes the CPU's 4:2:0 as before.
/// Main 10 takes its 10-bit planes.
fn open(cfg: &Config, signal: &Signal) -> std::result::Result<Nvenc, String> {
    if cfg.profile == Profile::HevcMain10 {
        return Nvenc::with_signal(cfg, signal);
    }
    Nvenc::with_rgba_input(cfg, signal).or_else(|why| {
        log::info!("NVENC RGBA input declined ({why}): converting to 4:2:0 on the CPU");
        Nvenc::with_signal(cfg, signal)
    })
}

/// The H.264 side of [`factory`], with the driver call (`open`) passed in so tests can stand in for it.
fn factory_with(
    format: Format,
    w: u32,
    h: u32,
    rate: FrameRate,
    s: &ExportSettings,
    open: impl FnOnce(&Config) -> std::result::Result<Nvenc, String>,
) -> Option<Result<Box<dyn VideoEncoder>>> {
    if s.hardware_encoding != HardwareEncoding::Auto || format != Format::H264 {
        return None;
    }
    let declined = |why: &str| {
        log::info!("hardware encoding declined: {why}");
        filmcraft_export::note_hw_encode_declined();
        None
    };
    let cfg = match config(format, w, h, rate, s) {
        Ok(c) => c,
        Err(why) => return declined(&why),
    };
    match open(&cfg) {
        Ok(enc) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(new_encoder(enc, w, h, rate, s.signal))))
        }
        Err(why) => declined(&why),
    }
}

/// Why NVENC does not take this H.265 export on Linux, or None. Main (8-bit) and Main 10 (10-bit
/// SDR, PQ, HLG) 4:2:0 go in since the XPS 16 runs (RTX 4050: files checked with ffprobe and
/// against the source, FORK-PLAN §7); 4:2:2 does not (NVIDIA encodes it only from Blackwell on, and
/// the export's 4:2:2 path is VA-API's).
#[cfg(target_os = "linux")]
fn linux_hevc_declines(s: &ExportSettings) -> Option<&'static str> {
    s.hevc_chroma_422.then_some("NVENC on Linux encodes 4:2:0 H.265 (Main / Main 10) here, not 4:2:2")
}

/// Fork XPS b / XPS 7: NVENC H.265 for the Linux HEVC export ([`crate::hevc_export`], which decides
/// when it is tried): Main and Main 10 4:2:0, SDR and HDR ([`linux_hevc_declines`]). `Err`: why not
/// (the caller tries its next encoder).
#[cfg(target_os = "linux")]
pub fn linux_hevc_encoder(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Box<dyn VideoEncoder>, String> {
    if let Some(why) = linux_hevc_declines(s) {
        return Err(why.into());
    }
    let cfg = config(Format::Hevc, w, h, rate, s)?;
    // planar input: Linux H.265 exports render each frame on the GPU and hand it over as 4:2:0
    // (`EncoderFrame::yuv` / `gpu`); a CPU-rendered RGBA frame is converted by `encode`
    let enc = Nvenc::with_signal(&cfg, &nvenc_signal(&s.signal))?;
    filmcraft_export::note_hw_encode_session();
    let target = gpu_target_for(&enc, w, h);
    Ok(Box::new(NvencEncoder { target, ..new_encoder(enc, w, h, rate, s.signal) }))
}

/// Fork 8.6d D2: the GPU-memory target of a planar-input session (`FILMCRAFT_NO_ZERO_COPY` turns
/// it off, as for VA-API's).
#[cfg(target_os = "linux")]
fn gpu_target_for(enc: &Nvenc, w: u32, h: u32) -> Option<std::sync::Arc<super::gpu_input::NvencTarget>> {
    if enc.takes_rgba() || std::env::var_os("FILMCRAFT_NO_ZERO_COPY").is_some() {
        return None;
    }
    super::gpu_input::NvencTarget::new((w, h), enc.is_ten_bit()).map(std::sync::Arc::new)
}

/// Fork 8.6d: [`linux_hevc_encoder`] for one lane of a two-GPU export: no B-frames (every lane's
/// chunks must keep the same timing, and VA-API's have no reordering) and VPS / SPS / PPS in every
/// IDR picture (`hev1`: NVENC's parameter sets are not VA-API's).
#[cfg(target_os = "linux")]
pub fn linux_hevc_lane_encoder(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Box<dyn VideoEncoder>, String> {
    if let Some(why) = linux_hevc_declines(s) {
        return Err(why.into());
    }
    let cfg = Config { bframes: false, ..config(Format::Hevc, w, h, rate, s)? };
    let enc = Nvenc::with_signal(&cfg, &nvenc_signal(&s.signal))?;
    filmcraft_export::note_hw_encode_session();
    let target = gpu_target_for(&enc, w, h);
    Ok(Box::new(NvencEncoder { in_band: true, target, ..new_encoder(enc, w, h, rate, s.signal) }))
}

/// Fork 8.6d: [`linux_hevc_encoder`] that carries on in the encoder `then` makes (VA-API in
/// process, parameter sets in band) when NVENC fails part-way ([`super::fallback`]).
#[cfg(target_os = "linux")]
pub fn linux_hevc_encoder_with_fallback(
    w: u32,
    h: u32,
    rate: FrameRate,
    s: &ExportSettings,
    then: super::fallback::MakeContinuation,
) -> std::result::Result<Box<dyn VideoEncoder>, String> {
    if let Some(why) = linux_hevc_declines(s) {
        return Err(why.into());
    }
    let cfg = config(Format::Hevc, w, h, rate, s)?;
    let enc = Nvenc::with_signal(&cfg, &nvenc_signal(&s.signal))?;
    filmcraft_export::note_hw_encode_session();
    let den = i32::try_from(rate.den).unwrap_or(i32::MAX);
    let shift = i32::try_from(enc.delay()).unwrap_or(i32::MAX).saturating_mul(den);
    let fallback = super::fallback::Fallback::new(then, w, h, shift);
    let target = gpu_target_for(&enc, w, h);
    Ok(Box::new(NvencEncoder { fallback: Some(fallback), target, ..new_encoder(enc, w, h, rate, s.signal) }))
}

/// The H.265 side of [`factory`]: FilmCraft's only HEVC encoder on Windows, so the Hardware encoding toggle
/// does not apply and a request NVENC cannot take is an error, not a fall-through.
fn hevc_factory(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    let declined = |why: &str| {
        log::info!("hardware H.265 encoding declined: {why}");
        filmcraft_export::note_hw_encode_declined();
        Some(Err(ExportError::Unsupported(format!("H.265 export with NVENC: {why}"))))
    };
    let cfg = match config(Format::Hevc, w, h, rate, s) {
        Ok(c) => c,
        Err(why) => return declined(&why),
    };
    match open(&cfg, &nvenc_signal(&s.signal)) {
        Ok(enc) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(new_encoder(enc, w, h, rate, s.signal))))
        }
        // no HEVC encoder here at all: not a hardware attempt, the export's own "encoder not available" error
        Err(why) if !super::hevc_available() => {
            log::info!("no hardware H.265 encoder: {why}");
            None
        }
        Err(why) => declined(&why),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hardware_encoder_speed_picks_the_nvenc_preset() {
        use filmcraft_export::HwEncodeSpeed as S;
        assert_eq!(nv_preset(S::Default), 5);
        assert_eq!(nv_preset(S::Speed), 1);
        assert_eq!(nv_preset(S::Balanced), 4);
        assert_eq!(nv_preset(S::Quality), 7);
        let cfg =
            |speed| config(Format::Hevc, 1920, 1080, FrameRate::FPS_24, &ExportSettings { hw_encode_speed: speed, ..Default::default() }).map(|c| c.preset);
        assert_eq!(cfg(S::Speed), Ok(1));
        assert_eq!(cfg(S::Default), Ok(5));
    }

    use super::*;

    /// XPS 7: on Linux NVENC takes 10-bit and HDR H.265 (Main 10), never 4:2:2.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_nvenc_takes_main_10_and_hdr_but_not_4_2_2() {
        let base = ExportSettings::default();
        assert_eq!(linux_hevc_declines(&base), None, "8-bit SDR");
        assert_eq!(linux_hevc_declines(&ExportSettings { hevc_bit_depth: 10, ..base.clone() }), None, "10-bit SDR");
        for signal in [ColorSignal::PQ, ColorSignal::HLG] {
            assert_eq!(linux_hevc_declines(&ExportSettings { hevc_bit_depth: 10, signal, ..base.clone() }), None, "{signal:?}");
        }
        assert!(linux_hevc_declines(&ExportSettings { hevc_bit_depth: 10, hevc_chroma_422: true, ..base }).is_some(), "4:2:2");
    }

    #[test]
    fn off_and_unsupported_settings_never_open_the_driver() {
        let declined = |format, w, h, s: &ExportSettings| {
            assert!(factory_with(format, w, h, FrameRate::FPS_24, s, |_| panic!("must not open the driver")).is_none());
        };
        let mut s = ExportSettings { hardware_encoding: HardwareEncoding::Off, ..Default::default() };
        declined(Format::H264, 640, 360, &s);
        s.hardware_encoding = HardwareEncoding::Auto;
        declined(Format::ProRes, 640, 360, &s);
        declined(Format::H264, 641, 360, &s);
        declined(Format::H264, 0, 360, &s);
        s.h264_pass = H264Pass::First;
        declined(Format::H264, 640, 360, &s);
        s.h264_pass = H264Pass::Single;
        s.field_order = FieldOrder::UpperFirst;
        declined(Format::H264, 640, 360, &s);
    }

    #[test]
    fn missing_or_rejecting_driver_declines_for_software_fallback() {
        let s = ExportSettings { hardware_encoding: HardwareEncoding::Auto, ..Default::default() };
        let mut attempted = false;
        assert!(
            factory_with(Format::H264, 640, 360, FrameRate::FPS_24, &s, |cfg| {
                attempted = true;
                assert_eq!((cfg.width, cfg.height), (640, 360));
                Err("no NVIDIA encoder driver".into())
            })
            .is_none()
        );
        assert!(attempted);
    }

    #[test]
    fn bitrate_ceiling_does_not_wrap() {
        let s = ExportSettings { bitrate_kbps: u32::MAX, ..Default::default() };
        assert_eq!(config(Format::H264, 640, 360, FrameRate::FPS_24, &s).unwrap().max_bitrate_kbps, u32::MAX);
    }

    #[test]
    fn composition_offsets_saturate() {
        assert_eq!(composition_offset(3, 1, 1001), 2002);
        assert_eq!(composition_offset(0, -2, 1001), 2002);
        assert_eq!(composition_offset(i64::MAX, i64::MIN, 1001), i32::MAX);
        assert_eq!(composition_offset(i64::MIN, i64::MAX, 1001), i32::MIN);
        assert_eq!(composition_offset(1 << 40, 0, 1), i32::MAX);
    }

    #[test]
    fn hevc_takes_what_nvenc_can_write_and_declines_the_rest() {
        let ok = ExportSettings { format: Format::Hevc, bitrate_kbps: 4000, keyframe_distance: Some(48), ..Default::default() };
        let c = config(Format::Hevc, 1280, 720, FrameRate::FPS_24, &ok).unwrap();
        assert_eq!((c.profile, c.level, c.keyint, c.fps), (Profile::HevcMain, None, 48, (24, 1)));
        // the H.264 level and profile settings mean nothing to HEVC
        let h = ExportSettings { h264_level: Some(41), h264_profile: H264Profile::Baseline, ..ok.clone() };
        assert_eq!(config(Format::Hevc, 1280, 720, FrameRate::FPS_24, &h).unwrap().level, None);
        // HDR (PQ / HLG, BT.2020) and 10-bit SDR are Main 10, 8-bit SDR stays Main; H.264 still declines HDR
        let ten = ExportSettings { hevc_bit_depth: 10, ..ok.clone() };
        assert_eq!(config(Format::Hevc, 1280, 720, FrameRate::FPS_24, &ten).unwrap().profile, Profile::HevcMain10);
        for signal in [ColorSignal::PQ, ColorSignal::HLG] {
            let hdr = ExportSettings { signal, ..ok.clone() };
            assert_eq!(config(Format::Hevc, 1280, 720, FrameRate::FPS_24, &hdr).unwrap().profile, Profile::HevcMain10);
            assert!(config(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { format: Format::H264, ..hdr }).is_err());
        }
        // the Hardware encoding toggle does not matter for HEVC (it has no software encoder)
        let off = ExportSettings { hardware_encoding: HardwareEncoding::Off, ..ok.clone() };
        assert!(config(Format::Hevc, 1280, 720, FrameRate::FPS_24, &off).is_ok());
        for (what, s) in [
            ("an unwritable colour signal", ExportSettings { signal: ColorSignal { primaries: 9, transfer: 1, matrix: 1 }, ..ok.clone() }),
            ("HDR with BT.709 primaries", ExportSettings { signal: ColorSignal { primaries: 1, transfer: 16, matrix: 9 }, ..ok.clone() }),
            ("two-pass", ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..ok.clone() }),
            ("CRF", ExportSettings { bitrate_mode: BitrateMode::Crf, ..ok.clone() }),
            ("analysis pass", ExportSettings { h264_pass: H264Pass::First, ..ok.clone() }),
            ("interlaced", ExportSettings { field_order: FieldOrder::UpperFirst, ..ok.clone() }),
            ("non-square pixels", ExportSettings { pixel_aspect: Some((4, 3)), ..ok.clone() }),
            ("MXF", ExportSettings { format: Format::MxfOp1a, ..ok.clone() }),
        ] {
            assert!(config(Format::Hevc, 1280, 720, FrameRate::FPS_24, &s).is_err(), "{what}");
        }
        // hostile sizes and frame rates
        assert!(config(Format::Hevc, 70_000, 64, FrameRate::FPS_24, &ok).is_err());
        assert!(config(Format::Hevc, 64, 70_000, FrameRate::FPS_24, &ok).is_err());
        for rate in [FrameRate { num: 0, den: 1 }, FrameRate { num: 24, den: 0 }, FrameRate { num: -24, den: 1 }, FrameRate { num: i64::MAX, den: 1 }] {
            assert!(config(Format::Hevc, 1280, 720, rate, &ok).is_err(), "{rate:?}");
        }
        // other formats are not NVENC's
        assert!(config(Format::ProRes, 1280, 720, FrameRate::FPS_24, &ok).is_err());
    }

    #[test]
    fn the_sei_messages_are_the_hdr10_static_metadata_the_software_h264_path_writes() {
        let pq = nvenc_signal(&ColorSignal::PQ);
        assert_eq!((pq.primaries, pq.transfer, pq.matrix), (9, 16, 9));
        assert_eq!(pq.sei.len(), 2);
        // mastering display colour volume (137): the 24-byte payload of ST 2086, the `mdcv` box's bytes
        let (md, _) = ColorSignal::PQ.static_metadata().unwrap();
        assert_eq!((pq.sei[0].payload_type, pq.sei[0].payload.as_slice()), (137, md.to_bytes().as_slice()));
        assert_eq!(pq.sei[0].payload.len(), 24);
        // BT.2020 primaries (G, B, R order), D65, 1000 / 0.0001 cd/m2 in 0.0001 units
        assert_eq!(
            pq.sei[0].payload,
            [0x21, 0x34, 0x9B, 0xAA, 0x19, 0x96, 0x08, 0xFC, 0x8A, 0x48, 0x39, 0x08, 0x3D, 0x13, 0x40, 0x42, 0x00, 0x98, 0x96, 0x80, 0x00, 0x00, 0x00, 0x01]
        );
        // content light level (144): MaxCLL 0, MaxFALL 0 (unknown)
        assert_eq!((pq.sei[1].payload_type, pq.sei[1].payload.as_slice()), (144, &[0u8, 0, 0, 0][..]));
        // laid out as one SEI RBSP (type, size, payload, ...) these are the bytes the software H.264 encoder
        // writes for the same metadata (checked against its real output in the export crate's hdr_tests)
        let rbsp: Vec<u8> =
            pq.sei.iter().flat_map(|m| [m.payload_type as u8, m.payload.len() as u8].into_iter().chain(m.payload.iter().copied())).chain([0x80]).collect();
        assert_eq!(&rbsp[..2], &[137, 24]);
        assert_eq!(&rbsp[26..], &[144, 4, 0, 0, 0, 0, 0x80]);
        // HLG: the colour description only; SDR: BT.709, no messages
        let hlg = nvenc_signal(&ColorSignal::HLG);
        assert_eq!((hlg.primaries, hlg.transfer, hlg.matrix, hlg.sei.len()), (9, 18, 9, 0));
        assert_eq!(nvenc_signal(&ColorSignal::default()), Signal::default());
    }

    #[test]
    fn oversized_pictures_are_declined() {
        let s = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Auto, ..Default::default() };
        assert!(config(Format::H264, 70_000, 64, FrameRate::FPS_24, &s).is_err());
        assert!(config(Format::H264, 64, 70_000, FrameRate::FPS_24, &s).is_err());
    }
}
