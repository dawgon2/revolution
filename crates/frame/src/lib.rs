//! Video frames and audio buffers.
//!
//! - [`VideoFrame`]: decoded or rendered pictures. Planar Y'CbCr (8 or 16-bit containers) straight
//!   from decoders, sRGB-encoded RGBA8 from stills/generators, or linear premultiplied RGBA f32 in the
//!   compositor. Pixel data is `Arc`-shared so caches, monitors and export share frames for free.
//! - [`AudioBuffer`]: planar f32 samples.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod pool;

use std::sync::Arc;

use filmcraft_color::{ColorInfo, DecodeTable, Matrix, Range, linear_to_srgb_u8, normalize_c, normalize_y, srgb_u8_to_linear_table, to_linear, ycbcr_to_rgb};
use filmcraft_time::Tick;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Chroma subsampling of planar Y'CbCr.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chroma {
    C420,
    C422,
    C444,
}

impl Chroma {
    /// (horizontal shift, vertical shift)
    pub fn shifts(self) -> (u32, u32) {
        match self {
            Chroma::C420 => (1, 1),
            Chroma::C422 => (1, 0),
            Chroma::C444 => (0, 0),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Chroma::C420 => "4:2:0",
            Chroma::C422 => "4:2:2",
            Chroma::C444 => "4:4:4",
        }
    }
}

#[derive(Clone, Debug)]
pub enum PixelData {
    /// Straight-alpha, sRGB/709-encoded RGBA, 8 bits per channel.
    Rgba8(Arc<Vec<u8>>),
    /// Premultiplied, linear-light RGBA f32 (compositor working format).
    RgbaF32(Arc<Vec<f32>>),
    /// 8-bit planar Y'CbCr (optionally with an alpha plane).
    Yuv8 { planes: [Arc<Vec<u8>>; 3], chroma: Chroma, alpha: Option<Arc<Vec<u8>>> },
    /// 9–16-bit planar Y'CbCr stored in u16 (`bits` significant bits).
    Yuv16 { planes: [Arc<Vec<u16>>; 3], chroma: Chroma, bits: u32, alpha: Option<Arc<Vec<u16>>> },
    /// 9–16-bit Y'CbCr in two planes, as hardware decoders hand it over (P010 / P016 for 4:2:0,
    /// P210 for 4:2:2): `y` is the luma plane (width × height), `uv` the Cb and Cr samples
    /// interleaved at chroma resolution (2 × chroma width per row). Samples are MSB-aligned:
    /// a `bits`-bit code `c` is stored as `c << (16 - bits)`. The GPU uploads the planes as they
    /// are; CPU paths go through [`VideoFrame::to_planar`]. No alpha.
    Yuv16Bi { y: Arc<Vec<u16>>, uv: Arc<Vec<u16>>, chroma: Chroma, bits: u32 },
    /// A picture in GPU memory where a hardware decoder wrote it (a VA-API surface on Linux): the
    /// compositor samples it in place; CPU paths read it back through [`GpuSurface::download`]
    /// ([`VideoFrame::to_planar`]).
    Gpu(Arc<dyn GpuSurface>),
}

/// Layout of a GPU-resident picture's memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuFormat {
    /// 8-bit 4:2:0: luma plane, interleaved CbCr plane.
    Nv12,
    /// 10-bit 4:2:0 in 16-bit samples (MSB-aligned): luma plane, interleaved CbCr plane.
    P010,
    /// 10-bit 4:2:2 packed in one plane, per two pixels `Y0 Cb Y1 Cr` in 16-bit samples
    /// (MSB-aligned): Intel's media engine hands decoded 4:2:2 pictures over this way (fork 6.1b,
    /// a linear copy made for the compositing GPU). The [`DmaBuf`] has one layer (`Y210`).
    Y210,
}

/// A buffer object of a DMA-BUF export: an owned file descriptor on Unix (none can exist elsewhere).
#[cfg(unix)]
pub type DmaBufObject = std::os::fd::OwnedFd;
#[cfg(not(unix))]
pub type DmaBufObject = std::convert::Infallible;

/// One plane of a DMA-BUF export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaBufPlane {
    /// Index of the buffer object holding the plane in [`DmaBuf::objects`].
    pub object: usize,
    pub offset: u32,
    pub pitch: u32,
}

/// A picture's memory as Linux DMA-BUFs (one single-plane layer per picture plane: luma, chroma;
/// packed formats ([`GpuFormat::Y210`]) have one layer, the second is empty with format 0).
#[derive(Clone, Debug)]
pub struct DmaBuf {
    /// DRM fourcc of each layer (`R16` / `GR32` for P010, `R8` / `GR88` for NV12, `Y210` / 0).
    pub layer_formats: [u32; 2],
    /// Format modifier (tiling) of the memory.
    pub modifier: u64,
    /// Memory planes of each layer (more than one when the modifier has metadata planes).
    pub layers: [Vec<DmaBufPlane>; 2],
    /// Coded size in luma samples.
    pub width: u32,
    pub height: u32,
    /// The buffer objects, open for as long as this description (or a clone) lives; importers
    /// duplicate them. Owned descriptors: a description cannot name an fd it does not own.
    pub objects: std::sync::Arc<[DmaBufObject]>,
}

/// The compositor refused a packed 4:2:2 GPU picture ([`GpuFormat::Y210`]: no 16-bit normalized
/// textures, a failed import or unpack) this session: decoders stop handing such pictures over
/// (their copy would only be read back) and read them back to memory instead (fork 6.1b).
static PACKED_422_REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that the compositor cannot take packed 4:2:2 GPU pictures (logged once).
pub fn refuse_packed_422(why: &str) {
    if !PACKED_422_REFUSED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!("the GPU compositor cannot take 4:2:2 pictures from the decoder ({why}); they are read back to memory from now on");
    }
}

/// Whether [`refuse_packed_422`] was called this session.
pub fn packed_422_refused() -> bool {
    PACKED_422_REFUSED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Tests: forget [`refuse_packed_422`].
#[doc(hidden)]
pub fn reset_packed_422_refusal() {
    PACKED_422_REFUSED.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// A decoded picture held in GPU memory (see [`PixelData::Gpu`]). Implemented by the platform
/// decoders; the memory goes back to its decoder when the last reference is dropped.
pub trait GpuSurface: Send + Sync + std::fmt::Debug {
    /// Distinguishes the memory of this surface from every other one, ever (import caches).
    fn memory_id(&self) -> u64;
    /// Dead once the memory is freed: caches of it (GPU imports) drop their entries.
    fn liveness(&self) -> std::sync::Weak<()>;
    fn format(&self) -> GpuFormat;
    /// Coded size (the memory's size, at least the frame's).
    fn coded_size(&self) -> (u32, u32);
    /// Top-left of the visible picture in the coded picture (conformance window).
    fn crop_origin(&self) -> (u32, u32);
    /// The memory as DMA-BUFs (None: not exportable here); the picture has finished decoding.
    fn dmabuf(&self) -> Option<DmaBuf>;
    /// Read the visible picture back to memory: [`PixelData::Yuv16Bi`] (P010: 4:2:0, Y210: 4:2:2)
    /// or [`PixelData::Yuv8`] (NV12).
    fn download(&self, width: u32, height: u32) -> Result<PixelData, String>;
    /// Bytes of GPU memory it occupies.
    fn gpu_bytes(&self) -> usize;
    /// The surface as its own type, for a compositor that knows it (fork 8.6c: pictures in memory
    /// shared with CUDA); None by default.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub data: PixelData,
    pub color: ColorInfo,
    /// Pixel aspect ratio (num, den).
    pub par: (u32, u32),
    /// Presentation time in media time (informational).
    pub pts: Tick,
}

/// A rectangle of pixels: `x`, `y` is its top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Region {
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    /// This rectangle cut to a `w`×`h` picture (empty when it lies outside).
    pub fn clip(self, w: usize, h: usize) -> Region {
        let x = self.x.min(w);
        let y = self.y.min(h);
        Region { x, y, w: self.w.min(w - x), h: self.h.min(h - y) }
    }
    /// Whether this rectangle is the whole `w`×`h` picture.
    pub fn is_full(&self, w: usize, h: usize) -> bool {
        (self.x, self.y, self.w, self.h) == (0, 0, w, h)
    }
}

/// First and last index of the items of `row` for which `on` holds.
fn row_extent<T>(row: &[T], on: impl Fn(&T) -> bool) -> Option<(usize, usize)> {
    let first = row.iter().position(&on)?;
    let last = row.iter().rposition(&on)?;
    Some((first, last))
}

impl VideoFrame {
    pub fn rgba8(width: u32, height: u32, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), (width * height * 4) as usize);
        Self { width, height, data: PixelData::Rgba8(Arc::new(data)), color: ColorInfo::SRGB_FULL, par: (1, 1), pts: Tick::ZERO }
    }
    pub fn rgba_f32(width: u32, height: u32, data: Vec<f32>) -> Self {
        debug_assert_eq!(data.len(), (width * height * 4) as usize);
        Self { width, height, data: PixelData::RgbaF32(Arc::new(data)), color: ColorInfo::SRGB_FULL, par: (1, 1), pts: Tick::ZERO }
    }
    pub fn transparent_f32(width: u32, height: u32) -> Self {
        Self::rgba_f32(width, height, vec![0.0; (width * height * 4) as usize])
    }
    pub fn with_pts(mut self, pts: Tick) -> Self {
        self.pts = pts;
        self
    }

    pub fn format_label(&self) -> String {
        match &self.data {
            PixelData::Rgba8(_) => "RGBA 8-bit".into(),
            PixelData::RgbaF32(_) => "RGBA 32-bit float".into(),
            PixelData::Yuv8 { chroma, .. } => format!("YUV {} 8-bit", chroma.label()),
            PixelData::Yuv16 { chroma, bits, .. } | PixelData::Yuv16Bi { chroma, bits, .. } => format!("YUV {} {bits}-bit", chroma.label()),
            PixelData::Gpu(s) => match s.format() {
                GpuFormat::Nv12 => "YUV 4:2:0 8-bit (GPU)".into(),
                GpuFormat::P010 => "YUV 4:2:0 10-bit (GPU)".into(),
                GpuFormat::Y210 => "YUV 4:2:2 10-bit (GPU)".into(),
            },
        }
    }

    /// Approximate memory footprint in bytes (for caches).
    pub fn byte_size(&self) -> usize {
        match &self.data {
            PixelData::Rgba8(d) => d.len(),
            PixelData::RgbaF32(d) => d.len() * 4,
            PixelData::Yuv8 { planes, alpha, .. } => planes.iter().map(|p| p.len()).sum::<usize>() + alpha.as_ref().map_or(0, |a| a.len()),
            PixelData::Yuv16 { planes, alpha, .. } => (planes.iter().map(|p| p.len()).sum::<usize>() + alpha.as_ref().map_or(0, |a| a.len())) * 2,
            PixelData::Yuv16Bi { y, uv, .. } => (y.len() + uv.len()) * 2,
            // in GPU memory: see `gpu_bytes`
            PixelData::Gpu(_) => 0,
        }
    }

    /// Bytes of GPU memory the frame holds (a hardware decoder's surface).
    pub fn gpu_bytes(&self) -> usize {
        match &self.data {
            PixelData::Gpu(s) => s.gpu_bytes(),
            _ => 0,
        }
    }

    /// A biplanar frame ([`PixelData::Yuv16Bi`]) as planar [`PixelData::Yuv16`] (codes in the
    /// low bits, chroma deinterleaved), for the CPU paths; None for every other layout. A plane
    /// shorter than the frame size says (damaged input) reads as black / neutral there.
    pub fn to_planar(&self) -> Option<VideoFrame> {
        if let PixelData::Gpu(s) = &self.data {
            // read back; a failed read-back is a black picture (and logged), never a crash
            let data = s.download(self.width, self.height).unwrap_or_else(|e| {
                log::warn!("reading a GPU picture back failed: {e}");
                let n = (self.width as usize).saturating_mul(self.height as usize);
                let c = (self.width as usize).div_ceil(2).saturating_mul((self.height as usize).div_ceil(2));
                PixelData::Yuv8 { planes: [Arc::new(vec![16; n]), Arc::new(vec![128; c]), Arc::new(vec![128; c])], chroma: Chroma::C420, alpha: None }
            });
            let f = VideoFrame { data, ..self.clone() };
            return Some(f.to_planar().unwrap_or(f));
        }
        let PixelData::Yuv16Bi { y, uv, chroma, bits } = &self.data else { return None };
        let (w, h) = (self.width as usize, self.height as usize);
        let (sx, sy) = chroma.shifts();
        let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
        let bits = (*bits).clamp(1, 16);
        let shift = 16 - bits;
        let mut yp = pool::take_u16(w * h);
        yp.resize(w * h, 0);
        let src = y.get(..w * h).unwrap_or(&[]);
        yp.par_chunks_mut(w.max(1)).zip(src.par_chunks(w.max(1))).for_each(|(d, s)| {
            for (d, s) in d.iter_mut().zip(s) {
                *d = s >> shift;
            }
        });
        let mid = 1u16 << (bits - 1);
        let (mut u, mut v) = (pool::take_u16(cw * ch), pool::take_u16(cw * ch));
        u.resize(cw * ch, mid);
        v.resize(cw * ch, mid);
        let src = uv.get(..cw * ch * 2).unwrap_or(&[]);
        u.par_chunks_mut(cw.max(1)).zip(v.par_chunks_mut(cw.max(1))).zip(src.par_chunks(cw.max(1) * 2)).for_each(|((ur, vr), s)| {
            for ((a, b), pair) in ur.iter_mut().zip(vr.iter_mut()).zip(s.as_chunks::<2>().0) {
                *a = pair[0] >> shift;
                *b = pair[1] >> shift;
            }
        });
        let data = PixelData::Yuv16 { planes: [Arc::new(yp), Arc::new(u), Arc::new(v)], chroma: *chroma, bits, alpha: None };
        Some(VideoFrame { width: self.width, height: self.height, data, color: self.color, par: self.par, pts: self.pts })
    }

    /// A planar Y'CbCr frame reduced by `n` (2, 4, 8…) in each direction: every sample (luma,
    /// chroma at its own resolution, alpha) is the rounded mean of an `n`×`n` block, chroma format
    /// and bit depth unchanged. Reduced-resolution playback hands the GPU this instead of the full
    /// picture (a quarter / sixteenth of the upload). None for RGBA frames or `n` < 2, and for
    /// GPU-resident frames: they would be read back to memory to be filtered, where the GPU
    /// scales them for nothing while drawing.
    pub fn box_decimated(&self, n: usize) -> Option<VideoFrame> {
        if n < 2 || matches!(self.data, PixelData::Gpu(_)) {
            return None;
        }
        if let Some(p) = self.to_planar() {
            return p.box_decimated(n);
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let (ow, oh) = ((w / n).max(1), (h / n).max(1));
        let data = match &self.data {
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let (ocw, och) = (ow.div_ceil(1 << sx), oh.div_ceil(1 << sy));
                PixelData::Yuv8 {
                    planes: [
                        Arc::new(box_plane(&planes[0], w, h, ow, oh, n)),
                        Arc::new(box_plane(&planes[1], cw, ch, ocw, och, n)),
                        Arc::new(box_plane(&planes[2], cw, ch, ocw, och, n)),
                    ],
                    chroma: *chroma,
                    alpha: alpha.as_ref().map(|a| Arc::new(box_plane(a, w, h, ow, oh, n))),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let (ocw, och) = (ow.div_ceil(1 << sx), oh.div_ceil(1 << sy));
                PixelData::Yuv16 {
                    planes: [
                        Arc::new(box_plane(&planes[0], w, h, ow, oh, n)),
                        Arc::new(box_plane(&planes[1], cw, ch, ocw, och, n)),
                        Arc::new(box_plane(&planes[2], cw, ch, ocw, och, n)),
                    ],
                    chroma: *chroma,
                    bits: *bits,
                    alpha: alpha.as_ref().map(|a| Arc::new(box_plane(a, w, h, ow, oh, n))),
                }
            }
            PixelData::Rgba8(_) | PixelData::RgbaF32(_) | PixelData::Yuv16Bi { .. } | PixelData::Gpu(_) => return None,
        };
        Some(VideoFrame { width: ow as u32, height: oh as u32, data, color: self.color, par: self.par, pts: self.pts })
    }

    /// The part of the frame inside `region` (a container's clean aperture). The region is clipped
    /// to the frame; on Y'CbCr frames its corner is moved up / left onto the chroma grid (at most
    /// one sample, keeping its size where the frame allows), so chroma is cut where luma is. An empty
    /// region, or one covering the whole frame, leaves the frame as it is.
    pub fn cropped(&self, region: Region) -> VideoFrame {
        let (w, h) = (self.width as usize, self.height as usize);
        let (sx, sy) = match &self.data {
            PixelData::Yuv8 { chroma, .. } | PixelData::Yuv16 { chroma, .. } => chroma.shifts(),
            PixelData::Rgba8(_) | PixelData::RgbaF32(_) => (0, 0),
            // Biplanar / GPU pictures are cropped as planar Y'CbCr (read back first); one that
            // can't be read back is left whole.
            PixelData::Yuv16Bi { .. } | PixelData::Gpu(_) => {
                let r = region.clip(w, h);
                if r.w == 0 || r.h == 0 || (r.w == w && r.h == h) {
                    return self.clone();
                }
                return match self.to_planar() {
                    Some(p) if matches!(p.data, PixelData::Yuv16 { .. } | PixelData::Yuv8 { .. }) => p.cropped(region),
                    _ => self.clone(),
                };
            }
        };
        let (mx, my) = ((1usize << sx) - 1, (1usize << sy) - 1);
        let (x, y) = (region.x.min(w) & !mx, region.y.min(h) & !my);
        let r = Region { x, y, w: region.w, h: region.h }.clip(w, h);
        if r.w == 0 || r.h == 0 || (r.w == w && r.h == h) {
            return self.clone();
        }
        let chroma_rect = |cw: usize, ch: usize| {
            (r.x >> sx, r.y >> sy, r.w.div_ceil(1 << sx).min(cw - (r.x >> sx).min(cw)), r.h.div_ceil(1 << sy).min(ch - (r.y >> sy).min(ch)))
        };
        let data = match &self.data {
            // Handled above.
            PixelData::Yuv16Bi { .. } | PixelData::Gpu(_) => return self.clone(),
            PixelData::Rgba8(d) => PixelData::Rgba8(Arc::new(crop_plane(d, w, h, 4, r))),
            PixelData::RgbaF32(d) => PixelData::RgbaF32(Arc::new(crop_plane(d, w, h, 4, r))),
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let (cx, cy, cr_w, cr_h) = chroma_rect(cw, ch);
                let c = Region { x: cx, y: cy, w: cr_w, h: cr_h };
                PixelData::Yuv8 {
                    planes: [
                        Arc::new(crop_plane(&planes[0], w, h, 1, r)),
                        Arc::new(crop_plane(&planes[1], cw, ch, 1, c)),
                        Arc::new(crop_plane(&planes[2], cw, ch, 1, c)),
                    ],
                    chroma: *chroma,
                    alpha: alpha.as_ref().map(|a| Arc::new(crop_plane(a, w, h, 1, r))),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let (cx, cy, cr_w, cr_h) = chroma_rect(cw, ch);
                let c = Region { x: cx, y: cy, w: cr_w, h: cr_h };
                PixelData::Yuv16 {
                    planes: [
                        Arc::new(crop_plane(&planes[0], w, h, 1, r)),
                        Arc::new(crop_plane(&planes[1], cw, ch, 1, c)),
                        Arc::new(crop_plane(&planes[2], cw, ch, 1, c)),
                    ],
                    chroma: *chroma,
                    bits: *bits,
                    alpha: alpha.as_ref().map(|a| Arc::new(crop_plane(a, w, h, 1, r))),
                }
            }
        };
        VideoFrame { width: r.w as u32, height: r.h as u32, data, color: self.color, par: self.par, pts: self.pts }
    }

    /// The frame turned clockwise by `quarter_turns` × 90° (a container's display rotation).
    /// Planes rotate at their own resolution; 4:2:2 chroma is widened to 4:4:4 first for a quarter
    /// or three-quarter turn (its half-width chroma would become half-height, which has no
    /// [`Chroma`] variant). The pixel aspect ratio follows the turn.
    pub fn rotated(&self, quarter_turns: u8) -> VideoFrame {
        let q = quarter_turns % 4;
        if q == 0 {
            return self.clone();
        }
        if let Some(p) = self.to_planar() {
            return p.rotated(q);
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let data = match &self.data {
            PixelData::Rgba8(d) => PixelData::Rgba8(Arc::new(rotate_plane(d, w, h, 4, q))),
            PixelData::RgbaF32(d) => PixelData::RgbaF32(Arc::new(rotate_plane(d, w, h, 4, q))),
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (c, chroma) = rotate_chroma(planes, *chroma, w, h, q);
                PixelData::Yuv8 {
                    planes: [Arc::new(rotate_plane(&planes[0], w, h, 1, q)), Arc::new(c[0].clone()), Arc::new(c[1].clone())],
                    chroma,
                    alpha: alpha.as_ref().map(|a| Arc::new(rotate_plane(a, w, h, 1, q))),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (c, chroma) = rotate_chroma(planes, *chroma, w, h, q);
                PixelData::Yuv16 {
                    planes: [Arc::new(rotate_plane(&planes[0], w, h, 1, q)), Arc::new(c[0].clone()), Arc::new(c[1].clone())],
                    chroma,
                    bits: *bits,
                    alpha: alpha.as_ref().map(|a| Arc::new(rotate_plane(a, w, h, 1, q))),
                }
            }
            // converted to planar above
            PixelData::Yuv16Bi { .. } | PixelData::Gpu(_) => return self.clone(),
        };
        let (width, height, par) = if q % 2 == 1 { (self.height, self.width, (self.par.1, self.par.0)) } else { (self.width, self.height, self.par) };
        VideoFrame { width, height, data, color: self.color, par, pts: self.pts }
    }

    /// Convert to premultiplied linear RGBA f32 (the compositor's working format).
    pub fn to_linear_f32(&self) -> Vec<f32> {
        self.to_linear_f32_decimated(1).2
    }

    /// Convert to premultiplied linear RGBA f32, box-filtering `n`×`n` blocks (n = 1, 2, 4, 8…)
    /// in linear light. Reduced-resolution playback uses this so it never builds full-size float
    /// buffers. Returns (width, height, pixels).
    pub fn to_linear_f32_decimated(&self, n: usize) -> (usize, usize, Vec<f32>) {
        self.to_linear_f32_decimated_with(n, None)
    }

    /// Like [`VideoFrame::to_linear_f32_decimated`], decoding each channel's signal through
    /// `decode` (a colour-managed curve: log, PQ, HLG scene light…) instead of the frame's
    /// transfer. Float frames are already linear and ignore it.
    pub fn to_linear_f32_decimated_with(&self, n: usize, decode: Option<&DecodeTable>) -> (usize, usize, Vec<f32>) {
        let n = n.max(1);
        let (ow, oh) = self.decimated_size(n);
        // every element is written below, so a recycled buffer needs no zero-fill
        let mut out = pool::take_f32_overwritten(ow * oh * 4);
        self.convert_region(n, decode, Region { x: 0, y: 0, w: ow, h: oh }, &mut out);
        (ow, oh, out)
    }

    /// The size of the picture after `n`×`n` box decimation.
    pub fn decimated_size(&self, n: usize) -> (usize, usize) {
        let n = n.max(1);
        ((self.width as usize / n).max(1), (self.height as usize / n).max(1))
    }

    /// [`VideoFrame::to_linear_f32_decimated_with`] for one rectangle of the decimated picture
    /// only: the rectangle (clipped to the picture) and its premultiplied linear RGBA pixels, row
    /// by row. Each pixel is exactly what the full conversion gives at that position. The buffer
    /// may come from the pool ([`pool::recycle_f32`] hands it back).
    pub fn to_linear_f32_region(&self, n: usize, decode: Option<&DecodeTable>, region: Region) -> (Region, Vec<f32>) {
        let n = n.max(1);
        let (ow, oh) = self.decimated_size(n);
        let r = region.clip(ow, oh);
        let mut out = pool::take_f32_overwritten(r.w * r.h * 4);
        self.convert_region(n, decode, r, &mut out);
        (r, out)
    }

    /// Convert the rectangle `r` (inside the decimated picture) into `out`, which must hold exactly
    /// `r.w * r.h * 4` floats; every one of them is written.
    fn convert_region(&self, n: usize, decode: Option<&DecodeTable>, r: Region, out: &mut [f32]) {
        // biplanar and GPU pictures (hardware decoders) go through their planar copy
        if let Some(p) = self.to_planar() {
            return p.convert_region(n, decode, r, out);
        }
        let (w, h) = (self.width as usize, self.height as usize);
        if r.w == 0 || r.h == 0 || out.len() != r.w * r.h * 4 {
            return;
        }
        if w == 0 || h == 0 {
            out.fill(0.0);
            return;
        }
        if let PixelData::RgbaF32(d) = &self.data
            && n == 1
        {
            for (ry, row) in out.chunks_exact_mut(r.w * 4).enumerate() {
                let start = ((r.y + ry) * w + r.x) * 4;
                match d.get(start..start + r.w * 4) {
                    Some(src) => row.copy_from_slice(src),
                    // a frame with less data than its size says: transparent, not what the buffer held before
                    None => row.fill(0.0),
                }
            }
            return;
        }
        // Encoded (0..1, quantised to 12 bits) → linear lookup for this frame's transfer.
        let info = self.color;
        let lin: Vec<f32> = if decode.is_some() { Vec::new() } else { (0..4096).map(|i| to_linear(i as f32 / 4095.0, info.transfer)).collect() };
        // a linear transfer passes values through unclamped (a colour-managed frame handed over as
        // its signal keeps its super-whites, as on the GPU)
        let linear = info.transfer == filmcraft_color::Transfer::Linear;
        let q = |v: f32| match decode {
            Some(t) => t.lookup(v),
            None if linear => v,
            None => lin[(v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize],
        };
        let inv = 1.0 / (n * n) as f32;
        match &self.data {
            PixelData::RgbaF32(d) => {
                out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                    let oy = r.y + ry;
                    for rx in 0..r.w {
                        let ox = r.x + rx;
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let i = (y * w + x) * 4;
                                for k in 0..4 {
                                    acc[k] += d[i + k];
                                }
                            }
                        }
                        for k in 0..4 {
                            row[rx * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
            PixelData::Rgba8(d) => {
                let table: Vec<f32>;
                let lut: &[f32] = match decode {
                    Some(t) => {
                        table = (0..256).map(|i| t.lookup(i as f32 / 255.0)).collect();
                        &table
                    }
                    None => srgb_u8_to_linear_table(),
                };
                out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                    let oy = r.y + ry;
                    for rx in 0..r.w {
                        let ox = r.x + rx;
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let s = &d[(y * w + x) * 4..(y * w + x) * 4 + 4];
                                let a = s[3] as f32 / 255.0;
                                acc[0] += lut[s[0] as usize] * a;
                                acc[1] += lut[s[1] as usize] * a;
                                acc[2] += lut[s[2] as usize] * a;
                                acc[3] += a;
                            }
                        }
                        for k in 0..4 {
                            row[rx * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (sx, sy) = chroma.shifts();
                let cw = w.div_ceil(1 << sx);
                let ytab: Vec<f32> = (0..256).map(|v| normalize_y(v, 8, info.range)).collect();
                let ctab: Vec<f32> = (0..256).map(|v| normalize_c(v, 8, info.range)).collect();
                let (kr, kb) = info.matrix.kr_kb();
                let kg = 1.0 - kr - kb;
                let (cr_r, cb_b) = (2.0 * (1.0 - kr), 2.0 * (1.0 - kb));
                let (cr_g, cb_g) = (cr_r * kr / kg, cb_b * kb / kg);
                out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                    let oy = r.y + ry;
                    for rx in 0..r.w {
                        let ox = r.x + rx;
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            let cy = y >> sy;
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let cx = x >> sx;
                                let yy = ytab[planes[0][y * w + x] as usize];
                                let u = ctab[planes[1][cy * cw + cx] as usize];
                                let v = ctab[planes[2][cy * cw + cx] as usize];
                                let a = alpha.as_ref().map_or(1.0, |al| al[y * w + x] as f32 / 255.0);
                                acc[0] += q(yy + cr_r * v) * a;
                                acc[1] += q(yy - cr_g * v - cb_g * u) * a;
                                acc[2] += q(yy + cb_b * u) * a;
                                acc[3] += a;
                            }
                        }
                        for k in 0..4 {
                            row[rx * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let cw = w.div_ceil(1 << sx);
                let bits = *bits;
                let amax = ((1u32 << bits) - 1) as f32;
                out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                    let oy = r.y + ry;
                    for rx in 0..r.w {
                        let ox = r.x + rx;
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            let cy = y >> sy;
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let cx = x >> sx;
                                let yy = normalize_y(planes[0][y * w + x] as u32, bits, info.range);
                                let u = normalize_c(planes[1][cy * cw + cx] as u32, bits, info.range);
                                let v = normalize_c(planes[2][cy * cw + cx] as u32, bits, info.range);
                                let rgb = ycbcr_to_rgb(yy, u, v, info.matrix);
                                let a = alpha.as_ref().map_or(1.0, |al| al[y * w + x] as f32 / amax);
                                acc[0] += q(rgb[0]) * a;
                                acc[1] += q(rgb[1]) * a;
                                acc[2] += q(rgb[2]) * a;
                                acc[3] += a;
                            }
                        }
                        for k in 0..4 {
                            row[rx * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
            // converted to planar above
            PixelData::Yuv16Bi { .. } | PixelData::Gpu(_) => {}
        }
    }

    /// Where the picture is not transparent: the smallest rectangle of the `n`×`n`-decimated
    /// picture that holds every pixel whose alpha is not zero (empty for a picture that is
    /// transparent everywhere). `None` when the frame has no alpha plane to look at (it is opaque,
    /// or its alpha is not stored as integers), so nothing can be skipped. Pixels outside the
    /// rectangle have alpha exactly 0 and, in the premultiplied conversion, colour 0 as well.
    pub fn alpha_region(&self, n: usize) -> Option<Region> {
        let n = n.max(1);
        let (w, h) = (self.width as usize, self.height as usize);
        let px = w.checked_mul(h)?;
        // the first and last x of non-transparent pixels in source row `y`
        let extent: Box<dyn Fn(usize) -> Option<(usize, usize)> + Sync + '_> = match &self.data {
            PixelData::Yuv8 { alpha: Some(a), .. } if a.len() == px => Box::new(move |y| row_extent(&a[y * w..(y + 1) * w], |v| *v != 0)),
            PixelData::Yuv16 { alpha: Some(a), .. } if a.len() == px => Box::new(move |y| row_extent(&a[y * w..(y + 1) * w], |v| *v != 0)),
            PixelData::Rgba8(d) if d.len() == px.checked_mul(4)? => {
                Box::new(move |y| row_extent(d[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0, |p: &[u8; 4]| p[3] != 0))
            }
            _ => return None,
        };
        // (first row, last row, first column, last column) over the rows that have any
        let found = (0..h)
            .into_par_iter()
            .filter_map(|y| extent(y).map(|(a, b)| (y, y, a, b)))
            .reduce_with(|p, q| (p.0.min(q.0), p.1.max(q.1), p.2.min(q.2), p.3.max(q.3)));
        let (ow, oh) = self.decimated_size(n);
        let Some((y0, y1, x0, x1)) = found else { return Some(Region { x: 0, y: 0, w: 0, h: 0 }) };
        // a decimated pixel covers source pixels [o·n, o·n + n)
        let (rx0, ry0) = (x0 / n, y0 / n);
        let (rx1, ry1) = (x1 / n + 1, y1 / n + 1);
        Some(Region { x: rx0, y: ry0, w: rx1.saturating_sub(rx0), h: ry1.saturating_sub(ry0) }.clip(ow, oh))
    }

    /// Convert to straight-alpha sRGB RGBA8 for display (fast paths for 8-bit sources).
    pub fn to_rgba8(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        match &self.data {
            PixelData::Rgba8(d) => d.as_ref().clone(),
            PixelData::Yuv8 { planes, chroma, alpha: None }
                if matches!(self.color.transfer, filmcraft_color::Transfer::Bt709 | filmcraft_color::Transfer::Srgb) =>
            {
                // Direct display path: Y'CbCr → R'G'B' without linearisation.
                let (sx, sy) = chroma.shifts();
                let cw = w.div_ceil(1 << sx);
                let (kr, kb) = self.color.matrix.kr_kb();
                let kg = 1.0 - kr - kb;
                let (ys, yo, cs) = match self.color.range {
                    Range::Limited => (255.0 / 219.0, 16.0, 255.0 / 224.0),
                    Range::Full => (1.0, 0.0, 1.0),
                };
                let (crr, cbb) = (2.0 * (1.0 - kr) * cs, 2.0 * (1.0 - kb) * cs);
                let (cgr, cgb) = (crr * kr / kg, cbb * kb / kg);
                let mut out = vec![0u8; w * h * 4];
                out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    let cy = y >> sy;
                    let yrow = &planes[0][y * w..y * w + w];
                    let urow = &planes[1][cy * cw..cy * cw + cw];
                    let vrow = &planes[2][cy * cw..cy * cw + cw];
                    for x in 0..w {
                        let yy = (yrow[x] as f32 - yo) * ys;
                        let u = urow[x >> sx] as f32 - 128.0;
                        let v = vrow[x >> sx] as f32 - 128.0;
                        let o = &mut row[x * 4..x * 4 + 4];
                        o[0] = (yy + crr * v).round().clamp(0.0, 255.0) as u8;
                        o[1] = (yy - cgr * v - cgb * u).round().clamp(0.0, 255.0) as u8;
                        o[2] = (yy + cbb * u).round().clamp(0.0, 255.0) as u8;
                        o[3] = 255;
                    }
                });
                out
            }
            _ => {
                let lin = self.to_linear_f32();
                let mut out = vec![0u8; w * h * 4];
                out.par_chunks_mut(w * 4).zip(lin.par_chunks(w * 4)).for_each(|(o, s)| linear_premul_to_srgb8(s, o));
                out
            }
        }
    }

    /// Luma plane (8-bit, for scopes/thumbnails analysis).
    pub fn luma8(&self) -> Vec<u8> {
        match &self.data {
            PixelData::Yuv8 { planes, .. } => planes[0].as_ref().clone(),
            _ => self.to_rgba8().as_chunks::<4>().0.iter().map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) as u8).collect(),
        }
    }
}

/// Convert a row of premultiplied linear f32 RGBA into straight sRGB RGBA8.
pub fn linear_premul_to_srgb8(src: &[f32], dst: &mut [u8]) {
    for (s, o) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0) {
        let a = s[3].clamp(0.0, 1.0);
        if a <= 0.0 {
            o.copy_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let inv = 1.0 / a;
        o[0] = linear_to_srgb_u8(s[0] * inv);
        o[1] = linear_to_srgb_u8(s[1] * inv);
        o[2] = linear_to_srgb_u8(s[2] * inv);
        o[3] = (a * 255.0 + 0.5) as u8;
    }
}

/// Planar f32 audio.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    /// One Vec per channel, all the same length.
    pub channels: Vec<Vec<f32>>,
}

impl AudioBuffer {
    pub fn silence(sample_rate: u32, channels: usize, frames: usize) -> Self {
        Self { sample_rate, channels: vec![vec![0.0; frames]; channels] }
    }
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
    /// Mix `other` into self with gain (channel counts are matched by wrapping/mono-spreading).
    pub fn mix_from(&mut self, other: &AudioBuffer, gains: &[f32]) {
        let n = self.frames().min(other.frames());
        for (c, dst) in self.channels.iter_mut().enumerate() {
            let src = &other.channels[if other.channels.len() == 1 { 0 } else { c % other.channels.len() }];
            let g = gains.get(c).copied().unwrap_or(1.0);
            for i in 0..n {
                dst[i] += src[i] * g;
            }
        }
    }
    /// Interleave into a single Vec (for audio output / encoders).
    pub fn interleaved(&self) -> Vec<f32> {
        let n = self.frames();
        let c = self.channels.len();
        let mut out = vec![0.0; n * c];
        for (ci, ch) in self.channels.iter().enumerate() {
            for (i, s) in ch.iter().enumerate() {
                out[i * c + ci] = *s;
            }
        }
        out
    }
    /// Peak absolute sample per channel.
    pub fn peaks(&self) -> Vec<f32> {
        self.channels.iter().map(|c| c.iter().fold(0f32, |m, s| m.max(s.abs()))).collect()
    }
}

/// Matrix used when a decoder does not signal one: BT.601 for SD, BT.709 otherwise (common practice).
pub fn default_matrix(width: u32, height: u32) -> Matrix {
    if width <= 1024 && height <= 576 { Matrix::Bt601 } else { Matrix::Bt709 }
}

/// `n`×`n` box mean of a `w`×`h` plane into `ow`×`oh` (blocks clamped at the right / bottom).
fn box_plane<T: Copy + Into<u32> + TryFrom<u32> + Send + Sync + Default>(src: &[T], w: usize, h: usize, ow: usize, oh: usize, n: usize) -> Vec<T> {
    let mut out = vec![T::default(); ow * oh];
    if w == 0 || h == 0 || src.len() < w * h {
        return out;
    }
    out.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        let mut acc = vec![0u32; ow];
        let mut cnt = vec![0u32; ow];
        for y in (oy * n..(oy + 1) * n).filter(|&y| y < h) {
            let line = &src[y * w..y * w + w];
            for ((a, c), chunk) in acc.iter_mut().zip(cnt.iter_mut()).zip(line.chunks(n)) {
                *a += chunk.iter().map(|&v| v.into()).sum::<u32>();
                *c += chunk.len() as u32;
            }
        }
        for ((o, a), c) in row.iter_mut().zip(&acc).zip(&cnt) {
            let c = (*c).max(1);
            *o = T::try_from((a + c / 2) / c).unwrap_or_default();
        }
    });
    out
}

/// The `r` part of a `w`×`h` plane of `n`-element pixels (`r` lies inside the plane). A plane too
/// short for its size gives a blank result, as in [`rotate_plane`].
fn crop_plane<T: Copy + Default>(src: &[T], w: usize, h: usize, n: usize, r: Region) -> Vec<T> {
    let mut out = vec![T::default(); r.w * r.h * n];
    if src.len() < w * h * n || r.x + r.w > w || r.y + r.h > h {
        return out;
    }
    for (row, o) in out.chunks_exact_mut((r.w * n).max(1)).enumerate().take(r.h) {
        let s = ((r.y + row) * w + r.x) * n;
        o.copy_from_slice(&src[s..s + r.w * n]);
    }
    out
}

/// A `w`×`h` plane of `n`-element pixels turned clockwise by `q` (1–3) quarter turns.
fn rotate_plane<T: Copy + Default + Send + Sync>(src: &[T], w: usize, h: usize, n: usize, q: u8) -> Vec<T> {
    let mut out = vec![T::default(); w * h * n];
    if w == 0 || h == 0 || src.len() < w * h * n {
        return out;
    }
    // output width: h for a quarter / three-quarter turn
    let ow = if q % 2 == 1 { h } else { w };
    out.par_chunks_mut(ow * n).enumerate().for_each(|(oy, row)| {
        for ox in 0..ow {
            // the source pixel shown at (ox, oy)
            let (x, y) = match q {
                1 => (oy, h - 1 - ox),
                2 => (w - 1 - ox, h - 1 - oy),
                _ => (w - 1 - oy, ox),
            };
            let s = (y * w + x) * n;
            row[ox * n..ox * n + n].copy_from_slice(&src[s..s + n]);
        }
    });
    out
}

/// The two chroma planes of a `w`×`h` picture turned by `q` quarter turns, and their new format.
fn rotate_chroma<T: Copy + Default + Send + Sync>(planes: &[Arc<Vec<T>>; 3], chroma: Chroma, w: usize, h: usize, q: u8) -> ([Vec<T>; 2], Chroma) {
    let (sx, sy) = chroma.shifts();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    if chroma == Chroma::C422 && q % 2 == 1 {
        // widen to 4:4:4 (each chroma sample covers two luma columns), then turn
        let widen = |p: &[T]| -> Vec<T> {
            let mut o = vec![T::default(); w * h];
            if p.len() >= cw * h {
                for y in 0..h {
                    for x in 0..w {
                        o[y * w + x] = p[y * cw + x / 2];
                    }
                }
            }
            o
        };
        return ([rotate_plane(&widen(&planes[1]), w, h, 1, q), rotate_plane(&widen(&planes[2]), w, h, 1, q)], Chroma::C444);
    }
    ([rotate_plane(&planes[1], cw, ch, 1, q), rotate_plane(&planes[2], cw, ch, 1, q)], chroma)
}

#[cfg(test)]
mod tests {

    /// A GPU-resident picture that counts how often it is read back.
    #[derive(Debug, Default)]
    struct Surface {
        downloads: std::sync::atomic::AtomicUsize,
        alive: Arc<()>,
    }

    impl GpuSurface for Surface {
        fn memory_id(&self) -> u64 {
            1
        }
        fn liveness(&self) -> std::sync::Weak<()> {
            Arc::downgrade(&self.alive)
        }
        fn format(&self) -> GpuFormat {
            GpuFormat::P010
        }
        fn coded_size(&self) -> (u32, u32) {
            (8, 8)
        }
        fn crop_origin(&self) -> (u32, u32) {
            (0, 0)
        }
        fn dmabuf(&self) -> Option<DmaBuf> {
            None
        }
        fn download(&self, width: u32, height: u32) -> Result<PixelData, String> {
            self.downloads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let n = (width * height) as usize;
            Ok(PixelData::Yuv16Bi { y: Arc::new(vec![0; n]), uv: Arc::new(vec![0; n / 2]), chroma: Chroma::C420, bits: 10 })
        }
        fn gpu_bytes(&self) -> usize {
            96
        }
    }

    #[test]
    fn reduced_resolution_never_reads_gpu_frames_back() {
        let surface = Arc::new(Surface::default());
        let f = VideoFrame { width: 8, height: 8, data: PixelData::Gpu(surface.clone()), color: ColorInfo::REC709, par: (1, 1), pts: Tick::ZERO };
        assert!(f.box_decimated(2).is_none());
        assert!(f.box_decimated(4).is_none());
        assert_eq!(surface.downloads.load(std::sync::atomic::Ordering::Relaxed), 0);
        // (the CPU paths still can, explicitly)
        assert!(f.to_planar().is_some());
        assert_eq!(surface.downloads.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    fn p010(w: u32, h: u32, y: Vec<u16>, uv: Vec<u16>, chroma: Chroma) -> VideoFrame {
        VideoFrame {
            width: w,
            height: h,
            data: PixelData::Yuv16Bi { y: Arc::new(y), uv: Arc::new(uv), chroma, bits: 10 },
            color: ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        }
    }

    #[test]
    fn biplanar_frames_convert_to_planar_codes() {
        // 3×2 4:2:0: chroma 2×1, odd width rounds up
        let f = p010(
            3,
            2,
            [64u16, 940, 512, 100, 200, 1023].iter().map(|c| c << 6).collect(),
            [100u16, 900, 512, 0].iter().map(|c| c << 6).collect(),
            Chroma::C420,
        );
        let p = f.to_planar().expect("biplanar");
        let PixelData::Yuv16 { planes, chroma, bits, alpha } = &p.data else { panic!("planar") };
        assert_eq!((*chroma, *bits, alpha.is_none()), (Chroma::C420, 10, true));
        assert_eq!((planes[0].as_slice(), planes[1].as_slice(), planes[2].as_slice()), (&[64, 940, 512, 100, 200, 1023][..], &[100, 512][..], &[900, 0][..]));
        assert_eq!((f.format_label(), f.byte_size()), ("YUV 4:2:0 10-bit".to_string(), (6 + 4) * 2));
        assert_eq!(f.to_rgba8(), p.to_rgba8());
        assert_eq!(f.box_decimated(2).map(|d| d.to_rgba8()), p.box_decimated(2).map(|d| d.to_rgba8()));
        assert_eq!(f.rotated(1).to_rgba8(), p.rotated(1).to_rgba8());
        assert!(p.to_planar().is_none(), "already planar");
    }

    #[test]
    fn damaged_biplanar_frames_do_not_panic() {
        // planes shorter than the size says (a broken decoder) and an absurd bit depth
        let f = p010(64, 32, vec![1 << 15; 10], vec![], Chroma::C422);
        let p = f.to_planar().expect("biplanar");
        assert_eq!(p.to_rgba8().len(), 64 * 32 * 4);
        let g = VideoFrame { data: PixelData::Yuv16Bi { y: Arc::new(vec![]), uv: Arc::new(vec![]), chroma: Chroma::C444, bits: 99 }, ..f };
        assert_eq!(g.to_linear_f32().len(), 64 * 32 * 4);
    }

    #[test]
    fn cropping_cuts_every_plane_at_the_same_place() {
        // 6x4 RGBA8, red = index
        let px: Vec<u8> = (0..24u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let f = VideoFrame { par: (4, 3), ..VideoFrame::rgba8(6, 4, px) };
        let c = f.cropped(Region { x: 1, y: 1, w: 3, h: 2 });
        assert_eq!((c.width, c.height, c.par), (3, 2, (4, 3)));
        assert_eq!(c.to_rgba8().chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [7, 8, 9, 13, 14, 15]);
        // a region reaching past the frame is clipped to it; an empty or full one changes nothing
        let c = f.cropped(Region { x: 4, y: 3, w: 100, h: 100 });
        assert_eq!((c.width, c.height), (2, 1));
        assert_eq!(c.to_rgba8().chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [22, 23]);
        for r in [
            Region { x: 0, y: 0, w: 0, h: 2 },
            Region { x: 9, y: 9, w: 2, h: 2 },
            Region { x: 0, y: 0, w: 6, h: 4 },
            Region { x: 0, y: 0, w: usize::MAX, h: usize::MAX },
        ] {
            let c = f.cropped(r);
            assert_eq!((c.width, c.height), (6, 4), "{r:?}");
        }

        // 4:2:0, odd sizes (7x5: chroma 4x3), with alpha: luma = index, chroma = 100 + index
        let luma: Vec<u8> = (0..35u8).collect();
        let cb: Vec<u8> = (100..112u8).collect();
        let yuv = |chroma| VideoFrame {
            width: 7,
            height: 5,
            data: PixelData::Yuv8 { planes: [Arc::new(luma.clone()), Arc::new(cb.clone()), Arc::new(cb.clone())], chroma, alpha: Some(Arc::new(luma.clone())) },
            color: Default::default(),
            par: (1, 1),
            pts: Default::default(),
        };
        // an odd corner moves onto the chroma grid (1,1 -> 0,0), keeping the size
        let c = yuv(Chroma::C420).cropped(Region { x: 1, y: 1, w: 3, h: 3 });
        assert_eq!((c.width, c.height), (3, 3));
        let PixelData::Yuv8 { planes, alpha, .. } = &c.data else { panic!() };
        assert_eq!(*planes[0], [0, 1, 2, 7, 8, 9, 14, 15, 16]);
        assert_eq!(**alpha.as_ref().unwrap(), *planes[0]);
        assert_eq!(*planes[1], [100, 101, 104, 105], "chroma 2x2 from (0,0)");
        // even corner: chroma starts at (1,1); the last chroma column of an odd width is kept
        let c = yuv(Chroma::C420).cropped(Region { x: 2, y: 2, w: 5, h: 3 });
        let PixelData::Yuv8 { planes, .. } = &c.data else { panic!() };
        assert_eq!((c.width, c.height), (5, 3));
        assert_eq!(*planes[1], [105, 106, 107, 109, 110, 111], "chroma 3x2 from (1,1)");
        // 4:4:4 needs no alignment
        let c = yuv(Chroma::C444).cropped(Region { x: 1, y: 1, w: 2, h: 1 });
        let PixelData::Yuv8 { planes, .. } = &c.data else { panic!() };
        assert_eq!(*planes[0], [8, 9]);
        // a plane shorter than its size gives a blank picture, never a panic
        let mut short = yuv(Chroma::C420);
        if let PixelData::Yuv8 { planes, .. } = &mut short.data {
            planes[0] = Arc::new(vec![1; 3]);
        }
        let c = short.cropped(Region { x: 2, y: 2, w: 2, h: 2 });
        let PixelData::Yuv8 { planes, .. } = &c.data else { panic!() };
        assert_eq!(*planes[0], [0, 0, 0, 0]);
    }

    #[test]
    fn rotation_turns_every_plane_clockwise() {
        // 4x2 RGBA8 with distinct pixels 0..8
        let px: Vec<u8> = (0..8u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let f = VideoFrame { par: (4, 3), ..VideoFrame::rgba8(4, 2, px) };
        let r = f.rotated(1);
        assert_eq!((r.width, r.height, r.par), (2, 4, (3, 4)));
        let red: Vec<u8> = r.to_rgba8().chunks(4).map(|p| p[0]).collect();
        // source rows [0 1 2 3] / [4 5 6 7] turned clockwise: the bottom row becomes the left column
        assert_eq!(red, [4, 0, 5, 1, 6, 2, 7, 3]);
        let red180: Vec<u8> = f.rotated(2).to_rgba8().chunks(4).map(|p| p[0]).collect();
        assert_eq!(red180, [7, 6, 5, 4, 3, 2, 1, 0]);
        let red270: Vec<u8> = f.rotated(3).to_rgba8().chunks(4).map(|p| p[0]).collect();
        assert_eq!(red270, [3, 7, 2, 6, 1, 5, 0, 4]);
        assert_eq!(f.rotated(4).width, 4);

        // 4:2:0 4x2: chroma 2x1 -> 1x2; 4:2:2 4x2: chroma 2x2 -> widened 4:4:4 2x4
        let yuv = |chroma: Chroma, c: Vec<u8>| VideoFrame {
            width: 4,
            height: 2,
            data: PixelData::Yuv8 { planes: [Arc::new((0..8).collect()), Arc::new(c.clone()), Arc::new(c)], chroma, alpha: None },
            color: ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let r = yuv(Chroma::C420, vec![10, 20]).rotated(1);
        let PixelData::Yuv8 { planes, chroma, .. } = &r.data else { panic!() };
        assert_eq!((*chroma, &planes[0][..], &planes[1][..]), (Chroma::C420, &[4, 0, 5, 1, 6, 2, 7, 3][..], &[10, 20][..]));
        let r = yuv(Chroma::C422, vec![10, 20, 30, 40]).rotated(1);
        let PixelData::Yuv8 { planes, chroma, .. } = &r.data else { panic!() };
        assert_eq!((*chroma, &planes[1][..]), (Chroma::C444, &[30, 10, 30, 10, 40, 20, 40, 20][..]));
    }

    #[test]
    fn box_decimation_averages_blocks_per_plane() {
        // 6x4 4:2:0: luma ramp, chroma 3x2
        let y: Vec<u8> = (0..24).map(|i| (i * 10) as u8).collect();
        let u = vec![10, 20, 30, 40, 50, 60];
        let v = vec![200; 6];
        let f = VideoFrame {
            width: 6,
            height: 4,
            data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None },
            color: ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let d = f.box_decimated(2).expect("yuv");
        assert_eq!((d.width, d.height), (3, 2));
        let PixelData::Yuv8 { planes, .. } = &d.data else { panic!() };
        // luma block (0,0): 0, 10, 60, 70 -> 35
        assert_eq!(planes[0][0], 35);
        assert_eq!(planes[0].len(), 6);
        // chroma 3x2 -> 2x1: blocks {10,20,40,50} = 30 and the clamped edge block {30,60} = 45
        assert_eq!(&planes[1][..], &[30, 45]);
        assert_eq!(&planes[2][..], &[200, 200]);
        assert!(f.box_decimated(1).is_none());
        assert!(VideoFrame::rgba8(2, 2, vec![0; 16]).box_decimated(2).is_none());
    }
    use super::*;

    #[test]
    fn rgba8_roundtrip_through_linear() {
        let px: Vec<u8> = (0..=255u8).flat_map(|v| [v, 255 - v, v / 2, 255]).collect();
        let f = VideoFrame::rgba8(256, 1, px.clone());
        let lin = f.to_linear_f32();
        let back = VideoFrame::rgba_f32(256, 1, lin).to_rgba8();
        assert_eq!(back, px);
    }

    #[test]
    fn yuv_grey_is_grey() {
        let (w, h) = (4, 2);
        let y = Arc::new(vec![126u8; w * h]);
        let u = Arc::new(vec![128u8; 2]);
        let v = Arc::new(vec![128u8; 2]);
        let f = VideoFrame {
            width: 4,
            height: 2,
            data: PixelData::Yuv8 { planes: [y, u, v], chroma: Chroma::C420, alpha: None },
            color: ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let rgb = f.to_rgba8();
        assert_eq!(&rgb[0..4], &[128, 128, 128, 255]);
        // slow path agrees within 1
        let slow = VideoFrame::rgba_f32(4, 2, f.to_linear_f32()).to_rgba8();
        assert!((slow[0] as i32 - 128).abs() <= 1);
    }

    #[test]
    fn audio_mix() {
        let mut a = AudioBuffer::silence(48000, 2, 4);
        let b = AudioBuffer { sample_rate: 48000, channels: vec![vec![0.5; 4]] };
        a.mix_from(&b, &[1.0, 0.5]);
        assert_eq!(a.channels[1][0], 0.25);
        assert_eq!(a.interleaved()[..2], [0.5, 0.25]);
    }

    /// Frames of every pixel format with partial and zero alpha, at a size that is not a multiple
    /// of the decimation factors.
    fn sample_frames() -> Vec<(&'static str, VideoFrame)> {
        sample_frames_sized(37, 23)
    }

    fn sample_frames_sized(w: usize, h: usize) -> Vec<(&'static str, VideoFrame)> {
        let alpha = |x: usize, y: usize| -> u8 {
            if y < 9 || x < 6 {
                0
            } else if y == 9 || x == 6 {
                77
            } else {
                255
            }
        };
        let rgba8: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7 % 251) as u8, (i * 13 % 241) as u8, (i * 29 % 239) as u8, alpha(i % w, i / w)]).collect();
        let f32s: Vec<f32> =
            (0..w * h).flat_map(|i| [(i % 17) as f32 / 17.0, (i % 5) as f32 / 5.0, (i % 11) as f32 / 11.0, alpha(i % w, i / w) as f32 / 255.0]).collect();
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let y8: Vec<u8> = (0..w * h).map(|i| (16 + i * 3 % 219) as u8).collect();
        let u8p: Vec<u8> = (0..cw * ch).map(|i| (16 + i * 5 % 224) as u8).collect();
        let v8p: Vec<u8> = (0..cw * ch).map(|i| (16 + i * 11 % 224) as u8).collect();
        let a8: Vec<u8> = (0..w * h).map(|i| alpha(i % w, i / w)).collect();
        let y16: Vec<u16> = (0..w * h).map(|i| (64 + i * 3 % 876) as u16).collect();
        let c16: Vec<u16> = (0..w * h).map(|i| (64 + i * 7 % 896) as u16).collect();
        let a16: Vec<u16> = (0..w * h).map(|i| (alpha(i % w, i / w) as u16) * 4).collect();
        let c422: Vec<u16> = (0..cw * h).map(|i| (64 + i * 7 % 896) as u16).collect();
        let mk = |data| VideoFrame { width: w as u32, height: h as u32, data, color: ColorInfo::REC709, par: (1, 1), pts: Tick::ZERO };
        vec![
            ("rgba8", mk(PixelData::Rgba8(Arc::new(rgba8)))),
            ("rgba f32", mk(PixelData::RgbaF32(Arc::new(f32s)))),
            (
                "yuv8 4:2:0 + alpha",
                mk(PixelData::Yuv8 {
                    planes: [Arc::new(y8.clone()), Arc::new(u8p.clone()), Arc::new(v8p.clone())],
                    chroma: Chroma::C420,
                    alpha: Some(Arc::new(a8)),
                }),
            ),
            ("yuv8 4:2:0", mk(PixelData::Yuv8 { planes: [Arc::new(y8), Arc::new(u8p), Arc::new(v8p)], chroma: Chroma::C420, alpha: None })),
            (
                "yuv16 4:4:4 + alpha",
                mk(PixelData::Yuv16 {
                    planes: [Arc::new(y16.clone()), Arc::new(c16.clone()), Arc::new(c16.clone())],
                    chroma: Chroma::C444,
                    bits: 10,
                    alpha: Some(Arc::new(a16.clone())),
                }),
            ),
            (
                "yuv16 4:2:2 + alpha",
                mk(PixelData::Yuv16 {
                    planes: [Arc::new(y16.clone()), Arc::new(c422.clone()), Arc::new(c422)],
                    chroma: Chroma::C422,
                    bits: 10,
                    alpha: Some(Arc::new(a16)),
                }),
            ),
            (
                "yuv16 4:4:4",
                mk(PixelData::Yuv16 { planes: [Arc::new(y16), Arc::new(c16.clone()), Arc::new(c16)], chroma: Chroma::C444, bits: 12, alpha: None }),
            ),
        ]
    }

    #[test]
    fn a_region_is_exactly_the_crop_of_the_full_conversion() {
        for (name, f) in sample_frames() {
            for n in [1usize, 2, 3] {
                let (ow, oh, full) = f.to_linear_f32_decimated_with(n, None);
                for want in [
                    Region { x: 0, y: 0, w: ow, h: oh },
                    Region { x: 0, y: 0, w: 1, h: 1 },
                    Region { x: ow - 1, y: oh - 1, w: 1, h: 1 },
                    Region { x: 3, y: 2, w: ow - 5, h: oh - 4 },
                    Region { x: 0, y: oh / 2, w: ow, h: 1 },
                    Region { x: ow / 2, y: 0, w: 1, h: oh },
                    // sticks out of the picture: clipped
                    Region { x: ow - 2, y: oh - 2, w: 10, h: 10 },
                ] {
                    let (r, px) = f.to_linear_f32_region(n, None, want);
                    assert_eq!(px.len(), r.w * r.h * 4, "{name} n={n} {want:?}");
                    assert_eq!(r, want.clip(ow, oh));
                    for ry in 0..r.h {
                        for rx in 0..r.w {
                            for k in 0..4 {
                                let (a, b) = (px[(ry * r.w + rx) * 4 + k], full[((r.y + ry) * ow + r.x + rx) * 4 + k]);
                                assert_eq!(a.to_bits(), b.to_bits(), "{name} n={n} {r:?} at ({rx},{ry}) channel {k}: {a} vs {b}");
                            }
                        }
                    }
                }
                // outside the picture altogether: nothing, and no panic
                let (r, px) = f.to_linear_f32_region(n, None, Region { x: ow + 5, y: 0, w: 4, h: 4 });
                assert!(r.is_empty() && px.is_empty(), "{name}");
            }
        }
    }

    /// The conversions write every float they hand out: into a buffer that held something else
    /// (here NaN, which the pool gives back as it was) the result is the one a fresh buffer gives.
    #[test]
    fn a_recycled_buffer_never_leaks_its_old_contents() {
        // big enough for the float images to be worth keeping (64 KiB), at sizes no other test of this
        // binary converts, so the poisoned buffer is the one the next conversion takes
        let poison = |len: usize| pool::recycle_f32(vec![f32::NAN; len]);
        let same = |what: &str, fresh: &[f32], reused: &[f32]| {
            assert_eq!(fresh.len(), reused.len(), "{what}");
            if let Some(i) = fresh.iter().zip(reused).position(|(a, b)| a.to_bits() != b.to_bits()) {
                panic!("{what}: element {i} is {} in a fresh buffer and {} in a recycled one", fresh[i], reused[i]);
            }
        };
        for (name, f) in sample_frames_sized(203, 151) {
            for n in [1usize, 2] {
                let (ow, oh) = f.decimated_size(n);
                let (_, _, fresh) = f.to_linear_f32_decimated(n);
                poison(ow * oh * 4);
                let (_, _, reused) = f.to_linear_f32_decimated(n);
                same(&format!("{name} n={n}, whole picture"), &fresh, &reused);

                let want = Region { x: 1, y: 1, w: 70, h: 60 };
                let (r, fresh) = f.to_linear_f32_region(n, None, want);
                assert_eq!(r, want, "{name} n={n}");
                poison(r.w * r.h * 4);
                let (_, reused) = f.to_linear_f32_region(n, None, want);
                same(&format!("{name} n={n}, rectangle"), &fresh, &reused);
            }
        }
        // a frame with less data than its size says (no row of it fits): transparent, never what the buffer held
        let short = VideoFrame { data: PixelData::RgbaF32(Arc::new(vec![0.5; 100])), ..VideoFrame::rgba_f32(203, 151, vec![0.0; 203 * 151 * 4]) };
        poison(203 * 151 * 4);
        let (_, _, px) = short.to_linear_f32_decimated(1);
        assert!(px.iter().all(|v| *v == 0.0), "rows without data are transparent");
    }

    #[test]
    fn alpha_region_is_the_box_around_what_is_not_transparent() {
        for (name, f) in sample_frames() {
            for n in [1usize, 2, 4] {
                let got = f.alpha_region(n);
                let has_alpha = matches!(f.data, PixelData::Rgba8(_) | PixelData::Yuv8 { alpha: Some(_), .. } | PixelData::Yuv16 { alpha: Some(_), .. });
                assert_eq!(got.is_some(), has_alpha, "{name}");
                let Some(got) = got else { continue };
                // the same box measured on the converted picture: pixels whose alpha is not zero
                let (ow, oh, px) = f.to_linear_f32_decimated(n);
                let on: Vec<(usize, usize)> = (0..oh).flat_map(|y| (0..ow).map(move |x| (x, y))).filter(|(x, y)| px[(y * ow + x) * 4 + 3] != 0.0).collect();
                let want = match (on.iter().map(|p| p.0).min(), on.iter().map(|p| p.0).max(), on.iter().map(|p| p.1).min(), on.iter().map(|p| p.1).max()) {
                    (Some(x0), Some(x1), Some(y0), Some(y1)) => Region { x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 },
                    _ => Region::default(),
                };
                assert_eq!(got, want, "{name} n={n}");
                // and everything outside it is exactly transparent black
                for y in 0..oh {
                    for x in 0..ow {
                        if !(x >= got.x && x < got.x + got.w && y >= got.y && y < got.y + got.h) {
                            assert_eq!(&px[(y * ow + x) * 4..(y * ow + x) * 4 + 4], &[0.0; 4], "{name} n={n} at ({x},{y})");
                        }
                    }
                }
            }
        }
        // a picture that is transparent everywhere
        let clear = VideoFrame {
            data: PixelData::Yuv8 {
                planes: [Arc::new(vec![16; 64]), Arc::new(vec![128; 16]), Arc::new(vec![128; 16])],
                chroma: Chroma::C420,
                alpha: Some(Arc::new(vec![0; 64])),
            },
            ..VideoFrame::rgba8(8, 8, vec![0; 256])
        };
        assert_eq!(clear.alpha_region(1), Some(Region::default()));
        // malformed alpha (too short): nothing can be assumed
        let short = VideoFrame {
            data: PixelData::Yuv8 {
                planes: [Arc::new(vec![16; 64]), Arc::new(vec![128; 16]), Arc::new(vec![128; 16])],
                chroma: Chroma::C420,
                alpha: Some(Arc::new(vec![255; 10])),
            },
            ..VideoFrame::rgba8(8, 8, vec![0; 256])
        };
        assert_eq!(short.alpha_region(1), None);
    }
}
