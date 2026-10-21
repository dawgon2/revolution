//! Still images (PNG, JPEG, GIF (first frame), WebP, TIFF, BMP, Targa, DPX) as media sources.

use std::sync::Arc;

use filmcraft_frame::VideoFrame;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

use crate::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, VideoStreamInfo};

/// Default still duration (Premiere's preference default is 5 seconds).
pub const DEFAULT_STILL_DURATION: Tick = Tick(5 * TICKS_PER_SECOND);

/// Whether `b` (the contents of the file `name`) is a still image we decode.
pub fn sniff(name: &str, b: &[u8]) -> bool {
    crate::dpx::sniff(b)
        || crate::tga::sniff(name, b)
        || b.starts_with(b"\x89PNG")
        || b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"GIF8")
        || (b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP")
        || b.starts_with(b"II*\0")
        || b.starts_with(b"MM\0*")
        || b.starts_with(b"BM")
}

/// A decoded picture: straight-alpha RGBA8, top row first.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Whether any pixel is not fully opaque.
    pub has_alpha: bool,
}

/// Decode a still image file (`name` picks Targa, which has no magic number; the content picks
/// every other format).
pub fn decode_raster(name: &str, bytes: &[u8]) -> Result<Raster> {
    if crate::dpx::sniff(bytes) {
        return crate::dpx::decode(name, bytes);
    }
    if crate::tga::sniff(name, bytes) {
        return crate::tga::decode(name, bytes);
    }
    let img = image::load_from_memory(bytes).map_err(|e| MediaError::Decode(format!("{name}: {e}")))?;
    let has_alpha = img.color().has_alpha();
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Raster { width, height, rgba: rgba.into_raw(), has_alpha })
}

/// Decode like [`decode_raster`], refusing pictures wider or taller than `max_dim` or whose
/// decode would allocate more than `max_alloc` bytes (hostile or absurd files), before decoding.
pub fn decode_raster_capped(name: &str, bytes: &[u8], max_dim: u32, max_alloc: u64) -> Result<Raster> {
    let too_big = |w: u32, h: u32| w > max_dim || h > max_dim || u64::from(w) * u64::from(h) * 4 > max_alloc;
    if crate::dpx::sniff(bytes) || crate::tga::sniff(name, bytes) {
        // the size the header declares, checked before anything is decoded or allocated
        let declared = if crate::dpx::sniff(bytes) {
            let at = |o: usize| bytes.get(o..o + 4).and_then(|b| <[u8; 4]>::try_from(b).ok());
            let big = bytes.get(0..4) == Some(b"SDPX".as_slice());
            let rd = |o: usize| at(o).map(|v| if big { u32::from_be_bytes(v) } else { u32::from_le_bytes(v) });
            rd(772).zip(rd(776))
        } else {
            let at = |o: usize| bytes.get(o..o + 2).and_then(|b| <[u8; 2]>::try_from(b).ok()).map(u16::from_le_bytes).map(u32::from);
            at(12).zip(at(14))
        };
        match declared {
            Some((w, h)) if !too_big(w, h) => {}
            _ => return Err(MediaError::Decode(format!("{name}: the picture is larger than {max_dim} px or its header is damaged"))),
        }
        let r = decode_raster(name, bytes)?;
        if too_big(r.width, r.height) {
            return Err(MediaError::Decode(format!("{name}: the picture is larger than {max_dim} px")));
        }
        return Ok(r);
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| MediaError::Decode(format!("{name}: {e}")))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_dim);
    limits.max_image_height = Some(max_dim);
    limits.max_alloc = Some(max_alloc);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| MediaError::Decode(format!("{name}: {e}")))?;
    let has_alpha = img.color().has_alpha();
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 || too_big(width, height) {
        return Err(MediaError::Decode(format!("{name}: the picture is empty or larger than {max_dim} px")));
    }
    Ok(Raster { width, height, rgba: rgba.into_raw(), has_alpha })
}

pub struct StillSource {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
}

impl StillSource {
    pub fn decode(name: &str, bytes: &[u8]) -> Result<Self> {
        let r = decode_raster(name, bytes)?;
        Ok(Self::from_rgba(name, r.width, r.height, r.rgba, r.has_alpha, bytes.len() as u64))
    }

    pub fn from_rgba(name: &str, w: u32, h: u32, rgba: Vec<u8>, has_alpha: bool, file_size: u64) -> Self {
        let codec = name.rsplit('.').next().unwrap_or("image").to_ascii_uppercase();
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::Still,
            duration: DEFAULT_STILL_DURATION,
            video: Some(VideoStreamInfo {
                camera_color_space: None,
                width: w,
                height: h,
                frame_rate: FrameRate::FPS_29_97,
                par: (1, 1),
                codec,
                pixel_format: if has_alpha { "RGBA 8-bit".into() } else { "RGB 8-bit".into() },
                color: filmcraft_color::ColorInfo::SRGB_FULL,
                has_alpha,
                bitrate: None,
                hdr: None,
            }),
            audio_streams: Vec::new(),
            container: "Image".into(),
            start_timecode: None,
            file_size: Some(file_size),
        };
        Self { info, frame: Arc::new(VideoFrame::rgba8(w, h, rgba)) }
    }
}

impl MediaSource for StillSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> Result<Arc<VideoFrame>> {
        Ok(self.frame.clone())
    }
    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<filmcraft_frame::AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
}
