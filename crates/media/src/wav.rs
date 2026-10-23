//! Minimal WAV (RIFF/RF64 PCM 8/16/24/32-bit int, 32/64-bit float) source, with the Broadcast Wave
//! start time: `bext` TimeReference (EBU Tech 3285), else iXML `BWF_TIME_REFERENCE_LOW/HIGH`. The
//! full `riff` crate (all BWF metadata, AIFF) will replace this.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_time::Tick;

use crate::reader::{MemReader, SharedReader, read_range};
use crate::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result};

pub fn sniff(b: &[u8]) -> bool {
    b.len() >= 12 && (&b[0..4] == b"RIFF" || &b[0..4] == b"RF64") && &b[8..12] == b"WAVE"
}

/// Chunks walked looking for `fmt `/`data`/`bext`/`iXML` (real files have a handful).
const MAX_CHUNKS: usize = 1 << 16;
/// Bytes of an iXML chunk searched for the time reference.
const IXML_MAX: u64 = 1 << 20;
/// Output frames converted per read of the data chunk.
const BLOCK_FRAMES: usize = 4096;
/// Largest single read of the data chunk; a wider span (an extreme resampling ratio) reads the
/// two frames each output frame needs instead.
const MAX_SPAN_BYTES: u64 = 4 << 20;

/// A WAV file read in place: opening it reads its chunk headers (not its samples), and
/// [`MediaSource::audio`] reads only the sample range it is asked for, so a 90-minute recording
/// costs its header rather than its size (#279).
pub struct WavSource {
    info: MediaInfo,
    reader: SharedReader,
    data_off: u64,
    /// Whole frames only.
    frames: u64,
    channels: usize,
    bits: u16,
    float: bool,
    rate: u32,
    time_reference: Option<u64>,
}

fn le16(b: &[u8], o: usize) -> u16 {
    b.get(o..o.saturating_add(2)).and_then(|s| s.try_into().ok()).map_or(0, u16::from_le_bytes)
}
fn le32(b: &[u8], o: usize) -> u32 {
    b.get(o..o.saturating_add(4)).and_then(|s| s.try_into().ok()).map_or(0, u32::from_le_bytes)
}

impl WavSource {
    /// Parse a WAV file held in memory.
    pub fn parse(name: &str, bytes: Arc<[u8]>) -> Result<Self> {
        Self::open(name, Arc::new(MemReader(bytes)))
    }

    /// Open a WAV file through a random-access reader.
    pub fn open(name: &str, reader: SharedReader) -> Result<Self> {
        let io = |e: std::io::Error| MediaError::Io(format!("{name}: {e}"));
        let len = reader.len();
        let mut pos: u64 = 12;
        let mut fmt = None;
        let mut data = None;
        let mut bext_tr = None;
        let mut ixml_tr = None;
        for _ in 0..MAX_CHUNKS {
            let Some(body) = pos.checked_add(8).filter(|&b| b <= len) else { break };
            let mut hdr = [0u8; 8];
            reader.read_at(pos, &mut hdr).map_err(io)?;
            let id = &hdr[0..4];
            let mut clen = u64::from(le32(&hdr, 4));
            let rest = len - body;
            if id == b"data" && (clen == 0xFFFF_FFFF || clen > rest) {
                clen = rest;
            }
            if clen > rest {
                break;
            }
            match id {
                b"fmt " if clen >= 16 => {
                    let b = read_range(&*reader, body, clen.min(40) as usize).map_err(io)?;
                    let mut tag = le16(&b, 0);
                    let ch = le16(&b, 2);
                    let rate = le32(&b, 4);
                    let bits = le16(&b, 14);
                    if tag == 0xFFFE && clen >= 40 {
                        tag = le16(&b, 24);
                    }
                    fmt = Some((tag, ch, rate, bits));
                }
                b"data" => data = Some((body, clen)),
                // bext: Description 256, Originator 32, OriginatorReference 32, OriginationDate 10,
                // OriginationTime 8, then TimeReference (low u32, high u32)
                b"bext" if clen >= 346 => {
                    let b = read_range(&*reader, body + 338, 8).map_err(io)?;
                    bext_tr = Some(u64::from(le32(&b, 0)) | u64::from(le32(&b, 4)) << 32);
                }
                b"iXML" => {
                    let b = read_range(&*reader, body, clen.min(IXML_MAX) as usize).map_err(io)?;
                    ixml_tr = ixml_time_reference(&b);
                }
                _ => {}
            }
            pos = body + clen + (clen & 1);
        }
        let (tag, ch, rate, bits) = fmt.ok_or_else(|| MediaError::Decode(format!("{name}: missing fmt chunk")))?;
        let (off, dlen) = data.ok_or_else(|| MediaError::Decode(format!("{name}: missing data chunk")))?;
        let float = tag == 3;
        if !(tag == 1 || float) || ch == 0 || !matches!(bits, 8 | 16 | 24 | 32 | 64) {
            return Err(MediaError::Unsupported(format!("{name}: WAV format tag {tag}, {bits} bits")));
        }
        let frame_bytes = u64::from(ch) * u64::from(bits / 8);
        let frames = dlen / frame_bytes;
        let time_reference = bext_tr.or(ixml_tr);
        let mut info = MediaInfo {
            name: name.into(),
            kind: MediaKind::AudioOnly,
            duration: Tick::from_units(i64::try_from(frames).unwrap_or(i64::MAX), rate as i64),
            video: None,
            audio_streams: vec![AudioStreamInfo {
                sample_rate: rate,
                channels: ch as u32,
                codec: if float { "PCM float".into() } else { "PCM".into() },
                bits_per_sample: Some(bits as u32),
            }],
            container: if time_reference.is_some() { "Broadcast WAV".into() } else { "WAV".into() },
            start_timecode: None,
            file_size: Some(len),
        };
        // start timecode: frames at the media's frame rate (the default rate for audio-only media)
        if let Some(tr) = time_reference.filter(|_| rate > 0) {
            let r = info.frame_rate();
            let n = tr as i128 * r.num as i128;
            let d = rate as i128 * r.den as i128;
            info.start_timecode = (n + d / 2).checked_div(d).and_then(|t| i64::try_from(t).ok());
        }
        Ok(Self { info, reader, data_off: off, frames, channels: ch as usize, bits, float, rate, time_reference })
    }

    /// Broadcast Wave start time: samples since midnight.
    pub fn time_reference(&self) -> Option<u64> {
        self.time_reference
    }

    fn frame_bytes(&self) -> usize {
        self.channels * (self.bits as usize / 8)
    }

    /// Read whole frames `first..=last` of the data chunk.
    fn read_frames(&self, first: u64, last: u64) -> Result<Vec<u8>> {
        let fb = self.frame_bytes() as u64;
        let n = last.saturating_sub(first).saturating_add(1).saturating_mul(fb);
        let off = self.data_off.saturating_add(first.saturating_mul(fb));
        let mut buf = vec![0u8; usize::try_from(n).map_err(|_| MediaError::Decode(format!("{}: audio read too large", self.info.name)))?];
        self.reader.read_at(off, &mut buf).map_err(|e| MediaError::Io(format!("{}: {e}", self.info.name)))?;
        Ok(buf)
    }

    /// Sample `ch` of frame `frame` of `block`, a run of whole frames.
    fn sample(&self, block: &[u8], frame: usize, ch: usize) -> f32 {
        let bps = self.bits as usize / 8;
        let o = (frame * self.channels + ch) * bps;
        let Some(b) = block.get(o..o.saturating_add(bps)) else { return 0.0 };
        match (self.bits, self.float, b) {
            (8, _, &[a]) => (a as f32 - 128.0) / 128.0,
            (16, _, &[a, b]) => i16::from_le_bytes([a, b]) as f32 / 32768.0,
            (24, _, &[a, b, c]) => ((i32::from_le_bytes([0, a, b, c]) >> 8) as f32) / 8_388_608.0,
            (32, false, &[a, b, c, d]) => i32::from_le_bytes([a, b, c, d]) as f32 / 2_147_483_648.0,
            (32, true, &[a, b, c, d]) => f32::from_le_bytes([a, b, c, d]),
            (64, true, b) => f64::from_le_bytes(b.try_into().unwrap_or([0; 8])) as f32,
            _ => 0.0,
        }
    }
}

impl MediaSource for WavSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> Result<Arc<VideoFrame>> {
        Err(MediaError::NoStream("video"))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        let total = self.frames;
        let mut out = AudioBuffer::silence(sample_rate, self.channels, frames);
        if total == 0 || sample_rate == 0 {
            return Ok(out);
        }
        // Linear-interpolating resample when rates differ (the audio crate provides sinc resampling).
        let ratio = self.rate as f64 / sample_rate as f64;
        let src_of = |i: usize| start.saturating_add(i as i64) as f64 * ratio;
        let mut i = 0;
        while i < frames {
            let end = frames.min(i + BLOCK_FRAMES);
            let (lo, hi) = (src_of(i), src_of(end - 1));
            if hi < 0.0 {
                i = end;
                continue;
            }
            let first = lo.max(0.0).floor() as u64;
            if first >= total {
                break;
            }
            let last = (hi.floor() as u64).saturating_add(1).min(total - 1);
            let span = last.saturating_sub(first).saturating_add(1).saturating_mul(self.frame_bytes() as u64);
            let block = if span <= MAX_SPAN_BYTES { Some(self.read_frames(first, last)?) } else { None };
            for slot in i..end {
                let src = src_of(slot);
                if src < 0.0 {
                    continue;
                }
                let i0 = src.floor() as u64;
                if i0 >= total {
                    break;
                }
                let i1 = (i0 + 1).min(total - 1);
                let fr = (src - i0 as f64) as f32;
                let (buf, base) = match &block {
                    Some(b) => (std::borrow::Cow::Borrowed(b.as_slice()), first),
                    None => (std::borrow::Cow::Owned(self.read_frames(i0, i1)?), i0),
                };
                let (k0, k1) = ((i0 - base) as usize, (i1 - base) as usize);
                for c in 0..self.channels {
                    let a = self.sample(&buf, k0, c);
                    let b = self.sample(&buf, k1, c);
                    if let Some(s) = out.channels.get_mut(c).and_then(|ch| ch.get_mut(slot)) {
                        *s = a + (b - a) * fr;
                    }
                }
            }
            i = end;
        }
        Ok(out)
    }
}

/// TimeReference from an iXML chunk (`<BWF_TIME_REFERENCE_LOW>` / `<BWF_TIME_REFERENCE_HIGH>`).
fn ixml_time_reference(x: &[u8]) -> Option<u64> {
    let text = String::from_utf8_lossy(x);
    let field = |tag: &str| -> Option<u64> {
        let open = format!("<{tag}>");
        let a = text.find(&open)? + open.len();
        let b = a + text[a..].find('<')?;
        text[a..b].trim().parse().ok()
    };
    let low = field("BWF_TIME_REFERENCE_LOW")?;
    Some(low | field("BWF_TIME_REFERENCE_HIGH").unwrap_or(0) << 32)
}

/// The most sample data a 16-bit WAV can hold: its RIFF sizes are 32-bit.
pub const WAV16_MAX_DATA_BYTES: u64 = u32::MAX as u64 - 36;

/// The 44-byte header of a 16-bit PCM WAV with `data_len` bytes of samples; `None` when a size
/// doesn't fit its 32-bit field (never wrapped).
fn wav16_header(data_len: u64, channels: u16, rate: u32) -> Option<[u8; 44]> {
    if data_len > WAV16_MAX_DATA_BYTES || channels == 0 {
        return None;
    }
    let riff = u32::try_from(36 + data_len).ok()?;
    let data = u32::try_from(data_len).ok()?;
    let block = channels.checked_mul(2)?;
    let byte_rate = rate.checked_mul(u32::from(block))?;
    let mut h = [0u8; 44];
    let parts: [&[u8]; 12] = [
        b"RIFF",
        &riff.to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
        &1u16.to_le_bytes(),
        &channels.to_le_bytes(),
        &rate.to_le_bytes(),
        &byte_rate.to_le_bytes(),
        &block.to_le_bytes(),
        &16u16.to_le_bytes(),
        b"data",
        &data.to_le_bytes(),
    ];
    let mut i = 0;
    for p in parts {
        h.get_mut(i..i + p.len())?.copy_from_slice(p);
        i += p.len();
    }
    Some(h)
}

/// Encode interleaved f32 samples as a 16-bit PCM WAV file. Input beyond what a WAV can hold
/// (4 GB of samples) or a sample rate / channel count whose header fields overflow is cut to
/// what fits (the header is never wrapped); [`try_write_wav16_planar`] refuses instead.
pub fn write_wav16(samples: &[f32], channels: u16, rate: u32) -> Vec<u8> {
    let channels = channels.clamp(1, 1024);
    let rate = rate.min(u32::MAX / 2048);
    let max = (WAV16_MAX_DATA_BYTES / 2 / u64::from(channels) * u64::from(channels)) as usize;
    let samples = samples.get(..samples.len().min(max)).unwrap_or_default();
    let data_len = samples.len() as u64 * 2;
    let Some(h) = wav16_header(data_len, channels, rate) else { return Vec::new() };
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(&h);
    for s in samples {
        v.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    v
}

/// Encode equal-length planar f32 channels as a 16-bit PCM WAV, without an interleaved copy.
/// Errors (never wraps or aborts) when the result wouldn't fit a WAV's 32-bit sizes or memory.
pub fn try_write_wav16_planar(channels: &[&[f32]], rate: u32) -> std::result::Result<Vec<u8>, String> {
    let n = channels.first().map_or(0, |c| c.len());
    let ch = u16::try_from(channels.len()).ok().filter(|c| *c > 0).ok_or("1 to 65535 channels are supported")?;
    if channels.iter().any(|c| c.len() != n) {
        return Err("channels of different lengths".into());
    }
    let data_len = (n as u64).checked_mul(u64::from(ch)).and_then(|v| v.checked_mul(2)).ok_or("too much audio for one WAV file")?;
    let h = wav16_header(data_len, ch, rate).ok_or_else(|| format!("{} MB of audio is more than one WAV file can hold (4 GB)", data_len / 1_000_000))?;
    let total = usize::try_from(data_len + 44).map_err(|_| "too much audio for one WAV file")?;
    let mut v = Vec::new();
    v.try_reserve_exact(total).map_err(|_| "not enough memory for the WAV file")?;
    v.extend_from_slice(&h);
    for i in 0..n {
        for c in channels {
            let s = c.get(i).copied().filter(|s| s.is_finite()).unwrap_or(0.0);
            v.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav16_sizes_never_wrap() {
        // planar = interleaved
        let (l, r) = ([0.5f32, -0.25, 1.0], [0.0f32, 0.75, f32::NAN]);
        let p = try_write_wav16_planar(&[&l, &r], 48_000).unwrap();
        let i = write_wav16(&[0.5, 0.0, -0.25, 0.75, 1.0, 0.0], 2, 48_000);
        assert_eq!(p, i);
        assert_eq!(&p[40..44], &12u32.to_le_bytes());
        // a size past the 32-bit fields is refused, never wrapped
        assert!(wav16_header(WAV16_MAX_DATA_BYTES, 2, 48_000).is_some());
        assert!(wav16_header(WAV16_MAX_DATA_BYTES + 1, 2, 48_000).is_none());
        assert!(wav16_header(1 << 33, 8, 48_000).is_none());
        assert!(wav16_header(4, 1, u32::MAX).is_none());
        assert!(try_write_wav16_planar(&[&l, &r[..2]], 48_000).is_err());
        assert!(try_write_wav16_planar(&[], 48_000).is_err());
        // the legacy writer clamps hostile header values instead of overflowing
        let w = write_wav16(&[0.1; 4], u16::MAX, u32::MAX);
        assert!(w.len() >= 44);
    }

    #[test]
    fn roundtrip() {
        let s: Vec<f32> = (0..200).map(|i| ((i as f32) * 0.1).sin() * 0.5).collect();
        let bytes = write_wav16(&s, 2, 48000);
        let src = WavSource::parse("t.wav", bytes.into()).unwrap();
        assert_eq!(src.info().audio().unwrap().channels, 2);
        let a = src.audio(0, 100, 48000).unwrap();
        assert!((a.channels[0][3] - s[6]).abs() < 1e-4);
        assert!((a.channels[1][3] - s[7]).abs() < 1e-4);
        assert_eq!(src.info().duration, Tick::from_units(100, 48000));
        assert_eq!(src.time_reference(), None);
        assert_eq!(src.info().start_timecode, None);
    }

    /// Insert a chunk before `data`.
    fn with_chunk(wav: &[u8], id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = wav[..36].to_vec();
        v.extend_from_slice(id);
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v.extend_from_slice(&wav[36..]);
        let riff = (v.len() - 8) as u32;
        v[4..8].copy_from_slice(&riff.to_le_bytes());
        v
    }

    #[test]
    fn bwf_time_reference_from_bext() {
        let wav = write_wav16(&[0.0; 96], 2, 48_000);
        // 01:00:00:00 at 23.976 fps: 86 400 frames of 2002 samples
        let tr: u64 = 86_400 * 2002;
        let mut bext = vec![0u8; 602];
        bext[338..342].copy_from_slice(&(tr as u32).to_le_bytes());
        bext[342..346].copy_from_slice(&((tr >> 32) as u32).to_le_bytes());
        let src = WavSource::parse("bwf.wav", with_chunk(&wav, b"bext", &bext).into()).unwrap();
        assert_eq!(src.time_reference(), Some(tr));
        assert_eq!(src.info().frame_rate(), filmcraft_time::FrameRate::FPS_23_976);
        assert_eq!(src.info().start_timecode, Some(86_400));
        assert_eq!(src.info().container, "Broadcast WAV");
        // the audio is unaffected
        assert_eq!(src.info().duration, Tick::from_units(48, 48_000));
    }

    #[test]
    fn bwf_time_reference_from_ixml_and_large_values() {
        let wav = write_wav16(&[0.0; 4], 1, 48_000);
        let tr: u64 = (1 << 32) + 5;
        let xml = format!(
            "<?xml version=\"1.0\"?><BWFXML><BEXT><BWF_TIME_REFERENCE_LOW>{}</BWF_TIME_REFERENCE_LOW><BWF_TIME_REFERENCE_HIGH>{}</BWF_TIME_REFERENCE_HIGH></BEXT></BWFXML>",
            tr & 0xFFFF_FFFF,
            tr >> 32
        );
        let src = WavSource::parse("ixml.wav", with_chunk(&wav, b"iXML", xml.as_bytes()).into()).unwrap();
        assert_eq!(src.time_reference(), Some(tr));
        // a short bext (no TimeReference) is ignored
        let src = WavSource::parse("short.wav", with_chunk(&wav, b"bext", &[0u8; 100]).into()).unwrap();
        assert_eq!(src.time_reference(), None);
    }
}
